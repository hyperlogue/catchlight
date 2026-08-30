//! SimplePhysics drivers: pendulums that write their state into params.
//!
//! **Physics integrates in substeps sized by the driver, and damping is
//! per-second.** `SimplePhysicsData::tick` clamps `dt` to `PHYSICS_MAX_DT` and
//! splits it by `max_substep()`, derived from `RK4_STABILITY_LIMIT *
//! RK4_STEP_SAFETY` and capped at `PHYSICS_MAX_SUBSTEPS`. `angle_damping` is a
//! fraction shed per **second** (`(1 - d).powf(dt)`), so the material a model
//! describes does not change with frame rate or substep count. Applying
//! damping per step would make 60 Hz and 144 Hz render different hair.
//!
//! **Physics drivers work in a Y-down frame.** `Arena::physics_anchor` flips Y
//! going in and `Puppet::write_physics_param_outputs` conjugates
//! `world_inverse` by the same flip coming out, matching the reference
//! pendulum's gravity-toward-+Y convention.

use crate::{Mat4, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PendulumKind {
    #[default]
    RigidPendulum,
    SpringPendulum,
}

impl PendulumKind {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Pendulum" | "RigidPendulum" => Some(Self::RigidPendulum),
            "SpringPendulum" => Some(Self::SpringPendulum),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PhysicsParamMapMode {
    XY,
    YX,
    #[default]
    AngleLength,
    LengthAngle,
}

impl PhysicsParamMapMode {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "XY" => Some(Self::XY),
            "YX" => Some(Self::YX),
            "AngleLength" => Some(Self::AngleLength),
            "LengthAngle" => Some(Self::LengthAngle),
            _ => None,
        }
    }
}

/// Two-point Verlet integrator for a fixed-length pendulum anchored
/// at `anchor`. Position and previous position encode velocity
/// implicitly; damping attenuates the implicit velocity each step.
///
/// Reserved as the one-link seed for the segmented-hair particle chain:
/// free-integrate then constrain to a fixed length is exactly the primitive
/// an N-link position-based solver generalizes. The `SimplePhysics` driver
/// models use RK4 instead and do not call this.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerletPendulum {
    pub bob: Vec2,
    pub prev_bob: Vec2,
}

impl VerletPendulum {
    pub fn hanging(anchor: Vec2, length: f32) -> Self {
        let bob = anchor + Vec2::new(0.0, length);
        Self { bob, prev_bob: bob }
    }

    /// `angle_damping` is the fraction of velocity shed per **second**, so
    /// the material a model describes is independent of how the caller
    /// chops up time. Applying it once per call instead would make a
    /// 60 Hz display and a 144 Hz display render different hair, and
    /// would make any substepping scheme silently change the look.
    pub fn tick(&mut self, anchor: Vec2, gravity: Vec2, length: f32, angle_damping: f32, dt: f32) {
        let retain = (1.0 - angle_damping.clamp(0.0, 1.0)).powf(dt);
        let velocity = (self.bob - self.prev_bob) * retain;
        let free_pos = self.bob + velocity + gravity * (dt * dt);
        let offset = free_pos - anchor;
        let constrained = if offset.length_squared() > 1e-12 {
            anchor + offset.normalize() * length
        } else {
            anchor + Vec2::new(0.0, length)
        };
        self.prev_bob = self.bob;
        self.bob = constrained;
    }

    /// Angle from straight-down, in radians. Positive = +x direction.
    pub fn angle(&self, anchor: Vec2) -> f32 {
        let d = self.bob - anchor;
        f32::atan2(-d.x, d.y)
    }
}

#[derive(Debug, Clone)]
pub struct SimplePhysicsData {
    pub kind: PendulumKind,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    pub gravity: f32,
    pub length: f32,
    /// Spring resonant frequency in Hz. Only used when `kind` is
    /// `SpringPendulum`. Defaults to 1.0.
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: Vec2,
    /// Per-frame multiplicative factor driven by `outputScale.x/.y`
    /// param bindings. Reset to (1, 1) at the start of every tick, then
    /// multiplied into `output_scale` when the parameter value is read.
    pub offset_output_scale: Vec2,
    /// Bob position. For `RigidPendulum` the bob is recomputed each
    /// tick from (anchor, angle, length); the persistent state across
    /// ticks is `d_angle`. For `SpringPendulum` the bob is the
    /// integrated state and `spring_vel` is its velocity. Neither model
    /// needs a previous position, so the driver stores a bare point
    /// rather than a [`VerletPendulum`].
    pub bob: Vec2,
    pub spring_vel: Vec2,
    /// RigidPendulum angular velocity (radians/sec). Persists across
    /// ticks; the angle itself is recomputed from the current bob/anchor
    /// at the start of each rigid step.
    pub d_angle: f32,
    pub anchor: Vec2,
    /// `false` until `tick()` first sees the world-space anchor and
    /// snaps `bob` to `anchor + (0, length)`. Construction has only the
    /// node-local transform, so the world-space snap is deferred to the
    /// first tick.
    pub anchor_initialized: bool,
}

