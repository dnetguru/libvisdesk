//! Windows window enumeration and property functionality.
//!
//! This module contains functions for enumerating windows and retrieving
//! their properties, such as position, size, and class name.

use log::{trace, warn};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::mem;

/// Windows enumeration callback.
///
/// This function is called by Windows for each top-level window during enumeration.
/// It collects information about visible windows for visibility calculations.
pub(crate) extern "system" fn enum_windows_collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // Quick initial checks to filter out windows early
    let is_visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let is_iconic = unsafe { IsIconic(hwnd).as_bool() };

    if !is_visible || is_iconic {
        trace!("Skipping non-visible or iconic window {:?}", hwnd);
        return TRUE;
    }

    // Get window style early to filter out transparent windows
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 };
    if (ex_style & WS_EX_TRANSPARENT.0) != 0 {
        trace!("Skipping transparent window {:?}", hwnd);
        return TRUE;
    }

    // Batch DWM attribute queries by preparing a struct to hold all attributes
    let mut rect = RECT::default();
    let mut extended_rect = RECT::default();
    let mut cloaked: u32 = 0;
    let mut iconic: BOOL = BOOL(0);

    // Get extended frame bounds
    let hr = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut extended_rect as *mut _ as *mut _, mem::size_of::<RECT>() as u32) };
    if hr.is_ok() {
        rect = extended_rect;
    } else {
        // Fallback to regular window rect
        unsafe {
            let get_res = GetWindowRect(hwnd, &mut rect);
            if get_res.is_err() {
                warn!("GetWindowRect failed for hwnd {:?}", hwnd);
                return TRUE;
            }
        }
    }

    // Check if window is cloaked
    let hr_cloaked = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, mem::size_of::<u32>() as u32) };
    if hr_cloaked.is_ok() && cloaked > 0 {
        trace!("Skipping cloaked window {:?}", hwnd);
        return TRUE;
    }

    // Check if window has iconic bitmap
    let hr_iconic = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_HAS_ICONIC_BITMAP, &mut iconic as *mut _ as *mut _, mem::size_of::<BOOL>() as u32) };
    if hr_iconic.is_ok() && iconic.as_bool() {
        trace!("Skipping window with iconic bitmap {:?}", hwnd);
        return TRUE;
    }

    // Get class name
    let mut class_buf = [0u16; 256];
    let class_len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let class_name = String::from_utf16_lossy(&class_buf[0..class_len as usize]).to_string();

    // Skip desktop window
    if class_name == "Progman" {
        trace!("Skipping Progman window");
        return TRUE;
    }

    // Check layered window attributes
    if (ex_style & WS_EX_LAYERED.0) != 0 {
        let mut alpha: u8 = 0;
        let mut flags: LAYERED_WINDOW_ATTRIBUTES_FLAGS = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        let layered_res = unsafe { GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)) };
        if layered_res.is_ok() && (alpha < 255 || (flags.0 & LWA_COLORKEY.0) != 0) {
            trace!("Skipping transparent layered window {:?}", hwnd);
            return TRUE;
        }
    }

    // Get process name (only if we've passed all other filters)
    let mut process_name = String::new();
    let mut pid: u32 = 0;
    unsafe {
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }

    if pid != 0 {
        let hproc = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) };
        if let Ok(hproc) = hproc {
            let mut buf = [0u16; 256];
            let len = unsafe { GetModuleBaseNameW(hproc, None, &mut buf) };
            process_name = String::from_utf16_lossy(&buf[0..len as usize]).to_string();
            unsafe {
                let _ = CloseHandle(hproc);
            }
        }
    }

    trace!("Adding window: rect={:?}, class={}, process={}", rect, class_name, process_name);

    // Add window to the collection
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<(RECT, String, String)>) };
    windows.push((rect, class_name, process_name));

    TRUE
}
