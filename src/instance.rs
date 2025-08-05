//! Core LibVisInstance implementation.
//!
//! This module contains the implementation of the LibVisInstance struct,
//! which is the main entry point for the library's functionality.

use std::cell::RefCell;
use std::env;
use std::fs::File;
use std::mem;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::time;

use crate::types::VisibilityMessage;

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

impl LibVisInstance {
    // Create a tokio runtime for async operations
    fn create_runtime() -> Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)  // Use just one worker thread
            .enable_time()
            .build()
            .expect("Failed to create tokio runtime")
    }

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
            tokio_timer_handle: None,
            tokio_runtime: None,
            message_sender: None,
            message_receiver: None,
            main_task_handle: None,

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
            tokio_timer_handle: None,
            tokio_runtime: None,
            message_sender: None,
            message_receiver: None,
            main_task_handle: None,
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

            // Create a tokio runtime for async operations
            let runtime = Arc::new(Self::create_runtime());
            state.tokio_runtime = Some(runtime.clone());

            // Create a channel for sending messages to the tokio task
            let (tx, rx) = mpsc::channel(100); // Buffer size of 100 messages
            state.message_sender = Some(tx.clone());

            // Spawn the main tokio task that will handle messages
            let thread_arc = arc.clone();
            let main_task = runtime.spawn(async move {
                Self::process_messages(thread_arc, rx).await;
            });

            state.main_task_handle = Some(main_task);
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

            // Windows message loop - now only handles Windows messages and forwards relevant ones to tokio
            let mut msg = unsafe { mem::zeroed::<MSG>() };
            loop {
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 == 0 || got.0 == -1 {
                    debug!("Received quit message or error, exiting loop");
                    break;
                }

                if msg.message == WM_DISPLAYCHANGE {
                    debug!("Display configuration changed, marking monitor cache for refresh");

                    // Get a clone of the sender if available
                    let sender_clone = {
                        let mut state = thread_arc.lock().unwrap();
                        state.force_monitor_refresh = true;
                        state.message_sender.clone()
                    };

                    // Send a message to the tokio task
                    if let Some(sender) = sender_clone {
                        if let Err(e) = sender.try_send(VisibilityMessage::DisplayChanged) {
                            warn!("Failed to send DisplayChanged message: {:?}", e);
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

            // Cleanup when the message loop exits
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

                // Send shutdown message to the tokio task
                if let Some(sender) = &state.message_sender {
                    if let Err(e) = sender.try_send(VisibilityMessage::Shutdown) {
                        warn!("Failed to send Shutdown message: {:?}", e);
                    }
                }
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

    // Process messages in the tokio task
    async fn process_messages(arc: Arc<Mutex<ThreadLocalState>>, mut rx: Receiver<VisibilityMessage>) {
        debug!("Started tokio message processing task");

        let mut throttle_timer: Option<time::Interval> = None;

        while let Some(msg) = rx.recv().await {
            match msg {
                VisibilityMessage::WindowChanged(hwnd) => {
                    trace!("Received WindowChanged message for hwnd: {}", hwnd);
                    {
                        let mut state = arc.lock().unwrap();
                        state.changed_windows.insert(hwnd);
                    } // Drop the MutexGuard before await

                    // Schedule a computation with throttling
                    Self::schedule_throttled_computation(&arc, &mut throttle_timer).await;
                },
                VisibilityMessage::DisplayChanged => {
                    debug!("Received DisplayChanged message");
                    {
                        let mut state = arc.lock().unwrap();
                        state.force_monitor_refresh = true;
                    } // Drop the MutexGuard before await

                    // Schedule a computation with throttling
                    Self::schedule_throttled_computation(&arc, &mut throttle_timer).await;
                },
                VisibilityMessage::ComputeNow => {
                    trace!("Received ComputeNow message");
                    // Perform computation immediately
                    perform_computation_async(&arc).await;
                },
                VisibilityMessage::Shutdown => {
                    debug!("Received Shutdown message, exiting tokio task");
                    break;
                }
            }
        }

        debug!("Tokio message processing task exiting");
    }

    // Schedule a throttled computation
    async fn schedule_throttled_computation(
        arc: &Arc<Mutex<ThreadLocalState>>, 
        throttle_timer: &mut Option<time::Interval>
    ) {
        let now = Instant::now();

        // Use a block to ensure the MutexGuard is dropped before any await points
        let should_compute = {
            let mut state = arc.lock().unwrap();

            match state.last_computation {
                Some(last) if now - last < state.throttle_duration => {
                    // If we're within the throttle duration, don't compute now
                    if !state.pending_timer {
                        state.pending_timer = true;

                        // Create a new interval timer if we don't have one
                        if throttle_timer.is_none() {
                            let elapsed = now - last;
                            let remaining = state.throttle_duration - elapsed;
                            let mut interval = time::interval(remaining);
                            interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                            *throttle_timer = Some(interval);
                        }
                    }
                    false
                }
                _ => true,
            }
        }; // MutexGuard is dropped here

        if should_compute {
            // Perform computation immediately
            perform_computation_async(arc).await;

            // Update state after computation
            let mut state = arc.lock().unwrap();
            state.last_computation = Some(Instant::now());
            state.pending_timer = false;
            *throttle_timer = None;
        } else if let Some(interval) = throttle_timer {
            // Wait for the next tick of the interval
            interval.tick().await;

            // Perform computation after throttle period
            perform_computation_async(arc).await;

            // Update state after computation
            let mut state = arc.lock().unwrap();
            state.last_computation = Some(Instant::now());
            state.pending_timer = false;
            *throttle_timer = None;
        }
    }

    pub fn stop_watch_visible_area(&mut self) -> bool {
        info!("Stopping watch_visible_area");
        let arc = self.0.clone();

        // First, try to send a shutdown message through the tokio channel
        {
            let state = arc.lock().unwrap();
            if let Some(sender) = &state.message_sender {
                debug!("Sending Shutdown message through tokio channel");
                if let Err(e) = sender.try_send(VisibilityMessage::Shutdown) {
                    warn!("Failed to send Shutdown message: {:?}", e);
                }
            }
        }

        // Then, post a quit message to the Windows message loop
        let tid_opt = { arc.lock().unwrap().thread_id };
        if let Some(tid) = tid_opt {
            unsafe {
                let post_res = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                if post_res.is_err() {
                    warn!("Failed to post quit message to thread");
                }
            }
        }

        // Clean up all resources
        let (th_opt, main_task_opt, runtime_opt) = {
            let mut state = arc.lock().unwrap();

            // Cancel any pending timers
            if state.pending_timer {
                if let Some(handle) = state.tokio_timer_handle.take() {
                    debug!("Aborting tokio timer task");
                    handle.abort();
                }
                state.pending_timer = false;
            }

            // Cancel the main tokio task if it exists
            let main_task = if let Some(handle) = state.main_task_handle.take() {
                debug!("Aborting main tokio task");
                Some(handle)
            } else {
                None
            };

            // Take the runtime to drop it later
            let runtime = state.tokio_runtime.take();

            // Clear other resources
            state.message_sender = None;
            state.callback = None;
            state.user_data = SendablePtr(ptr::null_mut());

            // Take the thread handle
            (mem::take(&mut state.thread), main_task, runtime)
        };

        // Abort the main task if it exists
        if let Some(task) = main_task_opt {
            task.abort();
        }

        // Join the thread if it exists
        if let Some(th) = th_opt {
            if let Err(e) = th.join() {
                error!("Failed to join watcher thread: {:?}", e);
            } else {
                debug!("Watcher thread joined successfully");
            }
        }

        // Drop the runtime explicitly
        if let Some(runtime) = runtime_opt {
            debug!("Dropping tokio runtime");
            drop(runtime);
        }

        true
    }
}

// Async version of perform_computation
async fn perform_computation_async(arc: &Arc<Mutex<ThreadLocalState>>) {
    debug!("Performing visible area computation asynchronously");

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

// Synchronous wrapper for perform_computation_async
fn perform_computation(arc: &Arc<Mutex<ThreadLocalState>>) {
    debug!("Performing visible area computation (sync wrapper)");

    // Create a tokio runtime for the computation
    let rt = LibVisInstance::create_runtime();

    // Block on the async computation
    rt.block_on(perform_computation_async(arc));
}

impl Default for LibVisInstance {
    fn default() -> Self {
        Self::new()
    }
}
