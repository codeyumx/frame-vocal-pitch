//! Dashboard settings panel (§4.2), 1200 × 800 px.
//!
//! Coordinates passed to [`SettingsPanel::pointer_move`] and [`SettingsPanel::click`] are panel
//! pixels with the origin at the **top-left** corner (OpenVR mouse events use a bottom-left
//! origin; flip `y` before calling).

use fvp_config::{Config, DisplayMode, MicMode, Placement, Preset, CUSTOM_MIN_GAP_HZ};
use tiny_skia::{Color, Pixmap};

use crate::draw::{self, Rgba, R};
use crate::hud::HudState;
use crate::strings::{fill_v, STRINGS};
use crate::text::{Align, TextRenderer, Weight};
use crate::Frame;

/// Settings panel width in pixels (also the OpenVR mouse scale x).
pub const SETTINGS_WIDTH_PX: u32 = 1200;
/// Settings panel height in pixels (also the OpenVR mouse scale y).
pub const SETTINGS_HEIGHT_PX: u32 = 800;

/// Dashboard step sizes and value ranges (SPEC §6.1).
pub mod steps {
    pub const SIZE_M: f32 = 0.01;
    pub const SIZE_RANGE: (f32, f32) = (0.02, 0.20);
    pub const OPACITY: f32 = 0.1;
    pub const OPACITY_RANGE: (f32, f32) = (0.1, 1.0);
    pub const NUDGE_M: f32 = 0.01;
    pub const NUDGE_RANGE: (f32, f32) = (-0.5, 0.5);
    pub const DELAY_S: f32 = 0.5;
    pub const DELAY_RANGE: (f32, f32) = (0.5, 10.0);
    pub const PITCH_HZ: f32 = 10.0;
    pub const PITCH_RANGE: (f32, f32) = (100.0, 1000.0);
    pub const VOLUME: f32 = 0.1;
    pub const VOLUME_RANGE: (f32, f32) = (0.0, 1.0);
    pub const VOCAL_REST_MIN: u32 = 5;
    pub const VOCAL_REST_RANGE: (u32, u32) = (5, 50);
    /// Small and large custom-band steps (Hz).
    pub const BAND_SMALL_HZ: f32 = 1.0;
    pub const BAND_LARGE_HZ: f32 = 10.0;
}

/// Live data shown in the panel (live strip, mic status line, autostart state).
#[derive(Clone, PartialEq, Debug)]
pub struct LiveInfo {
    pub f0_hz: Option<f32>,
    pub state: HudState,
    /// §6.3 status line: mode, node recorded from, fallback reason, filter state.
    pub mic_status_line: String,
    /// Whether the systemd user unit is enabled.
    pub autostart: bool,
}

/// Result of a click on a control.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// The full new configuration; write it to the config file immediately.
    SetConfig(Config),
    /// Enable/disable the systemd user unit.
    SetAutostart(bool),
    /// Quit (exit status 3).
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    X,
    Y,
    Z,
}

/// Every interactive control; `i8` payloads are step multipliers (sign = direction).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Control {
    HudEnabled(bool),
    Placement(Placement),
    Display(DisplayMode),
    Size(i8),
    Opacity(i8),
    Nudge(Axis, i8),
    Preset(Preset),
    CustomLow(i8),
    CustomHigh(i8),
    ReminderEnabled(bool),
    ReminderDelay(i8),
    ReminderPitch(i8),
    ReminderVolume(i8),
    VocalRest(i8),
    MicMode(MicMode),
    Autostart(bool),
    Quit,
}

#[derive(Clone, Copy, Debug)]
struct Widget {
    rect: R,
    control: Control,
    enabled: bool,
    /// Lives in the scrollable body (hit only inside the body viewport).
    in_body: bool,
}

/// Rounds to `1/inv` steps; dividing by the exact integer `inv` yields the nearest f32 (0.06, not
/// 0.060000002).
fn round_to(v: f32, inv: f32) -> f32 {
    (v * inv).round() / inv
}

fn step(v: f32, delta: f32, (lo, hi): (f32, f32)) -> f32 {
    round_to(v + delta, 1000.0).clamp(lo, hi)
}

fn step_vocal_rest(minutes: u32, dir: i8) -> u32 {
    let (lo, hi) = steps::VOCAL_REST_RANGE;
    let s = steps::VOCAL_REST_MIN;
    if dir > 0 {
        if minutes == 0 {
            lo
        } else {
            ((minutes / s + 1) * s).clamp(lo, hi)
        }
    } else if minutes <= lo {
        0
    } else {
        (((minutes - 1) / s) * s).clamp(lo, hi)
    }
}

/// Applies `control` to a copy of `cfg`; `None` when nothing would change.
fn apply(control: Control, cfg: &Config, autostart: bool) -> Option<Action> {
    let mut c = cfg.clone();
    let band_step = |d: i8| {
        d.signum() as f32
            * if d.unsigned_abs() >= 10 {
                steps::BAND_LARGE_HZ
            } else {
                steps::BAND_SMALL_HZ
            }
    };
    match control {
        Control::Quit => return Some(Action::Quit),
        Control::Autostart(on) => return (on != autostart).then_some(Action::SetAutostart(on)),
        Control::HudEnabled(on) => c.hud.enabled = on,
        Control::Placement(p) => c.hud.placement = p,
        Control::Display(d) => c.hud.display = d,
        Control::Size(d) => {
            c.hud.size_m = step(c.hud.size_m, d as f32 * steps::SIZE_M, steps::SIZE_RANGE)
        }
        Control::Opacity(d) => {
            c.hud.opacity = step(c.hud.opacity, d as f32 * steps::OPACITY, steps::OPACITY_RANGE)
        }
        Control::Nudge(axis, d) => {
            let v = match axis {
                Axis::X => &mut c.hud.nudge_x_m,
                Axis::Y => &mut c.hud.nudge_y_m,
                Axis::Z => &mut c.hud.nudge_z_m,
            };
            *v = step(*v, d as f32 * steps::NUDGE_M, steps::NUDGE_RANGE);
        }
        Control::Preset(p) => c.target.preset = p,
        Control::CustomLow(d) => {
            let t = &mut c.target;
            let lo_min = c.audio.detect_min_hz;
            let lo_max = (t.custom_high_hz - CUSTOM_MIN_GAP_HZ).max(lo_min);
            t.custom_low_hz = (t.custom_low_hz + band_step(d)).clamp(lo_min, lo_max);
        }
        Control::CustomHigh(d) => {
            let t = &mut c.target;
            let hi_max = c.audio.detect_max_hz;
            let hi_min = (t.custom_low_hz + CUSTOM_MIN_GAP_HZ).min(hi_max);
            t.custom_high_hz = (t.custom_high_hz + band_step(d)).clamp(hi_min, hi_max);
        }
        Control::ReminderEnabled(on) => c.reminder.enabled = on,
        Control::ReminderDelay(d) => {
            c.reminder.delay_s =
                step(c.reminder.delay_s, d as f32 * steps::DELAY_S, steps::DELAY_RANGE)
        }
        Control::ReminderPitch(d) => {
            c.reminder.pitch_hz =
                step(c.reminder.pitch_hz, d as f32 * steps::PITCH_HZ, steps::PITCH_RANGE)
        }
        Control::ReminderVolume(d) => {
            c.reminder.volume =
                step(c.reminder.volume, d as f32 * steps::VOLUME, steps::VOLUME_RANGE)
        }
        Control::VocalRest(d) => c.vocal_rest.minutes = step_vocal_rest(c.vocal_rest.minutes, d),
        Control::MicMode(m) => c.audio.mic_mode = m,
    }
    (c != *cfg).then_some(Action::SetConfig(c))
}

