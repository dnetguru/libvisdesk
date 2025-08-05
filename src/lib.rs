//! libvisdesk - A library for calculating visible desktop area.
//!
//! This library provides functionality for calculating the visible desktop area
//! by taking into account windows and monitors. It can be used to determine how
//! much of the desktop is visible to the user, which is useful for applications
//! that need to adapt their UI based on available screen space.

mod types;
mod ffi;
mod instance;
mod win;
mod visibility;

// Re-export public API
pub use crate::types::MonitorVisibleInfo;
pub use crate::instance::LibVisInstance;
