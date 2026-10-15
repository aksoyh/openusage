//! Panel implementation for Linux and Windows.
//!
//! There is no `NSPanel` equivalent here, so the popover is the plain borderless
//! `main` webview window: always on top, hidden from the taskbar, and hidden again
//! when it loses focus. It is placed next to the tray icon when the platform
//! reports the icon's rect (Windows), otherwise in the corner of the screen where
//! the tray usually lives (top-right on Linux, bottom-right on Windows).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Manager, Monitor, PhysicalPosition, Position, Size, WebviewWindow, WindowEvent,
};

use super::monitor_contains_physical_point;

/// Logical gap between the tray icon (or screen edge) and the panel.
const PANEL_GAP: f64 = 6.0;

/// Focus-out events that arrive right after showing the window are spurious on
/// some window managers; ignore them for this long.
const SHOW_FOCUS_GRACE: Duration = Duration::from_millis(300);

/// Clicking the tray icon while the panel is open first blurs (and hides) the
/// panel, then delivers the click. A click this soon after a blur-hide is treated
/// as "close", not "reopen".
const BLUR_CLICK_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Corner {
    TopRight,
    BottomRight,
}

#[derive(Default)]
struct PanelState {
    /// Last known tray icon rect, in physical pixels.
    anchor: Option<Rect>,
    shown_at: Option<Instant>,
    blur_hidden_at: Option<Instant>,
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<PanelState> {
    static STATE: OnceLock<Mutex<PanelState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(PanelState::default()))
}

fn with_state<R>(f: impl FnOnce(&mut PanelState) -> R) -> R {
    let mut guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

fn main_window(app_handle: &AppHandle) -> Option<WebviewWindow> {
    let window = app_handle.get_webview_window("main");
    if window.is_none() {
        log::warn!("panel: main window not found");
    }
    window
}

fn fallback_corner() -> Corner {
    if cfg!(windows) {
        Corner::BottomRight
    } else {
        Corner::TopRight
    }
}

pub fn init(app_handle: &AppHandle) -> tauri::Result<()> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let Some(window) = main_window(app_handle) else {
        INITIALIZED.store(false, Ordering::SeqCst);
        return Ok(());
    };

    window.set_skip_taskbar(true)?;
    window.set_always_on_top(true)?;
    // Native shadow draws a gray border around the transparent window; CSS draws the shadow.
    let _ = window.set_shadow(false);

    let handle = app_handle.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Focused(false) => on_focus_lost(&handle),
        // The frontend resizes the window to fit its content; keep it anchored.
        WindowEvent::Resized(_) => {
            if let Some(window) = handle.get_webview_window("main") {
                if window.is_visible().unwrap_or(false) {
                    reposition(&window);
                }
            }
        }
        _ => {}
    });

    Ok(())
}

fn on_focus_lost(app_handle: &AppHandle) {
    let within_grace = with_state(|s| {
        s.shown_at
            .is_some_and(|shown| shown.elapsed() < SHOW_FOCUS_GRACE)
    });
    if within_grace {
        return;
    }
    let Some(window) = app_handle.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        log::debug!("panel lost focus: hiding");
        let _ = window.hide();
        with_state(|s| s.blur_hidden_at = Some(Instant::now()));
    }
}

fn show(app_handle: &AppHandle, anchor: Option<Rect>) {
    if let Err(error) = init(app_handle) {
        log::error!("Failed to init panel: {}", error);
    }
    let Some(window) = main_window(app_handle) else {
        return;
    };
    if anchor.is_some() {
        with_state(|s| s.anchor = anchor);
    }
    with_state(|s| s.shown_at = Some(Instant::now()));

    reposition(&window);
    if let Err(error) = window.show() {
        log::warn!("Failed to show panel: {}", error);
        return;
    }
    // Positioning a hidden window is not honored by every window manager.
    reposition(&window);
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// Show the panel, next to the tray icon if its position is known.
pub fn show_panel(app_handle: &AppHandle) {
    show(app_handle, None);
}

pub fn hide_panel(app_handle: &AppHandle) {
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// Toggle panel visibility. Used by the global shortcut handler.
pub fn toggle_panel(app_handle: &AppHandle) {
    let Some(window) = main_window(app_handle) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        log::debug!("toggle_panel: hiding panel");
        let _ = window.hide();
    } else {
        log::debug!("toggle_panel: showing panel");
        show(app_handle, None);
    }
}

/// Toggle the panel in response to a tray icon click at the given icon rect.
pub fn toggle_panel_at_tray(app_handle: &AppHandle, icon_position: Position, icon_size: Size) {
    let Some(window) = main_window(app_handle) else {
        return;
    };

    if window.is_visible().unwrap_or(false) {
        log::debug!("tray click: hiding panel");
        let _ = window.hide();
        return;
    }

    let just_blur_hidden = with_state(|s| {
        s.blur_hidden_at
            .take()
            .is_some_and(|hidden| hidden.elapsed() < BLUR_CLICK_DEBOUNCE)
    });
    if just_blur_hidden {
        log::debug!("tray click: panel was just closed by this click");
        return;
    }

    log::debug!("tray click: showing panel");
    let scale = window.scale_factor().unwrap_or(1.0);
    show(app_handle, Some(icon_rect(icon_position, icon_size, scale)));
}

fn icon_rect(position: Position, size: Size, scale: f64) -> Rect {
    let (x, y) = match position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x * scale, p.y * scale),
    };
    let (w, h) = match size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width * scale, s.height * scale),
    };
    Rect { x, y, w, h }
}