// ---- Layout ---------------------------------------------------------------------------------

const W: f32 = SETTINGS_WIDTH_PX as f32;
const H: f32 = SETTINGS_HEIGHT_PX as f32;
const M: f32 = 24.0;
const VIEW_TOP: f32 = 160.0;
const COL_W: f32 = (W - 3.0 * M) / 2.0;
const LEFT_X: f32 = M;
const RIGHT_X: f32 = M + COL_W + M;
const PAD: f32 = 16.0;
const LABEL_W: f32 = 168.0;
const CTRL_DX: f32 = PAD + LABEL_W + 14.0;
const CTRL_W: f32 = COL_W - CTRL_DX - PAD;
const ROW_H: f32 = 46.0;
const CTRL_H: f32 = 38.0;
const HEAD_H: f32 = 44.0;
const CARD_GAP: f32 = 12.0;
const CARD_BOTTOM_PAD: f32 = 10.0;
const RADIO_H: f32 = 42.0;
const STATUS_LINE_H: f32 = 22.0;
const STATUS_LINES: usize = 3;
/// Scroll distance per unit of `scroll(dy)`.
const SCROLL_STEP_PX: f32 = 60.0;

const BG: Rgba = Rgba::hex(0x12151A, 255);
const CARD: Rgba = Rgba::hex(0x1C2028, 255);
const TEXT: Rgba = Rgba::hex(0xECEEF1, 255);
const TEXT_DIM: Rgba = Rgba::hex(0xA3A9B2, 255);
const CTRL: Rgba = Rgba::hex(0x2A303A, 255);
const CTRL_HOVER: Rgba = Rgba::hex(0x3A4452, 255);
const ACCENT: Rgba = Rgba::hex(0x3B7FE6, 255);
const ACCENT_HOVER: Rgba = Rgba::hex(0x5592F0, 255);
const QUIT: Rgba = Rgba::hex(0xB83A32, 255);
const QUIT_HOVER: Rgba = Rgba::hex(0xD04A40, 255);
const DISABLED_ALPHA: u8 = 90;

fn ctrl_rect(col_x: f32, row_y: f32) -> R {
    R::new(col_x + CTRL_DX, row_y + (ROW_H - CTRL_H) / 2.0, CTRL_W, CTRL_H)
}

/// Height of the scrollable content (from `VIEW_TOP`), used to bound scrolling.
const fn content_height() -> f32 {
    let left = hud_card_h() + CARD_GAP + target_card_h();
    let right = reminder_card_h() + CARD_GAP + rest_card_h() + CARD_GAP + system_card_h();
    (if left > right { left } else { right }) + PAD
}
const fn hud_card_h() -> f32 {
    HEAD_H + 7.0 * ROW_H + 28.0 + CARD_BOTTOM_PAD
}
const fn target_card_h() -> f32 {
    HEAD_H + 3.0 * ROW_H + CARD_BOTTOM_PAD
}
const fn reminder_card_h() -> f32 {
    HEAD_H + 4.0 * ROW_H + CARD_BOTTOM_PAD
}
const fn rest_card_h() -> f32 {
    HEAD_H + ROW_H + CARD_BOTTOM_PAD
}
const fn system_card_h() -> f32 {
    HEAD_H + 2.0 * RADIO_H + STATUS_LINES as f32 * STATUS_LINE_H + 8.0 + ROW_H + CARD_BOTTOM_PAD
}

fn fmt_num(v: f32, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

fn fmt_signed(v: f32) -> String {
    let v = if v.abs() < 0.0005 { 0.0 } else { v };
    if v < 0.0 {
        format!("\u{2212}{:.2}", -v)
    } else {
        format!("+{v:.2}")
    }
}

fn fmt_hz(v: f32) -> String {
    fill_v(STRINGS.fmt_hz, &fmt_num(v, 0))
}

fn state_label(state: HudState) -> &'static str {
    match state {
        HudState::InRange => STRINGS.state_in_range,
        HudState::OutOfRange => STRINGS.state_out_of_range,
        HudState::NoVoice => STRINGS.state_no_voice,
        HudState::Muted => STRINGS.state_muted,
        HudState::NoMic => STRINGS.state_no_mic,
    }
}

/// Immediate-mode builder: draws controls and records their hit rectangles.
struct Ui<'a> {
    pm: &'a mut Pixmap,
    tx: &'a mut TextRenderer,
    widgets: &'a mut Vec<Widget>,
    wrap: &'a mut Vec<(usize, usize)>,
    pointer: Option<(f32, f32)>,
    /// Vertical offset applied to body coordinates (−scroll).
    dy: f32,
    in_body: bool,
}

