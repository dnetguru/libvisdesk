//! Visibility calculation functionality for libvisdesk.
//! 
//! This module contains code for calculating the visible desktop area,
//! including region manipulation and visibility computation.

mod calculator;
mod region;

pub(crate) use calculator::calculate_visible_desktop_area;
pub(crate) use region::compute_region_area;
