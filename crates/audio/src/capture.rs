//! Capture thread (SPEC §3, §6.3): owns a PipeWire main loop on a dedicated thread, the
//! capture stream and, in bypass mode, the links from the hardware mic to that stream.
//!
//! Structure: PipeWire callbacks only record facts (graph changes into [`WatchState`], stream /
//! link / core events into an event queue). A 200 ms loop timer drains those facts and runs a
//! level-triggered `reconcile`, so no callback ever re-enters the engine while it is calling
//! into PipeWire. The process callback touches only the ring buffer, atomics and a channel
//! count; it never allocates, locks or logs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::context::ContextRc;
use pw::core::CoreRc;
use pw::link::{LinkListener, LinkState};
use pw::main_loop::MainLoopRc;
use pw::properties::properties;
use pw::registry::RegistryRc;
use pw::spa;
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use rtrb::Producer;

use crate::format::format_pod;
use crate::graph::{
    channel_position, filters_active, match_channels, resolve_hardware_source, Graph, HwSource,
};
use crate::mute::MutePoller;
use crate::pwgraph::Watch;
use crate::{CaptureStatus, MicMode};

/// Sample rate delivered to the ring buffer.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// `node.name` of the capture stream.
pub const CAPTURE_NODE_NAME: &str = "frame-vocal-pitch-capture";
pub(crate) const NO_NODE: u32 = u32::MAX;

const TICK: Duration = Duration::from_millis(200);
/// Graph must be quiet this long before reconciling (coalesces bursts of registry events)…
const DEBOUNCE: Duration = Duration::from_millis(150);
/// …but never wait longer than this after the first change (re-resolve within 1 s).
const MAX_DEBOUNCE: Duration = Duration::from_millis(600);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const STREAM_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Bypass stream ports must appear within this time, links within `LINK_TIMEOUT`.
const PORT_TIMEOUT: Duration = Duration::from_secs(3);
const LINK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LINK_ATTEMPTS: u32 = 3;
/// Input is considered healthy while buffers arrived within this window.
const HEALTH_WINDOW_MS: u64 = 3_000;
/// Samples discarded after (re)linking: 0.3 s at 48 kHz.
const WARMUP_FRAMES: u32 = SAMPLE_RATE_HZ * 3 / 10;

