//! Analysis thread (SPEC §3): pulls 48 kHz mono samples from the capture ring
//! buffer, runs [`Detector`] + [`Tracker`] + [`ReminderTimer`] + [`VocalRest`]
//! and, for the experimental resonance indicator (§5.1), a
//! [`ResonanceTracker`] plus an optional [`Calibration`].
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
    AnalysisFrame, Calibration, Detector, Hop, PitchParams, PitchState, ReminderTimer,
    ResonanceState, ResonanceTracker, Tracker, VocalRest,
};
use fvp_ui::CalibWhich;

/// Reminder tone length (SPEC §5): a tone request is one `play()` call.
pub const TONE_MS: u32 = TONE_DURATION_MS;

/// Samples pulled from the ring buffer per iteration: one 10 ms hop, so every
/// state reaches the main loop with at most one hop of delay.
const CHUNK: usize = 480;
/// Sleep while the ring buffer is empty. Half a hop: no detectable latency, and
/// the thread never spins.
const STARVED: Duration = Duration::from_millis(5);
/// Position grid used to decide whether the resonance marker moved enough to redraw.
const RESONANCE_STEP: f32 = 0.003;

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
    /// Resonance indicator is on (SPEC §5.1): run the formant tracker.
    pub resonance_enabled: bool,
    pub resonance_window_s: f32,
    pub relaxed_df_hz: Option<f32>,
    pub target_df_hz: Option<f32>,
    /// Active mic mode; a change resets the resonance window (§5.1).
    pub mic_mode: fvp_config::MicMode,
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
            resonance_enabled: cfg.resonance.enabled,
            resonance_window_s: cfg.resonance.window_s,
            relaxed_df_hz: cfg.resonance.relaxed_df_hz,
            target_df_hz: cfg.resonance.target_df_hz,
            mic_mode: cfg.audio.mic_mode,
        }
    }
}

/// Progress of a calibration recording reported to the main loop (§5.1).
#[derive(Clone, Debug, PartialEq)]
pub enum CalibUpdate {
    /// Recording, `progress` runs from 0 to 1 over the 10 s.
    Progress { which: CalibWhich, progress: f32 },
    /// Finished; `df_hz` is the median formant spacing of the accepted frames.
    Done { which: CalibWhich, df_hz: f32 },
    /// The recording was abandoned with a message to show as is.
    Failed { which: CalibWhich, message: String },
}

/// Requests from the main loop.
#[derive(Clone, Debug, PartialEq)]
enum Command {
    /// Begin a 10 s recording of `which`.
    Start(CalibWhich),
    /// Drop the running recording silently (resonance off, mic mode changed).
    Abort,
    /// Drop the running recording and report this message (muted, no mic).
    Fail(String),
}

/// What the main loop reads: the newest state plus one-shot events.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub state: Option<PitchState>,
    /// The reminder tone is due; cleared when read.
    pub reminder_due: bool,
    /// A vocal-rest toast is due with this many voiced minutes; cleared when read.
    pub toast_minutes: Option<u32>,
    /// Resonance indicator state while the tracker runs (SPEC §5.1).
    pub resonance: Option<ResonanceState>,
    /// Calibration progress or result since the last read.
    pub calib: Option<CalibUpdate>,
}

struct Shared {
    inner: Mutex<Snapshot>,
    commands: Mutex<Vec<Command>>,
    /// Some overlay shows live pitch data, so per-hop changes are worth waking for.
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
            commands: Mutex::new(Vec::new()),
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
        let snapshot = inner.clone();
        inner.reminder_due = false;
        inner.toast_minutes = None;
        inner.calib = None;
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

    /// Asks the thread to start a 10 s calibration recording (§5.1).
    pub fn start_calibration(&self, which: CalibWhich) {
        self.push(Command::Start(which));
    }

    /// Drops the running recording without a message.
    pub fn abort_calibration(&self) {
        self.push(Command::Abort);
    }

    /// Drops the running recording and reports `message`.
    pub fn fail_calibration(&self, message: impl Into<String>) {
        self.push(Command::Fail(message.into()));
    }

    fn push(&self, command: Command) {
        self.shared
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(command);
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
    let mut resonance = ResonanceTracker::new(applied.resonance_window_s);
    let mut recording: Option<(CalibWhich, Calibration)> = None;

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
            if wanted.resonance_window_s != applied.resonance_window_s {
                resonance.set_window(wanted.resonance_window_s);
                resonance.reset();
            }
            if wanted.mic_mode != applied.mic_mode {
                // SPEC §5.1: the window is not comparable across mic modes.
                resonance.reset();
            }
            applied = wanted;
        }

