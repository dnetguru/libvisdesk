#include <iostream>
#include "libvisdesk.h"

// Callback function for visibility changes
static void visibility_callback(const MonitorVisibleInfo* monitors, const size_t num_monitors, const int64_t total_visible, const int64_t total_area, void* user_data) {
    auto count = static_cast<int*>(user_data);
	std::cout << "Desktop visibility changed (#" << *count << "). Overall visible: " << total_visible << " px (out of " << total_area << " px)" << '\n';
    for (size_t i = 0; i < num_monitors; ++i) {
        const auto& mon = monitors[i];
        std::cout << "Monitor " << i << " (ID " << mon.monitor_id << "): current visible " << mon.current_visible
            << " px, max visible " << mon.max_visible << " px (out of " << mon.total_area << " px)" << '\n';
    }
    ++(*count);
}

int main() {
    // Initialize
    LibVisInstance* handle = libvisdesk_init();
    if (!handle) {
        std::cerr << "Failed to initialize." << '\n';
        return 1;
    }
    std::cout << "Initialized VisibleDeskState" << '\n';

    size_t num_monitors = 0;
    int64_t total_visible = 0;
    int64_t total_area = 0;
    MonitorVisibleInfo* monitors = nullptr;

    // Test get_visible_area (one shot)
    if (libvisdesk_get_visible_area(handle, &monitors, &num_monitors, &total_visible, &total_area) != 0) {
        std::cout << "Initial visible desktop area: " << total_visible << " px (out of " << total_area << " px)" <<'\n';
        for (size_t i = 0; i < num_monitors; ++i) {
            const auto& mon = monitors[i];
            std::cout << "Monitor " << i << " (ID " << mon.monitor_id << "): current visible " << mon.current_visible
                << " px, max visible " << mon.max_visible << " px (out of " << mon.total_area << " px)" << '\n';
        }
        libvisdesk_free_monitors(monitors, num_monitors);
    }
    else {
        std::cerr << "Failed to get visible area." << '\n';
    }

    // Test watch_visible_area monitoring with callback
    uint64_t count = 0;
    if (libvisdesk_watch_visible_area(handle, visibility_callback, 500, &count) != 0) {
        std::cout << "Started watching visible area. Make some window changes to trigger the callback." << '\n';
    }
    else {
        std::cerr << "Failed to start watching." << '\n';
        libvisdesk_destroy(handle);
        return 1;
    }

    // Wait for user input (press Enter in console)
    std::cout << "Press Enter to stop watching..." << '\n';
    std::cin.get();

    // Test stop_watch_visible_area
    if (libvisdesk_stop_watch_visible_area(handle) != 0) {
        std::cout << "Stopped watching visible area." << '\n';
    }
    else {
        std::cerr << "Failed to stop watching." << '\n';
    }

    // Test destroy
    libvisdesk_destroy(handle);
    std::cout << "Deinitialized VisibleDeskState" << '\n';

    return 0;
}