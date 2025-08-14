# libvisdesk

A Rust library for detecting the visible (unoccluded) desktop area on Windows monitors, accounting for cloaked windows and taskbars. It supports per-monitor statistics, real-time watching for changes via callbacks, and throttling to limit computation frequency. The library can be used natively in Rust or from C/C++ via FFI bindings.

## Features

- Compute current visible, max visible (excluding taskbars/Shell elements), and total area per monitor.
- Overall totals across all monitors.
- Watch for window events (create, destroy, show, hide, reorder, location change) and trigger callbacks on changes.
- Throttle callbacks to at most once every X ms (default 500ms), ensuring no events are ignored but batched.
- DPI-aware for accurate calculations.
- Cross-language support: Rust native API and C/C++ FFI.

## Building

### Prerequisites
- Rust (stable recommended): Install via [rustup](https://rustup.rs/).
- For C/C++ bindings: Install [cbindgen](https://github.com/mozilla/cbindgen) via `cargo install cbindgen`.
- Windows OS (as it uses Win32 APIs).

### Build the Library
1. Clone the repo: `git clone https://github.com/dnetguru/libvisdesk.git && cd libvisdesk`
2. Build: `cargo build --release`
   - Outputs: `target/release/libvisdesk.dll` (dynamic lib), `libvisdesk.dll.lib` (import lib for MSVC), `libvisdesk.pdb` (debug symbols).

### Re-generate C/C++ Header
Run: `cbindgen --config cbindgen.toml --crate libvisdesk --output libvisdesk.h`

This generates `libvisdesk.h` with struct definitions and function declarations.

## Usage

### Rust Native
Add to `Cargo.toml`: 
```toml
[dependencies]
libvisdesk = { path = "/path/to/libvisdesk" }
```

Example (see `./examples/basic.rs` for full code):
```rust
use libvisdesk::{LibVisInstance, MonitorVisibleInfo};

fn main() {
    let mut instance = LibVisInstance::new();

    // Get initial stats
    let (monitors, total_visible, total_area) = instance.get_visible_area();
    // ... print stats

    // Watch with callback and 500ms throttle
    let callback = |mons: &[MonitorVisibleInfo], tv: i64, ta: i64, _ud: *mut std::ffi::c_void| {
        // ... handle change
    };
    instance.watch_visible_area(callback, std::ptr::null_mut(), 500);

    // Wait, then stop
    std::thread::sleep(std::time::Duration::from_secs(10));
    instance.stop_watch_visible_area();

    instance.deinit();
}
```

Build and run: `cargo run --example basic`

### C/C++ (MSVC++)
Link against `libvisdesk.dll.lib` and include `libvisdesk.h`. Copy `libvisdesk.dll` to your executable's directory.

Example (see `./examples/msvcpp/main.cpp` for full code):
```cpp
#include "libvisdesk.h"
#include <iostream>

void callback(const MonitorVisibleInfo* monitors, size_t num, int64_t tv, int64_t ta, void* ud) {
    // ... handle change
}

int main() {
    LibVisHandle handle = libvisdesk_init();
    if (!handle) return 1;

    // Get stats
    MonitorVisibleInfo* mons = nullptr;
    size_t num = 0;
    int64_t tv = 0, ta = 0;
    if (libvisdesk_get_visible_area(handle, &mons, &num, &tv, &ta) != 0) {
        // ... print stats
        libvisdesk_free_monitors(mons, num);
    }

    // Watch with callback and 500ms throttle
    libvisdesk_watch_visible_area(handle, callback, 500, nullptr);

    // Wait, then stop
    std::cin.get();
    libvisdesk_stop_watch_visible_area(handle);

    libvisdesk_destroy(handle);
    return 0;
}
```

Build in Visual Studio: Create a console app, add header/lib paths in project properties, link the .lib, and build.

### Cross compilation on Linux
```shell
rustup target add x86_64-pc-windows-gnu
sudo apt install -y gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64
cargo build --target x86_64-pc-windows-gnu
```
