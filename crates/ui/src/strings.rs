//! The single table of user-visible strings. Layout code never hard-codes text; every label is
//! fitted (shrunk or wrapped) to its box, so translations up to ~30 % longer still fit.

/// All user-visible text of the HUD and the settings panel.
#[derive(Debug, Clone, Copy)]
pub struct Strings {
    pub title: &'static str,
    pub quit: &'static str,
    pub on: &'static str,
    pub off: &'static str,
    pub hz: &'static str,
    pub no_f0: &'static str,
    /// Value templates; `{v}` is replaced by the formatted number.
    pub fmt_hz: &'static str,
    pub fmt_cm: &'static str,
    pub fmt_percent: &'static str,
    pub fmt_seconds: &'static str,

    pub live_heading: &'static str,
    /// Template with `{lo}` and `{hi}`.
    pub live_target: &'static str,
    pub state_in_range: &'static str,
    pub state_out_of_range: &'static str,
    pub state_no_voice: &'static str,
    pub state_muted: &'static str,
    pub state_no_mic: &'static str,

    pub section_hud: &'static str,
    pub hud_enabled: &'static str,
    pub placement: &'static str,
    pub placement_head: &'static str,
    pub placement_left_wrist: &'static str,
    pub placement_right_wrist: &'static str,
    pub display: &'static str,
    pub display_color: &'static str,
    pub display_detailed: &'static str,
    pub size: &'static str,
    pub opacity: &'static str,
    pub position: &'static str,
    pub nudge_up: &'static str,
    pub nudge_down: &'static str,
    pub nudge_left: &'static str,
    pub nudge_right: &'static str,
    pub nudge_closer: &'static str,
    pub nudge_further: &'static str,
    /// Template with `{x}`, `{y}`, `{z}` (metres, signed).
    pub nudge_readout: &'static str,

    pub section_target: &'static str,
    pub preset: &'static str,
    pub preset_feminine: &'static str,
    pub preset_masculine: &'static str,
    pub preset_custom: &'static str,
    pub custom_low: &'static str,
    pub custom_high: &'static str,

    pub section_reminder: &'static str,
    pub reminder_enabled: &'static str,
    pub reminder_delay: &'static str,
    pub reminder_pitch: &'static str,
    pub reminder_volume: &'static str,

    pub section_vocal_rest: &'static str,
    pub vocal_rest: &'static str,
    /// Template; `{min}` is replaced by the minute count.
    pub vocal_rest_limit: &'static str,
    /// HUD toast template; `{min}` is replaced by the voiced minutes.
    pub vocal_rest_toast: &'static str,

    pub section_system: &'static str,
    pub autostart: &'static str,
    pub mic_bypass: &'static str,
    pub mic_processed: &'static str,
}

/// English (required) string table.
pub const EN: Strings = Strings {
    title: "Vocal Pitch",
    quit: "Quit",
    on: "On",
    off: "Off",
    hz: "Hz",
    no_f0: "\u{2014}",
    fmt_hz: "{v} Hz",
    fmt_cm: "{v} cm",
    fmt_percent: "{v} %",
    fmt_seconds: "{v} s",

    live_heading: "Live",
    live_target: "Target {lo}\u{2013}{hi} Hz",
    state_in_range: "In range",
    state_out_of_range: "Out of range",
    state_no_voice: "No voice",
    state_muted: "Mic muted",
    state_no_mic: "No microphone",

    section_hud: "HUD",
    hud_enabled: "Show HUD",
    placement: "Placement",
    placement_head: "Head",
    placement_left_wrist: "Left wrist",
    placement_right_wrist: "Right wrist",
    display: "Display",
    display_color: "Colour",
    display_detailed: "Detailed",
    size: "Size",
    opacity: "Opacity",
    position: "Position",
    nudge_up: "Up",
    nudge_down: "Down",
    nudge_left: "Left",
    nudge_right: "Right",
    nudge_closer: "Closer",
    nudge_further: "Further",
    nudge_readout: "Offset  x {x}  y {y}  z {z} m",

    section_target: "Target range",
    preset: "Preset",
    preset_feminine: "Feminine",
    preset_masculine: "Masculine",
    preset_custom: "Custom",
    custom_low: "Custom low",
    custom_high: "Custom high",

    section_reminder: "Reminder tone",
    reminder_enabled: "Tone",
    reminder_delay: "Delay",
    reminder_pitch: "Pitch",
    reminder_volume: "Volume",

    section_vocal_rest: "Vocal rest",
    vocal_rest: "Voiced limit",
    vocal_rest_limit: "{min} min / hour",
    vocal_rest_toast: "Vocal rest: {min} min voiced in the last hour",

    section_system: "Microphone & startup",
    autostart: "Autostart with SteamVR",
    mic_bypass: "Bypass Valve processing (this app only)",
    mic_processed: "Use processed mic",
};

/// The active string table.
pub const STRINGS: &Strings = &EN;

/// Text of the vocal-rest HUD toast (§5) for `minutes` voiced in the last hour.
pub fn vocal_rest_toast(minutes: u32) -> String {
    STRINGS.vocal_rest_toast.replace("{min}", &minutes.to_string())
}

/// Replaces `{v}` in a value template.
pub(crate) fn fill_v(template: &str, v: &str) -> String {
    template.replace("{v}", v)
}
