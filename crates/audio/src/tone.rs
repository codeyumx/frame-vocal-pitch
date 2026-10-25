//! Reminder tone (SPEC §5): a 48 kHz mono sine burst with a 20 ms raised-cosine fade in/out,
//! played through a PipeWire playback stream that only exists while it is needed.
//!
//! The PipeWire objects live on a dedicated thread with its own main loop. [`Tone::play`] only
//! queues a request through a [`pw::channel`]; the stream is created on the first request and
//! destroyed again once it has been idle for [`IDLE_TIMEOUT`], so a tone that never plays (the
//! default: `reminder.enabled = false`) costs no graph nodes at all.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::context::ContextRc;
use pw::core::CoreRc;
use pw::main_loop::MainLoopRc;
use pw::properties::properties;
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};

use crate::format::format_pod;
use crate::graph::CHANNEL_MONO;

/// Sample rate of the tone stream.
pub const TONE_RATE_HZ: u32 = 48_000;
/// `node.name` of the tone's playback stream.
pub const TONE_NODE_NAME: &str = "frame-vocal-pitch-tone";

/// Fade in and out, in milliseconds (SPEC §5).
const FADE_MS: u32 = 20;
/// The stream is torn down after this long without a request (SPEC §3: "destroyed when idle").
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Engine tick: how often [`Player::tick`] checks the idle deadline.
const TICK: Duration = Duration::from_millis(100);
/// F32 samples: mono, so one sample per frame.
const STRIDE: usize = std::mem::size_of::<f32>();

/// Result of [`Tone::new`]'s handshake with the audio thread.
type Ready = SyncSender<Result<(), String>>;

#[derive(Clone, Copy, Debug)]
struct PlayRequest {
    freq_hz: f32,
    volume: f32,
    duration_ms: u32,
}

enum Cmd {
    Play(PlayRequest),
    Quit,
}

/// Non-blocking reminder tone player. Dropping it stops the stream and joins the audio thread.
pub struct Tone {
    tx: pw::channel::Sender<Cmd>,
    shared: Arc<ToneShared>,
    thread: Option<JoinHandle<()>>,
}

