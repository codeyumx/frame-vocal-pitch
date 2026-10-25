//! Analysis thread (SPEC §3): pulls 48 kHz mono samples from the capture ring
//! buffer, runs [`Detector`] + [`Tracker`] + [`ReminderTimer`] + [`VocalRest`],
//! and publishes the latest [`PitchState`] to the main loop.
//!
//! The main loop never blocks on this thread: the published snapshot sits behind
//! one short-lived mutex, and a byte written to a non-blocking socket wakes the
//! main loop's `poll()` when something render-relevant changed.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use fvp_config::Config;
use fvp_pitch::params::{INPUT_RATE_HZ, TONE_DURATION_MS};
use fvp_pitch::{
    Detector, Hop, PitchParams, PitchState, ReminderTimer, Tracker, VocalRest,
};

/// Reminder tone length (SPEC §5): a tone request is one `play()` call.
pub const TONE_MS: u32 = TONE_DURATION_MS;

/// Samples pulled from the ring buffer per iteration: one 10 ms hop, so every
/// state reaches the main loop with at most one hop of delay.
const CHUNK: usize = 480;
/// Sleep while the ring buffer is empty. Half a hop: no detectable latency, and
/// the thread never spins.
const STARVED: Duration = Duration::from_millis(5);

/// Everything the analysis thread derives from the configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub params: PitchParams,
    pub band: (f32, f32),
    pub reminder_enabled: bool,
    pub reminder_delay_s: f32,
    pub vocal_rest_minutes: u32,
    /// The user's mute: samples are drained and discarded (SPEC §4.1).
    pub muted: bool,
    /// Nothing visible needs pitch and the vocal-rest timer is off, so the
    /// samples are drained and discarded (SPEC §4.1 "stop rendering").
    pub paused: bool,
}

impl Control {
    /// Configuration values plus the current mute and pause states.
    pub fn from_config(cfg: &Config, muted: bool, paused: bool) -> Control {
        Control {
            params: PitchParams::new(
                cfg.audio.detect_min_hz,
                cfg.audio.detect_max_hz,
                cfg.audio.noise_floor_min_dbfs,
            ),
            band: cfg.band(),
            reminder_enabled: cfg.reminder.enabled,
            reminder_delay_s: cfg.reminder.delay_s,
            vocal_rest_minutes: cfg.vocal_rest.minutes,
            muted,
            paused,
        }
    }
}

/// What the main loop reads: the newest state plus one-shot events.
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub state: Option<PitchState>,
    /// The reminder tone is due; cleared when read.
    pub reminder_due: bool,
    /// A vocal-rest toast is due with this many voiced minutes; cleared when read.
    pub toast_minutes: Option<u32>,
}

struct Shared {
    inner: Mutex<Snapshot>,
    /// Some overlay shows live pitch data, so per-hop F0 changes are worth waking for.
    live: AtomicBool,
    waker: UnixStream,
}

