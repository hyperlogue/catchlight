use catchlight_extension_expressions::{LIP_SYNC_VISEMES, LipSync, Weights};

const RATES: [u32; 5] = [8000, 16_000, 24_000, 44_100, 48_000];

fn tone(rate: u32, hz: f32, amplitude: f32, seconds: f32) -> Vec<f32> {
    (0..(seconds * rate as f32) as usize)
        .map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin())
        .collect()
}

/// Partials on 200 Hz falling in level, as a voiced vowel's would.
fn chord(rate: u32, gain: f32, dc: f32, seconds: f32) -> Vec<f32> {
    (0..(seconds * rate as f32) as usize)
        .map(|i| {
            let t = i as f32 / rate as f32;
            let partial = |hz: f32, level: f32| level * (std::f32::consts::TAU * hz * t).sin();
            gain * (partial(200.0, 1.0)
                + partial(600.0, 0.8)
                + partial(1200.0, 0.6)
                + partial(2400.0, 0.4))
                + dc
        })
        .collect()
}

fn heard(rate: u32, voice: &[f32]) -> LipSync {
    let mut sync = LipSync::new(rate);
    sync.push(voice);
    sync
}

fn weights(sync: &LipSync) -> Vec<f32> {
    sync.visemes().map(|(_, w)| w).collect()
}

/// The viseme holding the most weight.
fn strongest(sync: &LipSync) -> &'static str {
    sync.visemes()
        .fold(
            ("", 0.0),
            |best, (name, w)| if w > best.1 { (name, w) } else { best },
        )
        .0
}

#[test]
fn a_low_tone_is_round_a_high_one_wide_and_hiss_a_fricative() {
    for rate in RATES {
        let mut sync = LipSync::new(rate);
        sync.push(&tone(rate, 300.0, 0.3, 0.4));
        assert_eq!(strongest(&sync), "uw", "{rate} Hz");
        sync.push(&tone(rate, 2400.0, 0.3, 0.4));
        assert_eq!(strongest(&sync), "iy", "{rate} Hz");
        if rate >= 16_000 {
            sync.push(&tone(rate, 3600.0, 0.3, 0.4));
            assert_eq!(strongest(&sync), "ss", "{rate} Hz");
        }
    }
}

#[test]
fn silence_closes_the_mouth_and_a_steady_offset_is_not_speech() {
    for rate in RATES {
        let mut sync = LipSync::new(rate);
        sync.push(&chord(rate, 0.1, 0.0, 0.5));
        assert!(weights(&sync).iter().sum::<f32>() > 0.8, "{rate} Hz");
        sync.push(&vec![0.0; rate as usize]);
        assert!(weights(&sync).iter().all(|&w| w < 0.01), "{rate} Hz");
        assert_eq!(sync.level(), 0.0);
        sync.push(&vec![0.25; rate as usize]);
        assert!(weights(&sync).iter().all(|&w| w < 0.01), "{rate} Hz");
    }
}

#[test]
fn the_result_does_not_depend_on_how_the_samples_are_pushed() {
    for rate in RATES {
        let mut voice = chord(rate, 0.1, 0.0, 0.4);
        voice.extend(tone(rate, 300.0, 0.2, 0.3));
        voice.extend(vec![0.0; rate as usize / 5]);
        let whole = heard(rate, &voice);
        for chunk in [1, 127, 512, 4096] {
            let mut sync = LipSync::new(rate);
            for part in voice.chunks(chunk) {
                sync.push(part);
            }
            assert_eq!(weights(&sync), weights(&whole), "{rate} Hz in {chunk}s");
            assert_eq!(sync.level(), whole.level());
        }
    }
}

#[test]
fn the_shape_survives_gain_and_offset() {
    for rate in RATES {
        let shape = |voice: &[f32]| {
            let sync = heard(rate, voice);
            let w = weights(&sync);
            let total: f32 = w.iter().sum();
            w.into_iter().map(|x| x / total).collect::<Vec<_>>()
        };
        let reference = shape(&chord(rate, 0.1, 0.0, 0.5));
        for other in [chord(rate, 0.3, 0.0, 0.5), chord(rate, 0.1, 0.1, 0.5)] {
            let other = shape(&other);
            assert!(
                reference
                    .iter()
                    .zip(&other)
                    .all(|(a, b)| (a - b).abs() < 0.05),
                "{rate} Hz: {reference:?} {other:?}"
            );
        }
    }
}

/// Bursts of `amplitude` for 0.2 s with 0.1 s gaps, over `seconds`.
fn syllables(rate: u32, amplitude: f32, seconds: f32) -> Vec<f32> {
    let mut voice = Vec::new();
    while (voice.len() as f32) < seconds * rate as f32 {
        voice.extend(chord(rate, amplitude, 0.0, 0.2));
        voice.extend(vec![0.0; rate as usize / 10]);
    }
    voice
}

#[test]
fn a_quiet_voice_opens_the_mouth_as_far_as_a_loud_one_within_seconds() {
    let rate = 48_000;
    let mouth = |amplitude: f32, seconds: f32| {
        let mut voice = syllables(rate, amplitude, seconds);
        voice.extend(chord(rate, amplitude, 0.0, 0.15));
        weights(&heard(rate, &voice)).iter().sum::<f32>()
    };
    let loud = mouth(0.2, 0.0);
    assert!(loud > 0.9, "{loud}");
    assert!(mouth(0.004, 0.0) < 0.5 * loud, "quiet at first");
    let adapted = mouth(0.004, 6.0);
    assert!((adapted - loud).abs() < 0.1, "{adapted} {loud}");
}

#[test]
fn steady_noise_becomes_the_floor() {
    let rate = 48_000;
    // A hum under the speech band's shapes: low, and steady.
    let mut sync = heard(rate, &tone(rate, 150.0, 0.01, 20.0));
    assert!(sync.level() < 0.05, "{}", sync.level());
    sync.push(&chord(rate, 0.2, 0.0, 0.2));
    assert!(sync.level() > 0.9, "{}", sync.level());
}

#[test]
fn raising_keeps_a_higher_weight_and_adds_none_for_a_closed_mouth() {
    let rate = 48_000;
    let closed = LipSync::new(rate);
    let mut visemes = Weights::new();
    closed.raise_visemes(&mut visemes);
    assert!(visemes.is_empty());

    let sync = heard(rate, &tone(rate, 300.0, 0.3, 0.4));
    let round = sync.visemes().find(|(n, _)| *n == "uw").unwrap().1;
    let mut visemes: Weights = [("uw", 1.0), ("pp", 0.5)].into_iter().collect();
    sync.raise_visemes(&mut visemes);
    assert_eq!(visemes.get("uw"), Some(1.0));
    assert_eq!(visemes.get("pp"), Some(0.5));

    let mut visemes = Weights::new();
    sync.raise_visemes(&mut visemes);
    assert_eq!(visemes.get("uw"), Some(round));
    assert!(visemes.iter().all(|(n, _)| LIP_SYNC_VISEMES.contains(&n)));
}

#[test]
fn samples_that_are_not_finite_are_heard_as_silence() {
    let sync = heard(48_000, &[f32::NAN; 9600]);
    assert!(weights(&sync).iter().all(|&w| w == 0.0));
}
