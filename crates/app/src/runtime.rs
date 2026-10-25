//! Normal mode: the overlay run loop (SPEC §3, §3.1, §4, §6.2).
//!
//! Three threads: the PipeWire capture thread inside [`Capture`], the analysis
//! thread from [`crate::analysis`], and this one, which owns every OpenVR and
//! Vulkan object (update checks add a short-lived `curl` thread, §7.1). The main
//! loop blocks in one `poll()` whose timeout is the nearest deadline (SteamVR
//! retry, config watch, capture status, HUD animation
//! frame, update schedule) and is woken early by signals, by the analysis thread
//! or by a finished update check. Nothing is rendered unless an overlay is
//! visible and its renderer reports changed pixels.

use std::error::Error;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use fvp_audio::{Capture, CaptureStatus, Tone, CAPTURE_NODE_NAME};
use fvp_config::{Config, Watcher};
use fvp_openvr::{
    is_steamvr_running, Event, FrameRef, InputMethod, Mat34, MouseButton, OverlayFlag, OverlayHandle,
    TextureSubmitter, Vr, HMD_INDEX,
};
use fvp_pitch::{Class, PitchState, ResonanceState};
use fvp_ui::{
    hud_overlay_width_m, vocal_rest_toast, Action, Hud, HudState, HudView, LiveInfo, ResonanceLive,
    ResonanceView, SettingsPanel, SETTINGS_HEIGHT_PX, SETTINGS_WIDTH_PX,
};
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

use crate::analysis::{self, Analysis, Control};
use crate::{autostart, exit, instance, signals, update, VERSION_LINE};

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

/// HUD offset relative to the HMD (SPEC §4.1), before the config nudges.
const HEAD_OFFSET: [f32; 3] = [-0.12, -0.10, -0.6];

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
    let capture = Capture::start(producer);
    log::debug!("capture stream {CAPTURE_NODE_NAME} requested");
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
        resonance: None,
        resonance_hz: None,
        toast: None,
        tone: ToneState::Idle,
        autostart: autostart::Autostart::new(),
        autostart_enabled: false,
        quit: false,
        updater: update::Updater::new()?,
    };
    app.run_loop(&signal_handles)
}

