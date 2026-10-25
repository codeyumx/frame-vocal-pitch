//! Resonance feedback (SPEC §5.1): formant spacing ΔF of voiced frames,
//! a windowed median for display, calibration recordings and the position
//! of the current value between the two calibrations.
//!
//! Per frame: pre-emphasis, Hamming window, autocorrelation linear prediction
//! (Levinson–Durbin, order [`LPC_ORDER`]), roots of the prediction polynomial
//! (Laguerre's method with deflation). Each complex root `z` is a resonance at
//! `arg z · fs / 2π` Hz with bandwidth `−ln|z| · fs / π` Hz. The three lowest
//! resonances inside 200–4000 Hz with bandwidth below 500 Hz are F1–F3; ΔF is
//! the least-squares spacing of the uniform-tube model `Fi = (2i − 1)·ΔF/2`.
//!
//! Nothing here allocates per hop once the first frame has been analysed.

use crate::params::{
    CALIBRATION_MIN_ACCEPTED_S, CALIBRATION_MIN_SEPARATION, CALIBRATION_S,
    FORMANT_MAX_BANDWIDTH_HZ, FORMANT_MAX_HZ, FORMANT_MIN_HZ, HOP_S, LPC_ORDER, PRE_EMPHASIS,
    RESONANCE_MAX_F0_HZ, RESONANCE_MEDIAN_INTERVAL_S, RESONANCE_MIN_ACCEPTED_S,
    RESONANCE_SMOOTHING_S, RESONANCE_WINDOW_MAX_S, RESONANCE_WINDOW_MIN_S, RESONANCE_WINDOW_S,
    TIME_EPS,
};
use crate::yin::{AnalysisFrame, Hop};
use realfft::num_complex::Complex;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::fmt;

type C64 = Complex<f64>;

/// F1, F2, F3 of one frame in Hz, ascending.
pub type Formants = [f32; 3];

/// Whether the resonance path analyses this hop: voiced with F0 ≤ 300 Hz.
pub fn accepts(hop: &Hop) -> bool {
    hop.f0_hz.is_some_and(|f0| f0 <= RESONANCE_MAX_F0_HZ)
}

/// Least-squares formant spacing of the uniform-tube model:
/// `ΔF = Σ Fi·(2i−1)/2 ÷ Σ ((2i−1)/2)²`, i = 1..3. Higher = brighter.
pub fn formant_spacing(f: Formants) -> f32 {
    const W: [f32; 3] = [0.5, 1.5, 2.5];
    const NORM: f32 = 0.5 * 0.5 + 1.5 * 1.5 + 2.5 * 2.5;
    (W[0] * f[0] + W[1] * f[1] + W[2] * f[2]) / NORM
}

/// Per-frame LPC formant estimator. Holds the window table and one frame of
/// scratch, sized on first use for the frame length it is given.
#[derive(Debug, Default)]
pub struct FormantAnalyzer {
    /// Hamming window for `buf.len()` samples.
    window: Vec<f64>,
    /// Pre-emphasised, windowed frame.
    buf: Vec<f64>,
}

