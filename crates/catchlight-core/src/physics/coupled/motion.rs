//! Prescribed affine motion, separated from the strand's dynamic coordinates.
//!
//! Interpolate rotation and stretch separately: interpolating matrix columns
//! makes a pure head rotation shorten every rod between frames. Derivatives
//! are analytic, including angular acceleration's centripetal contribution.

use super::*;

pub(super) struct Motion {
    from: DMat2,
    to: DMat2,
    stretch: DMat2,
    delta: DMat2,
    angle: f64,
    turn: f64,
    duration: f64,
}

impl Motion {
    pub(super) fn new(from: Mat2, to: Mat2, duration: f64) -> Self {
        let from = usable(from);
        let to = usable(to);
        // The same reflection on both ends leaves positive-determinant
        // matrices for the polar rotation. Changing handedness is a discrete
        // edit, not a physically continuous invertible deformation.
        let mirror = if from.determinant() < 0.0 { -1.0 } else { 1.0 };
        let rotation = |m: DMat2| {
            let x = m.x_axis * mirror;
            (x.y - m.y_axis.x).atan2(x.x + m.y_axis.y)
        };
        let angle = rotation(from);
        let turn = wrap(rotation(to) - angle);
        let stretch = DMat2::from_angle(-angle) * from;
        let end = DMat2::from_angle(-(angle + turn)) * to;
        Self {
            from,
            to,
            stretch,
            delta: end - stretch,
            angle,
            turn,
            duration,
        }
    }

    /// Map, first derivative, and second derivative with respect to seconds.
    pub(super) fn sample(&self, t: f64) -> (DMat2, DMat2, DMat2) {
        if self.from == self.to {
            return (self.from, DMat2::ZERO, DMat2::ZERO);
        }
        let r = DMat2::from_angle(self.angle + self.turn * t);
        let s = self.stretch + self.delta * t;
        let omega = self.turn / self.duration;
        let ds = self.delta * (1.0 / self.duration);
        let j = DMat2::from_cols(DVec2::Y, -DVec2::X);
        let map = if t == 0.0 {
            self.from
        } else if t == 1.0 {
            self.to
        } else {
            r * s
        };
        let rate = r * (j * s * omega + ds);
        let acceleration = r * (s * (-omega * omega) + j * ds * (2.0 * omega));
        (map, rate, acceleration)
    }
}
