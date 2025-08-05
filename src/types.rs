use std::collections::HashSet;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle as TokioJoinHandle;
use windows::Win32::Foundation::RECT;

use crate::win::SendableWinEventHook;

/// Messages that can be sent through the tokio channel
#[derive(Debug)]
pub(crate) enum VisibilityMessage {
    /// A window has changed (created, destroyed, moved, etc.)
    WindowChanged(isize),
    /// The display configuration has changed
    DisplayChanged,
    /// Request to stop the watcher
    Shutdown,
}

#[repr(C)]
#[derive(Debug)]
pub struct MonitorVisibleInfo {
    pub monitor_id: i64,
    pub current_visible: i64,
    pub max_visible: i64,
    pub total_area: i64,
}

pub type Callback = Box<dyn Fn(&[MonitorVisibleInfo], i64, i64, *mut std::ffi::c_void) + Send + 'static>;

pub struct ThreadLocalState {
    pub(crate) hook: Option<SendableWinEventHook>,
    pub(crate) thread: Option<JoinHandle<()>>,
    pub(crate) thread_id: Option<u32>,
    pub(crate) callback: Option<Callback>,
    pub(crate) user_data: SendablePtr,
    pub(crate) last_computation: Option<Instant>,
    pub(crate) pending_timer: bool,
    pub(crate) throttle_duration: Duration,
    pub(crate) tokio_timer_handle: Option<TokioJoinHandle<()>>,
    // Tokio runtime for async operations
    pub(crate) tokio_runtime: Option<Arc<Runtime>>,
    // Channel for sending messages to the tokio task
    pub(crate) message_sender: Option<Sender<VisibilityMessage>>,
    // Main tokio task handle
    pub(crate) main_task_handle: Option<TokioJoinHandle<()>>,
    // Set of windows that have changed since last computation
    pub(crate) changed_windows: HashSet<isize>,
    // Reusable buffers to reduce allocations
    pub(crate) windows_buffer: Vec<(RECT, String, String)>,
    pub(crate) region_buffer: Vec<u8>,
}

#[repr(transparent)]
pub struct SendablePtr(pub *mut std::ffi::c_void);

unsafe impl Send for SendablePtr {}
unsafe impl Sync for SendablePtr {}
