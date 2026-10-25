//! Normal mode: the overlay run loop (SPEC §3, §3.1, §4, §6.2).
//!
//! Three threads: the PipeWire capture thread inside [`Capture`], the analysis
//! thread from [`crate::analysis`], and this one, which owns every OpenVR and
//! Vulkan object. The main loop blocks in one `poll()` whose timeout is the
//! nearest deadline (SteamVR retry, config watch, capture status, controller
//! re-resolve, HUD animation frame) and is woken early by signals or by the
//! analysis thread. Nothing is rendered unless an overlay is visible and its
//! renderer reports changed pixels.

use std::error::Error;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use fvp_audio::{Capture, CaptureStatus, MicMode, Tone, CAPTURE_NODE_NAME};
use fvp_config::{Config, Placement, Watcher};
use fvp_openvr::{
    cross, dot, is_steamvr_running, normalize, sub, ControllerRole, Event, FrameRef, InputMethod,
    Mat34, MouseButton, OverlayFlag, OverlayHandle, TextureSubmitter, Vr, HMD_INDEX,
};
use fvp_pitch::{Class, PitchState};
use fvp_ui::{
    hud_overlay_width_m, vocal_rest_toast, Action, Hud, HudState, HudView, LiveInfo, SettingsPanel,
    SETTINGS_HEIGHT_PX, SETTINGS_WIDTH_PX,
};
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

use crate::analysis::{self, Analysis, Control};
use crate::{autostart, exit, instance, signals, VERSION_LINE};

/// Overlay key of the HUD (SPEC §4.1).
pub const HUD_KEY: &str = "codeyumx.frame-vocal-pitch.hud";
/// Overlay key of the dashboard settings panel (SPEC §4.2).
pub const SETTINGS_KEY: &str = "codeyumx.frame-vocal-pitch.settings";
/// Dashboard title bar text.
const SETTINGS_NAME: &str = "Vocal Pitch";
/// Dashboard panel width in metres (SPEC §4.2: 1200 × 800 px at ~2.8 m width).
const DASHBOARD_WIDTH_M: f32 = 2.8;
/// HUD sort order: above normal scene overlays, which keep the default 0.
const HUD_SORT_ORDER: u32 = 100;

/// How often SteamVR is probed while it is not running (SPEC §3.1).
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
/// Config file watcher cadence (SPEC §6.1: stat poll at 2 Hz).
const CONFIG_INTERVAL: Duration = Duration::from_millis(500);
/// Capture status (= mute) poll cadence.
const STATUS_INTERVAL: Duration = Duration::from_millis(200);
/// Controller re-resolve cadence (SPEC §4.1: every 1 s, plus device events).
const CONTROLLER_INTERVAL: Duration = Duration::from_secs(1);
/// HUD frame cadence while a colour transition runs.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
/// `no_mic` after this long without buffers (SPEC §4.1).
const NO_MIC_AFTER: Duration = Duration::from_millis(3_000);
/// How long a vocal-rest toast stays on the HUD.
const TOAST_HOLD: Duration = Duration::from_secs(8);
/// Autostart state cache refresh.
const AUTOSTART_INTERVAL: Duration = Duration::from_secs(60);
/// Capture ring buffer length in seconds.
const RING_SECONDS: usize = 2;
/// Alpha changes smaller than this are not sent to the runtime.
const ALPHA_STEP: f32 = 0.01;

/// `head` placement offset (SPEC §4.1), before the config nudges.
const HEAD_OFFSET: [f32; 3] = [-0.12, -0.10, -0.6];
/// Wrist placement offset in controller space: just back from the grip, above the top face.
const WRIST_ORIGIN: [f32; 3] = [0.0, -0.04, -0.03];
/// The panel faces up from the back of the wrist: its normal is the controller's −Z.
const WRIST_NORMAL: [f32; 3] = [0.0, 0.0, -1.0];
/// Text-up direction on the wrist panel: along the handle, toward the fingers.
const WRIST_UP: [f32; 3] = [0.0, 1.0, 0.0];
/// Alpha starts fading here (angle between panel normal and the HMD direction).
const FADE_START_DEG: f32 = 60.0;
/// …and reaches 0 here (SPEC §4.1: beyond 75°).
const FADE_END_DEG: f32 = 75.0;

