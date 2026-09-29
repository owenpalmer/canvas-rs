//! Easing curves (CSS cubic-bezier) and small time helpers for the animations.

use std::time::Instant;

/// CSS cubic-bezier(x1, y1, x2, y2) at progress t (0..1).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let (cx, bx) = (3.0 * x1, 3.0 * (x2 - x1) - 3.0 * x1);
    let ax = 1.0 - cx - bx;
    let (cy, by) = (3.0 * y1, 3.0 * (y2 - y1) - 3.0 * y1);
    let ay = 1.0 - cy - by;
    let x_at = |s: f32| ((ax * s + bx) * s + cx) * s;
    let dx_at = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
    // Newton, then bisection if it doesn't settle.
    let mut s = t;
    for _ in 0..8 {
        let x = x_at(s) - t;
        if x.abs() < 1e-5 {
            return ((ay * s + by) * s + cy) * s;
        }
        let d = dx_at(s);
        if d.abs() < 1e-6 {
            break;
        }
        s -= x / d;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    s = t;
    for _ in 0..30 {
        let x = x_at(s);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t { lo = s } else { hi = s }
        s = (lo + hi) / 2.0;
    }
    ((ay * s + by) * s + cy) * s
}

pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, t)
}

pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}

pub fn ease_in_out(t: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
}

/// Seconds since `start`, as f32.
pub fn since(start: Instant) -> f32 {
    start.elapsed().as_secs_f32()
}

/// Progress 0..1 of an animation of `dur` seconds that starts `delay` seconds after `start`.
pub fn progress(start: Instant, delay: f32, dur: f32) -> f32 {
    ((since(start) - delay) / dur).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves() {
        assert!((cubic_bezier(0.0, 0.0, 1.0, 1.0, 0.3) - 0.3).abs() < 1e-3); // linear
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-3);
        assert!(ease_out(0.5) > 0.5);
        // overshoot curve (.3, 1.6, .5, 1) goes above 1 in the middle
        assert!((0..100).map(|i| cubic_bezier(0.3, 1.6, 0.5, 1.0, i as f32 / 100.0)).fold(0.0f32, f32::max) > 1.0);
    }
}
