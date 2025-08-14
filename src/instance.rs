//! Core LibVisInstance implementation.
//!
//! This module contains the implementation of the LibVisInstance struct,
//! which is the main entry point for the library's functionality.

use std::cell::RefCell;
use std::mem;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

use crate::types::VisibilityMessage;

use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use windows::Win32::Foundation::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::System::Threading::*;

use crate::types::{Callback, ThreadLocalState, MonitorVisibleInfo, SendablePtr};
use crate::visibility::calculate_visible_desktop_area;
use crate::win_callbacks::{win_event_proc, SendableWinEventHook};

pub struct LibVisInstance(Arc<Mutex<ThreadLocalState>>);

// Thread-local storage for the instance state
thread_local! {
    pub(crate) static STATE: RefCell<Option<Arc<Mutex<ThreadLocalState >>>> = const { RefCell::new(None) };
}

impl LibVisInstance {
    pub fn new() -> Self {
        let _ = tracing_subscriber::registry()
            .with(
                EnvFilter::try_from_env("LIBVISDESK_LOG_LEVEL")
                    .unwrap_or_else(|_| EnvFilter::try_new("info").unwrap()),
            )
            .with(tracing_subscriber::fmt::layer())
            .try_init();

        unsafe {
            let res = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if res.is_err() {
                warn!("Failed to set DPI awareness context: {:?}", res);
            }
        }

        info!("Created new LibVisInstance");

        Self(Arc::new(Mutex::new(ThreadLocalState::default())))
    }

