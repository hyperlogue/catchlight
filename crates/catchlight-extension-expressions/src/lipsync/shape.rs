//! Which mouth shape a moment of voice makes: open, wide, round, or a
//! fricative's. A small network fitted to English and Mandarin speech aligned
//! with its visemes reads the spectral envelope of the last few hops.

use std::sync::LazyLock;

pub(super) const SHAPES: usize = 4;
const BANDS: usize = 32;
/// The band log powers, then the share of power above 3 kHz, then loudness
/// against the recent peak.
const FEATURES: usize = BANDS + 2;
/// Hops the network reads, newest first.
const CONTEXT: usize = 4;
const HIDDEN: usize = 48;
const INPUTS: usize = FEATURES * CONTEXT;
/// The hop the network was fitted at: 512 samples at 24 kHz.
const FITTED_HOP_SECONDS: f32 = 512.0 / 24_000.0;
/// Per fitted hop, how much of the loudness peak is kept.
const PEAK_KEPT: f32 = 0.9;
/// A shape this certain plays alone; below it, all four mix.
const CERTAIN: f32 = 0.9;

/// Little-endian f32s: the band edges in Hz, the input weights by hidden
/// unit, the hidden biases, the output weights by shape, the output biases.
/// Input normalization is folded into the first layer.
const DATA: &[u8; 4 * (BANDS + 2 + HIDDEN * INPUTS + HIDDEN + SHAPES * HIDDEN + SHAPES)] =
    include_bytes!("shapes.bin");

struct Network {
    /// Mel-spaced; band `b` is a triangle over edges `b..=b + 2`.
    edges: [f32; BANDS + 2],
    input: Vec<[f32; INPUTS]>,
    hidden_bias: [f32; HIDDEN],
    output: [[f32; HIDDEN]; SHAPES],
    output_bias: [f32; SHAPES],
}

static NETWORK: LazyLock<Network> = LazyLock::new(|| {
    let mut at = 0;
    let mut next = || {
        let i = 4 * at;
        at += 1;
        f32::from_le_bytes([DATA[i], DATA[i + 1], DATA[i + 2], DATA[i + 3]])
    };
    let edges = std::array::from_fn(|_| next());
    let input = (0..HIDDEN)
        .map(|_| std::array::from_fn(|_| next()))
        .collect();
    let hidden_bias = std::array::from_fn(|_| next());
    let output = std::array::from_fn(|_| std::array::from_fn(|_| next()));
    let output_bias = std::array::from_fn(|_| next());
    Network {
        edges,
        input,
        hidden_bias,
        output,
        output_bias,
    }
});

/// One spectrum's measures, as [`Shapes::hear`] reads them.
#[derive(Debug, Clone, Copy)]
pub(super) struct Heard<'a> {
    /// Power by bin, from bin 0 to half the window.
    pub power: &'a [f32],
    /// Mean square of the window's samples.
    pub energy: f32,
    /// Power summed over every bin but 0.
    pub total: f32,
    /// Power above 3 kHz.
    pub high: f32,
}

#[derive(Debug, Clone)]
pub(super) struct Shapes {
    /// Each band's bins with their weights in its triangle.
    bands: Vec<Vec<(usize, f32)>>,
    /// Newest first; filled with the first hop's features when it comes.
    history: Option<[[f32; FEATURES]; CONTEXT]>,
    peak_energy: f32,
    /// Per hop of this rate.
    peak_kept: f32,
}

impl Shapes {
    pub(super) fn new(sample_rate: f32, window: usize, hop_seconds: f32) -> Self {
        let edges = NETWORK.edges;
        let bands = (0..BANDS)
            .map(|b| {
                let [low, mid, high] = [edges[b], edges[b + 1], edges[b + 2]];
                (1..window / 2)
                    .filter_map(|bin| {
                        let hz = bin as f32 * sample_rate / window as f32;
                        let weight = ((hz - low) / (mid - low)).min((high - hz) / (high - mid));
                        (weight > 0.0).then_some((bin, weight))
                    })
                    .collect()
            })
            .collect();
        Self {
            bands,
            history: None,
            peak_energy: 0.0,
            peak_kept: PEAK_KEPT.powf(hop_seconds / FITTED_HOP_SECONDS),
        }
    }

    /// How likely each shape is, open, wide, round and fricative, summing to
    /// 1: one shape alone when the network is sure of it.
    pub(super) fn hear(&mut self, heard: Heard<'_>) -> [f32; SHAPES] {
        let mut features = [0.0; FEATURES];
        for (feature, band) in features.iter_mut().zip(&self.bands) {
            let power: f32 = band.iter().map(|&(bin, w)| heard.power[bin] * w).sum();
            *feature = power.max(1e-12).ln();
        }
        // Against their mean, so the gain of the voice drops out.
        let mean = features[..BANDS].iter().sum::<f32>() / BANDS as f32;
        for feature in &mut features[..BANDS] {
            *feature -= mean;
        }
        self.peak_energy = heard.energy.max(self.peak_energy * self.peak_kept);
        features[BANDS] = heard.high / heard.total.max(1e-12);
        features[BANDS + 1] = (heard.energy / self.peak_energy.max(1e-12)).sqrt();

        let history = self.history.get_or_insert([features; CONTEXT]);
        history.rotate_right(1);
        history[0] = features;

        let network = &*NETWORK;
        let hidden: [f32; HIDDEN] = std::array::from_fn(|unit| {
            let sum: f32 = network.input[unit]
                .iter()
                .zip(history.iter().flatten())
                .map(|(w, x)| w * x)
                .sum();
            (network.hidden_bias[unit] + sum).tanh()
        });
        let scores: [f32; SHAPES] = std::array::from_fn(|shape| {
            let sum: f32 = network.output[shape]
                .iter()
                .zip(&hidden)
                .map(|(w, x)| w * x)
                .sum();
            network.output_bias[shape] + sum
        });
        mixture(scores)
    }
}

/// The softmax of `scores`, or the best shape alone when it holds
/// [`CERTAIN`] of it.
fn mixture(scores: [f32; SHAPES]) -> [f32; SHAPES] {
    let best = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut weights = scores.map(|s| (s - best).exp());
    let total: f32 = weights.iter().sum();
    for w in &mut weights {
        *w /= total;
    }
    match weights.iter().position(|&w| w >= CERTAIN) {
        Some(sure) => std::array::from_fn(|i| if i == sure { 1.0 } else { 0.0 }),
        None => weights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_is_finite_and_its_bands_rise() {
        let network = &*NETWORK;
        assert!(network.edges.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(network.edges[0] > 0.0 && network.edges[BANDS + 1] < 4000.0);
        assert!(
            network
                .input
                .iter()
                .flatten()
                .chain(&network.hidden_bias)
                .chain(network.output.iter().flatten())
                .chain(&network.output_bias)
                .all(|w| w.is_finite())
        );
    }

    #[test]
    fn an_unsure_shape_mixes_and_a_sure_one_plays_alone() {
        let mixed = mixture([0.0, 1.1, 1.0, -1.0]);
        assert!(mixed[1] > mixed[2] && mixed[2] > 0.25);
        assert!((mixed.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(mixture([0.0, 5.0, 0.0, 0.0]), [0.0, 1.0, 0.0, 0.0]);
    }
}