impl Ui<'_> {
    fn place(&self, r: R) -> R {
        R::new(r.x, r.y + self.dy, r.w, r.h)
    }

    /// Registers a control; returns whether the pointer hovers it.
    fn widget(&mut self, r: R, control: Control, enabled: bool) -> bool {
        let rect = self.place(r);
        self.widgets.push(Widget {
            rect,
            control,
            enabled,
            in_body: self.in_body,
        });
        enabled
            && self.pointer.is_some_and(|(x, y)| {
                rect.contains(x, y) && (!self.in_body || y >= VIEW_TOP)
            })
    }

    /// Single-line text fitted to `r.w` and vertically centred in `r`.
    fn text(&mut self, r: R, text: &str, max_px: f32, weight: Weight, color: Rgba, align: Align) {
        let r = self.place(r);
        let px = self.tx.fit(weight, max_px, 11.0, r.w, text);
        let base = r.cy() + TextRenderer::cap_height(weight, px) / 2.0;
        let x = match align {
            Align::Left => r.x,
            Align::Center => r.cx(),
            Align::Right => r.right(),
        };
        self.tx.draw(self.pm, weight, px, x, base, align, text, color, Some(r.clip()));
    }

    fn fill(&mut self, r: R, radius: f32, color: Rgba) {
        let r = self.place(r);
        draw::fill_rounded(self.pm, r, radius, color);
    }

    fn card(&mut self, x: f32, y: f32, h: f32, title: &str) -> f32 {
        self.fill(R::new(x, y, COL_W, h), 14.0, CARD);
        self.text(
            R::new(x + PAD, y + 8.0, COL_W - 2.0 * PAD, HEAD_H - 12.0),
            title,
            22.0,
            Weight::SemiBold,
            TEXT,
            Align::Left,
        );
        y + HEAD_H
    }

    fn row_label(&mut self, col_x: f32, row_y: f32, label: &str, enabled: bool) {
        let color = if enabled { TEXT } else { TEXT.with_alpha(DISABLED_ALPHA + 40) };
        self.text(
            R::new(col_x + PAD, row_y, LABEL_W, ROW_H),
            label,
            20.0,
            Weight::Regular,
            color,
            Align::Left,
        );
    }

    fn button(&mut self, r: R, label: &str, control: Control, enabled: bool, selected: bool) {
        let hover = self.widget(r, control, enabled);
        let (bg, fg) = match (selected, hover) {
            (true, false) => (ACCENT, Rgba::hex(0xFFFFFF, 255)),
            (true, true) => (ACCENT_HOVER, Rgba::hex(0xFFFFFF, 255)),
            (false, true) => (CTRL_HOVER, TEXT),
            (false, false) => (CTRL, TEXT),
        };
        let (bg, fg) = if enabled {
            (bg, fg)
        } else {
            (bg.with_alpha(DISABLED_ALPHA), fg.with_alpha(DISABLED_ALPHA + 30))
        };
        self.fill(r, 8.0, bg);
        let weight = if selected { Weight::SemiBold } else { Weight::Regular };
        self.text(r.inset(6.0), label, 19.0, weight, fg, Align::Center);
    }

    fn segmented(&mut self, r: R, items: &[(&str, Control, bool)], enabled: bool) {
        let gap = 4.0;
        let n = items.len() as f32;
        let w = (r.w - gap * (n - 1.0)) / n;
        for (i, &(label, control, selected)) in items.iter().enumerate() {
            let seg = R::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
            self.button(seg, label, control, enabled, selected);
        }
    }

    fn on_off(&mut self, r: R, value: bool, make: fn(bool) -> Control) {
        self.segmented(
            r,
            &[(STRINGS.off, make(false), !value), (STRINGS.on, make(true), value)],
            true,
        );
    }

    /// `[−big][−] value [+][+big]`; `steps` lists (label, control) for the left and right side.
    fn stepper(&mut self, r: R, value: &str, left: &[(&str, Control)], right: &[(&str, Control)], enabled: bool) {
        let bw = 56.0;
        let gap = 4.0;
        let mut x = r.x;
        for &(label, control) in left {
            self.button(R::new(x, r.y, bw, r.h), label, control, enabled, false);
            x += bw + gap;
        }
        let right_w = right.len() as f32 * (bw + gap);
        let value_r = R::new(x, r.y, r.right() - right_w - x, r.h);
        let fg = if enabled { TEXT } else { TEXT.with_alpha(DISABLED_ALPHA + 30) };
        self.text(value_r.inset(4.0), value, 20.0, Weight::SemiBold, fg, Align::Center);
        let mut x = r.right() - right_w + gap;
        for &(label, control) in right {
            self.button(R::new(x, r.y, bw, r.h), label, control, enabled, false);
            x += bw + gap;
        }
    }

    fn radio(&mut self, r: R, label: &str, control: Control, selected: bool) {
        let hover = self.widget(r, control, true);
        if hover {
            self.fill(r, 8.0, CTRL_HOVER);
        }
        let p = self.place(r);
        let (cx, cy) = (p.x + 20.0, p.cy());
        draw::fill_circle(self.pm, cx, cy, 11.0, if selected { ACCENT } else { TEXT_DIM });
        draw::fill_circle(self.pm, cx, cy, 8.5, CARD);
        if selected {
            draw::fill_circle(self.pm, cx, cy, 5.5, ACCENT);
        }
        let weight = if selected { Weight::SemiBold } else { Weight::Regular };
        self.text(
            R::new(r.x + 42.0, r.y, r.w - 50.0, r.h),
            label,
            20.0,
            weight,
            TEXT,
            Align::Left,
        );
    }

    /// Wrapped multi-line text in `r`, shrinking until it fits `max_lines`.
    fn paragraph(&mut self, r: R, text: &str, max_lines: usize, line_h: f32, color: Rgba) {
        let r = self.place(r);
        let mut px = 17.0;
        loop {
            self.tx.wrap(Weight::Regular, px, r.w, text, self.wrap);
            if self.wrap.len() <= max_lines || px <= 13.0 {
                break;
            }
            px -= 1.0;
        }
        let ascent = TextRenderer::cap_height(Weight::Regular, px);
        for (i, &(s, e)) in self.wrap.iter().take(max_lines).enumerate() {
            let base = r.y + i as f32 * line_h + (line_h + ascent) / 2.0;
            self.tx.draw(
                self.pm,
                Weight::Regular,
                px,
                r.x,
                base,
                Align::Left,
                &text[s..e],
                color,
                Some(r.clip()),
            );
        }
    }
}

