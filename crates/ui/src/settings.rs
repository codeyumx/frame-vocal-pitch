//! Dashboard settings panel (§4.2), 1200 × 800 px, with three pages chosen by tabs in the header
//! ("General", "Resonance (experimental)", §5.1, and "About & updates", §7.1) and the modal
//! resonance explanation card.
//!
//! Coordinates passed to [`SettingsPanel::pointer_move`] and [`SettingsPanel::click`] are panel
//! pixels with the origin at the **top-left** corner (OpenVR mouse events use a bottom-left
//! origin; flip `y` before calling).

use std::time::{Duration, Instant};

use fvp_config::{Config, DisplayMode, MicMode, Placement, Preset, CUSTOM_MIN_GAP_HZ};
use tiny_skia::{Color, Pixmap};

use crate::draw::{self, Rgba, R};
use crate::hud::{HudState, ResonanceView};
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

/// Which calibration recording (§5.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CalibWhich {
    Relaxed,
    Target,
}

/// State of one calibration recording (§5.1).
#[derive(Clone, PartialEq, Debug, Default)]
pub enum CalibStatus {
    /// Not recorded in this session; the panel shows the value stored in the config, if any.
    #[default]
    NotRecorded,
    /// Recording in progress; `progress` runs from 0 to 1 over the 10 s.
    Recording { progress: f32 },
    /// Finished: median formant spacing ΔF of the accepted frames.
    Done { df_hz: f32 },
    /// Failed; the message is shown as is (e.g. [`crate::Strings::calib_not_enough_speech`]).
    Failed(String),
}

/// Live resonance data for the settings panel.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResonanceLive {
    /// Current indicator state; `None` while resonance feedback is off.
    pub position: Option<ResonanceView>,
    pub relaxed: CalibStatus,
    pub target: CalibStatus,
}

/// What the Updates section shows (§4.2, §7.1); the text is [`update_status_text`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum UpdateStatus {
    /// No check result yet (automatic checks off, or the first one has not finished).
    #[default]
    NotChecked,
    /// The last check found no newer release; minutes since that check.
    UpToDate { checked_ago_min: u64 },
    /// The last check found this newer version.
    Available(String),
    Checking,
    /// The updater runs for this version (the service will restart the app).
    Updating(String),
    /// The updater runs and the app was started by hand, so the installer will stop it.
    UpdatingManual,
    /// Another updater unit was already active.
    AlreadyRunning,
    UpdateFailed(String),
    CheckFailed(String),
}

impl UpdateStatus {
    /// An updater is running: nothing may be started.
    fn updating(&self) -> bool {
        matches!(self, UpdateStatus::Updating(_) | UpdateStatus::UpdatingManual | UpdateStatus::AlreadyRunning)
    }
}

/// Live state of the Updates section.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct UpdateLive {
    /// The running version, `X.Y.Z`.
    pub version: &'static str,
    pub status: UpdateStatus,
    /// Newer release offered by the "Update to X.Y.Z" button; `None` hides the button.
    pub available: Option<String>,
}

/// Live data shown in the panel (live strip, mic status line, autostart state, resonance,
/// updates).
#[derive(Clone, PartialEq, Debug)]
pub struct LiveInfo {
    pub f0_hz: Option<f32>,
    pub state: HudState,
    /// §6.3 status line: mode, node recorded from, fallback reason, filter state.
    pub mic_status_line: String,
    /// Whether the systemd user unit is enabled.
    pub autostart: bool,
    pub resonance: ResonanceLive,
    pub update: UpdateLive,
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
    /// Start a 10 s calibration recording (§5.1).
    StartCalibration(CalibWhich),
    /// "Check now": ask GitHub for the latest release, ignoring the cache (§7.1).
    CheckUpdates,
    /// "Update to X.Y.Z" confirmed by "Confirm update": install this version (`X.Y.Z`).
    StartUpdate(String),
}

/// How long "Confirm update" waits for the second click (§4.2).
pub const UPDATE_CONFIRM_WINDOW: Duration = Duration::from_secs(10);

/// The two-step "Update to X.Y.Z" → "Confirm update" gate.
#[derive(Clone, Debug, Default)]
struct Confirm {
    version: String,
    /// End of the confirmation window; `None` = not armed.
    until: Option<Instant>,
}

impl Confirm {
    fn armed(&self, version: &str, now: Instant) -> bool {
        self.until.is_some_and(|t| now < t) && self.version == version
    }

    /// First press arms the gate for [`UPDATE_CONFIRM_WINDOW`]; a second press for the same
    /// version inside the window returns `true` and disarms it.
    fn press(&mut self, version: &str, now: Instant) -> bool {
        if self.armed(version, now) {
            self.until = None;
            return true;
        }
        self.version.clear();
        self.version.push_str(version);
        self.until = Some(now + UPDATE_CONFIRM_WINDOW);
        false
    }

    /// Drops an elapsed window.
    fn expire(&mut self, now: Instant) {
        if self.until.is_some_and(|t| now >= t) {
            self.until = None;
        }
    }
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
    /// "Check now".
    CheckUpdates,
    /// "Update to X.Y.Z" / "Confirm update" (two-step, panel-internal until confirmed).
    Update,
    UpdateAuto(bool),
    /// Switch the panel page (panel-internal).
    Tab(Page),
    /// `true` opens the explanation card first; `false` turns resonance off directly.
    ResonanceEnabled(bool),
    /// Re-opens the explanation card (panel-internal).
    HowItWorks,
    Record(CalibWhich),
    /// Explanation card: "Turn on".
    CardTurnOn,
    /// Explanation card: "Keep off" / "Close" (panel-internal).
    CardClose,
}

/// Pages of the panel, chosen with the tabs in the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page {
    General,
    Resonance,
    Updates,
}

/// Which variant of the explanation card is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Card {
    /// Opened by turning resonance on: "Turn on" / "Keep off".
    Enable,
    /// Opened by "How this works": "Close".
    Info,
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
        Control::Record(which) => return Some(Action::StartCalibration(which)),
        Control::CheckUpdates => return Some(Action::CheckUpdates),
        Control::Tab(_) | Control::HowItWorks | Control::CardClose | Control::Update => return None,
        Control::UpdateAuto(on) => c.update.check = on,
        Control::ResonanceEnabled(on) => c.resonance.enabled = on,
        Control::CardTurnOn => c.resonance.enabled = true,
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
/// Left edge of the page tabs in the header.
const TABS_X: f32 = 290.0;

// Resonance page.
const RES_INTRO_LINES: usize = 5;
const RES_INTRO_LINE_H: f32 = 24.0;
const RES_NOW_H: f32 = 30.0;
const RES_POS_LINES: usize = 3;
const RES_POS_LINE_H: f32 = 30.0;
const CALIB_BTN_H: f32 = 46.0;
const CALIB_STATUS_H: f32 = 60.0;
const CALIB_HINT_LINES: usize = 4;
/// Length of a calibration recording (§5.1), for the "s left" readout.
const CALIBRATION_S: f32 = 10.0;

// Updates page.
const UPD_STATUS_LINES: usize = 2;
const UPD_STATUS_LINE_H: f32 = 30.0;
const UPD_HINT_LINES: usize = 3;
const UPD_HINT_LINE_H: f32 = 24.0;
const ABOUT_NAME_H: f32 = 40.0;
const ABOUT_LINE_H: f32 = 30.0;

