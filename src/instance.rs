//! Core LibVisInstance implementation.
//!
//! This module contains the implementation of the LibVisInstance struct,
//! which is the main entry point for the library's functionality.

use std::cell::RefCell;
use std::env;
use std::fs::File;
use std::mem;
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use env_logger::{Builder, Target};
use log::{debug, error, info, trace, warn, LevelFilter};
use windows::Win32::Foundation::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::System::Threading::*;

use crate::types::{Callback, Inner, MonitorVisibleInfo, SendablePtr, SendableWinEventHook};
use crate::visibility::calculate_visible_desktop_area;
use crate::win::win_event_proc;

pub struct LibVisInstance(Arc<Mutex<Inner>>);

// Thread-local storage for the instance state
thread_local! {
    pub(crate) static STATE: RefCell<Option<Arc<Mutex<Inner>>>> = const { RefCell::new(None) };
}

// Custom message for triggering recalculation
pub(crate) const WM_RECOMPUTE: u32 = WM_USER + 1;

impl LibVisInstance {
    pub fn new() -> Self {
        let mut builder = Builder::new();
        builder.filter_level(LevelFilter::Error);

        let env_var = "LIBVISDESK_LOG_LEVEL";
        if let Ok(level_str) = env::var(env_var) {
            builder.parse_filters(&level_str);
        }

        if let Ok(path) = env::var("LIBVISDESK_LOG_FILE") {
            if let Ok(file) = File::create(&path) {
                builder.target(Target::Pipe(Box::new(file)));
            } else {
                eprintln!("Failed to create log file: {}", path);
            }
        }

        builder.init();

        unsafe {
            let res = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if res.is_err() {
                warn!("Failed to set DPI awareness context: {:?}", res);
            }
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
            cancel_timer: None,

            window_cache: Default::default(),
            changed_windows: Default::default(),
            monitor_cache: Vec::new(),
            windows_buffer: Vec::with_capacity(100), // Pre-allocate for ~100 windows
            region_buffer: Vec::with_capacity(4096), // Pre-allocate 4KB for region data
        };

        info!("Created new LibVisInstance");

        Self(Arc::new(Mutex::new(inner)))
    }

    pub fn deinit(&mut self) {
        let _ = self.stop_watch_visible_area();
    }

