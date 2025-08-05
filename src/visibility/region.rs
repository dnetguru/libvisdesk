//! Region manipulation functionality.
//!
//! This module contains functions for manipulating Windows GDI regions,
//! including computing the area of a region.

use log::{debug, trace, warn};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;

/// Compute the area of a region.
///
/// This function computes the area of a Windows GDI region by retrieving
/// the region data and summing the areas of the rectangles that make up the region.
///
/// Arguments:
/// * `rgn` - The region handle
/// * `buffer` - A reusable buffer for region data to avoid allocations
///
/// Returns the area of the region in pixels.
pub(crate) fn compute_region_area(rgn: HRGN, buffer: &mut Vec<u8>) -> i64 {
    let buffer_size = unsafe { GetRegionData(rgn, 0, None) };
    if buffer_size == 0 {
        debug!("GetRegionData returned 0 size");
        return 0;
    }

    // Resize the buffer if needed
    if buffer.len() < buffer_size as usize {
        buffer.resize(buffer_size as usize, 0);
    }

    let data_size = unsafe {
        GetRegionData(rgn, buffer_size, Some(buffer.as_mut_ptr() as *mut RGNDATA))
    };
    if data_size == 0 {
        warn!("Failed to get region data");
        return 0;
    }

    let rgn_data = unsafe { &*(buffer.as_ptr() as *const RGNDATA) };
    let mut area: i64 = 0;
    let rects_ptr = rgn_data.Buffer.as_ptr() as *const RECT;
    for i in 0..rgn_data.rdh.nCount as isize {
        let r = unsafe { *rects_ptr.offset(i) };
        trace!("Region rect: {:?}", r);
        area += ((r.right - r.left) as i64) * ((r.bottom - r.top) as i64);
    }
    trace!("Computed region area: {}", area);
    area
}