use std::ffi::c_void;
use std::mem;
use std::ptr;

use crate::{MonitorVisibleInfo, LibVisInstance};

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_init() -> *mut LibVisInstance {
    Box::into_raw(Box::new(LibVisInstance::new()))
}

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_destroy(handle: *mut LibVisInstance) {
    if handle.is_null() {
        return;
    }

    let mut state = unsafe { Box::from_raw(handle) };
    state.deinit();
}

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_get_visible_area(
    handle: *const LibVisInstance,
    out_monitors: *mut *mut MonitorVisibleInfo,
    out_num_monitors: *mut usize,
    out_total_visible: *mut i64,
    out_total_area: *mut i64,
) -> i32 {
    if handle.is_null() || out_monitors.is_null() || out_num_monitors.is_null() || out_total_visible.is_null() || out_total_area.is_null() {
        return 0;
    }

    let state = unsafe { &*handle };
    let (monitors_vec, total_visible, total_area) = state.get_visible_area();

    let len = monitors_vec.len();
    unsafe {
        if len > 0 {
            *out_monitors = monitors_vec.as_ptr() as *mut MonitorVisibleInfo;
        } else {
            *out_monitors = ptr::null_mut();
        }

        *out_num_monitors = len;
        *out_total_visible = total_visible;
        *out_total_area = total_area;
    }

    mem::forget(monitors_vec);

    1
}

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_free_monitors(monitors: *mut MonitorVisibleInfo, num_monitors: usize) {
    if !monitors.is_null() {
        unsafe {
            let _ = Vec::from_raw_parts(monitors, num_monitors, num_monitors);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_watch_visible_area(
    handle: *mut LibVisInstance,
    callback: Option<extern "C" fn(*const MonitorVisibleInfo, usize, i64, i64, *mut c_void)>,
    throttle_ms: u64,
    user_data: *mut c_void,
) -> i32 {
    if handle.is_null() || callback.is_none() {
        return 0;
    }
    
    let callback = callback.unwrap();

    let state = unsafe { &mut *handle };

    let rust_callback = move |mons: &[MonitorVisibleInfo], tv: i64, ta: i64, ud: *mut c_void| {
        callback(mons.as_ptr(), mons.len(), tv, ta, ud);
    };

    if state.watch_visible_area(rust_callback, throttle_ms, user_data) { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn libvisdesk_stop_watch_visible_area(handle: *mut LibVisInstance) -> i32 {
    if handle.is_null() {
        return 0;
    }

    let state = unsafe { &mut *handle };

    if state.stop_watch_visible_area() { 1 } else { 0 }
}