/// Builds (and draws) the whole panel. Body first, then the fixed header on top of it.
fn build(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo, scroll: f32) {
    ui.in_body = true;
    ui.dy = -scroll;
    body_left(ui, cfg);
    body_right(ui, cfg, live);

    ui.in_body = false;
    ui.dy = 0.0;
    draw::fill_rounded(ui.pm, R::new(0.0, 0.0, W, VIEW_TOP), 0.0, BG);
    header(ui, cfg, live);
}

fn header(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo) {
    let quit = R::new(W - M - 170.0, 14.0, 170.0, 48.0);
    ui.text(R::new(M, 14.0, quit.x - 2.0 * M, 48.0), STRINGS.title, 32.0, Weight::SemiBold, TEXT, Align::Left);
    let hover = ui.widget(quit, Control::Quit, true);
    ui.fill(quit, 10.0, if hover { QUIT_HOVER } else { QUIT });
    ui.text(quit.inset(8.0), STRINGS.quit, 22.0, Weight::SemiBold, Rgba::hex(0xFFFFFF, 255), Align::Center);

    // Live strip.
    let strip = R::new(M, 78.0, W - 2.0 * M, 66.0);
    ui.fill(strip, 14.0, CARD);
    ui.text(R::new(strip.x + 20.0, strip.y, 90.0, strip.h), STRINGS.live_heading, 20.0, Weight::Regular, TEXT_DIM, Align::Left);
    let voiced = matches!(live.state, HudState::InRange | HudState::OutOfRange);
    let f0 = live.f0_hz.filter(|f| voiced && f.is_finite() && *f > 0.0);
    let f0_text = f0.map_or_else(|| STRINGS.no_f0.to_string(), fmt_hz);
    ui.text(R::new(strip.x + 116.0, strip.y, 170.0, strip.h), &f0_text, 34.0, Weight::SemiBold, TEXT, Align::Left);

    let label = state_label(live.state);
    let label_px = 20.0;
    let chip_w = (ui.tx.measure(Weight::SemiBold, label_px, label) + 36.0).min(320.0);
    let chip = R::new(strip.x + 300.0, strip.y + 13.0, chip_w, 40.0);
    let chip_color = match live.state {
        HudState::NoVoice => Rgba::hex(0x3A3F47, 255),
        s => s.color(),
    };
    ui.fill(chip, 20.0, chip_color);
    ui.text(chip.inset(10.0), label, label_px, Weight::SemiBold, live.state.foreground(), Align::Center);

    let (lo, hi) = cfg.band();
    let target = STRINGS
        .live_target
        .replace("{lo}", &fmt_num(lo, 0))
        .replace("{hi}", &fmt_num(hi, 0));
    let tx = chip.right() + 24.0;
    ui.text(R::new(tx, strip.y, strip.right() - 20.0 - tx, strip.h), &target, 22.0, Weight::Regular, TEXT, Align::Right);
}

fn body_left(ui: &mut Ui<'_>, cfg: &Config) {
    let x = LEFT_X;
    let h = &cfg.hud;
    let mut y = ui.card(x, VIEW_TOP, hud_card_h(), STRINGS.section_hud);

    ui.row_label(x, y, STRINGS.hud_enabled, true);
    ui.on_off(ctrl_rect(x, y), h.enabled, Control::HudEnabled);
    y += ROW_H;

    ui.row_label(x, y, STRINGS.placement, true);
    let p = h.placement;
    ui.segmented(
        ctrl_rect(x, y),
        &[
            (STRINGS.placement_head, Control::Placement(Placement::Head), p == Placement::Head),
            (STRINGS.placement_left_wrist, Control::Placement(Placement::LeftWrist), p == Placement::LeftWrist),
            (STRINGS.placement_right_wrist, Control::Placement(Placement::RightWrist), p == Placement::RightWrist),
        ],
        true,
    );
    y += ROW_H;

    ui.row_label(x, y, STRINGS.display, true);
    let d = h.display;
    ui.segmented(
        ctrl_rect(x, y),
        &[
            (STRINGS.display_color, Control::Display(DisplayMode::Color), d == DisplayMode::Color),
            (STRINGS.display_detailed, Control::Display(DisplayMode::Detailed), d == DisplayMode::Detailed),
        ],
        true,
    );
    y += ROW_H;

    ui.row_label(x, y, STRINGS.size, true);
    let cm = h.size_m * 100.0;
    let cm_text = if (cm - cm.round()).abs() < 0.05 { fmt_num(cm, 0) } else { fmt_num(cm, 1) };
    ui.stepper(
        ctrl_rect(x, y),
        &fill_v(STRINGS.fmt_cm, &cm_text),
        &[("\u{2212}", Control::Size(-1))],
        &[("+", Control::Size(1))],
        true,
    );
    y += ROW_H;

    ui.row_label(x, y, STRINGS.opacity, true);
    ui.stepper(
        ctrl_rect(x, y),
        &fill_v(STRINGS.fmt_percent, &fmt_num(h.opacity * 100.0, 0)),
        &[("\u{2212}", Control::Opacity(-1))],
        &[("+", Control::Opacity(1))],
        true,
    );
    y += ROW_H;

    ui.row_label(x, y, STRINGS.position, true);
    let grid = ctrl_rect(x, y);
    let bw = (grid.w - 8.0) / 3.0;
    let nudges = [
        [(STRINGS.nudge_up, Axis::Y, 1), (STRINGS.nudge_left, Axis::X, -1), (STRINGS.nudge_closer, Axis::Z, 1)],
        [(STRINGS.nudge_down, Axis::Y, -1), (STRINGS.nudge_right, Axis::X, 1), (STRINGS.nudge_further, Axis::Z, -1)],
    ];
    for (row, items) in nudges.iter().enumerate() {
        for (col, &(label, axis, dir)) in items.iter().enumerate() {
            let r = R::new(grid.x + col as f32 * (bw + 4.0), grid.y + row as f32 * ROW_H, bw, CTRL_H);
            ui.button(r, label, Control::Nudge(axis, dir), true, false);
        }
    }
    y += 2.0 * ROW_H;
    let readout = STRINGS
        .nudge_readout
        .replace("{x}", &fmt_signed(h.nudge_x_m))
        .replace("{y}", &fmt_signed(h.nudge_y_m))
        .replace("{z}", &fmt_signed(h.nudge_z_m));
    ui.text(R::new(x + CTRL_DX, y - 4.0, CTRL_W, 28.0), &readout, 17.0, Weight::Regular, TEXT_DIM, Align::Center);

    let t = &cfg.target;
    let mut y = ui.card(x, VIEW_TOP + hud_card_h() + CARD_GAP, target_card_h(), STRINGS.section_target);
    ui.row_label(x, y, STRINGS.preset, true);
    ui.segmented(
        ctrl_rect(x, y),
        &[
            (STRINGS.preset_feminine, Control::Preset(Preset::Feminine), t.preset == Preset::Feminine),
            (STRINGS.preset_masculine, Control::Preset(Preset::Masculine), t.preset == Preset::Masculine),
            (STRINGS.preset_custom, Control::Preset(Preset::Custom), t.preset == Preset::Custom),
        ],
        true,
    );
    y += ROW_H;
    let custom = t.preset == Preset::Custom;
    for (label, value, make) in [
        (STRINGS.custom_low, t.custom_low_hz, Control::CustomLow as fn(i8) -> Control),
        (STRINGS.custom_high, t.custom_high_hz, Control::CustomHigh),
    ] {
        ui.row_label(x, y, label, custom);
        ui.stepper(
            ctrl_rect(x, y),
            &fmt_hz(value),
            &[("\u{2212}10", make(-10)), ("\u{2212}1", make(-1))],
            &[("+1", make(1)), ("+10", make(10))],
            custom,
        );
        y += ROW_H;
    }
}

