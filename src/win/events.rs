//! Windows event handling functionality.
//!
//! This module contains the event procedure for handling Windows events
//! related to window creation, destruction, and movement.

use log::{trace, warn};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::instance::STATE;
use crate::types::VisibilityMessage;

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
        // Get the window handle as isize
        let hwnd_val = hwnd.0 as isize;

        // First, track the window in the changed_windows set
        STATE.with(|s| {
            if let Some(arc) = s.borrow().as_ref() {
                let mut state = arc.lock().unwrap();

                // Track the changed window
                state.changed_windows.insert(hwnd_val);

                // If it's a destroy event, remove from cache
                if event == EVENT_OBJECT_DESTROY {
                    state.window_cache.invalidate(&hwnd_val);
                }

                // Get a clone of the sender if available
                let sender_clone = state.message_sender.clone();

                // Drop the lock before sending the message
                drop(state);

                // Send the message if we have a sender
                if let Some(sender) = sender_clone {
                    trace!("Sending WindowChanged message through tokio channel");
                    if let Err(e) = sender.try_send(VisibilityMessage::WindowChanged(hwnd_val)) {
                        warn!("Failed to send WindowChanged message: {:?}", e);
                    }
                } else {
                    warn!("No message_sender available");
                }
            } else {
                warn!("No STATE arc available for posting message");
            }
        });
    }
}
