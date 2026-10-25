//! Post-processing of per-hop estimates into a displayable pitch state
//! (SPEC §5 "Post-processing").

use crate::params::{
    BAND_HYSTERESIS_HZ, CLASS_HOLD_S, MEDIAN_LEN, MEDIAN_RESET_S, NO_VOICE_AFTER_S,
    OCTAVE_CONFIRM_HOPS, OCTAVE_TOLERANCE, TIME_EPS,
};
use crate::yin::Hop;

/// Classification of the current pitch against the target band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Class {
    InRange,
    OutOfRange,
    #[default]
    NoVoice,
}

/// Smoothed pitch state published to the main loop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PitchState {
    /// Timestamp of the hop that produced this state.
    pub t_s: f64,
    /// Smoothed F0; held through unvoiced gaps shorter than 150 ms.
    pub f0_hz: Option<f32>,
    pub class: Class,
    /// Voicing confidence of the latest hop: `1 − dip`, clamped to 0..1
    /// (0 for unvoiced hops).
    pub confidence: f32,
}

/// Median filter, octave-jump guard, classification hysteresis and the
/// `no_voice` timeout. Fixed-size state; never allocates.
#[derive(Clone, Debug)]
pub struct Tracker {
    band: (f32, f32),
    /// Ring of the last voiced estimates.
    history: [f32; MEDIAN_LEN],
    history_len: usize,
    history_pos: usize,
    /// Consecutive octave-jump candidates awaiting confirmation.
    pending: [f32; OCTAVE_CONFIRM_HOPS],
    pending_len: usize,
    last_voiced_t: Option<f64>,
    prev_t: Option<f64>,
    class: Class,
    f0: Option<f32>,
    /// Opposite voiced class being considered, and when it started.
    candidate: Option<(Class, f64)>,
}

impl Tracker {
    /// Creates a tracker for the target band `[band_lo, band_hi]` Hz.
    pub fn new(band_lo: f32, band_hi: f32) -> Self {
        Self {
            band: ordered(band_lo, band_hi),
            history: [0.0; MEDIAN_LEN],
            history_len: 0,
            history_pos: 0,
            pending: [0.0; OCTAVE_CONFIRM_HOPS],
            pending_len: 0,
            last_voiced_t: None,
            prev_t: None,
            class: Class::NoVoice,
            f0: None,
            candidate: None,
        }
    }

    /// Changes the target band; takes effect (with hysteresis) on the next hop.
    pub fn set_band(&mut self, band_lo: f32, band_hi: f32) {
        self.band = ordered(band_lo, band_hi);
    }

    pub fn band(&self) -> (f32, f32) {
        self.band
    }

    /// Processes one hop and returns the resulting state.
    pub fn update(&mut self, hop: &Hop) -> PitchState {
        let t = hop.t_s;
        let prev_t = self.prev_t.unwrap_or(t);
        self.prev_t = Some(t);
        let confidence = match hop.f0_hz {
            Some(_) => (1.0 - hop.dip).clamp(0.0, 1.0),
            None => 0.0,
        };
        match hop.f0_hz {
            Some(f) if f.is_finite() && f > 0.0 => self.voiced(t, prev_t, f),
            _ => self.unvoiced(t),
        }
        PitchState {
            t_s: t,
            f0_hz: self.f0,
            class: self.class,
            confidence,
        }
    }

    fn voiced(&mut self, t: f64, prev_t: f64, f: f32) {
        self.last_voiced_t = Some(t);
        self.accept_estimate(f);
        let Some(m) = self.median() else { return };
        self.f0 = Some(m);
        let candidate = self.classify(m);
        if self.class == Class::NoVoice {
            self.class = candidate;
            self.candidate = None;
        } else if candidate == self.class {
            self.candidate = None;
        } else {
            // The previous hop is the last one known to be in the old state.
            let since = match self.candidate {
                Some((c, since)) if c == candidate => since,
                _ => prev_t,
            };
            if t - since >= CLASS_HOLD_S - TIME_EPS {
                self.class = candidate;
                self.candidate = None;
            } else {
                self.candidate = Some((candidate, since));
            }
        }
    }

    fn unvoiced(&mut self, t: f64) {
        self.pending_len = 0;
        let silent_for = self.last_voiced_t.map_or(f64::INFINITY, |lv| t - lv);
        if silent_for >= MEDIAN_RESET_S - TIME_EPS {
            self.history_len = 0;
        }
        if silent_for >= NO_VOICE_AFTER_S - TIME_EPS {
            self.class = Class::NoVoice;
            self.f0 = None;
            self.candidate = None;
        }
    }

    /// Octave-jump guard in front of the median history.
    fn accept_estimate(&mut self, f: f32) {
        let is_jump = self.median().is_some_and(|m| {
            let r = f / m;
            near(r, 2.0, OCTAVE_TOLERANCE) || near(r, 0.5, OCTAVE_TOLERANCE)
        });
        if !is_jump {
            self.pending_len = 0;
            self.push_history(f);
            return;
        }
        let continues = self.pending_len > 0
            && near(
                f / self.pending[self.pending_len - 1],
                1.0,
                OCTAVE_TOLERANCE,
            );
        if !continues {
            self.pending_len = 0;
        }
        self.pending[self.pending_len] = f;
        self.pending_len += 1;
        if self.pending_len == OCTAVE_CONFIRM_HOPS {
            // Confirmed: the new register replaces the old history.
            self.history_len = 0;
            let pending = self.pending;
            for p in pending {
                self.push_history(p);
            }
            self.pending_len = 0;
        }
    }

    fn push_history(&mut self, f: f32) {
        self.history[self.history_pos] = f;
        self.history_pos = (self.history_pos + 1) % MEDIAN_LEN;
        self.history_len = (self.history_len + 1).min(MEDIAN_LEN);
    }

    /// Median of the stored estimates (mean of the middle two when even).
    fn median(&self) -> Option<f32> {
        let n = self.history_len;
        if n == 0 {
            return None;
        }
        // The newest `n` entries end just before `history_pos`.
        let mut v = [0.0f32; MEDIAN_LEN];
        for (i, slot) in v.iter_mut().take(n).enumerate() {
            *slot = self.history[(self.history_pos + MEDIAN_LEN - n + i) % MEDIAN_LEN];
        }
        let v = &mut v[..n];
        v.sort_unstable_by(f32::total_cmp);
        Some(if n % 2 == 1 {
            v[n / 2]
        } else {
            0.5 * (v[n / 2 - 1] + v[n / 2])
        })
    }

    /// In/out of band, with the band widened while in range and narrowed while
    /// out of range.
    fn classify(&self, f: f32) -> Class {
        let (lo, hi) = self.band;
        let h = match self.class {
            Class::InRange => BAND_HYSTERESIS_HZ,
            Class::OutOfRange => -BAND_HYSTERESIS_HZ,
            Class::NoVoice => 0.0,
        };
        if f >= lo - h && f <= hi + h {
            Class::InRange
        } else {
            Class::OutOfRange
        }
    }
}

fn ordered(a: f32, b: f32) -> (f32, f32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn near(value: f32, target: f32, tolerance: f32) -> bool {
    (value - target).abs() <= target * tolerance
}