impl Tone {
    /// Connects to PipeWire on a dedicated thread. Fails only when PipeWire is unreachable.
    pub fn new() -> Result<Tone, String> {
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let shared = Arc::new(ToneShared::default());
        let (ready, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("fvp-tone".into())
            .spawn(move || run(rx, ready, thread_shared))
            .map_err(|e| format!("cannot start tone thread: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Tone {
                tx,
                shared,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(e) => {
                // No answer: do not join a possibly stuck thread, just give up on it.
                drop(tx);
                Err(format!("PipeWire did not answer in time ({e})"))
            }
        }
    }

    /// Queues a tone; returns immediately. A request that fails to reach the audio thread is
    /// logged and dropped (a reminder tone is not worth blocking the caller for).
    pub fn play(&self, freq_hz: f32, volume: f32, duration_ms: u32) {
        let req = PlayRequest {
            freq_hz,
            volume,
            duration_ms,
        };
        if self.tx.send(Cmd::Play(req)).is_err() {
            log::warn!("tone thread is gone; reminder tone skipped");
        }
    }

    /// `Ok` while the playback thread can accept tones.
    pub fn status(&self) -> Result<(), String> {
        if let Some(e) = self.shared.error.lock().unwrap().clone() {
            return Err(e);
        }
        let running = self.shared.alive.load(Ordering::Relaxed)
            && self.thread.as_ref().is_some_and(|t| !t.is_finished());
        if running {
            Ok(())
        } else {
            Err("tone playback thread stopped".into())
        }
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
struct ToneShared {
    alive: AtomicBool,
    error: Mutex<Option<String>>,
}

impl ToneShared {
    fn fail(&self, error: String) {
        *self.error.lock().unwrap() = Some(error);
        self.alive.store(false, Ordering::Relaxed);
    }
}

fn run(rx: pw::channel::Receiver<Cmd>, ready: Ready, shared: Arc<ToneShared>) {
    pw::init();
    let mainloop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("tone: PipeWire main loop: {e}");
            log::warn!("{msg}");
            shared.fail(msg.clone());
            let _ = ready.send(Err(msg));
            return;
        }
    };
    // The receiver is attached before connecting so a Quit sent during startup is still handled.
    let player: Rc<RefCell<Option<Player>>> = Rc::new(RefCell::new(None));
    let _rx = rx.attach(mainloop.loop_(), {
        let player = player.clone();
        let mainloop = mainloop.downgrade();
        move |cmd| match cmd {
            Cmd::Play(req) => {
                if let Some(p) = player.borrow_mut().as_mut() {
                    p.play(req);
                }
            }
            Cmd::Quit => {
                if let Some(l) = mainloop.upgrade() {
                    l.quit();
                }
            }
        }
    });
    let core = match connect(&mainloop) {
        Ok(core) => core,
        Err(e) => {
            let msg = format!("tone: {e}");
            log::warn!("{msg}");
            shared.fail(msg.clone());
            let _ = ready.send(Err(msg));
            return;
        }
    };
    *player.borrow_mut() = Some(Player::new(core));
    let timer = mainloop.loop_().add_timer({
        let player = player.clone();
        move |_| {
            if let Some(p) = player.borrow_mut().as_mut() {
                p.tick();
            }
        }
    });
    let _ = timer.update_timer(Some(TICK), Some(TICK));
    shared.alive.store(true, Ordering::Relaxed);
    if ready.send(Ok(())).is_err() {
        return; // Tone::new gave up
    }
    mainloop.run();
    if let Some(mut p) = player.borrow_mut().take() {
        p.stop();
    }
    shared.alive.store(false, Ordering::Relaxed);
}

fn connect(mainloop: &MainLoopRc) -> Result<CoreRc, String> {
    let context = ContextRc::new(mainloop, None).map_err(|e| format!("PipeWire context: {e}"))?;
    context
        .connect_rc(None)
        .map_err(|e| format!("PipeWire connect: {e}"))
}

/// Owns the playback stream and the voice currently being rendered.
struct Player {
    core: CoreRc,
    stream: Option<OwnedStream>,
    voice: Rc<RefCell<Option<ToneVoice>>>,
    /// Negotiated channel count of the playback stream.
    channels: Rc<Cell<u32>>,
    failed: Rc<RefCell<Option<String>>>,
    /// Set while the stream exists and no tone is playing.
    idle_since: Option<Instant>,
}

struct OwnedStream {
    // Field order = drop order: listener before stream.
    _listener: StreamListener<()>,
    stream: StreamRc,
}

impl Player {
    fn new(core: CoreRc) -> Player {
        Player {
            core,
            stream: None,
            voice: Rc::new(RefCell::new(None)),
            channels: Rc::new(Cell::new(1)),
            failed: Rc::new(RefCell::new(None)),
            idle_since: None,
        }
    }

    fn play(&mut self, req: PlayRequest) {
        self.check_error();
        if self.stream.is_none() && !self.create_stream() {
            return;
        }
        *self.voice.borrow_mut() = Some(ToneVoice::new(req));
        self.idle_since = None;
    }

    fn create_stream(&mut self) -> bool {
        match self.try_create_stream() {
            Ok(stream) => {
                log::debug!("tone stream created");
                self.stream = Some(stream);
                true
            }
            Err(e) => {
                log::warn!("tone stream failed: {e}");
                false
            }
        }
    }

    /// Drops a stream that reported an error. Returns whether one was dropped.
    fn check_error(&mut self) -> bool {
        let Some(msg) = self.failed.borrow_mut().take() else {
            return false;
        };
        log::warn!("tone stream failed: {msg}");
        self.stop();
        true
    }

    fn try_create_stream(&mut self) -> Result<OwnedStream, String> {
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Notification",
            *pw::keys::NODE_NAME => TONE_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => "Vocal pitch reminder tone",
            *pw::keys::APP_NAME => "frame-vocal-pitch",
        };
        let stream = StreamRc::new(self.core.clone(), TONE_NODE_NAME, props)
            .map_err(|e| e.to_string())?;
        let voice = self.voice.clone();
        let channels = self.channels.clone();
        let failed = self.failed.clone();
        let listener = stream
            .add_local_listener::<()>()
            .state_changed(move |_, _, old, new| {
                log::debug!("tone stream state {old:?} -> {new:?}");
                if let StreamState::Error(e) = new {
                    *failed.borrow_mut() = Some(e);
                }
            })
            .param_changed({
                let channels = channels.clone();
                move |_, _, id, param| {
                    let Some(param) = param else { return };
                    if id != pw::spa::param::ParamType::Format.as_raw() {
                        return;
                    }
                    let mut info = pw::spa::param::audio::AudioInfoRaw::new();
                    if info.parse(param).is_ok() && info.channels() > 0 {
                        channels.set(info.channels());
                    }
                }
            })
            .process({
                let channels = channels.clone();
                move |stream, _| render(stream, &voice, &channels)
            })
            .register()
            .map_err(|e| e.to_string())?;
        let pod = format_pod(&[CHANNEL_MONO]);
        let mut params = [pw::spa::pod::Pod::from_bytes(&pod)
            .ok_or_else(|| "tone format pod".to_string())?];
        stream
            .connect(
                pw::spa::utils::Direction::Output,
                None,
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                &mut params,
            )
            .map_err(|e| e.to_string())?;
        Ok(OwnedStream {
            _listener: listener,
            stream,
        })
    }

    fn tick(&mut self) {
        if self.check_error() {
            return;
        }
        if self.stream.is_none() {
            return;
        }
        let playing = self.voice.borrow().is_some();
        if playing {
            self.idle_since = None;
            return;
        }
        match self.idle_since {
            None => self.idle_since = Some(Instant::now()),
            Some(since) if since.elapsed() >= IDLE_TIMEOUT => {
                log::debug!("tone stream idle; destroying it");
                self.stop();
            }
            Some(_) => {}
        }
    }

    fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.stream.disconnect();
            drop(s);
        }
        self.idle_since = None;
        *self.voice.borrow_mut() = None;
    }
}

