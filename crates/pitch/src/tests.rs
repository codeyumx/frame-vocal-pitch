//! SPEC §5 acceptance tests on synthetic 48 kHz signals, plus timing tests of
//! the post-processing state machines.

use crate::analyze::synth::{saw, sine, tone, Noise};
use crate::params::*;
use crate::*;

const RATE: u32 = INPUT_RATE_HZ;
const FEMININE: (f32, f32) = (165.0, 255.0);

/// Runs detector + tracker; returns (raw hops, tracker states).
fn run(signal: &[f32]) -> (Vec<Hop>, Vec<PitchState>) {
    let mut det = Detector::new(PitchParams::default(), RATE);
    let mut tr = Tracker::new(FEMININE.0, FEMININE.1);
    let mut hops = Vec::new();
    // Feed in uneven chunks to exercise hop boundaries inside push().
    for chunk in signal.chunks(1000) {
        det.push(chunk, &mut hops);
    }
    let states = hops.iter().map(|h| tr.update(h)).collect();
    (hops, states)
}

fn assert_tracks(signal: &[f32], f0: f32, tol: f32, what: &str) {
    let (hops, states) = run(signal);
    let mut checked = 0;
    for (h, s) in hops.iter().zip(&states) {
        if h.t_s < 0.1 - 1e-9 {
            continue;
        }
        checked += 1;
        let raw = h
            .f0_hz
            .unwrap_or_else(|| panic!("{what}: unvoiced at {:.2}s (dip {})", h.t_s, h.dip));
        assert!(
            (raw / f0 - 1.0).abs() <= tol,
            "{what}: raw {raw} Hz at {:.2}s",
            h.t_s
        );
        let smooth = s.f0_hz.expect("tracker f0");
        assert!(
            (smooth / f0 - 1.0).abs() <= tol,
            "{what}: tracked {smooth} Hz at {:.2}s",
            h.t_s
        );
        assert_ne!(s.class, Class::NoVoice, "{what}");
    }
    assert!(checked >= 30, "{what}: only {checked} hops checked");
}

#[test]
fn pure_sines_within_one_percent_after_100ms() {
    for f in [70.0, 100.0, 150.0, 220.0, 330.0, 450.0] {
        let s = tone(RATE, 0.5, |_| f, |p| 0.5 * sine(p));
        assert_tracks(&s, f as f32, 0.01, &format!("sine {f} Hz"));
    }
}

#[test]
fn quiet_sine_is_still_voiced() {
    // −43 dBFS RMS, well above the default −60 dBFS floor clamp.
    let s = tone(RATE, 0.5, |_| 220.0, |p| 0.01 * sine(p));
    assert_tracks(&s, 220.0, 0.01, "quiet sine");
}

#[test]
fn harmonic_rich_tones_give_fundamental() {
    for f in [110.0, 200.0] {
        let s = tone(RATE, 0.5, |_| f, |p| 0.5 * saw(p));
        assert_tracks(&s, f as f32, 0.01, &format!("sawtooth {f} Hz"));
        // Second harmonic at twice the fundamental's amplitude.
        let s = tone(RATE, 0.5, |_| f, |p| 0.2 * sine(p) + 0.4 * sine(2.0 * p));
        assert_tracks(&s, f as f32, 0.01, &format!("strong 2nd harmonic {f} Hz"));
    }
}

#[test]
fn white_noise_minus_20_dbfs_is_unvoiced() {
    // Uniform noise with RMS 0.1 (−20 dBFS): amplitude √3 × 0.1.
    let mut noise = Noise::new(42);
    let a = 0.1 * 3f32.sqrt();
    let s: Vec<f32> = (0..RATE as usize * 3).map(|_| a * noise.next()).collect();
    let (hops, states) = run(&s);
    let input_rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    assert!(
        (linear_to_dbfs(input_rms) + 20.0).abs() < 0.2,
        "noise level {input_rms}"
    );
    for (h, st) in hops.iter().zip(&states) {
        assert_eq!(h.f0_hz, None, "voiced noise at {:.2}s dip {}", h.t_s, h.dip);
        assert_eq!(st.class, Class::NoVoice);
    }
}

#[test]
fn digital_silence_is_unvoiced() {
    let (hops, states) = run(&vec![0.0; RATE as usize * 2]);
    assert_eq!(hops.len(), 200);
    assert!(hops.iter().all(|h| h.f0_hz.is_none() && h.rms == 0.0));
    assert!(states
        .iter()
        .all(|s| s.class == Class::NoVoice && s.f0_hz.is_none()));
}

