//! Magnification state: rest geometry captured once per hover, spring state per item and the slot growth of the
//! current frame. Mirrors `MainWindow.Magnification` in OpenTaskbar without its layout-system workarounds: the
//! geometry here is plain arithmetic, so nothing is measured mid-animation. Lengths are DIPs along the bar, in
//! window coordinates.

use std::time::Instant;

use crate::fisheye::{self, HOVER_ICON_RELEASE, HOVER_ICON_THRESHOLD, HOVER_SCALE};

#[derive(Default)]
pub struct Magnifier {
    /// Items that magnify (apps); shell buttons keep their size.
    magnifies: Vec<bool>,
    rest_centers: Vec<f64>,
    /// Slot length that grows per item (the cell for apps, 0 for shell buttons).
    bases: Vec<f64>,
    cell: f64,
    /// Rest span of the magnifying items; the pointer outside it releases the effect.
    span: (f64, f64),
    /// Slot growth budget, captured at rest.
    pub available: f64,
    effective_max: f64,
    /// Frozen while the pointer stays, so one hover never changes its peak size midway.
    session_max: f64,
    pointer: Option<f64>,
    pub scales: Vec<f64>,
    velocities: Vec<f64>,
    targets: Vec<f64>,
    pub extras: Vec<f64>,
    pub total_extra: f64,
    /// Larger icon variant shown (with hysteresis).
    pub hover: Vec<bool>,
    last_frame: Option<Instant>,
    /// Geometry differs from rest (slots grown or icons scaled): the dock must restore rest when this ends.
    pub engaged: bool,
}

pub struct Frame {
    pub settled: bool,
    /// Items whose hover icon state changed this frame.
    pub hover_changes: Vec<(usize, bool)>,
}

impl Magnifier {
    pub fn is_captured(&self) -> bool {
        !self.magnifies.is_empty()
    }

    /// Captures rest geometry. Only valid while not engaged.
    pub fn capture(
        &mut self,
        magnifies: Vec<bool>,
        rest_centers: Vec<f64>,
        cell: f64,
        icon: f64,
        available: f64,
    ) {
        let count = magnifies.len();
        let mut span = (f64::INFINITY, f64::NEG_INFINITY);
        for (center, _) in rest_centers.iter().zip(&magnifies).filter(|(_, m)| **m) {
            span.0 = span.0.min(center - cell / 2.0);
            span.1 = span.1.max(center + cell / 2.0);
        }
        self.bases = magnifies
            .iter()
            .map(|m| if *m { cell } else { 0.0 })
            .collect();
        self.magnifies = magnifies;
        self.rest_centers = rest_centers;
        self.cell = cell;
        self.span = span;
        self.available = available.max(0.0);
        self.effective_max = fisheye::effective_max_scale(cell, icon, HOVER_SCALE, self.available);
        self.scales = vec![1.0; count];
        self.velocities = vec![0.0; count];
        self.targets = vec![1.0; count];
        self.extras = vec![0.0; count];
        self.total_extra = 0.0;
        self.hover = vec![false; count];
        self.last_frame = None;
    }

    /// Forgets the captured geometry (items, size or placement changed).
    pub fn invalidate(&mut self) {
        if !self.engaged {
            self.magnifies.clear();
        }
    }

    /// Peak scale this hover can reach (the budget may keep it below `HOVER_SCALE`).
    pub fn peak(&self) -> f64 {
        if self.session_max > 1.0 {
            self.session_max
        } else {
            self.effective_max
        }
    }

    /// The largest scale the layout allows (`CurrentMagnificationMaxScale`).
    pub fn effective_max(&self) -> f64 {
        self.effective_max
    }

    pub fn has_pointer(&self) -> bool {
        self.pointer.is_some()
    }

    /// Upper bound of the slot growth one hover can use (`FullArchWeightSum` with some slack), for the input region.
    pub fn max_growth(&self) -> f64 {
        let need = 2.8 * (self.effective_max - 1.0) * self.cell * fisheye::SLOT_EXPANSION_FACTOR;
        self.available.min(need * 1.25 + 2.0)
    }

    /// Pointer at `axis`. Returns false when it is outside the magnifying span (the effect is released).
    pub fn pointer_at(&mut self, axis: f64) -> bool {
        if !self.is_captured() || axis < self.span.0 || axis > self.span.1 {
            self.release();
            return false;
        }
        if self.pointer.is_none() {
            self.session_max = self.effective_max;
        }
        self.pointer = Some(axis);
        let fraction = fisheye::expansion_fraction(self.cell, HOVER_SCALE, self.available);
        let radius = fisheye::influence_radius(self.cell, fraction);
        for i in 0..self.targets.len() {
            self.targets[i] = if self.magnifies[i] {
                fisheye::target_scale(axis - self.rest_centers[i], radius, self.session_max)
            } else {
                1.0
            };
        }
        self.engaged = true;
        true
    }

