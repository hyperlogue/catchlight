//! Root-driven strand: a chain of points hung from its root, advanced at a
//! fixed rate in one root-to-tip pass per step.
//!
//! Each step places every point in order: Verlet inertia with frame-relative
//! damping (`kd`, on motion relative to the root's own translation and turn)
//! and air drag (`ka`, on absolute motion), gravity, a pull of
//! `alpha = (2 pi stiffness h)^2` toward the drawn (or posed) turn carried by
//! the link above, an optional positional clamp to the link's limit, then
//! follow-the-leader placement at the link's exact length. The parent's
//! previous position absorbs `DFTL` of each child's length correction (Müller
//! et al. 2012), so upper links do not act heavier than the tips. There are no
//! solves or iterations; lengths are exact by construction.
//!
//! **Time.** Steps are `1 / rate` seconds from an accumulator. Root and frame
//! are interpolated across the frame, root velocity is measured from the last
//! *step's* root (so zero-step frames lose no motion), and output bends are
//! interpolated between the last two steps: a fixed one-step latency. A tick
//! simulates at most `MAX_TICK_SECONDS` (8 steps at 120 Hz); the rest of a
//! long frame is dropped, at any rate. A root jump longer than the strand
//! moves it rigidly with zero velocity. A tick with no time (`dt <= 0`)
//! carries the strand rigidly to its root and wakes it if the frame or pose
//! changed, so the next real tick swings toward them.
//!
//! **Sleep.** After `SLEEP_SECONDS` with unchanged input in which no point
//! moved faster than `QUIET_SPEED`, the strand sleeps: a tick with the same
//! root, frame and pose runs no step. A strand still creeping toward rest
//! slower than that stops short of it by about `QUIET_SPEED` times its
//! slowest time constant: hundredths of a pixel on a tuned strand.
//!
//! **Neutral rest.** Output is `bend - b0`, where `b0` is how far the strand
//! rests off its pose in its rest frame with the runtime constants (the rest
//! shape depends on damping too). A settled strand in its rest frame therefore
//! reads its pose: zero renders the drawing, and a keyed bend renders the key
//! however far gravity sags the strand off it. `b0` comes from the closed-form
//! balance (each link along the sum of its spring's pull, gravity and the
//! length correction its child hands up), so it is remeasured cheaply whenever
//! the posed bends change. The rest frame is the live carry turned so the root
//! points along `rest_dir`, the node's direction at its authored rest pose; a
//! live root within 1e-6 of it uses the live carry as is, and a strand hung in
//! that frame reads its pose exactly.
//!
//! **Hanging.** A hang starts from the balance turned `SETTLE_NUDGE` off it,
//! so a drawing on an unstable balance falls, then steps in place until
//! nothing moves.
//!
//! Geometry is world space (physics Y-down): link lengths and drawn turns are
//! the node's rest rods mapped through the carry. A carry whose metric
//! (`C^T C`) or handedness changes rebuilds them. Bends are read through the
//! carry's inverse so nonuniform scale and mirrors report node-frame bends.

use smallvec::SmallVec;

use super::{ChainLink, ChainParticle};
use crate::{Mat2, Vec2};

type Links<T> = SmallVec<[T; 8]>;

/// Share of a child's length correction passed to its parent's velocity.
const DFTL: f32 = 0.9;
/// Most simulated time a single tick advances; a longer frame drops the
/// rest. As steps, `rate / 15`: 8 at the default 120 Hz.
pub(super) const MAX_TICK_SECONDS: f32 = 1.0 / 15.0;
/// Quiet time before a strand sleeps.
const SLEEP_SECONDS: f32 = 0.2;
/// Point speed (px/s) below which a step counts as quiet.
const QUIET_SPEED: f32 = 0.12;
/// Step budget for settling; it stops early once nothing moves.
const SETTLE_STEPS: u32 = 4000;
/// Squared per-step motion (px²) at which settling has converged.
const SETTLED_SQ: f32 = 1e-10;
/// Turn (radians) each link starts settling from off its target.
const SETTLE_NUDGE: f32 = 1e-3;
/// Sweeps the closed-form balance takes at most; it stops once no link turns.
const BALANCE_SWEEPS: u32 = 64;
/// Squared change of a link's direction at which the balance has converged.
const BALANCED_SQ: f32 = 1e-12;
/// Relative change in the carry's metric that rebuilds the geometry.
const METRIC_EPS: f32 = 1e-4;

