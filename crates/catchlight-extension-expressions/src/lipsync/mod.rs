//! Lip sync: visemes that follow a voice as it is heard, read from its
//! spectrum in overlapping windows of about 40 ms.

mod fft;
mod shape;

use fft::Fft;
use shape::{Heard, SHAPES, Shapes};

use crate::Weights;

/// The standard visemes lip sync plays, in the order of the shapes it hears:
/// open, wide, round, fricative.
pub const LIP_SYNC_VISEMES: [&str; SHAPES] = ["ae", "iy", "uw", "ss"];

/// Power above this is hiss, not voice, in Hz.
const HIGH_HZ: f32 = 3000.0;
/// Mean square below which a window is silence, whatever its shape.
const SILENT: f32 = 1e-8;
/// Share of the power in one place above which the shape is read from that
/// place alone: hiss above 3 kHz, or a pure tone.
const PURE: f32 = 0.98;

/// How fast the smoothed weights move, as first-order time constants in
/// seconds.
mod smoothing {
    /// From a closed mouth, so a vowel onset lands within its own syllable.
    pub(super) const ATTACK: f32 = 0.025;
    /// Toward a closed mouth, so a phrase eases shut.
    pub(super) const RELEASE: f32 = 0.1;
    /// From shape to shape while speaking, so hop-to-hop doubt and short
    /// consonants do not make the mouth buzz.
    pub(super) const BLEND: f32 = 0.06;
    /// Total weight below which the mouth counts as closed.
    pub(super) const REST: f32 = 0.25;
}

/// Follows one mono voice, its samples pushed as they are heard, and gives
/// the weights of [`LIP_SYNC_VISEMES`] that show it. The shape comes from
/// the spectrum alone, so it does not depend on how loud the voice is; how
/// far the mouth opens comes from its voiced loudness against the room's
/// noise floor and the speaker's recent level, so no calibration is needed.
/// Everything runs on the audio's own clock: the result does not depend on
/// how the samples are split into pushes.
///
/// ```no_run
/// # use catchlight_extension_expressions::{LipSync, Weights};
/// # fn frame(sync: &mut LipSync, heard: &[f32], visemes: &mut Weights) {
/// // `sync` is `LipSync::new(sample_rate)`, kept from frame to frame, and
/// // `heard` the mono samples played since the last frame.
/// sync.push(heard);
/// sync.raise_visemes(visemes);
/// # }
/// ```
#[derive(Clone)]
pub struct LipSync {
    sample_rate: u32,
    /// Samples not yet past a hop, with the window's overlap in front.
    pending: Vec<f32>,
    /// Hann.
    window: Vec<f32>,
    hop: usize,
    hop_seconds: f32,
    fft: Fft,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Bin k holds `k * sample_rate / window` Hz, up to half the window.
    power: Vec<f32>,
    shapes: Shapes,
    loudness: Loudness,
    level: f32,
    /// Smoothed, in the order of [`LIP_SYNC_VISEMES`].
    weights: [f32; SHAPES],
}

impl std::fmt::Debug for LipSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LipSync")
            .field("sample_rate", &self.sample_rate)
            .field("window", &self.window.len())
            .field("level", &self.level)
            .field("weights", &self.weights)
            .finish_non_exhaustive()
    }
}