    fn ensure_tokio_runtime(&mut self) -> Handle {
        let arc = self.0.clone();
        let mut state = arc.lock().unwrap();
        let (runtime_opt, handle) = match Handle::try_current() {
            Ok(h) => (None, h),
            Err(_) => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)  // Use just one worker thread
                    .enable_time()
                    .build()
                    .expect("Failed to create tokio runtime");
                let runtime_handle = rt.handle().clone();
                (Some(rt), runtime_handle)
            }
        };

        state.tokio_runtime = runtime_opt;
        state.tokio_handle = Some(handle.clone());

        handle
    }

    pub fn deinit(&mut self) {
        let _ = self.stop_watch_visible_area();
    }

    pub fn get_visible_area(&self) -> (Vec<MonitorVisibleInfo>, i64, i64) {
        debug!("Starting visible area calculation");

        // Create a temporary state struct
        let mut temp_state = ThreadLocalState::default();

        let result = calculate_visible_desktop_area(&mut temp_state);

        debug!("Visible area calculation complete. Total visible: {}, Total area: {}", result.total_visible, result.total_area);

        (result.per_monitor_stats, result.total_visible, result.total_area)
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
            // Try to get an existing runtime handle or create a new one
            let handle = self.ensure_tokio_runtime();

            let mut state = arc.lock().unwrap();
            if state.thread.is_some() {
                warn!("Watcher thread already running, cannot start new one");
                return false;
            }
            state.callback = Some(callback);
            state.user_data = SendablePtr(user_data);
            state.throttle_duration = Duration::from_millis(throttle_ms);

            // Create a channel for sending messages to the tokio task
            let (tx, rx) = mpsc::channel(100); // Buffer size of 100 messages
            state.message_sender = Some(tx.clone());

            // Spawn the main tokio task that will handle messages
            let thread_arc = arc.clone();
            let main_task = handle.spawn(async move {
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
                        let state = thread_arc.lock().unwrap();
                        state.message_sender.clone()
                    };

                    // Send a message to the tokio task
                    if let Some(sender) = sender_clone && let Err(e) = sender.try_send(VisibilityMessage::DisplayChanged) {
                        warn!("Failed to send DisplayChanged message: {:?}", e);
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

                // Send a shutdown message to the tokio task
                if let Some(sender) = &state.message_sender && let Err(e) = sender.try_send(VisibilityMessage::Shutdown) {
                    warn!("Failed to send Shutdown message: {:?}", e);
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

    async fn process_messages(arc: Arc<Mutex<ThreadLocalState>>, mut rx: Receiver<VisibilityMessage>) {
        debug!("Started tokio message processing task");

        // Track when the next computation is allowed
        let mut next_computation_time: Option<Instant> = None;

        while let Some(msg) = rx.recv().await {
            match msg {
                VisibilityMessage::WindowChanged(hwnd) => {
                    trace!("Received WindowChanged message for hwnd: {}", hwnd);
                    {
                        let mut state = arc.lock().unwrap();
                        state.changed_windows.insert(hwnd);
                    }

                    Self::handle_computation_request(&arc, &mut next_computation_time).await;
                },
                VisibilityMessage::DisplayChanged => {
                    debug!("Received DisplayChanged message");
                    {
                        // TODO: Invalidate monitor cache
                    }

                    Self::handle_computation_request(&arc, &mut next_computation_time).await;
                },
                VisibilityMessage::Shutdown => {
                    debug!("Received Shutdown message, exiting tokio task");
                    break;
                }
            }
        }

        debug!("Tokio message processing task exiting");
    }

    async fn handle_computation_request(
        arc: &Arc<Mutex<ThreadLocalState>>,
        next_computation_time: &mut Option<Instant>
    ) {

        // Callback to do the actual computation
        async fn perform_computation(arc: &Arc<Mutex<ThreadLocalState>>) {
            debug!("Performing visible area computation asynchronously");

            // Get a mutable reference to the state
            let mut state = arc.lock().unwrap();

            // Calculate visible area with access to cached data
            let result = calculate_visible_desktop_area(&mut state);

            debug!("Computation results: {:?}", result.per_monitor_stats);
            debug!("Total visible: {}, Total area: {}", result.total_visible, result.total_area);

            // Clear the changed windows set since we've processed them
            state.changed_windows.clear();

            if let Some(cb) = &state.callback {
                trace!("Calling user callback");
                cb(&result.per_monitor_stats[..], result.total_visible, result.total_area, state.user_data.0);
            } else {
                warn!("No callback set for computation");
            }
        }

        let now = Instant::now();

        // Check if a computation is already pending or if we're within the throttle period
        let (should_compute_now, should_schedule, throttle_duration) = {
            let state = arc.lock().unwrap();

            // If a timer is already pending, don't do anything
            if state.pending_timer {
                (false, false, state.throttle_duration)
            } else if next_computation_time.is_none() || next_computation_time.is_some_and(|time| now >= time) {
                // No computation has happened yet, or we're past the throttle time
                // Perform computation immediately
                (true, false, state.throttle_duration)
            } else {
                // We're within the throttle period, schedule a future computation
                (false, true, state.throttle_duration)
            }
        };

        // Get the spawn handle outside the if blocks to avoid repeated locking
        let spawn_handle = {
            let state = arc.lock().unwrap();
            state.tokio_handle.clone().unwrap()
        };

        if should_compute_now {
            // Perform computation immediately
            perform_computation(arc).await;

            // Update next allowed computation time
            *next_computation_time = Some(now + throttle_duration);

            // Update state after computation
            let mut state = arc.lock().unwrap();
            state.last_computation = Some(now);
        } else if should_schedule {
            // Get the time for the next computation
            let time = next_computation_time.unwrap();
            let wait_duration = time - now;
            let arc_clone = arc.clone();

            // Update state to indicate a pending timer
            {
                let mut state = arc.lock().unwrap();
                state.pending_timer = true;
            }

            // Spawn a task that will wait and then perform the computation
            let timer_task_handle = spawn_handle.spawn(async move {
                tokio::time::sleep(wait_duration).await;
                perform_computation(&arc_clone).await;

                // Update state after computation
                let now = Instant::now();
                let mut state = arc_clone.lock().unwrap();
                state.last_computation = Some(now);
                state.pending_timer = false;
            });

            // Store the handle in the state
            {
                let mut state = arc.lock().unwrap();
                state.tokio_timer_handle = Some(timer_task_handle);
            }
        }
        // If neither should_compute_now nor should_schedule is true,
        // do nothing as a computation is already pending or scheduled
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

            // Take the runtime to shut it down later if we created it
            let runtime = state.tokio_runtime.take();

            // Clear other resources
            state.message_sender = None;
            state.callback = None;
            state.user_data = SendablePtr(ptr::null_mut());
            state.tokio_handle = None;

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

        // Shut down the runtime in the background if we created it
        if let Some(runtime) = runtime_opt {
            debug!("Shutting down tokio runtime");
            runtime.shutdown_background();
        }

        true
    }
}

impl Default for LibVisInstance {
    fn default() -> Self {
        Self::new()
    }
}