impl Default for SimplePhysicsData {
    fn default() -> Self {
        Self {
            kind: PendulumKind::RigidPendulum,
            map_mode: PhysicsParamMapMode::AngleLength,
            local_only: false,
            gravity: 9.8 * 100.0,
            length: 100.0,
            frequency: 1.0,
            angle_damping: 0.5,
            length_damping: 0.5,
            output_scale: Vec2::ONE,
            offset_output_scale: Vec2::ONE,
            bob: Vec2::ZERO,
            spring_vel: Vec2::ZERO,
            d_angle: 0.0,
            anchor: Vec2::ZERO,
            anchor_initialized: false,
        }
    }
}

const PHYSICS_MAX_DT: f32 = 10.0;

/// RK4 is stable while `|lambda| * h < 2.78` on the real axis (`2*sqrt(2)`
/// on the imaginary one), where `lambda` is the fastest eigenvalue of the
/// linearized system.
const RK4_STABILITY_LIMIT: f32 = 2.78;

/// Fraction of the stability limit the substep actually targets. Well
/// inside the bound, so local truncation error — which grows as
/// `(|lambda| * h)^5` — stays near 1%, with headroom for the `sin()`
/// nonlinearity the linearization ignores.
const RK4_STEP_SAFETY: f32 = 0.4;

/// Ceiling on substeps per `tick`, bounding the work a single call can ask
/// for however stiff the driver is. Every model that reaches the accuracy
/// target at a real frame's `dt` stays far below it.
const PHYSICS_MAX_SUBSTEPS: u32 = 1024;

#[derive(Debug, Clone, Copy)]
struct SpringParams {
    gravity: f32,
    length: f32,
    frequency: f32,
    angle_damping: f32,
    length_damping: f32,
}

/// Acceleration on the bob of a damped spring pendulum anchored at
/// `anchor`, under gravity along +Y. Combines a radial spring force around
/// the pre-gravity rest length with damping split into angular (tangential)
/// and length (radial) components using critical-damping coefficients.
fn spring_accel(bob: Vec2, vel: Vec2, anchor: Vec2, p: SpringParams) -> Vec2 {
    let spring_ksqrt = p.frequency * 2.0 * std::f32::consts::PI;
    let spring_k = spring_ksqrt * spring_ksqrt;

    let off = bob - anchor;
    let dist = off.length();
    let n = if dist > 1e-6 {
        off / dist
    } else {
        Vec2::new(0.0, 1.0)
    };

    let rest_length = if spring_k > 1e-6 {
        p.length - p.gravity / spring_k
    } else {
        p.length
    };
    let force = Vec2::new(0.0, p.gravity) - n * (dist - rest_length) * spring_k;

    let length_ratio = if p.length > 1e-6 {
        p.gravity / p.length
    } else {
        0.0
    };
    let crit_damp_angle = 2.0 * length_ratio.max(0.0).sqrt();
    let crit_damp_length = 2.0 * spring_ksqrt;

    // Rotate velocity into (tangential, radial) frame, damp each component by
    // its own critical-damping coefficient, rotate back. The forward rotation
    // is `R = [[n.y, n.x], [-n.x, n.y]]`, so the damping term must come back
    // through `R^-1 = R^T` — feeding the undamped `d_rot` into the rotate-back
    // yields neither `R^-1 D R` nor an acceleration in the x/y sum.
    let d_rot = Vec2::new(vel.x * n.y + vel.y * n.x, vel.y * n.y - vel.x * n.x);
    let dd_rot = Vec2::new(
        -d_rot.x * p.angle_damping * crit_damp_angle,
        -d_rot.y * p.length_damping * crit_damp_length,
    );
    let dd_damp = Vec2::new(
        dd_rot.x * n.y - dd_rot.y * n.x,
        dd_rot.x * n.x + dd_rot.y * n.y,
    );

    force + dd_damp
}

