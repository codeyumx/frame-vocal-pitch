//! `--render-png DIR`: renders every HUD state × display mode, the resonance indicator states, the
//! settings panel pages and the explanation card with fake data. CPU only; needs neither SteamVR
//! nor a GPU.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fvp_config::{Config, DisplayMode, MicMode, Preset};
use tiny_skia::{IntSize, Pixmap};

use crate::hud::{Hud, HudState, HudView, ResonanceView};
use crate::settings::{
    CalibStatus, Card, LiveInfo, Page, ResonanceLive, SettingsPanel, UpdateLive, UpdateStatus,
};
use crate::strings::STRINGS;
use crate::{icon, Frame};

fn state_name(s: HudState) -> &'static str {
    match s {
        HudState::InRange => "in_range",
        HudState::OutOfRange => "out_of_range",
        HudState::NoVoice => "no_voice",
        HudState::Muted => "muted",
        HudState::NoMic => "no_mic",
    }
}

const STATES: [HudState; 5] = [
    HudState::InRange,
    HudState::OutOfRange,
    HudState::NoVoice,
    HudState::Muted,
    HudState::NoMic,
];

fn write_frame(frame: &Frame<'_>, path: &Path) -> io::Result<()> {
    let size = IntSize::from_wh(frame.width, frame.height)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty frame"))?;
    let png = if frame.premultiplied {
        Pixmap::from_vec(frame.rgba.to_vec(), size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame size mismatch"))?
            .encode_png()
            .map_err(io::Error::other)?
    } else {
        icon::encode_straight_png(frame.width, frame.height, frame.rgba)
    };
    std::fs::write(path, png)
}

/// Writes `hud-<display>-<state>.png` for every state and display mode, `hud-resonance-*.png`,
/// a toast example, and `settings*.png` into `dir` (created if missing). Returns the written
/// paths.
pub fn render_all_png(dir: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    let mut emit = |name: String, frame: Option<Frame<'_>>| -> io::Result<()> {
        let frame = frame.ok_or_else(|| io::Error::other(format!("{name}: renderer produced no frame")))?;
        let path = dir.join(name);
        write_frame(&frame, &path)?;
        written.push(path);
        Ok(())
    };

    let cfg = Config::default();
    for display in [DisplayMode::Color, DisplayMode::Detailed] {
        for state in STATES {
            let view = HudView {
                state,
                f0_hz: match state {
                    HudState::InRange => Some(182.0),
                    HudState::OutOfRange => Some(128.0),
                    _ => None,
                },
                band: cfg.band(),
                span: (cfg.audio.detect_min_hz, cfg.audio.detect_max_hz),
                display,
                fade_ms: 0,
                toast: None,
                resonance: None,
            };
            let mut hud = Hud::new();
            emit(
                format!("hud-{}-{}.png", display.as_str(), state_name(state)),
                hud.render(&view, Instant::now()),
            )?;
        }
    }
    let toast_view = HudView {
        state: HudState::InRange,
        f0_hz: Some(201.0),
        band: cfg.band(),
        span: (cfg.audio.detect_min_hz, cfg.audio.detect_max_hz),
        display: DisplayMode::Detailed,
        fade_ms: 0,
        toast: Some(crate::vocal_rest_toast(32)),
        resonance: Some(ResonanceView::Position(0.7)),
    };
    emit("hud-detailed-toast.png".into(), Hud::new().render(&toast_view, Instant::now()))?;

    let resonance_views: [(&str, HudState, ResonanceView); 8] = [
        ("near-target", HudState::InRange, ResonanceView::Position(0.85)),
        ("between", HudState::OutOfRange, ResonanceView::Position(0.45)),
        ("beyond-clamped", HudState::InRange, ResonanceView::Position(3.0)),
        ("below-clamped", HudState::NoVoice, ResonanceView::Position(-2.0)),
        ("gathering", HudState::InRange, ResonanceView::Gathering),
        ("uncalibrated", HudState::InRange, ResonanceView::Uncalibrated),
        ("too-similar", HudState::OutOfRange, ResonanceView::TooSimilar),
        ("needs-bypass", HudState::Muted, ResonanceView::NeedsBypass),
    ];
    for (name, state, res) in resonance_views {
        let view = HudView {
            state,
            f0_hz: Some(if state == HudState::InRange { 182.0 } else { 128.0 }),
            band: cfg.band(),
            span: (cfg.audio.detect_min_hz, cfg.audio.detect_max_hz),
            display: DisplayMode::Detailed,
            fade_ms: 0,
            toast: None,
            resonance: Some(res),
        };
        emit(format!("hud-resonance-{name}.png"), Hud::new().render(&view, Instant::now()))?;
    }

    let live = LiveInfo {
        f0_hz: Some(182.0),
        state: HudState::InRange,
        mic_status_line: "Bypass \u{00b7} recording from Internal Microphone (hardware node) \u{00b7} \
                          Valve mic filters: inactive for other apps"
            .into(),
        autostart: true,
        resonance: ResonanceLive::default(),
        update: UpdateLive {
            version: env!("CARGO_PKG_VERSION"),
            status: UpdateStatus::UpToDate { checked_ago_min: 125 },
            available: None,
        },
    };
    emit("settings.png".into(), SettingsPanel::new().render(&cfg, &live))?;

    let mut custom = cfg.clone();
    custom.target.preset = Preset::Custom;
    custom.target.custom_low_hz = 140.0;
    custom.target.custom_high_hz = 210.0;
    custom.audio.mic_mode = MicMode::Bypass;
    custom.reminder.enabled = true;
    custom.vocal_rest.minutes = 30;
    custom.hud.display = DisplayMode::Detailed;
    custom.hud.nudge_y_m = -0.03;
    custom.hud.nudge_z_m = 0.05;
    let fallback = LiveInfo {
        f0_hz: None,
        state: HudState::NoMic,
        mic_status_line: "Processed (requested: bypass) \u{00b7} Bypass unavailable: hardware mic \
                          node not found \u{2014} using processed mic \u{00b7} recording from Steam \
                          Frame Microphone \u{00b7} Valve mic filters: active for other apps"
            .into(),
        autostart: false,
        resonance: ResonanceLive::default(),
        update: UpdateLive::default(),
    };
    emit("settings-custom.png".into(), SettingsPanel::new().render(&custom, &fallback))?;

    // Resonance page: off, on while recording, on with a failure and the "too similar" state.
    let mut panel = SettingsPanel::new();
    panel.show(Page::Resonance, None);
    emit("settings-resonance-off.png".into(), panel.render(&cfg, &live))?;

    let mut on = cfg.clone();
    on.resonance.enabled = true;
    on.resonance.relaxed_df_hz = Some(1012.0);
    let recording = LiveInfo {
        resonance: ResonanceLive {
            position: Some(ResonanceView::Position(0.82)),
            relaxed: CalibStatus::NotRecorded,
            target: CalibStatus::Recording { progress: 0.37 },
        },
        ..live.clone()
    };
    emit("settings-resonance-recording.png".into(), panel.render(&on, &recording))?;

    let failed = LiveInfo {
        resonance: ResonanceLive {
            position: Some(ResonanceView::TooSimilar),
            relaxed: CalibStatus::Done { df_hz: 1012.0 },
            target: CalibStatus::Failed(STRINGS.calib_not_enough_speech.into()),
        },
        ..live.clone()
    };
    emit("settings-resonance-failed.png".into(), panel.render(&on, &failed))?;

    let mut card = SettingsPanel::new();
    card.show(Page::Resonance, Some(Card::Enable));
    emit("settings-card-enable.png".into(), card.render(&cfg, &live))?;
    card.scroll(-100.0);
    emit("settings-card-enable-scrolled.png".into(), card.render(&cfg, &live))?;
    card.show(Page::Resonance, Some(Card::Info));
    emit("settings-card-info.png".into(), card.render(&on, &recording))?;

    // Updates page: up to date, a newer release (then armed for confirmation), updating after a
    // manual start, and the two failure lines.
    let mut updates = SettingsPanel::new();
    updates.show(Page::Updates, None);
    emit("settings-updates.png".into(), updates.render(&cfg, &live))?;
    let with = |status: UpdateStatus, available: Option<&str>| LiveInfo {
        update: UpdateLive { status, available: available.map(str::to_string), ..live.update.clone() },
        ..live.clone()
    };
    let available = with(UpdateStatus::Available("0.3.0".into()), Some("0.3.0"));
    emit("settings-updates-available.png".into(), updates.render(&cfg, &available))?;
    updates.arm_update();
    emit("settings-updates-confirm.png".into(), updates.render(&cfg, &available))?;
    let mut manual_cfg = cfg.clone();
    manual_cfg.update.check = false;
    let manual = with(UpdateStatus::UpdatingManual, Some("0.3.0"));
    emit("settings-updates-updating.png".into(), updates.render(&manual_cfg, &manual))?;
    let failed = with(
        UpdateStatus::UpdateFailed("checksum mismatch for frame-vocal-pitch-0.3.0-aarch64.tar.gz; nothing was changed".into()),
        Some("0.3.0"),
    );
    emit("settings-updates-failed.png".into(), updates.render(&cfg, &failed))?;
    let offline = with(UpdateStatus::CheckFailed("Could not resolve host: api.github.com".into()), None);
    emit("settings-updates-offline.png".into(), updates.render(&cfg, &offline))?;
    Ok(written)
}
