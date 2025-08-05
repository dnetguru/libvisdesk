//! Windows event handling functionality.
//!
//! This module contains the event procedure for handling Windows events
//! related to window creation, destruction, and movement.

use log::warn;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::instance::{STATE, WM_RECOMPUTE};

#[repr(transparent)]
pub struct SendableWinEventHook(pub HWINEVENTHOOK);

unsafe impl Send for SendableWinEventHook {}
unsafe impl Sync for SendableWinEventHook {}

/// Windows event procedure callback.
///
/// This function is called by Windows when relevant window events occur.
/// It tracks window changes and triggers recalculation of the visible desktop area.
pub(crate) extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    let is_window = unsafe { IsWindow(hwnd).as_bool() };
    let is_visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let parent = unsafe { GetParent(hwnd).unwrap_or_default() };
    let parent_invalid = parent.is_invalid();

    if !is_window || !is_visible || !parent_invalid {
        return;
    }

    if matches!(event, EVENT_OBJECT_CREATE | EVENT_OBJECT_DESTROY | EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE | EVENT_OBJECT_REORDER | EVENT_OBJECT_LOCATIONCHANGE) {
        STATE.with(|s| {
            if let Some(arc) = s.borrow().as_ref() {
                let mut state = arc.lock().unwrap();

                // Track the changed window
                let hwnd_val = hwnd.0 as isize;
                state.changed_windows.insert(hwnd_val);

                // If it's a destroy event, remove from cache
                if event == EVENT_OBJECT_DESTROY {
                    state.window_cache.remove(&hwnd_val);
                }

                if let Some(tid) = state.thread_id {
                    unsafe {
                        let post_res = PostThreadMessageW(tid, WM_RECOMPUTE, WPARAM(0), LPARAM(0));
                        if post_res.is_err() {
                            warn!("Failed to post WM_RECOMPUTE message");
                        }
                    }
                } else {
                    warn!("No thread_id set for posting message");
                }
            } else {
                warn!("No STATE arc available for posting message");
            }
        });
    }
}
