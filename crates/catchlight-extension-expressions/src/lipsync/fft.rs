//! An iterative radix-2 FFT for one power-of-two length, planned once.

#[derive(Debug, Clone)]
pub(super) struct Fft {
    /// `e^(-2πik/n)` for k below n/2, as (cos, sin).
    twiddles: Vec<(f32, f32)>,
    /// Where each index lands after bit reversal.
    reversed: Vec<usize>,
}

impl Fft {
    /// `n` is a power of two, at least 2.
    pub(super) fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let reversed = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let twiddles = (0..n / 2)
            .map(|k| {
                let angle = -std::f32::consts::TAU * k as f32 / n as f32;
                (angle.cos(), angle.sin())
            })
            .collect();
        Self { twiddles, reversed }
    }

    /// Transforms `re + i·im` in place; both are the planned length.
    pub(super) fn run(&self, re: &mut [f32], im: &mut [f32]) {
        for (i, &j) in self.reversed.iter().enumerate() {
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let n = re.len();
        let mut half = 1;
        while half < n {
            let stride = n / (2 * half);
            for start in (0..n).step_by(2 * half) {
                for k in 0..half {
                    let (cos, sin) = self.twiddles[k * stride];
                    let (a, b) = (start + k, start + k + half);
                    let br = re[b] * cos - im[b] * sin;
                    let bi = re[b] * sin + im[b] * cos;
                    re[b] = re[a] - br;
                    im[b] = im[a] - bi;
                    re[a] += br;
                    im[a] += bi;
                }
            }
            half *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Fft;

    #[test]
    fn matches_the_direct_transform() {
        let n = 64;
        let signal: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.37).sin() + 0.5 * (i as f32 * 1.9).cos())
            .collect();
        let (mut re, mut im) = (signal.clone(), vec![0.0; n]);
        Fft::new(n).run(&mut re, &mut im);
        for k in 0..n {
            let (mut dr, mut di) = (0.0f32, 0.0f32);
            for (t, x) in signal.iter().enumerate() {
                let angle = -std::f32::consts::TAU * (k * t) as f32 / n as f32;
                dr += x * angle.cos();
                di += x * angle.sin();
            }
            assert!(
                (re[k] - dr).abs() < 1e-3 && (im[k] - di).abs() < 1e-3,
                "bin {k}"
            );
        }
    }
}