/// One Runge-Kutta-4 step on the 4-DOF spring system (position x2,
/// velocity x2). Writes new state into `bob`/`vel` in place. Falls
/// back to the starting state if the step produces non-finite values.
fn spring_rk4_step(bob: &mut Vec2, vel: &mut Vec2, anchor: Vec2, p: SpringParams, dt: f32) {
    let x0 = *bob;
    let v0 = *vel;

    let k1_x = v0;
    let k1_v = spring_accel(x0, v0, anchor, p);

    let x2 = x0 + k1_x * (dt * 0.5);
    let v2 = v0 + k1_v * (dt * 0.5);
    let k2_x = v2;
    let k2_v = spring_accel(x2, v2, anchor, p);

    let x3 = x0 + k2_x * (dt * 0.5);
    let v3 = v0 + k2_v * (dt * 0.5);
    let k3_x = v3;
    let k3_v = spring_accel(x3, v3, anchor, p);

    let x4 = x0 + k3_x * dt;
    let v4 = v0 + k3_v * dt;
    let k4_x = v4;
    let k4_v = spring_accel(x4, v4, anchor, p);

    let new_bob = x0 + (k1_x + k2_x * 2.0 + k3_x * 2.0 + k4_x) * (dt / 6.0);
    let new_vel = v0 + (k1_v + k2_v * 2.0 + k3_v * 2.0 + k4_v) * (dt / 6.0);

    if new_bob.is_finite() && new_vel.is_finite() {
        *bob = new_bob;
        *vel = new_vel;
    }
}

/// One Runge-Kutta-4 step on the rigid-pendulum 2-DOF system
/// `(angle, dAngle)`:
///
/// ```text
/// dAngle' = -lengthRatio * sin(angle) - dAngle * angleDamping * critDamp
/// ```
///
/// where `lengthRatio = gravity / length` and
/// `critDamp = 2 * sqrt(lengthRatio)`.
fn rigid_pendulum_rk4_step(data: &mut SimplePhysicsData, anchor: Vec2, dt: f32) {
    let length = data.length;
    if length < 1e-6 {
        data.bob = anchor;
        return;
    }

    let length_ratio = data.gravity / length;
    let crit_damp = 2.0 * length_ratio.max(0.0).sqrt();
    let damping_coef = data.angle_damping * crit_damp;

    // Recompute angle from `bob - anchor` each tick so anchor motion changes
    // the angle while dAngle (velocity) persists across ticks.
    let off = data.bob - anchor;
    let angle0 = if off.length_squared() > 1e-12 {
        f32::atan2(-off.x, off.y)
    } else {
        0.0
    };
    let dangle0 = data.d_angle;

    let f = |angle: f32, dangle: f32| -> (f32, f32) {
        // (d_angle/dt, d_dangle/dt)
        let ddangle = -length_ratio * angle.sin() - dangle * damping_coef;
        (dangle, ddangle)
    };

    let (k1a, k1d) = f(angle0, dangle0);
    let (k2a, k2d) = f(angle0 + k1a * (dt * 0.5), dangle0 + k1d * (dt * 0.5));
    let (k3a, k3d) = f(angle0 + k2a * (dt * 0.5), dangle0 + k2d * (dt * 0.5));
    let (k4a, k4d) = f(angle0 + k3a * dt, dangle0 + k3d * dt);

    let new_angle = angle0 + (k1a + 2.0 * k2a + 2.0 * k3a + k4a) * (dt / 6.0);
    let new_dangle = dangle0 + (k1d + 2.0 * k2d + 2.0 * k3d + k4d) * (dt / 6.0);

    if !new_angle.is_finite() || !new_dangle.is_finite() {
        // Leave state untouched so a non-finite step cannot poison the
        // pendulum permanently.
        return;
    }

    data.d_angle = new_dangle;
    let bob = anchor + Vec2::new(-new_angle.sin(), new_angle.cos()) * length;
    data.bob = bob;
}

impl SimplePhysicsData {
    /// Magnitude of the fastest eigenvalue of the linearized system.
    /// Both models decompose into damped oscillators `x'' + 2*d*w*x' +
    /// w^2*x = 0`, whose roots are `w*(-d +- i*sqrt(1-d^2))` when
    /// underdamped — magnitude `w` — and `-w*(d + sqrt(d^2-1))` for the
    /// fast root when overdamped. The `crit_damp` factors in
    /// `spring_accel` and `rigid_pendulum_rk4_step` are exactly `2w` for
    /// their mode, which is what makes the damping fields read as ratios.
    fn max_eigenvalue(&self) -> f32 {
        // `abs` because this is a magnitude: gravity pointing the wrong way
        // gives an unstable real eigenvalue of the same size, and reporting
        // zero there would pick the coarsest possible step for the
        // stiffest possible system.
        let omega_gravity = if self.length > 1e-6 {
            (self.gravity / self.length).abs().sqrt()
        } else {
            0.0
        };
        let mode = |omega: f32, damping: f32| {
            let d = damping.max(0.0);
            omega * (d + (d * d - 1.0).max(0.0).sqrt()).max(1.0)
        };
        let angular = mode(omega_gravity, self.angle_damping);
        match self.kind {
            PendulumKind::RigidPendulum => angular,
            PendulumKind::SpringPendulum => angular.max(mode(
                self.frequency * 2.0 * std::f32::consts::PI,
                self.length_damping,
            )),
        }
    }

