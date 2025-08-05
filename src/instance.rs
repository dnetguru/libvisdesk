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

use moka::sync::Cache;

use env_logger::{Builder, Target};
use log::{debug, error, info, trace, warn, LevelFilter};
use windows::Win32::Foundation::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::System::Threading::*;

use crate::types::{Callback, ThreadLocalState, MonitorVisibleInfo, SendablePtr};
use crate::visibility::calculate_visible_desktop_area;
use crate::win::{win_event_proc, SendableWinEventHook};

pub struct LibVisInstance(Arc<Mutex<ThreadLocalState>>);

// Thread-local storage for the instance state
thread_local! {
    pub(crate) static STATE: RefCell<Option<Arc<Mutex<ThreadLocalState >>>> = const { RefCell::new(None) };
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

        let state = ThreadLocalState {
            hook: None,
            thread: None,
            thread_id: None,
            callback: None,
            user_data: SendablePtr(ptr::null_mut()),
            last_computation: None,
            pending_timer: false,
            throttle_duration: Duration::from_millis(500),
            cancel_timer: None,

            // Initialize window cache with size limit (1000 windows) and time-based expiration (1 hour)
            window_cache: Cache::builder()
                .max_capacity(1000)
                .time_to_idle(Duration::from_secs(3600))
                .build(),
            changed_windows: Default::default(),
            // Initialize monitor cache with time-based expiration (30 seconds)
            monitor_cache: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .build(),
            force_monitor_refresh: false,
            windows_buffer: Vec::with_capacity(100), // Pre-allocate for ~100 windows
            region_buffer: Vec::with_capacity(4096), // Pre-allocate 4KB for region data
        };

        info!("Created new LibVisInstance");

        Self(Arc::new(Mutex::new(state)))
    }

    pub fn deinit(&mut self) {
        let _ = self.stop_watch_visible_area();
    }

    pub fn get_visible_area(&self) -> (Vec<MonitorVisibleInfo>, i64, i64) {
        debug!("Starting visible area calculation");

        // Create temporary state struct
        let mut temp_state = ThreadLocalState {
            hook: None,
            thread: None,
            thread_id: None,
            callback: None,
            user_data: SendablePtr(ptr::null_mut()),
            last_computation: None,
            pending_timer: false,
            throttle_duration: Duration::from_millis(500),
            cancel_timer: None,
            // Initialize window cache with size limit (1000 windows) and time-based expiration (1 hour)
            window_cache: Cache::builder()
                .max_capacity(1000)
                .time_to_idle(Duration::from_secs(3600))
                .build(),
            changed_windows: Default::default(),
            // Initialize monitor cache with time-based expiration (30 seconds)
            monitor_cache: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .build(),
            force_monitor_refresh: false,
            windows_buffer: Vec::with_capacity(100),
            region_buffer: Vec::with_capacity(4096),
        };

        let result = calculate_visible_desktop_area(&mut temp_state);

        let monitors_vec: Vec<MonitorVisibleInfo> = result.per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
            monitor_id: id,
            current_visible: cur,
            max_visible: maxv,
            total_area: tot,
        }).collect();

        debug!("Visible area calculation complete. Total visible: {}, Total area: {}", result.total_visible, result.total_area);

        (monitors_vec, result.total_visible, result.total_area)
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
            let mut state = arc.lock().unwrap();
            if state.thread.is_some() {
                warn!("Watcher thread already running, cannot start new one");
                return false;
            }
            state.callback = Some(callback);
            state.user_data = SendablePtr(user_data);
            state.throttle_duration = Duration::from_millis(throttle_ms);
        }

        let thread_arc = arc.clone();
        let th = thread::spawn(move || {
            info!("Spawned watcher thread");
            STATE.with(|s| {
                *s.borrow_mut() = Some(thread_arc.clone());
            });

            let tid = unsafe { GetCurrentThreadId() };

            {
                let mut state = thread_arc.lock().unwrap();
                state.thread_id = Some(tid);
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
                let mut state = thread_arc.lock().unwrap();
                state.hook = Some(SendableWinEventHook(hook));
            }

            debug!("WinEventHook set successfully");

            let mut msg = unsafe { mem::zeroed::<MSG>() };
            loop {
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 == 0 || got.0 == -1 {
                    debug!("Received quit message or error, exiting loop");
                    break;
                }

                if msg.message == WM_DISPLAYCHANGE {
                    debug!("Display configuration changed, marking monitor cache for refresh");
                    let mut state = thread_arc.lock().unwrap();
                    state.force_monitor_refresh = true;
                    drop(state);

                    // Trigger a recomputation
                    unsafe {
                        let res = PostThreadMessageW(tid, WM_RECOMPUTE, WPARAM(0), LPARAM(0));
                        if res.is_err() {
                            warn!("PostThreadMessageW failed: {:?}", res);
                        }
                    }
                } else if msg.message == WM_RECOMPUTE {
                    trace!("Received WM_RECOMPUTE message");
                    let now = Instant::now();
                    let mut state = thread_arc.lock().unwrap();
                    let should_compute = match state.last_computation {
                        Some(last) if now - last < state.throttle_duration => {
                            if !state.pending_timer {
                                state.pending_timer = true;
                                let elapsed = now - last;
                                let remaining = state.throttle_duration - elapsed;

                                let mutex = Arc::new(Mutex::new(()));
                                let condvar = Arc::new(Condvar::new());
                                state.cancel_timer = Some(condvar.clone());

                                let tid = state.thread_id.unwrap();
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

                    drop(state);

                    if should_compute {
                        perform_computation(&thread_arc);
                        let mut state = thread_arc.lock().unwrap();
                        state.last_computation = Some(Instant::now());
                        if state.pending_timer {
                            if let Some(cond) = state.cancel_timer.take() {
                                cond.notify_one();
                            }
                            state.pending_timer = false;
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
                let state = thread_arc.lock().unwrap();
                if let Some(hook_wrapper) = &state.hook {
                    unsafe {
                        let unhook_res = UnhookWinEvent(hook_wrapper.0);
                        if !unhook_res.as_bool() {
                            warn!("Failed to unhook WinEventHook");
                        }
                    }
                }
            }

            {
                let mut state = thread_arc.lock().unwrap();
                if state.pending_timer {
                    if let Some(cond) = state.cancel_timer.take() {
                        cond.notify_one();
                    }
                    state.pending_timer = false;
                }
                state.hook = None;
            }

            STATE.with(|s| {
                *s.borrow_mut() = None;
            });
            info!("Watcher thread exiting");
        });

        {
            let mut state = arc.lock().unwrap();
            state.thread = Some(th);
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
            let mut state = arc.lock().unwrap();
            if state.pending_timer {
                if let Some(cond) = state.cancel_timer.take() {
                    cond.notify_one();
                }
                state.pending_timer = false;
            }
            state.callback = None;
            state.user_data = SendablePtr(ptr::null_mut());
            mem::take(&mut state.thread)
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

fn perform_computation(arc: &Arc<Mutex<ThreadLocalState>>) {
    debug!("Performing visible area computation");

    // Get a mutable reference to the state
    let mut state = arc.lock().unwrap();

    // Calculate visible area with access to cached data
    let result = calculate_visible_desktop_area(&mut state);

    let monitors_vec: Vec<MonitorVisibleInfo> = result.per_monitor_stats.into_iter().map(|(id, cur, maxv, tot)| MonitorVisibleInfo {
        monitor_id: id,
        current_visible: cur,
        max_visible: maxv,
        total_area: tot,
    }).collect();

    debug!("Computation results: {:?}", monitors_vec);
    debug!("Total visible: {}, Total area: {}", result.total_visible, result.total_area);

    // Clear the changed windows set since we've processed them
    state.changed_windows.clear();

    if let Some(cb) = &state.callback {
        trace!("Calling user callback");
        cb(&monitors_vec[..], result.total_visible, result.total_area, state.user_data.0);
    } else {
        warn!("No callback set for computation");
    }
}

impl Default for LibVisInstance {
    fn default() -> Self {
        Self::new()
    }
}