/// Runs normal mode until a signal, a SteamVR quit or a Quit action ends it.
pub fn run() -> Result<u8, Box<dyn Error>> {
    let lock_path = instance::lock_path();
    let _lock = match instance::acquire(&lock_path)? {
        instance::Acquired::Primary(lock) => lock,
        instance::Acquired::Secondary(pid) => {
            instance::signal_toggle(pid)?;
            log::info!("already running as PID {pid}; sent toggle request");
            return Ok(exit::OK);
        }
    };
    let signal_handles = signals::Signals::install()?;

    let config_path = fvp_config::default_path();
    let cfg = fvp_config::load_or_init(&config_path);
    log::info!("{VERSION_LINE} started (config {})", config_path.display());

    let (producer, consumer) =
        rtrb::RingBuffer::new(fvp_audio::SAMPLE_RATE_HZ as usize * RING_SECONDS);
    let capture = Capture::start(mic_mode(cfg.audio.mic_mode), producer);
    log::debug!("capture stream {CAPTURE_NODE_NAME} requested in {:?}", cfg.audio.mic_mode);
    let muted = capture.status().muted;
    let (analysis, wake_rx) = Analysis::spawn(consumer, Control::from_config(&cfg, muted, false))?;

    let app = App {
        cfg,
        watcher: Watcher::new(&config_path),
        capture,
        analysis,
        wake_rx,
        hud: Hud::new(),
        panel: SettingsPanel::new(),
        session: None,
        retry_at: Instant::now(),
        config_at: Instant::now(),
        status_at: Instant::now(),
        autostart_at: Instant::now(),
        status: None,
        unhealthy_since: None,
        muted,
        control_paused: false,
        state: HudState::NoVoice,
        f0_hz: None,
        toast: None,
        controller: None,
        controller_at: Instant::now(),
        tone: ToneState::Idle,
        autostart: autostart::Autostart::new(),
        autostart_enabled: false,
        quit: false,
    };
    app.run_loop(&signal_handles)
}

fn mic_mode(mode: fvp_config::MicMode) -> MicMode {
    match mode {
        fvp_config::MicMode::Bypass => MicMode::Bypass,
        fvp_config::MicMode::Processed => MicMode::Processed,
    }
}

/// The HUD transform/properties last sent to the runtime.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Applied {
    placement: Placement,
    device: u32,
    nudge: [f32; 3],
    size_m: f32,
    alpha: f32,
}

/// Live OpenVR session; exists only while SteamVR runs.
struct Session {
    vr: Vr,
    submitter: TextureSubmitter,
    hud: OverlayHandle,
    dashboard: OverlayHandle,
    applied: Option<Applied>,
    shown: bool,
    dashboard_active: bool,
}

/// Reminder tone lifecycle (SPEC §3: the playback stream is created on demand).
enum ToneState {
    /// Nothing to play; no stream.
    Idle,
    /// Being opened on a helper thread (PipeWire may take a moment).
    Loading(Receiver<Result<Tone, String>>),
    Ready(Tone),
    /// Opening failed; stays until the reminder is turned off and on again.
    Failed,
}

struct App {
    cfg: Config,
    watcher: Watcher,
    capture: Capture,
    analysis: Analysis,
    wake_rx: UnixStream,
    hud: Hud,
    panel: SettingsPanel,
    session: Option<Session>,
    retry_at: Instant,
    config_at: Instant,
    status_at: Instant,
    autostart_at: Instant,
    status: Option<CaptureStatus>,
    unhealthy_since: Option<Instant>,
    muted: bool,
    /// Analysis is currently paused (nothing visible, no vocal-rest timer).
    control_paused: bool,
    state: HudState,
    f0_hz: Option<f32>,
    toast: Option<(String, Instant)>,
    /// Wrist controller resolved for the current placement.
    controller: Option<(ControllerRole, u32)>,
    controller_at: Instant,
    tone: ToneState,
    autostart: autostart::Autostart,
    autostart_enabled: bool,
    /// The user chose Quit in the dashboard.
    quit: bool,
}