fn cmul(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x)
}

fn cconj(a: Vec2) -> Vec2 {
    Vec2::new(a.x, -a.y)
}

fn unit(angle: f32) -> Vec2 {
    Vec2::new(angle.cos(), angle.sin())
}

/// What the baked constants derive from; a change rebuilds the strand.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Settings {
    pub links: Links<ChainLink>,
    pub gravity: f32,
    pub damping: f32,
    pub air_drag: f32,
    pub rate: f32,
}

/// `C^T C` and handedness: everything about the carry except its rotation.
fn metric(carry: Mat2) -> [f32; 4] {
    let (x, y) = (carry.x_axis, carry.y_axis);
    [x.dot(x), x.dot(y), y.dot(y), carry.determinant().signum()]
}

fn same_metric(a: [f32; 4], b: [f32; 4]) -> bool {
    let scale = a[0].abs().max(a[2].abs()).max(1e-12);
    a[3] == b[3] && (0..3).all(|i| (a[i] - b[i]).abs() <= METRIC_EPS * scale)
}

/// The step's rate in Hz, made usable.
fn usable_rate(rate: f32) -> f32 {
    if rate.is_finite() && rate >= 1.0 {
        rate
    } else {
        super::DEFAULT_PHYSICS_RATE
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Strand {
    settings: Settings,
    metric: [f32; 4],
    // Baked.
    drawn0: Vec2,
    len: Links<f32>,
    total: f32,
    rel: Links<Vec2>,
    alpha: Links<f32>,
    limit: Links<Option<(f32, Vec2)>>,
    g: Links<Vec2>,
    kd: f32,
    ka: f32,
    h: f32,
    mirror: f32,
    rest_dir: Vec2,
    /// The live carry turned so the root points along `rest_dir`; turning the
    /// carry does not change it, so it is baked with the geometry.
    rest_carry: Mat2,
    /// The last rest-frame balance, each link's direction and the push its
    /// child hands up, which the next measure of `b0` starts from.
    rest_balance: (Links<Vec2>, Links<Vec2>),
    b0: Links<f32>,
    // Pose targets.
    posed: Links<f32>,
    pose_turn: Links<Vec2>,
    /// The previous frame's posed bends, kept for blending across a tick.
    pose_scratch: Links<f32>,
    // State.
    p: Links<Vec2>,
    q: Links<Vec2>,
    corr: Links<Vec2>,
    root: Vec2,
    carry: Mat2,
    step_dir: Vec2,
    acc: f32,
    asleep: bool,
    quiet: u32,
    out_prev: Links<f32>,
    out: Links<f32>,
    steps: u32,
    /// Largest squared point displacement in the last step.
    moved: f32,
}

impl Strand {
    /// Bake `settings` for `carry`, measure the neutral rest in the frame
    /// whose root points along `rest_dir`, and hang the strand at rest at
    /// `root` with `posed` targets.
    pub(super) fn new(
        settings: Settings,
        root: Vec2,
        carry: Mat2,
        posed: &[f32],
        rest_dir: Vec2,
    ) -> Self {
        let mut strand = Self::bake(settings, root, carry, posed, rest_dir);
        strand.settle(root, carry, posed);
        strand
    }

    /// Bake `settings` for `carry` and measure the neutral rest, without
    /// hanging: the points sit at `root` until a hang or [`Self::adopt`]
    /// places them. The neutral rest is the closed-form balance, so this is
    /// cheap enough to run on every carry rescale.
    pub(super) fn bake(
        settings: Settings,
        root: Vec2,
        carry: Mat2,
        posed: &[f32],
        rest_dir: Vec2,
    ) -> Self {
        let n = settings.links.len();
        let h = 1.0 / usable_rate(settings.rate);
        let drawn0 = settings
            .links
            .first()
            .map_or(Vec2::Y, |l| l.drawn.try_normalize().unwrap_or(Vec2::Y));
        let mut len = Links::new();
        let mut rel = Links::new();
        let mut parent = (carry * drawn0).try_normalize().unwrap_or(Vec2::Y);
        for link in &settings.links {
            let drawn = link.drawn.try_normalize().unwrap_or(Vec2::Y);
            let rod = carry * (drawn * link.length.max(1e-4));
            let length = rod.length().max(1e-6);
            let dir = rod / length;
            len.push(length);
            rel.push(cmul(dir, cconj(parent)));
            parent = dir;
        }
        let alpha = settings
            .links
            .iter()
            .map(|l| {
                let a = std::f32::consts::TAU * l.stiffness.max(0.0) * h;
                (a * a).min(1.0)
            })
            .collect();
        let limit = settings
            .links
            .iter()
            .map(|l| {
                l.limit.filter(|x| x.is_finite() && *x > 0.0).map(|x| {
                    let a = x.min(1.0) * std::f32::consts::PI;
                    (a.cos(), unit(a))
                })
            })
            .collect();
        let g = settings
            .links
            .iter()
            .map(|l| Vec2::new(0.0, settings.gravity * l.gravity_scale) * (h * h))
            .collect();
        let dir = (carry * drawn0).try_normalize().unwrap_or(Vec2::Y);
        let mut strand = Self {
            metric: metric(carry),
            drawn0,
            total: len.iter().sum(),
            len,
            rel,
            alpha,
            limit,
            g,
            kd: (-settings.damping.max(0.0) * h).exp(),
            ka: (-settings.air_drag.max(0.0) * h).exp(),
            h,
            mirror: if carry.determinant() < 0.0 { -1.0 } else { 1.0 },
            rest_dir,
            rest_carry: carry,
            rest_balance: (Links::new(), Links::new()),
            b0: smallvec::smallvec![0.0; n],
            posed: smallvec::smallvec![0.0; n],
            pose_turn: smallvec::smallvec![Vec2::X; n],
            pose_scratch: Links::new(),
            p: smallvec::smallvec![root; n + 1],
            q: smallvec::smallvec![root; n + 1],
            corr: smallvec::smallvec![Vec2::ZERO; n + 1],
            root,
            carry,
            step_dir: dir,
            acc: 0.0,
            asleep: true,
            quiet: 0,
            out_prev: smallvec::smallvec![0.0; n],
            out: smallvec::smallvec![0.0; n],
            steps: 0,
            moved: 0.0,
            settings,
        };
        // The rest frame: the live carry turned so its root points along
        // `rest_dir`. When they already agree the live carry is used as is,
        // so hanging there reads the pose exactly.
        strand.rest_carry =
            if strand.rest_dir.dot(dir) > 0.0 && strand.rest_dir.perp_dot(dir).abs() < 1e-6 {
                carry
            } else {
                let turn = cmul(strand.rest_dir, cconj(dir));
                Mat2::from_cols(turn, turn.perp()) * carry
            };
        if !strand.set_posed(posed) {
            strand.measure_neutral();
        }
        strand
    }

    /// Hang at rest for new input, keeping the baked constants and neutral
    /// offset. A sleeping strand already at this input does nothing.
    pub(super) fn rest(&mut self, root: Vec2, carry: Mat2, posed: &[f32]) {
        let posed_changed = self.set_posed(posed);
        if self.asleep && !posed_changed && root == self.root && carry == self.carry {
            return;
        }
        self.settle(root, carry, posed);
    }

    pub(super) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(super) fn fits(&self, carry: Mat2) -> bool {
        carry == self.carry || same_metric(self.metric, metric(carry))
    }

    pub(super) fn rest_dir(&self) -> Vec2 {
        self.rest_dir
    }

    /// Carry another strand's motion over to this freshly baked one, so an
    /// edit or a rescale continues the swing instead of re-hanging it.
    /// `false`, leaving this strand as baked, when the old one has a
    /// different link count or a non-finite point: then it must be hung.
    pub(super) fn adopt(&mut self, old: &Strand) -> bool {
        let finite = old.p.iter().chain(&old.q).all(|v| v.is_finite());
        if old.p.len() != self.p.len() || !finite {
            return false;
        }
        self.p.clone_from(&old.p);
        self.q.clone_from(&old.q);
        self.root = old.root;
        self.carry = old.carry;
        self.step_dir = old.step_dir;
        self.acc = old.acc.min(self.h * 0.999);
        self.out_prev.clone_from(&old.out_prev);
        self.out.clone_from(&old.out);
        self.asleep = false;
        self.quiet = 0;
        true
    }

    /// Hang this strand at rest for `root`, `carry` and `posed`.
    pub(super) fn hang_at(&mut self, root: Vec2, carry: Mat2, posed: &[f32]) {
        self.settle(root, carry, posed);
    }

    fn dir(&self, carry: Mat2) -> Vec2 {
        (carry * self.drawn0)
            .try_normalize()
            .unwrap_or(self.step_dir)
    }

    /// Update the pose targets, remeasuring the neutral offset for them;
    /// `true` when they changed.
    fn set_posed(&mut self, posed: &[f32]) -> bool {
        let mut changed = false;
        for i in 0..self.posed.len() {
            let b = posed
                .get(i)
                .copied()
                .filter(|b| b.is_finite())
                .unwrap_or(0.0);
            if b != self.posed[i] {
                self.posed[i] = b;
                self.pose_turn[i] = unit(-b * std::f32::consts::PI * self.mirror);
                changed = true;
            }
        }
        if changed {
            self.measure_neutral();
        }
        changed
    }

    /// Measure `b0` for the current pose: balance the strand in the rest
    /// frame and keep how far it lies off the pose.
    fn measure_neutral(&mut self) {
        let dir = self.dir(self.rest_carry);
        let (mut u, mut push) = std::mem::take(&mut self.rest_balance);
        self.balance(dir, &mut u, &mut push);
        let mut p: SmallVec<[Vec2; 16]> = SmallVec::with_capacity(u.len() + 1);
        p.push(Vec2::ZERO);
        for (u, l) in u.iter().zip(&self.len) {
            p.push(p[p.len() - 1] + *u * *l);
        }
        let mut b0 = std::mem::take(&mut self.b0);
        self.bends_of(&p, dir, self.rest_carry, &mut b0);
        for (b, pose) in b0.iter_mut().zip(&self.posed) {
            *b -= pose;
        }
        self.b0 = b0;
        self.rest_balance = (u, push);
    }

    /// Each link's direction at rest under root direction `dir`, written into
    /// `u`: the fixed point of [`Self::step`] with a still root, solved
    /// directly. A link at rest lies along `w = (1 - alpha) f + alpha l want`,
    /// where `f` is gravity plus the share of its child's length correction
    /// the step hands up as velocity; the other root of that balance is
    /// unstable. A limit clamps the direction as the step does.
    ///
    /// `u` and `push` (what each link's child hands up) start from a previous
    /// balance when they hold one, so a pose that moves a little per frame
    /// rebalances in a few sweeps; empty, they start from the pose.
    fn balance(&self, dir: Vec2, u: &mut Links<Vec2>, push: &mut Links<Vec2>) {
        let n = self.len.len();
        if u.len() != n || push.len() != n {
            u.clear();
            let mut parent = dir;
            for i in 0..n {
                parent = cmul(cmul(parent, self.rel[i]), self.pose_turn[i]);
                u.push(parent);
            }
            push.clear();
            push.resize(n, Vec2::ZERO);
        }
        let mut want: SmallVec<[Vec2; 16]> = smallvec::smallvec![Vec2::ZERO; n];
        let hand_up = DFTL * self.kd * self.ka;
        for _ in 0..BALANCE_SWEEPS {
            let mut parent = dir;
            let mut change = 0.0f32;
            for i in 0..n {
                let (a, l) = (self.alpha[i], self.len[i]);
                let drawn = cmul(parent, self.rel[i]);
                want[i] = cmul(drawn, self.pose_turn[i]);
                let w = (self.g[i] + push[i]) * (1.0 - a) + want[i] * (a * l);
                let mut v = w.try_normalize().unwrap_or(u[i]);
                if let Some((cos, turn)) = self.limit[i] {
                    if drawn.dot(v) < cos {
                        v = if drawn.perp_dot(v) >= 0.0 {
                            cmul(drawn, turn)
                        } else {
                            cmul(drawn, cconj(turn))
                        };
                    }
                }
                change = change.max(v.distance_squared(u[i]));
                u[i] = v;
                parent = v;
            }
            // The step moves each joint's previous position by `DFTL` of the
            // correction placing the link below it, which reads as velocity.
            for i in (1..n).rev() {
                let (a, l) = (self.alpha[i], self.len[i]);
                let x = (u[i] * l + self.g[i] + push[i]) * (1.0 - a) + want[i] * (a * l);
                push[i - 1] = (x - u[i] * l) * hand_up;
            }
            if change <= BALANCED_SQ {
                break;
            }
        }
    }

    /// Place the strand at its balance under root direction `dir`, a nudge
    /// off it, and step in place until it stops.
    fn hang(&mut self, root: Vec2, dir: Vec2) {
        let (mut u, mut push) = (Links::new(), Links::new());
        self.balance(dir, &mut u, &mut push);
        self.p[0] = root;
        // Start a hair off the balance, so a strand drawn on an unstable one
        // (upright, say) falls off it instead of resting there.
        let nudge = unit(SETTLE_NUDGE);
        for (i, u) in u.iter().enumerate() {
            self.p[i + 1] = self.p[i] + cmul(*u, nudge) * self.len[i];
        }
        self.q.clone_from(&self.p);
        for _ in 0..SETTLE_STEPS {
            self.step(root, dir, Vec2::ZERO, 0.0);
            if self.moved <= SETTLED_SQ {
                break;
            }
        }
    }

    /// Hang the strand on its targets and simulate in place until it stops.
    fn settle(&mut self, root: Vec2, carry: Mat2, posed: &[f32]) {
        self.set_posed(posed);
        let dir = self.dir(carry);
        self.hang(root, dir);
        self.q.clone_from(&self.p);
        self.root = root;
        self.carry = carry;
        self.step_dir = dir;
        self.acc = 0.0;
        if carry == self.rest_carry {
            // Hung in the frame `b0` was measured in: it reads its pose, which
            // the hang reaches only to its stopping tolerance.
            self.out.clear();
            self.out
                .extend(self.posed.iter().zip(&self.b0).map(|(p, b)| p + b));
        } else {
            let mut out = std::mem::take(&mut self.out);
            self.bends(dir, carry, &mut out);
            self.out = out;
        }
        self.out_prev.clone_from(&self.out);
        self.asleep = true;
        self.quiet = 0;
    }

    /// One fixed step: `root`/`dir` are this step's, `vr` the root's
    /// displacement since the last step and `w` the sine of its turn.
    #[inline]
    fn step(&mut self, root: Vec2, dir: Vec2, vr: Vec2, w: f32) {
        self.q[0] = self.p[0];
        self.p[0] = root;
        let mut parent = dir;
        let n = self.len.len();
        let mut moved = 0.0f32;
        for i in 1..=n {
            let pi = self.p[i];
            let v = pi - self.q[i];
            let vf = vr + (pi - root).perp() * w;
            let v = (vf + (v - vf) * self.kd) * self.ka;
            self.q[i] = pi;
            let mut x = pi + v + self.g[i - 1];
            let drawn = cmul(parent, self.rel[i - 1]);
            let want = cmul(drawn, self.pose_turn[i - 1]);
            let anchor = self.p[i - 1];
            x += (anchor + want * self.len[i - 1] - x) * self.alpha[i - 1];
            let d = x - anchor;
            let l = d.length();
            let mut u = if l > 1e-6 { d / l } else { want };
            if let Some((cos, turn)) = self.limit[i - 1] {
                if drawn.dot(u) < cos {
                    u = if drawn.perp_dot(u) >= 0.0 {
                        cmul(drawn, turn)
                    } else {
                        cmul(drawn, cconj(turn))
                    };
                }
            }
            let placed = anchor + u * self.len[i - 1];
            self.corr[i] = placed - x;
            moved = moved.max(placed.distance_squared(pi));
            self.p[i] = placed;
            parent = u;
        }
        self.moved = moved;
        for i in 1..n {
            self.q[i] += self.corr[i + 1] * DFTL;
        }
    }

    /// Raw node-frame bends in half turns for root direction `dir` under
    /// `carry`, without the neutral offset.
    fn bends(&self, dir: Vec2, carry: Mat2, out: &mut Links<f32>) {
        self.bends_of(&self.p, dir, carry, out);
    }

    /// [`Self::bends`] of the points `p`.
    fn bends_of(&self, p: &[Vec2], dir: Vec2, carry: Mat2, out: &mut Links<f32>) {
        out.clear();
        let inverse = carry.inverse();
        let mut parent = dir;
        for i in 0..self.len.len() {
            let u = (p[i + 1] - p[i]) / self.len[i];
            let a = inverse * cmul(parent, self.rel[i]);
            let b = inverse * u;
            parent = u;
            out.push(-a.perp_dot(b).atan2(a.dot(b)) / std::f32::consts::PI);
        }
    }

    fn write_out(&mut self, dir: Vec2, carry: Mat2) {
        std::mem::swap(&mut self.out_prev, &mut self.out);
        let mut out = std::mem::take(&mut self.out);
        self.bends(dir, carry, &mut out);
        self.out = out;
    }

    /// Advance one render frame of `dt` seconds toward the inputs sampled at
    /// its end.
    pub(super) fn tick(&mut self, root: Vec2, carry: Mat2, posed: &[f32], dt: f32) {
        self.steps = 0;
        let mut pose0 = std::mem::take(&mut self.pose_scratch);
        pose0.clone_from(&self.posed);
        let posed_changed = self.set_posed(posed);
        self.advance(root, carry, &pose0, posed_changed, dt);
        self.pose_scratch = pose0;
    }

    /// [`Self::tick`] after the pose targets are updated; `pose0` holds the
    /// previous frame's posed bends, which the steps blend from.
    fn advance(&mut self, root: Vec2, carry: Mat2, pose0: &[f32], posed_changed: bool, dt: f32) {
        if self.asleep && root == self.root && carry == self.carry && !posed_changed {
            return;
        }
        if !(dt.is_finite() && dt > 0.0) {
            // No time passes: carry the strand along rigidly. A turned frame
            // or a new pose is off this rest, so stay awake for the next real
            // tick to swing toward it.
            if carry != self.carry || posed_changed {
                self.asleep = false;
                self.quiet = 0;
            }
            self.translate(root - self.root);
            self.root = root;
            self.carry = carry;
            self.step_dir = self.dir(carry);
            return;
        }
        self.asleep = false;
        if root.distance(self.root) > self.total {
            self.translate(root - self.root);
            self.root = root;
        }
        let max_steps = self.max_steps();
        let dt = dt.min(max_steps as f32 * self.h);
        let (r0, c0) = (self.root, self.carry);
        let (d0, d1) = (self.dir(c0), self.dir(carry));
        let acc0 = self.acc;
        let steps = (((acc0 + dt) / self.h).floor() as u32).min(max_steps);
        self.acc = (acc0 + dt - steps as f32 * self.h).clamp(0.0, self.h);
        let mut prev_root = self.p[0];
        let mut prev_dir = self.step_dir;
        let mut moved = 0.0f32;
        for k in 1..=steps {
            let f = ((k as f32 * self.h - acc0) / dt).clamp(0.0, 1.0);
            let r = r0.lerp(root, f);
            let d = d0.lerp(d1, f).try_normalize().unwrap_or(d1);
            if posed_changed {
                // The pose is input like the root: blend it across the frame
                // so a key moving under a long frame does not jump.
                let f = if k == steps { 1.0 } else { f };
                for (i, turn) in self.pose_turn.iter_mut().enumerate() {
                    let b = pose0[i] + (self.posed[i] - pose0[i]) * f;
                    *turn = unit(-b * std::f32::consts::PI * self.mirror);
                }
            }
            self.step(r, d, r - prev_root, prev_dir.perp_dot(d));
            prev_root = r;
            prev_dir = d;
            if k + 1 >= steps {
                let c = Mat2::from_cols(
                    c0.x_axis.lerp(carry.x_axis, f),
                    c0.y_axis.lerp(carry.y_axis, f),
                );
                self.write_out(d, c);
                if k == steps {
                    moved = self.moved;
                }
            }
        }
        self.step_dir = prev_dir;
        self.steps = steps;
        let still = root == r0 && carry == c0 && !posed_changed;
        let quiet_step = QUIET_SPEED * self.h;
        if still && moved < quiet_step * quiet_step {
            self.quiet += steps;
        } else {
            self.quiet = 0;
        }
        self.root = root;
        self.carry = carry;
        if self.quiet as f32 * self.h >= SLEEP_SECONDS - 0.5 * self.h {
            self.asleep = true;
            self.q.clone_from(&self.p);
            self.out_prev.clone_from(&self.out);
        }
    }

    /// Most steps one tick runs at this strand's rate.
    pub(super) fn max_steps(&self) -> u32 {
        ((MAX_TICK_SECONDS / self.h).round() as u32).max(1)
    }

    fn translate(&mut self, by: Vec2) {
        for (p, q) in self.p.iter_mut().zip(self.q.iter_mut()) {
            *p += by;
            *q = *p;
        }
    }

    /// Displace every point below the root by `offset` and stop it there.
    pub(super) fn kick(&mut self, offset: Vec2) {
        for (p, q) in self.p.iter_mut().zip(self.q.iter_mut()).skip(1) {
            *p += offset;
            *q = *p;
        }
        if offset != Vec2::ZERO {
            self.asleep = false;
            self.quiet = 0;
        }
    }

    pub(super) fn asleep(&self) -> bool {
        self.asleep
    }

    pub(super) fn steps(&self) -> u32 {
        self.steps
    }

    /// How far the strand settles off its pose in its rest frame: the offset
    /// subtracted from the output.
    pub(super) fn neutral(&self) -> &[f32] {
        &self.b0
    }

    /// Rendered bends: one step of latency, interpolated by the accumulator.
    /// The steps store raw bends and the neutral offset comes off here, so a
    /// pose that moves `b0` during a frame shifts both stored steps alike and
    /// the result does not depend on how the frame was cut.
    pub(super) fn link_bends(&self, out: &mut Vec<f32>) {
        let f = (self.acc / self.h).clamp(0.0, 1.0);
        out.clear();
        out.extend(
            self.out_prev
                .iter()
                .zip(&self.out)
                .zip(&self.b0)
                .map(|((a, b), b0)| a + (b - a) * f - b0),
        );
    }

    pub(super) fn export(&self, particles: &mut Vec<ChainParticle>) {
        particles.clear();
        particles.extend(self.p.iter().zip(&self.q).map(|(p, q)| ChainParticle {
            pos: *p,
            vel: (*p - *q) / self.h,
        }));
    }
}
