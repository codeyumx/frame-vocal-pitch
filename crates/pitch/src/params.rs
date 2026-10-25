//! Every constant of the pitch pipeline (SPEC §5), in one module.

/// Native capture rate the app records at.
pub const INPUT_RATE_HZ: u32 = 48_000;
/// Target analysis rate after decimation (48 kHz / 3).
pub const ANALYSIS_RATE_HZ: u32 = 16_000;
/// Hop between analysis frames.
pub const HOP_S: f64 = 0.010;

/// Default detection span.
pub const DETECT_MIN_HZ: f32 = 60.0;
pub const DETECT_MAX_HZ: f32 = 500.0;
/// Limits accepted for the detection span (SPEC §6.1 `audio.detect_*_hz`).
pub const DETECT_LIMIT_LO_HZ: f32 = 40.0;
pub const DETECT_LIMIT_HI_HZ: f32 = 1000.0;
/// Minimum width of the detection span.
pub const DETECT_MIN_SPAN_HZ: f32 = 50.0;

/// Integration window W, in periods of the lowest detectable frequency.
pub const WINDOW_PERIODS: f64 = 2.0;
/// YIN absolute threshold on the cumulative-mean-normalised difference.
pub const YIN_THRESHOLD: f32 = 0.12;
/// A frame is voiced only if its YIN dip is at most this value.
pub const VOICING_MAX_DIP: f32 = 0.25;

/// Voicing needs RMS above `noise EMA × NOISE_FLOOR_FACTOR`.
pub const NOISE_FLOOR_FACTOR: f32 = 3.0;
/// Default lower clamp of the adaptive noise floor.
pub const NOISE_FLOOR_MIN_DBFS: f32 = -60.0;
/// Accepted range of the lower clamp (SPEC §6.1 `audio.noise_floor_min_dbfs`).
pub const NOISE_FLOOR_MIN_DBFS_LO: f32 = -90.0;
pub const NOISE_FLOOR_MIN_DBFS_HI: f32 = -20.0;
/// Time constant of the noise EMA when unvoiced RMS rises above it. Slow, so
/// short loud aperiodic sounds (consonants, breaths) barely move the floor.
pub const NOISE_EMA_RISE_S: f64 = 3.0;
/// Time constant of the noise EMA when unvoiced RMS falls below it. Fast, so
/// the floor recovers within a speech pause.
pub const NOISE_EMA_FALL_S: f64 = 0.2;

/// Anti-alias low-pass cutoff as a fraction of the decimated rate
/// (6 kHz at 16 kHz).
pub const LOWPASS_CUTOFF_FRACTION: f64 = 0.375;
/// FIR length per unit of decimation factor (63 taps for 3:1, Blackman window).
pub const LOWPASS_TAPS_PER_FACTOR: usize = 21;

/// Median filter length over voiced estimates.
pub const MEDIAN_LEN: usize = 5;
/// Unvoiced time after which the median history is cleared.
pub const MEDIAN_RESET_S: f64 = 0.100;
/// Relative tolerance of the octave-jump guard (≈2× or ≈0.5× ± 6 %).
pub const OCTAVE_TOLERANCE: f32 = 0.06;
/// Consecutive hops an octave jump must persist before it is accepted.
pub const OCTAVE_CONFIRM_HOPS: usize = 3;
/// Time a new in/out-of-range classification must persist before switching.
pub const CLASS_HOLD_S: f64 = 0.120;
/// Hysteresis applied to the target band edges.
pub const BAND_HYSTERESIS_HZ: f32 = 2.0;
/// Continuous unvoiced time after which the state becomes `no_voice`.
pub const NO_VOICE_AFTER_S: f64 = 0.150;

/// Reminder tone length and fade (played by the audio crate).
pub const TONE_DURATION_MS: u32 = 300;
pub const TONE_FADE_MS: u32 = 20;
/// Minimum interval between reminder tones while still out of range.
pub const REMINDER_REPEAT_S: f64 = 2.0;

/// Vocal-rest accounting: 1-second buckets over a rolling 60-minute window.
pub const VOCAL_REST_BUCKETS: usize = 3600;
/// Minimum interval between vocal-rest toasts.
pub const VOCAL_REST_REPEAT_S: f64 = 600.0;