impl App {
    fn run_loop(mut self, signal_handles: &signals::Signals) -> Result<u8, Box<dyn Error>> {
        self.apply_config();
        self.refresh_autostart(Instant::now());

        loop {
            let pending = signal_handles.take();
            if pending.terminate {
                log::info!("termination requested; shutting down");
                break;
            }
            for _ in 0..pending.toggles {
                self.toggle();
            }

            analysis::drain(&self.wake_rx);
            let now = Instant::now();

            if now >= self.status_at {
                self.refresh_status(now);
            }
            if now >= self.config_at {
                self.poll_config(now);
            }
            if now >= self.autostart_at {
                self.refresh_autostart(now);
            }
            self.poll_tone();

            let snapshot = self.analysis.take();
            if let Some(minutes) = snapshot.toast_minutes {
                log::info!("vocal rest: {minutes} min voiced in the last hour");
                self.toast = Some((vocal_rest_toast(minutes), now + TOAST_HOLD));
            }
            if snapshot.reminder_due {
                self.play_tone();
            }

            if self.session.is_none() && now >= self.retry_at {
                self.connect(now);
            }
            if self.session.is_some() {
                self.pump_events(now);
                self.update_state(now, snapshot.state);
                self.drive_hud(now);
                self.drive_dashboard();
            } else {
                self.update_state(now, snapshot.state);
            }
            self.analysis.set_live(self.live());
            let paused = self.paused();
            if paused != self.control_paused {
                self.control_paused = paused;
                self.refresh_control();
            }

            if self.quit {
                log::info!("quit requested from the dashboard");
                break;
            }
            let deadline = self.next_deadline();
            self.wait(signal_handles, deadline)?;
        }

        self.teardown();
        Ok(if self.quit { exit::QUIT } else { exit::OK })
    }