        for command in drain_commands(&shared) {
            match command {
                Command::Start(which) => {
                    recording = Some((which, Calibration::new()));
                    publish_calib(
                        &shared,
                        CalibUpdate::Progress {
                            which,
                            progress: 0.0,
                        },
                    );
                }
                Command::Abort => recording = None,
                Command::Fail(message) => {
                    if let Some((which, _)) = recording.take() {
                        publish_calib(
                            &shared,
                            CalibUpdate::Failed { which, message },
                        );
                    }
                }
            }
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
        // SPEC §5.1: the formant tracker only runs when it is on or calibrating.
        let resonance_wanted = applied.resonance_enabled || recording.is_some();
        let mut due = false;
        let mut toast = None;
        let mut state = None;
        let mut finished: Option<(CalibWhich, Result<f32, fvp_pitch::CalibError>)> = None;
        {
            let mut on_hop = |hop: Hop, frame: AnalysisFrame<'_>| {
                let current = tracker.update(&hop);
                if applied.reminder_enabled {
                    due |= reminder.update(current.t_s, current.class);
                } else {
                    reminder.reset();
                }
                if let Some(minutes) = vocal.update_hop(&hop) {
                    toast = Some(minutes);
                }
                let df = if resonance_wanted {
                    resonance.update(&hop, frame)
                } else {
                    None
                };
                if let Some((_, calibration)) = recording.as_mut() {
                    calibration.push(df);
                    if calibration.is_done() {
                        let (which, calibration) = recording.take().unwrap();
                        finished = Some((which, calibration.finish()));
                    }
                }
                state = Some(current);
            };
            let (first, second) = chunk.as_slices();
            detector.push_with(first, &mut on_hop);
            detector.push_with(second, &mut on_hop);
        }
        chunk.commit_all();
        if state.is_none() {
            continue;
        }

        let resonance_state = if resonance_wanted {
            Some(resonance.state(applied.relaxed_df_hz, applied.target_df_hz))
        } else {
            None
        };
        let calib = match finished {
            Some((which, Ok(df_hz))) => Some(CalibUpdate::Done { which, df_hz }),
            Some((which, Err(e))) => Some(CalibUpdate::Failed {
                which,
                message: e.to_string(),
            }),
            None => recording.as_ref().map(|(which, calibration)| CalibUpdate::Progress {
                which: *which,
                progress: calibration.progress(),
            }),
        };
        publish(&shared, state, due, toast, resonance_state, calib);
    }
}

fn drain_commands(shared: &Shared) -> Vec<Command> {
    let mut pending = shared.commands.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *pending)
}

/// Stores a calibration update and wakes the main loop.
fn publish_calib(shared: &Shared, update: CalibUpdate) {
    {
        let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.calib = Some(update);
    }
    wake(shared);
}

/// Stores the newest state and wakes the main loop when it would render differently.
fn publish(
    shared: &Shared,
    state: Option<PitchState>,
    due: bool,
    toast: Option<u32>,
    resonance: Option<ResonanceState>,
    calib: Option<CalibUpdate>,
) {
    let Some(new_state) = state else {
        if let Some(update) = calib {
            publish_calib(shared, update);
        }
        return;
    };
    let has_calib = calib.is_some();
    let needs_wake = {
        let live = shared.live.load(Ordering::Relaxed);
        let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let previous = inner.state;
        inner.state = Some(new_state);
        inner.reminder_due |= due;
        if toast.is_some() {
            inner.toast_minutes = toast;
        }
        if calib.is_some() {
            inner.calib = calib;
        }
        let resonance_changed = {
            let previous = inner.resonance;
            inner.resonance = resonance;
            match (previous, resonance) {
                (None, None) => false,
                (Some(ResonanceState::Position(before)), Some(ResonanceState::Position(after))) => {
                    (before - after).abs() >= RESONANCE_STEP && live
                }
                (before, after) => before != after,
            }
        };
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
            || has_calib
            || resonance_changed
            || (f0_changed && live)
    };
    if needs_wake {
        wake(shared);
    }
}

