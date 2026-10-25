//! Voiced-time accounting over a rolling hour (SPEC §5 "Vocal rest").

use crate::params::{HOP_S, TIME_EPS, VOCAL_REST_BUCKETS, VOCAL_REST_REPEAT_S};
use crate::yin::Hop;

/// Accumulates voiced time in 1-second buckets over the last 60 minutes and
/// signals a toast when the total exceeds the configured minutes, at most
/// once every 10 minutes. Voiced time is counted even while disabled, so
/// enabling it later reflects the past hour.
#[derive(Clone, Debug)]
pub struct VocalRest {
    /// Threshold in milliseconds; 0 = off.
    threshold_ms: u64,
    /// Voiced milliseconds per second of wall time, indexed by `second % 3600`.
    buckets: Vec<u32>,
    head_sec: Option<u64>,
    sum_ms: u64,
    last_toast: Option<f64>,
}

impl VocalRest {
    /// `minutes` of voiced time per rolling hour that triggers the toast;
    /// 0 disables it.
    pub fn new(minutes: u32) -> Self {
        Self {
            threshold_ms: minutes as u64 * 60_000,
            buckets: vec![0; VOCAL_REST_BUCKETS],
            head_sec: None,
            sum_ms: 0,
            last_toast: None,
        }
    }

    pub fn set_minutes(&mut self, minutes: u32) {
        self.threshold_ms = minutes as u64 * 60_000;
    }

    /// Voiced seconds within the current rolling window.
    pub fn voiced_s(&self) -> f64 {
        self.sum_ms as f64 / 1000.0
    }

    /// Adds `voiced_s` seconds of voicing at time `t_s` (monotonic seconds;
    /// earlier times are counted in the current bucket). Returns
    /// `Some(minutes voiced in the last hour)` when a toast is due. Toasts are
    /// only raised by updates that add voiced time, so a user who is already
    /// resting is not reminded again.
    pub fn update(&mut self, t_s: f64, voiced_s: f64) -> Option<u32> {
        let sec = if t_s.is_finite() {
            t_s.max(0.0).floor() as u64
        } else {
            0
        };
        let head = match self.head_sec {
            Some(head) if sec > head => {
                let steps = (sec - head).min(VOCAL_REST_BUCKETS as u64);
                for s in head + 1..=head + steps {
                    let slot = &mut self.buckets[(s % VOCAL_REST_BUCKETS as u64) as usize];
                    self.sum_ms -= *slot as u64;
                    *slot = 0;
                }
                sec
            }
            Some(head) => head,
            None => sec,
        };
        self.head_sec = Some(head);
        let ms = if voiced_s.is_finite() && voiced_s > 0.0 {
            (voiced_s * 1000.0).round() as u32
        } else {
            0
        };
        if ms == 0 {
            return None;
        }
        self.buckets[(head % VOCAL_REST_BUCKETS as u64) as usize] += ms;
        self.sum_ms += ms as u64;
        let due = self.threshold_ms > 0
            && self.sum_ms > self.threshold_ms
            && self
                .last_toast
                .is_none_or(|last| t_s - last >= VOCAL_REST_REPEAT_S - TIME_EPS);
        if !due {
            return None;
        }
        self.last_toast = Some(t_s);
        Some((self.sum_ms / 60_000) as u32)
    }

    /// Convenience for one detector hop: counts 10 ms if the hop is voiced.
    pub fn update_hop(&mut self, hop: &Hop) -> Option<u32> {
        self.update(hop.t_s, if hop.f0_hz.is_some() { HOP_S } else { 0.0 })
    }
}
