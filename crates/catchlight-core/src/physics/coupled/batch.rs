//! Four independent 2/8-link systems, with chain index in the innermost lane.
//! Safe fixed-size arrays let LLVM emit SIMD without unsafe intrinsics.
//! Bounds use scalar solves per chain; a failed lane cannot poison another.

use super::*;

#[derive(Clone, Copy, Default)]
struct Lanes([f64; 4]);

macro_rules! arithmetic {
    ($trait:ident, $method:ident, $op:tt) => {
        impl std::ops::$trait for Lanes {
            type Output = Self;
            #[inline(always)]
            fn $method(self, rhs: Self) -> Self {
                Self(std::array::from_fn(|i| self.0[i] $op rhs.0[i]))
            }
        }
    };
}
arithmetic!(Add, add, +);
arithmetic!(Sub, sub, -);
arithmetic!(Mul, mul, *);
arithmetic!(Div, div, /);

impl Lanes {
    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(self.0.map(f64::sqrt))
    }
}

fn kernel<const N: usize>(a: &mut [Lanes; 64], b: &mut [Lanes; 8]) {
    if N == 2 {
        let determinant = a[0] * a[3] - a[1] * a[2];
        let x = (b[0] * a[3] - b[1] * a[1]) / determinant;
        b[1] = (a[0] * b[1] - a[2] * b[0]) / determinant;
        b[0] = x;
        return;
    }
    for i in 0..N {
        for j in 0..=i {
            let mut value = a[i * N + j];
            for k in 0..j {
                value = value - a[i * N + k] * a[j * N + k];
            }
            a[i * N + j] = if i == j {
                value.sqrt()
            } else {
                value / a[j * N + j]
            };
        }
    }
    substitute::<N>(a, b);
}

fn substitute<const N: usize>(a: &[Lanes; 64], b: &mut [Lanes; 8]) {
    for i in 0..N {
        for j in 0..i {
            b[i] = b[i] - a[i * N + j] * b[j];
        }
        b[i] = b[i] / a[i * N + i];
    }
    for i in (0..N).rev() {
        for j in i + 1..N {
            b[i] = b[i] - a[j * N + i] * b[j];
        }
        b[i] = b[i] / a[i * N + i];
    }
}

pub(super) fn solve(
    frames: &mut [Frame<'_>],
    systems: &[System],
    h: f64,
) -> SmallVec<[Option<Vector>; 4]> {
    let n = systems[0].rhs.len();
    let mut result = reuse(frames, systems, h);
    let pending: [bool; 4] = std::array::from_fn(|i| result[i].is_none() && !frames[i].failed);
    // Packing is a net loss for a single fresh factor among reused lanes.
    if pending.iter().filter(|p| **p).count() < 2 {
        for i in 0..4 {
            if pending[i] {
                result[i] = frames[i].state.factor.fresh(&systems[i], h);
            }
        }
        return result;
    }
    let mut a = [Lanes::default(); 64];
    let mut b = [Lanes::default(); 8];
    for (i, a) in a.iter_mut().enumerate().take(n * n) {
        *a = Lanes(std::array::from_fn(|lane| {
            if pending[lane] {
                systems[lane].a[i]
            } else if i / n == i % n {
                1.0
            } else {
                0.0
            }
        }));
    }
    for (i, b) in b.iter_mut().enumerate().take(n) {
        *b = Lanes(std::array::from_fn(|lane| {
            if pending[lane] {
                systems[lane].rhs[i]
            } else {
                0.0
            }
        }));
    }
    if n == 2 {
        kernel::<2>(&mut a, &mut b);
    } else {
        kernel::<8>(&mut a, &mut b);
    }
    for lane in 0..4 {
        if !pending[lane] {
            continue;
        }
        let cache = &mut frames[lane].state.factor;
        cache.valid = false;
        let x: Vector = b[..n].iter().map(|v| v.0[lane]).collect();
        let valid = if n == 2 {
            let a = &systems[lane].a;
            let det = a[0] * a[3] - a[1] * a[2];
            det.is_finite() && det > 0.0
        } else {
            (0..n).all(|i| a[i * n + i].0[lane].is_finite() && a[i * n + i].0[lane] > 0.0)
        };
        if !valid || x.iter().any(|x| !x.is_finite()) {
            continue;
        }
        if !feasible(&systems[lane], &x) {
            result[lane] = constrained(&systems[lane]);
            continue;
        }
        if n > 2 {
            cache.l.clear();
            cache.l.extend(a[..n * n].iter().map(|v| v.0[lane]));
            cache.valid = systems[lane].reusable;
            cache.age = 0;
            cache.h = h;
        }
        result[lane] = Some(x);
    }
    result
}

fn reuse(frames: &mut [Frame<'_>], systems: &[System], h: f64) -> SmallVec<[Option<Vector>; 4]> {
    let mut result = smallvec![None;4];
    if !REUSE_FACTORS || systems[0].rhs.len() != 8 {
        return result;
    }
    let eligible: [bool; 4] = std::array::from_fn(|i| {
        !frames[i].failed && frames[i].state.factor.eligible(&systems[i], h)
    });
    if eligible.iter().filter(|p| **p).count() < 2 {
        for i in 0..4 {
            if eligible[i] {
                result[i] = frames[i].state.factor.reused(&systems[i], h);
            }
        }
        return result;
    }
    let mut l = [Lanes::default(); 64];
    let mut x = [Lanes::default(); 8];
    for (i, l) in l.iter_mut().enumerate() {
        *l = Lanes(std::array::from_fn(|lane| {
            if eligible[lane] {
                frames[lane].state.factor.l[i]
            } else if i / 8 == i % 8 {
                1.0
            } else {
                0.0
            }
        }));
    }
    for (i, x) in x.iter_mut().enumerate() {
        *x = Lanes(std::array::from_fn(|lane| {
            if eligible[lane] {
                systems[lane].rhs[i]
            } else {
                0.0
            }
        }));
    }
    substitute::<8>(&l, &mut x);
    let mut residual = Lanes::default();
    let mut scale = Lanes::default();
    for i in 0..8 {
        let mut ax = Lanes::default();
        for (j, x) in x.iter().enumerate() {
            let a = Lanes(std::array::from_fn(|lane| systems[lane].a[i * 8 + j]));
            ax = ax + a * *x;
        }
        let rhs = Lanes(std::array::from_fn(|lane| systems[lane].rhs[i]));
        let diagonal = Lanes(std::array::from_fn(|lane| systems[lane].a[i * 8 + i]));
        residual = residual + (ax - rhs) * (ax - rhs) / diagonal;
        scale = scale + rhs * rhs / diagonal;
    }
    for lane in 0..4 {
        if !eligible[lane] {
            continue;
        }
        let value: Vector = x.iter().map(|x| x.0[lane]).collect();
        if frames[lane].state.factor.accepts(
            &systems[lane],
            &value,
            residual.0[lane],
            scale.0[lane],
        ) {
            result[lane] = Some(value);
        }
    }
    result
}