fn body_right(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo) {
    let x = RIGHT_X;
    let r = &cfg.reminder;
    let mut y = ui.card(x, VIEW_TOP, reminder_card_h(), STRINGS.section_reminder);
    ui.row_label(x, y, STRINGS.reminder_enabled, true);
    ui.on_off(ctrl_rect(x, y), r.enabled, Control::ReminderEnabled);
    y += ROW_H;
    let rows: [(&str, String, fn(i8) -> Control); 3] = [
        (STRINGS.reminder_delay, fill_v(STRINGS.fmt_seconds, &fmt_num(r.delay_s, 1)), Control::ReminderDelay),
        (STRINGS.reminder_pitch, fmt_hz(r.pitch_hz), Control::ReminderPitch),
        (STRINGS.reminder_volume, fill_v(STRINGS.fmt_percent, &fmt_num(r.volume * 100.0, 0)), Control::ReminderVolume),
    ];
    for (label, value, make) in rows {
        ui.row_label(x, y, label, true);
        ui.stepper(ctrl_rect(x, y), &value, &[("\u{2212}", make(-1))], &[("+", make(1))], true);
        y += ROW_H;
    }

    let rest_y = VIEW_TOP + reminder_card_h() + CARD_GAP;
    let y = ui.card(x, rest_y, rest_card_h(), STRINGS.section_vocal_rest);
    ui.row_label(x, y, STRINGS.vocal_rest, true);
    let minutes = cfg.vocal_rest.minutes;
    let value = if minutes == 0 {
        STRINGS.off.to_string()
    } else {
        STRINGS.vocal_rest_limit.replace("{min}", &minutes.to_string())
    };
    ui.stepper(
        ctrl_rect(x, y),
        &value,
        &[("\u{2212}", Control::VocalRest(-1))],
        &[("+", Control::VocalRest(1))],
        true,
    );

    let sys_y = rest_y + rest_card_h() + CARD_GAP;
    let mut y = ui.card(x, sys_y, system_card_h(), STRINGS.section_system);
    let mode = cfg.audio.mic_mode;
    let full = R::new(x + PAD - 6.0, y, COL_W - 2.0 * PAD + 12.0, RADIO_H);
    ui.radio(full, STRINGS.mic_bypass, Control::MicMode(MicMode::Bypass), mode == MicMode::Bypass);
    y += RADIO_H;
    ui.radio(R::new(full.x, y, full.w, RADIO_H), STRINGS.mic_processed, Control::MicMode(MicMode::Processed), mode == MicMode::Processed);
    y += RADIO_H + 4.0;
    ui.paragraph(
        R::new(x + PAD, y, COL_W - 2.0 * PAD, STATUS_LINES as f32 * STATUS_LINE_H),
        &live.mic_status_line,
        STATUS_LINES,
        STATUS_LINE_H,
        TEXT_DIM,
    );
    y += STATUS_LINES as f32 * STATUS_LINE_H + 4.0;
    ui.row_label(x, y, STRINGS.autostart, true);
    ui.on_off(ctrl_rect(x, y), live.autostart, Control::Autostart);
}

/// Quantised live values that influence the pixels.
#[derive(Clone, PartialEq, Debug)]
struct LiveKey {
    f0: Option<i32>,
    state: HudState,
    autostart: bool,
}

impl LiveKey {
    fn of(live: &LiveInfo) -> Self {
        Self {
            f0: live.f0_hz.filter(|f| f.is_finite()).map(|f| f.round() as i32),
            state: live.state,
            autostart: live.autostart,
        }
    }
}

/// The dashboard settings panel.
pub struct SettingsPanel {
    pixmap: Pixmap,
    text: TextRenderer,
    widgets: Vec<Widget>,
    wrap: Vec<(usize, usize)>,
    pointer: Option<(f32, f32)>,
    hover: Option<usize>,
    scroll_px: f32,
    cfg: Option<Config>,
    autostart: bool,
    /// What the pixels currently show; `None` forces a render.
    last: Option<(LiveKey, Option<usize>, f32)>,
    last_status: String,
    dirty: bool,
}