    /// Pointer gone: every item springs back to rest.
    pub fn release(&mut self) {
        self.pointer = None;
        self.targets.fill(1.0);
    }

    /// Immediate rest (layout changed under the effect).
    pub fn reset(&mut self) {
        self.pointer = None;
        self.targets.fill(1.0);
        self.scales.fill(1.0);
        self.velocities.fill(0.0);
        self.extras.fill(0.0);
        self.total_extra = 0.0;
        self.hover.fill(false);
        self.last_frame = None;
        self.engaged = false;
        self.magnifies.clear();
    }

    pub fn needs_frames(&self) -> bool {
        self.engaged
            && self
                .scales
                .iter()
                .zip(&self.targets)
                .zip(&self.velocities)
                .any(|((s, t), v)| s != t || *v != 0.0)
    }

    pub fn at_rest(&self) -> bool {
        self.pointer.is_none() && self.scales.iter().all(|s| *s == 1.0)
    }

    /// Advances the springs to `now` and recomputes slot growth.
    pub fn step(&mut self, now: Instant) -> Frame {
        let dt = self
            .last_frame
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or(1.0 / 60.0)
            .clamp(0.001, 0.05);
        self.last_frame = Some(now);
        let max = if self.session_max > 1.0 {
            self.session_max
        } else {
            self.effective_max
        };
        let mut settled = true;
        let mut hover_changes = Vec::new();
        for i in 0..self.scales.len() {
            let (next, velocity, done) =
                fisheye::step_spring(self.scales[i], self.targets[i], self.velocities[i], dt, max);
            settled &= done;
            self.scales[i] = next;
            self.velocities[i] = velocity;
            let want = if self.hover[i] {
                next > HOVER_ICON_RELEASE
            } else {
                next > HOVER_ICON_THRESHOLD
            };
            if want != self.hover[i] {
                self.hover[i] = want;
                hover_changes.push((i, want));
            }
        }
        self.total_extra =
            fisheye::slot_extras(&self.scales, &self.bases, self.available, &mut self.extras);
        if settled {
            self.last_frame = None;
        }
        Frame {
            settled,
            hover_changes,
        }
    }

    /// Lift away from the screen edge for an item at its current scale.
    pub fn lift(&self, index: usize, icon: f64, headroom: f64) -> f64 {
        let max = if self.session_max > 1.0 {
            self.session_max
        } else {
            self.effective_max
        };
        fisheye::safe_lift(self.scales[index], max, icon, headroom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured() -> Magnifier {
        let mut m = Magnifier::default();
        // Start, three apps, Recycle Bin; 38 DIP cells starting at 100.
        let centers: Vec<f64> = (0..5).map(|i| 119.0 + 38.0 * f64::from(i)).collect();
        m.capture(
            vec![false, true, true, true, false],
            centers,
            38.0,
            28.0,
            500.0,
        );
        m
    }

    #[test]
    fn hover_grows_apps_only_and_springs_back() {
        let mut m = captured();
        assert!(m.pointer_at(195.0));
        let mut now = Instant::now();
        for _ in 0..60 {
            now += std::time::Duration::from_millis(16);
            m.step(now);
        }
        assert_eq!(m.scales[0], 1.0);
        assert_eq!(m.scales[4], 1.0);
        assert!((m.scales[2] - HOVER_SCALE).abs() < 1e-9, "{}", m.scales[2]);
        assert!(m.scales[1] > 1.0 && m.scales[1] < m.scales[2]);
        assert!(m.total_extra > 0.0 && m.extras[0] == 0.0);
        assert!(m.hover[2]);

        m.release();
        for _ in 0..60 {
            now += std::time::Duration::from_millis(16);
            m.step(now);
        }
        assert!(m.at_rest());
        assert_eq!(m.total_extra, 0.0);
        assert!(!m.hover[2]);
    }

    #[test]
    fn pointer_over_shell_buttons_releases() {
        let mut m = captured();
        assert!(!m.pointer_at(119.0)); // Start
        assert!(!m.pointer_at(271.0)); // Recycle Bin
        assert!(m.pointer_at(157.0));
    }
}