    /// Whether an overlay currently shows live pitch data, so per-hop F0 changes
    /// are worth waking the main loop for.
    fn live(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.shown || session.dashboard_active)
    }

    // ---- Waiting ------------------------------------------------------------------------

    /// Nearest moment the loop has work to do.
    fn next_deadline(&self) -> Instant {
        let mut deadline = self.config_at.min(self.status_at).min(self.autostart_at);
        match &self.session {
            None => deadline.min(self.retry_at),
            Some(_) => {
                deadline = deadline.min(self.controller_at);
                if self.hud.animating() {
                    deadline = deadline.min(Instant::now() + ANIMATION_INTERVAL);
                }
                deadline
            }
        }
    }

    /// Blocks until a signal, the analysis thread or the deadline arrives.
    fn wait(&self, signal_handles: &signals::Signals, deadline: Instant) -> io::Result<()> {
        let fds = signal_handles.fds();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut poll_fds = [
                PollFd::new(fds[0], PollFlags::POLLIN),
                PollFd::new(fds[1], PollFlags::POLLIN),
                PollFd::new(self.wake_rx.as_fd(), PollFlags::POLLIN),
            ];
            let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
            match poll(&mut poll_fds, timeout) {
                Ok(_) => return Ok(()),
                Err(Errno::EINTR) => {
                    if remaining.is_zero() {
                        return Ok(());
                    }
                }
                Err(e) => return Err(io::Error::from(e)),
            }
        }
    }

    // ---- Configuration ------------------------------------------------------------------

    /// Applies the current configuration to the subsystems that cache it.
    fn apply_config(&mut self) {
        self.capture.set_mode(mic_mode(self.cfg.audio.mic_mode));
        self.control_paused = self.paused();
        self.refresh_control();
        self.sync_tone();
    }

    fn refresh_control(&self) {
        self.analysis.set_control(Control::from_config(
            &self.cfg,
            self.muted,
            self.control_paused,
        ));
    }

    /// No overlay shows pitch and the vocal-rest timer is off (SPEC §4.1).
    fn paused(&self) -> bool {
        let showing = self
            .session
            .as_ref()
            .is_some_and(|session| session.shown || session.dashboard_active);
        !(showing || self.cfg.vocal_rest.minutes > 0)
    }

    fn poll_config(&mut self, now: Instant) {
        self.config_at = now + CONFIG_INTERVAL;
        if let Some(cfg) = self.watcher.poll() {
            log::info!("configuration reloaded from disk");
            self.cfg = cfg;
            self.apply_config();
            self.panel.invalidate();
        }
    }

    /// `SIGUSR1` or a second launch: toggle the HUD and open the dashboard tab (§6.2).
    fn toggle(&mut self) {
        self.cfg.hud.enabled = !self.cfg.hud.enabled;
        log::info!("HUD {}", if self.cfg.hud.enabled { "on" } else { "off" });
        if let Err(e) = self.watcher.save(&self.cfg) {
            log::warn!("cannot save the configuration: {e}");
        }
        self.apply_config();
        self.panel.invalidate();
        if let Some(session) = &self.session {
            if let Err(e) = session.vr.show_dashboard(SETTINGS_KEY) {
                log::debug!("ShowDashboard failed: {e}");
            }
        }
    }

    // ---- Capture status -----------------------------------------------------------------

    fn refresh_status(&mut self, now: Instant) {
        self.status_at = now + STATUS_INTERVAL;
        let status = self.capture.status();
        if status.muted != self.muted {
            self.muted = status.muted;
            log::info!(
                "microphone {}",
                if status.muted { "muted" } else { "unmuted" }
            );
            self.refresh_control();
        }
        if status.healthy {
            self.unhealthy_since = None;
        } else if self.unhealthy_since.is_none() {
            self.unhealthy_since = Some(now);
        }
        self.status = Some(status);
    }

    /// Maps capture state and the newest pitch state onto the HUD state (SPEC §4.1).
    fn update_state(&mut self, now: Instant, pitch: Option<PitchState>) {
        let no_mic = self
            .unhealthy_since
            .is_some_and(|since| now.saturating_duration_since(since) >= NO_MIC_AFTER);
        let (state, f0) = if self.muted {
            (HudState::Muted, None)
        } else if no_mic {
            (HudState::NoMic, None)
        } else {
            match pitch {
                Some(pitch) => (
                    match pitch.class {
                        Class::InRange => HudState::InRange,
                        Class::OutOfRange => HudState::OutOfRange,
                        Class::NoVoice => HudState::NoVoice,
                    },
                    pitch.f0_hz,
                ),
                None => (HudState::NoVoice, None),
            }
        };
        self.state = state;
        self.f0_hz = f0.filter(|hz| hz.is_finite());
    }

    // ---- Reminder tone ------------------------------------------------------------------

    fn sync_tone(&mut self) {
        if self.cfg.reminder.enabled {
            if matches!(&self.tone, ToneState::Idle) {
                // Opening a PipeWire stream must not block the main loop.
                let (tx, rx) = mpsc::channel();
                std::thread::Builder::new()
                    .name("fvp-tone-open".into())
                    .spawn(move || {
                        let _ = tx.send(Tone::new());
                    })
                    .ok();
                self.tone = ToneState::Loading(rx);
                log::debug!("opening the reminder tone stream");
            }
        } else if !matches!(&self.tone, ToneState::Idle) {
            self.tone = ToneState::Idle;
        }
    }

    fn poll_tone(&mut self) {
        let mut outcome = None;
        if let ToneState::Loading(rx) = &self.tone {
            match rx.try_recv() {
                Ok(result) => outcome = Some(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    outcome = Some(Err("tone thread did not report".to_owned()));
                }
            }
        }
        match outcome {
            Some(Ok(tone)) => self.tone = ToneState::Ready(tone),
            Some(Err(e)) => {
                log::warn!("reminder tone unavailable: {e}");
                self.tone = ToneState::Failed;
            }
            None => {}
        }
    }

    fn play_tone(&mut self) {
        if let ToneState::Ready(tone) = &self.tone {
            tone.play(
                self.cfg.reminder.pitch_hz,
                self.cfg.reminder.volume,
                analysis::TONE_MS,
            );
            log::debug!("reminder tone at {} Hz", self.cfg.reminder.pitch_hz);
        }
    }

    // ---- Autostart ----------------------------------------------------------------------

    fn refresh_autostart(&mut self, now: Instant) {
        self.autostart_at = now + AUTOSTART_INTERVAL;
        match self.autostart.is_enabled() {
            Ok(enabled) => self.autostart_enabled = enabled,
            Err(e) => log::debug!("autostart state unknown: {e}"),
        }
    }

    // ---- OpenVR session -----------------------------------------------------------------

    /// The §3.1 retry state: probe, then initialise as an overlay application.
    fn connect(&mut self, now: Instant) {
        self.retry_at = now + RETRY_INTERVAL;
        match is_steamvr_running() {
            Ok(true) => {}
            Ok(false) => {
                log::info!("SteamVR is not running; retrying in 3 s");
                return;
            }
            Err(e) => {
                log::info!("SteamVR unavailable ({e}); retrying in 3 s");
                return;
            }
        }
        match self.open_session() {
            Ok(session) => {
                log::info!(
                    "overlays created (OpenVR runtime {})",
                    session.vr.runtime_version()
                );
                self.session = Some(session);
                // New overlays are blank: force both renderers to produce a first frame.
                self.hud = Hud::new();
                self.panel.invalidate();
                self.controller_at = Instant::now();
                self.resolve_controller(Instant::now());
            }
            Err(e) => log::warn!("cannot create the overlays ({e}); retrying in 3 s"),
        }
    }

    fn open_session(&mut self) -> Result<Session, fvp_openvr::Error> {
        let mut vr = Vr::init_overlay()?;
        let hud = vr.create_overlay(HUD_KEY, "Vocal Pitch HUD")?;
        vr.set_width(hud, hud_overlay_width_m(self.cfg.hud.size_m))?;
        vr.set_alpha(hud, self.cfg.hud.opacity)?;
        vr.set_sort_order(hud, HUD_SORT_ORDER)?;
        vr.set_input_method(hud, InputMethod::None)?;

        let (dashboard, thumbnail) = vr.create_dashboard_overlay(SETTINGS_KEY, SETTINGS_NAME)?;
        vr.set_width(dashboard, DASHBOARD_WIDTH_M)?;
        vr.set_input_method(dashboard, InputMethod::Mouse)?;
        vr.set_mouse_scale(dashboard, SETTINGS_WIDTH_PX as f32, SETTINGS_HEIGHT_PX as f32)?;
        vr.set_flag(dashboard, OverlayFlag::SendVRSmoothScrollEvents, true)?;
        match icon_path() {
            Some(path) => {
                if let Err(e) = vr.set_thumbnail_from_file(thumbnail, &path) {
                    log::warn!("cannot set the dashboard icon {}: {e}", path.display());
                }
            }
            None => log::info!("no 256 px icon found; the dashboard thumbnail stays empty"),
        }

        // Created last so a failure above leaves no Vulkan objects behind.
        let submitter = TextureSubmitter::new(&vr)?;
        Ok(Session {
            vr,
            submitter,
            hud,
            dashboard,
            applied: None,
            shown: false,
            dashboard_active: false,
        })
    }

    /// §3.1 teardown: clear textures, destroy overlays, wait, shut OpenVR down, then Vulkan.
    fn teardown(&mut self) {
        if let Some(session) = self.session.take() {
            log::info!("closing the OpenVR session");
            let Session { vr, submitter, .. } = session;
            vr.shutdown(Some(submitter));
        }
    }

    /// Polls OpenVR events and maps them onto the overlays and the settings panel.
    fn pump_events(&mut self, now: Instant) {
        let mut events = Vec::new();
        let mut actions = Vec::new();
        let mut restart = false;
        let mut resolve = false;
        {
            let Some(session) = self.session.as_mut() else {
                return;
            };
            session.vr.poll_events_into(&mut events);
            for event in events {
                match event {
                    Event::Quit => {
                        restart = true;
                        break;
                    }
                    Event::MouseMove { overlay, x, y } if overlay == session.dashboard => {
                        self.panel.pointer_move(x, y);
                    }
                    Event::MouseDown {
                        overlay,
                        x,
                        y,
                        button: MouseButton::Left,
                    } if overlay == session.dashboard => {
                        if let Some(action) = self.panel.click(x, y) {
                            actions.push(action);
                        }
                    }
                    Event::Scroll { overlay, dy, .. } if overlay == session.dashboard => {
                        self.panel.scroll(dy);
                    }
                    Event::OverlayShown(overlay) if overlay == session.dashboard => {
                        self.panel.invalidate();
                    }
                    Event::OverlayHidden(overlay) if overlay == session.dashboard => {
                        self.panel.pointer_leave();
                        session.dashboard_active = false;
                    }
                    Event::DeviceConnected(_)
                    | Event::DeviceDisconnected(_)
                    | Event::DeviceRoleChanged(_) => resolve = true,
                    _ => {}
                }
            }
        }
        if resolve {
            self.resolve_controller(now);
        }
        if restart {
            log::info!("SteamVR asked the overlays to quit; returning to the retry state");
            self.teardown();
            self.retry_at = Instant::now() + RETRY_INTERVAL;
            return;
        }
        for action in actions {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::SetConfig(new) => {
                let mut new = new;
                new.clamp();
                if let Err(e) = self.watcher.save(&new) {
                    log::warn!("cannot save the configuration: {e}");
                }
                self.cfg = new;
                self.apply_config();
                self.panel.invalidate();
            }
            Action::SetAutostart(enabled) => {
                match self.autostart.set_enabled(enabled) {
                    Ok(()) => self.autostart_enabled = enabled,
                    Err(e) => log::warn!("autostart: {e}"),
                }
                self.panel.invalidate();
            }
            Action::Quit => self.quit = true,
        }
    }

    /// Resolves the wrist controller for the configured placement (SPEC §4.1).
    fn resolve_controller(&mut self, now: Instant) {
        self.controller_at = now + CONTROLLER_INTERVAL;
        let role = match self.cfg.hud.placement {
            Placement::Head => None,
            Placement::LeftWrist => Some(ControllerRole::LeftHand),
            Placement::RightWrist => Some(ControllerRole::RightHand),
        };
        let index = match (&self.session, role) {
            (Some(session), Some(role)) => session
                .vr
                .controller_index(role)
                .filter(|&device| session.vr.is_device_connected(device)),
            _ => None,
        };
        let resolved = role.zip(index);
        if resolved != self.controller {
            match resolved {
                Some((_, device)) => log::info!("HUD follows controller device {device}"),
                None => log::debug!("no controller for the wrist placement"),
            }
            self.controller = resolved;
        }
    }

    // ---- Rendering ----------------------------------------------------------------------

    fn drive_hud(&mut self, now: Instant) {
        let placement = self.cfg.hud.placement;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let device = match placement {
            Placement::Head => HMD_INDEX,
            _ => match self.controller {
                Some((role, device))
                    if (role == ControllerRole::LeftHand)
                        == (placement == Placement::LeftWrist) =>
                {
                    device
                }
                _ => {
                    set_hud_shown(session, false);
                    return;
                }
            },
        };
        let visible = self.cfg.hud.enabled && session.vr.hmd_worn();
        set_hud_shown(session, visible);
        if !visible {
            return;
        }

        let nudge = [
            self.cfg.hud.nudge_x_m,
            self.cfg.hud.nudge_y_m,
            self.cfg.hud.nudge_z_m,
        ];
        let mut alpha = self.cfg.hud.opacity;
        if placement != Placement::Head {
            alpha *= wrist_fade(&session.vr, device);
        }
        let size_m = self.cfg.hud.size_m;
        let placed = session
            .applied
            .is_none_or(|old| (old.placement, old.device, old.nudge, old.size_m) != (placement, device, nudge, size_m));
        if placed {
            let transform = match placement {
                Placement::Head => Mat34::translation(
                    HEAD_OFFSET[0] + nudge[0],
                    HEAD_OFFSET[1] + nudge[1],
                    HEAD_OFFSET[2] + nudge[2],
                ),
                _ => wrist_transform(nudge),
            };
            if let Err(e) =
                session
                    .vr
                    .set_transform_tracked_device_relative(session.hud, device, &transform)
            {
                log::warn!("HUD transform: {e}");
            }
            if let Err(e) = session.vr.set_width(session.hud, hud_overlay_width_m(size_m)) {
                log::warn!("HUD width: {e}");
            }
            session.applied = Some(Applied {
                placement,
                device,
                nudge,
                size_m,
                // NaN so that the first alpha update is always sent.
                alpha: f32::NAN,
            });
        }
        // Throttle alpha changes so a wrist swing does not spam the runtime.
        let sent_alpha = session.applied.map_or(f32::NAN, |old| old.alpha);
        if (sent_alpha - alpha).abs() >= ALPHA_STEP || sent_alpha.is_nan() {
            if let Err(e) = session.vr.set_alpha(session.hud, alpha) {
                log::warn!("HUD alpha: {e}");
            }
            session.applied = session.applied.map(|old| Applied { alpha, ..old });
        }
        // Fully transparent: nothing worth uploading.
        if alpha <= 0.0 {
            return;
        }

        let view = HudView {
            state: self.state,
            f0_hz: self.f0_hz,
            band: self.cfg.band(),
            span: (self.cfg.audio.detect_min_hz, self.cfg.audio.detect_max_hz),
            display: self.cfg.hud.display,
            fade_ms: self.cfg.hud.fade_ms,
            toast: self
                .toast
                .as_ref()
                .filter(|(_, until)| now < *until)
                .map(|(text, _)| text.clone()),
        };
        if let Some(frame) = self.hud.render(&view, now) {
            let frame = FrameRef {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
                premultiplied: frame.premultiplied,
            };
            if let Err(e) = session.submitter.submit(&mut session.vr, session.hud, &frame) {
                log::warn!("HUD texture upload failed: {e}");
            }
        }
    }

    fn drive_dashboard(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let visible = session.vr.is_dashboard_visible()
            && session.vr.is_active_dashboard_overlay(session.dashboard);
        if !visible {
            if session.dashboard_active {
                session.dashboard_active = false;
                self.panel.pointer_leave();
            }
            return;
        }
        if !session.dashboard_active {
            session.dashboard_active = true;
            self.panel.invalidate();
        }
        let live = LiveInfo {
            f0_hz: self.f0_hz,
            state: self.state,
            mic_status_line: status_line(self.status.as_ref()),
            autostart: self.autostart_enabled,
        };
        if let Some(frame) = self.panel.render(&self.cfg, &live) {
            let frame = FrameRef {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
                premultiplied: frame.premultiplied,
            };
            if let Err(e) = session
                .submitter
                .submit(&mut session.vr, session.dashboard, &frame)
            {
                log::warn!("settings texture upload failed: {e}");
            }
        }
    }
}