impl Default for SettingsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsPanel {
    pub fn new() -> Self {
        Self {
            pixmap: Pixmap::new(SETTINGS_WIDTH_PX, SETTINGS_HEIGHT_PX).expect("non-zero panel size"),
            text: TextRenderer::new(),
            widgets: Vec::with_capacity(64),
            wrap: Vec::with_capacity(8),
            pointer: None,
            hover: None,
            scroll_px: 0.0,
            cfg: None,
            autostart: false,
            last: None,
            last_status: String::new(),
            dirty: true,
        }
    }

    /// Forces the next [`SettingsPanel::render`] to return a frame (e.g. after the overlay
    /// texture was lost).
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Renders the panel; `None` when the pixels would not change.
    pub fn render(&mut self, cfg: &Config, live: &LiveInfo) -> Option<Frame<'_>> {
        let key = (LiveKey::of(live), self.hover, self.scroll_px);
        let unchanged = !self.dirty
            && self.cfg.as_ref() == Some(cfg)
            && self.last.as_ref() == Some(&key)
            && self.last_status == live.mic_status_line;
        if unchanged {
            return None;
        }
        self.dirty = false;
        self.last = Some(key);
        if self.last_status != live.mic_status_line {
            self.last_status.clear();
            self.last_status.push_str(&live.mic_status_line);
        }
        if self.cfg.as_ref() != Some(cfg) {
            self.cfg = Some(cfg.clone());
        }
        self.autostart = live.autostart;

