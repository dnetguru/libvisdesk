use libvisdesk::MonitorVisibleInfo;
use std::io::{self, BufRead};
use std::ptr;

fn main() {
    // Test initialization
    let mut state = libvisdesk::LibVisInstance::new();
    println!("Initialized VisibleDeskState");

    // Test get_visible_area
    let (monitors, total_visible, total_area) = state.get_visible_area();
    println!("Initial visible desktop area: {} px (out of {} px)", total_visible, total_area);
    for mon in monitors.iter() {
        println!("Monitor {} (ID {}): current visible {} px, max visible {} px (out of {} px)",
                 mon.monitor_index, mon.monitor_id, mon.current_visible, mon.max_visible, mon.total_area);
    }

    // Test watch_visible_area with a callback
    let callback = |mons: &[MonitorVisibleInfo], tv: i64, ta: i64, _ud: *mut std::ffi::c_void| {
        println!("Desktop visibility changed. Overall visible: {} px (out of {} px)", tv, ta);
        for mon in mons.iter() {
            println!("Monitor {} (ID {}): current visible {} px, max visible {} px (out of {} px)",
                     mon.monitor_index, mon.monitor_id, mon.current_visible, mon.max_visible, mon.total_area);
        }
    };

    if state.watch_visible_area(callback, 1000, ptr::null_mut()) {
        println!("Started watching visible area. Make some window changes to trigger the callback.");
    } else {
        println!("Failed to start watching.");
        return;
    }

    // Wait for user input to simulate runtime and allow changes
    println!("Press Enter to stop watching...");
    let stdin = io::stdin();
    let _ = stdin.lock().lines().next();

    // Test stop_watch_visible_area
    if state.stop_watch_visible_area() {
        println!("Stopped watching visible area.");
    } else {
        println!("Failed to stop watching.");
    }

    // Test deinit (called explicitly, though Drop would handle it)
    state.deinit();
    println!("Deinitialized VisibleDeskState");
}