/// Shows or hides the HUD overlay, remembering what the runtime was told.
fn set_hud_shown(session: &mut Session, shown: bool) {
    if session.shown == shown {
        return;
    }
    let result = if shown {
        session.vr.show(session.hud)
    } else {
        session.vr.hide(session.hud)
    };
    match result {
        Ok(()) => {
            session.shown = shown;
            log::debug!("HUD {}", if shown { "shown" } else { "hidden" });
        }
        Err(e) => log::warn!(
            "cannot {} the HUD: {e}",
            if shown { "show" } else { "hide" }
        ),
    }
}

/// Controller-relative transform of the wrist panel (SPEC §4.1).
///
/// The visible face (+Z) points along the controller's −Z axis, "up from the back of the
/// wrist"; the text runs along the handle (+Y) toward the fingers.
fn wrist_transform(nudge: [f32; 3]) -> Mat34 {
    let z = WRIST_NORMAL;
    let y = WRIST_UP;
    let x = cross(y, z);
    Mat34::from_basis(
        x,
        y,
        z,
        [
            WRIST_ORIGIN[0] + nudge[0],
            WRIST_ORIGIN[1] + nudge[1],
            WRIST_ORIGIN[2] + nudge[2],
        ],
    )
}

/// 1 while the panel faces the HMD, falling to 0 past [`FADE_END_DEG`].
fn wrist_fade(vr: &Vr, device: u32) -> f32 {
    let (Some(panel), Some(hmd)) = (vr.device_pose(device), vr.device_pose(HMD_INDEX)) else {
        return 1.0;
    };
    let origin = panel.transform_point(WRIST_ORIGIN);
    let normal = normalize(panel.transform_vector(WRIST_NORMAL));
    let towards_hmd = normalize(sub(hmd.origin(), origin));
    if normal == [0.0; 3] || towards_hmd == [0.0; 3] {
        return 1.0;
    }
    let degrees = dot(normal, towards_hmd)
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees();
    fade_for_degrees(degrees)
}

