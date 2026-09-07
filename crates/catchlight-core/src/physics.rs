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
//! `world_inverse` by the same flip coming out. Gravity points toward +Y
//! in that frame.
//!
//! **The particle chain is position-based, not Verlet.**
//! [`ParticleChainData`] stores each particle's velocity explicitly and
//! re-derives it from the move the rod constraint actually made. A Verlet
//! chain encodes velocity as `pos - prev_pos`, which is a velocity only for
//! the `dt` that produced it, so it mis-scales the moment `dt` changes
//! between calls — and on a real display it always does. Storing velocity
//! keeps a varying frame time rate independent. Every knob on a link is
//! per second for the same reason `angle_damping` is: `damping` sheds a
//! fraction of velocity per second and `time_scale` multiplies the link's
//! own clock, so neither the frame rate nor the substep count changes the
//! material a model describes.
//!
//! **The anchor and the node's down cross a frame; they do not jump at its
//! first substep.** Both arrive once a frame and describe the whole of it, so
//! [`ParticleChainData::tick`] walks each from what the last tick stored to
//! what this one was handed. Pinning every substep to the new anchor makes a
//! 30 Hz frame one lurch and seven still steps where 240 Hz slides: swept
//! ±10 px at 1 Hz, the tip ran 8.2 px from the 240 Hz curve, and 0.22 px once
//! the anchor travelled. What is left is the substep's own truncation, since
//! a rate that is not a whole number of `CHAIN_MAX_STEP`s takes a finer step:
//! 144 Hz and 288 Hz, which share one, agree to 0.01 px.
//!
//! **A chain is damped in its anchor's frame, not the world's.** A character
//! walking across the screen carries the whole strand along, and that is not
//! motion a hair's own drag resists — damping the absolute velocity streams
//! the hair backwards for as long as the walk lasts, and the bend spring's
//! damping, being a damping on the bend *rate*, bleeds a carried chain the
//! same way. So every link takes the anchor's velocity over the substep off
//! its own, damps what is left, and puts it back. Under a still anchor there
//! is nothing to take off and the chain integrates the bits it always did.
//!
//! **A link's bend spring pulls toward the art, not toward gravity.** A
//! link's `stiffness` is the frequency in Hz of a spring on the bend at the
//! joint above it, and the bend it pulls to zero is the one `link_bends`
//! reports: for the first link, the node's own down, so a stiff strand
//! follows the head as it tilts while a limp one hangs; for every later link,
//! the direction of the link above it, so a strand drawn straight is at rest
//! straight. There is no rest-bend knob — bend 0 is the art as drawn — and
//! `0` Hz is no spring at all, which is every model written before there was
//! one. The spring saturates rather than exploding: a step too coarse to
//! resolve it moves the joint to rest in that step instead of past it, so no
//! stiffness at any `time_scale` can blow the chain up.
//!
//! **Every bend spring damps itself.** A strand of equal links is a resonant
//! cascade — each link driven by the one above it at its own frequency, one
//! way only — so the spring carries a fixed damping ratio,
//! [`BEND_SPRING_DAMPING_RATIO`], on top of whatever drag the link's `damping`
//! names. Without it a chain of four equal 8 Hz links, kicked once, rings for
//! ever; with it every stiffness and link count measured settles. The constant
//! carries the numbers.
//!
//! **A chain is at rest when it has stopped, not when it stands in the pose
//! `settle_to_rest` computes.** The two are the same thing for a straight
//! hang, which is the only pose the rod projection reproduces exactly; a
//! sprung chain hangs at an angle and stops a hundredth of a pixel from the
//! analytic balance. `ParticleChainData::is_at_rest` therefore asks about
//! velocity alone, and `is_projection_noise` is what makes a stopped chain's
//! velocities exactly zero rather than a float-floor jitter.

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

    /// Whether the driver is standing exactly where [`Self::settle_to_rest`]
    /// would put it: the bob at `anchor + (0, length)` and both velocities
    /// zero. The inverse of "this driver is still moving", which is what a
    /// caller watches to know a frame changed nothing.
    ///
    /// `eps_sq` bounds the squared displacement from rest *and* each squared
    /// velocity, so one number covers three quantities in three different
    /// units. That is deliberate: the caller is `Puppet` and the number is its
    /// `SETTLE_EPS_SQ`, the same epsilon `settle_physics` calls an anchor
    /// converged at, so "settled" means one thing across the crate rather than
    /// two that nearly agree.
    ///
    /// A driver that has never seen a world anchor is *not* at rest: its first
    /// tick snaps the bob under the anchor, which is a move.
    pub fn is_at_rest(&self, eps_sq: f32) -> bool {
        self.anchor_initialized
            && (self.bob - (self.anchor + Vec2::new(0.0, self.length))).length_squared() <= eps_sq
            && self.spring_vel.length_squared() <= eps_sq
            && self.d_angle * self.d_angle <= eps_sq
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

/// One segment of a [`ParticleChainData`]: a rigid rod from the particle
/// above it down to its own particle, plus the knobs that particle answers to.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainLink {
    /// Fixed length in model pixels.
    pub length: f32,
    /// Multiplier on the chain's gravity for this link's particle.
    pub gravity_scale: f32,
    /// Fraction of velocity shed per **second** (0..=1), like `angle_damping`.
    pub damping: f32,
    /// Multiplier on this link's clock; 1 is real time. > 1 reacts faster,
    /// < 1 floatier. Zero or negative reads as a stopped clock: the particle
    /// still follows the rod above it but integrates nothing of its own.
    pub time_scale: f32,
    /// Natural frequency in **Hz** of the spring pulling this link's bend
    /// back to zero — how stiff the strand is at this joint. Zero is no
    /// spring at all, and the link hangs on gravity alone.
    pub stiffness: f32,
}

impl Default for ChainLink {
    fn default() -> Self {
        Self {
            length: 100.0,
            gravity_scale: 1.0,
            damping: 0.5,
            time_scale: 1.0,
            stiffness: 0.0,
        }
    }
}

/// A point mass on a chain. Both halves of the state are stored; see the
/// module doc on why the velocity is not left implicit in a previous position.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChainParticle {
    pub pos: Vec2,
    pub vel: Vec2,
}

