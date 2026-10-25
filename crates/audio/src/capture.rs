//! Capture thread (SPEC §3, §6.3): owns a PipeWire main loop on a dedicated thread and one
//! autoconnected capture stream on the default source.
//!
//! Structure: PipeWire callbacks only record facts (graph changes into [`WatchState`], stream /
//! core events into an event queue). A 200 ms loop timer drains those facts and runs a
//! level-triggered `reconcile`, so no callback ever re-enters the engine while it is calling
//! into PipeWire. The process callback touches only the ring buffer, atomics and a channel
//! count; it never allocates, locks or logs.
//!
//! [`WatchState`]: crate::pwgraph::WatchState

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::context::ContextRc;
use pw::core::CoreRc;
use pw::main_loop::MainLoopRc;
use pw::properties::properties;
use pw::registry::RegistryRc;
use pw::spa;
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use rtrb::Producer;

use crate::format::format_pod;
use crate::graph::Graph;
use crate::mute::MutePoller;
use crate::pwgraph::Watch;
use crate::CaptureStatus;

/// Sample rate delivered to the ring buffer.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// `node.name` of the capture stream.
pub const CAPTURE_NODE_NAME: &str = "frame-vocal-pitch-capture";

const TICK: Duration = Duration::from_millis(200);
/// Graph must be quiet this long before it is re-read (coalesces bursts of registry events)…
const DEBOUNCE: Duration = Duration::from_millis(150);
/// …but never wait longer than this after the first change.
const MAX_DEBOUNCE: Duration = Duration::from_millis(600);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const STREAM_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Input is considered healthy while buffers arrived within this window.
const HEALTH_WINDOW_MS: u64 = 3_000;
/// Samples discarded after the stream (re)connects: 0.3 s at 48 kHz.
const WARMUP_FRAMES: u32 = SAMPLE_RATE_HZ * 3 / 10;

/// State shared between the capture thread, the mute poller and [`Capture`] handles.
pub(crate) struct Shared {
    epoch: Instant,
    status: Mutex<StatusCore>,
    pub(crate) muted: AtomicBool,
    stream_alive: AtomicBool,
    last_buffer_ms: AtomicU64,
    discard_frames: AtomicU32,
    dropped_samples: AtomicU64,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
}

#[derive(Clone, Debug, Default)]
struct StatusCore {
    source_description: Option<String>,
    error: Option<String>,
}

/// The published status: facts recorded by the capture thread plus the live mute state and
/// whether buffers are actually arriving.
fn derive_status(core: &StatusCore, muted: bool, streaming: bool) -> CaptureStatus {
    CaptureStatus {
        source_description: core.source_description.clone(),
        error: core.error.clone(),
        muted,
        healthy: streaming,
    }
}

/// Running capture. Samples (48 kHz mono f32) go to the ring buffer passed to [`Capture::start`].
/// Dropping it stops the stream and joins the threads.
pub struct Capture {
    tx: pw::channel::Sender<()>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    _mute: Option<MutePoller>,
}

impl Capture {
    /// Starts the capture thread. Never fails: problems are reported through [`Capture::status`]
    /// (`healthy = false`, `error`) and retried in the background.
    pub fn start(producer: Producer<f32>) -> Capture {
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            status: Mutex::new(StatusCore::default()),
            muted: AtomicBool::new(false),
            stream_alive: AtomicBool::new(false),
            last_buffer_ms: AtomicU64::new(0),
            discard_frames: AtomicU32::new(0),
            dropped_samples: AtomicU64::new(0),
        });
        let (tx, rx) = pw::channel::channel::<()>();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("fvp-capture".into())
            .spawn(move || thread_main(rx, thread_shared, producer))
            .map_err(|e| log::error!("cannot start capture thread: {e}"))
            .ok();
        if thread.is_none() {
            shared.status.lock().unwrap().error = Some("capture thread could not start".into());
        }
        let mute = MutePoller::start(shared.clone())
            .map_err(|e| log::error!("cannot start mute poller: {e}"))
            .ok();
        Capture {
            tx,
            shared,
            thread,
            _mute: mute,
        }
    }

    pub fn status(&self) -> CaptureStatus {
        let core = self.shared.status.lock().unwrap().clone();
        let fresh = self
            .shared
            .now_ms()
            .saturating_sub(self.shared.last_buffer_ms.load(Ordering::Relaxed))
            <= HEALTH_WINDOW_MS;
        let streaming = self.thread.is_some()
            && self.shared.stream_alive.load(Ordering::Relaxed)
            && fresh;
        derive_status(&core, self.shared.muted.load(Ordering::Relaxed), streaming)
    }

    /// Samples dropped because the ring buffer was full (diagnostics).
    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped_samples.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.tx.send(()); // quit
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn thread_main(rx: pw::channel::Receiver<()>, shared: Arc<Shared>, producer: Producer<f32>) {
    pw::init();
    let mainloop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            log::error!("PipeWire main loop: {e}");
            shared.status.lock().unwrap().error = Some(format!("PipeWire unavailable: {e}"));
            return;
        }
    };
    let engine = Rc::new(RefCell::new(Engine {
        mainloop: mainloop.clone(),
        shared,
        producer: Rc::new(RefCell::new(producer)),
        session: None,
        reconnect_at: Instant::now(),
        stream_retry_at: Instant::now(),
        stream_gen: 0,
    }));
    let _rx = rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.downgrade();
        move |()| {
            if let Some(l) = mainloop.upgrade() {
                l.quit();
            }
        }
    });
    let timer = mainloop.loop_().add_timer({
        let engine = engine.clone();
        move |_| engine.borrow_mut().tick()
    });
    let _ = timer.update_timer(Some(Duration::from_millis(1)), Some(TICK));
    mainloop.run();
    // Clean stop: stream → proxies → core, before the loop goes away.
    engine.borrow_mut().drop_session();
}