/// 1 up to [`FADE_START_DEG`], 0 from [`FADE_END_DEG`] on, linear between.
fn fade_for_degrees(degrees: f32) -> f32 {
    let t = ((degrees - FADE_START_DEG) / (FADE_END_DEG - FADE_START_DEG)).clamp(0.0, 1.0);
    1.0 - t
}

/// §6.3 status line: mode, node recorded from, fallback reason, filter state.
fn status_line(status: Option<&CaptureStatus>) -> String {
    let Some(status) = status else {
        return "Microphone: starting".to_owned();
    };
    let mut parts = Vec::with_capacity(4);
    parts.push(match (status.active, status.requested) {
        (MicMode::Bypass, _) => "Bypass".to_owned(),
        (MicMode::Processed, MicMode::Bypass) => "Processed (bypass unavailable)".to_owned(),
        (MicMode::Processed, MicMode::Processed) => "Processed".to_owned(),
    });
    let source = status.source_description.as_deref().unwrap_or("no source");
    parts.push(match status.active {
        MicMode::Bypass => format!("recording from {source} (hardware)"),
        MicMode::Processed => format!("recording from {source}"),
    });
    if status.muted {
        parts.push("mic muted".to_owned());
    }
    if let Some(reason) = &status.fallback_reason {
        parts.push(reason.clone());
    }
    parts.push(
        match status.filters_active_for_others {
            Some(true) => "Valve filters active for other apps",
            Some(false) => "Valve filters inactive for other apps",
            None => "Valve mic filters not present",
        }
        .to_owned(),
    );
    parts.join(" \u{00b7} ")
}

