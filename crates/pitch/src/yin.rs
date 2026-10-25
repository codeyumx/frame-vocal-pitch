//! YIN fundamental-frequency estimator (de Cheveigné & Kawahara, JASA 111(4),
//! 2002) with an FFT-based difference function and adaptive voicing.

use crate::decimate::Decimator;
use crate::params::{
    dbfs_to_linear, PitchParams, ANALYSIS_RATE_HZ, HOP_S, NOISE_EMA_FALL_S, NOISE_EMA_RISE_S,
    RESONANCE_FRAME_S, WINDOW_PERIODS,
};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Result of one 10 ms analysis hop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hop {
    /// Stream time of the newest sample in the analysis frame (seconds since
    /// the detector was created).
    pub t_s: f64,
    /// Refined F0 estimate; `Some` only when the frame is voiced.
    pub f0_hz: Option<f32>,
    /// YIN dip: CMNDF value at the selected lag (0 = perfectly periodic).
    /// 1.0 when the frame was below the absolute noise-floor clamp and YIN
    /// was skipped.
    pub dip: f32,
    /// RMS of the analysis frame (linear, full scale = 1.0).
    pub rms: f32,
}

/// The newest analysis samples behind one [`Hop`] (SPEC §5.1 resonance input):
/// 25 ms of the decimated signal ending at `Hop::t_s`. Borrowed from the
/// detector's history, so nothing is copied or allocated.
#[derive(Clone, Copy, Debug)]
pub struct AnalysisFrame<'a> {
    /// Oldest sample first.
    pub samples: &'a [f32],
    /// Sample rate of `samples` (16 kHz for 48 kHz input).
    pub rate_hz: f32,
}

/// Streaming pitch detector. Feed arbitrary-sized chunks of mono samples; one
/// [`Hop`] is produced per 10 ms of input.
pub struct Detector {
    params: PitchParams,
    input_rate_hz: u32,
    decimator: Decimator,
    /// Analysis sample rate (input rate / decimation factor).
    rate: f64,
    hop_len: usize,
    hop_buf: Vec<f32>,
    hop_fill: usize,
    /// Most recent analysis samples, oldest first: the longer of the YIN frame
    /// and the 25 ms resonance frame.
    frame: Vec<f32>,
    /// Length of the resonance frame at the tail of `frame`.
    resonance_len: usize,
    /// Analysis samples consumed so far (drives `Hop::t_s`).
    consumed: u64,
    yin: Yin,
    noise_ema: f32,
    floor_min: f32,
    alpha_rise: f32,
    alpha_fall: f32,
}

impl Detector {
    /// Creates a detector for `input_rate_hz` mono input. Rates near a
    /// multiple of 16 kHz are decimated by that integer factor (48 kHz → 3:1).
    /// Parameters are sanitised first. All buffers are allocated here.
    pub fn new(params: PitchParams, input_rate_hz: u32) -> Self {
        let params = params.sanitized();
        let input_rate_hz = input_rate_hz.max(1);
        let factor = ((input_rate_hz as f64 / ANALYSIS_RATE_HZ as f64).round() as usize).max(1);
        let rate = input_rate_hz as f64 / factor as f64;
        let hop_len = ((rate * HOP_S).round() as usize).max(1);
        let yin = Yin::new(rate, &params);
        let resonance_len = ((rate * RESONANCE_FRAME_S).round() as usize).max(1);
        let floor_min = dbfs_to_linear(params.noise_floor_min_dbfs);
        let alpha = |tau_s: f64| (1.0 - (-HOP_S / tau_s).exp()) as f32;
        Self {
            params,
            input_rate_hz,
            decimator: Decimator::new(factor),
            rate,
            hop_len,
            hop_buf: vec![0.0; hop_len],
            hop_fill: 0,
            frame: vec![0.0; yin.frame_len.max(resonance_len)],
            resonance_len,
            consumed: 0,
            noise_ema: floor_min / params.noise_floor_factor,
            floor_min,
            alpha_rise: alpha(NOISE_EMA_RISE_S),
            alpha_fall: alpha(NOISE_EMA_FALL_S),
            yin,
        }
    }

    /// Replaces the parameters (rebuilds internal buffers; allocates). The
    /// stream clock keeps running so `Hop::t_s` stays monotonic.
    pub fn set_params(&mut self, params: PitchParams) {
        let consumed_s = self.consumed as f64 / self.rate;
        *self = Self::new(params, self.input_rate_hz);
        self.consumed = (consumed_s * self.rate).round() as u64;
    }

    /// Current (sanitised) parameters.
    pub fn params(&self) -> &PitchParams {
        &self.params
    }

    /// Input sample rate given at construction.
    pub fn input_rate_hz(&self) -> u32 {
        self.input_rate_hz
    }

