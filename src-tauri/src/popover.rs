//! Popover placement below the tray icon, kept inside the monitor work area.

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

#[cfg(test)]
mod tests {
    use super::*;

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
