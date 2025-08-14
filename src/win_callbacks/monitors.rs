use tracing::log::trace;
use windows::core::BOOL;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};

#[derive(Debug)]
#[derive(Copy, Clone)]
pub struct MonitorInfo {
    pub(crate) id: i64,
    pub(crate) index: i64,
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

    // Get GDI device name via GetMonitorInfoW
    let mut info_ex: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info_ex.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let success = unsafe { GetMonitorInfoW(hmonitor, &mut info_ex as *mut _ as *mut MONITORINFO) };
    let device_name = if success.0 != 0 {
        let arr = &info_ex.szDevice;
        String::from_utf16_lossy(&arr[0..arr.iter().position(|&c| c == 0).unwrap_or(arr.len())]) // Parse as UTF-16 from 0 to first null byte
    } else {
        String::new()
    };

    let id = if let Some(pos) = device_name.rfind("DISPLAY") {
        let num_str = &&device_name[pos + "DISPLAY".len()..];
        num_str.parse::<i64>().unwrap_or(-1)  // Temp -1 if parse fails
    } else {
        -1  // Temp -1 if no "DISPLAY" for now
    };

    trace!("Enumerating monitor: temp_handle={}, rect={:?}, area={}, device_name={}", id, rect, area, device_name);
    monitors.push(MonitorInfo { id, index: id, rect, total_area: area });
    TRUE
}

/// Post-process to set handle to display index, falling back to max+1 etc. for unparseable ones.
/// Call this after EnumDisplayMonitors.
pub fn monitor_index_post_process(monitors: &mut [MonitorInfo]) {
    let mut next = monitors.iter().filter(|m| m.index > 0).map(|m| m.index).max().unwrap_or(0) + 1;
    monitors.iter_mut().filter(|m| m.index < 0).for_each(|m| { m.index = next; next += 1; });
    monitors.sort_unstable_by_key(|m| m.index);
    monitors.iter_mut().enumerate().for_each(|(i ,m)| { m.index = i as i64 + 1;})
}