/// Events recorded by PipeWire callbacks for the next tick.
enum Ev {
    CoreDone(i32),
    CoreError { id: u32, res: i32, message: String },
    StreamState { gen: u64, error: Option<String> },
}

struct OwnedStream {
    // Field order = drop order: listener before stream.
    _listener: StreamListener<()>,
    stream: StreamRc,
    gen: u64,
}

struct Session {
    // Field order = drop order (stream, bound proxies, registry, core, context).
    stream: Option<OwnedStream>,
    watch: Watch,
    graph: Graph,
    events: Rc<RefCell<Vec<Ev>>>,
    sync_seq: Option<i32>,
    sync_rounds: u8,
    _core_listener: pw::core::Listener,
    _registry: RegistryRc,
    core: CoreRc,
    _context: ContextRc,
}

impl Session {
    /// Registry enumerated and metadata bound (two sync round trips).
    fn ready(&self) -> bool {
        self.sync_rounds >= 2
    }
}

struct Engine {
    mainloop: MainLoopRc,
    shared: Arc<Shared>,
    producer: Rc<RefCell<Producer<f32>>>,
    session: Option<Session>,
    reconnect_at: Instant,
    stream_retry_at: Instant,
    stream_gen: u64,
}

/// Only `-EPIPE` on the core means the connection is gone; every other core error refers to
/// one object and must not tear the session down.
pub(crate) fn is_connection_lost(id: u32, res: i32) -> bool {
    id == pw::core::PW_ID_CORE && res == -libc::EPIPE
}

impl Engine {
    fn tick(&mut self) {
        let now = Instant::now();
        if self.session.is_none() {
            if now >= self.reconnect_at {
                if let Err(e) = self.connect() {
                    log::warn!("PipeWire connect failed: {e}");
                    self.set_status(|s| s.error = Some(format!("PipeWire unavailable: {e}")));
                    self.reconnect_at = now + RECONNECT_DELAY;
                }
            }
            return;
        }
        if !self.handle_events() {
            return;
        }
        let Some(session) = self.session.as_mut() else { return };
        if !session.ready() {
            return;
        }
        {
            let mut st = session.watch.state.borrow_mut();
            if let (Some(since), Some(last)) = (st.dirty_since, st.last_change) {
                if now - last >= DEBOUNCE || now - since >= MAX_DEBOUNCE {
                    session.graph = st.take_change();
                }
            }
        }
        // Level-triggered and cheap: compare desired vs actual on every tick.
        self.reconcile(now);
    }

    fn connect(&mut self) -> Result<(), pw::Error> {
        let context = ContextRc::new(&self.mainloop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;
        let watch = Watch::new(&registry);
        let events = Rc::new(RefCell::new(Vec::new()));
        let core_listener = core
            .add_listener_local()
            .done({
                let events = events.clone();
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE {
                        events.borrow_mut().push(Ev::CoreDone(seq.seq()));
                    }
                }
            })
            .error({
                let events = events.clone();
                move |id, _seq, res, message| {
                    events.borrow_mut().push(Ev::CoreError {
                        id,
                        res,
                        message: message.to_owned(),
                    });
                }
            })
            .register();
        let seq = core.sync(0)?.seq();
        log::info!("connected to PipeWire");
        self.session = Some(Session {
            stream: None,
            watch,
            graph: Graph::default(),
            events,
            sync_seq: Some(seq),
            sync_rounds: 0,
            _core_listener: core_listener,
            _registry: registry,
            core,
            _context: context,
        });
        self.set_status(|s| s.error = None);
        Ok(())
    }

