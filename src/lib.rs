mod types;
mod ffi;

use std::cell::RefCell;
use std::mem;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub use crate::types::{Callback, Inner, MonitorInfo, MonitorVisibleInfo, SendablePtr, SendableWinEventHook};

pub struct LibVisInstance(Arc<Mutex<Inner>>);

thread_local! {
    static STATE: RefCell<Option<Arc<Mutex<Inner>>>> = const { RefCell::new(None) };
}

const WM_RECOMPUTE: u32 = WM_USER + 1;
const TIMER_ID: usize = 1;

impl LibVisInstance {
    pub fn new() -> Self {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }

        let inner = Inner {
            hook: None,
            thread: None,
            thread_id: None,
            callback: None,
            user_data: SendablePtr(ptr::null_mut()),
            last_computation: None,
            pending_timer: false,
            throttle_duration: Duration::from_millis(500),
        };

        Self(Arc::new(Mutex::new(inner)))
    }

    pub fn deinit(&mut self) {
        let _ = self.stop_watch_visible_area();
    }

    pub fn get_visible_area(&self) -> (Vec<MonitorVisibleInfo>, i64, i64) {
        let (per_monitor_stats, total_visible, total_area) = calculate_visible_desktop_area();

        let monitors_vec: Vec<MonitorVisibleInfo> = per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
            monitor_id: id,
            current_visible: cur,
            max_visible: maxv,
            total_area: tot,
        }).collect();

        (monitors_vec, total_visible, total_area)
    }

    pub fn watch_visible_area(
        &mut self,
        callback: impl Fn(&[MonitorVisibleInfo], i64, i64, *mut std::ffi::c_void) + Send + 'static,
        throttle_ms: u64,
        user_data: *mut std::ffi::c_void,
    ) -> bool {
        let callback: Callback = Box::new(callback);

        let arc = self.0.clone();

        {
            let mut inner = arc.lock().unwrap();
            if inner.thread.is_some() {
                return false;
            }
            inner.callback = Some(callback);
            inner.user_data = SendablePtr(user_data);
            inner.throttle_duration = Duration::from_millis(throttle_ms);
        }

        let thread_arc = arc.clone();
        let th = thread::spawn(move || {
            STATE.with(|s| {
                *s.borrow_mut() = Some(thread_arc.clone());
            });

            let tid = unsafe { GetCurrentThreadId() };

            {
                let mut inner = thread_arc.lock().unwrap();
                inner.thread_id = Some(tid);
            }

            let hook = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_CREATE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };

            if hook.is_invalid() {
                return;
            }

            {
                let mut inner = thread_arc.lock().unwrap();
                inner.hook = Some(SendableWinEventHook(hook));
            }

            let mut msg = unsafe { mem::zeroed::<MSG>() };
            loop {
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 == 0 || got.0 == -1 {
                    break;
                }

                if msg.message == WM_RECOMPUTE {
                    let now = Instant::now();
                    let mut inner = thread_arc.lock().unwrap();
                    let should_compute = if let Some(last) = inner.last_computation {
                        if now - last >= inner.throttle_duration {
                            true
                        } else {
                            if !inner.pending_timer {
                                let elapsed = now - last;
                                let delay = (inner.throttle_duration - elapsed).as_millis() as u32;
                                unsafe { SetTimer(HWND(std::ptr::null_mut()), TIMER_ID, delay, None); }
                                inner.pending_timer = true;
                            }
                            false
                        }
                    } else {
                        true
                    };

                    drop(inner);

                    if should_compute {
                        perform_computation(&thread_arc);
                        let mut inner = thread_arc.lock().unwrap();
                        inner.last_computation = Some(Instant::now());
                        if inner.pending_timer {
                            unsafe { let _ = KillTimer(HWND(std::ptr::null_mut()), TIMER_ID); }
                            inner.pending_timer = false;
                        }
                    }
                } else if msg.message == WM_TIMER && msg.wParam.0 == TIMER_ID {
                    perform_computation(&thread_arc);
                    let mut inner = thread_arc.lock().unwrap();
                    inner.last_computation = Some(Instant::now());
                    inner.pending_timer = false;
                } else {
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        let _ = DispatchMessageW(&msg);
                    }
                }
            }

            {
                let inner = thread_arc.lock().unwrap();
                if let Some(hook_wrapper) = &inner.hook {
                    unsafe { let _ = UnhookWinEvent(hook_wrapper.0); }
                }
            }

            {
                let mut inner = thread_arc.lock().unwrap();
                if inner.pending_timer {
                    unsafe { let _ = KillTimer(HWND(std::ptr::null_mut()), TIMER_ID); }
                    inner.pending_timer = false;
                }
                inner.hook = None;
            }

            STATE.with(|s| {
                *s.borrow_mut() = None;
            });
        });

        {
            let mut inner = arc.lock().unwrap();
            inner.thread = Some(th);
        }

        true
    }

    pub fn stop_watch_visible_area(&mut self) -> bool {
        let arc = self.0.clone();

        let tid_opt = { arc.lock().unwrap().thread_id };

        if let Some(tid) = tid_opt {
            unsafe { let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)); }
        }

        let th_opt = {
            let mut inner = arc.lock().unwrap();
            inner.callback = None;
            inner.user_data = SendablePtr(ptr::null_mut());
            mem::take(&mut inner.thread)
        };

        if let Some(th) = th_opt {
            let _ = th.join();
        }

        true
    }
}

