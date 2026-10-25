//! Reminder-tone scheduling (SPEC §5 "Reminder tone").

use crate::params::{REMINDER_REPEAT_S, TIME_EPS};
use crate::tracker::Class;

/// Decides when the reminder tone should start: after `delay_s` of continuous
/// `OutOfRange`, then every [`REMINDER_REPEAT_S`] while still out of range.
/// The tone itself (300 ms, 20 ms fades) is played by the caller.
#[derive(Clone, Debug)]
pub struct ReminderTimer {
    delay_s: f64,
    out_since: Option<f64>,
    last_play: Option<f64>,
}

impl ReminderTimer {
    pub fn new(delay_s: f32) -> Self {
        Self {
            delay_s: sanitize_delay(delay_s),
            out_since: None,
            last_play: None,
        }
    }

    /// Changes the delay; an out-of-range run in progress keeps its start time.
    pub fn set_delay(&mut self, delay_s: f32) {
        self.delay_s = sanitize_delay(delay_s);
    }

    /// Forgets the current out-of-range run.
    pub fn reset(&mut self) {
        self.out_since = None;
        self.last_play = None;
    }

    /// Feeds the classification at time `t_s` (monotonic seconds); returns
    /// `true` when the tone should be played now.
    pub fn update(&mut self, t_s: f64, class: Class) -> bool {
        if class != Class::OutOfRange {
            self.reset();
            return false;
        }
        let since = *self.out_since.get_or_insert(t_s);
        let due = match self.last_play {
            None => t_s - since >= self.delay_s - TIME_EPS,
            Some(last) => t_s - last >= REMINDER_REPEAT_S - TIME_EPS,
        };
        if due {
            self.last_play = Some(t_s);
        }
        due
    }
}

fn sanitize_delay(delay_s: f32) -> f64 {
    if delay_s.is_finite() {
        delay_s.max(0.0) as f64
    } else {
        0.0
    }
}