/// A chain of rigid links hanging from a moving anchor, solved with
/// position-based dynamics: free-integrate every particle, then walk the
/// rods root to tip putting each particle back on its circle, then read the
/// velocity back off the move that survived. It is [`VerletPendulum`]'s
/// primitive generalized to N links, minus the implicit velocity.
///
/// [`Self::link_bends`] reads the chain out as one number per link, and the
/// sign convention is the whole point of it: **positive bend = the link's tip
/// displaced toward +X of the node, straight down = 0, in units of half
/// turns.**
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleChainData {
    pub local_only: bool,
    /// Pre-folded like `SimplePhysicsData::gravity` (pixels/s², toward +Y in
    /// the physics frame).
    pub gravity: f32,
    pub links: Vec<ChainLink>,
    /// `links.len() + 1` particles; `particles[0]` is the anchor. Any tick
    /// that finds the two out of step re-hangs the chain, so editing `links`
    /// is enough to reshape it.
    pub particles: Vec<ChainParticle>,
    pub anchor: Vec2,
    /// Where bend 0 of the **first** link points, as a unit vector in the
    /// physics (Y-down) frame: the node's own down, which the caller hands
    /// over every tick because only the puppet knows the node's world
    /// rotation. `(0, 1)` — gravity's direction — until one is supplied, so
    /// a chain built and stepped by hand behaves as it always did.
    pub down: Vec2,
    /// `false` until `tick()` first sees the world-space anchor and hangs the
    /// chain under it, for the same reason `SimplePhysicsData` defers its
    /// snap: construction has only the node-local transform.
    pub anchor_initialized: bool,
    /// Whether the last thing done to this chain moved a particle: every
    /// substep of the last [`Self::tick`], or the displacement
    /// `Puppet::kick_chain` applied. Cleared by [`Self::settle_to_rest`].
    ///
    /// [`Self::is_at_rest`] needs the whole tick and not just the state it
    /// ended in, because a swinging chain passes through zero velocity at the
    /// top of every swing. Land a frame's last substep there and every
    /// velocity reads zero for that instant, though the next substep will
    /// pull it back the other way — eight such frames in one measured decay,
    /// each one a frame on which a viewport would have gone to sleep on a
    /// chain still swinging. A turning point lasts microseconds; a frame is
    /// sixteen milliseconds, and no turning point fills one.
    pub moved_last_tick: bool,
}

impl ParticleChainData {
    pub fn new(links: Vec<ChainLink>) -> Self {
        let mut chain = Self {
            local_only: false,
            gravity: 9.8 * 100.0,
            particles: Vec::with_capacity(links.len() + 1),
            links,
            anchor: Vec2::ZERO,
            down: Vec2::new(0.0, 1.0),
            anchor_initialized: false,
            moved_last_tick: true,
        };
        chain
            .particles
            .resize(chain.links.len() + 1, ChainParticle::default());
        chain
    }
}

impl Default for ParticleChainData {
    fn default() -> Self {
        Self::new(vec![ChainLink::default(); 3])
    }
}

/// Longest step the chain solver will take. Unlike the RK4 drivers there is
/// no stiffness to derive it from — a rod constraint has no frequency — so
/// the chain takes a fixed, fine step and lets `tick` chop a frame into as
/// many as it needs.
pub const CHAIN_MAX_STEP: f32 = 1.0 / 240.0;

/// The damping ratio every bend spring carries, whatever the link's own
/// `damping` says.
///
/// **A strand of equal links is a resonant cascade.** Each link's spring is an
/// oscillator at that link's frequency, driven by the link above it at the
/// same frequency, and the drive runs one way only — so with nothing but the
/// velocity damping to resist it, each stage amplifies the one before it by
/// its quality factor. That factor is `w0 / gamma`: about 70 for an 8 Hz
/// spring under a `damping` of 0.5, because `damping` is a drag on the
/// particle and not on the bend. Four equal 8 Hz links kicked once ring for
/// ever; the same four staggered 8, 6, 4, 2 Hz settle in seventeen seconds,
/// which is the cascade showing itself.
///
/// No real fibre has a Q of 70, and stiffness-proportional damping is the
/// standard material model, so the spring carries its own. At 0.1 the Q is 5
/// and a joint sheds about half its swing each cycle: enough that a displaced
/// joint still visibly wobbles a couple of times, and enough that every
/// configuration measured — 2 to 32 Hz across 2 to 6 links, at `damping` 0.1
/// and 0.5 — settles, the slowest in seventeen seconds. A link's own
/// `damping` adds to this and can never bring it below.
pub const BEND_SPRING_DAMPING_RATIO: f32 = 0.1;

impl ParticleChainData {
    /// Advance the chain by `dt` with its root pinned to `anchor_world`.
    /// The outer `dt` is clamped and split exactly the way
    /// [`SimplePhysicsData::tick`] splits its own: uniform steps from an
    /// integer count, so there is no drifting remainder and no ragged final
    /// step whose damping would land differently from its neighbours'.
    ///
    /// `down_world` is the unit vector, in the physics (Y-down) frame, that
    /// bend 0 of the first link points along — the node's own down, which is
    /// gravity's direction only while the node is upright. Every later link
    /// measures its bend against the link above it instead, so a chain drawn
    /// straight is bend 0 all the way down whatever the node is doing.
    ///
    /// **The anchor and the down travel across the substeps.** Both arrive
    /// once a frame but describe a whole frame's worth of motion, so substep
    /// `k` of `n` pins the root to `lerp(previous, now, k / n)` and points
    /// bend 0 the same way; the module doc carries what pinning every substep
    /// to `now` costs. A caller that chops a frame itself has to chop the
    /// anchor path with it.
    ///
    /// A tick that advances no time is a reposition rather than a move: the
    /// anchor and down are stored and nothing is interpolated toward them.
    pub fn tick(&mut self, anchor_world: Vec2, down_world: Vec2, dt: f32) {
        let down = unit_down(down_world);
        if !self.anchor_initialized || self.particles.len() != self.links.len() + 1 {
            self.settle_to_rest(anchor_world, down);
        }
        let (was_anchor, was_down) = (self.anchor, self.down);
        self.anchor = anchor_world;
        self.down = down;
        // NaN survives `clamp` and fails `<= 0.0`, and `NaN as u32` saturates
        // to 0, so an unguarded NaN dt would run zero steps and silently
        // freeze the chain instead of stepping it.
        if !dt.is_finite() {
            return;
        }
        let clamped = dt.clamp(0.0, PHYSICS_MAX_DT);
        if clamped <= 0.0 {
            return;
        }
        let steps = (clamped / CHAIN_MAX_STEP)
            .ceil()
            .clamp(1.0, PHYSICS_MAX_SUBSTEPS as f32) as u32;
        let h = clamped / steps as f32;
        let mut moved = false;
        let mut from = was_anchor;
        for k in 1..=steps {
            // The last substep lands on the caller's own numbers rather than
            // on a `lerp` of them, so a chain whose anchor never moves is
            // stepped at exactly the anchor it was handed.
            let (to, rest) = if k == steps {
                (anchor_world, down)
            } else {
                let t = k as f32 / steps as f32;
                (
                    was_anchor.lerp(anchor_world, t),
                    unit_down_or(was_down.lerp(down, t), down),
                )
            };
            moved |= self.step(to, to - from, rest, h);
            from = to;
        }
        self.moved_last_tick = moved;
    }