impl FormantAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    /// ΔF of the hop's frame, or `None` when the hop is not [`accepts`]ed or
    /// the frame lacks three valid formants.
    pub fn measure(&mut self, hop: &Hop, frame: AnalysisFrame<'_>) -> Option<f32> {
        if !accepts(hop) {
            return None;
        }
        self.formants(frame).map(formant_spacing)
    }

    /// F1–F3 of a frame (no voicing check), or `None` if fewer than three
    /// resonances qualify.
    pub fn formants(&mut self, frame: AnalysisFrame<'_>) -> Option<Formants> {
        let x = frame.samples;
        let n = x.len();
        let rate = f64::from(frame.rate_hz);
        if n <= 2 * LPC_ORDER || rate.is_nan() || rate <= 0.0 {
            return None;
        }
        if self.window.len() != n {
            self.resize(n);
        }

        // Pre-emphasis (the first sample has no predecessor; it is emphasised
        // against itself) and Hamming window.
        let k = f64::from(PRE_EMPHASIS);
        let mut prev = f64::from(x[0]);
        for ((out, &w), &s) in self.buf.iter_mut().zip(&self.window).zip(x) {
            let cur = f64::from(s);
            *out = (cur - k * prev) * w;
            prev = cur;
        }

        let mut r = [0.0f64; LPC_ORDER + 1];
        for (lag, r) in r.iter_mut().enumerate() {
            *r = self.buf[..n - lag]
                .iter()
                .zip(&self.buf[lag..])
                .map(|(a, b)| a * b)
                .sum();
        }
        let a = levinson_durbin(&r)?;
        let roots = polynomial_roots(&a)?;

        // Upper-half-plane roots are the resonances (the rest are conjugates
        // or real roots).
        let mut found = [0.0f64; LPC_ORDER / 2];
        let mut count = 0;
        for z in roots.iter().filter(|z| z.im > 0.0) {
            let freq = z.arg() * rate / (2.0 * PI);
            let bandwidth = -z.norm().ln() * rate / PI;
            if (f64::from(FORMANT_MIN_HZ)..=f64::from(FORMANT_MAX_HZ)).contains(&freq)
                && bandwidth < f64::from(FORMANT_MAX_BANDWIDTH_HZ)
                && count < found.len()
            {
                found[count] = freq;
                count += 1;
            }
        }
        if count < 3 {
            return None;
        }
        let found = &mut found[..count];
        found.sort_unstable_by(f64::total_cmp);
        Some([found[0] as f32, found[1] as f32, found[2] as f32])
    }

    fn resize(&mut self, n: usize) {
        let m = (n - 1) as f64;
        self.window = (0..n)
            .map(|i| 0.54 - 0.46 * (2.0 * PI * i as f64 / m).cos())
            .collect();
        self.buf = vec![0.0; n];
    }
}

