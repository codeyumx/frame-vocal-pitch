//! `--render-png DIR`: renders every HUD state × display mode, the resonance indicator states, the
//! settings panel pages and the explanation card with fake data. CPU only; needs neither SteamVR
//! nor a GPU.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fvp_config::{Config, DisplayMode, Preset};
use tiny_skia::{IntSize, Pixmap};

use crate::hud::{Hud, HudState, HudView, ResonanceView};
use crate::settings::{
    Card, LiveInfo, Page, ResonanceLive, SettingsPanel, UpdateLive, UpdateStatus,
};
use crate::{icon, Frame};

fn state_name(s: HudState) -> &'static str {
    match s {
        HudState::InRange => "in_range",
        HudState::TooLow => "too_low",
        HudState::TooHigh => "too_high",
        HudState::OutOfRange => "out_of_range",
        HudState::NoVoice => "no_voice",
        HudState::Muted => "muted",
        HudState::NoMic => "no_mic",
    }
}

const STATES: [HudState; 7] = [
    HudState::InRange,
    HudState::TooLow,
    HudState::TooHigh,
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
                    HudState::TooLow => Some(128.0),
                    HudState::TooHigh => Some(262.0),
                    HudState::OutOfRange => Some(320.0),
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

    let resonance_views: [(&str, HudState, ResonanceView); 6] = [
        ("near-target", HudState::InRange, ResonanceView::Position(0.85)),
        ("between", HudState::TooLow, ResonanceView::Position(0.45)),
        ("beyond-clamped", HudState::InRange, ResonanceView::Position(3.0)),
        ("below-clamped", HudState::NoVoice, ResonanceView::Position(-2.0)),
        ("gathering", HudState::InRange, ResonanceView::Gathering),
        ("too-similar", HudState::TooHigh, ResonanceView::TooSimilar),
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
        mic_status_line: "Recording from Built-in Audio Capture".into(),
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
    custom.reminder.enabled = true;
    custom.vocal_rest.minutes = 30;
    custom.hud.display = DisplayMode::Detailed;
    custom.hud.nudge_y_m = -0.03;
    custom.hud.nudge_z_m = 0.05;
    let fallback = LiveInfo {
        f0_hz: None,
        state: HudState::NoMic,
        mic_status_line: "Recording from no source \u{00b7} PipeWire connection lost".into(),
        autostart: false,
        resonance: ResonanceLive::default(),
        update: UpdateLive::default(),
    };
    emit("settings-custom.png".into(), SettingsPanel::new().render(&custom, &fallback))?;

    // Resonance page: off, on with a reading, and the "too similar" state.
    let mut panel = SettingsPanel::new();
    panel.show(Page::Resonance, None);
    emit("settings-resonance-off.png".into(), panel.render(&cfg, &live))?;

    let mut on = cfg.clone();
    on.resonance.enabled = true;
    on.resonance.relaxed_df_hz = 1010.0;
    let reading = LiveInfo {
        resonance: ResonanceLive {
            position: Some(ResonanceView::Position(0.82)),
            reading_hz: Some(1133.0),
        },
        ..live.clone()
    };
    emit("settings-resonance-reading.png".into(), panel.render(&on, &reading))?;

    let mut similar_cfg = on.clone();
    similar_cfg.resonance.target_df_hz = 1030.0;
    let similar = LiveInfo {
        resonance: ResonanceLive { position: Some(ResonanceView::TooSimilar), reading_hz: None },
        ..live.clone()
    };
    emit("settings-resonance-similar.png".into(), panel.render(&similar_cfg, &similar))?;

    let mut card = SettingsPanel::new();
    card.show(Page::Resonance, Some(Card::Enable));
    emit("settings-card-enable.png".into(), card.render(&cfg, &live))?;
    card.scroll(-100.0);
    emit("settings-card-enable-scrolled.png".into(), card.render(&cfg, &live))?;
    card.show(Page::Resonance, Some(Card::Info));
    emit("settings-card-info.png".into(), card.render(&on, &reading))?;

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