    /// Drains callback events. Returns false when the session was torn down.
    fn handle_events(&mut self) -> bool {
        let Some(session) = self.session.as_mut() else { return false };
        let events = std::mem::take(&mut *session.events.borrow_mut());
        for ev in events {
            let Some(session) = self.session.as_mut() else { return false };
            match ev {
                Ev::CoreDone(seq) if session.sync_seq == Some(seq) => {
                    session.sync_rounds += 1;
                    // Second round trip: metadata bound during enumeration has reported.
                    session.sync_seq = if session.ready() {
                        None
                    } else {
                        session.core.sync(0).ok().map(|s| s.seq())
                    };
                    if session.ready() {
                        session.graph = session.watch.state.borrow_mut().take_change();
                    }
                }
                Ev::CoreDone(_) => {}
                Ev::CoreError { id, res, message } => {
                    if is_connection_lost(id, res) {
                        log::warn!("PipeWire connection lost ({res}: {message}); reconnecting");
                        self.drop_session();
                        self.set_status(|s| {
                            s.source_description = None;
                            s.error = Some("PipeWire connection lost".into());
                        });
                        self.reconnect_at = Instant::now() + RECONNECT_DELAY;
                        return false;
                    }
                    // Per-object errors arrive on the core too (e.g. ENOENT "unknown resource"
                    // when a proxy is destroyed after its global went away).
                    log::debug!("PipeWire error on {id}: {message} ({res})");
                }
                Ev::StreamState { gen, error } => {
                    if session.stream.as_ref().is_some_and(|s| s.gen == gen) {
                        self.stream_failed(error.as_deref().unwrap_or("stream disconnected"));
                    }
                }
            }
        }
        true
    }

    fn reconcile(&mut self, now: Instant) {
        if !self.ensure_stream(now) {
            return;
        }
        let Some(session) = self.session.as_ref() else { return };
        let graph = &session.graph;
        let description = graph
            .default_source
            .as_deref()
            .and_then(|name| graph.node_by_name(name))
            .map(|n| n.label());
        self.set_status(|s| {
            s.error = None;
            s.source_description = description;
        });
    }

    /// Makes sure the capture stream exists. Returns false if none exists (retry pending).
    fn ensure_stream(&mut self, now: Instant) -> bool {
        let Some(session) = self.session.as_mut() else { return false };
        if session.stream.is_some() {
            return true;
        }
        if now < self.stream_retry_at {
            return false;
        }
        self.stream_gen += 1;
        match self.create_stream() {
            Ok(s) => {
                log::info!("capture stream created");
                if let Some(session) = self.session.as_mut() {
                    session.stream = Some(s);
                }
                true
            }
            Err(e) => {
                let msg = format!("capture stream failed: {e}");
                log::warn!("{msg}");
                self.stream_retry_at = now + STREAM_RETRY_DELAY;
                self.set_status(|s| s.error = Some(msg));
                false
            }
        }
    }