    /// Largest substep that holds `|lambda| * h` at the accuracy target.
    /// Sizing it from the driver's own stiffness is what lets a soft model
    /// take one step per frame while a stiff one takes as many as its
    /// frequency demands; any single constant serves one of them badly.
    fn max_substep(&self) -> f32 {
        let lambda = self.max_eigenvalue();
        if lambda > 1e-6 {
            RK4_STABILITY_LIMIT * RK4_STEP_SAFETY / lambda
        } else {
            PHYSICS_MAX_DT
        }
    }

    /// Substeps needed to cover `dt` at the accuracy target.
    ///
    /// Capped because the count scales with the driver's own stiffness: an
    /// absurd `frequency` against a clamped 10s frame asks for tens of
    /// millions of steps, which would hang rather than merely look wrong.
    /// Past the cap a model integrates too coarsely for its stiffness, and
    /// far enough past it the step leaves RK4's stability bound entirely —
    /// which `tick` checks for and answers by settling instead.
    fn substep_count(&self, dt: f32) -> u32 {
        (dt / self.max_substep())
            .ceil()
            .clamp(1.0, PHYSICS_MAX_SUBSTEPS as f32) as u32
    }

    /// Advance the pendulum by `dt` under gravity pulling down the
    /// local +Y axis. `anchor_world` is the current world-space anchor
    /// point (typically the node's global translation). The outer dt is
    /// clamped to 10s and split into substeps sized by the driver's own
    /// stiffness so a slow frame can't throw the integrator.
    pub fn tick(&mut self, anchor_world: Vec2, dt: f32) {
        if !self.anchor_initialized {
            // Initialize from the first world-space anchor. Without this,
            // every off-origin physics chain starts with a stretched spring
            // and oscillates from the wrong state.
            self.bob = anchor_world + Vec2::new(0.0, self.length);
            self.spring_vel = Vec2::ZERO;
            self.d_angle = 0.0;
            self.anchor_initialized = true;
        }
        self.anchor = anchor_world;
        // NaN survives `clamp` and fails `<= 0.0`, and `NaN as u32` saturates
        // to 0 in `substep_count`, so an unguarded NaN dt runs zero substeps
        // and silently freezes the driver instead of stepping it.
        if !dt.is_finite() {
            return;
        }
        let clamped = dt.clamp(0.0, PHYSICS_MAX_DT);
        if clamped <= 0.0 {
            return;
        }
        // Uniform steps from an integer count: there is no accumulated
        // remainder to drift in f32, and no ragged short final step whose
        // per-step damping would land differently from its neighbours'.
        let steps = self.substep_count(clamped);
        let h = clamped / steps as f32;
        // A frame long enough that the cap bites can land outside RK4's
        // stability bound, and there the state does not merely lose accuracy
        // — it diverges, and the per-step finiteness guard leaves the bob
        // saturated at the last enormous finite iterate, which then takes
        // many good frames to decay back. A session resuming from a long
        // suspension is better served landing at rest.
        if steps == PHYSICS_MAX_SUBSTEPS && self.max_eigenvalue() * h > RK4_STABILITY_LIMIT {
            self.settle_to_rest(anchor_world);
            return;
        }
        for _ in 0..steps {
            self.step(anchor_world, h);
        }
    }

    /// Place the pendulum at its analytic equilibrium for `anchor_world`,
    /// with no simulation. Both models rest at `anchor + (0, length)`: the
    /// rigid pendulum because its equilibrium needs `sin(angle) = 0`, the
    /// spring because `rest_length` is pre-compensated by `gravity / k`
    /// precisely so the loaded spring hangs at exactly `length`.
    ///
    /// That compensation needs a spring to compensate with. A
    /// `SpringPendulum` at `frequency` ~ 0 has no restoring force, so
    /// nothing balances gravity and the model has no equilibrium to place
    /// it at; the bob is parked at `length` and falls from there.
    pub fn settle_to_rest(&mut self, anchor_world: Vec2) {
        self.anchor = anchor_world;
        self.bob = anchor_world + Vec2::new(0.0, self.length);
        self.spring_vel = Vec2::ZERO;
        self.d_angle = 0.0;
        self.anchor_initialized = true;
    }

    fn step(&mut self, anchor_world: Vec2, dt: f32) {
        match self.kind {
            PendulumKind::RigidPendulum => {
                rigid_pendulum_rk4_step(self, anchor_world, dt);
            }
            PendulumKind::SpringPendulum => {
                spring_rk4_step(
                    &mut self.bob,
                    &mut self.spring_vel,
                    anchor_world,
                    SpringParams {
                        gravity: self.gravity,
                        length: self.length,
                        frequency: self.frequency,
                        angle_damping: self.angle_damping,
                        length_damping: self.length_damping,
                    },
                    dt,
                );
            }
        }
    }

