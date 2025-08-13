//! Windows-specific functionality for libvisdesk.
//! 
//! This module contains code for interacting with Windows APIs,
//! including event handling, window enumeration, and monitor information.

mod events;
pub(crate) mod windows;
pub(crate) mod monitors;

pub use events::SendableWinEventHook;
pub(crate) use events::win_event_proc;