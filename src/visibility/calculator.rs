//! Core visibility calculation functionality.
//!
//! This module contains the main function for calculating the visible desktop area,
//! taking into account windows and monitors.

use log::{debug, trace, warn};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::types::ThreadLocalState;
use crate::visibility::compute_region_area;
use crate::win::{enum_monitors_collect, enum_windows_collect, MonitorInfo};

/// Result of the visible desktop area calculation.
#[derive(Debug)]
pub(crate) struct VisibleDesktopAreaResult {
    /// Statistics for each monitor: (monitor_id, current_visible, max_visible, total_area)
    pub per_monitor_stats: Vec<(i64, i64, i64, i64)>,
    /// Total visible area across all monitors
    pub total_visible: i64,
    /// Total area across all monitors
    pub total_area: i64,
}

/// Calculate the visible desktop area.
///
/// This function calculates the visible desktop area by enumerating monitors and windows,
/// and then computing the area that is not covered by windows.
///
/// Returns a `VisibleDesktopAreaResult` containing per-monitor statistics and totals.
pub(crate) fn calculate_visible_desktop_area(state: &mut ThreadLocalState) -> VisibleDesktopAreaResult {
    // TODO: Cache monitors
    let mut monitors_vec: Vec<MonitorInfo> = Vec::new();

    unsafe {
        let enum_res = EnumDisplayMonitors(
            None, 
            None, 
            Some(enum_monitors_collect), 
            LPARAM(&mut monitors_vec as *mut _ as isize)
        );
        if !enum_res.as_bool() {
            warn!("EnumDisplayMonitors failed");
        }
    }

    debug!("Enumerated {} monitors", monitors_vec.len());
    trace!("Monitors: {:?}", monitors_vec);

    // Calculate total area
    let mut total_area: i64 = 0;
    for monitor in &monitors_vec {
        total_area += monitor.total_area;
    }
    debug!("Total desktop area: {}", total_area);

    // Clear and reuse the windows buffer
    state.windows_buffer.clear();

    // TODO: Cache windows
    unsafe {
        let enum_res = EnumWindows(
            Some(enum_windows_collect), 
            LPARAM(&mut state.windows_buffer as *mut _ as isize)
        );
        if enum_res.is_err() {
            warn!("EnumWindows failed");
        }
    }

    debug!("Enumerated {} windows", state.windows_buffer.len());

    // Calculate visible area for each monitor
    let mut per_monitor_stats: Vec<(i64, i64, i64, i64)> = Vec::with_capacity(monitors_vec.len());
    let mut total_visible: i64 = 0;

    for monitor in &monitors_vec {
        let current_rgn = unsafe { CreateRectRgnIndirect(&monitor.rect) };
        if current_rgn.is_invalid() {
            warn!("Failed to create current_rgn for monitor {}", monitor.handle);
            continue;
        }
        let max_rgn = unsafe { CreateRectRgnIndirect(&monitor.rect) };
        if max_rgn.is_invalid() {
            warn!("Failed to create max_rgn for monitor {}", monitor.handle);
            unsafe { let _ = DeleteObject(current_rgn); }
            continue;
        }

        for (win_rect, class_name, _process_name) in &state.windows_buffer {
            let mut intersect_rect = RECT::default();
            let intersects = unsafe { IntersectRect(&mut intersect_rect, win_rect, &monitor.rect).as_bool() };
            if intersects {
                trace!("Intersecting window: rect={:?}, class={}, process={}", win_rect, class_name, _process_name);
                let win_rgn = unsafe { CreateRectRgnIndirect(&intersect_rect) };
                if win_rgn.is_invalid() {
                    warn!("Failed to create win_rgn for intersecting window");
                    continue;
                }

                unsafe {
                    CombineRgn(current_rgn, current_rgn, win_rgn, RGN_DIFF);
                }

                if class_name.starts_with("Shell_") {
                    unsafe {
                        CombineRgn(max_rgn, max_rgn, win_rgn, RGN_DIFF);
                    }
                }

                unsafe {
                    let delete_res = DeleteObject(win_rgn);
                    if !delete_res.as_bool() {
                        trace!("Failed to delete win_rgn");
                    }
                }
            }
        }

        // Reuse the region buffer if possible
        state.region_buffer.clear();
        let current_visible = compute_region_area(current_rgn, &mut state.region_buffer);
        let max_visible = compute_region_area(max_rgn, &mut state.region_buffer);

        debug!("Monitor {}: current_visible={}, max_visible={}, total_area={}", 
               monitor.handle, current_visible, max_visible, monitor.total_area);

        total_visible += current_visible;
        per_monitor_stats.push((monitor.handle, current_visible, max_visible, monitor.total_area));

        unsafe {
            let _ = DeleteObject(current_rgn);
            let _ = DeleteObject(max_rgn);
        }
    }

    VisibleDesktopAreaResult {
        per_monitor_stats,
        total_visible,
        total_area,
    }
}