// Explanation card (modal over the whole panel).
const CARD_R: R = R::new(150.0, 96.0, 900.0, 608.0);
/// Scrollable text viewport inside the card.
const CARD_TEXT: R = R::new(CARD_R.x + 44.0, CARD_R.y + 92.0, CARD_R.w - 100.0, 400.0);
const CARD_TEXT_PX: f32 = 25.0;
const CARD_LINE_H: f32 = 36.0;
const CARD_PARA_GAP: f32 = 16.0;
const CARD_BULLET_GAP: f32 = 8.0;
const CARD_BULLET_INDENT: f32 = 30.0;
const CARD_BTN: (f32, f32) = (220.0, 56.0);

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
const WARN: Rgba = Rgba::hex(0xF0B860, 255);
const SCRIM: Rgba = Rgba::hex(0x000000, 170);
const DISABLED_ALPHA: u8 = 90;

fn ctrl_rect(col_x: f32, row_y: f32) -> R {
    R::new(col_x + CTRL_DX, row_y + (ROW_H - CTRL_H) / 2.0, CTRL_W, CTRL_H)
}

/// Height of the scrollable content of `page` (from `VIEW_TOP`), used to bound scrolling.
const fn content_height(page: Page) -> f32 {
    match page {
        Page::General => {
            let left = hud_card_h() + CARD_GAP + target_card_h();
            let right = reminder_card_h() + CARD_GAP + rest_card_h() + CARD_GAP + system_card_h();
            (if left > right { left } else { right }) + PAD
        }
        Page::Resonance => resonance_card_h() + PAD,
        Page::Updates => updates_card_h() + PAD,
    }
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
/// Both cards of the resonance page share this height.
const fn resonance_card_h() -> f32 {
    let left = HEAD_H
        + ROW_H
        + RES_INTRO_LINES as f32 * RES_INTRO_LINE_H
        + 8.0
        + ROW_H
        + RES_NOW_H
        + RES_POS_LINES as f32 * RES_POS_LINE_H;
    let right = HEAD_H + 2.0 * (CALIB_BTN_H + 6.0 + CALIB_STATUS_H + 10.0)
        + CALIB_HINT_LINES as f32 * RES_INTRO_LINE_H;
    (if left > right { left } else { right }) + CARD_BOTTOM_PAD
}
/// Both cards of the updates page share this height.
const fn updates_card_h() -> f32 {
    let left = HEAD_H
        + ROW_H
        + UPD_STATUS_LINES as f32 * UPD_STATUS_LINE_H
        + 6.0
        + 2.0 * ROW_H
        + 6.0
        + UPD_HINT_LINES as f32 * UPD_HINT_LINE_H;
    let right = HEAD_H + ABOUT_NAME_H + 3.0 * ABOUT_LINE_H;
    (if left > right { left } else { right }) + CARD_BOTTOM_PAD
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

/// Settings text for the resonance indicator (§5.1). Buckets of p: < 0.25 near relaxed,
/// < 0.75 between, ≤ 1.1 near target, above that beyond target.
fn position_text(view: Option<ResonanceView>) -> &'static str {
    match view {
        None => STRINGS.no_f0,
        Some(ResonanceView::Gathering) => STRINGS.res_gathering,
        Some(ResonanceView::Uncalibrated) => STRINGS.res_uncalibrated,
        Some(ResonanceView::TooSimilar) => STRINGS.res_too_similar,
        Some(ResonanceView::NeedsBypass) => STRINGS.res_needs_bypass,
        Some(ResonanceView::Position(p)) if !p.is_finite() => STRINGS.res_gathering,
        Some(ResonanceView::Position(p)) if p < 0.25 => STRINGS.res_near_relaxed,
        Some(ResonanceView::Position(p)) if p < 0.75 => STRINGS.res_between,
        Some(ResonanceView::Position(p)) if p <= 1.1 => STRINGS.res_near_target,
        Some(ResonanceView::Position(_)) => STRINGS.res_beyond_target,
    }
}

/// "2 h ago" etc. for the "Up to date" status line.
fn ago_text(minutes: u64) -> String {
    let s = STRINGS;
    let (template, n) = match minutes {
        0 => return s.upd_ago_now.to_string(),
        1..=59 => (s.upd_ago_min, minutes),
        60..=2879 => (s.upd_ago_h, minutes / 60),
        _ => (s.upd_ago_d, minutes / (24 * 60)),
    };
    template.replace("{n}", &n.to_string())
}

/// The Updates status line (§4.2).
pub fn update_status_text(status: &UpdateStatus) -> String {
    let s = STRINGS;
    match status {
        UpdateStatus::NotChecked => s.upd_not_checked.to_string(),
        UpdateStatus::UpToDate { checked_ago_min } => s.upd_up_to_date.replace("{ago}", &ago_text(*checked_ago_min)),
        UpdateStatus::Available(v) => fill_v(s.upd_available, v),
        UpdateStatus::Checking => s.upd_checking.to_string(),
        UpdateStatus::Updating(v) => fill_v(s.upd_updating, v),
        UpdateStatus::UpdatingManual => s.upd_updating_manual.to_string(),
        UpdateStatus::AlreadyRunning => s.upd_already_running.to_string(),
        UpdateStatus::UpdateFailed(reason) => s.upd_failed.replace("{reason}", reason),
        UpdateStatus::CheckFailed(reason) => s.upd_check_failed.replace("{reason}", reason),
    }
}

/// One line of the laid-out explanation card text.
struct CardLine<'s> {
    /// Top of the line relative to the top of the text.
    y: f32,
    indent: f32,
    weight: Weight,
    text: &'s str,
    /// First line of a bullet item (draw the bullet).
    bullet: bool,
}