fn monitor_rect(monitor: &Monitor) -> Rect {
    let area = monitor.work_area();
    Rect {
        x: area.position.x as f64,
        y: area.position.y as f64,
        w: area.size.width as f64,
        h: area.size.height as f64,
    }
}

fn monitor_for_point(window: &WebviewWindow, x: f64, y: f64) -> Option<Monitor> {
    window
        .available_monitors()
        .ok()?
        .into_iter()
        .find(|monitor| {
            let origin = monitor.position();
            let size = monitor.size();
            monitor_contains_physical_point(
                origin.x as f64,
                origin.y as f64,
                size.width as f64,
                size.height as f64,
                x,
                y,
            )
        })
}

fn target_monitor(window: &WebviewWindow, anchor: Option<Rect>) -> Option<Monitor> {
    let point = match anchor {
        Some(rect) => Some((rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)),
        None => window.cursor_position().ok().map(|p| (p.x, p.y)),
    };
    point
        .and_then(|(x, y)| monitor_for_point(window, x, y))
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten())
}

fn reposition(window: &WebviewWindow) {
    let anchor = with_state(|s| s.anchor);
    let Some(monitor) = target_monitor(window, anchor) else {
        log::debug!("panel: no monitor available for positioning");
        return;
    };
    let Ok(panel_size) = window.outer_size() else {
        return;
    };

    let (x, y) = panel_origin(
        anchor,
        panel_size.width as f64,
        panel_size.height as f64,
        monitor_rect(&monitor),
        PANEL_GAP * monitor.scale_factor(),
        fallback_corner(),
    );

    if let Err(error) = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    {
        log::warn!("Failed to position panel: {}", error);
    }
}

/// Top-left corner for the panel, in physical pixels.
///
/// With a tray icon rect, the panel is centered on the icon and opens away from
/// the screen edge the icon sits on (below a top bar, above a bottom taskbar).
/// Without one, it goes into `corner` of the work area. The result is always
/// clamped into the work area.
fn panel_origin(
    anchor: Option<Rect>,
    panel_w: f64,
    panel_h: f64,
    work: Rect,
    gap: f64,
    corner: Corner,
) -> (f64, f64) {
    let (x, y) = match anchor {
        Some(icon) => {
            let x = icon.x + icon.w / 2.0 - panel_w / 2.0;
            let icon_center_y = icon.y + icon.h / 2.0;
            let y = if icon_center_y > work.y + work.h / 2.0 {
                icon.y - gap - panel_h
            } else {
                icon.y + icon.h + gap
            };
            (x, y)
        }
        None => {
            let x = work.x + work.w - panel_w - gap;
            let y = match corner {
                Corner::TopRight => work.y + gap,
                Corner::BottomRight => work.y + work.h - panel_h - gap,
            };
            (x, y)
        }
    };

    let max_x = (work.x + work.w - panel_w).max(work.x);
    let max_y = (work.y + work.h - panel_h).max(work.y);
    (x.clamp(work.x, max_x), y.clamp(work.y, max_y))
}

#[cfg(test)]
mod tests {
    use super::{Corner, Rect, panel_origin};

    const WORK: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1040.0,
    };

    #[test]
    fn opens_above_a_bottom_taskbar_icon() {
        let icon = Rect {
            x: 1700.0,
            y: 1045.0,
            w: 30.0,
            h: 30.0,
        };
        let (x, y) = panel_origin(Some(icon), 400.0, 500.0, WORK, 6.0, Corner::BottomRight);
        assert_eq!(x, 1515.0);
        assert_eq!(y, 539.0);
    }

    #[test]
    fn opens_below_a_top_bar_icon() {
        let work = Rect {
            y: 32.0,
            ..WORK
        };
        let icon = Rect {
            x: 1000.0,
            y: 2.0,
            w: 28.0,
            h: 28.0,
        };
        let (x, y) = panel_origin(Some(icon), 400.0, 500.0, work, 6.0, Corner::TopRight);
        assert_eq!(x, 814.0);
        assert_eq!(y, 36.0);
    }

    #[test]
    fn clamps_into_the_work_area_near_the_screen_edge() {
        let icon = Rect {
            x: 1900.0,
            y: 1045.0,
            w: 20.0,
            h: 30.0,
        };
        let (x, _) = panel_origin(Some(icon), 400.0, 500.0, WORK, 6.0, Corner::BottomRight);
        assert_eq!(x, 1520.0);
    }

    #[test]
    fn falls_back_to_the_configured_corner() {
        let (x, y) = panel_origin(None, 400.0, 500.0, WORK, 6.0, Corner::TopRight);
        assert_eq!((x, y), (1514.0, 6.0));

        let (x, y) = panel_origin(None, 400.0, 500.0, WORK, 6.0, Corner::BottomRight);
        assert_eq!((x, y), (1514.0, 534.0));
    }

    #[test]
    fn panel_taller_than_work_area_stays_at_the_top() {
        let (_, y) = panel_origin(None, 400.0, 2000.0, WORK, 6.0, Corner::BottomRight);
        assert_eq!(y, 0.0);
    }
}