/// The HUD transform/properties last sent to the runtime.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Applied {
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
    thumbnail: OverlayHandle,
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
    /// Latest resonance state published by the analysis thread (§5.1).
    resonance: Option<ResonanceState>,
    /// Latest smoothed resonance reading in Hz (§5.1), shown on the resonance page.
    resonance_hz: Option<f32>,
    toast: Option<(String, Instant)>,
    tone: ToneState,
    autostart: autostart::Autostart,
    autostart_enabled: bool,
    /// The user chose Quit in the dashboard.
    quit: bool,
    /// Update checks and installs (§7.1).
    updater: update::Updater,
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
            self.updater.drain_wake();
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
            self.updater.tick(now, self.cfg.update.check);

            let snapshot = self.analysis.take();
            if let Some(minutes) = snapshot.toast_minutes {
                log::info!("vocal rest: {minutes} min voiced in the last hour");
                self.toast = Some((vocal_rest_toast(minutes), now + TOAST_HOLD));
            }
            if snapshot.reminder_due {
                self.play_tone();
            }
            self.resonance = snapshot.resonance;
            self.resonance_hz = snapshot.resonance_hz;

            if self.session.is_none() && now >= self.retry_at {
                self.connect(now);
            }
            if self.session.is_some() {
                self.pump_events();
                self.update_state(now, snapshot.state);
                self.drive_hud(now);
                self.drive_dashboard();
            } else {
                self.update_state(now, snapshot.state);
            }
            self.analysis.set_live(self.live());
            self.sync_paused();

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
        if let Some(update_at) = self.updater.next_deadline() {
            deadline = deadline.min(update_at);
        }
        match &self.session {
            None => deadline.min(self.retry_at),
            Some(session) => {
                if self.hud.animating() {
                    deadline = deadline.min(Instant::now() + ANIMATION_INTERVAL);
                }
                // The "Confirm update" button reverts after its window.
                if let Some(panel_at) = self.panel.next_deadline().filter(|_| session.dashboard_active) {
                    deadline = deadline.min(panel_at);
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
                PollFd::new(self.updater.wake_fd(), PollFlags::POLLIN),
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
        self.sync_paused();
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

    /// Nothing visible needs pitch and no other feature is waiting for it (SPEC §4.1, §5.1).
    fn paused(&self) -> bool {
        let showing = self
            .session
            .as_ref()
            .is_some_and(|session| session.shown || session.dashboard_active);
        !(showing
            || self.cfg.vocal_rest.minutes > 0
            || self.cfg.resonance.enabled)
    }

    /// Pushes the pause state to the analysis thread if it changed.
    fn sync_paused(&mut self) {
        let paused = self.paused();
        if paused != self.control_paused {
            self.control_paused = paused;
            self.refresh_control();
        }
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

    /// Capture has been unhealthy for long enough to call it a missing microphone.
    fn no_mic(&self, now: Instant) -> bool {
        self.unhealthy_since
            .is_some_and(|since| now.saturating_duration_since(since) >= NO_MIC_AFTER)
    }

    /// Maps capture state and the newest pitch state onto the HUD state (SPEC §4.1).
    fn update_state(&mut self, now: Instant, pitch: Option<PitchState>) {
        let no_mic = self.no_mic(now);
        let (state, f0) = if self.muted {
            (HudState::Muted, None)
        } else if no_mic {
            (HudState::NoMic, None)
        } else {
            match pitch {
                Some(pitch) => (
                    match pitch.class {
                        Class::InRange => HudState::InRange,
                        Class::OutOfRange => {
                            out_of_range_state(pitch.f0_hz, self.cfg.band(), self.cfg.target.near_hz, self.state)
                        }
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

    // ---- Resonance (§5.1) ---------------------------------------------------------------

    /// Indicator content for the HUD and the panel, or `None` while it is off.
    fn resonance_view(&self) -> Option<ResonanceView> {
        resonance_view(self.cfg.resonance.enabled, self.resonance)
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
        // SPEC §4.2: the control bar's close (×) button quits like the Quit button.
        vr.set_flag(dashboard, OverlayFlag::EnableControlBarClose, true)?;
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
            thumbnail,
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
    fn pump_events(&mut self) {
        let mut events = Vec::new();
        let mut actions = Vec::new();
        let mut restart = false;
        {
            let Some(session) = self.session.as_mut() else {
                return;
            };
            session.vr.poll_events_into(&mut events);
            for event in events {
                if closes_dashboard(&event, session.dashboard, session.thumbnail) {
                    log::info!("dashboard tab closed from its control bar");
                    actions.push(Action::Quit);
                    continue;
                }
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
                    _ => {}
                }
            }
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
            Action::CheckUpdates => self.updater.check_now(),
            Action::StartUpdate(version) => self.updater.start_update(&version, Instant::now()),
            Action::Quit => self.quit = true,
        }
    }

    // ---- Rendering ----------------------------------------------------------------------

    fn drive_hud(&mut self, now: Instant) {
        let resonance = self.resonance_view();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let device = HMD_INDEX;
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
        let alpha = self.cfg.hud.opacity;
        let size_m = self.cfg.hud.size_m;
        let placed = session.applied.is_none_or(|old| (old.nudge, old.size_m) != (nudge, size_m));
        if placed {
            let transform = Mat34::translation(
                HEAD_OFFSET[0] + nudge[0],
                HEAD_OFFSET[1] + nudge[1],
                HEAD_OFFSET[2] + nudge[2],
            );
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
                nudge,
                size_m,
                // NaN so that the first alpha update is always sent.
                alpha: f32::NAN,
            });
        }
        // Only send alpha changes that are visible.
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
            // Only carried while the indicator is on; the HUD ignores it in colour mode.
            resonance,
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
        let resonance = ResonanceLive {
            position: self.resonance_view(),
            reading_hz: self.resonance_hz,
        };
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
            resonance,
            update: self.updater.view(),
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

/// §6.3 status line: source recorded from, mute, current problem.
fn status_line(status: Option<&CaptureStatus>) -> String {
    let Some(status) = status else {
        return "Microphone: starting".to_owned();
    };
    let source = status.source_description.as_deref().unwrap_or("no source");
    let mut parts = vec![format!("Recording from {source}")];
    if status.muted {
        parts.push("mic muted".to_owned());
    }
    if let Some(error) = &status.error {
        parts.push(error.clone());
    }
    parts.join(" \u{00b7} ")
}

/// Colour state for an out-of-range voice (§4.1). Within `near_hz` below the band it is "too
/// low" (yellow), within `near_hz` above it "too high" (orange), further away "out of range"
/// (red). The side is decided by the band's geometric centre because the tracker's hysteresis
/// can report out-of-range a little inside an edge (that counts as near). Without an F0 the
/// previous out-of-range state is kept.
fn out_of_range_state(f0_hz: Option<f32>, band: (f32, f32), near_hz: f32, previous: HudState) -> HudState {
    let (lo, hi) = band;
    match f0_hz.filter(|f| f.is_finite() && *f > 0.0) {
        Some(f) if f < (lo * hi).sqrt() => {
            if lo - f <= near_hz && near_hz > 0.0 { HudState::TooLow } else { HudState::OutOfRange }
        }
        Some(f) => {
            if f - hi <= near_hz && near_hz > 0.0 { HudState::TooHigh } else { HudState::OutOfRange }
        }
        None if matches!(previous, HudState::TooLow | HudState::TooHigh) => previous,
        None => HudState::OutOfRange,
    }
}

/// Indicator content for the resonance switch and tracker state (SPEC §5.1); `None` while off.
/// The returned value never influences the tile colour or the reminder.
fn resonance_view(enabled: bool, state: Option<ResonanceState>) -> Option<ResonanceView> {
    if !enabled {
        return None;
    }
    Some(match state {
        Some(ResonanceState::Position(p)) => ResonanceView::Position(p),
        Some(ResonanceState::TooSimilar) => ResonanceView::TooSimilar,
        Some(ResonanceState::Gathering) | None => ResonanceView::Gathering,
    })
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

/// Whether `event` is the user closing this app's dashboard tab (main overlay or its thumbnail);
/// SPEC §4.2 treats that exactly like the Quit button.
fn closes_dashboard(event: &Event, dashboard: OverlayHandle, thumbnail: OverlayHandle) -> bool {
    matches!(event, Event::OverlayClosed(overlay) if *overlay == dashboard || *overlay == thumbnail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> CaptureStatus {
        CaptureStatus {
            source_description: Some("Built-in Audio Capture".into()),
            error: None,
            muted: false,
            healthy: true,
        }
    }

    #[test]
    fn closing_the_dashboard_tab_quits() {
        let dashboard = OverlayHandle(1);
        let thumbnail = OverlayHandle(2);
        let other = OverlayHandle(3);
        assert!(closes_dashboard(&Event::OverlayClosed(dashboard), dashboard, thumbnail));
        assert!(closes_dashboard(&Event::OverlayClosed(thumbnail), dashboard, thumbnail));
        assert!(!closes_dashboard(&Event::OverlayClosed(other), dashboard, thumbnail));
        assert!(!closes_dashboard(&Event::OverlayHidden(dashboard), dashboard, thumbnail));
        assert!(!closes_dashboard(&Event::Quit, dashboard, thumbnail));
    }

    #[test]
    fn status_line_reports_source_mute_and_problem() {
        assert_eq!(status_line(Some(&status())), "Recording from Built-in Audio Capture");
        let mut troubled = status();
        troubled.muted = true;
        troubled.error = Some("PipeWire connection lost".into());
        troubled.source_description = None;
        assert_eq!(
            status_line(Some(&troubled)),
            "Recording from no source \u{00b7} mic muted \u{00b7} PipeWire connection lost"
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
    fn icon_never_returns_a_missing_path() {
        if let Some(path) = icon_path() {
            assert!(path.is_file(), "{}", path.display());
        }
    }

    #[test]
    fn out_of_range_colour_follows_side_and_near_zone() {
        let band = (165.0, 255.0);
        let s = |f, near| out_of_range_state(f, band, near, HudState::InRange);
        assert_eq!(s(Some(160.0), 10.0), HudState::TooLow);
        assert_eq!(s(Some(155.0), 10.0), HudState::TooLow, "edge of the zone is still near");
        assert_eq!(s(Some(154.0), 10.0), HudState::OutOfRange);
        assert_eq!(s(Some(262.0), 10.0), HudState::TooHigh);
        assert_eq!(s(Some(266.0), 10.0), HudState::OutOfRange);
        // Hysteresis can report out-of-range just inside an edge: near, on that side.
        assert_eq!(s(Some(166.0), 10.0), HudState::TooLow);
        assert_eq!(s(Some(254.0), 10.0), HudState::TooHigh);
        // Zone off: everything outside is red.
        assert_eq!(s(Some(164.0), 0.0), HudState::OutOfRange);
        // No F0: keep the previous state.
        assert_eq!(out_of_range_state(None, band, 10.0, HudState::TooHigh), HudState::TooHigh);
        assert_eq!(out_of_range_state(None, band, 10.0, HudState::InRange), HudState::OutOfRange);
    }
}