fn perform_computation(arc: &Arc<Mutex<Inner>>) {
    let (per_monitor_stats, total_visible, total_area) = calculate_visible_desktop_area();

    let monitors_vec: Vec<MonitorVisibleInfo> = per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
        monitor_id: id,
        current_visible: cur,
        max_visible: maxv,
        total_area: tot,
    }).collect();

    let inner = arc.lock().unwrap();
    if let Some(cb) = &inner.callback {
        cb(&monitors_vec[..], total_visible, total_area, inner.user_data.0);
    }
}

impl Default for LibVisInstance { 
    fn default() -> Self { 
        Self::new()
    } 
}


extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    let is_window = unsafe { IsWindow(hwnd).as_bool() };
    let is_visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let parent = unsafe { GetParent(hwnd).unwrap_or_default() };
    let parent_invalid = parent.is_invalid();

    if !is_window || !is_visible || !parent_invalid {
        return;
    }

    if matches!(event, EVENT_OBJECT_CREATE | EVENT_OBJECT_DESTROY | EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE | EVENT_OBJECT_REORDER | EVENT_OBJECT_LOCATIONCHANGE) {
        STATE.with(|s| {
            if let Some(arc) = s.borrow().as_ref() {
                let inner = arc.lock().unwrap();
                if let Some(tid) = inner.thread_id {
                    unsafe { let _ = PostThreadMessageW(tid, WM_RECOMPUTE, WPARAM(0), LPARAM(0)); }
                }
            }
        });
    }
}

fn calculate_visible_desktop_area() -> (Vec<(i64, i64, i64, i64)>, i64, i64) {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_monitors_collect), LPARAM(&mut monitors as *mut _ as isize));
    }

    let mut total_area: i64 = 0;
    for mon in &monitors {
        total_area += mon.total_area;
    }

    let mut windows: Vec<(RECT, String, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_windows_collect), LPARAM(&mut windows as *mut _ as isize));
    }

    let mut per_monitor_stats: Vec<(i64, i64, i64, i64)> = Vec::with_capacity(monitors.len());
    let mut total_visible: i64 = 0;
    for mon in monitors {
        let current_rgn = unsafe { CreateRectRgnIndirect(&mon.rect) };
        let max_rgn = unsafe { CreateRectRgnIndirect(&mon.rect) };

        for (win_rect, class_name, _process_name) in &windows {
            let mut intersect_rect = RECT::default();
            let intersects = unsafe { IntersectRect(&mut intersect_rect, win_rect, &mon.rect).as_bool() };
            if intersects {
                let win_rgn = unsafe { CreateRectRgnIndirect(&intersect_rect) };

                unsafe {
                    CombineRgn(current_rgn, current_rgn, win_rgn, RGN_DIFF);
                }

                if class_name.starts_with("Shell_") {
                    unsafe {
                        CombineRgn(max_rgn, max_rgn, win_rgn, RGN_DIFF);
                    }
                }

                unsafe {
                    let _ = DeleteObject(win_rgn);
                }
            }
        }

        let current_visible = compute_region_area(current_rgn);
        let max_visible = compute_region_area(max_rgn);
        total_visible += current_visible;
        per_monitor_stats.push((mon.handle, current_visible, max_visible, mon.total_area));

        unsafe {
            let _ = DeleteObject(current_rgn);
            let _ = DeleteObject(max_rgn);
        }
    }

    (per_monitor_stats, total_visible, total_area)
}

