// types.rs
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;

#[repr(C)]
pub struct MonitorVisibleInfo {
    pub monitor_id: i64,
    pub current_visible: i64,
    pub max_visible: i64,
    pub total_area: i64,
}

#[derive(Copy, Clone)]
pub struct MonitorInfo {
    pub(crate) handle: i64,
    pub(crate) rect: RECT,
    pub(crate) total_area: i64,
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
}

#[repr(transparent)]
pub struct SendablePtr(pub *mut std::ffi::c_void);

#[repr(transparent)]
pub struct SendableWinEventHook(pub HWINEVENTHOOK);

unsafe impl Send for SendablePtr {}
unsafe impl Sync for SendablePtr {}

unsafe impl Send for SendableWinEventHook {}
unsafe impl Sync for SendableWinEventHook {}