        self.pixmap.fill(Color::from_rgba8(BG.r, BG.g, BG.b, 255));
        self.widgets.clear();
        let mut ui = Ui {
            pm: &mut self.pixmap,
            tx: &mut self.text,
            widgets: &mut self.widgets,
            wrap: &mut self.wrap,
            pointer: self.pointer,
            dy: 0.0,
            in_body: true,
        };
        build(&mut ui, cfg, live, self.scroll_px);
        Some(Frame {
            width: SETTINGS_WIDTH_PX,
            height: SETTINGS_HEIGHT_PX,
            rgba: self.pixmap.data(),
            premultiplied: true,
        })
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.widgets.iter().position(|w| {
            w.enabled && w.rect.contains(x, y) && (!w.in_body || y >= VIEW_TOP)
        })
    }

    /// Pointer moved to panel pixel (x, y), top-left origin.
    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        self.hover = self.hit(x, y);
    }

    /// Pointer left the panel.
    pub fn pointer_leave(&mut self) {
        self.pointer = None;
        self.hover = None;
    }

    /// Click at panel pixel (x, y), top-left origin. Returns the action to perform; config edits
    /// are also applied to the panel's copy so rapid clicks accumulate before the next render.
    pub fn click(&mut self, x: f32, y: f32) -> Option<Action> {
        let control = self.widgets[self.hit(x, y)?].control;
        let action = apply(control, self.cfg.as_ref()?, self.autostart)?;
        match &action {
            Action::SetConfig(c) => {
                self.cfg = Some(c.clone());
                self.dirty = true;
            }
            Action::SetAutostart(on) => {
                self.autostart = *on;
                self.dirty = true;
            }
            Action::Quit => {}
        }
        Some(action)
    }

    /// Scrolls the body; positive `dy` (wheel up / swipe down) moves towards the top.
    pub fn scroll(&mut self, dy: f32) {
        let max = (content_height() - (H - VIEW_TOP)).max(0.0);
        let s = (self.scroll_px - dy * SCROLL_STEP_PX).clamp(0.0, max);
        if s != self.scroll_px {
            self.scroll_px = s;
            if let Some((x, y)) = self.pointer {
                self.hover = self.hit(x, y);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> LiveInfo {
        LiveInfo {
            f0_hz: Some(182.0),
            state: HudState::InRange,
            mic_status_line: "Bypass \u{00b7} recording from Internal Microphone".into(),
            autostart: false,
        }
    }

    /// Renders and returns the centre of the first enabled widget with `control`.
    fn center_of(panel: &SettingsPanel, control: Control) -> (f32, f32) {
        let w = panel
            .widgets
            .iter()
            .find(|w| w.control == control)
            .unwrap_or_else(|| panel_missing(control));
        (w.rect.cx(), w.rect.cy())
    }

    fn panel_missing(control: Control) -> ! {
        panic!("no widget for {control:?}")
    }

    fn click_control(cfg: &Config, live: &LiveInfo, control: Control) -> Option<Action> {
        let mut panel = SettingsPanel::new();
        panel.render(cfg, live).expect("first render");
        let (x, y) = center_of(&panel, control);
        panel.click(x, y)
    }

    fn expect_cfg(cfg: &Config, control: Control, edit: impl FnOnce(&mut Config)) {
        let mut want = cfg.clone();
        edit(&mut want);
        assert_eq!(
            click_control(cfg, &live(), control),
            Some(Action::SetConfig(want)),
            "{control:?}"
        );
    }

    #[test]
    fn every_control_hits_and_produces_its_action() {
        let base = Config::default();
        expect_cfg(&base, Control::HudEnabled(false), |c| c.hud.enabled = false);
        expect_cfg(&base, Control::Placement(Placement::LeftWrist), |c| c.hud.placement = Placement::LeftWrist);
        expect_cfg(&base, Control::Placement(Placement::RightWrist), |c| c.hud.placement = Placement::RightWrist);
        expect_cfg(&base, Control::Display(DisplayMode::Detailed), |c| c.hud.display = DisplayMode::Detailed);
        expect_cfg(&base, Control::Size(1), |c| c.hud.size_m = 0.06);
        expect_cfg(&base, Control::Size(-1), |c| c.hud.size_m = 0.04);
        expect_cfg(&base, Control::Opacity(1), |c| c.hud.opacity = 1.0);
        expect_cfg(&base, Control::Opacity(-1), |c| c.hud.opacity = 0.8);
        expect_cfg(&base, Control::Nudge(Axis::Y, 1), |c| c.hud.nudge_y_m = 0.01);
        expect_cfg(&base, Control::Nudge(Axis::Y, -1), |c| c.hud.nudge_y_m = -0.01);
        expect_cfg(&base, Control::Nudge(Axis::X, -1), |c| c.hud.nudge_x_m = -0.01);
        expect_cfg(&base, Control::Nudge(Axis::X, 1), |c| c.hud.nudge_x_m = 0.01);
        expect_cfg(&base, Control::Nudge(Axis::Z, 1), |c| c.hud.nudge_z_m = 0.01);
        expect_cfg(&base, Control::Nudge(Axis::Z, -1), |c| c.hud.nudge_z_m = -0.01);
        expect_cfg(&base, Control::Preset(Preset::Masculine), |c| c.target.preset = Preset::Masculine);
        expect_cfg(&base, Control::Preset(Preset::Custom), |c| c.target.preset = Preset::Custom);
        expect_cfg(&base, Control::ReminderEnabled(true), |c| c.reminder.enabled = true);
        expect_cfg(&base, Control::ReminderDelay(1), |c| c.reminder.delay_s = 2.5);
        expect_cfg(&base, Control::ReminderDelay(-1), |c| c.reminder.delay_s = 1.5);
        expect_cfg(&base, Control::ReminderPitch(1), |c| c.reminder.pitch_hz = 450.0);
        expect_cfg(&base, Control::ReminderPitch(-1), |c| c.reminder.pitch_hz = 430.0);
        expect_cfg(&base, Control::ReminderVolume(1), |c| c.reminder.volume = 0.6);
        expect_cfg(&base, Control::ReminderVolume(-1), |c| c.reminder.volume = 0.4);
        expect_cfg(&base, Control::VocalRest(1), |c| c.vocal_rest.minutes = 5);
        expect_cfg(&base, Control::MicMode(MicMode::Processed), |c| c.audio.mic_mode = MicMode::Processed);

        let mut custom = base.clone();
        custom.target.preset = Preset::Custom;
        custom.target.custom_low_hz = 150.0;
        custom.target.custom_high_hz = 250.0;
        expect_cfg(&custom, Control::CustomLow(-10), |c| c.target.custom_low_hz = 140.0);
        expect_cfg(&custom, Control::CustomLow(-1), |c| c.target.custom_low_hz = 149.0);
        expect_cfg(&custom, Control::CustomLow(1), |c| c.target.custom_low_hz = 151.0);
        expect_cfg(&custom, Control::CustomLow(10), |c| c.target.custom_low_hz = 160.0);
        expect_cfg(&custom, Control::CustomHigh(-10), |c| c.target.custom_high_hz = 240.0);
        expect_cfg(&custom, Control::CustomHigh(-1), |c| c.target.custom_high_hz = 249.0);
        expect_cfg(&custom, Control::CustomHigh(1), |c| c.target.custom_high_hz = 251.0);
        expect_cfg(&custom, Control::CustomHigh(10), |c| c.target.custom_high_hz = 260.0);
        let mut rest = base.clone();
        rest.vocal_rest.minutes = 30;
        expect_cfg(&rest, Control::VocalRest(-1), |c| c.vocal_rest.minutes = 25);

        let mut off = base.clone();
        off.hud.enabled = false;
        off.audio.mic_mode = MicMode::Processed;
        off.hud.placement = Placement::LeftWrist;
        off.hud.display = DisplayMode::Detailed;
        off.target.preset = Preset::Masculine;
        off.reminder.enabled = true;
        expect_cfg(&off, Control::HudEnabled(true), |c| c.hud.enabled = true);
        expect_cfg(&off, Control::MicMode(MicMode::Bypass), |c| c.audio.mic_mode = MicMode::Bypass);
        expect_cfg(&off, Control::Placement(Placement::Head), |c| c.hud.placement = Placement::Head);
        expect_cfg(&off, Control::Display(DisplayMode::Color), |c| c.hud.display = DisplayMode::Color);
        expect_cfg(&off, Control::Preset(Preset::Feminine), |c| c.target.preset = Preset::Feminine);
        expect_cfg(&off, Control::ReminderEnabled(false), |c| c.reminder.enabled = false);

        assert_eq!(click_control(&base, &live(), Control::Autostart(true)), Some(Action::SetAutostart(true)));
        let mut on = live();
        on.autostart = true;
        assert_eq!(click_control(&base, &on, Control::Autostart(false)), Some(Action::SetAutostart(false)));
        assert_eq!(click_control(&base, &live(), Control::Quit), Some(Action::Quit));
    }

    #[test]
    fn every_control_is_reachable_and_inside_the_viewport() {
        let mut panel = SettingsPanel::new();
        let mut cfg = Config::default();
        cfg.target.preset = Preset::Custom;
        panel.render(&cfg, &live()).unwrap();
        for (i, w) in panel.widgets.iter().enumerate() {
            assert!(w.rect.x >= 0.0 && w.rect.right() <= W && w.rect.y >= 0.0 && w.rect.bottom() <= H, "{w:?}");
            let (x, y) = (w.rect.cx(), w.rect.cy());
            assert_eq!(panel.hit(x, y), Some(i), "{:?} overlaps another control", w.control);
        }
        // 17 HUD (2+3+2+2+2+6), 11 target (3+4+4), 8 reminder, 2 vocal rest, 2 mic, 2 autostart, quit.
        assert_eq!(panel.widgets.len(), 43);
    }

    #[test]
    fn clicks_outside_controls_do_nothing() {
        let mut panel = SettingsPanel::new();
        panel.render(&Config::default(), &live()).unwrap();
        assert_eq!(panel.click(5.0, 5.0), None);
        assert_eq!(panel.click(600.0, 120.0), None);
        assert_eq!(panel.click(-10.0, 9000.0), None);
    }

    #[test]
    fn clicking_the_selected_option_is_a_no_op() {
        let base = Config::default();
        assert_eq!(click_control(&base, &live(), Control::HudEnabled(true)), None);
        assert_eq!(click_control(&base, &live(), Control::Placement(Placement::Head)), None);
        assert_eq!(click_control(&base, &live(), Control::Autostart(false)), None);
    }

    #[test]
    fn disabled_custom_steppers_do_not_fire() {
        let base = Config::default(); // feminine preset
        assert_eq!(click_control(&base, &live(), Control::CustomLow(1)), None);
        assert_eq!(click_control(&base, &live(), Control::CustomHigh(-10)), None);
    }

    #[test]
    fn steppers_clamp_to_spec_ranges() {
        let mut c = Config::default();
        c.hud.size_m = 0.20;
        assert_eq!(apply(Control::Size(1), &c, false), None);
        c.hud.size_m = 0.02;
        assert_eq!(apply(Control::Size(-1), &c, false), None);
        c.hud.opacity = 0.1;
        assert_eq!(apply(Control::Opacity(-1), &c, false), None);
        c.hud.opacity = 1.0;
        assert_eq!(apply(Control::Opacity(1), &c, false), None);
        c.hud.nudge_z_m = -0.5;
        assert_eq!(apply(Control::Nudge(Axis::Z, -1), &c, false), None);
        c.hud.nudge_x_m = 0.5;
        assert_eq!(apply(Control::Nudge(Axis::X, 1), &c, false), None);
        c.reminder.delay_s = 10.0;
        assert_eq!(apply(Control::ReminderDelay(1), &c, false), None);
        c.reminder.delay_s = 0.5;
        assert_eq!(apply(Control::ReminderDelay(-1), &c, false), None);
        c.reminder.pitch_hz = 1000.0;
        assert_eq!(apply(Control::ReminderPitch(1), &c, false), None);
        c.reminder.pitch_hz = 100.0;
        assert_eq!(apply(Control::ReminderPitch(-1), &c, false), None);
        c.reminder.volume = 0.0;
        assert_eq!(apply(Control::ReminderVolume(-1), &c, false), None);
        c.reminder.volume = 1.0;
        assert_eq!(apply(Control::ReminderVolume(1), &c, false), None);
        c.vocal_rest.minutes = 50;
        assert_eq!(apply(Control::VocalRest(1), &c, false), None);
        c.vocal_rest.minutes = 0;
        assert_eq!(apply(Control::VocalRest(-1), &c, false), None);

        // Partial steps land exactly on the limit.
        c.hud.size_m = 0.195;
        let Some(Action::SetConfig(n)) = apply(Control::Size(1), &c, false) else { panic!() };
        assert_eq!(n.hud.size_m, 0.20);
        c.reminder.pitch_hz = 995.0;
        let Some(Action::SetConfig(n)) = apply(Control::ReminderPitch(1), &c, false) else { panic!() };
        assert_eq!(n.reminder.pitch_hz, 1000.0);
    }

    #[test]
    fn vocal_rest_steps_between_off_and_5_to_50() {
        assert_eq!(step_vocal_rest(0, 1), 5);
        assert_eq!(step_vocal_rest(5, -1), 0);
        assert_eq!(step_vocal_rest(5, 1), 10);
        assert_eq!(step_vocal_rest(7, 1), 10);
        assert_eq!(step_vocal_rest(7, -1), 5);
        assert_eq!(step_vocal_rest(50, 1), 50);
        assert_eq!(step_vocal_rest(50, -1), 45);
    }

    #[test]
    fn custom_band_keeps_gap_and_detection_span() {
        let mut c = Config::default();
        c.target.preset = Preset::Custom;
        c.target.custom_low_hz = 200.0;
        c.target.custom_high_hz = 212.0;
        // Low may rise to high − 10 but not beyond.
        let Some(Action::SetConfig(n)) = apply(Control::CustomLow(10), &c, false) else { panic!() };
        assert_eq!(n.target.custom_low_hz, 202.0);
        let mut at_gap = c.clone();
        at_gap.target.custom_low_hz = 202.0;
        assert_eq!(apply(Control::CustomLow(1), &at_gap, false), None);
        assert_eq!(apply(Control::CustomHigh(-1), &at_gap, false), None);
        // High may fall to low + 10.
        let Some(Action::SetConfig(n)) = apply(Control::CustomHigh(-10), &c, false) else { panic!() };
        assert_eq!(n.target.custom_high_hz, 210.0);
        // Detection span limits.
        c.target.custom_low_hz = 65.0;
        let Some(Action::SetConfig(n)) = apply(Control::CustomLow(-10), &c, false) else { panic!() };
        assert_eq!(n.target.custom_low_hz, c.audio.detect_min_hz);
        c.target.custom_high_hz = 495.0;
        let Some(Action::SetConfig(n)) = apply(Control::CustomHigh(10), &c, false) else { panic!() };
        assert_eq!(n.target.custom_high_hz, c.audio.detect_max_hz);
    }

    #[test]
    fn rapid_clicks_accumulate_before_rerender() {
        let mut panel = SettingsPanel::new();
        let cfg = Config::default();
        panel.render(&cfg, &live()).unwrap();
        let (x, y) = center_of(&panel, Control::Size(1));
        panel.click(x, y).unwrap();
        let Some(Action::SetConfig(c)) = panel.click(x, y) else { panic!() };
        assert_eq!(c.hud.size_m, 0.07);
    }

    #[test]
    fn render_none_when_unchanged_some_on_changes() {
        let mut panel = SettingsPanel::new();
        let cfg = Config::default();
        let mut l = live();
        assert!(panel.render(&cfg, &l).is_some());
        assert!(panel.render(&cfg, &l).is_none());
        l.f0_hz = Some(182.3);
        assert!(panel.render(&cfg, &l).is_none(), "same displayed Hz");
        l.f0_hz = Some(190.0);
        assert!(panel.render(&cfg, &l).is_some());
        l.mic_status_line.push_str(" (fallback)");
        assert!(panel.render(&cfg, &l).is_some());
        let mut c2 = cfg.clone();
        c2.hud.opacity = 0.5;
        assert!(panel.render(&c2, &l).is_some());
        assert!(panel.render(&c2, &l).is_none());
        // Hover changes re-render; moving within the same control does not.
        let (x, y) = center_of(&panel, Control::Quit);
        panel.pointer_move(x, y);
        assert!(panel.render(&c2, &l).is_some());
        panel.pointer_move(x + 1.0, y);
        assert!(panel.render(&c2, &l).is_none());
        panel.invalidate();
        assert!(panel.render(&c2, &l).is_some());
    }

    #[test]
    fn layout_fits_without_scrolling() {
        assert!(content_height() <= H - VIEW_TOP, "content {} > viewport", content_height());
        let mut panel = SettingsPanel::new();
        panel.scroll(-3.0);
        assert_eq!(panel.scroll_px, 0.0);
    }
}