    /// Analysis sample rate after decimation (16 kHz for 48 kHz input).
    pub fn analysis_rate_hz(&self) -> f32 {
        self.rate as f32
    }

    /// Current voicing RMS threshold (linear): `max(EMA × factor, clamp)`.
    pub fn noise_floor(&self) -> f32 {
        (self.noise_ema * self.params.noise_floor_factor).max(self.floor_min)
    }

    /// Consumes input samples and appends one [`Hop`] to `out` per completed
    /// hop. Non-finite samples are treated as 0. Does not allocate unless
    /// `out` has to grow.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Hop>) {
        self.push_with(samples, |hop, _| out.push(hop));
    }

    /// Like [`Detector::push`], but hands every completed hop to `on_hop`
    /// together with its [`AnalysisFrame`]. Never allocates.
    pub fn push_with(&mut self, samples: &[f32], mut on_hop: impl FnMut(Hop, AnalysisFrame<'_>)) {
        for &x in samples {
            let x = if x.is_finite() { x } else { 0.0 };
            let Some(y) = self.decimator.push(x) else {
                continue;
            };
            self.hop_buf[self.hop_fill] = y;
            self.hop_fill += 1;
            if self.hop_fill == self.hop_len {
                self.hop_fill = 0;
                let hop = self.process_hop();
                let n = self.frame.len();
                on_hop(
                    hop,
                    AnalysisFrame {
                        samples: &self.frame[n - self.resonance_len..],
                        rate_hz: self.rate as f32,
                    },
                );
            }
        }
    }

    fn process_hop(&mut self) -> Hop {
        let hop = self.hop_len;
        self.frame.copy_within(hop.., 0);
        let n = self.frame.len();
        self.frame[n - hop..].copy_from_slice(&self.hop_buf);
        self.consumed += hop as u64;
        let t_s = self.consumed as f64 / self.rate;

        let yin_frame = &self.frame[n - self.yin.frame_len..];
        let rms = self.yin.load_energy(yin_frame);
        let floor = self.noise_floor();
        let (lag, dip) = if rms > self.floor_min {
            self.yin.estimate(yin_frame, self.params.yin_threshold)
        } else {
            (None, 1.0)
        };
        let periodic = lag.is_some() && dip <= self.params.voicing_max_dip;
        let voiced = periodic && rms > floor;
        // Frames that YIN judged periodic never feed the noise estimate, so a
        // floor that once rose above the voice cannot lock itself in place.
        if !periodic {
            let alpha = if rms > self.noise_ema {
                self.alpha_rise
            } else {
                self.alpha_fall
            };
            self.noise_ema += alpha * (rms - self.noise_ema);
        }
        Hop {
            t_s,
            f0_hz: if voiced {
                lag.map(|period| (self.rate / period) as f32)
            } else {
                None
            },
            dip,
            rms,
        }
    }
}

/// YIN working state; all buffers sized once from the detection span.
struct Yin {
    /// Integration window W (samples).
    window: usize,
    tau_min: usize,
    tau_max: usize,
    /// `window + tau_max + 2` (one extra lag for parabolic interpolation).
    frame_len: usize,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    in_window: Vec<f32>,
    in_frame: Vec<f32>,
    spec_window: Vec<Complex<f32>>,
    spec_frame: Vec<Complex<f32>>,
    corr: Vec<f32>,
    scratch_fwd: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,
    /// Prefix sums of squared samples: `energy[i] = Σ_{j<i} x_j²`.
    energy: Vec<f64>,
    /// Difference function d(τ), τ = 0..=tau_max+1.
    diff: Vec<f64>,
    /// Cumulative-mean-normalised difference d'(τ).
    cmnd: Vec<f32>,
}

impl Yin {
    fn new(rate: f64, params: &PitchParams) -> Self {
        let tau_min = ((rate / params.detect_max_hz as f64).floor() as usize).max(2);
        let tau_max = ((rate / params.detect_min_hz as f64).ceil() as usize).max(tau_min + 1);
        let window = (WINDOW_PERIODS * rate / params.detect_min_hz as f64).ceil() as usize;
        let frame_len = window + tau_max + 2;
        // Circular cross-correlation of the window against the frame equals the
        // linear one for lags 0..=tau_max as long as fft_len ≥ frame_len.
        let fft_len = frame_len.next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let c2r = planner.plan_fft_inverse(fft_len);
        Self {
            window,
            tau_min,
            tau_max,
            frame_len,
            in_window: r2c.make_input_vec(),
            in_frame: r2c.make_input_vec(),
            spec_window: r2c.make_output_vec(),
            spec_frame: r2c.make_output_vec(),
            corr: c2r.make_output_vec(),
            scratch_fwd: r2c.make_scratch_vec(),
            scratch_inv: c2r.make_scratch_vec(),
            r2c,
            c2r,
            energy: vec![0.0; frame_len + 1],
            diff: vec![0.0; tau_max + 2],
            cmnd: vec![1.0; tau_max + 2],
        }
    }

