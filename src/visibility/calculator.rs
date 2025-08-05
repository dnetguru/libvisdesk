//! Core visibility calculation functionality.
//!
//! This module contains the main function for calculating the visible desktop area,
//! taking into account windows and monitors.

use log::{debug, trace, warn};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::time::Instant;

use crate::types::ThreadLocalState;
use crate::visibility::compute_region_area;
use crate::win::{enum_monitors_collect, enum_windows_collect, WindowInfo, MonitorInfo};

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
    // Check if we need to update monitor information
    let need_monitor_update = state.monitor_cache.entry_count() == 0 || state.force_monitor_refresh;

    if need_monitor_update {
        // Create a temporary vector to collect monitors
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

        // Update the monitor cache with the collected monitors
        state.monitor_cache.invalidate_all();
        for monitor in monitors_vec {
            state.monitor_cache.insert(monitor.handle, monitor);
        }

        // Reset force refresh flag
        state.force_monitor_refresh = false;
    } else {
        debug!("Using cached monitor information ({} entries)", state.monitor_cache.entry_count());
    }

    // Calculate total area
    let mut total_area: i64 = 0;
    for (_key, monitor) in state.monitor_cache.iter() {
        total_area += monitor.total_area;
    }
    debug!("Total desktop area: {}", total_area);

    // If we have changed windows or empty cache, we need to update window information
    let need_full_window_update = state.window_cache.entry_count() == 0 || !state.changed_windows.is_empty();

    // Clear and reuse the windows buffer
    state.windows_buffer.clear();

    if need_full_window_update {
        // Enumerate all windows
        unsafe {
            let enum_res = EnumWindows(
                Some(enum_windows_collect), 
                LPARAM(&mut state.windows_buffer as *mut _ as isize)
            );
            if enum_res.is_err() {
                warn!("EnumWindows failed");
            }
        }

        // Update the window cache with the new information
        let now = Instant::now();
        state.window_cache.invalidate_all();
        for (rect, class_name, process_name) in &state.windows_buffer {
            let hwnd_val = rect as *const RECT as isize; // Use pointer as unique ID
            let is_shell = class_name.starts_with("Shell_");
            let window_info = WindowInfo {
                rect: *rect,
                class_name: class_name.clone(),
                process_name: process_name.clone(),
                is_shell,
                last_updated: now,
            };
            state.window_cache.insert(hwnd_val, window_info);
        }

        debug!("Updated window cache, now contains {} windows", state.window_cache.entry_count());
    } else {
        debug!("Using cached window information ({} windows)", state.window_cache.entry_count());

        // Convert cached windows to the format needed for region calculations
        for (_key, window_info) in state.window_cache.iter() {
            state.windows_buffer.push((
                window_info.rect,
                window_info.class_name.clone(),
                window_info.process_name.clone()
            ));
        }
    }

    // Calculate visible area for each monitor
    let mut per_monitor_stats: Vec<(i64, i64, i64, i64)> = Vec::with_capacity(state.monitor_cache.entry_count() as usize);
    let mut total_visible: i64 = 0;

    for (_key, monitor) in state.monitor_cache.iter() {
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
