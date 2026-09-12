//! Angular two-way coupling with a prescribed anchor.
//!
//! Relative joint angles preserve local rod lengths and bend bounds; the
//! full carry maps them to world space, including scale and mirrors. Each
//! link's mass is its rest length, lumped at its distal joint. Endpoint
//! Jacobians give M = sum(m J^T J), so distal masses react on their ancestors.
//! A substep solves (M + h D + h² K) v_next =
//! J^T m(v_world - v_carried) + h(torque - bias). Bias includes centripetal
//! acceleration and the changing carry's cross term. Springs and stops are
//! linearized once: this is a first-order integrator, not an exact trajectory.
//!
//! Bend response scales rest subtree inertia, not individual modal
//! frequencies. Authored damping d contributes drag -ln(1-d) against relative
//! joint motion, plus a spring damping ratio of 0.1; d = 1 uses finite heavy
//! drag. Rest support projects each sprung rod's resultant gravity
//! off its drawn tangent; differences give fixed endpoint fields. Pose
//! targets move springs, not those fields. Limp rods retain gravity;
//! static settling seeds limp rods toward gravity and nudges upward sprung
//! rods off unstable equilibria, then uses relaxation within their bounds.
//!
//! The cubic soft stop starts at 75% of max bend with zero torque and slope,
//! an 8 Hz response, and outward damping of 2 sqrt(k I). Hard bounds remain:
//! (-limit - q)/h <= v_next <= (limit - q)/h. Active bounds require additional
//! direct solves. Failure or budget exhaustion retains the last valid
//! particles and frame and keeps the chain awake, including during settling.
//!
//! Rest geometry and material coefficients are cached; support and rest
//! inertia also depend on carry. Angular state stays in f64, with f32
//! particles exported once per tick. Comparing links, gravity, frame and
//! particles with the last export detects public edits. Targets are sampled
//! once per frame. Caches are runtime-only and reset on rebake.
//!
//! Two suffix sums assemble M in O(n²); Cholesky remains O(n³). Independent
//! 2/8-link locks batch in fours after anchors and targets are sampled.
//! Other sizes, incomplete groups and targets without SIMD use scalar solves.
//! Substep scratch is inline through eight links, with heap spill above that.
//!
//! Every substep solves its current matrix, including constrained solves.

mod batch;

use std::f64::consts::{PI, TAU};

use glam::{DMat2, DVec2};
use smallvec::{smallvec, SmallVec};

use super::{ChainParticle, ParticleChainData};
use crate::{Mat2, Vec2};

type Vector = SmallVec<[f64; 16]>;
type Matrix = SmallVec<[f64; 64]>;
type Points = SmallVec<[DVec2; 16]>;

const STOP_ONSET: f64 = 0.75;
const STOP_HZ: f64 = 8.0;
const BEND_SPRING_DAMPING_RATIO: f32 = 0.1;

fn wrap(angle: f64) -> f64 {
    (angle + PI).rem_euclid(TAU) - PI
}

fn cap(limit: Option<f32>) -> f64 {
    limit
        .filter(|x| x.is_finite() && *x > 0.0)
        .map_or(f64::INFINITY, |x| f64::from(x.min(1.0)) * PI)
}

/// Unit-inertia torque, positive stiffness, and outward damping coefficient.
fn stop(angle: f64, velocity: f64, limit: f64) -> (f64, f64, f64) {
    if !limit.is_finite() || limit <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let width = (1.0 - STOP_ONSET) * limit;
    // The hard bound contains integrated angles; limiting r also keeps a
    // manually displaced particle from creating an unbounded initial force.
    let r = ((angle.abs() - STOP_ONSET * limit) / width).clamp(0.0, 1.0);
    let omega = TAU * STOP_HZ;
    let stiffness = 3.0 * omega * omega * r * r;
    let damping = if angle * velocity > 0.0 {
        2.0 * stiffness.sqrt()
    } else {
        0.0
    };
    (
        -angle.signum() * omega * omega * width * r.powi(3),
        stiffness,
        damping,
    )
}

fn usable(carry: Mat2) -> DMat2 {
    let carry = super::usable_carry(carry);
    DMat2::from_cols(carry.x_axis.as_dvec2(), carry.y_axis.as_dvec2())
}

fn rest_rods(chain: &ParticleChainData) -> Points {
    chain
        .links
        .iter()
        .map(|link| {
            let drawn = link.drawn.as_dvec2().try_normalize().unwrap_or(DVec2::Y);
            drawn * f64::from(link.length.max(1e-4))
        })
        .collect()
}

fn rods_at(rest: &[DVec2], q: &[f64]) -> Points {
    let mut angle = 0.0;
    rest.iter()
        .zip(q)
        .map(|(rod, bend)| {
            angle += bend;
            DMat2::from_angle(angle) * rod
        })
        .collect()
}

