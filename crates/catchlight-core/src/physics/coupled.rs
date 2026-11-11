//! Planar articulated point masses with physical forces and prescribed roots.
//!
//! Each distal joint carries a mass proportional to its link's rest length.
//! Relative angles enforce local rod lengths exactly. Endpoint Jacobians give
//! M = sum(m J^T J), so forces and constraint impulses couple both ways.
//! Gravity acts on every mass independently of bending stiffness. Torsional
//! spring energy is k * wrap(q-target)^2 / 2; k uses rest subtree inertia times
//! (2 pi * authored Hz)^2. Joint drag is the authored -ln(1-d) times that
//! inertia (d=1 uses finite heavy drag). Air drag opposes world velocity.
//! There is no gravity cancellation, implicit spring damping, or soft stop.
//!
//! The prescribed map is interpolated as rotation times stretch, including
//! mirrors. Analytic first and second derivatives enter the angular equations:
//! M qddot = J^T(F - m(Cddot r + 2 Cdot J_local qdot + C Jdot_local qdot))
//! minus spring and damping torques. World momentum is projected once at each
//! step's start when prescribed root velocity changes. Stages then use M v
//! in angular coordinates, avoiding double-counting moving-frame terms.
//!
//! A two-stage, second-order L-stable SDIRK method integrates those equations.
//! Each stage solves springs and drag implicitly and iterates nonlinear
//! geometry to an angular tolerance. Embedded position and velocity estimates
//! bound local error. Unconverged or inaccurate stages bisect their interval
//! up to eight times before retaining the last valid state and staying awake.
//! Authored substeps are a minimum; no step exceeds 1/480 second. Rendering at
//! a lower rate therefore does not silently coarsen the material's integration.
//!
//! Optional hard angular stops use a bounded SPD solve, followed by a coupled
//! velocity projection for perfectly inelastic contact. They introduce no
//! resistance before contact. Limits refer to the drawn relative bend, not
//! to world gravity. A frame that moves at any step remains awake.
//!
//! Angular state and integration use f64; public particles are exported as
//! f32 once per tick. Comparing links, gravity, frame and particles with the
//! last export detects public edits; edited bends project into authored bounds.
//! Static settling minimizes gravity plus spring potential with bounded Newton
//! steps, rather than changing damping and simulating an arbitrary duration.
//!
//! This is a 2D point-mass/rigid-link model: it has no strand collisions,
//! torsion or distributed rod inertia. Nonuniform animated scale prescribes
//! changing world geometry; it is external work, not a material deformation.

mod motion;

use std::f64::consts::{PI, TAU};

use glam::{DMat2, DVec2};
use smallvec::{smallvec, SmallVec};

use super::{ChainParticle, ParticleChainData};
use crate::{Mat2, Vec2};

type Vector = SmallVec<[f64; 16]>;
type Matrix = SmallVec<[f64; 64]>;
type Points = SmallVec<[DVec2; 16]>;

const MAX_STEP: f64 = 1.0 / 480.0;
const POSITION_TOLERANCE: f64 = 1e-5;
// Two nonzero stages of the ARS(2,2,2) implicit tableau:
// A = [[gamma, 0], [1-gamma, gamma]], b = [1-gamma, gamma].
// https://sundials.readthedocs.io/en/latest/arkode/Butcher_link.html#c.ARKODE_ARS222_ESDIRK_3_1_2
const SDIRK_GAMMA: f64 = 1.0 - std::f64::consts::FRAC_1_SQRT_2;

fn wrap(angle: f64) -> f64 {
    (angle + PI).rem_euclid(TAU) - PI
}

fn cap(limit: Option<f32>) -> f64 {
    limit
        .filter(|x| x.is_finite() && *x > 0.0)
        .map_or(f64::INFINITY, |x| f64::from(x.min(1.0)) * PI)
}

fn usable(carry: Mat2) -> DMat2 {
    let carry = super::usable_carry(carry);
    DMat2::from_cols(carry.x_axis.as_dvec2(), carry.y_axis.as_dvec2())
}

