use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar};
// types.rs
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;

#[repr(C)]
#[derive(Debug)]
pub struct MonitorVisibleInfo {
    pub monitor_id: i64,
    pub current_visible: i64,
    pub max_visible: i64,
    pub total_area: i64,
}

#[derive(Copy, Clone)]
#[derive(Debug)]
pub struct MonitorInfo {
    pub(crate) handle: i64,
    pub(crate) rect: RECT,
    pub(crate) total_area: i64,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub(crate) rect: RECT,
    pub(crate) class_name: String,
    pub(crate) process_name: String,
    pub(crate) is_shell: bool,
    pub(crate) last_updated: Instant,
}

pub type Callback = Box<dyn Fn(&[MonitorVisibleInfo], i64, i64, *mut std::ffi::c_void) + Send + 'static>;

pub struct Inner {
    pub(crate) hook: Option<SendableWinEventHook>,
    pub(crate) thread: Option<JoinHandle<()>>,
    pub(crate) thread_id: Option<u32>,
    pub(crate) callback: Option<Callback>,
    pub(crate) user_data: SendablePtr,
    pub(crate) last_computation: Option<Instant>,
    pub(crate) pending_timer: bool,
    pub(crate) throttle_duration: Duration,
    pub(crate) cancel_timer: Option<Arc<Condvar>>,
    // Cache for windows to avoid re-querying information
    pub(crate) window_cache: HashMap<isize, WindowInfo>,
    // Set of windows that have changed since last computation
    pub(crate) changed_windows: HashSet<isize>,
    // Cached monitor information
    pub(crate) monitor_cache: Vec<MonitorInfo>,
    // Reusable buffers to reduce allocations
    pub(crate) windows_buffer: Vec<(RECT, String, String)>,
    pub(crate) region_buffer: Vec<u8>,
}

#[repr(transparent)]
pub struct SendablePtr(pub *mut std::ffi::c_void);

#[repr(transparent)]
pub struct SendableWinEventHook(pub HWINEVENTHOOK);

unsafe impl Send for SendablePtr {}
unsafe impl Sync for SendablePtr {}

unsafe impl Send for SendableWinEventHook {}
unsafe impl Sync for SendableWinEventHook {}