/// Levinson–Durbin recursion on autocorrelation `r[0..=p]`. Returns the
/// prediction polynomial `A(z) = 1 + a1·z⁻¹ + … + ap·z⁻ᵖ` as `[1, a1, …, ap]`,
/// or `None` for a silent or numerically singular frame.
fn levinson_durbin(r: &[f64; LPC_ORDER + 1]) -> Option<[f64; LPC_ORDER + 1]> {
    // A −90 dB white-noise floor keeps near-singular frames well conditioned.
    let r0 = r[0] * (1.0 + 1e-9);
    if !(r0 > 0.0 && r0.is_finite()) {
        return None;
    }
    let mut a = [0.0f64; LPC_ORDER + 1];
    let mut prev = [0.0f64; LPC_ORDER + 1];
    a[0] = 1.0;
    let mut err = r0;
    for i in 1..=LPC_ORDER {
        let acc = r[i] + (1..i).map(|j| a[j] * r[i - j]).sum::<f64>();
        let k = -acc / err;
        prev[..i].copy_from_slice(&a[..i]);
        for j in 1..i {
            a[j] = prev[j] + k * prev[i - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err.is_nan() || err <= 0.0 {
            return None;
        }
    }
    Some(a)
}

/// All complex roots of `zᵖ + a1·zᵖ⁻¹ + … + ap` (the roots of `A(z)` in the
/// z-plane), by Laguerre's method from the origin with deflation. `None` if
/// an iteration fails to converge.
fn polynomial_roots(a: &[f64; LPC_ORDER + 1]) -> Option<[C64; LPC_ORDER]> {
    // Ascending powers: c[i] is the coefficient of zⁱ.
    let mut c = [C64::default(); LPC_ORDER + 1];
    for (i, c) in c.iter_mut().enumerate() {
        *c = C64::new(a[LPC_ORDER - i], 0.0);
    }
    let mut roots = [C64::default(); LPC_ORDER];
    for m in (1..=LPC_ORDER).rev() {
        let z = laguerre(&c[..=m])?;
        roots[m - 1] = z;
        // Synthetic division by (z − root); c[..m] becomes the quotient.
        let mut carry = c[m];
        for j in (0..m).rev() {
            let old = c[j];
            c[j] = carry;
            carry = old + carry * z;
        }
    }
    Some(roots)
}

/// One root of the polynomial with ascending coefficients `c`, starting at 0.
fn laguerre(c: &[C64]) -> Option<C64> {
    const MAX_ITER: usize = 100;
    /// Every `CYCLE_BREAK` iterations take a fractional step, which breaks
    /// the rare limit cycles of the plain iteration.
    const CYCLE_BREAK: usize = 10;
    const FRACTIONS: [f64; 8] = [0.5, 0.25, 0.75, 0.13, 0.38, 0.62, 0.88, 1.0];
    let degree = c.len() - 1;
    let m = degree as f64;
    let mut x = C64::default();
    for iter in 1..=MAX_ITER {
        // Horner for p, p′ and p″/2, plus a bound on p's rounding error.
        let abs_x = x.norm();
        let (mut p, mut d1, mut d2) = (c[degree], C64::default(), C64::default());
        let mut bound = c[degree].norm();
        for &cj in c[..degree].iter().rev() {
            d2 = d2 * x + d1;
            d1 = d1 * x + p;
            p = p * x + cj;
            bound = bound * abs_x + cj.norm();
        }
        if p.norm() <= bound * 4.0 * f64::EPSILON {
            return Some(x);
        }
        let g = d1 / p;
        let g2 = g * g;
        let h = g2 - d2 * 2.0 / p;
        let sq = ((h * m - g2) * (m - 1.0)).sqrt();
        let (plus, minus) = (g + sq, g - sq);
        let denom = if plus.norm() >= minus.norm() {
            plus
        } else {
            minus
        };
        let step = if denom.norm() > 0.0 {
            C64::new(m, 0.0) / denom
        } else {
            C64::from_polar(1.0 + abs_x, iter as f64)
        };
        let next = x - step;
        if next == x {
            return Some(x);
        }
        x = if iter % CYCLE_BREAK == 0 {
            x - step * FRACTIONS[(iter / CYCLE_BREAK) % FRACTIONS.len()]
        } else {
            next
        };
    }
    None
}

/// Clamps a median window to 4–20 s; NaN gives the default.
fn sanitize_window(window_s: f32) -> f32 {
    if window_s.is_nan() {
        RESONANCE_WINDOW_S
    } else {
        window_s.clamp(RESONANCE_WINDOW_MIN_S, RESONANCE_WINDOW_MAX_S)
    }
}

/// Hops in `seconds`, rounded.
fn hops(seconds: f64) -> usize {
    (seconds / HOP_S).round() as usize
}

/// Median of `v` (mean of the two middle values for even lengths); reorders
/// `v`. `None` when empty.
fn median(v: &mut [f32]) -> Option<f32> {
    let n = v.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    let (lower, &mut upper, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    if n % 2 == 1 {
        return Some(upper);
    }
    let below = lower.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Some(0.5 * (below + upper))
}

/// Streaming resonance value: median ΔF of the accepted frames of the last
/// `window_s` seconds, plus its 1 s EMA for display.
///
/// Call [`ResonanceTracker::update`] for **every** hop (voiced or not) so the
/// window keeps sliding through pauses.
#[derive(Debug)]
pub struct ResonanceTracker {
    analyzer: FormantAnalyzer,
    window_s: f32,
    /// `(Hop::t_s, ΔF)` of accepted frames inside the window, oldest first.
    frames: VecDeque<(f64, f32)>,
    /// Scratch for the median selection.
    scratch: Vec<f32>,
    /// Hops until the median is recomputed.
    median_countdown: usize,
    reading: Option<f32>,
    smoothed: Option<f32>,
    smoothing_alpha: f32,
}

impl ResonanceTracker {
    /// Creates a tracker with a median window of `window_s` (clamped 4–20 s).
    /// Buffers for the window are allocated here.
    pub fn new(window_s: f32) -> Self {
        let window_s = sanitize_window(window_s);
        let capacity = Self::capacity(window_s);
        Self {
            analyzer: FormantAnalyzer::new(),
            window_s,
            frames: VecDeque::with_capacity(capacity),
            scratch: Vec::with_capacity(capacity),
            median_countdown: 0,
            reading: None,
            smoothed: None,
            smoothing_alpha: (1.0 - (-HOP_S / RESONANCE_SMOOTHING_S).exp()) as f32,
        }
    }

    /// Accepted frames the window can hold (one per hop).
    fn capacity(window_s: f32) -> usize {
        hops(f64::from(window_s)) + 1
    }

    /// Changes the median window (clamped 4–20 s); may allocate. Frames
    /// outside a shorter window are dropped on the next update.
    pub fn set_window(&mut self, window_s: f32) {
        let window_s = sanitize_window(window_s);
        if window_s == self.window_s {
            return;
        }
        self.window_s = window_s;
        let capacity = Self::capacity(window_s);
        self.frames
            .reserve(capacity.saturating_sub(self.frames.len()));
        self.scratch
            .reserve(capacity.saturating_sub(self.scratch.len()));
        self.median_countdown = 0;
    }

    /// Current (clamped) median window in seconds.
    pub fn window_s(&self) -> f32 {
        self.window_s
    }

    /// Forgets all frames (e.g. after the mic mode changed).
    pub fn reset(&mut self) {
        self.frames.clear();
        self.median_countdown = 0;
        self.reading = None;
        self.smoothed = None;
    }

    /// Feeds one hop with its analysis frame. Returns the frame's ΔF when it
    /// was accepted (useful to feed a [`Calibration`] alongside).
    pub fn update(&mut self, hop: &Hop, frame: AnalysisFrame<'_>) -> Option<f32> {
        let df = self.analyzer.measure(hop, frame);
        let oldest = hop.t_s - f64::from(self.window_s) + TIME_EPS;
        while self.frames.front().is_some_and(|&(t, _)| t < oldest) {
            self.frames.pop_front();
        }
        if let Some(df) = df {
            if self.frames.len() == self.frames.capacity() {
                self.frames.pop_front();
            }
            self.frames.push_back((hop.t_s, df));
        }

        if self.median_countdown == 0 {
            self.median_countdown = hops(RESONANCE_MEDIAN_INTERVAL_S).max(1);
            let accepted_s = self.frames.len() as f64 * HOP_S;
            self.reading = if accepted_s + TIME_EPS >= RESONANCE_MIN_ACCEPTED_S {
                self.scratch.clear();
                self.scratch.extend(self.frames.iter().map(|&(_, df)| df));
                median(&mut self.scratch)
            } else {
                None
            };
        }
        self.median_countdown -= 1;

        self.smoothed = match (self.reading, self.smoothed) {
            (None, _) => None,
            (Some(r), None) => Some(r),
            (Some(r), Some(s)) => Some(s + self.smoothing_alpha * (r - s)),
        };
        df
    }

    /// R: median ΔF (Hz) over the window; `None` while gathering (fewer than
    /// 2 s of accepted frames in the window). Recomputed every 100 ms.
    pub fn reading(&self) -> Option<f32> {
        self.reading
    }

    /// R after the 1 s EMA; this is what the display uses. Since the position
    /// is affine in R, smoothing R smooths the position identically.
    pub fn smoothed(&self) -> Option<f32> {
        self.smoothed
    }

    /// Display state for the given calibration: [`position`] of the smoothed
    /// reading.
    pub fn state(&self, relaxed_df_hz: Option<f32>, target_df_hz: Option<f32>) -> ResonanceState {
        position(self.smoothed, relaxed_df_hz, target_df_hz)
    }
}

/// What the resonance indicator can show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResonanceState {
    /// Calibrated, but fewer than 2 s of accepted frames in the window.
    Gathering,
    /// Relaxed or target calibration missing.
    Uncalibrated,
    /// The two calibrations are less than 5 % apart.
    TooSimilar,
    /// `p = (R − relaxed) / (target − relaxed)`, unclamped (0 = relaxed,
    /// 1 = target); the display clamps it to −0.25…1.25.
    Position(f32),
}

/// True when the calibrations differ by less than 5 % of `relaxed`.
pub fn too_similar(relaxed_df_hz: f32, target_df_hz: f32) -> bool {
    (target_df_hz - relaxed_df_hz).abs() < CALIBRATION_MIN_SEPARATION * relaxed_df_hz
}

/// Maps a reading `r` (normally [`ResonanceTracker::smoothed`]) onto the two
/// calibrations. Precedence: missing calibration, too similar, gathering.
pub fn position(
    r: Option<f32>,
    relaxed_df_hz: Option<f32>,
    target_df_hz: Option<f32>,
) -> ResonanceState {
    let (Some(relaxed), Some(target)) = (relaxed_df_hz, target_df_hz) else {
        return ResonanceState::Uncalibrated;
    };
    if too_similar(relaxed, target) {
        return ResonanceState::TooSimilar;
    }
    match r {
        None => ResonanceState::Gathering,
        Some(r) => ResonanceState::Position((r - relaxed) / (target - relaxed)),
    }
}

/// A calibration recording failed.
#[derive(Clone, Debug, PartialEq)]
pub enum CalibError {
    /// Fewer than 3 s of frames were accepted.
    NotEnoughSpeech {
        /// Seconds of accepted frames that were recorded.
        accepted_s: f32,
    },
}

impl fmt::Display for CalibError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalibError::NotEnoughSpeech { .. } => f.write_str(
                "Not enough clear speech — try again in a quiet place, speaking normally",
            ),
        }
    }
}