#[test]
fn glide_120_to_260_is_monotonic_without_octave_errors() {
    let freq = |t: f64| 120.0 + 140.0 * (t / 2.0);
    let s = tone(RATE, 2.0, freq, |p| 0.5 * sine(p));
    let (hops, states) = run(&s);
    let mut prev_raw = 0.0f32;
    let mut prev_smooth = 0.0f32;
    for (h, st) in hops.iter().zip(&states).filter(|(h, _)| h.t_s >= 0.1) {
        let raw = h
            .f0_hz
            .unwrap_or_else(|| panic!("unvoiced at {:.2}s", h.t_s));
        // Compare against the instantaneous frequency mid-frame (~25 ms back).
        let expect = freq(h.t_s - 0.025) as f32;
        assert!(
            (raw / expect - 1.0).abs() < 0.02,
            "raw {raw} vs {expect} at {:.2}s",
            h.t_s
        );
        assert!(
            raw >= prev_raw,
            "raw not monotonic at {:.2}s: {prev_raw} → {raw}",
            h.t_s
        );
        let smooth = st.f0_hz.unwrap();
        assert!(
            smooth >= prev_smooth,
            "tracked not monotonic at {:.2}s",
            h.t_s
        );
        prev_raw = raw;
        prev_smooth = smooth;
    }
    assert!(prev_smooth > 250.0);
}

#[test]
fn frequencies_outside_detection_span_are_unvoiced() {
    for f in [50.0, 540.0] {
        let s = tone(RATE, 0.5, |_| f, |p| 0.5 * sine(p));
        let (hops, _) = run(&s);
        for h in hops.iter().filter(|h| h.t_s >= 0.1) {
            assert_eq!(
                h.f0_hz, None,
                "{f} Hz detected as {:?} at {:.2}s",
                h.f0_hz, h.t_s
            );
        }
    }
}

#[test]
fn other_input_rates_are_supported() {
    for rate in [16_000, 44_100] {
        let s = tone(rate, 0.5, |_| 200.0, |p| 0.5 * sine(p));
        let out = analyze(&s, rate, PitchParams::default(), FEMININE);
        assert!(
            (out.len() as i64 - 50).abs() <= 1,
            "{rate}: {} hops",
            out.len()
        );
        for (t, f0, class) in out.iter().filter(|(t, ..)| *t >= 0.1) {
            let f0 = f0.unwrap_or_else(|| panic!("{rate}: unvoiced at {t}"));
            assert!((f0 / 200.0 - 1.0).abs() <= 0.01, "{rate}: {f0} Hz at {t}");
            assert_eq!(*class, Class::InRange);
        }
    }
}

#[test]
fn hop_timestamps_advance_by_10ms() {
    let (hops, _) = run(&vec![0.0; 4800]);
    let times: Vec<f64> = hops.iter().map(|h| h.t_s).collect();
    assert_eq!(times.len(), 10);
    for (i, t) in times.iter().enumerate() {
        assert!((t - 0.01 * (i + 1) as f64).abs() < 1e-9);
    }
}

#[test]
fn noise_floor_adapts_and_gates_quiet_voice() {
    let mut det = Detector::new(PitchParams::default(), RATE);
    let mut hops = Vec::new();
    let floor0 = det.noise_floor();
    assert!((linear_to_dbfs(floor0) - NOISE_FLOOR_MIN_DBFS).abs() < 0.01);

    // 10 s of −40 dBFS (full-band) noise. The frame RMS is measured after the
    // 6 kHz anti-alias filter, i.e. about −46 dBFS, so the floor rises towards
    // 3 × 0.005.
    let mut noise = Noise::new(7);
    let a = 0.01 * 3f32.sqrt();
    let s: Vec<f32> = (0..RATE as usize * 10).map(|_| a * noise.next()).collect();
    det.push(&s, &mut hops);
    let floor = det.noise_floor();
    assert!(floor > 0.012 && floor < 0.016, "floor {floor}");

    // A sine at −41 dBFS RMS (above the noise, below the floor) is gated…
    hops.clear();
    let quiet: Vec<f32> = tone(RATE, 0.5, |_| 200.0, |p| 0.012 * sine(p))
        .iter()
        .map(|x| x + a * noise.next())
        .collect();
    det.push(&quiet, &mut hops);
    assert!(hops.iter().all(|h| h.f0_hz.is_none()));

    // …while the same sine alone is voiced by a fresh detector.
    let mut fresh = Detector::new(PitchParams::default(), RATE);
    hops.clear();
    fresh.push(&tone(RATE, 0.5, |_| 200.0, |p| 0.012 * sine(p)), &mut hops);
    assert!(hops
        .iter()
        .filter(|h| h.t_s >= 0.1)
        .all(|h| h.f0_hz.is_some()));

    // Floor falls back quickly in silence.
    hops.clear();
    det.push(&vec![0.0; RATE as usize * 2], &mut hops);
    assert!((det.noise_floor() - floor0).abs() < 1e-6);
}