/// 256 px dashboard icon: the installed hicolor icon, then one next to the executable.
fn icon_path() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(installed) = data_home
        .map(|dir| dir.join("icons/hicolor/256x256/apps/frame-vocal-pitch.png"))
        .filter(|path| path.is_file())
    {
        return Some(installed);
    }
    let next_to_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("assets/icons/256.png")));
    next_to_exe.filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(active: MicMode, requested: MicMode) -> CaptureStatus {
        CaptureStatus {
            requested,
            active,
            source_description: Some("Internal Microphone".into()),
            fallback_reason: None,
            filters_active_for_others: Some(false),
            muted: false,
            healthy: true,
        }
    }

    #[test]
    fn status_line_matches_the_spec_example() {
        assert_eq!(
            status_line(Some(&status(MicMode::Bypass, MicMode::Bypass))),
            "Bypass \u{00b7} recording from Internal Microphone (hardware) \u{00b7} \
             Valve filters inactive for other apps"
        );
    }

    #[test]
    fn status_line_reports_fallback_and_mute() {
        let mut fallback = status(MicMode::Processed, MicMode::Bypass);
        fallback.fallback_reason =
            Some("Bypass unavailable: hardware mic node not found — using processed mic".into());
        fallback.muted = true;
        fallback.filters_active_for_others = Some(true);
        let line = status_line(Some(&fallback));
        assert!(line.starts_with("Processed (bypass unavailable)"), "{line}");
        assert!(line.contains("recording from Internal Microphone \u{00b7}"), "{line}");
        assert!(line.contains("mic muted"), "{line}");
        assert!(line.contains("using processed mic"), "{line}");
        assert!(
            line.ends_with("Valve filters active for other apps"),
            "{line}"
        );
        assert_eq!(status_line(None), "Microphone: starting");
    }

    #[test]
    fn head_placement_uses_the_spec_offsets() {
        let transform = Mat34::translation(
            HEAD_OFFSET[0] + 0.01,
            HEAD_OFFSET[1],
            HEAD_OFFSET[2] - 0.02,
        );
        assert_eq!(transform.origin(), [-0.11, -0.10, -0.62]);
        assert_eq!(transform.m[0][0], 1.0);
    }

    #[test]
    fn wrist_panel_faces_up_and_is_right_handed() {
        let transform = wrist_transform([0.0, 0.0, 0.0]);
        let m = transform.m;
        let (x, y, z) = ([m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [
            m[0][2], m[1][2], m[2][2],
        ]);
        assert_eq!(z, WRIST_NORMAL);
        assert_eq!(y, WRIST_UP);
        assert_eq!(cross(x, y), z, "not a right-handed basis");
        assert_eq!(dot(x, x), 1.0);
        assert_eq!(transform.origin(), WRIST_ORIGIN);
    }

    #[test]
    fn icon_never_returns_a_missing_path() {
        if let Some(path) = icon_path() {
            assert!(path.is_file(), "{}", path.display());
        }
    }

    #[test]
    fn wrist_fade_uses_the_documented_thresholds() {
        assert_eq!(fade_for_degrees(0.0), 1.0);
        assert_eq!(fade_for_degrees(FADE_START_DEG), 1.0);
        assert!((fade_for_degrees(67.5) - 0.5).abs() < 1e-6);
        assert_eq!(fade_for_degrees(FADE_END_DEG), 0.0);
        assert_eq!(fade_for_degrees(120.0), 0.0);
    }
}
