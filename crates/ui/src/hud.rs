//! HUD overlay rendering (§4.1).
//!
//! The texture is [`HUD_WIDTH_PX`] × [`HUD_HEIGHT_PX`]. The state tile ([`HUD_TILE_PX`] square) is
//! centred in it, so the overlay transform positions the tile itself; the transparent margin below
//! the tile holds the vocal-rest toast. Size the overlay with [`hud_overlay_width_m`] so the tile
//! is `hud.size_m` wide.

use std::time::{Duration, Instant};

use fvp_config::DisplayMode;
use tiny_skia::{Color, Pixmap};

use crate::draw::{self, Rgba, R};
use crate::strings::STRINGS;
use crate::text::{Align, TextRenderer, Weight};
use crate::Frame;

/// Width and height of the coloured tile in texture pixels.
pub const HUD_TILE_PX: u32 = 256;
/// HUD texture width in pixels.
pub const HUD_WIDTH_PX: u32 = 512;
/// HUD texture height in pixels.
pub const HUD_HEIGHT_PX: u32 = 544;

/// Overlay width in metres (for `SetOverlayWidthInMeters`) that makes the tile `size_m` wide.
pub fn hud_overlay_width_m(size_m: f32) -> f32 {
    size_m * HUD_WIDTH_PX as f32 / HUD_TILE_PX as f32
}

const TILE: R = R::new(
    ((HUD_WIDTH_PX - HUD_TILE_PX) / 2) as f32,
    ((HUD_HEIGHT_PX - HUD_TILE_PX) / 2) as f32,
    HUD_TILE_PX as f32,
    HUD_TILE_PX as f32,
);
const TILE_RADIUS: f32 = 44.0;
const TOAST_TOP: f32 = TILE.y + TILE.h + 10.0;
const TOAST_MAX_LINES: usize = 3;

/// What the HUD shows (§4.1 state table).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum HudState {
    InRange,
    OutOfRange,
    NoVoice,
    Muted,
    NoMic,
}

impl HudState {
    /// Tile colour (straight sRGB + alpha) for this state.
    pub(crate) fn color(self) -> Rgba {
        match self {
            HudState::InRange => Rgba::hex(0x2EBD59, 255),
            HudState::OutOfRange => Rgba::hex(0xD9433B, 255),
            HudState::NoVoice => Rgba::hex(0x202020, 153),
            HudState::Muted => Rgba::hex(0x6B6B6B, 255),
            HudState::NoMic => Rgba::hex(0xE0A030, 255),
        }
    }

    /// Foreground (glyph/text) colour on this state's tile.
    pub(crate) fn foreground(self) -> Rgba {
        match self {
            HudState::NoMic => Rgba::hex(0x1E1A12, 255),
            _ => Rgba::hex(0xFFFFFF, 255),
        }
    }

    fn voiced(self) -> bool {
        matches!(self, HudState::InRange | HudState::OutOfRange)
    }
}

/// Everything the HUD needs to draw one frame.
#[derive(Clone, PartialEq, Debug)]
pub struct HudView {
    pub state: HudState,
    /// Current F0; only shown while the state is voiced.
    pub f0_hz: Option<f32>,
    /// Target band (low, high) in Hz.
    pub band: (f32, f32),
    /// Detection span (min, max) in Hz; the range bar covers it on a log scale.
    pub span: (f32, f32),
    pub display: DisplayMode,
    /// Colour transition duration; 0 disables animation.
    pub fade_ms: u32,
    /// Vocal-rest toast text (see [`crate::vocal_rest_toast`]).
    pub toast: Option<String>,
}

/// Everything that influences the pixels, in quantised form.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Key {
    state: HudState,
    detailed: bool,
    color: Rgba,
    f0_text: Option<i32>,
    marker_y2: Option<i32>,
    band_y2: (i32, i32),
}

struct Transition {
    from: Rgba,
    start: Instant,
    duration: Duration,
}

/// HUD renderer. Keeps its own pixel buffer; [`Hud::render`] returns `None` when the pixels would
/// not change, so callers upload only on `Some`.
pub struct Hud {
    pixmap: Pixmap,
    text: TextRenderer,
    target: Option<Rgba>,
    transition: Option<Transition>,
    last: Option<Key>,
    last_toast: String,
    last_has_toast: bool,
    f0_text: String,
    wrap: Vec<(usize, usize)>,
}

impl Default for Hud {
    fn default() -> Self {
        Self::new()
    }
}

impl Hud {
    pub fn new() -> Self {
        Self {
            pixmap: Pixmap::new(HUD_WIDTH_PX, HUD_HEIGHT_PX).expect("non-zero HUD size"),
            text: TextRenderer::new(),
            target: None,
            transition: None,
            last: None,
            last_toast: String::new(),
            last_has_toast: false,
            f0_text: String::with_capacity(16),
            wrap: Vec::with_capacity(8),
        }
    }