#[test]
fn noise_floor_clamp_is_configurable() {
    let p = PitchParams::new(60.0, 500.0, -30.0);
    let mut det = Detector::new(p, RATE);
    let mut hops = Vec::new();
    // −43 dBFS sine: voiced with the default clamp, gated with −30 dBFS.
    det.push(&tone(RATE, 0.5, |_| 220.0, |p| 0.01 * sine(p)), &mut hops);
    assert!(hops.iter().all(|h| h.f0_hz.is_none()));
}

#[test]
fn params_are_sanitized() {
    let p = PitchParams {
        detect_min_hz: 10.0,
        detect_max_hz: 20.0,
        yin_threshold: f32::NAN,
        voicing_max_dip: 0.0,
        noise_floor_min_dbfs: -200.0,
        noise_floor_factor: 0.0,
    }
    .sanitized();
    assert_eq!(p.detect_min_hz, DETECT_LIMIT_LO_HZ);
    assert_eq!(p.detect_max_hz, DETECT_LIMIT_LO_HZ + DETECT_MIN_SPAN_HZ);
    assert_eq!(p.yin_threshold, YIN_THRESHOLD);
    assert_eq!(p.voicing_max_dip, YIN_THRESHOLD);
    assert_eq!(p.noise_floor_min_dbfs, NOISE_FLOOR_MIN_DBFS_LO);
    assert_eq!(p.noise_floor_factor, 1.0);
    assert_eq!(PitchParams::default().sanitized(), PitchParams::default());
    // Extreme spans still construct and detect.
    for (lo, hi) in [(40.0, 90.0), (950.0, 1000.0), (40.0, 1000.0)] {
        let _ = Detector::new(PitchParams::new(lo, hi, -60.0), RATE);
    }
}

// ---- Tracker state machine, driven with synthetic hops ----------------------

struct HopFeed {
    tracker: Tracker,
    t: f64,
}

impl HopFeed {
    fn new() -> Self {
        Self {
            tracker: Tracker::new(FEMININE.0, FEMININE.1),
            t: 0.0,
        }
    }

    fn hop(&mut self, f0: Option<f32>) -> PitchState {
        self.t += 0.01;
        self.tracker.update(&Hop {
            t_s: self.t,
            f0_hz: f0,
            dip: 0.05,
            rms: 0.1,
        })
    }

    fn hops(&mut self, f0: Option<f32>, n: usize) -> PitchState {
        let mut s = PitchState::default();
        for _ in 0..n {
            s = self.hop(f0);
        }
        s
    }
}

#[test]
fn classification_switch_needs_120ms() {
    let mut feed = HopFeed::new();
    assert_eq!(feed.hops(Some(200.0), 50).class, Class::InRange);

    // A 100 ms excursion out of range does not switch.
    let s = feed.hops(Some(300.0), 10);
    assert_eq!(s.class, Class::InRange);
    assert_eq!(feed.hops(Some(200.0), 20).class, Class::InRange);

    // A sustained change switches 120 ms after the median moved (the median
    // of 5 follows on the 3rd new hop).
    let first_new_t = feed.t + 0.01;
    let mut switched_at = None;
    for _ in 0..40 {
        let s = feed.hop(Some(300.0));
        if s.class == Class::OutOfRange {
            switched_at = Some(s.t_s);
            break;
        }
        assert_eq!(s.class, Class::InRange);
    }
    let dt = switched_at.expect("switched") - first_new_t;
    assert!((dt - 0.13).abs() < 1e-6, "switched after {dt}");
    assert_eq!(feed.hop(Some(300.0)).f0_hz, Some(300.0));
}