/// Wakes the main loop; a full socket means a wake-up is already pending.
fn wake(shared: &Shared) {
    let mut waker = &shared.waker;
    let _ = waker.write(&[1]);
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

    /// Pushes `samples` into the ring, waiting whenever the buffer is full.
    fn feed(producer: &mut rtrb::Producer<f32>, samples: &[f32]) {
        for &sample in samples {
            while producer.push(sample).is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn sine(f0: f64, seconds: f64) -> Vec<f32> {
        let rate = 48_000.0;
        (0..(rate * seconds) as usize)
            .map(|i| (i as f64 / rate * f0 * std::f64::consts::TAU).sin() as f32 * 0.5)
            .collect()
    }

    /// Glottal pulse train through three formant resonators (a synthetic vowel).
    fn vowel(f0: f64, formants: [f64; 3], seconds: f64) -> Vec<f32> {
        let rate = 48_000.0f64;
        let n = (rate * seconds) as usize;
        let period = (rate / f0).round() as usize;
        // Impulse train: a pulse every glottal period, with a little tilt.
        let mut source = vec![0.0f64; n];
        let mut i = 0;
        while i < n {
            source[i] = 1.0;
            if i + 1 < n {
                source[i + 1] = -0.7;
            }
            i += period;
        }
        let mut signal = source;
        for &formant in &formants {
            let r = (-std::f64::consts::PI * 100.0 / rate).exp();
            let theta = std::f64::consts::TAU * formant / rate;
            let (a1, a2) = (2.0 * r * theta.cos(), -(r * r));
            let mut y1 = 0.0;
            let mut y2 = 0.0;
            for sample in &mut signal {
                let y = *sample + a1 * y1 + a2 * y2;
                y2 = y1;
                y1 = y;
                *sample = y;
            }
        }
        let peak = signal.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let scale = if peak > 0.0 { 0.5 / peak } else { 0.0 };
        signal.iter().map(|v| (v * scale) as f32).collect()
    }

    #[test]
    fn publishes_pitch_state() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(48_000);
        let (analysis, wake_rx) = Analysis::spawn(consumer, control()).unwrap();

        feed(&mut producer, &sine(220.0, 6.0));

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

    #[test]
    fn resonance_runs_only_when_enabled() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(48_000);
        let (analysis, wake_rx) = Analysis::spawn(consumer, control()).unwrap();
        feed(&mut producer, &sine(150.0, 4.0));
        std::thread::sleep(Duration::from_millis(150));
        drain(&wake_rx);
        let snapshot = analysis.take();
        assert!(snapshot.state.is_some(), "pitch should still be published");
        assert!(
            snapshot.resonance.is_none(),
            "resonance tracker ran while disabled"
        );
    }

    #[test]
    fn calibration_records_and_reports_a_formant_spacing() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(96_000);
        let mut cfg = control();
        cfg.resonance_enabled = true;
        let (analysis, wake_rx) = Analysis::spawn(consumer, cfg).unwrap();

        analysis.start_calibration(CalibWhich::Relaxed);
        // 12 s: 10 s of recording plus slack for the ring buffer. Fed from a second
        // thread so the test can observe progress while the recording runs.
        let samples = vowel(150.0, [700.0, 1200.0, 2600.0], 12.0);
        let feeder = std::thread::spawn(move || feed(&mut producer, &samples));

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_progress = false;
        let result = loop {
            drain(&wake_rx);
            match analysis.take().calib {
                Some(CalibUpdate::Progress { which, progress }) => {
                    assert_eq!(which, CalibWhich::Relaxed);
                    saw_progress |= progress > 0.0;
                }
                Some(CalibUpdate::Done { which, df_hz }) => {
                    assert_eq!(which, CalibWhich::Relaxed);
                    break Ok(df_hz);
                }
                Some(CalibUpdate::Failed { message, .. }) => break Err(message),
                None => {}
            }
            assert!(Instant::now() < deadline, "calibration never finished");
            std::thread::sleep(Duration::from_millis(10));
        };

        // Keep draining until the feeder is done, then stop the thread.
        feeder.join().unwrap();
        drop(analysis);
        let df_hz = match result {
            Ok(df_hz) => df_hz,
            Err(message) => panic!("calibration failed: {message}"),
        };
        assert!(saw_progress, "no progress was published");
        // /a/ at 700/1200/2600 Hz implies ΔF ≈ 990 Hz under the uniform-tube fit.
        assert!(
            (600.0..1400.0).contains(&df_hz),
            "implausible ΔF {df_hz} Hz"
        );
    }

    #[test]
    fn abort_and_fail_commands_stop_the_recording() {
        let (_producer, consumer) = rtrb::RingBuffer::new(4_800);
        let (analysis, _wake_rx) = Analysis::spawn(consumer, control()).unwrap();

        analysis.start_calibration(CalibWhich::Target);
        analysis.fail_calibration("Mic muted");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(CalibUpdate::Failed { which, message }) = analysis.take().calib {
                assert_eq!(which, CalibWhich::Target);
                assert_eq!(message, "Mic muted");
                break;
            }
            assert!(Instant::now() < deadline, "no failure was published");
            std::thread::sleep(Duration::from_millis(5));
        }

        // An abort reports nothing at all.
        analysis.start_calibration(CalibWhich::Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        let _ = analysis.take();
        analysis.abort_calibration();
        std::thread::sleep(Duration::from_millis(100));
        assert!(analysis.take().calib.is_none(), "abort published an update");
    }
}