fn compute_region_area(rgn: HRGN) -> i64 {
    let buffer_size = unsafe { GetRegionData(rgn, 0, None) };
    if buffer_size == 0 {
        return 0;
    }
    let mut buffer: Vec<u8> = vec![0; buffer_size as usize];
    unsafe {
        GetRegionData(rgn, buffer_size, Some(buffer.as_mut_ptr() as *mut RGNDATA));
    }
    let rgn_data = unsafe { &*(buffer.as_ptr() as *const RGNDATA) };
    let mut area: i64 = 0;
    let rects_ptr = rgn_data.Buffer.as_ptr() as *const RECT;
    for i in 0..rgn_data.rdh.nCount as isize {
        let r = unsafe { *rects_ptr.offset(i) };
        area += ((r.right - r.left) as i64) * ((r.bottom - r.top) as i64);
    }
    area
}

extern "system" fn enum_monitors_collect(hmonitor: HMONITOR, _hdc: HDC, lprc_monitor: *mut RECT, lparam: LPARAM) -> BOOL {
    let monitors = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
    let rect = unsafe { *lprc_monitor };
    let area = ((rect.right - rect.left) as i64) * ((rect.bottom - rect.top) as i64);
    let handle = hmonitor.0 as i64;
    monitors.push(MonitorInfo { handle, rect, total_area: area });
    TRUE
}

extern "system" fn enum_windows_collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let is_visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let is_iconic = unsafe { IsIconic(hwnd).as_bool() };
    if !is_visible || is_iconic {
        return TRUE;
    }

    let mut rect = RECT::default();
    let mut extended_rect = RECT::default();
    let hr = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut extended_rect as *mut _ as *mut _, mem::size_of::<RECT>() as u32) };
    if hr.is_ok() {
        rect = extended_rect;
    } else {
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
    }

    let mut cloaked: u32 = 0;
    let hr_cloaked = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, mem::size_of::<u32>() as u32) };
    if hr_cloaked.is_ok() && cloaked > 0 {
        return TRUE;
    }

    let mut iconic: BOOL = BOOL(0);
    let hr_iconic = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_HAS_ICONIC_BITMAP, &mut iconic as *mut _ as *mut _, mem::size_of::<BOOL>() as u32) };
    if hr_iconic.is_ok() && iconic.as_bool() {
        return TRUE;
    }

    let mut class_buf = [0u16; 256];
    let class_len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let class_name = String::from_utf16_lossy(&class_buf[0..class_len as usize]).to_string();

    if class_name == "Progman" {
        return TRUE;
    }

    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 };
    if (ex_style & WS_EX_TRANSPARENT.0) != 0 {
        return TRUE;
    }

    if (ex_style & WS_EX_LAYERED.0) != 0 {
        let mut alpha: u8 = 0;
        let mut flags: LAYERED_WINDOW_ATTRIBUTES_FLAGS = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        if unsafe { GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok() } && (alpha < 255 || (flags.0 & LWA_COLORKEY.0) != 0) {
            return TRUE;
        }
    }

    let mut pid: u32 = 0;
    unsafe {
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    let mut process_name = String::new();
    if pid != 0 {
        let hproc = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) };
        if let Ok(hproc) = hproc {
            let mut buf = [0u16; 256];
            let len = unsafe { GetModuleBaseNameW(hproc, None, &mut buf) };
            process_name = String::from_utf16_lossy(&buf[0..len as usize]).to_string();
            unsafe {
                let _ = CloseHandle(hproc);
            }
        }
    }

    let windows = unsafe { &mut *(lparam.0 as *mut Vec<(RECT, String, String)>) };
    windows.push((rect, class_name, process_name));

    TRUE
}