    /// Hang the chain from `anchor_world` with no motion, and resize
    /// `particles` to match `links`. `down_world` is [`Self::tick`]'s, and is
    /// stored the same way.
    ///
    /// This is the solver's analytic equilibrium, link by link. A springless
    /// link hangs along gravity, which is the whole of what this used to do:
    /// gravity is parallel to every rod there, so the solver leaves it
    /// exactly alone. A sprung one balances the two torques it feels — see
    /// [`rest_pose_dir`] for the equation and why it is exact rather than
    /// approximate.
    pub fn settle_to_rest(&mut self, anchor_world: Vec2, down_world: Vec2) {
        self.anchor = anchor_world;
        self.down = unit_down(down_world);
        self.particles.clear();
        self.particles.reserve(self.links.len() + 1);
        let mut pos = anchor_world;
        self.particles.push(ChainParticle {
            pos,
            vel: Vec2::ZERO,
        });
        let mut rest = self.down;
        for link in &self.links {
            let dir = rest_pose_dir(link, rest, self.gravity);
            pos += dir * link.length;
            self.particles.push(ChainParticle {
                pos,
                vel: Vec2::ZERO,
            });
            rest = dir;
        }
        self.anchor_initialized = true;
        self.moved_last_tick = false;
    }

    /// Whether the chain is standing still: no particle carries a velocity a
    /// next step would move it by.
    ///
    /// **Standing still, not standing in the analytic rest pose.** The pose
    /// [`Self::settle_to_rest`] computes is the exact balance, and the
    /// solver's own arithmetic reaches it only where the rod projection is
    /// exact, which is the straight hang. A sprung chain hangs at an angle,
    /// where `above + normalize(offset) * length` rounds, and a chain that
    /// relaxes into that pose from a kick stops a hundredth of a pixel or so
    /// short of it — far below anything visible, and far above the epsilon
    /// this is asked about. Comparing against the analytic pose would call
    /// such a chain moving forever and keep a viewport redrawing a picture
    /// that never changes. What a caller wants to know is whether the next
    /// frame would differ from this one, and that is a question about
    /// velocity alone.
    ///
    /// The check is exact in practice rather than approximate: a chain the
    /// solver has stopped moving carries velocities of exactly zero, because
    /// the same deadband that stopped it zeroes them (see
    /// [`is_projection_noise`]). `eps_sq` bounds each squared velocity, and
    /// is the crate's one notion of settled, the same number
    /// [`SimplePhysicsData::is_at_rest`] is asked about.
    ///
    /// It is the whole of the last tick that has to have moved nothing, not
    /// the instant it ended in; [`Self::moved_last_tick`] says why.
    ///
    /// A chain that has never seen a world anchor is *not* at rest: its first
    /// tick hangs it under the anchor, which is a move.
    pub fn is_at_rest(&self, eps_sq: f32) -> bool {
        if !self.anchor_initialized || self.particles.len() != self.links.len() + 1 {
            return false;
        }
        !self.moved_last_tick
            && self
                .particles
                .iter()
                .all(|p| p.vel.length_squared() <= eps_sq)
    }

    /// The bend at the joint above each link, in half turns, written into
    /// `out` (cleared first). One value per link, so a chain reads out as a
    /// vector the same shape as the thing that authored it.
    ///
    /// `world_inverse` rotates each rod out of world space and into the
    /// node's frame exactly as [`SimplePhysicsData::param_value`] does: the
    /// translation column is ignored, and the caller hands over a matrix
    /// already conjugated by the Y flip.
    ///
    /// The first link's bend is measured from straight down; every later one
    /// is measured from the link above it, wrapped into a half turn either
    /// way. **Positive = the link's tip displaced toward +X of the node.**
    pub fn link_bends(&self, world_inverse: Mat4, out: &mut Vec<f32>) {
        out.clear();
        let rods = self.links.len().min(self.particles.len().saturating_sub(1));
        let mut previous = 0.0f32;
        for i in 0..rods {
            let d_world = self.particles[i + 1].pos - self.particles[i].pos;
            let d_local = world_inverse
                .transform_vector3(d_world.extend(0.0))
                .truncate();
            let dir = if d_local.length_squared() > 1e-12 {
                d_local.normalize()
            } else {
                Vec2::new(0.0, 1.0)
            };
            // Y-down: (0, 1) is straight down and reads 0, (1, 0) points at
            // +X and reads +pi/2. Note the sign is the opposite of
            // `param_value`'s angle, which reports a pendulum's swing in a
            // Y-up parameter frame; this one stays in the node's own frame.
            let theta = f32::atan2(dir.x, dir.y);
            let bend = if i == 0 {
                theta
            } else {
                wrap_to_half_turn(theta - previous)
            };
            previous = theta;
            out.push(bend / std::f32::consts::PI);
        }
    }

    /// One position-based step of size `h`, reporting whether it moved any
    /// particle.
    ///
    /// The root is pinned to `anchor`, which travelled `anchor_moved` over
    /// this step, and `down` is where bend 0 of the first link points for it;
    /// [`Self::tick`] walks all three across a frame's substeps.
    ///
    /// Prediction, constraint and velocity fuse into a single root-to-tip
    /// pass: a particle's free integration does not depend on its neighbour,
    /// and by the time the rod above it is solved that neighbour is already
    /// final, so the pass yields exactly what integrate-all-then-constrain-all
    /// would. The result lands in scratch and is committed only if all of it
    /// is finite, so one bad step cannot poison the chain permanently.
    fn step(&mut self, anchor: Vec2, anchor_moved: Vec2, down: Vec2, h: f32) -> bool {
        let n = self.links.len();
        // `tick` re-hangs a chain whose two vectors disagree, so this never
        // fires; it is here so the indexing below cannot panic on its own.
        if self.particles.len() != n + 1 {
            return false;
        }
        let mut next: smallvec::SmallVec<[ChainParticle; 16]> =
            smallvec::SmallVec::with_capacity(n + 1);
        next.push(ChainParticle {
            pos: anchor,
            vel: if h > 0.0 && h.is_finite() {
                anchor_moved / h
            } else {
                Vec2::ZERO
            },
        });

        // Where bend 0 points for the link being solved. The first link
        // answers to the node's own down; every later one answers to the link
        // above it, which this pass has already put in its final place.
        let mut rest = down;
        for i in 1..=n {
            let link = self.links[i - 1];
            let above = next[i - 1].pos;
            let old = self.particles[i].pos;
            let h_i = h * link.time_scale;
            let moving = h_i.is_finite() && h_i > 0.0;

            let free = if moving {
                let mut v =
                    self.particles[i].vel + Vec2::new(0.0, self.gravity * link.gravity_scale) * h_i;
                // The bend spring rides alongside gravity, on this link's own
                // clock like everything else. Skipped whole when it has
                // nothing to say, so an unsprung link integrates the same
                // bits it did before there was a spring at all.
                if let Some(acc) =
                    bend_spring_acceleration(rest, old - above, link.length, link.stiffness, h_i)
                {
                    v += acc * h_i;
                }
                old + v * h_i
            } else {
                old
            };

            let offset = free - above;
            let projected = if offset.length_squared() > 1e-12 {
                above + offset.normalize() * link.length
            } else {
                above + Vec2::new(0.0, link.length)
            };

            // Read velocity off the move that survived the constraint, not
            // the one prediction asked for: the rod's correction is a real
            // impulse, and reusing the predicted velocity would leave the
            // particle carrying motion the rod just cancelled. Damping is a
            // fraction shed per second, so it is raised to this link's own
            // elapsed time.
            //
            // Both dampings act on the velocity the particle has *in the
            // anchor's frame*: the anchor's own velocity over this substep,
            // on this link's clock like everything else, comes off before
            // they run and goes back on after. See the module doc — a strand
            // a walk carries across the screen is not moving as far as its
            // own drag is concerned. A still anchor subtracts zero, which is
            // what makes this bit for bit what the link always integrated.
            let vel = if moving {
                let carried = anchor_moved / h_i;
                let own = (projected - old) / h_i - carried;
                let own = own * (1.0 - link.damping.clamp(0.0, 1.0)).powf(h_i);
                carried + damp_bend(own, projected - above, link.stiffness, h_i)
            } else {
                Vec2::ZERO
            };

            // A move too small to be one is no move. See `is_projection_noise`
            // for what this is for and why it cannot touch a trajectory.
            let (pos, vel) = if is_projection_noise(projected - old, old, link.length) {
                (old, Vec2::ZERO)
            } else {
                (projected, vel)
            };

            // The rod as the constraint left it, which is what the link below
            // measures its own bend against.
            let solved = pos - above;
            if solved.length_squared() > 1e-12 {
                rest = solved.normalize();
            }

            next.push(ChainParticle { pos, vel });
        }

        if !next.iter().all(|p| p.pos.is_finite() && p.vel.is_finite()) {
            return false;
        }
        let moved = next
            .iter()
            .zip(self.particles.iter())
            .any(|(now, was)| now.pos != was.pos);
        self.particles.copy_from_slice(&next);
        moved
    }
}