/// State shared between the capture thread, the mute poller and [`Capture`] handles.
pub(crate) struct Shared {
    epoch: Instant,
    status: Mutex<StatusCore>,
    pub(crate) muted: AtomicBool,
    /// Hardware node whose own mute is honoured (bypass only), or [`NO_NODE`].
    pub(crate) hw_node_id: AtomicU32,
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

#[derive(Clone, Debug)]
struct StatusCore {
    requested: MicMode,
    active: MicMode,
    source_description: Option<String>,
    fallback_reason: Option<String>,
    filters_active_for_others: Option<bool>,
}

/// The published status: facts recorded by the capture thread plus the live mute state and
/// whether buffers are actually arriving.
fn derive_status(core: &StatusCore, muted: bool, streaming: bool) -> CaptureStatus {
    CaptureStatus {
        requested: core.requested,
        active: core.active,
        source_description: core.source_description.clone(),
        fallback_reason: core.fallback_reason.clone(),
        filters_active_for_others: core.filters_active_for_others,
        muted,
        healthy: streaming,
    }
}

enum Cmd {
    SetMode(MicMode),
    Quit,
}

/// Running capture. Samples (48 kHz mono f32) go to the ring buffer passed to [`Capture::start`].
/// Dropping it stops the stream, removes the links and joins the threads.
pub struct Capture {
    tx: pw::channel::Sender<Cmd>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    _mute: Option<MutePoller>,
}

impl Capture {
    /// Starts the capture thread. Never fails: problems are reported through [`Capture::status`]
    /// (`healthy = false`, `fallback_reason`) and retried in the background.
    pub fn start(mode: MicMode, producer: Producer<f32>) -> Capture {
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            status: Mutex::new(StatusCore {
                requested: mode,
                active: mode,
                source_description: None,
                fallback_reason: None,
                filters_active_for_others: None,
            }),
            muted: AtomicBool::new(false),
            hw_node_id: AtomicU32::new(NO_NODE),
            stream_alive: AtomicBool::new(false),
            last_buffer_ms: AtomicU64::new(0),
            discard_frames: AtomicU32::new(0),
            dropped_samples: AtomicU64::new(0),
        });
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("fvp-capture".into())
            .spawn(move || thread_main(rx, thread_shared, producer, mode))
            .map_err(|e| log::error!("cannot start capture thread: {e}"))
            .ok();
        if thread.is_none() {
            shared.status.lock().unwrap().fallback_reason = Some("capture thread could not start".into());
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

    /// Switches mode; takes effect within ~200 ms.
    pub fn set_mode(&self, mode: MicMode) {
        self.shared.status.lock().unwrap().requested = mode;
        if self.tx.send(Cmd::SetMode(mode)).is_err() {
            log::warn!("capture thread is gone; mode change ignored");
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
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn thread_main(rx: pw::channel::Receiver<Cmd>, shared: Arc<Shared>, producer: Producer<f32>, mode: MicMode) {
    pw::init();
    let mainloop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            log::error!("PipeWire main loop: {e}");
            shared.status.lock().unwrap().fallback_reason = Some(format!("PipeWire unavailable: {e}"));
            return;
        }
    };
    let engine = Rc::new(RefCell::new(Engine {
        mainloop: mainloop.clone(),
        shared,
        producer: Rc::new(RefCell::new(producer)),
        requested: mode,
        session: None,
        reconnect_at: Instant::now(),
        stream_retry_at: Instant::now(),
        blocked: None,
        link_attempts: 0,
        stream_gen: 0,
        link_gen: 0,
    }));
    let _rx = rx.attach(mainloop.loop_(), {
        let engine = engine.clone();
        let mainloop = mainloop.downgrade();
        move |cmd| match cmd {
            Cmd::Quit => {
                if let Some(l) = mainloop.upgrade() {
                    l.quit();
                }
            }
            Cmd::SetMode(m) => {
                // Setting the mode (even to the same value) clears a sticky bypass fallback.
                let mut e = engine.borrow_mut();
                e.requested = m;
                e.blocked = None;
                e.link_attempts = 0;
                e.stream_retry_at = Instant::now();
            }
        }
    });
    let timer = mainloop.loop_().add_timer({
        let engine = engine.clone();
        move |_| engine.borrow_mut().tick()
    });
    let _ = timer.update_timer(Some(Duration::from_millis(1)), Some(TICK));
    mainloop.run();
    // Clean stop: links → stream → proxies → core, before the loop goes away.
    engine.borrow_mut().drop_session();
}

/// Events recorded by PipeWire callbacks for the next tick.
enum Ev {
    CoreDone(i32),
    CoreError { id: u32, res: i32, message: String },
    StreamState { gen: u64, error: Option<String> },
    LinkFailed { gen: u64, message: String },
}

#[derive(Clone, Debug, PartialEq)]
enum StreamKind {
    Processed,
    Bypass { hw_node: u32, channels: Vec<String> },
}

struct OwnedStream {
    // Field order = drop order: listener before stream.
    _listener: StreamListener<()>,
    stream: StreamRc,
    kind: StreamKind,
    gen: u64,
    created: Instant,
}

struct OwnedLink {
    _listener: LinkListener,
    link: pw::link::Link,
}

struct Linked {
    gen: u64,
    hw_node: u32,
    stream_node: u32,
    pairs: Vec<(u32, u32)>,
    seen: Vec<bool>,
    created: Instant,
}

struct Session {
    // Field order = drop order (links, stream, bound proxies, registry, core, context).
    links: Vec<OwnedLink>,
    linked: Option<Linked>,
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
    requested: MicMode,
    session: Option<Session>,
    reconnect_at: Instant,
    stream_retry_at: Instant,
    /// Bypass failed for this hardware node (name, user-facing reason); stays in processed
    /// until the resolved node changes or the mode is set again.
    blocked: Option<(String, String)>,
    link_attempts: u32,
    stream_gen: u64,
    link_gen: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum Plan {
    Processed { reason: Option<String> },
    Bypass(HwSource),
}

/// SPEC §6.3 steps 1 and 6: what to record from, given the graph and a sticky bypass failure.
/// Pure so the decision can be unit tested without a PipeWire server.
fn choose_plan(requested: MicMode, graph: &Graph, blocked: Option<&(String, String)>) -> Plan {
    if requested == MicMode::Processed {
        return Plan::Processed { reason: None };
    }
    match resolve_hardware_source(graph) {
        Ok(hw) => match blocked {
            Some((name, reason)) if *name == hw.name => Plan::Processed {
                reason: Some(reason.clone()),
            },
            _ => Plan::Bypass(hw),
        },
        Err(e) => Plan::Processed {
            reason: Some(bypass_reason(&e.to_string())),
        },
    }
}

fn bypass_reason(detail: &str) -> String {
    format!("Bypass unavailable: {detail} — using processed mic")
}

impl Engine {
    fn tick(&mut self) {
        let now = Instant::now();
        if self.session.is_none() {
            if now >= self.reconnect_at {
                if let Err(e) = self.connect() {
                    log::warn!("PipeWire connect failed: {e}");
                    self.set_status(|s| s.fallback_reason = Some(format!("PipeWire unavailable: {e}")));
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
            links: Vec::new(),
            linked: None,
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
        self.set_status(|s| s.fallback_reason = None);
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
                    if id == pw::core::PW_ID_CORE {
                        log::warn!("PipeWire connection lost ({res}: {message}); reconnecting");
                        self.drop_session();
                        self.set_status(|s| {
                            s.source_description = None;
                            s.fallback_reason = Some("PipeWire connection lost".into());
                        });
                        self.reconnect_at = Instant::now() + RECONNECT_DELAY;
                        return false;
                    }
                    let ours = session
                        .links
                        .iter()
                        .any(|l| pw::proxy::ProxyT::upcast_ref(&l.link).id() == id);
                    if ours {
                        self.link_failed(&format!("{message} ({res})"));
                    } else {
                        log::debug!("PipeWire error on proxy {id}: {message} ({res})");
                    }
                }
                Ev::StreamState { gen, error } => {
                    if session.stream.as_ref().is_some_and(|s| s.gen == gen) {
                        self.stream_failed(error.as_deref().unwrap_or("stream disconnected"));
                    }
                }
                Ev::LinkFailed { gen, message } => {
                    if session.linked.as_ref().is_some_and(|l| l.gen == gen) {
                        self.link_failed(&message);
                    }
                }
            }
        }
        true
    }

    fn plan(&mut self, graph: &Graph) -> Plan {
        let plan = choose_plan(self.requested, graph, self.blocked.as_ref());
        if self.blocked.is_some() && matches!(plan, Plan::Bypass(_)) {
            // A different hardware node resolved (unplug/replug): retry bypass from scratch.
            self.blocked = None;
            self.link_attempts = 0;
        }
        plan
    }

    fn reconcile(&mut self, now: Instant) {
        let Some(session) = self.session.as_ref() else { return };
        let graph = session.graph.clone();
        let filters = filters_active(&graph);
        match self.plan(&graph) {
            Plan::Processed { reason } => {
                self.drop_links();
                self.shared.hw_node_id.store(NO_NODE, Ordering::Relaxed);
                if !self.ensure_stream(StreamKind::Processed, now) {
                    return;
                }
                let description = self.stream_node().map(|id| {
                    graph
                        .upstream_of(id)
                        .iter()
                        .map(|n| n.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                });
                self.set_status(|s| {
                    s.active = MicMode::Processed;
                    s.fallback_reason = reason;
                    s.source_description = description.filter(|d| !d.is_empty());
                    s.filters_active_for_others = filters;
                });
            }
            Plan::Bypass(hw) => {
                let kind = StreamKind::Bypass {
                    hw_node: hw.node_id,
                    channels: hw.channels(),
                };
                // Before the stream exists: the mute poller honours the hardware node's mute.
                self.shared.hw_node_id.store(hw.node_id, Ordering::Relaxed);
                if !self.ensure_stream(kind, now) {
                    return;
                }
                self.set_status(|s| {
                    s.active = MicMode::Bypass;
                    s.fallback_reason = None;
                    s.source_description = Some(hw.description.clone());
                    s.filters_active_for_others = filters;
                });
                self.ensure_links(&graph, &hw, now);
            }
        }
    }

    fn stream_node(&self) -> Option<u32> {
        let id = self.session.as_ref()?.stream.as_ref()?.stream.node_id();
        (id != pw::constants::ID_ANY).then_some(id)
    }

    /// Makes sure a stream of `kind` exists. Returns false if none exists (retry pending).
    fn ensure_stream(&mut self, kind: StreamKind, now: Instant) -> bool {
        let Some(session) = self.session.as_mut() else { return false };
        if session.stream.as_ref().is_some_and(|s| s.kind == kind) {
            return true;
        }
        self.drop_links();
        self.drop_stream();
        if now < self.stream_retry_at {
            return false;
        }
        self.stream_gen += 1;
        match self.create_stream(kind.clone()) {
            Ok(s) => {
                log::info!("capture stream created ({kind:?})");
                if let Some(session) = self.session.as_mut() {
                    session.stream = Some(s);
                }
                true
            }
            Err(e) => {
                let msg = format!("capture stream failed: {e}");
                log::warn!("{msg}");
                self.stream_retry_at = now + STREAM_RETRY_DELAY;
                if let StreamKind::Bypass { .. } = kind {
                    self.block_current(&msg);
                } else {
                    self.set_status(|s| s.fallback_reason = Some(msg));
                }
                false
            }
        }
    }

    fn create_stream(&mut self, kind: StreamKind) -> Result<OwnedStream, pw::Error> {
        let session = self.session.as_ref().ok_or(pw::Error::CreationFailed)?;
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_CLASS => "Stream/Input/Audio",
            *pw::keys::NODE_NAME => CAPTURE_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => "Vocal Pitch capture",
            *pw::keys::APP_NAME => "frame-vocal-pitch",
        };
        let (flags, positions) = match &kind {
            StreamKind::Processed => (StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS, Vec::new()),
            StreamKind::Bypass { channels, .. } => {
                props.insert("node.autoconnect", "false");
                props.insert("node.dont-reconnect", "true");
                props.insert("node.virtual", "true");
                let pos = channels
                    .iter()
                    .map(|c| channel_position(c).ok_or(pw::Error::CreationFailed))
                    .collect::<Result<Vec<_>, _>>()?;
                (StreamFlags::MAP_BUFFERS, pos)
            }
        };
        let stream = StreamRc::new(session.core.clone(), CAPTURE_NODE_NAME, props)?;
        let gen = self.stream_gen;
        let channels = Rc::new(Cell::new(positions.len().max(1) as u32));
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
        let pod = format_pod(&positions);
        let mut params = [spa::pod::Pod::from_bytes(&pod).ok_or(pw::Error::CreationFailed)?];
        // No RT_PROCESS: `process` runs on this thread, so it never races the other callbacks.
        stream.connect(spa::utils::Direction::Input, None, flags, &mut params)?;
        self.shared.discard_frames.store(WARMUP_FRAMES, Ordering::Relaxed);
        self.shared
            .last_buffer_ms
            .store(self.shared.now_ms(), Ordering::Relaxed); // health grace period
        self.shared.stream_alive.store(true, Ordering::Relaxed);
        Ok(OwnedStream {
            _listener: listener,
            stream,
            kind,
            gen,
            created: Instant::now(),
        })
    }

    fn ensure_links(&mut self, graph: &Graph, hw: &HwSource, now: Instant) {
        let Some(stream_node) = self.stream_node() else {
            self.check_port_timeout(now);
            return;
        };
        let stream_ports = graph.input_ports(stream_node);
        if stream_ports.len() < hw.ports.len() {
            self.check_port_timeout(now);
            return;
        }
        let pairs = match match_channels(&hw.ports, &stream_ports) {
            Ok(p) => p,
            Err(ch) => {
                self.block_current(&format!("capture stream has no {ch} port"));
                return;
            }
        };
        enum Check {
            Ok,
            Pending,
            Failed(&'static str),
            Relink,
        }
        let check = match self.session.as_mut().and_then(|s| s.linked.as_mut()) {
            Some(l) if l.hw_node == hw.node_id && l.stream_node == stream_node && l.pairs == pairs => {
                let mut vanished = false;
                let mut all = true;
                for (i, (o, inp)) in pairs.iter().enumerate() {
                    let present = graph.has_link(*o, *inp);
                    vanished |= l.seen[i] && !present;
                    l.seen[i] |= present;
                    all &= present;
                }
                if all {
                    Check::Ok
                } else if vanished {
                    Check::Failed("link removed")
                } else if now - l.created > LINK_TIMEOUT {
                    Check::Failed("link did not appear")
                } else {
                    Check::Pending
                }
            }
            _ => Check::Relink,
        };
        match check {
            Check::Ok => self.link_attempts = 0,
            Check::Pending => {}
            Check::Failed(msg) => self.link_failed(msg),
            Check::Relink => {
                self.drop_links();
                self.create_links(hw.node_id, stream_node, pairs);
            }
        }
    }

    fn check_port_timeout(&mut self, now: Instant) {
        let created = self.session.as_ref().and_then(|s| s.stream.as_ref()).map(|s| s.created);
        if created.is_some_and(|c| now - c > PORT_TIMEOUT) {
            self.block_current("capture stream ports did not appear");
        }
    }

    fn create_links(&mut self, hw_node: u32, stream_node: u32, pairs: Vec<(u32, u32)>) {
        self.link_attempts += 1;
        self.link_gen += 1;
        let gen = self.link_gen;
        let Some(session) = self.session.as_mut() else { return };
        let mut links = Vec::with_capacity(pairs.len());
        let mut error = None;
        for (out_port, in_port) in &pairs {
            let props = properties! {
                "link.output.node" => hw_node.to_string(),
                "link.output.port" => out_port.to_string(),
                "link.input.node" => stream_node.to_string(),
                "link.input.port" => in_port.to_string(),
                "object.linger" => "false",
            };
            match session.core.create_object::<pw::link::Link>("link-factory", &props) {
                Ok(link) => {
                    let events = session.events.clone();
                    let listener = link
                        .add_listener_local()
                        .info(move |info| {
                            if let LinkState::Error(msg) = info.state() {
                                events.borrow_mut().push(Ev::LinkFailed {
                                    gen,
                                    message: msg.to_owned(),
                                });
                            }
                        })
                        .register();
                    links.push(OwnedLink {
                        _listener: listener,
                        link,
                    });
                }
                Err(e) => {
                    error = Some(format!("link-factory: {e}"));
                    break;
                }
            }
        }
        if let Some(e) = error {
            drop(links);
            self.link_failed(&e);
            return;
        }
        log::info!("bypass: linking node {hw_node} -> {stream_node} ({} ports)", pairs.len());
        session.links = links;
        session.linked = Some(Linked {
            gen,
            hw_node,
            stream_node,
            seen: vec![false; pairs.len()],
            pairs,
            created: Instant::now(),
        });
        self.shared.discard_frames.store(WARMUP_FRAMES, Ordering::Relaxed);
    }

    fn link_failed(&mut self, msg: &str) {
        log::warn!("bypass link problem: {msg}");
        self.drop_links();
        if self.link_attempts >= MAX_LINK_ATTEMPTS {
            self.block_current(&format!("links could not be made ({msg})"));
        }
    }

    fn stream_failed(&mut self, msg: &str) {
        log::warn!("capture stream failed: {msg}");
        let was_bypass = self
            .session
            .as_ref()
            .and_then(|s| s.stream.as_ref())
            .is_some_and(|s| matches!(s.kind, StreamKind::Bypass { .. }));
        self.drop_links();
        self.drop_stream();
        self.stream_retry_at = Instant::now() + STREAM_RETRY_DELAY;
        let msg = format!("capture stream failed: {msg}");
        if was_bypass {
            self.block_current(&msg);
        } else {
            self.set_status(|s| s.fallback_reason = Some(msg));
        }
    }

    /// Falls back to processed for the currently resolved hardware node.
    fn block_current(&mut self, detail: &str) {
        let name = self
            .session
            .as_ref()
            .and_then(|s| resolve_hardware_source(&s.graph).ok())
            .map(|hw| hw.name);
        let reason = bypass_reason(detail);
        log::warn!("{reason}");
        self.drop_links();
        if let Some(name) = name {
            self.blocked = Some((name, reason.clone()));
        }
        self.set_status(|s| s.fallback_reason = Some(reason));
    }

    fn drop_links(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.linked = None;
            let links = std::mem::take(&mut session.links);
            drop(links);
        }
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
        self.drop_links();
        self.drop_stream();
        self.shared.hw_node_id.store(NO_NODE, Ordering::Relaxed);
        self.session = None;
    }

    fn set_status(&self, f: impl FnOnce(&mut StatusCore)) {
        let mut s = self.shared.status.lock().unwrap();
        s.requested = self.requested;
        f(&mut s);
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
    use crate::graph::{Direction, Node, Port};

    const HW: &str = "alsa_input.usb-mic";

    /// A plain two-channel device node as the default source (USB mic topology).
    fn device_graph() -> Graph {
        let mut g = Graph::default();
        g.nodes.insert(
            7,
            Node {
                id: 7,
                name: Some(HW.into()),
                description: Some("USB Mic".into()),
                media_class: Some("Audio/Source".into()),
                ..Node::default()
            },
        );
        for (i, (id, ch)) in [(10u32, "AUX0"), (11, "AUX1")].into_iter().enumerate() {
            g.ports.insert(
                id,
                Port {
                    id,
                    node_id: 7,
                    direction: Direction::Out,
                    channel: Some(ch.into()),
                    index: i as u32,
                    monitor: false,
                },
            );
        }
        g.default_source = Some(HW.into());
        g
    }

    #[test]
    fn processed_request_never_uses_bypass() {
        assert_eq!(
            choose_plan(MicMode::Processed, &device_graph(), None),
            Plan::Processed { reason: None }
        );
    }

    #[test]
    fn bypass_plan_targets_the_resolved_hardware_node() {
        let plan = choose_plan(MicMode::Bypass, &device_graph(), None);
        let Plan::Bypass(hw) = plan else {
            panic!("expected a bypass plan, got {plan:?}");
        };
        assert_eq!(hw.node_id, 7);
        assert_eq!(hw.name, HW);
        assert_eq!(hw.channels(), vec!["AUX0".to_string(), "AUX1".to_string()]);
        // The link target is the hardware node itself, not whatever the default source names.
        assert_eq!(hw.ports.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![10, 11]);
    }

    #[test]
    fn sticky_bypass_failure_only_applies_to_the_same_hardware_node() {
        let blocked = Some((
            HW.to_string(),
            bypass_reason("links could not be made (link removed)"),
        ));
        assert_eq!(
            choose_plan(MicMode::Bypass, &device_graph(), blocked.as_ref()),
            Plan::Processed {
                reason: Some(bypass_reason("links could not be made (link removed)"))
            }
        );

        // A different device resolved (unplug/replug): bypass is attempted again.
        let mut other = device_graph();
        other.nodes.get_mut(&7).unwrap().name = Some("alsa_input.usb-mic-2".into());
        other.default_source = Some("alsa_input.usb-mic-2".into());
        assert!(matches!(
            choose_plan(MicMode::Bypass, &other, blocked.as_ref()),
            Plan::Bypass(_)
        ));
    }

    #[test]
    fn unresolvable_hardware_falls_back_with_a_reason() {
        let mut g = device_graph();
        g.default_source = None;
        match choose_plan(MicMode::Bypass, &g, None) {
            Plan::Processed { reason: Some(reason) } => {
                assert_eq!(reason, "Bypass unavailable: no default source — using processed mic");
            }
            other => panic!("expected a reasoned fallback, got {other:?}"),
        }
    }

    #[test]
    fn status_derivation() {
        let core = StatusCore {
            requested: MicMode::Bypass,
            active: MicMode::Processed,
            source_description: Some("USB Mic".into()),
            fallback_reason: Some(bypass_reason("no default source")),
            filters_active_for_others: Some(true),
        };
        let status = derive_status(&core, true, false);
        assert_eq!(
            status,
            CaptureStatus {
                requested: MicMode::Bypass,
                active: MicMode::Processed,
                source_description: Some("USB Mic".into()),
                fallback_reason: Some(bypass_reason("no default source")),
                filters_active_for_others: Some(true),
                muted: true,
                healthy: false,
            }
        );
        // Live facts are not stored in the core.
        let healthy = derive_status(&core, false, true);
        assert_eq!((healthy.muted, healthy.healthy), (false, true));
        assert_eq!(healthy.fallback_reason, core.fallback_reason);
    }
}