/// One tone burst: phase-continuous sine with a raised-cosine fade in and out.
#[derive(Debug)]
struct ToneVoice {
    freq_hz: f64,
    volume: f32,
    /// Total frames (48 kHz).
    total: u32,
    /// Frames rendered so far.
    done: u32,
    /// Phase in cycles, `[0, 1)`.
    phase: f64,
    /// Fade length in frames.
    fade: u32,
}

impl ToneVoice {
    fn new(req: PlayRequest) -> ToneVoice {
        let total = ((req.duration_ms as u64 * TONE_RATE_HZ as u64) / 1000) as u32;
        let fade = ((FADE_MS as u64 * TONE_RATE_HZ as u64) / 1000) as u32;
        ToneVoice {
            freq_hz: f64::from(req.freq_hz.clamp(1.0, 20_000.0)),
            volume: req.volume.clamp(0.0, 1.0),
            total,
            done: 0,
            phase: 0.0,
            fade: fade.min(total / 2),
        }
    }

    fn finished(&self) -> bool {
        self.done >= self.total
    }

    /// Raised-cosine envelope for a frame index: 0 → 1 over the first `fade` frames, 1 → 0 over
    /// the last `fade` frames.
    fn envelope(&self, frame: u32) -> f32 {
        let fade = self.fade;
        if fade == 0 {
            return 1.0;
        }
        let mut env = 1.0f64;
        if frame < fade {
            env *= 0.5 * (1.0 - (std::f64::consts::PI * f64::from(frame) / f64::from(fade)).cos());
        }
        let remaining = self.total - 1 - frame;
        if remaining < fade {
            env *= 0.5
                * (1.0 - (std::f64::consts::PI * f64::from(remaining) / f64::from(fade)).cos());
        }
        env as f32
    }

    /// Next mono sample, or `None` once the burst is complete.
    fn next_sample(&mut self) -> Option<f32> {
        if self.finished() {
            return None;
        }
        let frame = self.done;
        let sample = (std::f64::consts::TAU * self.phase).sin() as f32
            * self.volume
            * self.envelope(frame);
        self.phase += self.freq_hz / f64::from(TONE_RATE_HZ);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        self.done += 1;
        Some(sample)
    }