/// Lays out the explanation card text (§5.1 "User guidance") at [`CARD_TEXT`] width, calling
/// `emit` per line; returns the total text height.
fn layout_card_text(
    tx: &mut TextRenderer,
    wrap: &mut Vec<(usize, usize)>,
    mut emit: impl FnMut(&mut TextRenderer, CardLine<'static>),
) -> f32 {
    let s = STRINGS;
    let paragraphs = [
        (Weight::Regular, false, s.card_what),
        (Weight::Regular, false, s.card_others),
        (Weight::Regular, false, s.card_pitch),
        (Weight::SemiBold, false, s.card_turn_off),
        (Weight::Regular, true, s.card_cases[0]),
        (Weight::Regular, true, s.card_cases[1]),
        (Weight::Regular, true, s.card_cases[2]),
        (Weight::Regular, true, s.card_cases[3]),
    ];
    let mut y = 0.0;
    for (i, (weight, bullet, text)) in paragraphs.into_iter().enumerate() {
        if i > 0 {
            y += if bullet { CARD_BULLET_GAP } else { CARD_PARA_GAP };
        }
        let indent = if bullet { CARD_BULLET_INDENT } else { 0.0 };
        tx.wrap(weight, CARD_TEXT_PX, CARD_TEXT.w - indent, text, wrap);
        for (j, &(a, b)) in wrap.iter().enumerate() {
            let line = CardLine { y, indent, weight, text: &text[a..b], bullet: bullet && j == 0 };
            emit(tx, line);
            y += CARD_LINE_H;
        }
    }
    y
}

/// Largest scroll offset of the explanation card text.
fn card_scroll_max(tx: &mut TextRenderer, wrap: &mut Vec<(usize, usize)>) -> f32 {
    (layout_card_text(tx, wrap, |_, _| {}) - CARD_TEXT.h).max(0.0)
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

    /// Wrapped multi-line text in `r`, shrinking from `max_px` until it fits `max_lines`.
    #[allow(clippy::too_many_arguments)]
    fn paragraph(&mut self, r: R, text: &str, max_lines: usize, line_h: f32, max_px: f32, weight: Weight, color: Rgba) {
        let r = self.place(r);
        let mut px = max_px;
        loop {
            self.tx.wrap(weight, px, r.w, text, self.wrap);
            if self.wrap.len() <= max_lines || px <= 13.0 {
                break;
            }
            px -= 1.0;
        }
        let ascent = TextRenderer::cap_height(weight, px);
        for (i, &(s, e)) in self.wrap.iter().take(max_lines).enumerate() {
            let base = r.y + i as f32 * line_h + (line_h + ascent) / 2.0;
            self.tx.draw(
                self.pm,
                weight,
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

/// Builds (and draws) the whole panel. Body first, then the fixed header on top of it, then the
/// modal explanation card, which alone receives the pointer while open.
fn build(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo, view: ViewState, armed: bool) {
    let pointer = ui.pointer;
    if view.card.is_some() {
        ui.pointer = None;
    }
    ui.in_body = true;
    ui.dy = -view.scroll_px;
    match view.page {
        Page::General => {
            body_left(ui, cfg);
            body_right(ui, cfg, live);
        }
        Page::Resonance => resonance_page(ui, cfg, live),
        Page::Updates => updates_page(ui, cfg, &live.update, armed),
    }

    ui.in_body = false;
    ui.dy = 0.0;
    draw::fill_rounded(ui.pm, R::new(0.0, 0.0, W, VIEW_TOP), 0.0, BG);
    header(ui, cfg, live, view.page);

    if let Some(card) = view.card {
        ui.widgets.clear();
        ui.pointer = pointer;
        explanation_card(ui, card, view.card_scroll_px);
    }
}

fn header(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo, page: Page) {
    // Quit button with its one-line hint (§4.2) under it; the hint column is wider than the button
    // so a ~30 % longer translation still fits at a readable size.
    let quit = R::new(W - M - 170.0, 10.0, 170.0, 42.0);
    let hint = R::new(W - M - 320.0, 54.0, 320.0, 20.0);
    ui.text(R::new(M, 14.0, TABS_X - M - 16.0, 48.0), STRINGS.title, 32.0, Weight::SemiBold, TEXT, Align::Left);
    // Three tabs end left of Quit and above its hint.
    ui.segmented(
        R::new(TABS_X, 12.0, quit.x - 16.0 - TABS_X, quit.bottom() - 12.0),
        &[
            (STRINGS.tab_general, Control::Tab(Page::General), page == Page::General),
            (STRINGS.tab_resonance, Control::Tab(Page::Resonance), page == Page::Resonance),
            (STRINGS.tab_updates, Control::Tab(Page::Updates), page == Page::Updates),
        ],
        true,
    );
    let hover = ui.widget(quit, Control::Quit, true);
    ui.fill(quit, 10.0, if hover { QUIT_HOVER } else { QUIT });
    ui.text(quit.inset(8.0), STRINGS.quit, 22.0, Weight::SemiBold, Rgba::hex(0xFFFFFF, 255), Align::Center);
    let hint_text = if live.autostart { STRINGS.quit_hint_autostart } else { STRINGS.quit_hint };
    ui.text(hint, hint_text, 16.0, Weight::Regular, TEXT_DIM, Align::Right);

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
        17.0,
        Weight::Regular,
        TEXT_DIM,
    );
    y += STATUS_LINES as f32 * STATUS_LINE_H + 4.0;
    ui.row_label(x, y, STRINGS.autostart, true);
    ui.on_off(ctrl_rect(x, y), live.autostart, Control::Autostart);
}

/// Resonance page (§4.2, §5.1): switch, explanation and current position on the left,
/// calibration on the right.
fn resonance_page(ui: &mut Ui<'_>, cfg: &Config, live: &LiveInfo) {
    let on = cfg.resonance.enabled;
    let res = &live.resonance;
    let h = resonance_card_h();
    let inner = COL_W - 2.0 * PAD;

    let x = LEFT_X;
    let mut y = ui.card(x, VIEW_TOP, h, STRINGS.section_resonance);
    ui.row_label(x, y, STRINGS.res_enabled, true);
    ui.on_off(ctrl_rect(x, y), on, Control::ResonanceEnabled);
    y += ROW_H;
    let intro_h = RES_INTRO_LINES as f32 * RES_INTRO_LINE_H;
    ui.paragraph(
        R::new(x + PAD, y, inner, intro_h),
        STRINGS.res_intro,
        RES_INTRO_LINES,
        RES_INTRO_LINE_H,
        18.0,
        Weight::Regular,
        TEXT_DIM,
    );
    y += intro_h + 8.0;
    let how = R::new(x + PAD, y + (ROW_H - CTRL_H) / 2.0, inner, CTRL_H);
    ui.button(how, STRINGS.res_how, Control::HowItWorks, on, false);
    y += ROW_H;
    ui.text(R::new(x + PAD, y, inner, RES_NOW_H), STRINGS.res_now, 18.0, Weight::Regular, TEXT_DIM, Align::Left);
    y += RES_NOW_H;
    let (text, weight, color) = match res.position {
        _ if !on => (STRINGS.res_off_hint, Weight::Regular, TEXT_DIM),
        Some(ResonanceView::Position(p)) if p.is_finite() => (position_text(res.position), Weight::SemiBold, TEXT),
        pos => (position_text(pos), Weight::Regular, TEXT),
    };
    ui.paragraph(
        R::new(x + PAD, y, inner, RES_POS_LINES as f32 * RES_POS_LINE_H),
        text,
        RES_POS_LINES,
        RES_POS_LINE_H,
        23.0,
        weight,
        color,
    );

    let x = RIGHT_X;
    let mut y = ui.card(x, VIEW_TOP, h, STRINGS.section_calibration);
    let busy = [&res.relaxed, &res.target].iter().any(|s| matches!(s, CalibStatus::Recording { .. }));
    let can_record = on && !busy && res.position != Some(ResonanceView::NeedsBypass);
    let calibrations = [
        (CalibWhich::Relaxed, STRINGS.record_relaxed, &res.relaxed, cfg.resonance.relaxed_df_hz),
        (CalibWhich::Target, STRINGS.record_target, &res.target, cfg.resonance.target_df_hz),
    ];
    for (which, label, status, stored) in calibrations {
        let recording = matches!(status, CalibStatus::Recording { .. });
        ui.button(R::new(x + PAD, y, inner, CALIB_BTN_H), label, Control::Record(which), can_record, recording);
        y += CALIB_BTN_H + 6.0;
        calib_status(ui, R::new(x + PAD, y, inner, CALIB_STATUS_H), status, stored, on);
        y += CALIB_STATUS_H + 10.0;
    }
    ui.paragraph(
        R::new(x + PAD, y, inner, CALIB_HINT_LINES as f32 * RES_INTRO_LINE_H),
        STRINGS.calib_hint,
        CALIB_HINT_LINES,
        RES_INTRO_LINE_H,
        18.0,
        Weight::Regular,
        TEXT_DIM,
    );
}

/// Updates page (§4.2, §7.1): version, status line, "Check now", "Update to X.Y.Z" /
/// "Confirm update" and "Check automatically" on the left, about lines on the right.
fn updates_page(ui: &mut Ui<'_>, cfg: &Config, upd: &UpdateLive, armed: bool) {
    let h = updates_card_h();
    let inner = COL_W - 2.0 * PAD;

    let x = LEFT_X;
    let mut y = ui.card(x, VIEW_TOP, h, STRINGS.section_updates);
    ui.row_label(x, y, STRINGS.upd_version, true);
    ui.text(ctrl_rect(x, y), upd.version, 22.0, Weight::SemiBold, TEXT, Align::Left);
    y += ROW_H;
    let failed = matches!(upd.status, UpdateStatus::UpdateFailed(_) | UpdateStatus::CheckFailed(_));
    let status_h = UPD_STATUS_LINES as f32 * UPD_STATUS_LINE_H;
    ui.paragraph(
        R::new(x + PAD, y, inner, status_h),
        &update_status_text(&upd.status),
        UPD_STATUS_LINES,
        UPD_STATUS_LINE_H,
        22.0,
        Weight::SemiBold,
        if failed { WARN } else { TEXT },
    );
    y += status_h + 6.0;
    let updating = upd.status.updating();
    let busy = updating || upd.status == UpdateStatus::Checking;
    let half = (inner - 12.0) / 2.0;
    let by = y + (ROW_H - CTRL_H) / 2.0;
    ui.button(R::new(x + PAD, by, half, CTRL_H), STRINGS.upd_check_now, Control::CheckUpdates, !busy, false);
    if let Some(version) = &upd.available {
        let label = if armed { STRINGS.upd_confirm.to_string() } else { fill_v(STRINGS.upd_update_to, version) };
        let r = R::new(x + PAD + half + 12.0, by, half, CTRL_H);
        ui.button(r, &label, Control::Update, !updating, armed);
    }
    y += ROW_H;
    ui.row_label(x, y, STRINGS.upd_auto, true);
    ui.on_off(ctrl_rect(x, y), cfg.update.check, Control::UpdateAuto);
    y += ROW_H + 6.0;
    ui.paragraph(
        R::new(x + PAD, y, inner, UPD_HINT_LINES as f32 * UPD_HINT_LINE_H),
        STRINGS.upd_hint,
        UPD_HINT_LINES,
        UPD_HINT_LINE_H,
        18.0,
        Weight::Regular,
        TEXT_DIM,
    );

    let x = RIGHT_X;
    let mut y = ui.card(x, VIEW_TOP, h, STRINGS.section_about);
    let name = fill_v(STRINGS.about_name, upd.version);
    ui.text(R::new(x + PAD, y, inner, ABOUT_NAME_H), &name, 24.0, Weight::SemiBold, TEXT, Align::Left);
    y += ABOUT_NAME_H;
    for line in STRINGS.about_lines {
        ui.text(R::new(x + PAD, y, inner, ABOUT_LINE_H), line, 19.0, Weight::Regular, TEXT_DIM, Align::Left);
        y += ABOUT_LINE_H;
    }
}

/// Progress bar while recording, else the result, failure message or "not recorded".
fn calib_status(ui: &mut Ui<'_>, r: R, status: &CalibStatus, stored: Option<f32>, on: bool) {
    let fade = |c: Rgba| if on { c } else { c.with_alpha(DISABLED_ALPHA + 40) };
    let done = |df: f32| fill_v(STRINGS.calib_done, &fmt_num(df, 0));
    match status {
        CalibStatus::Recording { progress } => {
            let p = progress.clamp(0.0, 1.0);
            let bar = R::new(r.x, r.y + 6.0, r.w, 12.0);
            ui.fill(bar, 6.0, CTRL);
            if p > 0.0 {
                ui.fill(R::new(bar.x, bar.y, (bar.w * p).max(bar.h), bar.h), 6.0, ACCENT);
            }
            let left = ((1.0 - p) * CALIBRATION_S).ceil() as u32;
            let text = STRINGS.calib_recording.replace("{s}", &left.to_string());
            ui.text(R::new(r.x, r.y + 24.0, r.w, 30.0), &text, 19.0, Weight::Regular, fade(TEXT), Align::Left);
        }
        CalibStatus::Failed(msg) => {
            ui.paragraph(r, msg, 2, r.h / 2.0, 19.0, Weight::Regular, fade(WARN));
        }
        CalibStatus::Done { df_hz } => {
            ui.text(R::new(r.x, r.y, r.w, 32.0), &done(*df_hz), 19.0, Weight::Regular, fade(TEXT), Align::Left);
        }
        CalibStatus::NotRecorded => match stored {
            Some(df) => ui.text(R::new(r.x, r.y, r.w, 32.0), &done(df), 19.0, Weight::Regular, fade(TEXT), Align::Left),
            None => ui.text(R::new(r.x, r.y, r.w, 32.0), STRINGS.calib_not_recorded, 19.0, Weight::Regular, fade(TEXT_DIM), Align::Left),
        },
    }
}

/// The modal explanation card (§5.1 "User guidance") with its scrollable text.
fn explanation_card(ui: &mut Ui<'_>, card: Card, scroll: f32) {
    draw::fill_rounded(ui.pm, R::new(0.0, 0.0, W, H), 0.0, SCRIM);
    ui.fill(CARD_R, 18.0, CARD);
    draw::stroke_rounded(ui.pm, CARD_R, 18.0, 2.0, CTRL_HOVER);
    let title = R::new(CARD_TEXT.x, CARD_R.y + 26.0, CARD_R.w - 88.0, 48.0);
    ui.text(title, STRINGS.card_title, 30.0, Weight::SemiBold, TEXT, Align::Left);

    let clip = Some(CARD_TEXT.clip());
    let pm = &mut *ui.pm;
    let total = layout_card_text(ui.tx, ui.wrap, |tx, line| {
        let top = CARD_TEXT.y + line.y - scroll;
        if top + CARD_LINE_H <= CARD_TEXT.y || top >= CARD_TEXT.bottom() {
            return;
        }
        let base = top + (CARD_LINE_H + TextRenderer::cap_height(line.weight, CARD_TEXT_PX)) / 2.0;
        if line.bullet {
            tx.draw(pm, Weight::SemiBold, CARD_TEXT_PX, CARD_TEXT.x + 6.0, base, Align::Left, "\u{2022}", TEXT, clip);
        }
        let x = CARD_TEXT.x + line.indent;
        tx.draw(pm, line.weight, CARD_TEXT_PX, x, base, Align::Left, line.text, TEXT, clip);
    });

    let max = (total - CARD_TEXT.h).max(0.0);
    if max > 0.0 {
        let track = R::new(CARD_TEXT.right() + 22.0, CARD_TEXT.y, 6.0, CARD_TEXT.h);
        ui.fill(track, 3.0, CTRL);
        let thumb_h = (track.h * CARD_TEXT.h / total).max(40.0);
        let thumb_y = track.y + (track.h - thumb_h) * (scroll / max).clamp(0.0, 1.0);
        ui.fill(R::new(track.x, thumb_y, track.w, thumb_h), 3.0, TEXT_DIM);
    }

    let (bw, bh) = CARD_BTN;
    let by = CARD_TEXT.bottom() + 28.0;
    let right = CARD_R.right() - 44.0;
    let primary = R::new(right - bw, by, bw, bh);
    let mut buttons_left = primary.x;
    match card {
        Card::Enable => {
            let keep_off = R::new(primary.x - 16.0 - bw, by, bw, bh);
            ui.button(keep_off, STRINGS.card_keep_off, Control::CardClose, true, false);
            ui.button(primary, STRINGS.card_turn_on, Control::CardTurnOn, true, true);
            buttons_left = keep_off.x;
        }
        Card::Info => ui.button(primary, STRINGS.card_close, Control::CardClose, true, true),
    }
    if scroll < max - 0.5 {
        let hint = R::new(CARD_TEXT.x, by, buttons_left - 24.0 - CARD_TEXT.x, bh);
        ui.text(hint, STRINGS.card_scroll_hint, 20.0, Weight::Regular, TEXT_DIM, Align::Left);
    }
}

/// Quantised calibration status (failure messages are compared separately).
#[derive(Clone, Copy, PartialEq, Debug)]
enum CalibKey {
    NotRecorded,
    /// Percent.
    Recording(i32),
    Done(i32),
    Failed,
}

impl CalibKey {
    fn of(s: &CalibStatus) -> Self {
        match s {
            CalibStatus::NotRecorded => CalibKey::NotRecorded,
            CalibStatus::Recording { progress } => CalibKey::Recording((progress.clamp(0.0, 1.0) * 100.0).round() as i32),
            CalibStatus::Done { df_hz } => CalibKey::Done(df_hz.round() as i32),
            CalibStatus::Failed(_) => CalibKey::Failed,
        }
    }
}

fn failure_message(s: &CalibStatus) -> &str {
    match s {
        CalibStatus::Failed(msg) => msg,
        _ => "",
    }
}

/// Quantised live values that influence the pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
struct LiveKey {
    f0: Option<i32>,
    state: HudState,
    autostart: bool,
    res_text: &'static str,
    calib: [CalibKey; 2],
}

impl LiveKey {
    fn of(live: &LiveInfo) -> Self {
        let res = &live.resonance;
        Self {
            f0: live.f0_hz.filter(|f| f.is_finite()).map(|f| f.round() as i32),
            state: live.state,
            autostart: live.autostart,
            res_text: position_text(res.position),
            calib: [CalibKey::of(&res.relaxed), CalibKey::of(&res.target)],
        }
    }
}

/// Panel-internal view state.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ViewState {
    page: Page,
    card: Option<Card>,
    /// Body scroll offset of the current page.
    scroll_px: f32,
    /// Scroll offset of the explanation card text.
    card_scroll_px: f32,
}

/// Everything besides the config and the strings that determines the pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
struct RenderKey {
    live: LiveKey,
    hover: Option<usize>,
    view: ViewState,
    /// "Confirm update" is showing.
    armed: bool,
}

/// The dashboard settings panel.
pub struct SettingsPanel {
    pixmap: Pixmap,
    text: TextRenderer,
    widgets: Vec<Widget>,
    wrap: Vec<(usize, usize)>,
    pointer: Option<(f32, f32)>,
    hover: Option<usize>,
    view: ViewState,
    cfg: Option<Config>,
    autostart: bool,
    /// What the pixels currently show; `None` forces a render.
    last: Option<RenderKey>,
    last_status: String,
    /// Failure messages (relaxed, target) the pixels currently show.
    last_failures: [String; 2],
    /// Update state the pixels currently show; its `available` version is what "Update to"
    /// installs.
    last_update: UpdateLive,
    confirm: Confirm,
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
            view: ViewState { page: Page::General, card: None, scroll_px: 0.0, card_scroll_px: 0.0 },
            cfg: None,
            autostart: false,
            last: None,
            last_status: String::new(),
            last_failures: [String::new(), String::new()],
            last_update: UpdateLive::default(),
            confirm: Confirm::default(),
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
        let now = Instant::now();
        self.confirm.expire(now);
        if live.update.status.updating() {
            self.confirm.until = None;
        }
        let armed = live.update.available.as_deref().is_some_and(|v| self.confirm.armed(v, now));
        let key = RenderKey { live: LiveKey::of(live), hover: self.hover, view: self.view, armed };
        let failures = [failure_message(&live.resonance.relaxed), failure_message(&live.resonance.target)];
        let unchanged = !self.dirty
            && self.cfg.as_ref() == Some(cfg)
            && self.last.as_ref() == Some(&key)
            && self.last_status == live.mic_status_line
            && self.last_failures[0] == failures[0]
            && self.last_failures[1] == failures[1]
            && self.last_update == live.update;
        if unchanged {
            return None;
        }
        self.dirty = false;
        if self.last_status != live.mic_status_line {
            self.last_status.clear();
            self.last_status.push_str(&live.mic_status_line);
        }
        for (last, now) in self.last_failures.iter_mut().zip(failures) {
            if last != now {
                last.clear();
                last.push_str(now);
            }
        }
        if self.cfg.as_ref() != Some(cfg) {
            self.cfg = Some(cfg.clone());
        }
        if self.last_update != live.update {
            self.last_update.clone_from(&live.update);
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
        build(&mut ui, cfg, live, self.view, armed);
        // The layout may have moved under a resting pointer (page or card change).
        self.hover = self.pointer.and_then(|(x, y)| self.hit(x, y));
        self.last = Some(RenderKey { hover: self.hover, ..key });
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
    /// Turning resonance on opens the explanation card; only its "Turn on" yields the
    /// [`Action::SetConfig`]. "Update to X.Y.Z" only arms "Confirm update"; a second click within
    /// [`UPDATE_CONFIRM_WINDOW`] yields [`Action::StartUpdate`].
    pub fn click(&mut self, x: f32, y: f32) -> Option<Action> {
        let control = self.widgets[self.hit(x, y)?].control;
        let resonance_on = self.cfg.as_ref()?.resonance.enabled;
        match control {
            Control::Tab(page) => {
                if page != self.view.page {
                    self.set_view(ViewState { page, scroll_px: 0.0, ..self.view });
                }
                return None;
            }
            Control::ResonanceEnabled(true) if !resonance_on => {
                self.open_card(Card::Enable);
                return None;
            }
            Control::HowItWorks => {
                self.open_card(Card::Info);
                return None;
            }
            Control::CardClose => {
                self.set_view(ViewState { card: None, ..self.view });
                return None;
            }
            Control::CardTurnOn => self.set_view(ViewState { card: None, ..self.view }),
            Control::Update => {
                let version = self.last_update.available.clone()?;
                self.dirty = true;
                return self.confirm.press(&version, Instant::now()).then_some(Action::StartUpdate(version));
            }
            _ => {}
        }
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
            Action::Quit | Action::StartCalibration(_) | Action::CheckUpdates | Action::StartUpdate(_) => {}
        }
        Some(action)
    }

    /// When the panel must be rendered again without any input: the end of the "Confirm update"
    /// window, after which the button reverts to "Update to X.Y.Z".
    pub fn next_deadline(&self) -> Option<Instant> {
        self.confirm.until
    }

    /// Switches panel-internal view state. The old hit rectangles are dropped so that no click
    /// lands on a control that is no longer shown before the next render.
    fn set_view(&mut self, view: ViewState) {
        self.view = view;
        self.widgets.clear();
        self.hover = None;
        self.dirty = true;
    }

    /// Shows `page` with the card `card` (or none) at the top of its text; used by the previews.
    pub(crate) fn show(&mut self, page: Page, card: Option<Card>) {
        self.set_view(ViewState { page, card, scroll_px: 0.0, card_scroll_px: 0.0 });
    }

    /// Presses "Update to X.Y.Z" once (shows "Confirm update"); used by the previews.
    pub(crate) fn arm_update(&mut self) {
        if let Some(version) = self.last_update.available.clone() {
            self.confirm.press(&version, Instant::now());
            self.dirty = true;
        }
    }

    fn open_card(&mut self, card: Card) {
        self.set_view(ViewState { card: Some(card), card_scroll_px: 0.0, ..self.view });
    }

    /// Scrolls the body, or the explanation card text while the card is open; positive `dy`
    /// (wheel up / swipe down) moves towards the top.
    pub fn scroll(&mut self, dy: f32) {
        if self.view.card.is_some() {
            let max = card_scroll_max(&mut self.text, &mut self.wrap);
            self.view.card_scroll_px = (self.view.card_scroll_px - dy * SCROLL_STEP_PX).clamp(0.0, max);
            return;
        }
        let max = (content_height(self.view.page) - (H - VIEW_TOP)).max(0.0);
        let s = (self.view.scroll_px - dy * SCROLL_STEP_PX).clamp(0.0, max);
        if s != self.view.scroll_px {
            self.view.scroll_px = s;
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
            resonance: ResonanceLive::default(),
            update: UpdateLive::default(),
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
        // 3 tabs, 17 HUD (2+3+2+2+2+6), 11 target (3+4+4), 8 reminder, 2 vocal rest, 2 mic,
        // 2 autostart, quit.
        assert_eq!(panel.widgets.len(), 46);
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
        for page in [Page::General, Page::Resonance, Page::Updates] {
            let h = content_height(page);
            assert!(h <= H - VIEW_TOP, "{page:?}: content {h} > viewport");
        }
        let mut panel = SettingsPanel::new();
        panel.scroll(-3.0);
        assert_eq!(panel.view.scroll_px, 0.0);
    }

    fn on_cfg() -> Config {
        let mut c = Config::default();
        c.resonance.enabled = true;
        c
    }

    fn panel_on(page: Page, cfg: &Config, live: &LiveInfo) -> SettingsPanel {
        let mut panel = SettingsPanel::new();
        panel.show(page, None);
        panel.render(cfg, live).expect("first render");
        panel
    }

    fn click(panel: &mut SettingsPanel, control: Control) -> Option<Action> {
        let (x, y) = center_of(panel, control);
        panel.click(x, y)
    }

    fn controls(panel: &SettingsPanel) -> Vec<Control> {
        panel.widgets.iter().map(|w| w.control).collect()
    }

    fn is_enabled(panel: &SettingsPanel, control: Control) -> bool {
        panel.widgets.iter().find(|w| w.control == control).unwrap_or_else(|| panel_missing(control)).enabled
    }

    #[test]
    fn tabs_switch_pages() {
        let cfg = Config::default();
        let mut panel = panel_on(Page::General, &cfg, &live());
        assert_eq!(click(&mut panel, Control::Tab(Page::Resonance)), None);
        assert_eq!(panel.view.page, Page::Resonance);
        assert!(panel.render(&cfg, &live()).is_some());
        assert!(controls(&panel).contains(&Control::ResonanceEnabled(true)));
        assert!(!controls(&panel).contains(&Control::HudEnabled(true)));
        assert_eq!(click(&mut panel, Control::Tab(Page::Resonance)), None, "selected tab");
        assert!(panel.render(&cfg, &live()).is_none());
        assert_eq!(click(&mut panel, Control::Tab(Page::General)), None);
        assert!(panel.render(&cfg, &live()).is_some());
        assert!(controls(&panel).contains(&Control::HudEnabled(true)));
    }

    #[test]
    fn resonance_controls_hit_and_produce_their_actions() {
        let on = on_cfg();
        let mut panel = panel_on(Page::Resonance, &on, &live());
        let relaxed = Control::Record(CalibWhich::Relaxed);
        let target = Control::Record(CalibWhich::Target);
        assert_eq!(click(&mut panel, relaxed), Some(Action::StartCalibration(CalibWhich::Relaxed)));
        assert_eq!(click(&mut panel, target), Some(Action::StartCalibration(CalibWhich::Target)));
        assert_eq!(click(&mut panel, Control::ResonanceEnabled(true)), None, "already on");
        assert_eq!(panel.view.card, None);
        let mut off = on.clone();
        off.resonance.enabled = false;
        assert_eq!(click(&mut panel, Control::ResonanceEnabled(false)), Some(Action::SetConfig(off)));

        // "How this works" re-opens the card with only "Close".
        let mut panel = panel_on(Page::Resonance, &on, &live());
        assert_eq!(click(&mut panel, Control::HowItWorks), None);
        assert_eq!(panel.view.card, Some(Card::Info));
        assert!(panel.render(&on, &live()).is_some());
        assert_eq!(controls(&panel), [Control::CardClose]);
        assert_eq!(click(&mut panel, Control::CardClose), None);
        assert_eq!(panel.view.card, None);
        assert!(panel.render(&on, &live()).is_some());
        assert!(controls(&panel).contains(&Control::HowItWorks));
    }

    #[test]
    fn enabling_goes_through_the_explanation_card() {
        let cfg = Config::default();
        let mut panel = panel_on(Page::Resonance, &cfg, &live());
        let switch_on = center_of(&panel, Control::ResonanceEnabled(true));
        let quit = center_of(&panel, Control::Quit);
        assert_eq!(panel.click(switch_on.0, switch_on.1), None);
        assert_eq!(panel.view.card, Some(Card::Enable));
        assert_eq!(panel.click(quit.0, quit.1), None, "old controls are gone until the next render");
        assert!(panel.render(&cfg, &live()).is_some());
        assert_eq!(controls(&panel), [Control::CardClose, Control::CardTurnOn]);
        // Modal: nothing outside the card's buttons reacts.
        assert_eq!(panel.click(switch_on.0, switch_on.1), None);
        assert_eq!(panel.click(quit.0, quit.1), None);
        assert_eq!(panel.view.card, Some(Card::Enable));

        let mut want = cfg.clone();
        want.resonance.enabled = true;
        assert_eq!(click(&mut panel, Control::CardTurnOn), Some(Action::SetConfig(want.clone())));
        assert_eq!(panel.view.card, None);
        assert!(panel.render(&want, &live()).is_some());
        assert!(is_enabled(&panel, Control::Record(CalibWhich::Relaxed)));

        // "Keep off" emits nothing, closes the card and leaves resonance off.
        let mut panel = panel_on(Page::Resonance, &cfg, &live());
        assert_eq!(click(&mut panel, Control::ResonanceEnabled(true)), None);
        assert!(panel.render(&cfg, &live()).is_some());
        assert_eq!(click(&mut panel, Control::CardClose), None);
        assert_eq!(panel.view.card, None);
        assert!(!panel.cfg.as_ref().unwrap().resonance.enabled);
        assert!(panel.render(&cfg, &live()).is_some());
        assert!(controls(&panel).contains(&Control::ResonanceEnabled(true)));
    }

    #[test]
    fn resonance_controls_are_disabled_when_off_busy_or_processed() {
        let relaxed = Control::Record(CalibWhich::Relaxed);
        let target = Control::Record(CalibWhich::Target);
        let cfg = Config::default();
        let mut panel = panel_on(Page::Resonance, &cfg, &live());
        for c in [Control::HowItWorks, relaxed, target] {
            assert!(!is_enabled(&panel, c), "{c:?}");
            assert_eq!(click(&mut panel, c), None);
        }
        assert_eq!(panel.view.card, None);

        let on = on_cfg();
        let mut busy = live();
        busy.resonance.relaxed = CalibStatus::Recording { progress: 0.4 };
        let panel = panel_on(Page::Resonance, &on, &busy);
        assert!(!is_enabled(&panel, relaxed) && !is_enabled(&panel, target));
        assert!(is_enabled(&panel, Control::HowItWorks));

        let mut processed = live();
        processed.resonance.position = Some(ResonanceView::NeedsBypass);
        let panel = panel_on(Page::Resonance, &on, &processed);
        assert!(!is_enabled(&panel, relaxed) && !is_enabled(&panel, target));
    }

    #[test]
    fn every_resonance_control_is_reachable_and_inside_the_viewport() {
        let cases = [(on_cfg(), None, 9), (Config::default(), Some(Card::Enable), 2), (on_cfg(), Some(Card::Info), 1)];
        for (cfg, card, count) in cases {
            let mut panel = SettingsPanel::new();
            panel.show(Page::Resonance, card);
            panel.render(&cfg, &live()).unwrap();
            assert_eq!(panel.widgets.len(), count, "{card:?}");
            let bounds = if card.is_some() { CARD_R } else { R::new(0.0, 0.0, W, H) };
            for (i, w) in panel.widgets.iter().enumerate() {
                let r = w.rect;
                assert!(r.x >= bounds.x && r.right() <= bounds.right() && r.y >= bounds.y && r.bottom() <= bounds.bottom(), "{w:?}");
                assert_eq!(panel.hit(r.cx(), r.cy()), Some(i), "{:?} overlaps another control", w.control);
            }
        }
    }

    #[test]
    fn card_shows_at_most_120_words_and_scrolls_to_the_rest() {
        let mut tx = TextRenderer::new();
        let mut wrap = Vec::new();
        let (mut visible, mut total, mut turn_off_y) = (0, 0, None);
        let height = layout_card_text(&mut tx, &mut wrap, |_, line| {
            let words = line.text.split_whitespace().count();
            total += words;
            if line.y < CARD_TEXT.h {
                visible += words;
            }
            if line.weight == Weight::SemiBold && turn_off_y.is_none() {
                turn_off_y = Some(line.y);
            }
        });
        assert!(visible <= 120, "{visible} words visible without scrolling");
        assert!(total > visible && height > CARD_TEXT.h);
        let turn_off_y = turn_off_y.expect("emphasised paragraph");
        assert!(turn_off_y + CARD_LINE_H <= CARD_TEXT.h, "'turn it off if' starts in the first view");

        let cfg = Config::default();
        let mut panel = SettingsPanel::new();
        panel.show(Page::Resonance, Some(Card::Enable));
        assert!(panel.render(&cfg, &live()).is_some());
        panel.scroll(-100.0);
        assert_eq!(panel.view.card_scroll_px, height - CARD_TEXT.h);
        assert!(panel.render(&cfg, &live()).is_some());
        assert!(panel.render(&cfg, &live()).is_none());
        panel.scroll(100.0);
        assert_eq!(panel.view.card_scroll_px, 0.0);
        assert_eq!(panel.view.scroll_px, 0.0, "the body does not scroll under the card");
        // Re-opening starts at the top again.
        panel.scroll(-1.0);
        assert!(panel.view.card_scroll_px > 0.0);
        panel.render(&cfg, &live()).unwrap();
        assert_eq!(click(&mut panel, Control::CardClose), None);
        panel.render(&cfg, &live()).unwrap();
        let mut on = cfg.clone();
        on.resonance.enabled = true;
        panel.render(&on, &live()).unwrap();
        assert_eq!(click(&mut panel, Control::HowItWorks), None);
        assert_eq!(panel.view.card_scroll_px, 0.0);
    }

    #[test]
    fn resonance_render_none_when_unchanged_some_on_changes() {
        let on = on_cfg();
        let mut panel = SettingsPanel::new();
        panel.show(Page::Resonance, None);
        let mut l = live();
        l.resonance.position = Some(ResonanceView::Position(0.8));
        assert!(panel.render(&on, &l).is_some());
        assert!(panel.render(&on, &l).is_none());
        l.resonance.position = Some(ResonanceView::Position(0.9));
        assert!(panel.render(&on, &l).is_none(), "same text");
        l.resonance.position = Some(ResonanceView::Position(1.2));
        assert!(panel.render(&on, &l).is_some());
        l.resonance.target = CalibStatus::Recording { progress: 0.5 };
        assert!(panel.render(&on, &l).is_some());
        l.resonance.target = CalibStatus::Recording { progress: 0.502 };
        assert!(panel.render(&on, &l).is_none(), "same percent");
        l.resonance.target = CalibStatus::Recording { progress: 0.6 };
        assert!(panel.render(&on, &l).is_some());
        l.resonance.target = CalibStatus::Failed("a".into());
        assert!(panel.render(&on, &l).is_some());
        assert!(panel.render(&on, &l).is_none());
        l.resonance.target = CalibStatus::Failed("b".into());
        assert!(panel.render(&on, &l).is_some());
        l.resonance.relaxed = CalibStatus::Done { df_hz: 1000.2 };
        assert!(panel.render(&on, &l).is_some());
        l.resonance.relaxed = CalibStatus::Done { df_hz: 1000.4 };
        assert!(panel.render(&on, &l).is_none(), "same displayed Hz");
        panel.show(Page::General, None);
        assert!(panel.render(&on, &l).is_some());
        assert!(panel.render(&on, &l).is_none());
    }

    #[test]
    fn position_text_follows_spec_wording() {
        let p = |v: f32| position_text(Some(ResonanceView::Position(v)));
        assert_eq!(p(-0.6), STRINGS.res_near_relaxed);
        assert_eq!(p(0.0), STRINGS.res_near_relaxed);
        assert_eq!(p(0.5), STRINGS.res_between);
        assert_eq!(p(1.0), STRINGS.res_near_target);
        assert_eq!(p(1.3), STRINGS.res_beyond_target);
        assert_eq!(p(f32::NAN), STRINGS.res_gathering);
        assert_eq!(position_text(Some(ResonanceView::Gathering)), STRINGS.res_gathering);
        assert_eq!(position_text(Some(ResonanceView::Uncalibrated)), STRINGS.res_uncalibrated);
        assert_eq!(position_text(Some(ResonanceView::TooSimilar)), STRINGS.res_too_similar);
        assert_eq!(position_text(Some(ResonanceView::NeedsBypass)), STRINGS.res_needs_bypass);
    }

    fn update_live(status: UpdateStatus, available: Option<&str>) -> LiveInfo {
        LiveInfo {
            update: UpdateLive { version: "0.2.1", status, available: available.map(str::to_string) },
            ..live()
        }
    }

    #[test]
    fn update_status_lines_follow_spec_wording() {
        let t = update_status_text;
        assert_eq!(t(&UpdateStatus::UpToDate { checked_ago_min: 120 }), "Up to date \u{00b7} checked 2 h ago");
        assert_eq!(t(&UpdateStatus::UpToDate { checked_ago_min: 0 }), "Up to date \u{00b7} checked just now");
        assert_eq!(t(&UpdateStatus::UpToDate { checked_ago_min: 5 }), "Up to date \u{00b7} checked 5 min ago");
        assert_eq!(t(&UpdateStatus::UpToDate { checked_ago_min: 3 * 24 * 60 }), "Up to date \u{00b7} checked 3 d ago");
        assert_eq!(t(&UpdateStatus::Available("1.2.3".into())), "Version 1.2.3 is available");
        assert_eq!(t(&UpdateStatus::Checking), "Checking\u{2026}");
        assert_eq!(t(&UpdateStatus::Updating("1.2.3".into())), "Updating to 1.2.3\u{2026}");
        assert_eq!(t(&UpdateStatus::UpdateFailed("no network".into())), "Update failed: no network");
        assert_eq!(t(&UpdateStatus::CheckFailed("no network".into())), "Couldn't check: no network");
        assert_eq!(t(&UpdateStatus::AlreadyRunning), "An update is already running");
        assert_eq!(
            t(&UpdateStatus::UpdatingManual),
            "Updating\u{2026} the app will close; start it again from the + list"
        );
        assert_eq!(t(&UpdateStatus::NotChecked), STRINGS.upd_not_checked);
    }

    #[test]
    fn confirm_gate_needs_a_second_press_within_10_s() {
        let t0 = Instant::now();
        let at = |secs: f32| t0 + Duration::from_secs_f32(secs);
        let mut c = Confirm::default();
        assert!(!c.armed("0.3.0", t0));
        assert!(!c.press("0.3.0", t0), "the first press only arms");
        assert!(c.armed("0.3.0", at(9.9)));
        assert!(!c.armed("0.4.0", at(1.0)), "armed for one version only");
        assert!(c.press("0.3.0", at(9.9)));
        assert!(!c.armed("0.3.0", at(9.9)), "confirming disarms");
        // Too late: the second press arms again instead of confirming.
        assert!(!c.press("0.3.0", t0));
        assert!(!c.press("0.3.0", at(10.0)));
        assert!(c.press("0.3.0", at(15.0)));
        // A different version arms afresh.
        assert!(!c.press("0.3.0", t0));
        assert!(!c.press("0.4.0", at(1.0)));
        assert!(c.press("0.4.0", at(2.0)));
        assert!(!c.press("0.4.0", t0));
        c.expire(at(9.0));
        assert!(c.until.is_some());
        c.expire(at(10.0));
        assert_eq!(c.until, None);
    }

    #[test]
    fn update_button_asks_for_confirmation_within_the_window() {
        let cfg = Config::default();
        let l = update_live(UpdateStatus::Available("0.3.0".into()), Some("0.3.0"));
        let mut panel = panel_on(Page::Updates, &cfg, &l);
        assert_eq!(panel.next_deadline(), None);
        assert_eq!(click(&mut panel, Control::Update), None, "the first click only arms");
        let deadline = panel.next_deadline().expect("armed");
        assert!(deadline <= Instant::now() + UPDATE_CONFIRM_WINDOW);
        assert!(panel.render(&cfg, &l).is_some(), "label changes to Confirm update");
        assert!(panel.last.unwrap().armed);
        assert_eq!(click(&mut panel, Control::Update), Some(Action::StartUpdate("0.3.0".into())));
        assert_eq!(panel.next_deadline(), None);

        // When the window ends the label reverts and the next click arms again.
        panel.render(&cfg, &l).unwrap();
        assert_eq!(click(&mut panel, Control::Update), None);
        panel.confirm.until = Some(Instant::now());
        assert!(panel.render(&cfg, &l).is_some(), "label reverts");
        assert!(!panel.last.unwrap().armed);
        assert_eq!(panel.next_deadline(), None);
        assert_eq!(click(&mut panel, Control::Update), None, "arms again");

        // A running update disarms.
        let updating = update_live(UpdateStatus::Updating("0.3.0".into()), Some("0.3.0"));
        assert!(panel.render(&cfg, &updating).is_some());
        assert!(!panel.last.unwrap().armed);
        assert_eq!(panel.next_deadline(), None);
    }

    #[test]
    fn update_controls_hit_and_follow_the_state() {
        let cfg = Config::default();
        let mut panel = panel_on(Page::Updates, &cfg, &update_live(UpdateStatus::UpToDate { checked_ago_min: 3 }, None));
        assert!(!controls(&panel).contains(&Control::Update), "no newer release, no button");
        assert_eq!(click(&mut panel, Control::CheckUpdates), Some(Action::CheckUpdates));
        let mut off = cfg.clone();
        off.update.check = false;
        assert_eq!(click(&mut panel, Control::UpdateAuto(false)), Some(Action::SetConfig(off)));
        assert_eq!(click(&mut panel, Control::UpdateAuto(true)), Some(Action::SetConfig(cfg.clone())));

        let cases = [
            (UpdateStatus::Available("0.3.0".into()), true, true),
            (UpdateStatus::Checking, false, true),
            (UpdateStatus::Updating("0.3.0".into()), false, false),
            (UpdateStatus::UpdatingManual, false, false),
            (UpdateStatus::AlreadyRunning, false, false),
            (UpdateStatus::UpdateFailed("x".into()), true, true),
            (UpdateStatus::CheckFailed("x".into()), true, true),
        ];
        for (status, check, update) in cases {
            let panel = panel_on(Page::Updates, &cfg, &update_live(status.clone(), Some("0.3.0")));
            assert_eq!(is_enabled(&panel, Control::CheckUpdates), check, "{status:?}");
            assert_eq!(is_enabled(&panel, Control::Update), update, "{status:?}");
        }
    }

    #[test]
    fn every_update_control_is_reachable_and_inside_the_viewport() {
        let l = update_live(UpdateStatus::Available("0.3.0".into()), Some("0.3.0"));
        let panel = panel_on(Page::Updates, &Config::default(), &l);
        // 3 tabs, quit, check now, update, 2 check automatically.
        assert_eq!(panel.widgets.len(), 8);
        for (i, w) in panel.widgets.iter().enumerate() {
            let r = w.rect;
            assert!(r.x >= 0.0 && r.right() <= W && r.y >= 0.0 && r.bottom() <= H, "{w:?}");
            assert_eq!(panel.hit(r.cx(), r.cy()), Some(i), "{:?} overlaps another control", w.control);
        }
    }

    #[test]
    fn update_render_none_when_unchanged_some_on_changes() {
        let cfg = Config::default();
        let mut l = update_live(UpdateStatus::UpToDate { checked_ago_min: 3 }, None);
        let mut panel = panel_on(Page::Updates, &cfg, &l);
        assert!(panel.render(&cfg, &l).is_none());
        l.update.status = UpdateStatus::UpToDate { checked_ago_min: 4 };
        assert!(panel.render(&cfg, &l).is_some());
        l.update.status = UpdateStatus::Checking;
        assert!(panel.render(&cfg, &l).is_some());
        assert!(panel.render(&cfg, &l).is_none());
        l.update.available = Some("0.3.0".into());
        assert!(panel.render(&cfg, &l).is_some());
    }
}
