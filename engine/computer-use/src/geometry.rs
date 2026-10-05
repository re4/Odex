//! Coordinate math shared by all platforms.

use crate::{CoordinateSpace, Rect, Screenshot};

/// Size that fits within `max_edge` on the longest side, keeping the aspect ratio. `max_edge == 0` = no limit.
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if max_edge == 0 || longest <= max_edge || longest == 0 {
        return (width, height);
    }
    let scale = max_edge as f64 / longest as f64;
    let w = ((width as f64 * scale).round() as u32).clamp(1, max_edge);
    let h = ((height as f64 * scale).round() as u32).clamp(1, max_edge);
    (w, h)
}

/// Map a point the model gave in screenshot space to physical virtual-screen coordinates.
///
/// * `Pixels`: screenshot pixels (as returned in the PNG, after downscaling).
/// * `Normalized1000`: 0..=1000 across the screenshot.
/// * `Normalized1`: 0.0..=1.0 across the screenshot.
///
/// The result is clamped to the captured area so a click never lands outside it.
pub fn map_point(shot: &Screenshot, x: f64, y: f64, space: CoordinateSpace) -> (i32, i32) {
    let (w, h) = (shot.width as f64, shot.height as f64);
    let (sx, sy) = match space {
        CoordinateSpace::Pixels => (x, y),
        CoordinateSpace::Normalized1000 => (x / 1000.0 * w, y / 1000.0 * h),
        CoordinateSpace::Normalized1 => (x * w, y * h),
    };
    let (sx, sy) = (finite_or_zero(sx), finite_or_zero(sy));
    let (pw, ph) = shot.physical_size();
    let px = (sx * shot.scale_x).round().clamp(0.0, pw.saturating_sub(1) as f64);
    let py = (sy * shot.scale_y).round().clamp(0.0, ph.saturating_sub(1) as f64);
    (shot.origin_x.saturating_add(px as i32), shot.origin_y.saturating_add(py as i32))
}

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Center of a rectangle, rounded to whole pixels.
pub fn rect_center(r: &Rect) -> (i32, i32) {
    ((r.x + r.width / 2.0).round() as i32, (r.y + r.height / 2.0).round() as i32)
}

/// Physical virtual-screen point → `SendInput` absolute coordinates (0..=65535 across the virtual desktop,
/// used with `MOUSEEVENTF_VIRTUALDESK`).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn to_absolute(x: i32, y: i32, vx: i32, vy: i32, vw: i32, vh: i32) -> (i32, i32) {
    fn axis(p: i32, origin: i32, size: i32) -> i32 {
        if size <= 1 {
            return 0;
        }
        let rel = (p - origin).clamp(0, size - 1) as f64;
        (rel * 65535.0 / (size - 1) as f64).round() as i32
    }
    (axis(x, vx, vw), axis(y, vy, vh))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(width: u32, height: u32, scale: f64, ox: i32, oy: i32) -> Screenshot {
        Screenshot {
            png: Vec::new(),
            width,
            height,
            scale_x: scale,
            scale_y: scale,
            origin_x: ox,
            origin_y: oy,
            window: None,
            note: None,
        }
    }

    #[test]
    fn fit_within_downscales_longest_edge() {
        assert_eq!(fit_within(1920, 1080, 0), (1920, 1080));
        assert_eq!(fit_within(1920, 1080, 4000), (1920, 1080));
        assert_eq!(fit_within(3840, 2160, 1920), (1920, 1080));
        assert_eq!(fit_within(1080, 1920, 960), (540, 960));
        assert_eq!(fit_within(5120, 1440, 1568), (1568, 441));
        assert_eq!(fit_within(10000, 1, 100), (100, 1));
        assert_eq!(fit_within(0, 0, 100), (0, 0));
    }

    #[test]
    fn map_pixels_with_scale_and_origin() {
        // 3840x2160 physical captured at origin (0,0), downscaled to 1920x1080.
        let s = shot(1920, 1080, 2.0, 0, 0);
        assert_eq!(map_point(&s, 100.0, 50.0, CoordinateSpace::Pixels), (200, 100));
        assert_eq!(map_point(&s, 0.0, 0.0, CoordinateSpace::Pixels), (0, 0));
        // Clamped to the captured area.
        assert_eq!(map_point(&s, 5000.0, -10.0, CoordinateSpace::Pixels), (3839, 0));
    }

    #[test]
    fn map_negative_origin_monitor() {
        // Left monitor at x = -2560, y = -200; captured 1:1.
        let s = shot(2560, 1440, 1.0, -2560, -200);
        assert_eq!(map_point(&s, 10.0, 20.0, CoordinateSpace::Pixels), (-2550, -180));
        assert_eq!(map_point(&s, 1280.0, 720.0, CoordinateSpace::Pixels), (-1280, 520));
        assert_eq!(map_point(&s, 500.0, 500.0, CoordinateSpace::Normalized1000), (-1280, 520));
        assert_eq!(map_point(&s, 0.5, 0.5, CoordinateSpace::Normalized1), (-1280, 520));
    }

    #[test]
    fn map_normalized_spaces() {
        // Window at (100, 200), 1000x500 physical, screenshot downscaled to 500x250.
        let s = shot(500, 250, 2.0, 100, 200);
        assert_eq!(map_point(&s, 0.0, 0.0, CoordinateSpace::Normalized1000), (100, 200));
        assert_eq!(map_point(&s, 250.0, 500.0, CoordinateSpace::Normalized1000), (350, 450));
        assert_eq!(map_point(&s, 1000.0, 1000.0, CoordinateSpace::Normalized1000), (1099, 699));
        assert_eq!(map_point(&s, 0.25, 0.5, CoordinateSpace::Normalized1), (350, 450));
        assert_eq!(map_point(&s, 1.0, 1.0, CoordinateSpace::Normalized1), (1099, 699));
        assert_eq!(map_point(&s, f64::NAN, f64::INFINITY, CoordinateSpace::Normalized1), (100, 200));
    }

    #[test]
    fn map_non_uniform_scale() {
        let mut s = shot(800, 600, 1.0, -50, 10);
        s.scale_x = 1.5;
        s.scale_y = 1.25;
        assert_eq!(map_point(&s, 100.0, 100.0, CoordinateSpace::Pixels), (100, 135));
    }

    #[test]
    fn absolute_mouse_coordinates() {
        // Virtual screen spanning (-1920,0)..(1920,1080).
        assert_eq!(to_absolute(-1920, 0, -1920, 0, 3840, 1080), (0, 0));
        assert_eq!(to_absolute(1919, 1079, -1920, 0, 3840, 1080), (65535, 65535));
        let (ax, _) = to_absolute(0, 0, -1920, 0, 3840, 1080);
        assert!((ax - 32776).abs() <= 1, "{ax}");
        // Out-of-range points clamp.
        assert_eq!(to_absolute(-5000, 5000, -1920, 0, 3840, 1080), (0, 65535));
        assert_eq!(to_absolute(5, 5, 0, 0, 1, 1), (0, 0));
    }

    #[test]
    fn rect_centers() {
        assert_eq!(rect_center(&Rect { x: -100.0, y: 10.0, width: 50.0, height: 21.0 }), (-75, 21));
    }
}