/// Whether a step's move is the rod projection's own rounding rather than
/// motion — in which case [`ParticleChainData::step`] puts the particle back
/// where it was and calls its velocity zero.
///
/// **Why it is needed.** `above + normalize(offset) * length` is exact only
/// where the rod lies on an axis: at a straight hang the x is zero and the y
/// is a sum, so a settled unsprung chain lands on itself bit for bit and
/// stops. At any other angle — which is where a bend spring holds a strand
/// under a turned node — the projection lands an ulp or so from `old` even
/// when the forces balance exactly and `free` came back as `old`. The
/// velocity is then read off that ulp, `ulp / h` is about 2e-3 px/s on a
/// hundred-pixel coordinate at a 240 Hz substep, and that feeds the next
/// step: a limit cycle at the float floor that never decays and never lets a
/// viewport's frame loop go to sleep.
///
/// **Why it cannot alter a trajectory.** The floor is a few ulps of the
/// coordinate — under 1e-4 px for a chain within a few hundred pixels of the
/// model origin — while a chain actually in motion moves a particle by
/// something like 0.17 px in a substep, three orders of magnitude more. The
/// committed trajectory baseline is byte for byte unchanged for every
/// scenario with no spring in it. The scale is `max(|old|, length)` so that a
/// rod hanging through the origin, where `|old|` goes to zero, still has the
/// projection's own `EPSILON * length` rounding covered.
///
/// **What it gives up: a relaxation stops a hair short of where it was
/// heading.** A chain easing into rest has a velocity proportional to how far
/// out it still is, so its per-step move crosses this floor while it is still
/// `floor / (rate * h)` from the end — a few hundredths of a pixel on a 60 px
/// link, well under the width of a rendered pixel, and it stops there for
/// good instead of creeping for another minute. This is why
/// [`ParticleChainData::is_at_rest`] asks whether the chain has stopped and
/// not whether it stands in [`ParticleChainData::settle_to_rest`]'s pose. A
/// per-component floor scaled to each coordinate would land closer, but it
/// shrinks with the coordinate it scales, so a chain easing to zero along one
/// axis never stops at all and the frame loop never sleeps: the thing this
/// exists to prevent.
///
/// The same trade freezes motion the floor cannot resolve at all — a link
/// whose `time_scale` is under about a tenth, alone with gravity and no
/// anchor moving. Below the floor `f32` cannot represent the move either way.
fn is_projection_noise(moved: Vec2, old: Vec2, length: f32) -> bool {
    let floor = 4.0 * f32::EPSILON * old.length().max(length);
    moved.length_squared() <= floor * floor
}

/// A caller's down as a unit vector, falling back to gravity's own direction
/// when what arrives has no direction in it. `(0, 1)` is what a chain that
/// nobody tells about a node hangs along, and what every chain did before a
/// link had a bend spring to point anywhere else.
fn unit_down(v: Vec2) -> Vec2 {
    unit_down_or(v, Vec2::new(0.0, 1.0))
}

/// The same, for a down mid-way between two the caller gave: a node that
/// turned exactly half a turn in one frame passes through a zero vector at
/// the middle substep, and there the half-way down is the frame's own.
fn unit_down_or(v: Vec2, fallback: Vec2) -> Vec2 {
    if v.is_finite() && v.length_squared() > 1e-12 {
        v.normalize()
    } else {
        fallback
    }
}

/// The angle from `from` to `to`, in `(-pi, pi]`, positive the way
/// [`rotate_by`] turns.
fn signed_angle(from: Vec2, to: Vec2) -> f32 {
    f32::atan2(from.x * to.y - from.y * to.x, from.dot(to))
}