    fn create_stream(&mut self) -> Result<OwnedStream, pw::Error> {
        let session = self.session.as_ref().ok_or(pw::Error::CreationFailed)?;
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_CLASS => "Stream/Input/Audio",
            *pw::keys::NODE_NAME => CAPTURE_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => "Vocal Pitch capture",
            *pw::keys::APP_NAME => "frame-vocal-pitch",
        };
        let stream = StreamRc::new(session.core.clone(), CAPTURE_NODE_NAME, props)?;
        let gen = self.stream_gen;
        let channels = Rc::new(Cell::new(1u32));
        let listener = stream
            .add_local_listener::<()>()
            .state_changed({
                let events = session.events.clone();
                move |_, _, old, new| {
                    log::debug!("capture stream state {old:?} -> {new:?}");
                    let error = match new {
                        StreamState::Error(e) => Some(e),
                        StreamState::Unconnected => None,
                        _ => return,
                    };
                    events.borrow_mut().push(Ev::StreamState { gen, error });
                }
            })
            .param_changed({
                let channels = channels.clone();
                move |_, _, id, param| {
                    let Some(param) = param else { return };
                    if id != spa::param::ParamType::Format.as_raw() {
                        return;
                    }
                    let mut info = spa::param::audio::AudioInfoRaw::new();
                    if info.parse(param).is_ok() && info.channels() > 0 {
                        log::debug!("capture format: {} Hz, {} ch", info.rate(), info.channels());
                        channels.set(info.channels());
                    }
                }
            })
            .process({
                let shared = self.shared.clone();
                let producer = self.producer.clone();
                move |stream, _| process(stream, &shared, &producer, &channels)
            })
            .register()?;
        // Empty positions: let the graph pick the layout; `process` downmixes whatever arrives.
        let pod = format_pod(&[]);
        let mut params = [spa::pod::Pod::from_bytes(&pod).ok_or(pw::Error::CreationFailed)?];
        // No RT_PROCESS: `process` runs on this thread, so it never races the other callbacks.
        stream.connect(
            spa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        self.shared.discard_frames.store(WARMUP_FRAMES, Ordering::Relaxed);
        self.shared
            .last_buffer_ms
            .store(self.shared.now_ms(), Ordering::Relaxed); // health grace period
        self.shared.stream_alive.store(true, Ordering::Relaxed);
        Ok(OwnedStream {
            _listener: listener,
            stream,
            gen,
        })
    }

    fn stream_failed(&mut self, msg: &str) {
        log::warn!("capture stream failed: {msg}");
        self.drop_stream();
        self.stream_retry_at = Instant::now() + STREAM_RETRY_DELAY;
        let msg = format!("capture stream failed: {msg}");
        self.set_status(|s| s.error = Some(msg));
    }

    fn drop_stream(&mut self) {
        if let Some(session) = self.session.as_mut() {
            if let Some(s) = session.stream.take() {
                let _ = s.stream.disconnect();
                drop(s);
            }
        }
        self.shared.stream_alive.store(false, Ordering::Relaxed);
    }

    fn drop_session(&mut self) {
        self.drop_stream();
        self.session = None;
    }

    fn set_status(&self, f: impl FnOnce(&mut StatusCore)) {
        f(&mut self.shared.status.lock().unwrap());
    }
}

/// Real-time path: downmix interleaved F32 to mono by averaging and push to the ring buffer.
/// No allocation, no locks, no logging.
fn process(stream: &pw::stream::Stream, shared: &Shared, producer: &RefCell<Producer<f32>>, channels: &Cell<u32>) {
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    shared.last_buffer_ms.store(shared.now_ms(), Ordering::Relaxed);
    let datas = buffer.datas_mut();
    let Some(data) = datas.first_mut() else { return };
    let (offset, size) = {
        let chunk = data.chunk();
        (chunk.offset() as usize, chunk.size() as usize)
    };
    let Some(bytes) = data.data() else { return };
    let start = offset.min(bytes.len());
    let end = start.saturating_add(size).min(bytes.len());
    if shared.muted.load(Ordering::Relaxed) {
        return; // user mute: discard input (buffer is re-queued on drop)
    }
    let ch = channels.get().max(1) as usize;
    let frame_bytes = ch * 4;
    let frames = (end - start) / frame_bytes;
    let discard = shared.discard_frames.load(Ordering::Relaxed);
    let skip = frames.min(discard as usize);
    if skip > 0 {
        shared.discard_frames.store(discard - skip as u32, Ordering::Relaxed);
    }
    let wanted = frames - skip;
    if wanted == 0 {
        return;
    }
    let Ok(mut producer) = producer.try_borrow_mut() else { return };
    let n = wanted.min(producer.slots());
    if n < wanted {
        shared.dropped_samples.fetch_add((wanted - n) as u64, Ordering::Relaxed);
    }
    if n == 0 {
        return;
    }
    let first = start + skip * frame_bytes;
    let frames_bytes = &bytes[first..first + n * frame_bytes];
    let scale = 1.0 / ch as f32;
    if let Ok(chunk) = producer.write_chunk_uninit(n) {
        chunk.fill_from_iter(frames_bytes.chunks_exact(frame_bytes).map(|frame| {
            let sum: f32 = frame
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .sum();
            sum * scale
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_epipe_on_the_core_is_a_lost_connection() {
        assert!(is_connection_lost(pw::core::PW_ID_CORE, -libc::EPIPE));
        // "unknown resource N op:M" after a proxy outlived its global: not fatal.
        assert!(!is_connection_lost(pw::core::PW_ID_CORE, -libc::ENOENT));
        assert!(!is_connection_lost(14, -libc::EPIPE));
    }
}
