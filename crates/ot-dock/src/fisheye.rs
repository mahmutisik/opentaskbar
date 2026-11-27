//! Dock magnification mathematics, ported value for value from OpenTaskbar's `DockFisheyeLayout` and the
//! magnification constants in `DockMetrics`: raised-cosine falloff, closed-form damped spring, slot expansion within
//! a budget and the small outward lift. All lengths are DIPs.

/// Peak magnification (`DockMetrics.HoverScale`).
pub const HOVER_SCALE: f64 = 1.5;
/// Fallback influence radius (`DockMetrics.InfluenceRadius`).
pub const INFLUENCE_RADIUS: f64 = 135.0;
/// Headroom above the shelf (`DockMetrics.Headroom`).
pub const HEADROOM: f64 = 24.0;
/// Ratio of slot growth to icon scale growth.
pub const SLOT_EXPANSION_FACTOR: f64 = 0.95;
/// Extra lift away from the screen edge at peak magnification.
pub const MAX_LIFT: f64 = 1.0;
/// Minimum gap kept between neighbours when slots cannot expand.
pub const MINIMUM_NO_OVERLAP_GAP: f64 = 4.0;
pub const SPRING_STIFFNESS: f64 = 280.0;
pub const SPRING_DAMPING: f64 = 30.0;
const FULL_ARCH_WEIGHT_SUM: f64 = 2.8;
/// Scale above which the larger icon variant is shown; it goes back below `HOVER_ICON_RELEASE`.
pub const HOVER_ICON_THRESHOLD: f64 = 1.08;
pub const HOVER_ICON_RELEASE: f64 = 1.06;

fn constrained_radius(cell: f64) -> f64 {
    (cell * 2.1).max(95.0)
}

fn free_radius(cell: f64) -> f64 {
    (cell * 2.8).max(110.0)
}

/// 1 at distance 0, 0 at `radius` and beyond, with zero slope at both ends.
pub fn falloff(distance: f64, radius: f64) -> f64 {
    if !distance.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return 0.0;
    }
    let d = distance.abs();
    if d >= radius {
        0.0
    } else if d <= 0.0 {
        1.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * d / radius).cos())
    }
}

/// Peak scale that keeps the hovered icon and its neighbour apart when slots cannot grow.
pub fn no_overlap_max_scale(cell: f64, icon: f64, configured_max: f64) -> f64 {
    if !cell.is_finite() || !icon.is_finite() || icon <= 0.0 || configured_max <= 1.0 {
        return 1.0;
    }
    let neighbour = falloff(cell, constrained_radius(cell));
    let combined_slope = 0.5 * (1.0 + neighbour);
    let margin = (cell - MINIMUM_NO_OVERLAP_GAP - icon).max(0.0);
    let safe_delta = margin / (icon * combined_slope);
    (1.0 + safe_delta).clamp(1.08, configured_max)
}

/// How much of the configured magnification the expansion budget can afford (0..1).
pub fn expansion_fraction(cell: f64, configured_max: f64, available: f64) -> f64 {
    if !configured_max.is_finite()
        || configured_max <= 1.0
        || !available.is_finite()
        || available <= 0.0
    {
        return 0.0;
    }
    let need =
        FULL_ARCH_WEIGHT_SUM * (configured_max - 1.0) * cell.max(1.0) * SLOT_EXPANSION_FACTOR;
    (available / need.max(1.0)).clamp(0.0, 1.0)
}

/// Peak scale for the current budget: full magnification with room to grow, no overlap without it.
pub fn effective_max_scale(cell: f64, icon: f64, configured_max: f64, available: f64) -> f64 {
    if !configured_max.is_finite() || configured_max <= 1.0 {
        return 1.0;
    }
    let no_overlap = no_overlap_max_scale(cell, icon, configured_max);
    let fraction = expansion_fraction(cell, configured_max, available);
    no_overlap + (configured_max - no_overlap) * fraction
}

pub fn influence_radius(cell: f64, fraction: f64) -> f64 {
    if !cell.is_finite() || cell <= 0.0 {
        return INFLUENCE_RADIUS;
    }
    let constrained = constrained_radius(cell);
    constrained + (free_radius(cell) - constrained) * fraction.clamp(0.0, 1.0)
}

pub fn target_scale(distance: f64, radius: f64, max_scale: f64) -> f64 {
    if !max_scale.is_finite() || max_scale <= 1.0 {
        return 1.0;
    }
    1.0 + (max_scale - 1.0) * falloff(distance, radius)
}

/// One spring step, exact for any frame time. Returns (value, velocity, settled).
pub fn step_spring(
    current: f64,
    target: f64,
    velocity: f64,
    dt_seconds: f64,
    max_scale: f64,
) -> (f64, f64, bool) {
    let dt = if dt_seconds.is_finite() {
        dt_seconds.clamp(0.001, 0.05)
    } else {
        0.016
    };
    let omega = SPRING_STIFFNESS.sqrt();
    let eta = SPRING_DAMPING / 2.0;
    let omega_d = (omega * omega - eta * eta).max(1.0).sqrt();
    let decay = (-eta * dt).exp();
    let (sin, cos) = (omega_d * dt).sin_cos();
    let offset = current - target;
    let temp = (velocity + eta * offset) / omega_d;
    let next = target + decay * (offset * cos + temp * sin);
    let next_velocity = decay * (velocity * cos - (omega_d * offset + eta * temp) * sin);
    let next = next.clamp(1.0, max_scale.max(1.0) + 0.02);
    if (target - next).abs() < 0.004 && next_velocity.abs() < 0.02 {
        (target, 0.0, true)
    } else {
        (next, next_velocity, false)
    }
}