/// Resonance (SPEC §5.1): analysis frame length.
pub const RESONANCE_FRAME_S: f64 = 0.025;
/// Resonance: only voiced hops with F0 at or below this are analysed.
pub const RESONANCE_MAX_F0_HZ: f32 = 300.0;
/// Resonance: pre-emphasis coefficient, `y[n] = x[n] − k·x[n−1]`.
pub const PRE_EMPHASIS: f32 = 0.97;
/// Resonance: linear-prediction order.
pub const LPC_ORDER: usize = 16;
/// Resonance: band in which LPC resonances count as formants.
pub const FORMANT_MIN_HZ: f32 = 200.0;
pub const FORMANT_MAX_HZ: f32 = 4000.0;
/// Resonance: a formant's bandwidth must be below this.
pub const FORMANT_MAX_BANDWIDTH_HZ: f32 = 500.0;
/// Resonance: default median window (`resonance.window_s`) and its limits.
pub const RESONANCE_WINDOW_S: f32 = 8.0;
pub const RESONANCE_WINDOW_MIN_S: f32 = 4.0;
pub const RESONANCE_WINDOW_MAX_S: f32 = 20.0;
/// Resonance: accepted frames needed in the window before a value is shown.
pub const RESONANCE_MIN_ACCEPTED_S: f64 = 2.0;
/// Resonance: interval between recomputations of the window median.
pub const RESONANCE_MEDIAN_INTERVAL_S: f64 = 0.1;
/// Resonance: time constant of the displayed-value EMA.
pub const RESONANCE_SMOOTHING_S: f64 = 1.0;
/// Resonance reference values closer than this fraction of relaxed are "too similar".
pub const RESONANCE_MIN_SEPARATION: f32 = 0.05;

/// Slack for comparisons of hop timestamps against durations.
pub(crate) const TIME_EPS: f64 = 1e-6;

/// Detector parameters. `Default` gives the SPEC §5 values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchParams {
    /// Lowest detectable F0 (Hz).
    pub detect_min_hz: f32,
    /// Highest detectable F0 (Hz).
    pub detect_max_hz: f32,
    /// YIN absolute threshold.
    pub yin_threshold: f32,
    /// Voicing: maximum YIN dip.
    pub voicing_max_dip: f32,
    /// Voicing: lower clamp of the adaptive noise floor (dBFS, RMS).
    pub noise_floor_min_dbfs: f32,
    /// Voicing: multiplier on the unvoiced-RMS EMA.
    pub noise_floor_factor: f32,
}

impl Default for PitchParams {
    fn default() -> Self {
        Self {
            detect_min_hz: DETECT_MIN_HZ,
            detect_max_hz: DETECT_MAX_HZ,
            yin_threshold: YIN_THRESHOLD,
            voicing_max_dip: VOICING_MAX_DIP,
            noise_floor_min_dbfs: NOISE_FLOOR_MIN_DBFS,
            noise_floor_factor: NOISE_FLOOR_FACTOR,
        }
    }
}

impl PitchParams {
    /// Default parameters with a given detection span and noise-floor clamp
    /// (the three values exposed in the config file).
    pub fn new(detect_min_hz: f32, detect_max_hz: f32, noise_floor_min_dbfs: f32) -> Self {
        Self {
            detect_min_hz,
            detect_max_hz,
            noise_floor_min_dbfs,
            ..Self::default()
        }
        .sanitized()
    }

    /// Clamps every field into its valid range; NaN becomes the default.
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let min_hz = clamp_or(
            self.detect_min_hz,
            DETECT_LIMIT_LO_HZ,
            DETECT_LIMIT_HI_HZ - DETECT_MIN_SPAN_HZ,
            d.detect_min_hz,
        );
        let max_hz = clamp_or(
            self.detect_max_hz,
            min_hz + DETECT_MIN_SPAN_HZ,
            DETECT_LIMIT_HI_HZ,
            d.detect_max_hz.max(min_hz + DETECT_MIN_SPAN_HZ),
        );
        let yin_threshold = clamp_or(self.yin_threshold, 0.01, 1.0, d.yin_threshold);
        Self {
            detect_min_hz: min_hz,
            detect_max_hz: max_hz,
            yin_threshold,
            voicing_max_dip: clamp_or(
                self.voicing_max_dip,
                yin_threshold,
                1.0,
                d.voicing_max_dip.max(yin_threshold),
            ),
            noise_floor_min_dbfs: clamp_or(
                self.noise_floor_min_dbfs,
                NOISE_FLOOR_MIN_DBFS_LO,
                NOISE_FLOOR_MIN_DBFS_HI,
                d.noise_floor_min_dbfs,
            ),
            noise_floor_factor: clamp_or(self.noise_floor_factor, 1.0, 100.0, d.noise_floor_factor),
        }
    }
}

fn clamp_or(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_nan() {
        default.clamp(lo, hi)
    } else {
        v.clamp(lo, hi)
    }
}

/// Converts an RMS level in dBFS to a linear amplitude.
pub fn dbfs_to_linear(dbfs: f32) -> f32 {
    10f32.powf(dbfs / 20.0)
}

/// Converts a linear RMS amplitude to dBFS (−∞ for 0).
pub fn linear_to_dbfs(lin: f32) -> f32 {
    20.0 * lin.log10()
}