    pub fn get_visible_area(&self) -> (Vec<MonitorVisibleInfo>, i64, i64) {
        debug!("Starting visible area calculation");

        // Create temporary Inner struct
        let mut temp_inner = Inner {
            hook: None,
            thread: None,
            thread_id: None,
            callback: None,
            user_data: SendablePtr(ptr::null_mut()),
            last_computation: None,
            pending_timer: false,
            throttle_duration: Duration::from_millis(500),
            cancel_timer: None,
            window_cache: Default::default(),
            changed_windows: Default::default(),
            monitor_cache: Vec::new(),
            windows_buffer: Vec::with_capacity(100),
            region_buffer: Vec::with_capacity(4096),
        };

        // Call the function with the temporary inner struct
        let (per_monitor_stats, total_visible, total_area) = calculate_visible_desktop_area(&mut temp_inner);

        let monitors_vec: Vec<MonitorVisibleInfo> = per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
            monitor_id: id,
            current_visible: cur,
            max_visible: maxv,
            total_area: tot,
        }).collect();

        debug!("Visible area calculation complete. Total visible: {}, Total area: {}", total_visible, total_area);

        (monitors_vec, total_visible, total_area)
    }

    pub fn watch_visible_area(
        &mut self,
        callback: impl Fn(&[MonitorVisibleInfo], i64, i64, *mut std::ffi::c_void) + Send + 'static,
        throttle_ms: u64,
        user_data: *mut std::ffi::c_void,
    ) -> bool {
        info!("Starting watch_visible_area with throttle: {}ms", throttle_ms);
        let callback: Callback = Box::new(callback);

        let arc = self.0.clone();

        {
            let mut inner = arc.lock().unwrap();
            if inner.thread.is_some() {
                warn!("Watcher thread already running, cannot start new one");
                return false;
            }
            inner.callback = Some(callback);
            inner.user_data = SendablePtr(user_data);
            inner.throttle_duration = Duration::from_millis(throttle_ms);
        }

        let thread_arc = arc.clone();
        let th = thread::spawn(move || {
            info!("Spawned watcher thread");
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
                error!("Failed to set WinEventHook");
                return;
            }

            {
                let mut inner = thread_arc.lock().unwrap();
                inner.hook = Some(SendableWinEventHook(hook));
            }

            debug!("WinEventHook set successfully");

            let mut msg = unsafe { mem::zeroed::<MSG>() };
            loop {
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 == 0 || got.0 == -1 {
                    debug!("Received quit message or error, exiting loop");
                    break;
                }

                if msg.message == WM_RECOMPUTE {
                    trace!("Received WM_RECOMPUTE message");
                    let now = Instant::now();
                    let mut inner = thread_arc.lock().unwrap();
                    let should_compute = match inner.last_computation {
                        Some(last) if now - last < inner.throttle_duration => {
                            if !inner.pending_timer {
                                inner.pending_timer = true;
                                let elapsed = now - last;
                                let remaining = inner.throttle_duration - elapsed;

                                let mutex = Arc::new(Mutex::new(()));
                                let condvar = Arc::new(Condvar::new());
                                inner.cancel_timer = Some(condvar.clone());

                                let tid = inner.thread_id.unwrap();
                                thread::spawn(move || {
                                    let guard = mutex.lock().unwrap();
                                    let result = condvar.wait_timeout(guard, remaining).unwrap();
                                    if result.1.timed_out() {
                                        unsafe {
                                            let res = PostThreadMessageW(tid, WM_RECOMPUTE, WPARAM(0), LPARAM(0));
                                            if res.is_err() {
                                                warn!("PostThreadMessageW failed: {:?}", res);
                                            }
                                        }
                                    }
                                    // Thread exits (skips post if woken early for cancel)
                                });
                            }
                            false
                        }
                        _ => true,
                    };

                    drop(inner);

                    if should_compute {
                        perform_computation(&thread_arc);
                        let mut inner = thread_arc.lock().unwrap();
                        inner.last_computation = Some(Instant::now());
                        if inner.pending_timer {
                            if let Some(cond) = inner.cancel_timer.take() {
                                cond.notify_one();
                            }
                            inner.pending_timer = false;
                        }
                    }
                } else {
                    trace!("Received other message: {}", msg.message);
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        let _ = DispatchMessageW(&msg);
                    }
                }
            }

            {
                let inner = thread_arc.lock().unwrap();
                if let Some(hook_wrapper) = &inner.hook {
                    unsafe {
                        let unhook_res = UnhookWinEvent(hook_wrapper.0);
                        if !unhook_res.as_bool() {
                            warn!("Failed to unhook WinEventHook");
                        }
                    }
                }
            }

            {
                let mut inner = thread_arc.lock().unwrap();
                if inner.pending_timer {
                    if let Some(cond) = inner.cancel_timer.take() {
                        cond.notify_one();
                    }
                    inner.pending_timer = false;
                }
                inner.hook = None;
            }

            STATE.with(|s| {
                *s.borrow_mut() = None;
            });
            info!("Watcher thread exiting");
        });

        {
            let mut inner = arc.lock().unwrap();
            inner.thread = Some(th);
        }

        true
    }

    pub fn stop_watch_visible_area(&mut self) -> bool {
        info!("Stopping watch_visible_area");
        let arc = self.0.clone();

        let tid_opt = { arc.lock().unwrap().thread_id };

        if let Some(tid) = tid_opt {
            unsafe {
                let post_res = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                if post_res.is_err() {
                    warn!("Failed to post quit message to thread");
                }
            }
        }

        let th_opt = {
            let mut inner = arc.lock().unwrap();
            if inner.pending_timer {
                if let Some(cond) = inner.cancel_timer.take() {
                    cond.notify_one();
                }
                inner.pending_timer = false;
            }
            inner.callback = None;
            inner.user_data = SendablePtr(ptr::null_mut());
            mem::take(&mut inner.thread)
        };

        if let Some(th) = th_opt {
            if let Err(e) = th.join() {
                error!("Failed to join watcher thread: {:?}", e);
            } else {
                debug!("Watcher thread joined successfully");
            }
        }

        true
    }
}

fn perform_computation(arc: &Arc<Mutex<Inner>>) {
    debug!("Performing visible area computation");

    // Get a mutable reference to the inner state
    let mut inner = arc.lock().unwrap();

    // Calculate visible area with access to cached data
    let (per_monitor_stats, total_visible, total_area) = calculate_visible_desktop_area(&mut inner);

    let monitors_vec: Vec<MonitorVisibleInfo> = per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
        monitor_id: id,
        current_visible: cur,
        max_visible: maxv,
        total_area: tot,
    }).collect();

    debug!("Computation results: {:?}", monitors_vec);
    debug!("Total visible: {}, Total area: {}", total_visible, total_area);

    // Clear the changed windows set since we've processed them
    inner.changed_windows.clear();

    if let Some(cb) = &inner.callback {
        trace!("Calling user callback");
        cb(&monitors_vec[..], total_visible, total_area, inner.user_data.0);
    } else {
        warn!("No callback set for computation");
    }
}

impl Default for LibVisInstance {
    fn default() -> Self {
        Self::new()
    }
}
