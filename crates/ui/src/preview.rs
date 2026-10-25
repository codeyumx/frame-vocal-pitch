//! `--render-png DIR`: renders every HUD state × display mode and the settings panel with fake
//! data. CPU only; needs neither SteamVR nor a GPU.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fvp_config::{Config, DisplayMode, MicMode, Preset};
use tiny_skia::{IntSize, Pixmap};

use crate::hud::{Hud, HudState, HudView};
use crate::settings::{LiveInfo, SettingsPanel};
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

/// Writes `hud-<display>-<state>.png` for every state and display mode, a toast example, and
/// `settings*.png` into `dir` (created if missing). Returns the written paths.
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
    };
    emit("hud-detailed-toast.png".into(), Hud::new().render(&toast_view, Instant::now()))?;

    let live = LiveInfo {
        f0_hz: Some(182.0),
        state: HudState::InRange,
        mic_status_line: "Bypass \u{00b7} recording from Internal Microphone (hardware node) \u{00b7} \
                          Valve mic filters: inactive for other apps"
            .into(),
        autostart: true,
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
    };
    emit("settings-custom.png".into(), SettingsPanel::new().render(&custom, &fallback))?;
    Ok(written)
}