    /// Convert the current pendulum state into a 2D parameter value
    /// per the map_mode. Output is scaled by output_scale. Driver output uses
    /// a Y-up parameter frame, so the Y component is flipped at this boundary.
    ///
    /// `world_inverse` is the inverse of the node's puppet-local world
    /// matrix and rotates the world-space displacement (`bob - anchor`)
    /// back into the node's local frame before the angle is read. For
    /// `local_only=true` callers pass
    /// `Mat4::IDENTITY` (the integrator already ran in parent-local
    /// coords, so no further rotation is needed). `relative_length`
    /// stays in world space.
    pub fn param_value(&self, world_inverse: Mat4) -> Vec2 {
        let d_world = self.bob - self.anchor;
        let length = d_world.length();
        let relative_length = if self.length > 1e-6 {
            length / self.length
        } else {
            0.0
        };
        // transform_vector3 ignores the translation column so the
        // anchor-relative displacement stays anchor-relative. With a
        // pure rotation the magnitude is preserved; non-uniform scale
        // skews `dir` consistently with the node transform.
        let d_local = world_inverse
            .transform_vector3(d_world.extend(0.0))
            .truncate();
        let dir = if d_local.length_squared() > 1e-12 {
            d_local.normalize()
        } else {
            Vec2::new(0.0, 1.0)
        };

        let raw = match self.map_mode {
            PhysicsParamMapMode::XY => {
                let local = dir * relative_length;
                Vec2::new(local.x, -(local.y - 1.0))
            }
            PhysicsParamMapMode::YX => {
                let local = dir * relative_length;
                Vec2::new(-(local.y - 1.0), local.x)
            }
            PhysicsParamMapMode::AngleLength => {
                let a = f32::atan2(-dir.x, dir.y) / std::f32::consts::PI;
                Vec2::new(a, relative_length)
            }
            PhysicsParamMapMode::LengthAngle => {
                let a = f32::atan2(-dir.x, dir.y) / std::f32::consts::PI;
                Vec2::new(relative_length, a)
            }
        };

        raw * self.output_scale * self.offset_output_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanging_pendulum_stays_still_with_no_forces() {
        let anchor = Vec2::ZERO;
        let mut p = VerletPendulum::hanging(anchor, 100.0);
        for _ in 0..100 {
            p.tick(anchor, Vec2::ZERO, 100.0, 0.0, 1.0 / 60.0);
        }
        assert!((p.bob - Vec2::new(0.0, 100.0)).length() < 1e-3);
    }

    #[test]
    fn perturbed_pendulum_swings_past_rest_without_damping() {
        let anchor = Vec2::ZERO;
        let mut p = VerletPendulum {
            bob: Vec2::new(50.0, 86.6), // ~30° off vertical
            prev_bob: Vec2::new(50.0, 86.6),
        };
        let mut max_right_x = p.bob.x;
        let mut min_right_x = p.bob.x;
        for _ in 0..240 {
            p.tick(anchor, Vec2::new(0.0, 981.0), 100.0, 0.0, 1.0 / 60.0);
            max_right_x = max_right_x.max(p.bob.x);
            min_right_x = min_right_x.min(p.bob.x);
        }
        assert!(
            min_right_x < -10.0,
            "pendulum never swung left, min x = {}",
            min_right_x
        );
        assert!(
            max_right_x > 10.0,
            "pendulum never swung right, max x = {}",
            max_right_x
        );
    }

    #[test]
    fn damped_pendulum_decays_toward_rest() {
        let anchor = Vec2::ZERO;
        let mut p = VerletPendulum {
            bob: Vec2::new(50.0, 86.6),
            prev_bob: Vec2::new(50.0, 86.6),
        };
        for _ in 0..1200 {
            p.tick(anchor, Vec2::new(0.0, 981.0), 100.0, 0.15, 1.0 / 60.0);
        }
        assert!(
            p.bob.distance(Vec2::new(0.0, 100.0)) < 5.0,
            "pendulum didn't settle near rest: bob = {:?}",
            p.bob
        );
    }

    #[test]
    fn param_value_zero_at_rest_for_anglelength() {
        let mut d = SimplePhysicsData {
            length: 100.0,
            ..Default::default()
        };
        d.bob = Vec2::new(0.0, 100.0);
        let v = d.param_value(Mat4::IDENTITY);
        assert!(v.x.abs() < 1e-6, "angle at rest should be 0, got {}", v.x);
        assert!(
            (v.y - 1.0).abs() < 1e-6,
            "relative length at rest should be 1, got {}",
            v.y
        );
    }

    /// Exercise all four map modes at the same non-rest state so a swap
    /// between modes would flip exactly one assertion.
    ///
    /// State: anchor=(0,0), length=100, bob=(60,80) — lies on the rest
    /// circle (|bob|=100) tilted into the +x/+y quadrant. So
    /// relative_length=1, dir=(0.6, 0.8), local=(0.6, 0.8).
    fn tilted_pendulum(map_mode: PhysicsParamMapMode) -> SimplePhysicsData {
        SimplePhysicsData {
            length: 100.0,
            map_mode,
            bob: Vec2::new(60.0, 80.0),
            anchor: Vec2::ZERO,
            output_scale: Vec2::ONE,
            ..Default::default()
        }
    }

    #[test]
    fn param_value_xy_at_tilted_state() {
        let v = tilted_pendulum(PhysicsParamMapMode::XY).param_value(Mat4::IDENTITY);
        // XY maps dir*rel_len -> (local.x, 1 - local.y)
        assert!((v.x - 0.6).abs() < 1e-4, "XY.x: expected 0.6, got {}", v.x);
        assert!((v.y - 0.2).abs() < 1e-4, "XY.y: expected 0.2, got {}", v.y);
    }

    #[test]
    fn param_value_yx_at_tilted_state() {
        let v = tilted_pendulum(PhysicsParamMapMode::YX).param_value(Mat4::IDENTITY);
        // YX swaps XY's channels: (1 - local.y, local.x)
        assert!((v.x - 0.2).abs() < 1e-4, "YX.x: expected 0.2, got {}", v.x);
        assert!((v.y - 0.6).abs() < 1e-4, "YX.y: expected 0.6, got {}", v.y);
    }

    #[test]
    fn param_value_lengthangle_at_tilted_state() {
        let v = tilted_pendulum(PhysicsParamMapMode::LengthAngle).param_value(Mat4::IDENTITY);
        // atan2(-0.6, 0.8) ≈ -0.6435 rad; / PI ≈ -0.20483.
        let expected_angle = f32::atan2(-0.6, 0.8) / std::f32::consts::PI;
        assert!(
            (v.x - 1.0).abs() < 1e-4,
            "LA.x (length): expected 1.0, got {}",
            v.x
        );
        assert!(
            (v.y - expected_angle).abs() < 1e-4,
            "LA.y (angle): expected {}, got {}",
            expected_angle,
            v.y
        );
    }

    /// Distance, after one simulated second, between stepping `base` at a
    /// frame-realistic dt and a reference integrated 100x finer.
    ///
    /// Substep count is derived from `dt` and the driver's stiffness, so
    /// different dt splits legitimately land on different grids and cannot
    /// be compared for equality. What must hold is that a real frame's dt
    /// tracks the converged solution: an integrator that mis-sized its
    /// substep, or applied damping per step instead of per unit time,
    /// misses by orders of magnitude more than the tolerance here.
    fn substep_error_over_one_second(base: &SimplePhysicsData) -> f32 {
        let anchor = Vec2::ZERO;
        let mut frame = base.clone();
        for _ in 0..100 {
            frame.tick(anchor, 0.01);
        }
        let mut fine = base.clone();
        for _ in 0..10_000 {
            fine.tick(anchor, 0.0001);
        }
        frame.bob.distance(fine.bob)
    }

    #[test]
    fn rigid_substep_tracks_a_fine_reference() {
        // `anchor_initialized` must be set or the first tick snaps the bob
        // to rest and the pendulum never moves — a silently vacuous test.
        let err = substep_error_over_one_second(&SimplePhysicsData {
            length: 100.0,
            gravity: 981.0,
            angle_damping: 0.1,
            bob: Vec2::new(50.0, 86.6),
            anchor: Vec2::ZERO,
            anchor_initialized: true,
            ..Default::default()
        });
        assert!(err < 0.01, "rigid substep drift on a 100-unit arm: {err}");
    }

    #[test]
    fn tick_clamps_extreme_dt_and_terminates() {
        // dt=60s would loop 6000x at 10ms but is clamped to 10s (1000
        // substeps). The pendulum should settle near rest under damping
        // and the call must actually return.
        let anchor = Vec2::ZERO;
        let mut d = SimplePhysicsData {
            length: 100.0,
            gravity: 981.0,
            angle_damping: 0.3,
            bob: Vec2::new(50.0, 86.6),
            anchor,
            ..Default::default()
        };
        d.tick(anchor, 60.0);
        assert!(
            d.bob.distance(Vec2::new(0.0, 100.0)) < 5.0,
            "heavy damping over 10s clamped dt didn't settle: bob={:?}",
            d.bob,
        );
    }

    #[test]
    fn param_value_rotates_displacement_into_local_frame() {
        // Bob at world (60, 80), anchor at world origin, length 100.
        // World-space direction is (0.6, 0.8). With the node's local
        // frame rotated +90° around Z relative to world, the local-frame
        // displacement is the world displacement rotated by -90° back
        // into local: (80, -60) — so local dir is (0.8, -0.6) and the
        // mapped angle differs from the identity case. relLength uses
        // the world distance and stays = 1.0.
        let base = SimplePhysicsData {
            length: 100.0,
            map_mode: PhysicsParamMapMode::AngleLength,
            bob: Vec2::new(60.0, 80.0),
            anchor: Vec2::ZERO,
            anchor_initialized: true,
            ..Default::default()
        };

        let world = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let v_local = base.param_value(world.inverse());
        let v_world = base.param_value(Mat4::IDENTITY);

        // World case: dir (0.6, 0.8), angle = atan2(-0.6, 0.8) / PI ≈ -0.205.
        let expected_world = f32::atan2(-0.6f32, 0.8f32) / std::f32::consts::PI;
        // Local case: dir (0.8, -0.6), angle = atan2(-0.8, -0.6) / PI ≈ -0.705.
        let expected_local = f32::atan2(-0.8f32, -0.6f32) / std::f32::consts::PI;
        assert!((v_world.x - expected_world).abs() < 1e-4);
        assert!((v_local.x - expected_local).abs() < 1e-4);
        // relLength is world-space, so unchanged across frames.
        assert!((v_world.y - 1.0).abs() < 1e-4);
        assert!((v_local.y - 1.0).abs() < 1e-4);
    }

    #[test]
    fn param_value_anglelength_matches_lengthangle_swapped() {
        let a = tilted_pendulum(PhysicsParamMapMode::AngleLength).param_value(Mat4::IDENTITY);
        let l = tilted_pendulum(PhysicsParamMapMode::LengthAngle).param_value(Mat4::IDENTITY);
        assert!((a.x - l.y).abs() < 1e-6, "AL.x should equal LA.y");
        assert!((a.y - l.x).abs() < 1e-6, "AL.y should equal LA.x");
    }

    fn spring_base() -> SimplePhysicsData {
        // Gravity-equilibrium for this spring lies at bob=(0, length)
        // for any frequency: dist=length, rest_length=length - g/k, so
        // spring force = -(0,1)*(length - (length - g/k))*k = -(0, g),
        // exactly cancelling gravity.
        SimplePhysicsData {
            kind: PendulumKind::SpringPendulum,
            length: 100.0,
            gravity: 981.0,
            frequency: 1.0,
            angle_damping: 0.0,
            length_damping: 0.0,
            bob: Vec2::new(0.0, 100.0),
            spring_vel: Vec2::ZERO,
            anchor: Vec2::ZERO,
            // Tests construct the bob explicitly; opt out of the
            // first-tick world-anchor snap so the manually-set state
            // isn't overwritten.
            anchor_initialized: true,
            ..Default::default()
        }
    }

    #[test]
    fn spring_pendulum_stays_near_equilibrium() {
        let mut d = spring_base();
        for _ in 0..120 {
            d.tick(Vec2::ZERO, 1.0 / 60.0);
        }
        // 2s of sim; if the equilibrium analysis or the RK4 step is
        // broken this drifts fast.
        assert!(
            d.bob.distance(Vec2::new(0.0, 100.0)) < 1e-2,
            "spring didn't hold equilibrium: bob={:?}",
            d.bob,
        );
    }

    #[test]
    fn stretched_spring_rebounds() {
        // Pull the bob 30px further down. With no damping the spring
        // must pull it back past equilibrium within one period
        // (~1s at f=1Hz). Asserting we cross back above y=100 proves
        // the spring force has the right sign.
        let mut d = spring_base();
        d.bob = Vec2::new(0.0, 130.0);
        let mut min_y = d.bob.y;
        for _ in 0..60 {
            d.tick(Vec2::ZERO, 1.0 / 60.0);
            min_y = min_y.min(d.bob.y);
        }
        assert!(
            min_y < 100.0,
            "stretched spring never rebounded past equilibrium, min_y = {}",
            min_y,
        );
    }

    #[test]
    fn damped_spring_settles_to_equilibrium() {
        let mut d = spring_base();
        d.angle_damping = 0.5;
        d.length_damping = 0.5;
        d.bob = Vec2::new(0.0, 130.0);
        for _ in 0..600 {
            d.tick(Vec2::ZERO, 1.0 / 60.0);
        }
        assert!(
            d.bob.distance(Vec2::new(0.0, 100.0)) < 1.0,
            "damped spring didn't settle: bob={:?}",
            d.bob,
        );
    }

    #[test]
    fn isotropic_damping_opposes_velocity_off_axis() {
        // Match the two critical-damping coefficients so the damping matrix is
        // `c*I` in the rotated frame. `R^-1 (c*I) R = c*I` for any rotation, so
        // the damping term must come back exactly antiparallel to velocity
        // whatever direction the arm points. A rotate-back that isn't `R^-1`
        // only satisfies that on the vertical, where the cross terms vanish.
        let crit_angle = 2.0 * (981.0f32 / 100.0).sqrt();
        let crit_length = 2.0 * 2.0 * std::f32::consts::PI;
        let p = SpringParams {
            gravity: 981.0,
            length: 100.0,
            frequency: 1.0,
            angle_damping: 1.0,
            length_damping: crit_angle / crit_length,
        };
        let bob = Vec2::new(60.0, 80.0);
        let vel = Vec2::new(-25.0, 40.0);
        let damping =
            spring_accel(bob, vel, Vec2::ZERO, p) - spring_accel(bob, Vec2::ZERO, Vec2::ZERO, p);
        let expected = -vel * crit_angle;
        assert!(
            damping.distance(expected) < 1e-2,
            "isotropic damping should be -c*vel: got {:?}, want {:?}",
            damping,
            expected,
        );
    }

    #[test]
    fn spring_substep_tracks_a_fine_reference() {
        // The spring's substep is sized off the radial mode (2*pi*f), a
        // different branch of `max_eigenvalue` than the rigid arm's, so a
        // bug in only one of them lands in only one of these two tests.
        let mut d = spring_base();
        d.angle_damping = 0.2;
        d.length_damping = 0.2;
        d.bob = Vec2::new(10.0, 120.0);
        let err = substep_error_over_one_second(&d);
        assert!(err < 0.01, "spring substep drift on a 100-unit arm: {err}");
    }

    #[test]
    fn absurd_stiffness_stays_bounded_instead_of_hanging() {
        // Substep count scales with stiffness, so without a ceiling this
        // model asks for ~5e7 RK4 steps in one call and the frame never
        // returns. Deriving the step from the system is only safe if the
        // derivation cannot run away.
        let mut d = spring_base();
        d.frequency = 1.0e6;
        let uncapped = (PHYSICS_MAX_DT / d.max_substep()).ceil();
        assert!(
            uncapped > 1.0e7,
            "test model isn't stiff enough to exercise the cap: {uncapped}",
        );
        assert_eq!(d.substep_count(PHYSICS_MAX_DT), PHYSICS_MAX_SUBSTEPS);

        // A real frame of a realistically stiff model stays well under it.
        d.frequency = 30.0;
        assert!(d.substep_count(1.0 / 60.0) < 8);
    }

    #[test]
    fn a_capped_unstable_frame_settles_instead_of_diverging() {
        // Between "needs more steps than the cap" and "so stiff the first
        // step overflows" sits a band where the capped step is outside RK4's
        // stability bound: the bob saturates at a huge finite value rather
        // than staying put, and drags on the model for many frames after. A
        // 60 Hz spring on a 10s catch-up frame is in it.
        let mut d = spring_base();
        d.frequency = 60.0;
        d.bob = Vec2::new(10.0, 120.0);
        assert_eq!(d.substep_count(PHYSICS_MAX_DT), PHYSICS_MAX_SUBSTEPS);
        assert!(
            d.max_eigenvalue() * (PHYSICS_MAX_DT / PHYSICS_MAX_SUBSTEPS as f32)
                > RK4_STABILITY_LIMIT,
            "test model is not in the unstable band",
        );

        d.tick(Vec2::ZERO, PHYSICS_MAX_DT);
        assert_eq!(
            d.bob,
            Vec2::new(0.0, d.length),
            "a capped unstable frame left the driver somewhere other than rest",
        );

        // The merely-coarse band keeps integrating rather than snapping.
        let mut soft = spring_base();
        soft.frequency = 30.0;
        soft.bob = Vec2::new(10.0, 120.0);
        assert_eq!(soft.substep_count(PHYSICS_MAX_DT), PHYSICS_MAX_SUBSTEPS);
        assert!(
            soft.max_eigenvalue() * (PHYSICS_MAX_DT / PHYSICS_MAX_SUBSTEPS as f32)
                < RK4_STABILITY_LIMIT
        );
    }

    #[test]
    fn stiff_spring_substeps_finer_than_a_soft_one() {
        // The whole point of deriving the substep: a 30 Hz cloth model must
        // take more steps per frame than a 1 Hz hair model, where a fixed
        // constant necessarily over- or under-serves one of them.
        let soft = spring_base();
        let mut stiff = spring_base();
        stiff.frequency = 30.0;
        assert!(
            stiff.max_substep() < soft.max_substep() / 10.0,
            "stiff={} soft={}",
            stiff.max_substep(),
            soft.max_substep(),
        );
        // A soft model fits a 60 fps frame in one step.
        assert!(soft.max_substep() > 1.0 / 60.0);
    }
}