impl LipSync {
    /// A voice sampled at `sample_rate` Hz. The shapes are read below 4 kHz,
    /// so a rate under 8 kHz misreads them.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let rate = sample_rate as f32;
        // About 40 ms, as the shapes were fitted on.
        let size = (sample_rate as usize / 40).next_power_of_two().max(64);
        let hop = size / 2;
        let hop_seconds = hop as f32 / rate;
        let last = (size - 1) as f32;
        Self {
            sample_rate,
            pending: Vec::with_capacity(2 * size),
            window: (0..size)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / last).cos())
                .collect(),
            hop,
            hop_seconds,
            fft: Fft::new(size),
            re: vec![0.0; size],
            im: vec![0.0; size],
            power: vec![0.0; size / 2],
            shapes: Shapes::new(rate, size, hop_seconds),
            loudness: Loudness::default(),
            level: 0.0,
            weights: [0.0; SHAPES],
        }
    }

    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Hears the voice's next `samples`, in -1..1. A sample that is not
    /// finite is heard as silence.
    pub fn push(&mut self, samples: &[f32]) {
        self.pending
            .extend(samples.iter().map(|&s| if s.is_finite() { s } else { 0.0 }));
        let size = self.window.len();
        let mut read = 0;
        while self.pending.len() - read >= size {
            self.hear(read);
            read += self.hop;
        }
        self.pending.drain(..read);
    }

    /// How loud the voice was at the last hop, 0..1: 0 at the noise floor,
    /// 1 at the speaker's recent loud moments.
    #[must_use]
    pub const fn level(&self) -> f32 {
        self.level
    }

    /// Each of [`LIP_SYNC_VISEMES`] with its weight now, 0..1.
    pub fn visemes(&self) -> impl Iterator<Item = (&'static str, f32)> + '_ {
        LIP_SYNC_VISEMES.into_iter().zip(self.weights)
    }

    /// Raises each of [`LIP_SYNC_VISEMES`] in `visemes` to at least its
    /// weight now; one with no weight is left as it was.
    pub fn raise_visemes(&self, visemes: &mut Weights) {
        for (name, weight) in self.visemes() {
            if weight > 0.0 && visemes.get(name).is_none_or(|w| w < weight) {
                visemes.set(name, weight);
            }
        }
    }

    /// Reads the window starting at `from` in `pending` and steps the weights
    /// by one hop.
    fn hear(&mut self, from: usize) {
        let size = self.window.len();
        let samples = &self.pending[from..from + size];
        let mean = samples.iter().sum::<f32>() / size as f32;
        let energy = samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f32>() / size as f32;
        for (((re, im), s), w) in self
            .re
            .iter_mut()
            .zip(&mut self.im)
            .zip(samples)
            .zip(&self.window)
        {
            *re = (s - mean) * w;
            *im = 0.0;
        }
        self.fft.run(&mut self.re, &mut self.im);
        let bin_hz = self.sample_rate as f32 / size as f32;
        let (mut total, mut high, mut peak) = (0.0, 0.0, 1);
        for (bin, (p, (re, im))) in self
            .power
            .iter_mut()
            .zip(self.re.iter().zip(&self.im))
            .enumerate()
        {
            *p = re * re + im * im;
            if bin == 0 {
                continue;
            }
            total += *p;
            if bin as f32 * bin_hz > HIGH_HZ {
                high += *p;
            }
        }
        for bin in 1..self.power.len() {
            if self.power[bin] > self.power[peak] {
                peak = bin;
            }
        }
        let total_or_tiny = f32::max(total, 1e-12);
        let around_peak: f32 = self.power[peak - 1..(peak + 2).min(self.power.len())]
            .iter()
            .sum();

        let heard = Heard {
            power: &self.power,
            energy,
            total,
            high,
        };
        // Read every hop, so the network's history stays continuous.
        let likely = self.shapes.hear(heard);
        let shape = if energy < SILENT {
            None
        } else if high / total_or_tiny > PURE {
            Some(one(3))
        } else if around_peak / total_or_tiny > PURE {
            Some(one(tone_shape(peak as f32 * bin_hz)))
        } else {
            Some(likely)
        };

        // Over the whole band: a fricative's own shape keeps the mouth
        // nearly shut, however loud its hiss.
        self.level = match shape {
            Some(_) => self.loudness.hear(energy, self.hop_seconds),
            None => {
                self.loudness.hear(0.0, self.hop_seconds);
                0.0
            }
        };
        // Concave, so a soft syllable still shows and most of speech sits
        // near full.
        let open = 1.0 - (1.0 - self.level) * (1.0 - self.level);
        let targets = shape.map_or([0.0; SHAPES], |shape| shape.map(|w| w * open));
        self.step(targets);
    }

    fn step(&mut self, targets: [f32; SHAPES]) {
        use smoothing::{ATTACK, BLEND, RELEASE, REST};
        let tau = if targets.iter().sum::<f32>() < REST {
            RELEASE
        } else if self.weights.iter().sum::<f32>() < REST {
            ATTACK
        } else {
            BLEND
        };
        let k = 1.0 - (-self.hop_seconds / tau).exp();
        for (weight, target) in self.weights.iter_mut().zip(targets) {
            *weight += (target - *weight) * k;
        }
    }
}

/// One shape alone.
fn one(shape: usize) -> [f32; SHAPES] {
    std::array::from_fn(|i| if i == shape { 1.0 } else { 0.0 })
}

/// The shape a pure tone at `hz` makes, as its place among the formants
/// suggests: low is round, high is wide, very high is hiss.
fn tone_shape(hz: f32) -> usize {
    if hz > HIGH_HZ {
        3
    } else if hz > 1800.0 {
        1
    } else if hz < 800.0 {
        2
    } else {
        0
    }
}

/// Loudness against the room and the speaker. The floor sinks to quiet
/// moments at once and creeps up through steady noise; the loud mark jumps to
/// loud moments and sinks slowly, so a quiet speaker opens the mouth as far
/// as a loud one after a few seconds. Both are in dB of mean square.
#[derive(Debug, Clone, Copy)]
struct Loudness {
    floor: f32,
    loud: f32,
}

/// Seconds for the floor to sink most of the way to a quieter moment.
const FLOOR_SINK_SECONDS: f32 = 0.1;
const FLOOR_RISE_DB_PER_SECOND: f32 = 4.0;
const LOUD_SINK_DB_PER_SECOND: f32 = 4.0;
/// Above the floor before the mouth starts to open.
const GATE_DB: f32 = 6.0;
/// Below the loud mark, ordinary voiced speech, which opens the mouth fully.
const FULL_BELOW_LOUD_DB: f32 = 6.0;
/// The loud mark never sits closer to the floor than this.
const RANGE_DB: f32 = 24.0;
const QUIETEST_DB: f32 = -100.0;

impl Default for Loudness {
    /// A quiet room, and ordinary speech as loud.
    fn default() -> Self {
        Self {
            floor: -70.0,
            loud: -26.0,
        }
    }
}

impl Loudness {
    /// Hears `energy`, a mean square, `seconds` after the last; returns 0..1,
    /// linear in energy between the gate and full.
    fn hear(&mut self, energy: f32, seconds: f32) -> f32 {
        let db = (10.0 * energy.max(1e-12).log10()).max(QUIETEST_DB);
        self.floor = if db < self.floor {
            self.floor + (db - self.floor) * (1.0 - (-seconds / FLOOR_SINK_SECONDS).exp())
        } else {
            (self.floor + FLOOR_RISE_DB_PER_SECOND * seconds).min(db)
        };
        self.loud = (self.loud - LOUD_SINK_DB_PER_SECOND * seconds)
            .max(db)
            .max(self.floor + RANGE_DB);
        let power = |db: f32| 10f32.powf(db / 10.0);
        let gate = power(self.floor + GATE_DB);
        let full = power(self.loud - FULL_BELOW_LOUD_DB);
        ((energy - gate) / (full - gate)).clamp(0.0, 1.0)
    }
}