fn finite_geometry(chain: &ParticleChainData) -> bool {
    chain
        .links
        .iter()
        .all(|link| link.length.is_finite() && link.drawn.is_finite())
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
    air_drag: f64,
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
        let q: Vector = angles(chain, &rest, usable(carry))
            .iter()
            .zip(&chain.links)
            .map(|(q, l)| q.clamp(-cap(l.limit), cap(l.limit)))
            .collect();
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
                .map(|link| {
                    if link.damping >= 1.0 {
                        1e6
                    } else {
                        -f64::from(1.0 - link.damping.max(0.0)).ln()
                    }
                })
                .collect(),
            limits: chain.links.iter().map(|l| cap(l.limit)).collect(),
            air_drag: f64::from(chain.air_drag.max(0.0)),
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
                    && state.air_drag == f64::from(chain.air_drag.max(0.0))
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
        for i in 0..n {
            self.field[i] = DVec2::new(
                0.0,
                self.masses[i] * f64::from(self.gravity * self.links[i].gravity_scale),
            );
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
    /// Project world momentum onto the prescribed frame's instantaneous
    /// admissible velocities. Root velocity changes act as constraint impulses.
    fn project_velocity(&self, step: &Step) -> Option<Vector> {
        if self.motion_carry == Some(step.carry)
            && self.motion_rate == step.carry_rate
            && self.velocities[0] == step.anchor_velocity
        {
            return Some(self.angular.clone());
        }
        let n = self.q.len();
        let mut a: Matrix = smallvec![0.0; n*n];
        let mut rhs: Vector = smallvec![0.0; n];
        let mut jac: Points = smallvec![DVec2::ZERO; n];
        let mut pos = DVec2::ZERO;
        for k in 0..n {
            let tangent = step.carry * self.rods[k].perp();
            pos += self.rods[k];
            let momentum = self.masses[k]
                * (self.velocities[k + 1] - step.anchor_velocity - step.carry_rate * pos);
            for i in 0..=k {
                jac[i] += tangent;
                rhs[i] += jac[i].dot(momentum);
            }
            for i in 0..=k {
                for j in 0..=k {
                    a[i * n + j] += self.masses[k] * jac[i].dot(jac[j]);
                }
            }
        }
        factor_solve(&mut a, &mut rhs).then_some(rhs)
    }

    /// One implicit Runge--Kutta stage: q = base_q + h v. Gravity never
    /// depends on stiffness. Spring energy is k (q-target)^2 / 2 and joint
    /// damping is the authored Rayleigh dissipation alone.
    fn system_at(
        &mut self,
        step: &Step,
        targets: &[f64],
        base_q: &[f64],
        base_v: &[f64],
        guess: &[f64],
    ) -> System {
        self.fields(step.carry);
        let h = step.h;
        let n = self.q.len();
        let q: Vector = base_q.iter().zip(guess).map(|(q, v)| q + h * v).collect();
        let rods = rods_at(&self.rest, &q);
        let mut a: Matrix = smallvec![0.0; n*n];
        let mut rhs: Vector = smallvec![0.0; n];
        let mut jac: Points = smallvec![DVec2::ZERO; n];
        let mut position = DVec2::ZERO;
        let mut bias = DVec2::ZERO;
        let mut omega = 0.0;
        for k in 0..n {
            let rod = rods[k];
            let tangent = step.carry * rod.perp();
            omega += guess[k];
            position += rod;
            // Full acceleration in a prescribed affine frame. Unlike a
            // world-momentum update, the angular ODE needs both Coriolis
            // terms: its RHS starts from M * base_v in this same frame.
            bias += step.carry_acceleration * rod + 2.0 * (step.carry_rate * rod.perp()) * omega
                - (step.carry * rod) * omega * omega;
            let carried = step.anchor_velocity + step.carry_rate * position;
            let force = self.field[k] - self.masses[k] * (bias + self.air_drag * carried);
            for i in 0..=k {
                jac[i] += tangent;
                rhs[i] += h * jac[i].dot(force);
            }
            for i in 0..=k {
                for j in 0..=k {
                    a[i * n + j] += self.masses[k] * jac[i].dot(jac[j]);
                }
            }
        }
        for i in 0..n {
            rhs[i] += a[i * n..(i + 1) * n]
                .iter()
                .zip(base_v)
                .map(|(m, v)| m * v)
                .sum::<f64>();
        }
        for value in &mut a {
            *value *= 1.0 + h * self.air_drag;
        }
        let mut lower: Vector = smallvec![f64::NEG_INFINITY; n];
        let mut upper: Vector = smallvec![f64::INFINITY; n];
        let mut initial: Vector = guess.iter().copied().collect();
        for i in 0..n {
            let inertia = self.inertia[i];
            // Re-evaluate the periodic spring's branch at the stage iterate;
            // freezing it at base_q gives the wrong torque across +/- pi.
            let spring_offset = wrap(q[i] - targets[i]) - h * guess[i];
            rhs[i] -= h * inertia * self.omega2[i] * spring_offset;
            a[i * n + i] += inertia * (h * self.damping[i] + h * h * self.omega2[i]);
            if h > 0.0 {
                lower[i] = (-self.limits[i] - base_q[i]) / h;
                upper[i] = (self.limits[i] - base_q[i]) / h;
                initial[i] = initial[i].clamp(lower[i], upper[i]);
            }
        }
        System {
            a,
            rhs,
            lower,
            upper,
            initial,
        }
    }

    fn system(&mut self, step: &Step, targets: &[f64]) -> System {
        let q = self.q.clone();
        let v = self
            .project_velocity(step)
            .unwrap_or_else(|| smallvec![f64::NAN; q.len()]);
        self.system_at(step, targets, &q, &v, &v)
    }

    fn stage(&mut self, step: &Step, targets: &[f64], q: &[f64], v: &[f64]) -> Option<Vector> {
        let mut guess: Vector = v.iter().copied().collect();
        for i in 0..guess.len() {
            guess[i] = guess[i].clamp(
                (-self.limits[i] - q[i]) / step.h,
                (self.limits[i] - q[i]) / step.h,
            );
        }
        for _ in 0..16 {
            let system = self.system_at(step, targets, q, v, &guess);
            let next = solve(&system)?;
            let error = next
                .iter()
                .zip(&guess)
                .map(|(a, b)| (a - b).abs() * step.h)
                .fold(0.0, f64::max);
            guess = next;
            if error < 1e-9 {
                return Some(guess);
            }
        }
        None
    }

    /// Perfectly inelastic impact: a joint landing on a stop has zero normal
    /// relative velocity. Project with the coupled mass matrix so free-joint
    /// momentum is preserved. An inward force releases it on the next step.
    fn contact_velocity(&mut self, step: &Step, q: &[f64], v: Vector) -> Option<Vector> {
        if !(0..q.len()).any(|i| self.limits[i].is_finite() && q[i].abs() >= self.limits[i] - 1e-10)
        {
            return Some(v);
        }
        let n = q.len();
        let mut stationary = Step::stationary(step.anchor, step.carry, 0.0);
        stationary.carry_rate = step.carry_rate;
        let mut s = self.system_at(&stationary, &vec![0.0; n], q, &v, &v);
        for i in 0..n {
            if self.limits[i].is_finite() && q[i].abs() >= self.limits[i] - 1e-10 {
                s.lower[i] = 0.0;
                s.upper[i] = 0.0;
            }
            s.initial[i] = v[i].clamp(s.lower[i], s.upper[i]);
        }
        solve(&s)
    }
}

/// Solve an SPD system in place. The 2-link unconstrained path is analytic.
fn factor_solve(a: &mut [f64], b: &mut [f64]) -> bool {
    let n = b.len();
    if n == 2 {
        let determinant = a[0] * a[3] - a[1] * a[2];
        if a[0] <= 0.0 || a[3] <= 0.0 || !determinant.is_finite() || determinant <= 0.0 {
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
    carry_acceleration: DMat2,
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
            carry_acceleration: DMat2::ZERO,
            carry,
            carry_rate: DMat2::ZERO,
            h,
        }
    }
}

impl State {
    fn commit(
        &mut self,
        step: &Step,
        q: Vector,
        velocity: &[f64],
        targets: &[f64],
    ) -> Option<bool> {
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
    start_anchor: DVec2,
    end_anchor: DVec2,
    motion: motion::Motion,
    duration: f64,
    from: Vec2,
    from_carry: Mat2,
    moved: bool,
    failed: bool,
}

fn prepare(chain: &mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) -> bool {
    if !anchor.is_finite() || !finite_geometry(chain) {
        chain.moved_last_tick = true;
        return false;
    }
    if !chain.anchor_initialized || chain.particles.len() != chain.links.len() + 1 {
        chain.settle_to_rest(anchor, carry, posed);
    }
    chain.anchor_initialized && chain.particles.len() == chain.links.len() + 1
}

impl<'a> Frame<'a> {
    fn new(
        chain: &'a mut ParticleChainData,
        anchor: Vec2,
        carry: Mat2,
        posed: &[f32],
        duration: f64,
    ) -> Option<Self> {
        if !prepare(chain, anchor, carry, posed) {
            return None;
        }
        let start_anchor = chain.anchor;
        // A reflection change cannot interpolate through an invertible map.
        // Re-express the same world state in the new handedness immediately;
        // subsequent continuous motion still uses analytic frame derivatives.
        let start_carry = if chain.carry.determinant().signum() != carry.determinant().signum() {
            carry
        } else {
            chain.carry
        };
        let state = State::take(chain, start_carry);
        let targets = (0..chain.links.len())
            .map(|i| -f64::from(posed.get(i).copied().unwrap_or(0.0)) * PI)
            .collect();
        Some(Self {
            chain,
            state,
            targets,
            start_anchor: start_anchor.as_dvec2(),
            end_anchor: anchor.as_dvec2(),
            motion: motion::Motion::new(start_carry, carry, duration),
            duration,
            from: start_anchor,
            from_carry: start_carry,
            moved: false,
            failed: false,
        })
    }

    fn sample(&self, t: f64, h: f64) -> Step {
        let (carry, carry_rate, carry_acceleration) = self.motion.sample(t);
        Step {
            anchor: self.start_anchor.lerp(self.end_anchor, t),
            anchor_velocity: (self.end_anchor - self.start_anchor) / self.duration,
            old_carry: carry,
            carry,
            carry_rate,
            carry_acceleration,
            h,
        }
    }

    fn integrate(&mut self, t0: f64, t1: f64) -> Option<bool> {
        let h = (t1 - t0) * self.duration;
        let start = self.sample(t0, h);
        let q = self.state.q.clone();
        let v = self.state.project_velocity(&start)?;
        let a = SDIRK_GAMMA;
        let first = self.sample(t0 + (t1 - t0) * a, h * a);
        let v1 = self.state.stage(&first, &self.targets, &q, &v)?;
        let qbase: Vector = q
            .iter()
            .zip(&v1)
            .map(|(q, v)| q + h * (1.0 - a) * v)
            .collect();
        let vbase: Vector = v
            .iter()
            .zip(&v1)
            .map(|(v, w)| v + (1.0 - a) / a * (w - v))
            .collect();
        let end = self.sample(t1, h * a);
        let v2 = self.state.stage(&end, &self.targets, &qbase, &vbase)?;
        let q2: Vector = qbase
            .iter()
            .zip(&v2)
            .enumerate()
            .map(|(i, (q, v))| (q + h * a * v).clamp(-self.state.limits[i], self.state.limits[i]))
            .collect();
        // The first-order embedded weights [3/5, 2/5] give an independent
        // local position-error estimate. Refine excited fast modes even when
        // their implicit stages converge (stability alone is not accuracy).
        // Impact steps are nonsmooth and use the hard-contact solve instead.
        if !(0..q2.len()).any(|i| {
            self.state.limits[i].is_finite()
                && (q2[i].abs() >= self.state.limits[i] - 1e-10
                    || (q[i] + h * a * v1[i]).abs() >= self.state.limits[i] - 1e-10)
        }) {
            let rods = rods_at(&self.state.rest, &q2);
            let mut angle_error = 0.0;
            let mut angular_velocity_error = 0.0;
            let mut displacement = DVec2::ZERO;
            let mut velocity_error = DVec2::ZERO;
            let mut error = 0.0_f64;
            let mut length = 0.0;
            for i in 0..q2.len() {
                angle_error += h * (0.4 - a) * (v2[i] - v1[i]);
                angular_velocity_error += (0.4 - a) / a * ((v2[i] - vbase[i]) - (v1[i] - v[i]));
                let rod = end.carry * rods[i];
                length += rod.length();
                displacement += (end.carry * rods[i].perp()) * angle_error;
                velocity_error += (end.carry * rods[i].perp()) * angular_velocity_error;
                error = error
                    .max(displacement.length())
                    .max(velocity_error.length() * MAX_STEP);
            }
            if error > POSITION_TOLERANCE * length {
                return None;
            }
        }
        let v2 = self.state.contact_velocity(&end, &q2, v2)?;
        let mut commit = self.sample(t1, h);
        commit.old_carry = start.carry;
        let moved = self.state.commit(&commit, q2, &v2, &self.targets)?;
        self.from = commit.anchor.as_vec2();
        self.from_carry =
            Mat2::from_cols(commit.carry.x_axis.as_vec2(), commit.carry.y_axis.as_vec2());
        self.moved |= moved;
        Some(moved)
    }

    fn interval(&mut self, t0: f64, t1: f64, depth: u8) -> bool {
        if self.integrate(t0, t1).is_some() {
            return true;
        }
        // A nonlinear stage must converge before it changes particle state.
        // Retry at finer resolution; exhaustion retains the last valid state.
        if depth == 8 {
            return false;
        }
        let mid = (t0 + t1) * 0.5;
        self.interval(t0, mid, depth + 1) && self.interval(mid, t1, depth + 1)
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
    tick_chains(
        &mut [super::ChainTick {
            chain,
            anchor,
            carry,
            posed: posed.iter().copied().collect(),
        }],
        dt,
        substeps,
    );
}

pub(super) fn tick_chains(
    jobs: &mut [super::ChainTick<'_>],
    dt: f32,
    substeps: std::num::NonZeroU8,
) {
    for job in jobs {
        let carry = super::usable_carry(job.carry);
        if !dt.is_finite() || dt <= 0.0 {
            if prepare(job.chain, job.anchor, carry, &job.posed) {
                job.chain.anchor = job.anchor;
                job.chain.carry = carry;
                job.chain.coupled = None;
            }
            continue;
        }
        let duration = f64::from(dt.min(super::PHYSICS_MAX_DT));
        let steps =
            u32::from(substeps.get()).max((duration as f32 / MAX_STEP as f32).ceil() as u32);
        let Some(mut frame) = Frame::new(job.chain, job.anchor, carry, &job.posed, duration) else {
            continue;
        };
        for k in 0..steps {
            if !frame.interval(
                f64::from(k) / f64::from(steps),
                f64::from(k + 1) / f64::from(steps),
                0,
            ) {
                frame.failed = true;
                break;
            }
        }
        frame.finish();
    }
}

/// Potential, gradient and Hessian of gravity plus torsional springs.
/// The Hessian is allowed to be indefinite (an upright limp rod is unstable).
fn potential(state: &State, carry: DMat2, q: &[f64], targets: &[f64]) -> (f64, Vector, Matrix) {
    let n = q.len();
    let rods = rods_at(&state.rest, q);
    let mut value = 0.0;
    let mut gradient: Vector = smallvec![0.0;n];
    let mut hessian: Matrix = smallvec![0.0;n*n];
    let mut weight = 0.0;
    for k in (0..n).rev() {
        weight += state.masses[k] * f64::from(state.gravity * state.links[k].gravity_scale);
        value -= weight * (carry * rods[k]).y;
        let torque = -weight * (carry * rods[k].perp()).y;
        let curvature = weight * (carry * rods[k]).y;
        for i in 0..=k {
            gradient[i] += torque;
            for j in 0..=k {
                hessian[i * n + j] += curvature;
            }
        }
    }
    for i in 0..n {
        let k = state.inertia[i] * state.omega2[i];
        let angle = wrap(q[i] - targets[i]);
        value += 0.5 * k * angle * angle;
        gradient[i] += k * angle;
        hessian[i * n + i] += k;
    }
    (value, gradient, hessian)
}

/// Bounded damped Newton minimization. Static settling is not simulated
/// elapsed time and must not change the authored damping or spring forces.
fn relax(chain: &mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) -> bool {
    let mut state = State::take(chain, carry);
    let map = usable(carry);
    state.fields(map);
    let n = state.q.len();
    let targets: Vector = (0..n)
        .map(|i| -f64::from(posed.get(i).copied().unwrap_or(0.0)) * PI)
        .collect();
    let mut q = state.q.clone();
    let mut converged = false;
    for _ in 0..512 {
        let (energy, gradient, hessian) = potential(&state, map, &q, &targets);
        if !energy.is_finite() || gradient.iter().any(|g| !g.is_finite()) {
            break;
        }
        let scale = (0..n).map(|i| hessian[i * n + i].abs()).fold(1.0, f64::max);
        let mut shift = 0.0;
        let mut direction = None;
        for _ in 0..24 {
            let mut a = hessian.clone();
            for i in 0..n {
                a[i * n + i] += shift;
            }
            let system = System {
                a,
                rhs: gradient.iter().map(|g| -g).collect(),
                lower: q.iter().zip(&state.limits).map(|(q, l)| -l - q).collect(),
                upper: q.iter().zip(&state.limits).map(|(q, l)| l - q).collect(),
                initial: smallvec![0.0;n],
            };
            if let Some(d) = solve(&system) {
                direction = Some(d);
                break;
            }
            shift = if shift == 0.0 {
                scale * 1e-6
            } else {
                shift * 10.0
            };
        }
        let Some(direction) = direction else { break };
        let largest = direction.iter().map(|d| d.abs()).fold(0.0, f64::max);
        if largest < 1e-10 {
            converged = true;
            break;
        }
        let slope: f64 = gradient.iter().zip(&direction).map(|(g, d)| g * d).sum();
        let mut fraction = (0.5 / largest).min(1.0);
        let mut accepted = false;
        for _ in 0..32 {
            let trial: Vector = q
                .iter()
                .zip(&direction)
                .enumerate()
                .map(|(i, (q, d))| (q + fraction * d).clamp(-state.limits[i], state.limits[i]))
                .collect();
            let next = potential(&state, map, &trial, &targets).0;
            if next.is_finite()
                && next <= energy + 1e-4 * fraction * slope + 1e-14 * energy.abs().max(1.0)
            {
                q = trial;
                accepted = true;
                break;
            }
            fraction *= 0.5;
        }
        if !accepted {
            break;
        }
    }
    // Always export a valid minimization iterate. An unfinished solve stays
    // awake so the ordinary dynamics can continue from it.
    let step = Step::stationary(anchor.as_dvec2(), map, MAX_STEP);
    let result = state.commit(&step, q, &vec![0.0; n], &targets);
    state.export(chain);
    chain.coupled = Some(state);
    converged && result.is_some()
}

pub(super) fn settle(chain: &mut ParticleChainData, anchor: Vec2, carry: Mat2, posed: &[f32]) {
    chain.moved_last_tick = true;
    if !anchor.is_finite() || !finite_geometry(chain) {
        return;
    }
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
                } else if (DMat2::from_angle(parent + bend) * rest[i]).dot(target) < 0.0 {
                    // Break inverted zero-torque equilibria deterministically,
                    // including a downward drawing posed upward. Strong
                    // springs return to a stable upright pose; weak ones fall.
                    bend += wrap(toward_gravity - bend).clamp(-1e-3, 1e-3);
                }
            }
            bend = bend.clamp(-cap(link.limit), cap(link.limit));
            parent += bend;
            bend
        })
        .collect();
    let rods = rods_at(&rest, &q);
    let mut particles: SmallVec<[ChainParticle; 16]> = SmallVec::new();
    particles.push(ChainParticle {
        pos: anchor,
        vel: Vec2::ZERO,
    });
    let mut pos = anchor.as_dvec2();
    for rod in rods {
        pos += usable(carry) * rod;
        particles.push(ChainParticle {
            pos: pos.as_vec2(),
            vel: Vec2::ZERO,
        });
    }
    if !particles.iter().all(|p| p.pos.is_finite()) {
        return;
    }
    chain.anchor = anchor;
    chain.carry = carry;
    chain.anchor_initialized = true;
    chain.particles.clear();
    chain.particles.extend(particles);
    chain.coupled = None;
    let settled = relax(chain, anchor, carry, posed);
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
                            Mat2::from_angle(0.03 * t),
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
    fn mass_assembly_matches_endpoint_kinetic_energy() {
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
    fn multiple_locks_match_independent_ticks_with_edits_and_moving_frames() {
        // Several independent groups, remainders and arbitrary-sized
        // chains; every lane has different fields, poses and bound activity.
        let sizes = [8, 2, 8, 2, 3, 8, 2, 8, 2, 17, 8, 2];
        let mut grouped: Vec<_> = sizes
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
        let mut scalar = grouped.clone();
        for frame in 1..=160 {
            let t = frame as f32 / 60.0;
            if frame == 80 {
                for chains in [&mut grouped, &mut scalar] {
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
            let mut jobs: Vec<_> = grouped
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
            for (a, b) in grouped.iter().zip(&scalar) {
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
    fn a_failed_lock_does_not_freeze_other_locks() {
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
    fn subnormal_frame_time_is_safe_at_bend_bounds() {
        for n in [2, 8] {
            for steps in [DEFAULT_CHAIN_SUBSTEPS, std::num::NonZeroU8::MAX] {
                let mut c = chain(n);
                let posed = vec![1.0; n];
                c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &posed);
                // Anchor motion puts the cached angles exactly at a bound.
                c.tick(Vec2::X, Mat2::IDENTITY, &posed, 1.0 / 60.0, steps);
                c.tick(Vec2::ZERO, Mat2::IDENTITY, &posed, f32::from_bits(1), steps);
                assert!(c
                    .particles
                    .iter()
                    .all(|p| p.pos.is_finite() && p.vel.is_finite()));
                c.tick(Vec2::ZERO, Mat2::IDENTITY, &[], 1.0 / 60.0, steps);
                assert_eq!(c.anchor, Vec2::ZERO);
                assert!(c
                    .particles
                    .iter()
                    .all(|p| p.pos.is_finite() && p.vel.is_finite()));
            }
        }
    }

    #[test]
    fn overflowed_singular_carries_fall_back_to_identity() {
        for size in [1e20, f32::MAX] {
            let carry = Mat2::from_cols(Vec2::splat(size), Vec2::splat(size));
            let mut c = chain(2);
            let mut identity = c.clone();
            c.settle_to_rest(Vec2::ZERO, carry, &[]);
            assert_eq!(c.carry, Mat2::IDENTITY);
            assert_eq!(c.particles, identity.particles);
            c.tick(Vec2::X, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            identity.tick(
                Vec2::X,
                Mat2::IDENTITY,
                &[],
                1.0 / 60.0,
                DEFAULT_CHAIN_SUBSTEPS,
            );
            assert_eq!(c.carry, identity.carry);
            assert_eq!(c.particles, identity.particles);
        }
    }

    #[test]
    fn nonfinite_geometry_preserves_state_and_can_recover() {
        let spine = crate::spine::SpineData::new(vec![Vec2::new(0.0, 1e20)]);
        let (length, drawn) = spine.link_geometry().next().unwrap();
        let mut c = chain(2);
        let before = c.clone();
        c.links[0].length = length;
        c.links[0].drawn = drawn;
        let target = Vec2::new(5.0, 0.0);
        let carry = Mat2::from_angle(0.1);
        c.settle_to_rest(target, carry, &[]);
        assert_eq!(c.particles, before.particles);
        assert_eq!(c.anchor, before.anchor);
        assert_eq!(c.carry, before.carry);
        assert!(!c.is_at_rest(1e-4));
        for dt in [0.0, 1.0 / 60.0] {
            c.tick(target, carry, &[], dt, DEFAULT_CHAIN_SUBSTEPS);
            assert_eq!(c.particles, before.particles);
            assert_eq!(c.anchor, before.anchor);
            assert_eq!(c.carry, before.carry);
            assert!(!c.is_at_rest(1e-4));
        }
        c.links = before.links;
        c.tick(target, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
        assert_eq!(c.anchor, target);
        assert_eq!(c.carry, carry);
    }

    #[test]
    fn unrepresentable_settling_preserves_state_and_retries_initialization() {
        for initialized in [false, true] {
            for changed_count in [false, true] {
                let mut c = chain(2);
                c.anchor_initialized = initialized;
                let before = c.clone();
                if changed_count {
                    c.links.push(c.links[0]);
                }
                c.links[0].length = f32::MAX;
                let carry = Mat2::from_diagonal(Vec2::splat(2.0));
                c.settle_to_rest(Vec2::X, carry, &[]);
                assert_eq!(c.particles, before.particles);
                assert_eq!(c.anchor, before.anchor);
                assert_eq!(c.carry, before.carry);
                assert_eq!(c.anchor_initialized, initialized);
                assert!(!c.is_at_rest(1e-4));
                c.tick(Vec2::X, carry, &[], 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
                assert_eq!(c.particles, before.particles);
                assert_eq!(c.anchor, before.anchor);
                assert_eq!(c.carry, before.carry);
                assert!(!c.is_at_rest(1e-4));
                c.links[0].length = 80.0;
                c.tick(
                    Vec2::X,
                    Mat2::IDENTITY,
                    &[],
                    1.0 / 60.0,
                    DEFAULT_CHAIN_SUBSTEPS,
                );
                assert_eq!(c.anchor, Vec2::X);
                assert!(c.anchor_initialized);
                assert_eq!(c.particles.len(), c.links.len() + 1);
            }
        }
    }

    #[test]
    fn invalid_geometry_does_not_block_other_locks() {
        for invalid_count in 1..=4 {
            let mut chains: Vec<_> = (0..4).map(|_| chain(8)).collect();
            for c in &mut chains[..invalid_count] {
                c.links[0].length = f32::INFINITY;
            }
            let before = chains.clone();
            let mut jobs: Vec<_> = chains
                .iter_mut()
                .map(|chain| super::super::ChainTick {
                    chain,
                    anchor: Vec2::X,
                    carry: Mat2::IDENTITY,
                    posed: smallvec![],
                })
                .collect();
            tick_chains(&mut jobs, 1.0 / 60.0, DEFAULT_CHAIN_SUBSTEPS);
            for (c, before) in chains[..invalid_count].iter().zip(&before) {
                assert_eq!(c.particles, before.particles);
                assert_eq!(c.anchor, before.anchor);
                assert!(!c.is_at_rest(1e-4));
            }
            for c in &chains[invalid_count..] {
                assert_eq!(c.anchor, Vec2::X);
            }
        }
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
        let h = 1e-7;
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
            assert!(((actual[i] - projected[i]) / h - expected[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn sprung_links_settle_with_gravity_and_full_carry() {
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
            assert!(
                angles(&c, &rest_rods(&c), usable(carry))
                    .iter()
                    .any(|q| q.abs() > 1e-4),
                "gravity must deflect sprung links away from their drawing"
            );
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
            direct.air_drag = 4.0;
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

    /// The tip's distance from where it hangs, each frame after a kick at
    /// the root.
    fn ring(air_drag: f32, n: usize, frames: usize) -> Vec<f32> {
        let mut c = chain(n);
        c.air_drag = air_drag;
        let rest = c.particles[n].pos;
        let mut v = vec![0.0; n];
        v[0] = 2.0;
        set_state(&mut c, &vec![0.0; n], &v);
        (0..frames)
            .map(|_| {
                c.tick(
                    Vec2::ZERO,
                    Mat2::IDENTITY,
                    &[],
                    1.0 / 60.0,
                    DEFAULT_CHAIN_SUBSTEPS,
                );
                c.particles[n].pos.distance(rest)
            })
            .collect()
    }

    #[test]
    fn air_drag_settles_the_coupled_swing() {
        let late = |r: &[f32]| r[60..].iter().copied().fold(0.0, f32::max);
        for n in [2, 8] {
            let still = ring(0.0, n, 180);
            let dragged = ring(6.0, n, 180);
            assert!(
                late(&dragged) < 0.25 * late(&still),
                "{n}: {} against {} a second after the kick",
                late(&dragged),
                late(&still)
            );
        }
    }

    #[test]
    fn air_drag_trails_a_moving_anchor() {
        for (drag, n) in [(0.0, 2), (8.0, 2), (0.0, 8), (8.0, 8)] {
            let mut c = chain(n);
            c.air_drag = drag;
            // Start already gliding: zero air drag should exert no force,
            // independently of how slowly joint damping dissipates a kick.
            for p in &mut c.particles {
                p.vel = Vec2::new(200.0, 0.0);
            }
            let mut x = 0.0;
            for _ in 0..900 {
                x += 200.0 / 60.0;
                c.tick(
                    Vec2::new(x, 0.0),
                    Mat2::IDENTITY,
                    &[],
                    1.0 / 60.0,
                    DEFAULT_CHAIN_SUBSTEPS,
                );
            }
            // Gliding through still air the strand leans back against the
            // motion; without air a steady glide is no force at all.
            let lean = c.particles[n].pos.x - c.particles[0].pos.x;
            if drag == 0.0 {
                assert!(lean.abs() < 0.5, "{n}: leans {lean} with no air");
            } else {
                assert!(lean < -2.0, "{n}: leans {lean} into the air");
            }
        }
    }
}