#[test]
fn band_edges_have_2hz_hysteresis() {
    let mut feed = HopFeed::new();
    assert_eq!(feed.hops(Some(200.0), 20).class, Class::InRange);
    // Up to hi + 2 Hz stays in range indefinitely.
    assert_eq!(feed.hops(Some(256.9), 100).class, Class::InRange);
    assert_eq!(feed.hops(Some(163.1), 100).class, Class::InRange);
    // Beyond it switches.
    assert_eq!(feed.hops(Some(257.5), 100).class, Class::OutOfRange);
    // Coming back, the band is narrowed by 2 Hz.
    assert_eq!(feed.hops(Some(253.5), 100).class, Class::OutOfRange);
    assert_eq!(feed.hops(Some(252.5), 100).class, Class::InRange);
    // From no_voice the plain band applies immediately.
    feed.hops(None, 20);
    assert_eq!(feed.hop(Some(254.0)).class, Class::InRange);
    feed.hops(None, 20);
    assert_eq!(feed.hop(Some(256.0)).class, Class::OutOfRange);
}

#[test]
fn set_band_reclassifies_with_hysteresis() {
    let mut feed = HopFeed::new();
    assert_eq!(feed.hops(Some(120.0), 20).class, Class::OutOfRange);
    feed.tracker.set_band(85.0, 180.0);
    assert_eq!(feed.hop(Some(120.0)).class, Class::OutOfRange);
    assert_eq!(feed.hops(Some(120.0), 12).class, Class::InRange);
}