    /// Fills the energy prefix sums for `frame`; returns the frame RMS.
    fn load_energy(&mut self, frame: &[f32]) -> f32 {
        let mut acc = 0.0f64;
        self.energy[0] = 0.0;
        for (i, &x) in frame.iter().enumerate() {
            acc += x as f64 * x as f64;
            self.energy[i + 1] = acc;
        }
        (acc / frame.len() as f64).sqrt() as f32
    }

    /// Runs YIN steps 2–5 on `frame` (energy must be loaded). Returns the
    /// refined period in samples (None if the dip lies on the edge of the
    /// detection span, i.e. the true F0 is outside it) and the dip value.
    fn estimate(&mut self, frame: &[f32], threshold: f32) -> (Option<f64>, f32) {
        let (w, n) = (self.window, self.frame_len);
        let fft_len = self.in_frame.len();

        // Step 2 via FFT: r(τ) = Σ_{j<W} x_j x_{j+τ}.
        self.in_window[..w].copy_from_slice(&frame[..w]);
        self.in_window[w..].fill(0.0);
        self.in_frame[..n].copy_from_slice(frame);
        self.in_frame[n..].fill(0.0);
        let fwd_a = self.r2c.process_with_scratch(
            &mut self.in_window,
            &mut self.spec_window,
            &mut self.scratch_fwd,
        );
        let fwd_b = self.r2c.process_with_scratch(
            &mut self.in_frame,
            &mut self.spec_frame,
            &mut self.scratch_fwd,
        );
        if fwd_a.is_err() || fwd_b.is_err() {
            return (None, 1.0);
        }
        for (a, b) in self.spec_window.iter_mut().zip(&self.spec_frame) {
            *a = a.conj() * b;
        }
        // DC and Nyquist bins of a real signal's spectrum are real.
        let last = self.spec_window.len() - 1;
        self.spec_window[0].im = 0.0;
        self.spec_window[last].im = 0.0;
        if self
            .c2r
            .process_with_scratch(&mut self.spec_window, &mut self.corr, &mut self.scratch_inv)
            .is_err()
        {
            return (None, 1.0);
        }
        let scale = 1.0 / fft_len as f64;

        // d(τ) = Σ x_j² + Σ x_{j+τ}² − 2 r(τ)  (paper eq. 7), then step 3 CMNDF.
        let e0 = self.energy[w] - self.energy[0];
        self.diff[0] = 0.0;
        self.cmnd[0] = 1.0;
        let mut running = 0.0f64;
        for tau in 1..=self.tau_max + 1 {
            let e_tau = self.energy[tau + w] - self.energy[tau];
            let d = (e0 + e_tau - 2.0 * self.corr[tau] as f64 * scale).max(0.0);
            self.diff[tau] = d;
            running += d;
            self.cmnd[tau] = if running > 0.0 {
                (d * tau as f64 / running) as f32
            } else {
                1.0
            };
        }

        // Step 4: first lag under the absolute threshold, walked down to its
        // local minimum; otherwise the global minimum within the span. The
        // threshold search starts an octave above the span so that a tone
        // above `detect_max_hz` is found at its own period (and rejected)
        // instead of being reported at a sub-harmonic inside the span.
        let (lo, hi) = (self.tau_min, self.tau_max);
        let search_lo = (lo / 2).max(2);
        let cm = &self.cmnd;
        let tau = match (search_lo..=hi).find(|&t| cm[t] < threshold) {
            Some(mut t) => {
                while t < hi && cm[t + 1] < cm[t] {
                    t += 1;
                }
                t
            }
            None => (lo..=hi).fold(lo, |best, t| if cm[t] < cm[best] { t } else { best }),
        };
        let dip = cm[tau];
        // Outside the span: period too short, or still descending at either
        // end of the span.
        let out_of_span =
            tau < lo || (tau == lo && cm[lo - 1] < cm[lo]) || (tau == hi && cm[hi + 1] < cm[hi]);
        if out_of_span {
            return (None, dip);
        }

        // Step 5: parabolic interpolation on the raw difference function
        // (the paper notes d' interpolation is slightly biased).
        let (a, b, c) = (self.diff[tau - 1], self.diff[tau], self.diff[tau + 1]);
        let denom = a - 2.0 * b + c;
        let shift = if denom > 0.0 {
            (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        (Some(tau as f64 + shift), dip)
    }
}