/// `v` turned by `angle`, the sign [`signed_angle`] reports.
fn rotate_by(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// The direction one link takes at rest: `rest` is where its bend reads zero
/// (the node's down for the first link, the link above for the rest), and
/// `gravity` is the chain's, before this link's own scale.
///
/// **Springless links keep what they always did.** A weighted one hangs along
/// gravity, whatever the node is doing; one with no weight either has nothing
/// to decide it, so it keeps the bend it was drawn with — zero — which is
/// `rest`.
///
/// A sprung link solves the torque balance about its joint,
///
/// ```text
///   w0^2 * L * theta  =  g * sin(theta_g - theta)
/// ```
///
/// for `theta`, the bend measured from `rest`: the spring's pull back to zero
/// against gravity's pull toward `theta_g`, gravity's own direction measured
/// the same way. **This is the solver's exact equilibrium and not an
/// approximation of it**, because the coupling runs one way only — a link is
/// hung off the rod above it and never feels the weight below it — so each
/// joint balances on its own and a root-to-tip walk is the whole answer.
///
/// Solved by bisection on `[min(0, theta_g), max(0, theta_g)]`: at `0` only
/// gravity pulls, at `theta_g` only the spring does, so the ends have
/// opposite signs and the root is between them. A negative `gravity_scale` is
/// gravity pointing the other way, so it flips `theta_g` rather than breaking
/// the bracket.
fn rest_pose_dir(link: &ChainLink, rest: Vec2, gravity: f32) -> Vec2 {
    let g = gravity * link.gravity_scale;
    let down = Vec2::new(0.0, 1.0);
    // NaN is spelled out rather than left to fail a comparison: a link whose
    // stiffness is not a number carries no spring.
    if link.stiffness.is_nan() || link.stiffness <= 0.0 {
        return if g != 0.0 && g.is_finite() {
            down
        } else {
            rest
        };
    }
    if g == 0.0 || !g.is_finite() {
        return rest;
    }
    let w0 = std::f32::consts::TAU * link.stiffness;
    let k = w0 * w0 * link.length;
    let theta_g = signed_angle(rest, if g > 0.0 { down } else { -down });
    let g = g.abs();
    let balance = |theta: f32| k * theta - g * (theta_g - theta).sin();

    let (mut lo, mut hi) = (theta_g.min(0.0), theta_g.max(0.0));
    let (f_lo, f_hi) = (balance(lo), balance(hi));
    if !f_lo.is_finite() || !f_hi.is_finite() {
        return rest;
    }
    if f_lo == 0.0 {
        return rotate_by(rest, lo);
    }
    if f_hi == 0.0 {
        return rotate_by(rest, hi);
    }
    if (f_lo > 0.0) == (f_hi > 0.0) {
        // No bracket to bisect on; nothing sensible to say, so leave the link
        // where it was drawn.
        return rest;
    }
    let hi_positive = f_hi > 0.0;
    // Enough halvings to exhaust an f32 mantissa several times over; the
    // early break is what actually ends it.
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if mid == lo || mid == hi {
            break;
        }
        if (balance(mid) > 0.0) == hi_positive {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    rotate_by(rest, 0.5 * (lo + hi))
}

/// The bend spring's tangential acceleration on one particle, or `None` when
/// the spring has nothing to add: an unsprung link, a rod with no direction
/// to it, or a bend already exactly at rest. `None` rather than a zero
/// vector, so a chain without stiffness integrates bit for bit what it did
/// before the spring existed.
///
/// `rest` is where the bend reads zero and `rod` is the joint's current one,
/// from the particle above to this one. With `w0 = 2*pi*stiffness` the spring
/// pulls the bend `theta` back toward zero with a tangential `w0^2 * length *
/// theta` — the pendulum form, so `stiffness` reads straight off the strand
/// as the frequency in Hz a displaced joint rings at.
///
/// **A step too coarse for the spring saturates at critical rather than
/// exploding.** The spring's own displacement over a step is `w0^2 * theta *
/// h^2` in angle, so once `w0^2 * h^2` passes 1 it would carry the joint
/// through rest and out the far side, further every step. `w0^2` is clamped
/// to `1 / h^2` there instead: the joint lands on rest in that step. That is
/// what keeps any stiffness finite at any `time_scale`, at the price of
/// stiffnesses the step is too coarse to tell apart.
fn bend_spring_acceleration(
    rest: Vec2,
    rod: Vec2,
    length: f32,
    stiffness: f32,
    h: f32,
) -> Option<Vec2> {
    if stiffness.is_nan() || stiffness <= 0.0 || rod.length_squared() <= 1e-12 {
        return None;
    }
    let dir = rod.normalize();
    let theta = signed_angle(rest, dir);
    if theta == 0.0 {
        return None;
    }
    let w0 = std::f32::consts::TAU * stiffness;
    let k = (w0 * w0).min(1.0 / (h * h));
    // Perpendicular to the rod, on the side that shrinks `theta`.
    let acc = Vec2::new(-dir.y, dir.x) * -(k * length * theta);
    acc.is_finite().then_some(acc)
}

/// `vel` with [`BEND_SPRING_DAMPING_RATIO`] applied for one step of `h`: the
/// part of it across the rod is kept in the proportion
/// `exp(-2 * zeta * w0 * h)`, and the part along the rod is left alone,
/// because that part is the rod's own to cancel.
///
/// Returned unchanged, bit for bit, for a link with no spring or a rod with no
/// direction, so an unsprung chain reads back the velocity it always did.
///
/// A retain rather than a force term, and so it needs no saturation of its
/// own: the factor is in `[0, 1)` for every stiffness and every step, and goes
/// to zero — the whole cross-rod velocity shed in that step — where an
/// explicit `-2 * zeta * w0 * v` would have overshot and flipped it. The two
/// forms measure the same period at 1 Hz to within a thousandth.
fn damp_bend(vel: Vec2, rod: Vec2, stiffness: f32, h: f32) -> Vec2 {
    if stiffness.is_nan() || stiffness <= 0.0 || rod.length_squared() <= 1e-12 {
        return vel;
    }
    let dir = rod.normalize();
    let along = dir * vel.dot(dir);
    let w0 = std::f32::consts::TAU * stiffness;
    let damped = along + (vel - along) * (-2.0 * BEND_SPRING_DAMPING_RATIO * w0 * h).exp();
    if damped.is_finite() {
        damped
    } else {
        vel
    }
}

/// Fold an angle into `(-pi, pi]` so a joint that crosses the back of the
/// chain reports the short way round rather than a full turn.
fn wrap_to_half_turn(angle: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let folded = angle.rem_euclid(TAU);
    if folded > PI {
        folded - TAU
    } else {
        folded
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

    /// Straight down in the physics frame: what an upright node's chain gets
    /// handed, and what every chain assumed before a node could tilt one.
    const DOWN: Vec2 = Vec2::new(0.0, 1.0);

    /// Three links of unequal length, so a bug that only holds for a uniform
    /// chain shows up.
    fn chain(damping: f32) -> ParticleChainData {
        ParticleChainData::new(
            [60.0f32, 50.0, 40.0]
                .map(|length| ChainLink {
                    length,
                    damping,
                    ..Default::default()
                })
                .to_vec(),
        )
    }

    #[test]
    fn a_hanging_chain_stays_hanging() {
        let mut c = chain(0.5);
        for _ in 0..300 {
            c.tick(Vec2::ZERO, DOWN, 1.0 / 60.0);
        }
        let mut rest = Vec2::ZERO;
        for i in 0..c.particles.len() {
            if i > 0 {
                rest += Vec2::new(0.0, c.links[i - 1].length);
            }
            assert!(
                c.particles[i].pos.distance(rest) < 1e-3,
                "particle {i} drifted off rest: {:?} want {:?}",
                c.particles[i].pos,
                rest,
            );
        }
        assert!(c.is_at_rest(1e-6), "a hanging chain reads as at rest");
    }

    #[test]
    fn a_sideways_kick_reaches_the_tip_after_the_root() {
        let mut c = chain(0.5);
        c.tick(Vec2::ZERO, DOWN, 1.0 / 60.0);
        let tip = c.particles.len() - 1;
        let kicked = Vec2::new(40.0, 0.0);

        let (mut root_peak, mut root_at) = (0.0f32, 0usize);
        let (mut tip_peak, mut tip_at) = (0.0f32, 0usize);
        for f in 0..600 {
            c.tick(kicked, DOWN, 1.0 / 60.0);
            let root = c.particles[1].vel.x.abs();
            if root > root_peak {
                root_peak = root;
                root_at = f;
            }
            let end = c.particles[tip].vel.x.abs();
            if end > tip_peak {
                tip_peak = end;
                tip_at = f;
            }
        }

        assert!(
            tip_at > root_at,
            "the kick reached the tip at frame {tip_at}, no later than the first link at {root_at}",
        );
        for i in 0..c.particles.len() {
            assert!(
                c.particles[i].pos.x > 0.0,
                "particle {i} never followed the anchor: {:?}",
                c.particles[i].pos,
            );
        }
    }

    #[test]
    fn every_link_holds_its_length_while_swinging() {
        let mut c = chain(0.1);
        for f in 0..300 {
            let t = f as f32 / 60.0;
            c.tick(Vec2::new(60.0 * (t * 3.0).sin(), 0.0), DOWN, 1.0 / 60.0);
            for i in 0..c.links.len() {
                let rod = c.particles[i + 1].pos.distance(c.particles[i].pos);
                assert!(
                    (rod - c.links[i].length).abs() < 1e-3,
                    "frame {f} link {i} stretched to {rod}, want {}",
                    c.links[i].length,
                );
            }
        }
    }

    #[test]
    fn one_tick_equals_the_substeps_it_would_have_taken() {
        let dt = 1.0 / 60.0;
        let steps = (dt / CHAIN_MAX_STEP).ceil() as u32;
        assert_eq!(steps, 4, "the split this test hand-runs");

        let mut whole = chain(0.4);
        whole.settle_to_rest(Vec2::ZERO, DOWN);
        let mut split = whole.clone();

        // The anchor path is chopped along with the frame: `tick` slides the
        // root from where it was to where it has been put, so a caller
        // handing over quarter-frames has to hand over the quarter-way
        // anchors that go with them. Handing all four the destination would
        // be telling the solver the anchor jumped in the first quarter, which
        // is a different move and rightly integrates differently.
        let anchor = Vec2::new(40.0, 0.0);
        whole.tick(anchor, DOWN, dt);
        for k in 1..=steps {
            let part = Vec2::ZERO.lerp(anchor, k as f32 / steps as f32);
            split.tick(part, DOWN, dt / steps as f32);
        }

        // The substep is the same size either way, so this is exact; the
        // tolerance only guards against a compiler reassociating the split.
        for i in 0..whole.particles.len() {
            assert!(
                whole.particles[i].pos.distance(split.particles[i].pos) < 1e-6
                    && whole.particles[i].vel.distance(split.particles[i].vel) < 1e-6,
                "particle {i}: {:?} vs {:?}",
                whole.particles[i],
                split.particles[i],
            );
        }
    }

    /// One second of the same anchor path, sampled at two rates, lands the
    /// tip in one place.
    ///
    /// **The path is a ramp and not a step, because a step is not one path.**
    /// An anchor 40 px away on the very next tick says it travelled 40 px in
    /// that frame, which is 2400 px/s at 60 Hz and 9600 px/s at 240 — three
    /// different pieces of physics wearing one number, and the chain is right
    /// to whip differently for each. Sliding the anchor over a quarter second
    /// instead asks both rates the same question, and they answer it to
    /// within 0.29 px where the step is 6.6 px apart.
    #[test]
    fn the_chain_lands_in_one_place_at_two_frame_rates() {
        // Where the anchor is `t` seconds in: 40 px to the side over the
        // first quarter second, held there after.
        let path = |t: f32| Vec2::new(40.0 * (t / 0.25).min(1.0), 0.0);
        let ran = |rate: u32| {
            let mut c = chain(0.4);
            c.settle_to_rest(path(0.0), DOWN);
            for f in 0..rate {
                c.tick(path((f + 1) as f32 / rate as f32), DOWN, 1.0 / rate as f32);
            }
            c.particles[c.particles.len() - 1].pos
        };

        let sixty = ran(60);
        let one_forty_four = ran(144);
        let total: f32 = chain(0.4).links.iter().map(|l| l.length).sum();
        let gap = sixty.distance(one_forty_four);
        // Under twice the 0.289 px measured; the difference is the substep
        // size, 1/240 against 1/288, and not the rate that chose it.
        assert!(
            gap < 0.5,
            "one second at two frame rates ended {gap} px apart on a {total} px chain",
        );
        // And it is a swing being compared, not a chain standing still.
        assert!(
            sixty.x > 20.0,
            "the tip followed the anchor across, ended at {sixty:?}",
        );
    }

    /// Three 50 px links on 2 Hz springs under an anchor sliding ±10 px at
    /// 1 Hz, for six seconds. Tip position every half second, which 30, 60,
    /// 120, 144, 240 and 288 Hz all land on exactly.
    ///
    /// Near resonance on purpose: the links' own frequency is
    /// `sqrt(g/L) / 2pi` ≈ 0.7 Hz against a 1 Hz sweep, so the chain answers
    /// with everything it has and any difference in how the anchor is fed to
    /// it shows up amplified rather than buried.
    fn swept_tip(rate: u32) -> Vec<Vec2> {
        // `ParticleChainData::new`'s own gravity, the way a chain nobody has
        // authored a number onto gets it.
        let mut c = ParticleChainData::new(vec![
            ChainLink {
                length: 50.0,
                gravity_scale: 1.0,
                damping: 0.5,
                time_scale: 1.0,
                stiffness: 2.0,
            };
            3
        ]);
        let sweep = |t: f32| Vec2::new(10.0 * (std::f32::consts::TAU * t).sin(), 0.0);
        c.settle_to_rest(sweep(0.0), DOWN);
        let tip = c.particles.len() - 1;
        let mut samples = Vec::with_capacity(12);
        for f in 0..6 * rate {
            c.tick(sweep((f + 1) as f32 / rate as f32), DOWN, 1.0 / rate as f32);
            if (f + 1) % (rate / 2) == 0 {
                samples.push(c.particles[tip].pos);
            }
        }
        samples
    }

    /// The widest the tip of [`swept_tip`] ever gets from itself at two rates.
    fn swept_gap(a: u32, b: u32) -> f32 {
        let (left, right) = (swept_tip(a), swept_tip(b));
        assert_eq!(left.len(), right.len(), "both rates sample 12 times");
        left.iter()
            .zip(&right)
            .map(|(p, q)| p.distance(*q))
            .fold(0.0f32, f32::max)
    }

    /// **A display's refresh rate is not a material.** The same authored
    /// strand under the same swept anchor has to draw the same curve at 30 Hz
    /// as at 240.
    ///
    /// It did not: pinning every substep to the frame's final anchor made a
    /// 30 Hz frame one anchor lurch followed by seven still substeps, and the
    /// tip ran 8.2 px from the 240 Hz curve (4.0 px at 60 Hz, 1.7 px at
    /// 120 Hz). Sliding the anchor across the substeps leaves the numbers
    /// below, which are the chord of the sine the frame cuts across — a
    /// thirtieth of a second of a 1 Hz sweep — and nothing else. The measured
    /// gap at 144 Hz against 240 is 2.36 px, and 2.36 px is also what 288 Hz
    /// measures there, which is the whole of it.
    ///
    /// **Rates are compared to one that takes the same substep.** A frame is
    /// chopped into whole [`CHAIN_MAX_STEP`]s, so 144 Hz steps 1/288 where
    /// 240 Hz steps 1/240, and the solver's own truncation between two step
    /// sizes is the larger effect on a chain this close to resonance: 2.5 px,
    /// which 288 Hz measures against 240 Hz just the same at one substep per
    /// frame. That is `CHAIN_MAX_STEP`'s accuracy and not the anchor's, so
    /// the tight bound here is asked of rates that share a substep, and
    /// 144 Hz is asked against 288 Hz.
    #[test]
    fn a_swept_anchor_draws_one_curve_at_every_frame_rate() {
        // Under twice the worst measured (0.22 px at 30 Hz, 0.05 at 60,
        // 0.01 at 120, 0.008 for 144 against 288), and a twentieth of what
        // the jump it replaced measured at 30 Hz.
        const TOL: f32 = 0.4;
        for (rate, reference) in [(30u32, 240u32), (60, 240), (120, 240), (144, 288)] {
            let gap = swept_gap(rate, reference);
            assert!(
                gap < TOL,
                "{rate} Hz drew {gap:.4} px from the {reference} Hz curve, tolerance {TOL}",
            );
        }

        // The one cross-substep pair, held to what the step size alone is
        // worth: 288 Hz differs from 240 Hz by the same amount at one substep
        // per frame, so none of it is the anchor.
        let mixed = swept_gap(144, 240);
        let step_size_alone = swept_gap(288, 240);
        assert!(
            mixed < 4.5 && (mixed - step_size_alone).abs() < TOL,
            "144 Hz is {mixed:.4} px from 240 Hz where the substep size alone is worth \
             {step_size_alone:.4} px",
        );
    }

    #[test]
    fn link_bends_reads_zero_hanging_and_a_quarter_turn_at_a_bent_joint() {
        let mut c = chain(0.5);
        c.settle_to_rest(Vec2::ZERO, DOWN);
        let mut bends = Vec::new();

        c.link_bends(Mat4::IDENTITY, &mut bends);
        assert_eq!(bends.len(), 3, "one bend per link");
        for (i, bend) in bends.iter().enumerate() {
            assert!(bend.abs() < 1e-6, "hanging link {i} bends {bend}");
        }

        // Joint 1 turned 45 degrees toward +X, joint 2 left straight: link 2
        // is parallel to link 1, so only the middle bend is non-zero.
        let diagonal = Vec2::splat(std::f32::consts::FRAC_1_SQRT_2);
        c.particles[1].pos = Vec2::new(0.0, c.links[0].length);
        c.particles[2].pos = c.particles[1].pos + diagonal * c.links[1].length;
        c.particles[3].pos = c.particles[2].pos + diagonal * c.links[2].length;

        c.link_bends(Mat4::IDENTITY, &mut bends);
        assert!(bends[0].abs() < 1e-6, "link 0 still hangs: {}", bends[0]);
        assert!(
            (bends[1] - 0.25).abs() < 1e-5,
            "joint 1 bends a quarter turn toward +X: {}",
            bends[1],
        );
        assert!(
            bends[2].abs() < 1e-6,
            "link 2 is parallel to link 1: {}",
            bends[2],
        );
    }

    /// One link, no weight and no drag of its own, so the bend spring is the
    /// only thing acting on it. `stiffness` is a frequency in Hz, and this is
    /// what says so: 1 Hz has to ring once a second.
    ///
    /// **The frequency a link names is the undamped one**, and the ring lands
    /// a few percent under it. The solver's own discretisation accounts for
    /// most of that — a 1 Hz spring measures 0.973 s with no damping at all —
    /// and [`BEND_SPRING_DAMPING_RATIO`] takes it to 0.968. The 5% here is
    /// sized to hold both while still failing anything that has the frequency
    /// wrong.
    #[test]
    fn a_weightless_sprung_link_rings_at_its_own_frequency() {
        let mut c = ParticleChainData::new(vec![ChainLink {
            length: 60.0,
            gravity_scale: 0.0,
            damping: 0.0,
            time_scale: 1.0,
            stiffness: 1.0,
        }]);
        c.settle_to_rest(Vec2::ZERO, DOWN);
        // A small bend, so the small-angle period the frequency names is the
        // one being measured.
        c.particles[1].pos = rotate_by(Vec2::new(0.0, c.links[0].length), 0.05);

        let mut bends = Vec::new();
        let mut crossings: Vec<f32> = Vec::new();
        let mut previous = 0.05f32;
        for f in 0..600 {
            c.tick(Vec2::ZERO, DOWN, 1.0 / 60.0);
            c.link_bends(Mat4::IDENTITY, &mut bends);
            let bend = bends[0];
            if (bend > 0.0) != (previous > 0.0) {
                // Where the sign flipped, to the frame.
                let t = (f as f32 - previous / (bend - previous)) / 60.0;
                crossings.push(t);
            }
            previous = bend;
        }

        assert!(
            crossings.len() >= 4,
            "1 Hz over ten seconds crosses zero many times, got {}",
            crossings.len(),
        );
        let half_turns = (crossings.len() - 1) as f32;
        let last = crossings[crossings.len() - 1];
        let period = 2.0 * (last - crossings[0]) / half_turns;
        assert!(
            (period - 1.0).abs() < 0.05,
            "1 Hz means a 1 s period, measured {period} s",
        );
    }

    /// The same link with damping on it stops, rather than ringing forever:
    /// the one existing velocity damping is what the spring is damped by, and
    /// there is no second knob for it.
    #[test]
    fn a_damped_sprung_link_returns_to_rest() {
        let mut c = ParticleChainData::new(vec![ChainLink {
            length: 60.0,
            gravity_scale: 0.0,
            damping: 0.6,
            time_scale: 1.0,
            stiffness: 1.0,
        }]);
        c.settle_to_rest(Vec2::ZERO, DOWN);
        c.particles[1].pos = rotate_by(Vec2::new(0.0, c.links[0].length), 0.4);

        // Forty seconds: the spring here is barely damped at all, and the
        // point is that it does stop, not how soon.
        for _ in 0..2400 {
            c.tick(Vec2::ZERO, DOWN, 1.0 / 60.0);
        }
        assert!(
            c.is_at_rest(1e-6),
            "a damped spring winds down to its rest pose, ended at {:?}",
            c.particles,
        );
    }

    /// A stiffness far past what the step can resolve, on a link whose clock
    /// runs four times as fast, so the substep is four times too coarse for
    /// it. The spring saturates instead of exploding: every number stays
    /// finite and the chain still ends up at rest.
    #[test]
    fn an_unresolvable_stiffness_saturates_instead_of_exploding() {
        let mut c = ParticleChainData::new(vec![
            ChainLink {
                length: 60.0,
                gravity_scale: 1.0,
                damping: 0.5,
                time_scale: 4.0,
                stiffness: 1e6,
            };
            3
        ]);
        c.gravity = 980.0;
        c.settle_to_rest(Vec2::ZERO, DOWN);
        c.particles[1].pos = rotate_by(Vec2::new(0.0, 60.0), 0.6);
        c.particles[2].pos = c.particles[1].pos + Vec2::new(60.0, 0.0);
        c.particles[3].pos = c.particles[2].pos + Vec2::new(0.0, -60.0);

        for f in 0..1200 {
            c.tick(Vec2::ZERO, DOWN, 1.0 / 60.0);
            for (i, p) in c.particles.iter().enumerate() {
                assert!(
                    p.pos.is_finite() && p.vel.is_finite(),
                    "frame {f} particle {i} left the numbers: {p:?}",
                );
            }
        }
        assert!(
            c.is_at_rest(1e-6),
            "the saturated spring still settles, ended at {:?}",
            c.particles,
        );
    }

    /// **A strand of equal links must not ring itself.** Every link is an
    /// oscillator driven by the link above it at the very same frequency, so
    /// before [`BEND_SPRING_DAMPING_RATIO`] existed these four were still
    /// doing 574 px/s three minutes after one kick, and never stopped. It is
    /// the equal frequencies that do it: staggering the same four at 8, 6, 4
    /// and 2 Hz settled them in seventeen seconds without any damping floor
    /// at all.
    #[test]
    fn a_strand_of_equal_links_does_not_ring_itself() {
        let tilted = rotate_by(DOWN, 0.9);
        let mut c = ParticleChainData::new(vec![
            ChainLink {
                length: 60.0,
                gravity_scale: 1.0,
                damping: 0.5,
                time_scale: 1.0,
                stiffness: 8.0,
            };
            4
        ]);
        c.gravity = 980.0;
        c.settle_to_rest(Vec2::ZERO, tilted);
        for particle in c.particles.iter_mut().skip(1) {
            particle.pos += Vec2::new(5.0, -2.0);
        }

        let mut quiet_at = None;
        for f in 0..600 {
            c.tick(Vec2::ZERO, tilted, 1.0 / 60.0);
            for (i, particle) in c.particles.iter().enumerate() {
                assert!(
                    particle.pos.is_finite() && particle.vel.is_finite(),
                    "frame {f} particle {i} left the numbers: {particle:?}",
                );
            }
            if quiet_at.is_none() && c.is_at_rest(1e-6) {
                quiet_at = Some(f);
            }
        }
        let quiet_at = quiet_at.expect("four equal 8 Hz links stop within ten seconds");
        assert!(
            quiet_at < 600,
            "settled at frame {quiet_at}, which is inside the ten seconds",
        );
    }

    /// A sprung chain that has *relaxed* into rest reads as at rest, not just
    /// one that was placed there. This is the case the analytic-pose
    /// comparison used to fail: these chains stop a few thousandths of a
    /// pixel from where `settle_to_rest` puts them, so a test against that
    /// pose called them moving forever.
    #[test]
    fn a_kicked_sprung_chain_reads_as_at_rest_once_it_stops() {
        for (links, stiffness, damping) in [
            (1usize, 4.0f32, 0.5f32),
            (2, 3.0, 0.5),
            (2, 4.0, 0.5),
            (3, 4.0, 0.5),
            (3, 2.0, 0.3),
        ] {
            let tilted = rotate_by(DOWN, 0.5);
            let mut c = ParticleChainData::new(vec![
                ChainLink {
                    length: 60.0,
                    gravity_scale: 1.0,
                    damping,
                    time_scale: 1.0,
                    stiffness,
                };
                links
            ]);
            c.gravity = 980.0;
            c.settle_to_rest(Vec2::ZERO, tilted);
            for particle in c.particles.iter_mut().skip(1) {
                particle.pos += Vec2::new(5.0, -2.0);
            }
            assert!(
                !c.is_at_rest(1e-6) || {
                    c.tick(Vec2::ZERO, tilted, 1.0 / 60.0);
                    !c.is_at_rest(1e-6)
                },
                "{links}x{stiffness}Hz: a displaced chain is about to move",
            );

            // Two minutes, which is far longer than any of these take; the
            // point is that they stop at all, not how soon.
            for _ in 0..7200 {
                c.tick(Vec2::ZERO, tilted, 1.0 / 60.0);
            }
            let before: Vec<Vec2> = c.particles.iter().map(|p| p.pos).collect();
            for _ in 0..120 {
                c.tick(Vec2::ZERO, tilted, 1.0 / 60.0);
            }
            for (i, was) in before.iter().enumerate() {
                assert_eq!(
                    c.particles[i].pos, *was,
                    "{links}x{stiffness}Hz: particle {i} is still moving",
                );
            }
            assert!(
                c.is_at_rest(1e-6),
                "{links}x{stiffness}Hz: a chain that has stopped reads as at rest, \
                 velocities {:?}",
                c.particles.iter().map(|p| p.vel).collect::<Vec<_>>(),
            );
        }
    }

    /// [`ParticleChainData::settle_to_rest`] claims to be the solver's own
    /// equilibrium and not merely near it, so stepping a settled chain has to
    /// leave it exactly where it is — under a tilted down, where the spring
    /// and gravity pull different ways and the pose is nothing like a
    /// straight hang, and with real weight on every link, which is the case
    /// the rod projection's rounding used to turn into a permanent wobble.
    ///
    /// Three links, because the wobble compounded down a chain: each link
    /// read its parent's jitter as a bend and answered it with `w0^2 *
    /// length` of spring.
    #[test]
    fn a_settled_sprung_chain_is_a_fixed_point_of_the_step() {
        let tilted = rotate_by(DOWN, 0.5);
        let mut c = ParticleChainData::new(vec![
            ChainLink {
                length: 60.0,
                gravity_scale: 1.0,
                damping: 0.5,
                time_scale: 1.0,
                stiffness: 4.0,
            };
            3
        ]);
        c.gravity = 980.0;
        c.settle_to_rest(Vec2::ZERO, tilted);
        let settled: Vec<Vec2> = c.particles.iter().map(|p| p.pos).collect();

        // The pose is a real compromise between the two pulls and not either
        // one of them: a spring this stiff wins, but gravity still shows.
        let first = (c.particles[1].pos - c.particles[0].pos).normalize();
        assert!(
            first.distance(tilted) > 1e-3 && first.distance(DOWN) > 1e-3,
            "the first link balances the node's down against gravity, got {first}",
        );

        let mut worst = 0.0f32;
        for _ in 0..200 {
            c.tick(Vec2::ZERO, tilted, 1.0 / 60.0);
            for (i, was) in settled.iter().enumerate() {
                worst = worst.max(c.particles[i].pos.distance(*was));
            }
        }
        // `SETTLE_EPS_SQ`, the crate's one notion of settled, squares 1e-3 —
        // but there is nothing to spend it on: the pose does not move at all.
        assert!(
            worst < 1e-3,
            "the settled pose drifted by {worst} px over 200 frames",
        );
        for (i, particle) in c.particles.iter().enumerate() {
            assert_eq!(
                particle.vel,
                Vec2::ZERO,
                "particle {i} carries a velocity at rest: {:?}",
                particle.vel,
            );
        }
        assert!(
            c.is_at_rest(1e-6),
            "and the chain reads as at rest, so a viewport over it may idle",
        );
    }
}