#[test]
fn octave_jump_needs_three_consecutive_hops() {
    let mut feed = HopFeed::new();
    feed.hops(Some(200.0), 10);

    // One or two octave-up hops are ignored.
    assert_eq!(feed.hop(Some(400.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(405.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(200.0)).f0_hz, Some(200.0));
    // Two octave-down hops, then an unvoiced hop breaks the run.
    assert_eq!(feed.hop(Some(100.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(100.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(None).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(100.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(200.0)).f0_hz, Some(200.0));

    // Three consecutive hops at ≈2× are accepted at once.
    assert_eq!(feed.hop(Some(398.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(400.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(402.0)).f0_hz, Some(400.0));
    assert_eq!(feed.hop(Some(400.0)).f0_hz, Some(400.0));

    // Non-octave jumps go straight into the median.
    let mut feed = HopFeed::new();
    feed.hops(Some(200.0), 10);
    feed.hops(Some(300.0), 2);
    assert_eq!(feed.hop(Some(300.0)).f0_hz, Some(300.0));
}

#[test]
fn median_of_five_rejects_outliers() {
    let mut feed = HopFeed::new();
    feed.hops(Some(200.0), 5);
    assert_eq!(feed.hop(Some(260.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(150.0)).f0_hz, Some(200.0));
    assert_eq!(feed.hop(Some(201.0)).f0_hz, Some(200.0));
}

#[test]
fn median_resets_after_100ms_unvoiced() {
    // 90 ms gap: history kept, so an octave jump is still guarded.
    let mut feed = HopFeed::new();
    feed.hops(Some(200.0), 10);
    feed.hops(None, 9);
    assert_eq!(feed.hop(Some(400.0)).f0_hz, Some(200.0));

    // 100 ms gap: history cleared, new estimate accepted immediately.
    let mut feed = HopFeed::new();
    feed.hops(Some(200.0), 10);
    feed.hops(None, 10);
    assert_eq!(feed.hop(Some(400.0)).f0_hz, Some(400.0));
}

#[test]
fn no_voice_after_150ms_unvoiced() {
    let mut feed = HopFeed::new();
    assert_eq!(feed.hops(Some(300.0), 20).class, Class::OutOfRange);
    for i in 1..=15 {
        let s = feed.hop(None);
        if i < 15 {
            assert_eq!(s.class, Class::OutOfRange, "after {i} unvoiced hops");
            assert_eq!(s.f0_hz, Some(300.0));
            assert_eq!(s.confidence, 0.0);
        } else {
            assert_eq!(s.class, Class::NoVoice);
            assert_eq!(s.f0_hz, None);
        }
    }
}

// ---- Reminder tone ----------------------------------------------------------

fn play_times(timer: &mut ReminderTimer, classes: &[(Class, f64)]) -> Vec<f64> {
    let mut t = 0.0;
    let mut plays = Vec::new();
    for &(class, duration) in classes {
        let end = t + duration;
        while t < end - 1e-9 {
            t += 0.01;
            if timer.update(t, class) {
                plays.push((t * 100.0).round() / 100.0);
            }
        }
    }
    plays
}

#[test]
fn reminder_after_delay_then_every_2s() {
    let mut timer = ReminderTimer::new(1.5);
    let plays = play_times(
        &mut timer,
        &[(Class::InRange, 1.0), (Class::OutOfRange, 7.0)],
    );
    // Out of range from t = 1.01 → first tone at 2.51, then every 2 s.
    assert_eq!(plays, vec![2.51, 4.51, 6.51]);
}

#[test]
fn reminder_resets_when_back_in_range() {
    let mut timer = ReminderTimer::new(2.0);
    let plays = play_times(
        &mut timer,
        &[
            (Class::OutOfRange, 1.9),
            (Class::InRange, 0.1),
            (Class::OutOfRange, 1.9),
            (Class::NoVoice, 0.1),
            (Class::OutOfRange, 2.5),
        ],
    );
    // Out of range again from t = 4.01 → first tone 2 s later.
    assert_eq!(plays, vec![6.01]);
    // Shorter delay takes effect for the running interval.
    let mut timer = ReminderTimer::new(10.0);
    assert!(!timer.update(0.0, Class::OutOfRange));
    timer.set_delay(0.5);
    assert!(timer.update(0.5, Class::OutOfRange));
    assert!(!timer.update(2.4, Class::OutOfRange));
    assert!(timer.update(2.5, Class::OutOfRange));
}

// ---- Vocal rest -------------------------------------------------------------

/// Feeds 10 ms hops from `from` to `to` seconds; returns toasts (t, minutes).
fn rest_feed(rest: &mut VocalRest, from: f64, to: f64, voiced: bool) -> Vec<(f64, u32)> {
    let mut toasts = Vec::new();
    let n = ((to - from) * 100.0).round() as usize;
    for i in 1..=n {
        let t = from + i as f64 * 0.01;
        let hop = Hop {
            t_s: t,
            f0_hz: voiced.then_some(200.0),
            dip: 0.0,
            rms: 0.1,
        };
        if let Some(m) = rest.update_hop(&hop) {
            toasts.push((t, m));
        }
    }
    toasts
}

#[test]
fn vocal_rest_toast_and_10_minute_repeat() {
    let mut rest = VocalRest::new(5);
    let toasts = rest_feed(&mut rest, 0.0, 1600.0, true);
    assert_eq!(toasts.len(), 3);
    // Exceeds 300 s of voicing on the hop after t = 300 s.
    assert!((toasts[0].0 - 300.01).abs() < 1e-6, "{toasts:?}");
    assert_eq!(toasts[0].1, 5);
    assert!((toasts[1].0 - 900.01).abs() < 1e-6);
    assert_eq!(toasts[1].1, 15);
    assert!((toasts[2].0 - 1500.01).abs() < 1e-6);
    assert_eq!(toasts[2].1, 25);
}

#[test]
fn vocal_rest_rolling_window() {
    let mut rest = VocalRest::new(5);
    // 6 min voiced, toast once.
    assert_eq!(rest_feed(&mut rest, 0.0, 360.0, true).len(), 1);
    assert!((rest.voiced_s() - 360.0).abs() < 1e-9);
    // Silence until t = 3700 s: buckets older than 60 min drop out.
    assert!(rest_feed(&mut rest, 360.0, 3700.0, false).is_empty());
    // Seconds 101..=360 remain (bucket placement of boundary hops may shift
    // a single 10 ms hop).
    assert!(
        (rest.voiced_s() - 259.01).abs() < 0.015,
        "{}",
        rest.voiced_s()
    );
    assert!(rest_feed(&mut rest, 3700.0, 3960.0, false).is_empty());
    assert!(rest.voiced_s() <= 0.01, "{}", rest.voiced_s());
    // 4 min voiced in the new hour stays under 5 min: no toast.
    assert!(rest_feed(&mut rest, 3960.0, 4200.0, true).is_empty());
    // A jump of more than an hour clears everything.
    assert_eq!(rest.update(20_000.0, 0.0), None);
    assert_eq!(rest.voiced_s(), 0.0);
}

#[test]
fn vocal_rest_off_still_counts() {
    let mut rest = VocalRest::new(0);
    assert!(rest_feed(&mut rest, 0.0, 400.0, true).is_empty());
    rest.set_minutes(5);
    assert_eq!(rest.update(400.5, 0.0), None, "no toast without voicing");
    assert_eq!(rest.update(400.6, 0.01), Some(6));
}

#[test]
fn bench_reports_positive_time() {
    let us = bench();
    assert!(us > 0.0 && us < 10_000.0, "{us} µs/hop");
}