    /// True while a colour transition is in progress as of the last [`Hud::render`]; the caller
    /// should keep calling `render` at its frame rate until this turns false.
    pub fn animating(&self) -> bool {
        self.transition.is_some()
    }

    /// Renders `view`. Returns `None` when the output would be identical to the previous frame.
    pub fn render(&mut self, view: &HudView, now: Instant) -> Option<Frame<'_>> {
        let color = self.advance_color(view, now);
        let key = self.key(view, color);
        let toast = view.toast.as_deref().filter(|t| !t.is_empty());
        let toast_changed = match toast {
            Some(t) => !self.last_has_toast || self.last_toast != t,
            None => self.last_has_toast,
        };
        if self.last == Some(key) && !toast_changed {
            return None;
        }
        self.last = Some(key);
        self.last_has_toast = toast.is_some();
        self.last_toast.clear();
        if let Some(t) = toast {
            self.last_toast.push_str(t);
        }

        self.pixmap.fill(Color::TRANSPARENT);
        draw::fill_rounded(&mut self.pixmap, TILE, TILE_RADIUS, color);
        if key.detailed {
            self.draw_detailed(view, key, color);
        } else {
            self.draw_glyph(view.state, TILE.cx(), TILE.cy(), 120.0, color);
        }
        if let Some(t) = toast {
            // Borrow juggling: the toast text lives in `view`, the buffers in `self`.
            self.draw_toast(t);
        }
        Some(Frame {
            width: HUD_WIDTH_PX,
            height: HUD_HEIGHT_PX,
            rgba: self.pixmap.data(),
            premultiplied: true,
        })
    }

    fn advance_color(&mut self, view: &HudView, now: Instant) -> Rgba {
        let target = view.state.color();
        match self.target {
            None => {
                self.target = Some(target);
                self.transition = None;
            }
            Some(old) if old != target => {
                let current = self.current_color(old, now);
                self.target = Some(target);
                self.transition = (view.fade_ms > 0 && current != target).then(|| Transition {
                    from: current,
                    start: now,
                    duration: Duration::from_millis(view.fade_ms as u64),
                });
            }
            Some(_) => {}
        }
        let color = self.current_color(target, now);
        if color == target {
            self.transition = None;
        }
        color
    }

    fn current_color(&self, target: Rgba, now: Instant) -> Rgba {
        match &self.transition {
            None => target,
            Some(tr) => {
                let t = now.saturating_duration_since(tr.start).as_secs_f32()
                    / tr.duration.as_secs_f32();
                if t >= 1.0 {
                    target
                } else {
                    // Smoothstep easing.
                    let e = t * t * (3.0 - 2.0 * t);
                    tr.from.lerp(target, e)
                }
            }
        }
    }

    fn key(&self, view: &HudView, color: Rgba) -> Key {
        let detailed = view.display == DisplayMode::Detailed;
        let (mut f0_text, mut marker_y2, mut band_y2) = (None, None, (0, 0));
        if detailed {
            let bar = BarGeom::new(view.span);
            let f0 = view.f0_hz.filter(|f| f.is_finite() && *f > 0.0);
            if view.state.voiced() {
                f0_text = f0.map(|f| f.round() as i32);
                marker_y2 = f0.map(|f| (bar.y_of(f) * 2.0).round() as i32);
            }
            let (lo, hi) = band_px(&bar, view.band);
            band_y2 = ((lo * 2.0).round() as i32, (hi * 2.0).round() as i32);
        }
        Key {
            state: view.state,
            detailed,
            color,
            f0_text,
            marker_y2,
            band_y2,
        }
    }

    fn draw_glyph(&mut self, state: HudState, cx: f32, cy: f32, h: f32, tile: Rgba) {
        let fg = state.foreground();
        match state {
            HudState::Muted => {
                draw::mic_glyph(&mut self.pixmap, cx, cy, h, fg, tile.with_alpha(255), true)
            }
            HudState::NoMic => draw::bang_glyph(&mut self.pixmap, cx, cy, h, fg),
            _ => {}
        }
    }

    fn draw_detailed(&mut self, view: &HudView, key: Key, tile: Rgba) {
        let fg = view.state.foreground();
        let bar = BarGeom::new(view.span);
        // Track, band, marker.
        draw::fill_rounded(&mut self.pixmap, bar.track, bar.track.w / 2.0, Rgba::hex(0, 80));
        let (y_hi, y_lo) = (key.band_y2.1 as f32 / 2.0, key.band_y2.0 as f32 / 2.0);
        if y_lo - y_hi > 0.5 {
            let band = R::new(bar.track.x + 4.0, y_hi, bar.track.w - 8.0, y_lo - y_hi);
            draw::fill_rounded(&mut self.pixmap, band, 6.0, Rgba::hex(0xFFFFFF, 150));
        }
        if let Some(y2) = key.marker_y2 {
            let y = y2 as f32 / 2.0;
            let m = R::new(bar.track.x - 9.0, y - 6.0, bar.track.w + 18.0, 12.0);
            draw::fill_rounded(&mut self.pixmap, m, 6.0, Rgba::hex(0xFFFFFF, 255));
            draw::stroke_rounded(&mut self.pixmap, m, 6.0, 2.5, Rgba::hex(0x101010, 200));
        }

        // Text area left of the bar.
        let area = R::new(TILE.x + 14.0, TILE.y, bar.track.x - TILE.x - 26.0, TILE.h);
        match view.state {
            HudState::Muted | HudState::NoMic => {
                self.draw_glyph(view.state, area.cx(), area.cy(), 104.0, tile)
            }
            _ => match key.f0_text {
                Some(hz) => self.draw_f0(area, hz, fg),
                None => {
                    let px = 72.0;
                    let base = area.cy() + 12.0;
                    self.text.draw(
                        &mut self.pixmap,
                        Weight::SemiBold,
                        px,
                        area.cx(),
                        base,
                        Align::Center,
                        STRINGS.no_f0,
                        fg,
                        Some(area.clip()),
                    );
                }
            },
        }
    }

    fn draw_f0(&mut self, area: R, hz: i32, fg: Rgba) {
        use std::fmt::Write;
        self.f0_text.clear();
        let _ = write!(self.f0_text, "{hz}");
        let unit_px = 30.0;
        let gap = 6.0;
        let unit_w = self.text.measure(Weight::Regular, unit_px, STRINGS.hz);
        let num_px = self.text.fit(
            Weight::SemiBold,
            74.0,
            28.0,
            area.w - unit_w - gap,
            &self.f0_text,
        );
        let num_w = self.text.measure(Weight::SemiBold, num_px, &self.f0_text);
        let total = num_w + gap + unit_w;
        let x = area.cx() - total / 2.0;
        let base = area.cy() + TextRenderer::cap_height(Weight::SemiBold, num_px) / 2.0;
        let clip = Some(area.clip());
        let num = std::mem::take(&mut self.f0_text);
        self.text.draw(
            &mut self.pixmap,
            Weight::SemiBold,
            num_px,
            x,
            base,
            Align::Left,
            &num,
            fg,
            clip,
        );
        self.f0_text = num;
        self.text.draw(
            &mut self.pixmap,
            Weight::Regular,
            unit_px,
            x + num_w + gap,
            base,
            Align::Left,
            STRINGS.hz,
            fg.with_alpha(220),
            clip,
        );
    }

    fn draw_toast(&mut self, toast: &str) {
        let max_w = HUD_WIDTH_PX as f32 - 16.0 - 36.0;
        let mut px = 28.0;
        loop {
            self.text.wrap(Weight::Regular, px, max_w, toast, &mut self.wrap);
            if self.wrap.len() <= TOAST_MAX_LINES || px <= 18.0 {
                break;
            }
            px -= 2.0;
        }
        let lines = self.wrap.len().min(TOAST_MAX_LINES);
        let line_h = (px * 1.25).round();
        let mut widest: f32 = 0.0;
        for i in 0..lines {
            let (s, e) = self.wrap[i];
            widest = widest.max(self.text.measure(Weight::Regular, px, &toast[s..e]));
        }
        let box_w = (widest.min(max_w) + 36.0).max(120.0);
        let box_h = lines as f32 * line_h + 22.0;
        let bx = R::new((HUD_WIDTH_PX as f32 - box_w) / 2.0, TOAST_TOP, box_w, box_h);
        draw::fill_rounded(&mut self.pixmap, bx, 18.0, Rgba::hex(0x141414, 225));
        let ascent = TextRenderer::cap_height(Weight::Regular, px);
        let first_base = bx.y + 11.0 + (line_h - ascent) / 2.0 + ascent;
        for i in 0..lines {
            let (s, e) = self.wrap[i];
            self.text.draw(
                &mut self.pixmap,
                Weight::Regular,
                px,
                bx.cx(),
                first_base + i as f32 * line_h,
                Align::Center,
                &toast[s..e],
                Rgba::hex(0xFFFFFF, 255),
                Some(bx.clip()),
            );
        }
    }
}

