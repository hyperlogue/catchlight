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

pub(super) fn solve(systems: &[System]) -> SmallVec<[Option<Vector>; 4]> {
    let n = systems[0].rhs.len();
    let mut result = smallvec![None; 4];
    let mut a = [Lanes::default(); 64];
    let mut b = [Lanes::default(); 8];
    for (i, a) in a.iter_mut().enumerate().take(n * n) {
        *a = Lanes(std::array::from_fn(|lane| systems[lane].a[i]));
    }
    for (i, b) in b.iter_mut().enumerate().take(n) {
        *b = Lanes(std::array::from_fn(|lane| systems[lane].rhs[i]));
    }
    if n == 2 {
        kernel::<2>(&mut a, &mut b);
    } else {
        kernel::<8>(&mut a, &mut b);
    }
    for lane in 0..4 {
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
        result[lane] = Some(x);
    }
    result
}