impl std::error::Error for CalibError {}

/// One 10 s calibration recording. Feed the per-hop result of
/// [`ResonanceTracker::update`] (or [`FormantAnalyzer::measure`]) to
/// [`Calibration::push`] for every hop until [`Calibration::is_done`].
#[derive(Debug, Clone)]
pub struct Calibration {
    hops: usize,
    total_hops: usize,
    values: Vec<f32>,
}

impl Default for Calibration {
    fn default() -> Self {
        Self::new()
    }
}

impl Calibration {
    /// Starts a recording; allocates room for every hop of it.
    pub fn new() -> Self {
        let total_hops = hops(CALIBRATION_S);
        Self {
            hops: 0,
            total_hops,
            values: Vec::with_capacity(total_hops),
        }
    }

    /// Records one hop: `Some(ΔF)` if its frame was accepted. Ignored once
    /// the recording is done.
    pub fn push(&mut self, df_hz: Option<f32>) {
        if self.is_done() {
            return;
        }
        self.hops += 1;
        if let Some(df) = df_hz {
            self.values.push(df);
        }
    }

    /// Fraction of the 10 s recorded, 0–1.
    pub fn progress(&self) -> f32 {
        self.hops as f32 / self.total_hops as f32
    }

    /// True once 10 s of hops have been pushed.
    pub fn is_done(&self) -> bool {
        self.hops >= self.total_hops
    }

