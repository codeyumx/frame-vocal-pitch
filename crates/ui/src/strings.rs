//! The single table of user-visible strings. Layout code never hard-codes text; every label is
//! fitted (shrunk or wrapped) to its box, so translations up to ~30 % longer still fit.

/// All user-visible text of the HUD and the settings panel.
#[derive(Debug, Clone, Copy)]
pub struct Strings {
    pub title: &'static str,
    pub quit: &'static str,
    /// One-line hint under Quit while autostart is on / off.
    pub quit_hint_autostart: &'static str,
    pub quit_hint: &'static str,
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

    pub tab_general: &'static str,
    pub tab_resonance: &'static str,
    pub tab_updates: &'static str,

    // "About & updates" page (§4.2, §7.1).
    pub section_updates: &'static str,
    pub upd_version: &'static str,
    pub upd_auto: &'static str,
    pub upd_check_now: &'static str,
    /// Button template; `{v}` is replaced by the newer version.
    pub upd_update_to: &'static str,
    pub upd_confirm: &'static str,
    pub upd_hint: &'static str,
    /// Status lines. `{v}` = version, `{reason}` = failure reason, `{ago}` = one of the
    /// `upd_ago_*` texts with `{n}` replaced.
    pub upd_not_checked: &'static str,
    pub upd_up_to_date: &'static str,
    pub upd_ago_now: &'static str,
    pub upd_ago_min: &'static str,
    pub upd_ago_h: &'static str,
    pub upd_ago_d: &'static str,
    pub upd_available: &'static str,
    pub upd_checking: &'static str,
    pub upd_updating: &'static str,
    pub upd_updating_manual: &'static str,
    pub upd_already_running: &'static str,
    pub upd_failed: &'static str,
    pub upd_check_failed: &'static str,
    pub section_about: &'static str,
    /// `{v}` = the running version.
    pub about_name: &'static str,
    pub about_lines: [&'static str; 3],

    // Resonance indicator on the HUD (§4.1, §5.1); shown in a narrow column, keep short.
    pub hud_res_relaxed: &'static str,
    pub hud_res_target: &'static str,
    pub hud_res_uncalibrated: &'static str,
    pub hud_res_too_similar: &'static str,
    pub hud_res_needs_bypass: &'static str,

    // Resonance page of the settings panel (§4.2, §5.1).
    pub section_resonance: &'static str,
    pub res_enabled: &'static str,
    pub res_intro: &'static str,
    pub res_how: &'static str,
    pub res_now: &'static str,
    pub res_off_hint: &'static str,
    pub res_gathering: &'static str,
    pub res_uncalibrated: &'static str,
    pub res_too_similar: &'static str,
    pub res_needs_bypass: &'static str,
    pub res_near_relaxed: &'static str,
    pub res_between: &'static str,
    pub res_near_target: &'static str,
    pub res_beyond_target: &'static str,

    pub section_calibration: &'static str,
    pub record_relaxed: &'static str,
    pub record_target: &'static str,
    pub calib_not_recorded: &'static str,
    /// Template; `{s}` is replaced by the whole seconds left.
    pub calib_recording: &'static str,
    /// Template; `{v}` is replaced by the stored formant spacing in Hz.
    pub calib_done: &'static str,
    /// Failure message for a calibration with fewer than 3 s of accepted frames (§5.1).
    pub calib_not_enough_speech: &'static str,
    pub calib_hint: &'static str,

    // Explanation card (§5.1 "User guidance"). Paragraphs in display order: `card_what`,
    // `card_others`, `card_pitch`, `card_turn_off` (emphasised), then `card_cases` as bullets.
    pub card_title: &'static str,
    pub card_what: &'static str,
    pub card_others: &'static str,
    pub card_pitch: &'static str,
    pub card_turn_off: &'static str,
    pub card_cases: [&'static str; 4],
    pub card_scroll_hint: &'static str,
    pub card_turn_on: &'static str,
    pub card_keep_off: &'static str,
    pub card_close: &'static str,
}

/// English (required) string table.
pub const EN: Strings = Strings {
    title: "Vocal Pitch",
    quit: "Quit",
    quit_hint_autostart: "Stops the app until SteamVR starts again",
    quit_hint: "Stops the app",
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

    tab_general: "General",
    tab_resonance: "Resonance (experimental)",
    tab_updates: "About & updates",

    section_updates: "Updates",
    upd_version: "Version",
    upd_auto: "Check automatically",
    upd_check_now: "Check now",
    upd_update_to: "Update to {v}",
    upd_confirm: "Confirm update",
    upd_hint: "Checks GitHub at most once a day. Nothing is installed until you confirm, and a \
               failed update keeps the current version.",
    upd_not_checked: "Not checked yet",
    upd_up_to_date: "Up to date \u{00b7} checked {ago}",
    upd_ago_now: "just now",
    upd_ago_min: "{n} min ago",
    upd_ago_h: "{n} h ago",
    upd_ago_d: "{n} d ago",
    upd_available: "Version {v} is available",
    upd_checking: "Checking\u{2026}",
    upd_updating: "Updating to {v}\u{2026}",
    upd_updating_manual: "Updating\u{2026} the app will close; start it again from the + list",
    upd_already_running: "An update is already running",
    upd_failed: "Update failed: {reason}",
    upd_check_failed: "Couldn't check: {reason}",
    section_about: "About",
    about_name: "frame-vocal-pitch {v}",
    about_lines: [
        "Vocal pitch trainer overlay for SteamVR",
        "Source: github.com/codeyumx/frame-vocal-pitch",
        "License: Apache-2.0",
    ],

    hud_res_relaxed: "relaxed",
    hud_res_target: "target",
    hud_res_uncalibrated: "Calibrate in settings",
    hud_res_too_similar: "Recordings too similar",
    hud_res_needs_bypass: "Needs bypass mic mode",

    section_resonance: "Resonance (experimental)",
    res_enabled: "Indicator",
    res_intro: "Shows whether your voice is drifting brighter or darker compared with your own two \
                recordings. It is rough and moves slowly. It appears on the HUD next to the pitch \
                bar when Display is set to Detailed.",
    res_how: "How this works",
    res_now: "Right now",
    res_off_hint: "Off. Turn the indicator on to record your voice and see where it is.",
    res_gathering: "Listening\u{2026} keep talking for a few seconds",
    res_uncalibrated: "Record your relaxed and your target voice to see the indicator",
    res_too_similar: "These two recordings are too similar to tell apart; the indicator will not \
                      be useful",
    res_needs_bypass: "Resonance needs the bypass mic mode",
    res_near_relaxed: "Brightness: near relaxed",
    res_between: "Brightness: between",
    res_near_target: "Brightness: near target",
    res_beyond_target: "Brightness: beyond target",

    section_calibration: "Calibration",
    record_relaxed: "Record relaxed voice (10 s)",
    record_target: "Record target voice (10 s)",
    calib_not_recorded: "Not recorded yet",
    calib_recording: "Recording\u{2026} keep talking ({s} s left)",
    calib_done: "Recorded \u{00b7} {v} Hz",
    calib_not_enough_speech: "Not enough clear speech \u{2014} try again in a quiet place, speaking \
                              normally",
    calib_hint: "Speak normally for the whole 10 seconds, in a quiet place, in the language and \
                 accent you use in VR. Record again whenever you change language or accent.",

    card_title: "About the resonance indicator (experimental)",
    card_what: "This is a rough estimate of how bright or dark your voice sounds, compared with \
                two recordings of your own voice: one relaxed, one in your target voice. It moves \
                slowly on purpose.",
    card_others: "It cannot tell you how other people hear your voice. A voice coach or someone \
                  you trust can.",
    card_pitch: "Pitch feedback works the same with this on or off.",
    card_turn_off: "Turn it off if any of these apply, or if it does not match what you hear or \
                    what people you trust tell you:",
    card_cases: [
        "You speak a different language or accent in VR than in your recordings, or you switch \
         often. Vowels differ between languages and accents, and they move the estimate. Record \
         again in the language you are using.",
        "You are whispering, singing or shouting, your voice is very high (above about 300 Hz it \
         cannot measure), or the room is noisy.",
        "Your goal is not about how bright or dark your voice sounds.",
        "Watching it makes you tense or self-conscious.",
    ],
    card_scroll_hint: "Scroll for more",
    card_turn_on: "Turn on",
    card_keep_off: "Keep off",
    card_close: "Close",
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
