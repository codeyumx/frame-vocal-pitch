//! CPU-rendered HUD (§4.1) and dashboard settings panel (§4.2) for frame-vocal-pitch.
//!
//! Everything renders into premultiplied RGBA8 buffers with tiny-skia and the bundled Inter font
//! (SIL OFL 1.1, `assets/OFL.txt`). Renderers return `None` when the pixels would not change, so
//! callers upload textures only when something visible changed.
#![forbid(unsafe_code)]

mod draw;
mod hud;
mod icon;
mod preview;
mod settings;
mod strings;
mod text;

pub use hud::{
    hud_overlay_width_m, Hud, HudState, HudView, ResonanceView, HUD_HEIGHT_PX, HUD_TILE_PX,
    HUD_WIDTH_PX, RESONANCE_DISPLAY_RANGE,
};
pub use icon::{icon_png, render_icon};
pub use preview::render_all_png;
pub use settings::{
    steps, update_status_text, Action, CalibStatus, CalibWhich, LiveInfo, ResonanceLive,
    SettingsPanel, UpdateLive, UpdateStatus, SETTINGS_HEIGHT_PX, SETTINGS_WIDTH_PX,
    UPDATE_CONFIRM_WINDOW,
};
pub use strings::{vocal_rest_toast, Strings, EN, STRINGS};

/// A rendered RGBA8 image borrowed from its renderer (row-major, top row first, no padding).
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// True when colour channels are premultiplied by alpha (set `VROverlayFlags_IsPremultiplied`).
    pub premultiplied: bool,
}
