use log::trace;
use windows::core::BOOL;
use windows::Win32::Graphics::Gdi::{HDC, HMONITOR};
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};

#[derive(Debug)]
#[derive(Copy, Clone)]
pub struct MonitorInfo {
    pub(crate) handle: i64,
    pub(crate) rect: RECT,
    pub(crate) total_area: i64,
}

/// Monitor enumeration callback.
///
/// This function is called by Windows for each monitor during enumeration.
/// It collects information about monitors for visibility calculations.
pub(crate) extern "system" fn enum_monitors_collect_cb(hmonitor: HMONITOR, _hdc: HDC, lprc_monitor: *mut RECT, lparam: LPARAM) -> BOOL {
    let monitors = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
    let rect = unsafe { *lprc_monitor };
    let area = ((rect.right - rect.left) as i64) * ((rect.bottom - rect.top) as i64);
    let handle = hmonitor.0 as i64;
    trace!("Enumerating monitor: handle={}, rect={:?}, area={}", handle, rect, area);
    monitors.push(MonitorInfo { handle, rect, total_area: area });
    TRUE
}