/// Geometry of the vertical range bar inside the tile (detailed mode).
struct BarGeom {
    track: R,
    /// Usable y range (top, bottom) inside the rounded track ends.
    y_top: f32,
    y_bottom: f32,
    ln_lo: f32,
    ln_hi: f32,
}

impl BarGeom {
    fn new(span: (f32, f32)) -> Self {
        let track = R::new(TILE.x + 190.0, TILE.y + 22.0, 34.0, TILE.h - 44.0);
        let lo = span.0.max(1.0);
        let hi = span.1.max(lo * 1.01);
        Self {
            track,
            y_top: track.y + 8.0,
            y_bottom: track.bottom() - 8.0,
            ln_lo: lo.ln(),
            ln_hi: hi.ln(),
        }
    }

    /// Log-scale position of `hz`, clamped to the span; higher pitch is higher on screen.
    fn y_of(&self, hz: f32) -> f32 {
        let frac = ((hz.max(1.0).ln() - self.ln_lo) / (self.ln_hi - self.ln_lo)).clamp(0.0, 1.0);
        self.y_bottom - frac * (self.y_bottom - self.y_top)
    }
}

/// (y of band low edge, y of band high edge).
fn band_px(bar: &BarGeom, band: (f32, f32)) -> (f32, f32) {
    (bar.y_of(band.0), bar.y_of(band.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(state: HudState, display: DisplayMode) -> HudView {
        HudView {
            state,
            f0_hz: Some(182.0),
            band: (165.0, 255.0),
            span: (60.0, 500.0),
            display,
            fade_ms: 300,
            toast: None,
        }
    }

    #[test]
    fn first_render_is_some_then_none_when_unchanged() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let v = view(HudState::InRange, DisplayMode::Color);
        assert!(hud.render(&v, t0).is_some());
        assert!(!hud.animating());
        assert!(hud.render(&v, t0 + Duration::from_millis(16)).is_none());
        assert!(hud.render(&v, t0 + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn color_mode_ignores_f0_changes() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let mut v = view(HudState::InRange, DisplayMode::Color);
        assert!(hud.render(&v, t0).is_some());
        v.f0_hz = Some(200.0);
        assert!(hud.render(&v, t0).is_none());
    }

    #[test]
    fn detailed_mode_rerenders_on_displayed_f0_change_only() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let mut v = view(HudState::InRange, DisplayMode::Detailed);
        assert!(hud.render(&v, t0).is_some());
        v.f0_hz = Some(182.2); // same text, same marker pixel
        assert!(hud.render(&v, t0).is_none());
        v.f0_hz = Some(190.0);
        assert!(hud.render(&v, t0).is_some());
    }

    #[test]
    fn fade_renders_some_while_animating_then_settles() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let mut v = view(HudState::InRange, DisplayMode::Color);
        hud.render(&v, t0).unwrap();
        v.state = HudState::OutOfRange;
        assert!(hud.render(&v, t0).is_some(), "state change renders");
        assert!(hud.animating());
        let mut t = t0;
        let mut frames = 0;
        for _ in 0..30 {
            t += Duration::from_millis(11);
            if hud.render(&v, t).is_some() {
                frames += 1;
            }
        }
        assert!(frames >= 20, "fade must produce frames ({frames})");
        assert!(!hud.animating());
        assert!(hud.render(&v, t + Duration::from_millis(11)).is_none());
        // Final colour is the target colour at the tile centre.
        let px = hud.pixmap.pixel(TILE.cx() as u32, TILE.y as u32 + 10).unwrap();
        assert_eq!((px.red(), px.green(), px.blue(), px.alpha()), (0xD9, 0x43, 0x3B, 255));
    }

    #[test]
    fn fade_zero_switches_immediately() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let mut v = view(HudState::InRange, DisplayMode::Color);
        v.fade_ms = 0;
        hud.render(&v, t0).unwrap();
        v.state = HudState::NoMic;
        assert!(hud.render(&v, t0).is_some());
        assert!(!hud.animating());
    }

    #[test]
    fn toast_change_rerenders() {
        let mut hud = Hud::new();
        let t0 = Instant::now();
        let mut v = view(HudState::NoVoice, DisplayMode::Color);
        hud.render(&v, t0).unwrap();
        v.toast = Some(crate::vocal_rest_toast(30));
        assert!(hud.render(&v, t0).is_some());
        assert!(hud.render(&v, t0).is_none());
        v.toast = None;
        assert!(hud.render(&v, t0).is_some());
    }

    #[test]
    fn frame_is_premultiplied_and_sized() {
        let mut hud = Hud::new();
        let v = view(HudState::NoVoice, DisplayMode::Detailed);
        let f = hud.render(&v, Instant::now()).unwrap();
        assert!(f.premultiplied);
        assert_eq!(f.rgba.len(), (f.width * f.height * 4) as usize);
        assert!(f.rgba.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }
}