/// Handle of the running analysis thread.
pub struct Analysis {
    shared: Arc<Shared>,
    control: Arc<Mutex<Control>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Analysis {
    /// Starts the thread and returns it with the read end of its wake socket.
    pub fn spawn(
        consumer: rtrb::Consumer<f32>,
        control: Control,
    ) -> std::io::Result<(Analysis, UnixStream)> {
        let (waker, wake_rx) = UnixStream::pair()?;
        waker.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            inner: Mutex::new(Snapshot::default()),
            live: AtomicBool::new(true),
            waker,
        });
        let control = Arc::new(Mutex::new(control));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = Arc::clone(&shared);
            let control = Arc::clone(&control);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("fvp-analysis".into())
                .spawn(move || run(consumer, shared, control, stop))?
        };
        Ok((
            Analysis {
                shared,
                control,
                stop,
                thread: Some(thread),
            },
            wake_rx,
        ))
    }

    /// Newest state, consuming the pending one-shot events.
    pub fn take(&self) -> Snapshot {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = *inner;
        inner.reminder_due = false;
        inner.toast_minutes = None;
        snapshot
    }

    /// Applies new parameters (picked up at the next iteration).
    pub fn set_control(&self, control: Control) {
        *self.control.lock().unwrap_or_else(|e| e.into_inner()) = control;
    }

    /// Whether an overlay currently shows live pitch data.
    pub fn set_live(&self, live: bool) {
        self.shared.live.store(live, Ordering::Relaxed);
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Analysis {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    mut consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    control: Arc<Mutex<Control>>,
    stop: Arc<AtomicBool>,
) {
    let mut applied = *control.lock().unwrap_or_else(|e| e.into_inner());
    let mut detector = Detector::new(applied.params, INPUT_RATE_HZ);
    let mut tracker = Tracker::new(applied.band.0, applied.band.1);
    let mut reminder = ReminderTimer::new(applied.reminder_delay_s);
    let mut vocal = VocalRest::new(applied.vocal_rest_minutes);
    let mut hops: Vec<Hop> = Vec::with_capacity(8);

    while !stop.load(Ordering::Relaxed) {
        let wanted = *control.lock().unwrap_or_else(|e| e.into_inner());
        if wanted != applied {
            if wanted.params != applied.params {
                detector.set_params(wanted.params);
            }
            if wanted.band != applied.band {
                tracker.set_band(wanted.band.0, wanted.band.1);
            }
            if wanted.reminder_delay_s != applied.reminder_delay_s {
                reminder.set_delay(wanted.reminder_delay_s);
            }
            if wanted.vocal_rest_minutes != applied.vocal_rest_minutes {
                vocal.set_minutes(wanted.vocal_rest_minutes);
            }
            applied = wanted;
        }

        let slots = consumer.slots().min(CHUNK);
        if slots == 0 {
            thread::sleep(STARVED);
            continue;
        }
        let Ok(chunk) = consumer.read_chunk(slots) else {
            thread::sleep(STARVED);
            continue;
        };
        if applied.muted || applied.paused {
            // Discard: muted by the user, or nothing visible needs the analysis.
            chunk.commit_all();
            continue;
        }
        hops.clear();
        {
            let (first, second) = chunk.as_slices();
            detector.push(first, &mut hops);
            detector.push(second, &mut hops);
        }
        chunk.commit_all();
        if hops.is_empty() {
            continue;
        }

        let mut due = false;
        let mut toast = None;
        let mut state = None;
        for hop in &hops {
            let current = tracker.update(hop);
            if applied.reminder_enabled {
                due |= reminder.update(current.t_s, current.class);
            } else {
                reminder.reset();
            }
            if let Some(minutes) = vocal.update_hop(hop) {
                toast = Some(minutes);
            }
            state = Some(current);
        }
        publish(&shared, state, due, toast);
    }
}

/// Stores the newest state and wakes the main loop when it would render differently.
fn publish(
    shared: &Shared,
    state: Option<PitchState>,
    due: bool,
    toast: Option<u32>,
) {
    let Some(new_state) = state else {
        return;
    };
    let wake = {
        let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let previous = inner.state;
        inner.state = Some(new_state);
        inner.reminder_due |= due;
        if toast.is_some() {
            inner.toast_minutes = toast;
        }
        let class_changed = previous.is_some_and(|p| p.class != new_state.class);
        let f0_changed = match (previous.and_then(|p| p.f0_hz), new_state.f0_hz) {
            (Some(before), Some(after)) => (before.round()) != (after.round()),
            (before, after) => before.is_some() != after.is_some(),
        };
        let first = previous.is_none();
        first
            || class_changed
            || due
            || toast.is_some()
            || (f0_changed && shared.live.load(Ordering::Relaxed))
    };
    if wake {
        // A full socket means a wake-up is already pending; losing a byte is fine.
        let mut waker = &shared.waker;
        let _ = waker.write(&[1]);
    }
}

/// Drains the wake socket without blocking.
pub fn drain(rx: &UnixStream) {
    let mut buf = [0u8; 256];
    let mut rx = rx;
    loop {
        match rx.read(&mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn control() -> Control {
        Control::from_config(&Config::default(), false, false)
    }

    /// Feeds a 220 Hz sine through the real pipeline and reads it back through the API.
    #[test]
    fn publishes_pitch_state() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(48_000);
        let (analysis, wake_rx) = Analysis::spawn(consumer, control()).unwrap();

        let mut phase = 0.0f64;
        let mut block = Vec::with_capacity(4_800);
        for _ in 0..4_800 * 6 {
            block.push((phase * std::f64::consts::TAU).sin() as f32 * 0.5);
            phase = (phase + 220.0 / 48_000.0).fract();
            if block.len() == 4_800 {
                for sample in block.drain(..) {
                    while producer.push(sample).is_err() {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            drain(&wake_rx);
            let snapshot = analysis.take();
            if let Some(state) = snapshot.state {
                if let Some(f0) = state.f0_hz {
                    assert!((f0 - 220.0).abs() < 2.2, "F0 {f0}");
                    assert_eq!(state.class, fvp_pitch::Class::InRange);
                    break;
                }
            }
            assert!(Instant::now() < deadline, "no pitch state published");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(analysis);
    }

    #[test]
    fn muted_input_is_discarded() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(4_800);
        let mut muted = control();
        muted.muted = true;
        let (analysis, wake_rx) = Analysis::spawn(consumer, muted).unwrap();
        for i in 0..4_800 {
            let _ = producer.push((i as f32 * 0.1).sin());
        }
        std::thread::sleep(Duration::from_millis(100));
        drain(&wake_rx);
        assert!(analysis.take().state.is_none(), "muted input was analysed");
    }
}