/// Slot growth per item for the given scales, scaled down together to fit `available`. Returns the total.
pub fn slot_extras(scales: &[f64], base_sizes: &[f64], available: f64, extras: &mut [f64]) -> f64 {
    let count = scales.len().min(base_sizes.len()).min(extras.len());
    let mut desired_total = 0.0;
    for i in 0..count {
        let desired = (scales[i] - 1.0).max(0.0) * base_sizes[i].max(0.0) * SLOT_EXPANSION_FACTOR;
        extras[i] = desired;
        desired_total += desired;
    }
    if desired_total <= 0.001 || !available.is_finite() || available <= 0.0 {
        extras[..count].fill(0.0);
        return 0.0;
    }
    if desired_total <= available {
        return desired_total;
    }
    let ratio = available / desired_total;
    let mut total = 0.0;
    for extra in &mut extras[..count] {
        *extra *= ratio;
        total += *extra;
    }
    total
}

/// Outward lift for a scale, limited so the grown icon stays inside the headroom.
pub fn safe_lift(scale: f64, max_scale: f64, icon: f64, headroom: f64) -> f64 {
    if scale <= 1.001 || max_scale <= 1.0 {
        return 0.0;
    }
    let progress = ((scale - 1.0) / (max_scale - 1.0)).clamp(0.0, 1.0);
    let growth = icon.max(0.0) * (scale - 1.0);
    let remaining = (headroom - growth - 1.0).max(0.0);
    (MAX_LIFT * progress).min(remaining)
}

/// `ComputePreviewClearance`: extra gap (DIP) above the preview gap so window previews clear the magnified peak.
pub fn preview_clearance(icon: f64, effective_max: f64, preview_gap: f64) -> f64 {
    if !icon.is_finite() || icon <= 0.0 || !effective_max.is_finite() || effective_max <= 1.0 {
        return 0.0;
    }
    let peak_growth = icon * (effective_max - 1.0) + MAX_LIFT;
    (peak_growth + 10.0 - preview_gap).ceil().max(0.0)
}

/// Smallest native icon size that covers `rest_pixels` magnified by `scale` (`ComputeHoverIconPixels`).
pub fn hover_icon_pixels(rest_pixels: u32, scale: f64) -> u32 {
    let needed = (f64::from(rest_pixels) * scale.max(1.0)).ceil() as u32;
    crate::layout::NATIVE_ICON_SIZES
        .iter()
        .copied()
        .find(|&size| size >= needed)
        .unwrap_or(needed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_clears_the_magnified_peak() {
        // OpenTaskbar's check: gap + clearance >= 38 * (1.5 - 1) + 1 + 9.
        let clearance = preview_clearance(38.0, HOVER_SCALE, 18.0);
        assert!(18.0 + clearance >= 38.0 * (HOVER_SCALE - 1.0) + MAX_LIFT + 9.0);
        assert_eq!(clearance, 12.0);
        assert_eq!(preview_clearance(38.0, 1.0, 18.0), 0.0);
    }

    #[test]
    fn falloff_is_a_smooth_bell() {
        assert_eq!(falloff(0.0, 100.0), 1.0);
        assert_eq!(falloff(100.0, 100.0), 0.0);
        assert!((falloff(50.0, 100.0) - 0.5).abs() < 1e-12);
        assert_eq!(falloff(-30.0, 100.0), falloff(30.0, 100.0));
    }

    #[test]
    fn spring_settles_like_the_csharp_one() {
        // 60 Hz frames from rest to the peak: same closed form, same frame count as OpenTaskbar.
        let (mut value, mut velocity, mut frames) = (1.0, 0.0, 0);
        loop {
            let (v, vel, settled) = step_spring(value, 1.5, velocity, 1.0 / 60.0, 1.5);
            value = v;
            velocity = vel;
            frames += 1;
            if settled {
                break;
            }
        }
        assert_eq!(value, 1.5);
        assert!((15..=30).contains(&frames), "{frames} frames");
        // Frame-rate independence: 120 Hz reaches the same state in the same time (within one frame).
        let (mut value, mut velocity, mut frames_120) = (1.0, 0.0, 0);
        loop {
            let (v, vel, settled) = step_spring(value, 1.5, velocity, 1.0 / 120.0, 1.5);
            value = v;
            velocity = vel;
            frames_120 += 1;
            if settled {
                break;
            }
        }
        assert!((frames_120 as f64 / 2.0 - frames as f64).abs() <= 1.0);
    }

    #[test]
    fn extras_fit_the_budget() {
        let scales = [1.5, 1.25, 1.0];
        let bases = [38.0, 38.0, 38.0];
        let mut extras = [0.0; 3];
        let free = slot_extras(&scales, &bases, 1000.0, &mut extras);
        assert!((free - (0.5 + 0.25) * 38.0 * 0.95).abs() < 1e-9);
        let limited = slot_extras(&scales, &bases, 10.0, &mut extras);
        assert!((limited - 10.0).abs() < 1e-9);
        assert!((extras[0] / extras[1] - 2.0).abs() < 1e-9);
        assert_eq!(slot_extras(&scales, &bases, 0.0, &mut extras), 0.0);
    }

    #[test]
    fn peak_scale_follows_the_budget() {
        let full = effective_max_scale(38.0, 28.0, HOVER_SCALE, 10_000.0);
        assert!((full - HOVER_SCALE).abs() < 1e-12);
        let none = effective_max_scale(38.0, 28.0, HOVER_SCALE, 0.0);
        assert!(none < full && none >= 1.08);
    }

    #[test]
    fn hover_pixels_pick_a_native_size() {
        assert_eq!(hover_icon_pixels(28, 1.5), 48);
        assert_eq!(hover_icon_pixels(32, 1.5), 48);
        assert_eq!(hover_icon_pixels(40, 1.5), 64);
    }
}