    /// Renders up to `out.len()` samples; returns how many were written.
    #[cfg(test)]
    fn render_into(&mut self, out: &mut [f32]) -> usize {
        let mut written = 0;
        for slot in out.iter_mut() {
            let Some(sample) = self.next_sample() else {
                break;
            };
            *slot = sample;
            written += 1;
        }
        written
    }
}

/// Playback callback: fills the buffer with the current voice, silence otherwise. The mono
/// sample is written to every channel of the negotiated layout.
fn render(stream: &pw::stream::Stream, voice: &RefCell<Option<ToneVoice>>, channels: &Cell<u32>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    let Some(data) = datas.first_mut() else {
        return;
    };
    let frame_bytes = channels.get().max(1) as usize * STRIDE;
    let mut written = 0usize;
    if let Some(bytes) = data.data() {
        if let Ok(mut slot) = voice.try_borrow_mut() {
            if let Some(v) = slot.as_mut() {
                for frame in bytes.chunks_exact_mut(frame_bytes) {
                    let Some(sample) = v.next_sample() else {
                        break;
                    };
                    let le = sample.to_le_bytes();
                    for lane in frame.chunks_exact_mut(STRIDE) {
                        lane.copy_from_slice(&le);
                    }
                    written += 1;
                }
            }
            if slot.as_ref().is_some_and(ToneVoice::finished) {
                *slot = None; // the burst is over: the idle timer may now destroy the stream
            }
        }
    }
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = frame_bytes as i32;
    *chunk.size_mut() = (written * frame_bytes) as u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(freq_hz: f32, volume: f32, duration_ms: u32) -> ToneVoice {
        ToneVoice::new(PlayRequest {
            freq_hz,
            volume,
            duration_ms,
        })
    }

    fn render_all(v: &mut ToneVoice) -> Vec<f32> {
        let mut out = Vec::new();
        loop {
            let mut buf = [0.0f32; 256];
            let n = v.render_into(&mut buf);
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn burst_length_and_fade() {
        let v = voice(440.0, 0.5, 300);
        assert_eq!(v.total, 14_400); // 0.3 s at 48 kHz
        assert_eq!(v.fade, 960); // 20 ms
        assert_eq!(v.envelope(0), 0.0);
        assert!((v.envelope(960) - 1.0).abs() < 1e-6);
        assert!((v.envelope(v.total / 2) - 1.0).abs() < 1e-6);
        assert!(v.envelope(v.total - 1) < 1e-4);
    }

    #[test]
    fn rendered_burst_is_sine_at_the_requested_level() {
        let mut v = voice(440.0, 0.5, 300);
        let samples = render_all(&mut v);
        assert_eq!(samples.len(), 14_400);
        // Starts and ends at (near) zero because of the fade.
        assert!(samples[0].abs() < 1e-6);
        assert!(samples[samples.len() - 1].abs() < 1e-4);
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        // 440 Hz over 0.3 s ⇒ ~132 positive-going zero crossings.
        let rising = samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((131..=133).contains(&rising), "{rising} rising crossings");
    }

    #[test]
    fn stops_after_the_burst() {
        let mut v = voice(1000.0, 1.0, 1);
        assert_eq!(v.total, 48);
        let mut buf = [0.0f32; 64];
        assert_eq!(v.render_into(&mut buf), 48);
        assert!(v.finished());
        assert_eq!(v.next_sample(), None);
        assert_eq!(v.render_into(&mut buf), 0);
    }

    #[test]
    fn zero_length_and_clamped_parameters() {
        assert_eq!(voice(440.0, 0.5, 0).next_sample(), None);
        let v = voice(0.0, 9.0, 100);
        assert_eq!(v.freq_hz, 1.0);
        assert_eq!(v.volume, 1.0);
        let v = voice(100_000.0, -1.0, 100);
        assert_eq!(v.freq_hz, 20_000.0);
        assert_eq!(v.volume, 0.0);
    }

    #[test]
    fn fade_never_exceeds_half_the_burst() {
        // 5 ms is shorter than the 20 ms fade: halves so the envelope stays a complete shape.
        let v = voice(440.0, 0.5, 5);
        assert_eq!(v.total, 240);
        assert_eq!(v.fade, 120);
        assert_eq!(v.envelope(0), 0.0);
        assert!(v.envelope(v.total - 1) < 1e-4);
    }
}
