//! Popover placement below the tray icon, kept inside the monitor work area,
//! and the rule that keeps it open while settings are open.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Gap between the tray icon and the popover, and minimum distance to the work area edge.
pub const MARGIN: f64 = 6.0;

/// Top-left corner for a popover centred under `icon`, clamped into `work_area`.
/// All values share one coordinate space (physical pixels in the app).
pub fn popover_origin(
    icon: Rect,
    window_width: f64,
    window_height: f64,
    work_area: Rect,
) -> (f64, f64) {
    let centered = icon.x + icon.width / 2.0 - window_width / 2.0;
    let max_x = work_area.x + work_area.width - window_width - MARGIN;
    let x = centered.min(max_x).max(work_area.x + MARGIN);
    let below = icon.y + icon.height + MARGIN;
    let max_y = work_area.y + work_area.height - window_height - MARGIN;
    let y = below.min(max_y).max(work_area.y);
    (x, y)
}

/// Why the popover is about to hide. Quit is not a hide and always exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideReason {
    FocusLost,
    Escape,
    CloseRequested,
    TrayToggle,
}

/// Every hide path asks this first. While the settings window exists, or is being
/// created, the popover stays put.
pub fn hide_allowed(_reason: HideReason, settings_open: bool) -> bool {
    !settings_open
}

/// Counts settings windows being created, so focus moving to a window that is not
/// registered yet cannot hide the popover. The count drops when the token drops,
/// including on errors, so it never goes stale.
#[derive(Debug, Default)]
pub struct SettingsOpening(AtomicUsize);

pub struct OpeningToken<'a>(&'a SettingsOpening);

impl SettingsOpening {
    pub fn begin(&self) -> OpeningToken<'_> {
        self.0.fetch_add(1, Ordering::SeqCst);
        OpeningToken(self)
    }

    pub fn in_progress(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }
}

impl Drop for OpeningToken<'_> {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Focuses the existing settings window, or creates one. Never creates a second.
pub fn open_or_focus<W>(
    existing: Option<W>,
    focus: impl FnOnce(&W) -> Result<(), String>,
    create: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    match existing {
        Some(window) => focus(&window).map(|()| false),
        None => create().map(|()| true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REASONS: [HideReason; 4] = [
        HideReason::FocusLost,
        HideReason::Escape,
        HideReason::CloseRequested,
        HideReason::TrayToggle,
    ];

    #[test]
    fn every_hide_path_is_blocked_while_settings_are_open() {
        for reason in REASONS {
            assert!(
                hide_allowed(reason, false),
                "{reason:?} with settings closed"
            );
            assert!(!hide_allowed(reason, true), "{reason:?} with settings open");
        }
    }

    fn build_while_opening(opening: &SettingsOpening, fail: bool) -> Result<(), String> {
        let _token = opening.begin();
        assert!(opening.in_progress());
        if fail {
            return Err("window build failed".into());
        }
        Ok(())
    }

    #[test]
    fn opening_flag_clears_on_success_and_on_error() {
        let opening = SettingsOpening::default();
        assert!(!opening.in_progress());
        assert!(build_while_opening(&opening, false).is_ok());
        assert!(!opening.in_progress());
        assert!(build_while_opening(&opening, true).is_err());
        assert!(
            !opening.in_progress(),
            "stale keep-open flag after an error"
        );
    }

    #[test]
    fn reuses_the_existing_settings_window() {
        let mut created = 0;
        let mut focused = 0;
        assert!(open_or_focus(
            None::<()>,
            |_| Ok(()),
            || {
                created += 1;
                Ok(())
            }
        )
        .unwrap());
        assert!(!open_or_focus(
            Some(()),
            |_| {
                focused += 1;
                Ok(())
            },
            || {
                created += 1;
                Ok(())
            }
        )
        .unwrap());
        assert_eq!((created, focused), (1, 1));
        assert_eq!(
            open_or_focus(Some(()), |_| Err("focus failed".into()), || Ok(())),
            Err("focus failed".into())
        );
    }

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 25.0,
        width: 1440.0,
        height: 875.0,
    };

    #[test]
    fn centers_below_icon() {
        let icon = Rect {
            x: 700.0,
            y: 0.0,
            width: 40.0,
            height: 24.0,
        };
        assert_eq!(popover_origin(icon, 420.0, 620.0, SCREEN), (510.0, 30.0));
    }

    #[test]
    fn clamps_to_right_edge() {
        let icon = Rect {
            x: 1400.0,
            y: 0.0,
            width: 30.0,
            height: 24.0,
        };
        assert_eq!(
            popover_origin(icon, 420.0, 620.0, SCREEN),
            (1440.0 - 420.0 - MARGIN, 30.0)
        );
    }

    #[test]
    fn clamps_to_left_edge_of_secondary_monitor() {
        let screen = Rect {
            x: -1920.0,
            y: 25.0,
            width: 1920.0,
            height: 1055.0,
        };
        let icon = Rect {
            x: -1915.0,
            y: 0.0,
            width: 30.0,
            height: 24.0,
        };
        assert_eq!(
            popover_origin(icon, 420.0, 620.0, screen),
            (-1920.0 + MARGIN, 30.0)
        );
    }

    #[test]
    fn stays_inside_short_work_area() {
        let screen = Rect {
            x: 0.0,
            y: 25.0,
            width: 1024.0,
            height: 500.0,
        };
        let icon = Rect {
            x: 500.0,
            y: 0.0,
            width: 30.0,
            height: 24.0,
        };
        let (_, y) = popover_origin(icon, 420.0, 620.0, screen);
        assert_eq!(y, 25.0);
    }
}