fn angles(chain: &ParticleChainData, rest: &[DVec2], carry: DMat2) -> Vector {
    let inverse = carry.inverse();
    let mut previous = 0.0;
    rest.iter()
        .enumerate()
        .map(|(i, drawn)| {
            let rod = inverse * (chain.particles[i + 1].pos - chain.particles[i].pos).as_dvec2();
            let rotation = if rod.length_squared() > 1e-16 {
                drawn.perp_dot(rod).atan2(drawn.dot(rod))
            } else {
                previous
            };
            let bend = wrap(rotation - previous);
            previous = rotation;
            bend
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct State {
    links: SmallVec<[super::ChainLink; 8]>,
    gravity: f32,
    rest: Points,
    masses: Vector,
    omega2: Vector,
    damping: Vector,
    limits: Vector,
    field: Points,
    inertia: Vector,
    field_carry: Option<DMat2>,
    q: Vector,
    rods: Points,
    positions: Points,
    velocities: Points,
    angular: Vector,
    motion_carry: Option<DMat2>,
    motion_rate: DMat2,
    // The public particles remain editable. Compare their last export once
    // per tick, so kicks, rebakes and hand edits invalidate angular state.
    exported: SmallVec<[ChainParticle; 16]>,
    exported_carry: Mat2,
}

impl State {
    fn new(chain: &ParticleChainData, carry: Mat2) -> Self {
        let rest = rest_rods(chain);
        let q = angles(chain, &rest, usable(carry));
        let rods = rods_at(&rest, &q);
        let omega: Vector = chain
            .links
            .iter()
            .map(|link| TAU * f64::from(link.stiffness.max(0.0)))
            .collect();
        Self {
            links: chain.links.iter().copied().collect(),
            gravity: chain.gravity,
            masses: rest.iter().map(|r| r.length()).collect(),
            omega2: omega.iter().map(|w| w * w).collect(),
            damping: chain
                .links
                .iter()
                .zip(&omega)
                .map(|(link, w)| {
                    let drag = if link.damping >= 1.0 {
                        1e6
                    } else {
                        -f64::from(1.0 - link.damping.max(0.0)).ln()
                    };
                    drag + 2.0 * f64::from(BEND_SPRING_DAMPING_RATIO) * w
                })
                .collect(),
            limits: chain.links.iter().map(|l| cap(l.limit)).collect(),
            rest,
            q,
            rods,
            positions: chain.particles.iter().map(|p| p.pos.as_dvec2()).collect(),
            velocities: chain.particles.iter().map(|p| p.vel.as_dvec2()).collect(),
            angular: smallvec![0.0; chain.links.len()],
            motion_carry: None,
            motion_rate: DMat2::ZERO,
            exported: chain.particles.iter().copied().collect(),
            exported_carry: carry,
            field: Points::new(),
            inertia: Vector::new(),
            field_carry: None,
        }
    }

    fn take(chain: &mut ParticleChainData, carry: Mat2) -> Box<Self> {
        match chain.coupled.take() {
            Some(state)
                if state.links.as_slice() == chain.links
                    && state.gravity == chain.gravity
                    && state.exported.as_slice() == chain.particles
                    && state.exported_carry == carry =>
            {
                state
            }
            _ => Box::new(Self::new(chain, carry)),
        }
    }

    fn fields(&mut self, carry: DMat2) {
        if self.field_carry == Some(carry) {
            return;
        }
        let n = self.rest.len();
        self.field.resize(n, DVec2::ZERO);
        self.inertia.clear();
        self.inertia.resize(n, 0.0);
        let mut gravity = 0.0;
        let mut distal = DVec2::ZERO;
        for i in (0..n).rev() {
            gravity += self.masses[i] * f64::from(self.gravity * self.links[i].gravity_scale);
            let mut resultant = DVec2::new(0.0, gravity);
            if self.links[i].stiffness > 0.0 {
                let tangent = (carry * self.rest[i].perp()).normalize();
                resultant -= tangent * resultant.dot(tangent);
            }
            self.field[i] = resultant - distal;
            distal = resultant;
        }
        let mut jacobian: Points = smallvec![DVec2::ZERO; n];
        for k in 0..n {
            let tangent = carry * self.rest[k].perp();
            for (i, j) in jacobian.iter_mut().enumerate().take(k + 1) {
                *j += tangent;
                self.inertia[i] += self.masses[k] * j.length_squared();
            }
        }
        self.field_carry = Some(carry);
    }

    fn export(&mut self, chain: &mut ParticleChainData) {
        for (i, p) in chain.particles.iter_mut().enumerate() {
            p.pos = self.positions[i].as_vec2();
            p.vel = self.velocities[i].as_vec2();
        }
        self.exported.clear();
        self.exported.extend_from_slice(&chain.particles);
        self.exported_carry = chain.carry;
    }
}

struct System {
    a: Matrix,
    rhs: Vector,
    lower: Vector,
    upper: Vector,
    initial: Vector,
}

impl State {
    fn system(&mut self, step: &Step, targets: &[f64]) -> System {
        let &Step {
            old_carry,
            carry,
            carry_rate,
            anchor_velocity,
            h,
            ..
        } = step;
        self.fields(carry);
        let n = self.q.len();
        let mut a: Matrix = smallvec![0.0; n * n];
        let mut rhs: Vector = smallvec![0.0; n];
        let mut velocity: Vector = smallvec![0.0; n];
        let mut jacobian: Points = smallvec![DVec2::ZERO; n];
        let mut tangents: Points = smallvec![DVec2::ZERO; n];
        let mut position = DVec2::ZERO;
        let mut bias = DVec2::ZERO;
        let mut previous_omega = 0.0;
        let cached_motion = self.motion_carry == Some(old_carry) && self.motion_rate == carry_rate;
        for k in 0..n {
            let rod = self.rods[k];
            let tangent = carry * rod.perp();
            tangents[k] = tangent;
            let omega = if cached_motion {
                previous_omega + self.angular[k]
            } else {
                let old_tangent = old_carry * rod.perp();
                let rod_velocity = self.velocities[k + 1] - self.velocities[k] - carry_rate * rod;
                rod_velocity.dot(old_tangent) / old_tangent.length_squared()
            };
            velocity[k] = if cached_motion {
                self.angular[k]
            } else {
                omega - previous_omega
            };
            previous_omega = omega;
            position += rod;
            bias += -(carry * rod) * omega * omega + 2.0 * (carry_rate * rod.perp()) * omega;
            let mass = self.masses[k];
            let momentum =
                mass * (self.velocities[k + 1] - anchor_velocity - carry_rate * position);
            let force = self.field[k] - mass * bias;
            for i in 0..=k {
                jacobian[i] += tangent;
                rhs[i] += jacobian[i].dot(momentum + h * force);
            }
        }
        // Absolute rod rotations have M_ij = distal_mass(max(i,j)) t_i.t_j.
        // Relative bends rotate every distal rod: two suffix sums transform
        // that matrix in O(n²), instead of accumulating n endpoint matrices.
        let mut distal_mass = 0.0;
        for i in (0..n).rev() {
            distal_mass += self.masses[i];
            for j in 0..=i {
                let value = distal_mass * tangents[i].dot(tangents[j]);
                a[i * n + j] = value;
                a[j * n + i] = value;
            }
        }
        for i in (0..n.saturating_sub(1)).rev() {
            for j in 0..n {
                a[i * n + j] += a[(i + 1) * n + j];
            }
        }
        for i in 0..n {
            for j in (0..n.saturating_sub(1)).rev() {
                a[i * n + j] += a[i * n + j + 1];
            }
        }
        let mut lower: Vector = smallvec![0.0; n];
        let mut upper: Vector = smallvec![0.0; n];
        for i in 0..n {
            // Use one triangle to preserve exact symmetry after suffix sums.
            for j in 0..i {
                a[j * n + i] = a[i * n + j];
            }
            let limit = self.limits[i];
            let (force, k, d) = stop(self.q[i], velocity[i], limit);
            let inertia = self.inertia[i];
            rhs[i] += h * inertia * (-self.omega2[i] * wrap(self.q[i] - targets[i]) + force);
            a[i * n + i] += inertia * (h * (self.damping[i] + d) + h * h * (self.omega2[i] + k));
            lower[i] = (-limit - self.q[i]) / h;
            upper[i] = (limit - self.q[i]) / h;
            velocity[i] = velocity[i].clamp(lower[i], upper[i]);
        }
        System {
            a,
            rhs,
            lower,
            upper,
            initial: velocity,
        }
    }
}

/// Solve an SPD system in place. The 2-link unconstrained path is analytic.
fn factor_solve(a: &mut [f64], b: &mut [f64]) -> bool {
    let n = b.len();
    if n == 2 {
        let determinant = a[0] * a[3] - a[1] * a[2];
        if !determinant.is_finite() || determinant <= 0.0 {
            return false;
        }
        let x = (b[0] * a[3] - b[1] * a[1]) / determinant;
        b[1] = (a[0] * b[1] - a[2] * b[0]) / determinant;
        b[0] = x;
        return b.iter().all(|v| v.is_finite());
    }
    if !factor(a, n) {
        return false;
    }
    substitute(a, b)
}

fn factor(a: &mut [f64], n: usize) -> bool {
    for i in 0..n {
        for j in 0..=i {
            let mut value = a[i * n + j];
            for k in 0..j {
                value -= a[i * n + k] * a[j * n + k];
            }
            if i == j {
                if !value.is_finite() || value <= 0.0 {
                    return false;
                }
                a[i * n + j] = value.sqrt();
            } else {
                a[i * n + j] = value / a[j * n + j];
            }
        }
    }
    true
}

fn substitute(a: &[f64], b: &mut [f64]) -> bool {
    let n = b.len();
    for i in 0..n {
        for j in 0..i {
            b[i] -= a[i * n + j] * b[j];
        }
        b[i] /= a[i * n + i];
    }
    for i in (0..n).rev() {
        for j in i + 1..n {
            b[i] -= a[j * n + i] * b[j];
        }
        b[i] /= a[i * n + i];
    }
    b.iter().all(|v| v.is_finite())
}

fn feasible(s: &System, x: &[f64]) -> bool {
    x.iter()
        .enumerate()
        .all(|(i, x)| x.is_finite() && *x >= s.lower[i] && *x <= s.upper[i])
}

fn solve(s: &System) -> Option<Vector> {
    let mut a = s.a.clone();
    let mut x = s.rhs.clone();
    if !factor_solve(&mut a, &mut x) {
        return None;
    }
    if feasible(s, &x) {
        Some(x)
    } else {
        constrained(s)
    }
}

/// Fix blocking velocities at their bounds, then release bounds whose
/// reaction points inward. The budget bounds work if the active set cycles.
fn constrained(s: &System) -> Option<Vector> {
    let mut x = s.initial.clone();
    let n = x.len();
    let mut active: SmallVec<[i8; 16]> = smallvec![0; n];
    let mut free: SmallVec<[usize; 16]> = SmallVec::with_capacity(n);
    let mut a = Matrix::with_capacity(n * n);
    let mut b = Vector::with_capacity(n);
    for _ in 0..(8 * n * n + 16) {
        free.clear();
        free.extend((0..n).filter(|&i| active[i] == 0));
        let m = free.len();
        a.resize(m * m, 0.0);
        b.resize(m, 0.0);
        for (ii, &i) in free.iter().enumerate() {
            b[ii] = s.rhs[i];
            for j in 0..n {
                if active[j] != 0 {
                    b[ii] -= s.a[i * n + j] * x[j];
                }
            }
            for (jj, &j) in free.iter().enumerate() {
                a[ii * m + jj] = s.a[i * n + j];
            }
        }
        if !factor_solve(&mut a, &mut b) {
            return None;
        }
        let mut alpha = 1.0_f64;
        let mut blocking = None;
        for (ii, &i) in free.iter().enumerate() {
            let delta = b[ii] - x[i];
            let (fraction, side) = if b[ii] < s.lower[i] {
                ((s.lower[i] - x[i]) / delta, -1)
            } else if b[ii] > s.upper[i] {
                ((s.upper[i] - x[i]) / delta, 1)
            } else {
                continue;
            };
            if fraction <= alpha {
                alpha = fraction.max(0.0);
                blocking = Some((i, side));
            }
        }
        for (ii, &i) in free.iter().enumerate() {
            x[i] = (x[i] + alpha * (b[ii] - x[i])).clamp(s.lower[i], s.upper[i]);
        }
        if let Some((i, side)) = blocking {
            active[i] = side;
            x[i] = if side < 0 { s.lower[i] } else { s.upper[i] };
            continue;
        }
        let mut release = None;
        let mut worst = 1e-10_f64;
        for i in 0..n {
            if active[i] == 0 {
                continue;
            }
            let gradient = s.a[i * n..(i + 1) * n]
                .iter()
                .zip(x.iter())
                .map(|(a, x)| a * x)
                .sum::<f64>()
                - s.rhs[i];
            let violation = f64::from(active[i]) * gradient / s.a[i * n + i].sqrt();
            if violation > worst {
                worst = violation;
                release = Some(i);
            }
        }
        if let Some(i) = release {
            active[i] = 0;
        } else {
            return Some(x);
        }
    }
    None
}

struct Step {
    anchor: DVec2,
    anchor_velocity: DVec2,
    old_carry: DMat2,
    carry: DMat2,
    carry_rate: DMat2,
    h: f64,
}

impl Step {
    fn stationary(anchor: DVec2, carry: DMat2, h: f64) -> Self {
        Self {
            anchor,
            anchor_velocity: DVec2::ZERO,
            old_carry: carry,
            carry,
            carry_rate: DMat2::ZERO,
            h,
        }
    }
}

impl State {
    fn advance(&mut self, step: &Step, velocity: &[f64], targets: &[f64]) -> Option<bool> {
        let mut q = self.q.clone();
        for (i, angle) in q.iter_mut().enumerate() {
            *angle = (*angle + step.h * velocity[i]).clamp(-self.limits[i], self.limits[i]);
        }
        let rods = rods_at(&self.rest, &q);
        let mut positions: Points = SmallVec::with_capacity(q.len() + 1);
        let mut velocities: Points = SmallVec::with_capacity(q.len() + 1);
        positions.push(step.anchor);
        velocities.push(step.anchor_velocity);
        let mut pos = DVec2::ZERO;
        let mut vel = DVec2::ZERO;
        let mut omega = 0.0;
        for (i, rod) in rods.iter().enumerate() {
            omega += velocity[i];
            pos += rod;
            vel += step.carry * rod.perp() * omega;
            positions.push(step.anchor + step.carry * pos);
            velocities.push(step.anchor_velocity + step.carry_rate * pos + vel);
        }
        if positions
            .iter()
            .chain(&velocities)
            .any(|p| !p.as_vec2().is_finite())
        {
            return None;
        }
        let still = step.anchor_velocity == DVec2::ZERO
            && step.old_carry == step.carry
            && (0..q.len()).all(|i| {
                let length = (step.carry * rods[i]).length() as f32;
                let old = self.positions[i + 1].as_vec2();
                super::is_projection_noise(positions[i + 1].as_vec2() - old, old, length)
                    && super::is_projection_noise(
                        (velocities[i + 1] * step.h).as_vec2(),
                        old,
                        length,
                    )
            });
        if still
            && (self.velocities.iter().all(|v| *v == DVec2::ZERO)
                || self.can_sleep(step.carry, targets, step.h))
        {
            self.velocities.fill(DVec2::ZERO);
            self.angular.fill(0.0);
            self.motion_rate = DMat2::ZERO;
            return Some(false);
        }
        let moved = positions
            .iter()
            .zip(&self.positions)
            .any(|(a, b)| a.as_vec2() != b.as_vec2());
        self.angular.clear();
        self.angular.extend_from_slice(velocity);
        self.motion_carry = Some(step.carry);
        self.motion_rate = step.carry_rate;
        self.q = q;
        self.rods = rods;
        self.positions = positions;
        self.velocities = velocities;
        Some(moved)
    }

    fn can_sleep(&mut self, carry: DMat2, targets: &[f64], h: f64) -> bool {
        let saved = self.velocities.clone();
        let angular = self.angular.clone();
        self.angular.fill(0.0);
        self.velocities.fill(DVec2::ZERO);
        let step = Step::stationary(self.positions[0], carry, h);
        let s = self.system(&step, targets);
        self.velocities = saved;
        self.angular = angular;
        let Some(velocity) = solve(&s) else {
            return false;
        };
        let mut displacement = DVec2::ZERO;
        let mut omega = 0.0;
        for (i, rod) in self.rods.iter().enumerate() {
            omega += velocity[i];
            displacement += carry * rod.perp() * omega * h;
            if !super::is_projection_noise(
                displacement.as_vec2(),
                self.positions[i + 1].as_vec2(),
                (carry * rod).length() as f32,
            ) {
                return false;
            }
        }
        true
    }
}

struct Frame<'a> {
    chain: &'a mut ParticleChainData,
    state: Box<State>,
    targets: Vector,
    start_anchor: Vec2,
    end_anchor: Vec2,
    start_carry: Mat2,
    end_carry: Mat2,
    from: Vec2,
    from_carry: Mat2,
    moved: bool,
    failed: bool,
}

impl<'a> Frame<'a> {
    fn new(chain: &'a mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) -> Self {
        if !chain.anchor_initialized || chain.particles.len() != chain.links.len() + 1 {
            chain.settle_to_rest(anchor, carry, posed);
        }
        let start_anchor = chain.anchor;
        let start_carry = chain.carry;
        let state = State::take(chain, start_carry);
        let targets = (0..chain.links.len())
            .map(|i| -f64::from(posed.get(i).copied().unwrap_or(0.0)) * PI)
            .collect();
        Self {
            chain,
            state,
            targets,
            start_anchor,
            start_carry,
            end_anchor: anchor,
            end_carry: carry,
            from: start_anchor,
            from_carry: start_carry,
            moved: false,
            failed: false,
        }
    }

    fn step(&self, k: u32, steps: u32, h: f64) -> Step {
        let t = k as f32 / steps as f32;
        let anchor = if k == steps {
            self.end_anchor
        } else {
            self.start_anchor.lerp(self.end_anchor, t)
        };
        let carry = if k == steps {
            self.end_carry
        } else {
            super::lerp_carry(self.start_carry, self.end_carry, t)
        };
        let old_carry = usable(self.from_carry);
        let carry = usable(carry);
        Step {
            anchor: anchor.as_dvec2(),
            anchor_velocity: (anchor - self.from).as_dvec2() / h,
            old_carry,
            carry,
            carry_rate: (carry - old_carry) * (1.0 / h),
            h,
        }
    }

    fn accept(&mut self, step: &Step, velocity: Option<Vector>) {
        let moved = velocity.and_then(|v| self.state.advance(step, &v, &self.targets));
        if let Some(moved) = moved {
            self.moved |= moved;
            self.from = step.anchor.as_vec2();
            self.from_carry =
                Mat2::from_cols(step.carry.x_axis.as_vec2(), step.carry.y_axis.as_vec2());
        } else {
            self.failed = true;
        }
    }

    fn finish(mut self) {
        self.chain.anchor = self.from;
        self.chain.carry = self.from_carry;
        self.chain.moved_last_tick = self.moved || self.failed;
        self.state.export(self.chain);
        self.chain.coupled = Some(self.state);
    }
}

pub(super) fn tick(
    chain: &mut ParticleChainData,
    anchor: Vec2,
    carry: Mat2,
    posed: &[f32],
    dt: f32,
    substeps: std::num::NonZeroU8,
) {
    let mut job = [super::ChainTick {
        chain,
        anchor,
        carry,
        posed: posed.iter().copied().collect(),
    }];
    tick_chains(&mut job, dt, substeps);
}

pub(super) fn tick_chains(
    jobs: &mut [super::ChainTick<'_>],
    dt: f32,
    substeps: std::num::NonZeroU8,
) {
    for job in jobs.iter_mut() {
        job.carry = super::usable_carry(job.carry);
    }
    if !dt.is_finite() || dt <= 0.0 {
        for job in jobs {
            if !job.chain.anchor_initialized
                || job.chain.particles.len() != job.chain.links.len() + 1
            {
                job.chain.settle_to_rest(job.anchor, job.carry, &job.posed);
            }
            job.chain.anchor = job.anchor;
            job.chain.carry = job.carry;
            // Repositioning changes the frame without carrying particle state.
            job.chain.coupled = None;
        }
        return;
    }
    let dt = dt.min(super::PHYSICS_MAX_DT);
    let steps = u32::from(substeps.get());
    let h = f64::from(dt / steps as f32);
    jobs.sort_unstable_by_key(|job| job.chain.links.len());
    // Sort only independent requests after all anchors and posed targets
    // have been sampled. A batch never reads another batch's output.
    let mut start = 0;
    while start < jobs.len() {
        let n = jobs[start].chain.links.len();
        let batch_len = if cfg!(any(
            target_feature = "sse2",
            all(target_arch = "aarch64", target_feature = "neon"),
            target_feature = "simd128"
        )) && matches!(n, 2 | 8)
            && jobs[start..]
                .iter()
                .take(4)
                .filter(|j| j.chain.links.len() == n)
                .count()
                == 4
        {
            4
        } else {
            1
        };
        let mut frames: SmallVec<[Frame<'_>; 4]> = jobs[start..start + batch_len]
            .iter_mut()
            .map(|job| Frame::new(job.chain, job.anchor, job.carry, &job.posed))
            .collect();
        for k in 1..=steps {
            let mut systems: SmallVec<[System; 4]> = SmallVec::new();
            let mut substeps: SmallVec<[Step; 4]> = SmallVec::new();
            for frame in &mut frames {
                let step = frame.step(k, steps, h);
                systems.push(frame.state.system(&step, &frame.targets));
                substeps.push(step);
            }
            let solutions = if batch_len == 4 {
                batch::solve(&systems)
            } else {
                smallvec![solve(&systems[0])]
            };
            for ((frame, step), velocity) in frames.iter_mut().zip(&substeps).zip(solutions) {
                if !frame.failed {
                    frame.accept(step, velocity);
                }
            }
            if frames.iter().all(|f| f.failed) {
                break;
            }
        }
        for frame in frames {
            frame.finish();
        }
        start += batch_len;
    }
}

fn relax(chain: &mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) -> bool {
    let step = Step::stationary(anchor.as_dvec2(), usable(carry), f64::from(1.0 / 240.0_f32));
    let targets: Vector = (0..chain.links.len())
        .map(|i| -f64::from(posed.get(i).copied().unwrap_or(0.0)) * PI)
        .collect();
    for _ in 0..2400 {
        let mut state = State::take(chain, carry);
        let system = state.system(&step, &targets);
        let result = solve(&system).and_then(|v| state.advance(&step, &v, &targets));
        state.export(chain);
        chain.coupled = Some(state);
        match result {
            Some(false) => return true,
            Some(true) => {}
            None => return false,
        }
    }
    false
}

pub(super) fn settle(chain: &mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) {
    chain.anchor = anchor;
    chain.carry = carry;
    chain.anchor_initialized = true;
    let rest = rest_rods(chain);
    let mut gravity: Vector = chain
        .links
        .iter()
        .zip(&rest)
        .map(|(link, rod)| rod.length() * f64::from(chain.gravity * link.gravity_scale))
        .collect();
    for i in (1..gravity.len()).rev() {
        gravity[i - 1] += gravity[i];
    }
    let down = usable(carry).transpose() * DVec2::Y;
    let mut parent = 0.0;
    let q: Vector = chain
        .links
        .iter()
        .enumerate()
        .map(|(i, link)| {
            let mut bend = -f64::from(posed.get(i).copied().unwrap_or(0.0)) * PI;
            if gravity[i].is_finite() && gravity[i] != 0.0 {
                let target = down * gravity[i].signum();
                let toward_gravity =
                    wrap(rest[i].perp_dot(target).atan2(rest[i].dot(target)) - parent);
                if link.stiffness <= 0.0 {
                    bend = toward_gravity;
                } else if rest[i].dot(target) < 0.0 {
                    // Break an inverted zero-torque equilibrium. Relaxation
                    // returns strong springs to the drawing and lets weak
                    // ones sag, without a separate stability solve.
                    bend += toward_gravity.clamp(-1e-3, 1e-3);
                }
            }
            bend = bend.clamp(-cap(link.limit), cap(link.limit));
            parent += bend;
            bend
        })
        .collect();
    let rods = rods_at(&rest, &q);
    chain.particles.clear();
    chain.particles.push(ChainParticle {
        pos: anchor,
        vel: Vec2::ZERO,
    });
    let mut pos = anchor.as_dvec2();
    for rod in rods {
        pos += usable(carry) * rod;
        chain.particles.push(ChainParticle {
            pos: pos.as_vec2(),
            vel: Vec2::ZERO,
        });
    }
    // Off the hot path. Damping is temporarily increased to settle faster,
    // never written back to authored data.
    let damping: SmallVec<[f32; 16]> = chain.links.iter().map(|l| l.damping).collect();
    for link in &mut chain.links {
        link.damping = 0.9999;
    }
    relax(chain, anchor, carry, posed);
    for (link, damping) in chain.links.iter_mut().zip(damping) {
        link.damping = damping;
    }
    // Heavy relaxation can fall below the storage floor before the authored
    // damping would. Finish on the actual equations, so the next ordinary
    // tick does not release a residual that the relaxation masked.
    let settled = relax(chain, anchor, carry, posed);
    for particle in &mut chain.particles {
        particle.vel = Vec2::ZERO;
    }
    chain.moved_last_tick = !settled;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{ChainLink, DEFAULT_CHAIN_SUBSTEPS};

    fn chain(n: usize) -> ParticleChainData {
        let mut chain = ParticleChainData::new(vec![
            ChainLink {
                length: 160.0 / n as f32,
                stiffness: 4.0,
                damping: 0.3,
                limit: Some(14.0 / 180.0),
                ..ChainLink::default()
            };
            n
        ]);
        chain.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        chain
    }

    fn set_state(chain: &mut ParticleChainData, q: &[f64], v: &[f64]) {
        let rods = rods_at(&rest_rods(chain), q);
        let mut pos = chain.anchor.as_dvec2();
        let mut vel = DVec2::ZERO;
        let mut omega = 0.0;
        for (i, rod) in rods.iter().enumerate() {
            omega += v[i];
            pos += usable(chain.carry) * rod;
            vel += usable(chain.carry) * rod.perp() * omega;
            chain.particles[i + 1] = ChainParticle {
                pos: pos.as_vec2(),
                vel: vel.as_vec2(),
            };
        }
    }

    #[test]
    fn configured_substeps_match_individually_swept_steps() {
        for n in [2, 8] {
            for count in [1, 4, 8, 16] {
                for dt in [1.0 / 30.0, 1.0 / 60.0, 1.0 / 144.0] {
                    let mut whole = chain(n);
                    set_state(&mut whole, &vec![0.02; n], &vec![0.1; n]);
                    let mut sliced = whole.clone();
                    let anchor = Vec2::new(3.0, 1.0);
                    let carry = Mat2::from_angle(0.03);
                    whole.tick(
                        anchor,
                        carry,
                        &[],
                        dt,
                        std::num::NonZeroU8::new(count).unwrap(),
                    );
                    for k in 1..=count {
                        let t = f32::from(k) / f32::from(count);
                        sliced.tick(
                            anchor * t,
                            super::super::lerp_carry(Mat2::IDENTITY, carry, t),
                            &[],
                            dt / f32::from(count),
                            std::num::NonZeroU8::MIN,
                        );
                    }
                    for (a, b) in whole.particles.iter().zip(&sliced.particles) {
                        assert!(
                            a.pos.distance(b.pos) < 1e-3,
                            "{n}, {count}, {dt}: {a:?} vs {b:?}"
                        );
                    }
                }
            }
        }
    }

    fn kkt(s: &System, x: &[f64]) -> f64 {
        let n = x.len();
        (0..n)
            .map(|i| {
                let gradient = s.a[i * n..(i + 1) * n]
                    .iter()
                    .zip(x)
                    .map(|(a, x)| a * x)
                    .sum::<f64>()
                    - s.rhs[i];
                let violation = if x[i] <= s.lower[i] + 1e-9 {
                    (-gradient).max(0.0)
                } else if x[i] >= s.upper[i] - 1e-9 {
                    gradient.max(0.0)
                } else {
                    gradient.abs()
                };
                violation / s.a[i * n + i].sqrt()
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn suffix_mass_assembly_matches_endpoint_kinetic_energy() {
        for n in [1, 2, 3, 8, 17, 32] {
            let mut c = chain(n);
            for (i, l) in c.links.iter_mut().enumerate() {
                l.length = 3.0 + ((i * 7) % 19) as f32;
                l.drawn = Mat2::from_angle(0.17 * i as f32) * Vec2::Y;
                l.stiffness = 0.0;
                l.damping = 0.0;
                l.limit = None;
            }
            c.carry = Mat2::from_cols(Vec2::new(-1.3, 0.2), Vec2::new(0.3, 0.7));
            set_state(
                &mut c,
                &(0..n).map(|i| 0.2 * (i as f64).sin()).collect::<Vec<_>>(),
                &vec![0.0; n],
            );
            let mut state = State::new(&c, c.carry);
            let carry = usable(c.carry);
            let step = Step::stationary(c.anchor.as_dvec2(), carry, 0.0);
            let s = state.system(&step, &vec![0.0; n]);
            // Compare kinetic energy directly rather than duplicate the
            // optimized matrix construction. Each endpoint moves by J v.
            for seed in 0..8 {
                let velocity: Vec<_> = (0..n).map(|i| ((i * 11 + seed) as f64).sin()).collect();
                let mut endpoint = DVec2::ZERO;
                let mut omega = 0.0;
                let mut expected = 0.0;
                for (i, rod) in state.rods.iter().enumerate() {
                    omega += velocity[i];
                    endpoint += carry * rod.perp() * omega;
                    expected += state.masses[i] * endpoint.length_squared();
                }
                let actual: f64 = (0..n)
                    .map(|i| {
                        (0..n)
                            .map(|j| velocity[i] * s.a[i * n + j] * velocity[j])
                            .sum::<f64>()
                    })
                    .sum();
                assert!(
                    (expected - actual).abs() < 1e-10 * expected.max(1.0),
                    "{n}: {expected} vs {actual}"
                );
            }
        }
    }

    #[test]
    fn batched_locks_match_independent_ticks_with_edits_and_moving_frames() {
        // Two complete SIMD batches, remainders and arbitrary-sized scalar
        // chains; every lane has different fields, poses and bound activity.
        let sizes = [8, 2, 8, 2, 3, 8, 2, 8, 2, 17, 8, 2];
        let mut batched: Vec<_> = sizes
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let mut c = chain(n);
                c.gravity = 9800.0;
                for (j, l) in c.links.iter_mut().enumerate() {
                    l.stiffness = 2.0 + (i + j) as f32 * 0.2;
                    l.limit = Some((8.0 + j as f32) / 180.0);
                }
                c
            })
            .collect();
        let mut scalar = batched.clone();
        for frame in 1..=160 {
            let t = frame as f32 / 60.0;
            if frame == 80 {
                for chains in [&mut batched, &mut scalar] {
                    chains[0].particles[8].pos.x += 4.0;
                    chains[1].links[0].length *= 0.9;
                    chains[2].links[3].damping = 0.9;
                    chains[3].gravity *= 0.5;
                }
            }
            let inputs: Vec<_> = (0..sizes.len())
                .map(|i| {
                    let anchor = Vec2::new((5.0 + i as f32 * 10.0) * (t * 6.0).sin(), 0.0);
                    let carry = if i % 3 == 0 {
                        Mat2::from_angle(0.1 * t.sin())
                    } else {
                        Mat2::IDENTITY
                    };
                    let posed: SmallVec<[f32; 8]> =
                        (0..sizes[i]).map(|j| 0.02 * (t + j as f32).sin()).collect();
                    (anchor, carry, posed)
                })
                .collect();
            for (c, (anchor, carry, posed)) in scalar.iter_mut().zip(&inputs) {
                c.tick(*anchor, *carry, posed, 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            }
            let mut jobs: Vec<_> = batched
                .iter_mut()
                .zip(&inputs)
                .map(|(chain, (anchor, carry, posed))| super::super::ChainTick {
                    chain,
                    anchor: *anchor,
                    carry: *carry,
                    posed: posed.clone(),
                })
                .collect();
            tick_chains(&mut jobs, 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            for (a, b) in batched.iter().zip(&scalar) {
                assert_eq!(a.anchor, b.anchor);
                assert_eq!(a.carry, b.carry);
                for (a, b) in a.particles.iter().zip(&b.particles) {
                    assert!(
                        a.pos.distance(b.pos) < 2e-5,
                        "frame {frame}: {a:?} vs {b:?}"
                    );
                    assert!(
                        a.vel.distance(b.vel) < 2e-4,
                        "frame {frame}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_failed_simd_lane_does_not_freeze_other_locks() {
        let mut chains: Vec<_> = (0..4).map(|_| chain(8)).collect();
        let before = chains[1].particles.clone();
        chains[1].gravity = f32::NAN;
        let mut jobs: Vec<_> = chains
            .iter_mut()
            .map(|chain| super::super::ChainTick {
                chain,
                anchor: Vec2::new(10.0, 0.0),
                carry: Mat2::IDENTITY,
                posed: smallvec![],
            })
            .collect();
        tick_chains(&mut jobs, 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
        assert_eq!(chains[1].particles, before);
        assert_eq!(chains[1].anchor, Vec2::ZERO);
        assert!(chains[1].moved_last_tick);
        for i in [0, 2, 3] {
            assert_eq!(chains[i].anchor, Vec2::new(10.0, 0.0));
        }
        chains[1].gravity = 9800.0;
        chains[1].tick(
            Vec2::new(10.0, 0.0),
            Mat2::IDENTITY,
            &[],
            1.0 / 60.0,
            DEFAULT_CHAIN_SUBSTEPS,
        );
        assert_eq!(chains[1].anchor, Vec2::new(10.0, 0.0));
    }

    #[test]
    fn public_particle_and_geometry_edits_invalidate_cached_state() {
        let mut c = chain(8);
        c.tick(
            Vec2::new(10.0, 0.0),
            Mat2::IDENTITY,
            &[],
            1.0 / 60.0,
            DEFAULT_CHAIN_SUBSTEPS,
        );
        for change in 0..7 {
            match change {
                0 => c.particles[4].pos.x += 2.0,
                1 => c.particles[6].vel.y += 3.0,
                2 => c.links[2].drawn = Vec2::new(0.2, 1.0).normalize(),
                3 => c.links[3].length *= 1.2,
                4 => c.links[1].limit = Some(0.03),
                5 => c.gravity *= 0.8,
                _ => c.carry = Mat2::from_angle(0.1),
            }
            let mut fresh = c.clone();
            fresh.coupled = None;
            c.tick(c.anchor, c.carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            fresh.tick(
                fresh.anchor,
                fresh.carry,
                &[],
                1.0 / 60.0,
                DEFAULT_CHAIN_SUBSTEPS,
            );
            assert_eq!(c.particles, fresh.particles, "edit {change}");
        }
    }

    #[test]
    fn progressive_stop_is_continuous_and_opposes_outward_motion() {
        let limit = 14.0_f64.to_radians();
        assert_eq!(stop(limit * 0.75, 1.0, limit), (0.0, 0.0, 0.0));
        let near = stop(limit * 0.750001, 1.0, limit);
        assert!(near.0.abs() < 1e-12 && near.1 < 1e-6);
        let middle = stop(limit * 0.9, 1.0, limit);
        let wall = stop(limit, 1.0, limit);
        assert!(wall.0 < middle.0 && middle.0 < 0.0);
        assert!(wall.1 > middle.1 && wall.2 > middle.2);
        assert_eq!(stop(limit, -1.0, limit).2, 0.0);
        assert_eq!(stop(-limit, 1.0, limit).0, -wall.0);
        assert_eq!(stop(1.0, 1.0, f64::INFINITY), (0.0, 0.0, 0.0));
    }

    #[test]
    fn direct_solve_satisfies_bounded_optimality_conditions() {
        // A deterministic family includes both signs of active bounds and
        // strongly coupled matrices. Check optimality, not implementation.
        for n in [1, 2, 3, 8, 17] {
            for seed in 0..30 {
                let mut a: Matrix = smallvec![0.0; n * n];
                for i in 0..n {
                    for j in 0..n {
                        a[i * n + j] = (0..n)
                            .map(|k| {
                                let x = ((i * 7 + k * 11 + seed) as f64).sin();
                                let y = ((j * 7 + k * 11 + seed) as f64).sin();
                                x * y
                            })
                            .sum::<f64>()
                            + if i == j { 2.0 } else { 0.0 };
                    }
                }
                let s = System {
                    a,
                    rhs: (0..n)
                        .map(|i| ((i * 13 + seed) as f64).cos() * 10.0)
                        .collect(),
                    lower: smallvec![-0.4; n],
                    upper: smallvec![0.7; n],
                    initial: smallvec![0.0; n],
                };
                let exact = solve(&s).expect("a positive definite bounded system solves");
                assert!(kkt(&s, &exact) < 1e-8, "n={n}, seed={seed}");
                assert!(exact.iter().all(|x| (-0.4..=0.7).contains(x)));
            }
        }
    }

    #[test]
    fn direct_solve_rejects_singular_systems_and_nonfinite_forces() {
        for n in [2, 3] {
            let mut s = System {
                a: smallvec![0.0; n * n],
                rhs: smallvec![1.0; n],
                lower: smallvec![-1.0; n],
                upper: smallvec![1.0; n],
                initial: smallvec![0.0; n],
            };
            assert!(solve(&s).is_none());
            for i in 0..n {
                s.a[i * n + i] = 1.0;
            }
            s.rhs[0] = f64::NAN;
            assert!(solve(&s).is_none());
        }
    }

    #[test]
    fn a_failed_substep_keeps_the_last_valid_state_and_can_recover() {
        let mut c = chain(8);
        let before = c.clone();
        c.gravity = f32::NAN;
        let target = Vec2::new(5.0, 0.0);
        let carry = Mat2::from_angle(0.1);
        c.tick(target, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
        assert_eq!(c.particles, before.particles);
        assert_eq!(c.anchor, before.anchor);
        assert_eq!(c.carry, before.carry);
        assert!(!c.is_at_rest(1e-4));
        c.gravity = before.gravity;
        c.tick(target, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
        assert_eq!(c.anchor, target);
        assert_eq!(c.carry, carry);
        assert_eq!(c.particles[0].pos, target);
        assert_ne!(c.particles, before.particles);

        // Static relaxation must not turn a failed solve into a rest claim,
        // nor retain its temporary damping override.
        let damping: Vec<_> = c.links.iter().map(|link| link.damping).collect();
        c.gravity = f32::NAN;
        c.settle_to_rest(target, carry, &[]);
        assert!(!c.is_at_rest(1e-4));
        assert_eq!(
            c.links.iter().map(|link| link.damping).collect::<Vec<_>>(),
            damping
        );
    }

    #[test]
    fn direct_coupling_transmits_a_distal_bend_to_the_root() {
        let mut c = chain(2);
        c.gravity = 0.0;
        set_state(&mut c, &[0.0, 0.1], &[0.0; 2]);
        c.tick(
            Vec2::ZERO,
            Mat2::IDENTITY,
            &[],
            1.0 / 240.0,
            DEFAULT_CHAIN_SUBSTEPS,
        );
        let root = angles(&c, &rest_rods(&c), DMat2::IDENTITY)[0];
        assert!(root > 1e-5, "{root}");
    }

    #[test]
    fn free_double_pendulum_matches_instantaneous_equations() {
        let mut c = chain(2);
        for link in &mut c.links {
            link.stiffness = 0.0;
            link.damping = 0.0;
            link.limit = None;
        }
        set_state(&mut c, &[0.2, -0.1], &[0.4, -0.3]);
        let rest = rest_rods(&c);
        let q = angles(&c, &rest, DMat2::IDENTITY);
        let h = 1e-4;
        let mut state = State::new(&c, c.carry);
        let mut step = Step::stationary(c.anchor.as_dvec2(), DMat2::IDENTITY, h);
        let s = state.system(&step, &[0.0; 2]);
        let actual = solve(&s).expect("the pendulum system solves");
        // Remove the velocity projection of f32 particle storage before
        // taking a finite difference of the acceleration.
        step.h = 0.0;
        let baseline = state.system(&step, &[0.0; 2]);
        let projected = solve(&baseline).expect("the velocity projection solves");
        let l = 80.0;
        let m = 80.0;
        let theta = [q[0], q[0] + q[1]];
        let omega = [s.initial[0], s.initial[0] + s.initial[1]];
        let cross = m * l * l * (theta[0] - theta[1]).cos();
        let mut mass = [2.0 * m * l * l, cross, cross, m * l * l];
        let mut force = [
            -2.0 * m * f64::from(c.gravity) * l * theta[0].sin()
                - m * l * l * (theta[0] - theta[1]).sin() * omega[1].powi(2),
            -m * f64::from(c.gravity) * l * theta[1].sin()
                - m * l * l * (theta[1] - theta[0]).sin() * omega[0].powi(2),
        ];
        assert!(factor_solve(&mut mass, &mut force));
        let expected = [force[0], force[1] - force[0]];
        for i in 0..2 {
            assert!(((actual[i] - projected[i]) / h - expected[i]).abs() < 1e-7);
        }
    }

    #[test]
    fn drawing_is_equilibrium_with_distal_mass_and_full_carry() {
        for carry in [
            Mat2::IDENTITY,
            Mat2::from_angle(0.4),
            Mat2::from_cols(Vec2::new(-1.5, 0.2), Vec2::new(0.3, 0.8)),
        ] {
            let mut c = chain(8);
            for (i, link) in c.links.iter_mut().enumerate() {
                link.drawn = Mat2::from_angle(i as f32 * 0.12 - 0.4) * Vec2::Y;
                link.gravity_scale = 0.3 + i as f32 * 0.2;
            }
            c.settle_to_rest(Vec2::new(20.0, 10.0), carry, &[]);
            for _ in 0..120 {
                c.tick(c.anchor, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            }
            for q in angles(&c, &rest_rods(&c), usable(carry)) {
                assert!(q.abs() < 2e-5, "{q}");
            }
            assert!(c.is_at_rest(1e-4));
        }
    }

    #[test]
    fn settling_and_ticking_preserve_small_valid_carries() {
        for scale in [Vec2::new(1e-6, 5e-5), Vec2::new(-1e-6, 5e-5)] {
            let carry = Mat2::from_diagonal(scale);
            let mut c = chain(2);
            c.gravity = 0.0;
            c.settle_to_rest(Vec2::ZERO, carry, &[]);
            let expected = carry * Vec2::new(0.0, 160.0);
            assert!(c.particles[2].pos.distance(expected) < 1e-8);
            for _ in 0..120 {
                c.tick(Vec2::ZERO, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
                assert_eq!(c.carry, carry);
                assert!(c.particles[2].pos.distance(expected) < 1e-8);
            }
        }
    }

    #[test]
    fn settling_inverted_springs_finds_a_stable_pose() {
        for n in [2, 8] {
            for stiffness in [0.1, 16.0] {
                let mut c = chain(n);
                c.gravity = 9800.0;
                for link in &mut c.links {
                    link.drawn = -Vec2::Y;
                    link.stiffness = stiffness;
                }
                c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
                let tip = c.particles[n].pos;
                if stiffness == 0.1 {
                    assert!(tip.x.abs() > 1.0, "{n}: {tip}");
                } else {
                    assert!(tip.distance(Vec2::new(0.0, -160.0)) < 0.02, "{n}: {tip}");
                }
                assert!(c.is_at_rest(1e-4), "{n}, {stiffness}");
                c.particles[n].pos.x += 0.1;
                for _ in 0..1200 {
                    c.tick(
                        Vec2::ZERO,
                        Mat2::IDENTITY,
                        &[],
                        1.0 / 60.0,
                        DEFAULT_CHAIN_SUBSTEPS,
                    );
                }
                assert!(c.particles[n].pos.distance(tip) < 0.2, "{n}, {stiffness}");
            }
        }
    }

    #[test]
    fn settling_inverted_limp_links_finds_a_stable_pose() {
        for n in [2, 8] {
            for limit in [None, Some(14.0 / 180.0)] {
                let mut c = chain(n);
                c.gravity = 9800.0;
                for link in &mut c.links {
                    link.drawn = -Vec2::Y;
                    link.stiffness = 0.0;
                    link.limit = limit;
                }
                c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
                let tip = c.particles[n].pos;
                if limit.is_none() {
                    assert!(tip.distance(Vec2::new(0.0, 160.0)) < 0.01, "{n}: {tip}");
                } else {
                    assert!(tip.x.abs() > 1.0, "{n}: {tip}");
                }
                for q in angles(&c, &rest_rods(&c), DMat2::IDENTITY) {
                    assert!(q.abs() <= cap(limit) + 1e-6);
                }
                assert!(c.is_at_rest(1e-4), "{n}, {limit:?}");
                // A tiny disturbance must not release a large inverted fall.
                c.particles[n].pos.x += 0.1;
                for _ in 0..1200 {
                    c.tick(
                        Vec2::ZERO,
                        Mat2::IDENTITY,
                        &[],
                        1.0 / 60.0,
                        DEFAULT_CHAIN_SUBSTEPS,
                    );
                }
                assert!(c.particles[n].pos.distance(tip) < 0.2, "{n}, {limit:?}");
            }
        }
    }

    #[test]
    fn settling_weightless_limp_links_preserves_the_pose() {
        for gravity in [0.0, 9800.0] {
            let mut c = chain(2);
            c.gravity = gravity;
            for link in &mut c.links {
                link.drawn = -Vec2::Y;
                link.stiffness = 0.0;
                link.gravity_scale = if gravity == 0.0 { 1.0 } else { 0.0 };
            }
            let posed = [0.02, -0.01];
            let q = posed.map(|p| -f64::from(p) * PI);
            let expected: DVec2 = rods_at(&rest_rods(&c), &q).into_iter().sum();
            c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &posed);
            assert!(c.particles[2].pos.distance(expected.as_vec2()) < 1e-4);
            assert!(c.is_at_rest(1e-4));
        }
    }

    #[test]
    fn settling_limp_links_accounts_for_carry_and_distal_gravity() {
        for carry in [
            Mat2::from_angle(0.4),
            Mat2::from_cols(Vec2::new(-1.5, 0.2), Vec2::new(0.3, 0.8)),
        ] {
            let mut c = chain(2);
            for link in &mut c.links {
                link.drawn = -Vec2::Y;
                link.stiffness = 0.0;
                link.limit = None;
            }
            c.links[0].gravity_scale = 0.0;
            c.settle_to_rest(Vec2::ZERO, carry, &[]);
            // Minimum potential energy with local lengths fixed: maximize
            // each carried rod's projection onto world gravity.
            let expected = carry * (carry.transpose() * Vec2::Y).normalize() * 160.0;
            assert!(c.particles[2].pos.distance(expected) < 1e-4);
            assert!(c.is_at_rest(1e-4));
        }
    }

    #[test]
    fn lengths_and_authored_bounds_survive_strong_driving_and_scale() {
        for n in [2, 8, 17] {
            let mut c = chain(n);
            for (i, link) in c.links.iter_mut().enumerate() {
                link.limit = Some((8.0 + i as f32) / 180.0);
            }
            let carry = Mat2::from_cols(Vec2::new(-1.5, 0.1), Vec2::new(0.3, 0.8));
            c.settle_to_rest(Vec2::ZERO, carry, &[]);
            for frame in 0..180 {
                let t = frame as f32 / 60.0;
                let anchor = Vec2::new(120.0 * (t * std::f32::consts::TAU).sin(), 10.0 * t);
                c.tick(anchor, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
                assert_eq!(c.particles[0].pos, anchor);
                let inverse = carry.inverse();
                for i in 0..n {
                    let rod = inverse * (c.particles[i + 1].pos - c.particles[i].pos);
                    assert!((rod.length() - c.links[i].length).abs() < 5e-5);
                }
                for (i, q) in angles(&c, &rest_rods(&c), usable(carry)).iter().enumerate() {
                    assert!(q.abs() <= cap(c.links[i].limit) + 5e-6);
                }
            }
        }
    }

    #[test]
    fn translation_at_constant_velocity_does_not_create_drag() {
        let mut c = chain(8);
        let speed = Vec2::new(25.0, -8.0);
        for p in &mut c.particles {
            p.vel = speed;
        }
        for frame in 1..=120 {
            c.tick(
                speed * (frame as f32 / 60.0),
                Mat2::IDENTITY,
                &[],
                1.0 / 60.0,
                DEFAULT_CHAIN_SUBSTEPS,
            );
        }
        for q in angles(&c, &rest_rods(&c), DMat2::IDENTITY) {
            assert!(q.abs() < 2e-5);
        }
    }

    #[test]
    fn direct_motion_settles_after_a_kick() {
        for n in [2, 8] {
            let mut direct = chain(n);
            let q: Vector = (0..n).map(|i| 0.1 * (i as f64 + 1.0).sin()).collect();
            set_state(&mut direct, &q, &vec![0.0; n]);
            for _ in 0..1200 {
                direct.tick(
                    Vec2::ZERO,
                    Mat2::IDENTITY,
                    &[],
                    1.0 / 60.0,
                    DEFAULT_CHAIN_SUBSTEPS,
                );
            }
            assert!(direct.is_at_rest(1e-4), "{n}: {:?}", direct.particles);
        }
    }

    #[test]
    fn coupled_posed_equilibrium_stays_settled() {
        for n in [2, 8] {
            let mut c = chain(n);
            c.gravity = 9800.0;
            let mut posed = vec![0.0; n];
            posed[0] = 0.04;
            posed[n - 1] = -0.04;
            c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &posed);
            let before = c.particles.clone();
            for _ in 0..120 {
                c.tick(
                    Vec2::ZERO,
                    Mat2::IDENTITY,
                    &posed,
                    1.0 / 60.0,
                    DEFAULT_CHAIN_SUBSTEPS,
                );
            }
            assert!(c
                .particles
                .iter()
                .zip(before)
                .all(|(a, b)| a.pos.distance(b.pos) < 0.01));
            assert!(
                c.is_at_rest(1e-4),
                "{n}: {:?}, moved={}",
                c.particles,
                c.moved_last_tick
            );
        }
    }

    #[test]
    fn rotating_a_weightless_unsprung_frame_does_not_turn_the_hair() {
        let mut c = chain(8);
        c.gravity = 0.0;
        for link in &mut c.links {
            link.stiffness = 0.0;
            link.damping = 0.0;
            link.limit = None;
        }
        let tip = c.particles[8].pos;
        for frame in 1..=120 {
            let angle = 0.3 * (frame as f32 / 60.0).sin();
            c.tick(
                Vec2::ZERO,
                Mat2::from_angle(angle),
                &[],
                1.0 / 60.0,
                DEFAULT_CHAIN_SUBSTEPS,
            );
        }
        assert!(
            c.particles[8].pos.distance(tip) < 0.2,
            "{:?}",
            c.particles[8]
        );
    }
}
