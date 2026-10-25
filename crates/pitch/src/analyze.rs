//! Offline helpers behind the `--analyze` and `--bench` CLI modes.

use crate::params::{PitchParams, INPUT_RATE_HZ};
use crate::tracker::{Class, Tracker};
use crate::yin::{Detector, Hop};
use std::time::Instant;

/// Runs the full pipeline (detector + tracker) over a mono signal and returns
/// `(time s, smoothed F0, class)` per 10 ms hop.
pub fn analyze(
    samples: &[f32],
    rate_hz: u32,
    params: PitchParams,
    band: (f32, f32),
) -> Vec<(f64, Option<f32>, Class)> {
    let mut detector = Detector::new(params, rate_hz);
    let mut tracker = Tracker::new(band.0, band.1);
    let mut hops = Vec::with_capacity(samples.len() / (rate_hz.max(1) as usize / 100).max(1) + 1);
    detector.push(samples, &mut hops);
    hops.iter()
        .map(|hop| {
            let s = tracker.update(hop);
            (s.t_s, s.f0_hz, s.class)
        })
        .collect()
}

/// Seconds of synthetic audio processed by [`bench`].
const BENCH_SECONDS: usize = 20;
/// Chunk size fed per call, like a PipeWire quantum.
const BENCH_CHUNK: usize = 1024;

/// Measures detector + tracker cost on synthetic voiced audio (a harmonic
/// tone gliding 80 → 400 Hz with noise, so YIN runs on every hop) and returns
/// the mean wall time per 10 ms hop in microseconds. The share of one core is
/// `µs/hop / 10 000 × 100 %`.
pub fn bench() -> f64 {
    let signal = synth::bench_signal(INPUT_RATE_HZ, BENCH_SECONDS);
    let mut hops: Vec<Hop> = Vec::with_capacity(BENCH_CHUNK);
    // Warm-up pass (page faults, caches, CPU clock ramp).
    run_pipeline(&signal[..signal.len() / 4], &mut hops);
    let start = Instant::now();
    let count = run_pipeline(&signal, &mut hops);
    start.elapsed().as_secs_f64() * 1e6 / count.max(1) as f64
}

fn run_pipeline(signal: &[f32], hops: &mut Vec<Hop>) -> usize {
    let mut detector = Detector::new(PitchParams::default(), INPUT_RATE_HZ);
    let mut tracker = Tracker::new(165.0, 255.0);
    let mut count = 0;
    for chunk in signal.chunks(BENCH_CHUNK) {
        hops.clear();
        detector.push(chunk, hops);
        for hop in hops.iter() {
            std::hint::black_box(tracker.update(hop));
        }
        count += hops.len();
    }
    count
}

/// Deterministic synthetic signals (bench and tests).
pub(crate) mod synth {
    use std::f64::consts::TAU;

    /// xorshift64* — tiny deterministic PRNG, uniform in [-1, 1).
    pub(crate) struct Noise(u64);

    impl Noise {
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        pub(crate) fn next(&mut self) -> f32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40; // 24 bits
            (v as f32 / (1u64 << 23) as f32) - 1.0
        }
    }

    /// Generates `seconds` of a signal whose instantaneous frequency is
    /// `freq(t)`, shaped by `wave(phase in cycles)`.
    pub(crate) fn tone(
        rate: u32,
        seconds: f64,
        freq: impl Fn(f64) -> f64,
        wave: impl Fn(f64) -> f64,
    ) -> Vec<f32> {
        let n = (rate as f64 * seconds).round() as usize;
        let dt = 1.0 / rate as f64;
        let mut phase = 0.0f64;
        (0..n)
            .map(|i| {
                let y = wave(phase) as f32;
                phase = (phase + freq(i as f64 * dt) * dt).fract();
                y
            })
            .collect()
    }

    pub(crate) fn sine(p: f64) -> f64 {
        (TAU * p).sin()
    }

    /// Naive (non-band-limited) sawtooth in [-1, 1).
    pub(crate) fn saw(p: f64) -> f64 {
        2.0 * p - 1.0
    }

    pub(crate) fn bench_signal(rate: u32, seconds: usize) -> Vec<f32> {
        let half = seconds as f64 / 2.0;
        let mut noise = Noise::new(0x5EED);
        let mut v = tone(
            rate,
            seconds as f64,
            // Up and down glide 80 → 400 → 80 Hz.
            |t| 80.0 + 320.0 * (1.0 - ((t - half) / half).abs()),
            |p| 0.3 * saw(p) + 0.2 * sine(p),
        );
        for x in &mut v {
            *x += 0.01 * noise.next();
        }
        v
    }
}
