//! Offline helpers behind the `--analyze` and `--bench` CLI modes.

use crate::params::{PitchParams, INPUT_RATE_HZ, RESONANCE_WINDOW_S};
use crate::resonance::{Calibration, ResonanceTracker};
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

/// Measures detector + tracker + resonance cost (SPEC §5.1 CPU line) and
/// returns the mean wall time per 10 ms hop in microseconds. The signal is a
/// cycle of synthetic vowels /a/ /i/ /u/ at 150 Hz, so every hop is voiced
/// below 300 Hz and runs the full formant analysis (worst case), and a
/// calibration recorder is fed alongside as during a recording.
pub fn bench_resonance() -> f64 {
    let signal = synth::vowel_cycle(INPUT_RATE_HZ, BENCH_SECONDS, 150.0);
    // Warm-up pass (page faults, caches, CPU clock ramp).
    run_resonance_pipeline(&signal[..signal.len() / 4]);
    let start = Instant::now();
    let count = run_resonance_pipeline(&signal);
    start.elapsed().as_secs_f64() * 1e6 / count.max(1) as f64
}

fn run_resonance_pipeline(signal: &[f32]) -> usize {
    let mut detector = Detector::new(PitchParams::default(), INPUT_RATE_HZ);
    let mut tracker = Tracker::new(165.0, 255.0);
    let mut resonance = ResonanceTracker::new(RESONANCE_WINDOW_S);
    let mut calibration = Calibration::new();
    let mut count = 0;
    for chunk in signal.chunks(BENCH_CHUNK) {
        detector.push_with(chunk, |hop, frame| {
            std::hint::black_box(tracker.update(&hop));
            calibration.push(resonance.update(&hop, frame));
            std::hint::black_box(resonance.state(Some(1000.0), Some(1200.0)));
            count += 1;
        });
    }
    std::hint::black_box(calibration.finish().ok());
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

    /// Formant frequencies (Hz) of the test vowels /a/, /i/, /u/ (SPEC §5.1).
    pub(crate) const VOWELS: [[f64; 3]; 3] = [
        [700.0, 1200.0, 2600.0],
        [300.0, 2300.0, 3000.0],
        [300.0, 800.0, 2300.0],
    ];
    /// Resonator bandwidths (Hz) for F1, F2, F3.
    pub(crate) const BANDWIDTHS: [f64; 3] = [80.0, 100.0, 140.0];

    /// Rosenberg glottal flow pulse over one period (`p` in cycles):
    /// raised-cosine opening over 40 %, quarter-cosine closing over 16 %,
    /// closed for the rest.
    fn glottal_flow(p: f64) -> f64 {
        const OPEN: f64 = 0.40;
        const CLOSE: f64 = 0.16;
        if p < OPEN {
            0.5 * (1.0 - (std::f64::consts::PI * p / OPEN).cos())
        } else if p < OPEN + CLOSE {
            (std::f64::consts::FRAC_PI_2 * (p - OPEN) / CLOSE).cos()
        } else {
            0.0
        }
    }

    /// Source–filter vowel: glottal pulse train at `f0`, lip radiation (first
    /// difference), then three cascaded second-order resonators (unity DC
    /// gain) at `formants` with [`BANDWIDTHS`]. Peak-normalised to 0.5.
    pub(crate) fn vowel(rate: u32, seconds: f64, f0: f64, formants: [f64; 3]) -> Vec<f32> {
        let fs = rate as f64;
        let mut v: Vec<f64> = Vec::new();
        let mut prev = 0.0;
        for g in tone(rate, seconds, |_| f0, glottal_flow) {
            let g = g as f64;
            v.push(g - prev);
            prev = g;
        }
        for (f, bw) in formants.iter().zip(BANDWIDTHS) {
            let r = (-std::f64::consts::PI * bw / fs).exp();
            let a1 = 2.0 * r * (TAU * f / fs).cos();
            let a2 = -r * r;
            let gain = 1.0 - a1 - a2;
            let (mut y1, mut y2) = (0.0, 0.0);
            for x in &mut v {
                let y = gain * *x + a1 * y1 + a2 * y2;
                y2 = y1;
                y1 = y;
                *x = y;
            }
        }
        let peak = v.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        let scale = if peak > 0.0 { 0.5 / peak } else { 0.0 };
        v.iter().map(|x| (x * scale) as f32).collect()
    }

    /// `seconds` of /a/ /i/ /u/ in turn, one second each, at `f0`.
    pub(crate) fn vowel_cycle(rate: u32, seconds: usize, f0: f64) -> Vec<f32> {
        (0..seconds)
            .flat_map(|i| vowel(rate, 1.0, f0, VOWELS[i % VOWELS.len()]))
            .collect()
    }
}
