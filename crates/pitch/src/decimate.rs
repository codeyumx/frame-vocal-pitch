//! Integer-factor decimation with a windowed-sinc low-pass FIR.

use crate::params::{LOWPASS_CUTOFF_FRACTION, LOWPASS_TAPS_PER_FACTOR};
use std::f64::consts::PI;

/// Low-pass filters and keeps every `factor`-th sample. Only the kept outputs
/// are computed (polyphase-equivalent cost).
pub(crate) struct Decimator {
    factor: usize,
    taps: Vec<f32>,
    /// History duplicated twice so the newest `taps.len()` samples are always
    /// contiguous at `hist[pos..pos + taps.len()]`.
    hist: Vec<f32>,
    pos: usize,
    phase: usize,
}

impl Decimator {
    pub(crate) fn new(factor: usize) -> Self {
        let factor = factor.max(1);
        let taps = if factor == 1 {
            Vec::new()
        } else {
            lowpass_taps(factor)
        };
        let hist = vec![0.0; taps.len() * 2];
        Self {
            factor,
            taps,
            hist,
            pos: 0,
            phase: 0,
        }
    }

    /// Feeds one input sample; returns an output sample every `factor` inputs.
    #[inline]
    pub(crate) fn push(&mut self, x: f32) -> Option<f32> {
        if self.factor == 1 {
            return Some(x);
        }
        let len = self.taps.len();
        self.hist[self.pos] = x;
        self.hist[self.pos + len] = x;
        self.pos += 1;
        if self.pos == len {
            self.pos = 0;
        }
        self.phase += 1;
        if self.phase < self.factor {
            return None;
        }
        self.phase = 0;
        // Oldest → newest; the taps are symmetric so orientation is irrelevant.
        let window = &self.hist[self.pos..self.pos + len];
        Some(window.iter().zip(&self.taps).map(|(a, b)| a * b).sum())
    }
}

/// Blackman-windowed sinc low-pass with unity DC gain, cutoff at
/// `LOWPASS_CUTOFF_FRACTION` of the output rate.
fn lowpass_taps(factor: usize) -> Vec<f32> {
    let n = (LOWPASS_TAPS_PER_FACTOR * factor) | 1; // odd → integer group delay
    let fc = LOWPASS_CUTOFF_FRACTION / factor as f64; // cycles per input sample
    let m = (n - 1) as f64 / 2.0;
    let taps: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * x).sin() / (PI * x)
            };
            let r = i as f64 / (n - 1) as f64;
            let window = 0.42 - 0.5 * (2.0 * PI * r).cos() + 0.08 * (4.0 * PI * r).cos();
            sinc * window
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter().map(|t| (t / sum) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady-state gain of the decimator for a sine at `freq` (48 kHz in).
    fn gain(freq: f64) -> f64 {
        let mut d = Decimator::new(3);
        let mut peak = 0.0f64;
        for i in 0..48_000 {
            let x = (2.0 * PI * freq * i as f64 / 48_000.0).sin() as f32;
            if let Some(y) = d.push(x) {
                if i > 4_800 {
                    peak = peak.max(y.abs() as f64);
                }
            }
        }
        peak
    }

    #[test]
    fn passband_flat_and_stopband_attenuated() {
        for f in [60.0, 500.0, 1000.0, 3000.0] {
            let g = gain(f);
            assert!((g - 1.0).abs() < 0.01, "passband {f} Hz gain {g}");
        }
        // Frequencies that would alias into the analysis band.
        for f in [9_000.0, 12_000.0, 15_000.0, 17_000.0, 23_000.0] {
            let g = gain(f);
            assert!(g < 10f64.powf(-60.0 / 20.0), "stopband {f} Hz gain {g}");
        }
    }

    #[test]
    fn output_rate_is_one_third() {
        let mut d = Decimator::new(3);
        let outputs = (0..4800).filter_map(|_| d.push(0.0)).count();
        assert_eq!(outputs, 1600);
    }
}