    /// Seconds of accepted frames so far.
    pub fn accepted_s(&self) -> f32 {
        (self.values.len() as f64 * HOP_S) as f32
    }

    /// The stored value: median ΔF of the accepted frames, or an error if
    /// fewer than 3 s were accepted. May be called before the recording is
    /// done (it then judges what was recorded so far).
    pub fn finish(mut self) -> Result<f32, CalibError> {
        let accepted_s = self.values.len() as f64 * HOP_S;
        if accepted_s + TIME_EPS < CALIBRATION_MIN_ACCEPTED_S {
            return Err(CalibError::NotEnoughSpeech {
                accepted_s: accepted_s as f32,
            });
        }
        median(&mut self.values).ok_or(CalibError::NotEnoughSpeech { accepted_s: 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_of_known_polynomial() {
        // Eight conjugate pairs at known radii/angles → expand → recover.
        let expected: Vec<C64> = (0..LPC_ORDER / 2)
            .map(|i| C64::from_polar(0.6 + 0.04 * i as f64, 0.2 + 0.35 * i as f64))
            .collect();
        let mut poly = vec![C64::new(1.0, 0.0)]; // descending powers
        for z in expected.iter().flat_map(|z| [*z, z.conj()]) {
            let mut next = vec![C64::default(); poly.len() + 1];
            for (i, c) in poly.iter().enumerate() {
                next[i] += c;
                next[i + 1] -= c * z;
            }
            poly = next;
        }
        let mut a = [0.0; LPC_ORDER + 1];
        for (a, c) in a.iter_mut().zip(&poly) {
            assert!(c.im.abs() < 1e-12);
            *a = c.re;
        }
        let roots = polynomial_roots(&a).expect("converges");
        for z in &expected {
            let nearest = roots
                .iter()
                .map(|r| (r - z).norm())
                .fold(f64::MAX, f64::min);
            assert!(nearest < 1e-9, "root {z} missing (nearest {nearest})");
        }
    }

    #[test]
    fn levinson_recovers_ar2() {
        // AR(2) x[n] = 1.2 x[n−1] − 0.5 x[n−2] + e: exact autocorrelation
        // via the Yule–Walker relations, then A(z) = 1 − 1.2 z⁻¹ + 0.5 z⁻².
        let (a1, a2) = (1.2f64, -0.5f64);
        let mut r = [0.0; LPC_ORDER + 1];
        r[0] = 1.0;
        r[1] = a1 / (1.0 - a2);
        for k in 2..=LPC_ORDER {
            r[k] = a1 * r[k - 1] + a2 * r[k - 2];
        }
        let a = levinson_durbin(&r).expect("stable");
        assert!(
            (a[1] + 1.2).abs() < 1e-6 && (a[2] - 0.5).abs() < 1e-6,
            "{a:?}"
        );
        assert!(a[3..].iter().all(|c| c.abs() < 1e-6), "{a:?}");
        assert!(levinson_durbin(&[0.0; LPC_ORDER + 1]).is_none());
    }

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
    }
}
