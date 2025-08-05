//! Windows-specific functionality for libvisdesk.
//! 
//! This module contains code for interacting with Windows APIs,
//! including event handling, window enumeration, and monitor information.

mod events;
mod window;
mod monitor;

pub(crate) use events::win_event_proc;
pub use events::SendableWinEventHook;
pub(crate) use window::enum_windows_collect;
pub(crate) use monitor::enum_monitors_collect;
pub use monitor::MonitorInfo;
