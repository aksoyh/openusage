//! The tray-anchored popover panel.
//!
//! macOS uses a non-activating `NSPanel` (via `tauri-nspanel`) so the popover
//! behaves like a native menu-bar extra. Linux and Windows use the regular
//! borderless webview window, kept on top and hidden from the taskbar.
//!
//! Both implementations expose the same functions: `init`, `show_panel`,
//! `hide_panel`, `toggle_panel` and `toggle_panel_at_tray`.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod desktop;
#[cfg(not(target_os = "macos"))]
pub use desktop::*;

fn monitor_contains_physical_point(
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
    point_x: f64,
    point_y: f64,
) -> bool {
    point_x >= origin_x
        && point_x < origin_x + width
        && point_y >= origin_y
        && point_y < origin_y + height
}
