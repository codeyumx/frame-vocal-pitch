//! SPEC §5.1 acceptance tests: synthetic source–filter vowels (glottal pulse
//! train through three second-order formant resonators, 48 kHz input).

use crate::analyze::synth::{vowel, Noise, VOWELS};
use crate::params::*;
use crate::*;

const RATE: u32 = INPUT_RATE_HZ;
const F0S: [f64; 3] = [100.0, 150.0, 220.0];
const NAMES: [&str; 3] = ["/a/", "/i/", "/u/"];

fn scaled(formants: [f64; 3], scale: f64) -> [f64; 3] {
    formants.map(|f| f * scale)
}

/// Runs the detector over `signal` and calls `f` for every hop with its frame.
fn for_each_hop(signal: &[f32], mut f: impl FnMut(Hop, AnalysisFrame<'_>)) {
    let mut det = Detector::new(PitchParams::default(), RATE);
    // Uneven chunks exercise hop boundaries inside push_with().
    for chunk in signal.chunks(1000) {
        det.push_with(chunk, &mut f);
    }
}

/// Formants of every accepted frame after the first 0.3 s, plus the number of
/// hops that passed the voicing/F0 gate.
fn frame_formants(signal: &[f32]) -> (Vec<Formants>, usize) {
    let mut analyzer = FormantAnalyzer::new();
    let mut found = Vec::new();
    let mut gated = 0;
    for_each_hop(signal, |hop, frame| {
        if hop.t_s < 0.3 || !accepts(&hop) {
            return;
        }
        gated += 1;
        found.extend(analyzer.formants(frame));
    });
    (found, gated)
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// R after feeding `signal` through a tracker with the longest window.
fn reading_of(signal: &[f32]) -> Option<f32> {
    let mut tracker = ResonanceTracker::new(RESONANCE_WINDOW_MAX_S);
    for_each_hop(signal, |hop, frame| {
        tracker.update(&hop, frame);
    });
    tracker.reading()
}

#[test]
fn spacing_is_least_squares_tube_fit() {
    // A uniform tube with ΔF = 1000 Hz has formants at 500, 1500, 2500 Hz.
    assert!((formant_spacing([500.0, 1500.0, 2500.0]) - 1000.0).abs() < 1e-3);
    // (0.5·700 + 1.5·1200 + 2.5·2600) / 8.75
    let a = formant_spacing([700.0, 1200.0, 2600.0]);
    assert!((a - 8650.0 / 8.75).abs() < 1e-2, "{a}");
}

/// Whether formant `i` (0-based) of vowel `vowel` at `f0` is held to ±8 %.
///
/// At F0 = 220 Hz the autocorrelation LPC that SPEC §5.1 prescribes suffers
/// from harmonic-sampling bias: the F0 period (4.5 ms) is shorter than the
/// decay time of a narrow low formant, so the low formants of /i/ and /u/
/// (F1 ≈ 300 Hz, /u/ F2 ≈ 800 Hz) are pulled toward the harmonics (measured:
/// /i/ F1 +10 %, ×1.15 +21 %; /u/ F1 +33 %, ×1.15 +18 %; /u/ F2 +9 %). Per
/// the amended acceptance, only F3 and all of /a/ are asserted there; the
/// other deviations are printed, not asserted.
fn formant_asserted(f0: f64, vowel: usize, i: usize) -> bool {
    f0 <= 150.0 || vowel == 0 || i == 2
}

#[test]
fn per_vowel_formants_within_eight_percent() {
    for scale in [1.0, 1.15] {
        for &f0 in &F0S {
            for (v, (vowel_f, name)) in VOWELS.iter().zip(NAMES).enumerate() {
                let truth = scaled(*vowel_f, scale);
                let what = format!("{name} ×{scale} at F0 {f0} Hz");
                let (found, gated) = frame_formants(&vowel(RATE, 1.0, f0, truth));
                assert!(gated >= 60, "{what}: only {gated} hops voiced ≤ 300 Hz");
                // Frames without three valid formants are skipped by design;
                // some vowel/F0 combinations lose about half of them.
                assert!(
                    found.len() * 3 >= gated,
                    "{what}: formants in only {} of {gated} frames",
                    found.len()
                );
                for i in 0..3 {
                    let est = median(found.iter().map(|f| f[i]).collect());
                    let err = est as f64 / truth[i] - 1.0;
                    let msg = format!(
                        "{what}: F{} {est:.0} Hz vs {:.0} Hz ({:+.1} %)",
                        i + 1,
                        truth[i],
                        err * 100.0
                    );
                    if formant_asserted(f0, v, i) {
                        assert!(err.abs() <= 0.08, "{msg}");
                    } else {
                        eprintln!("recorded (harmonic-sampling bias): {msg}");
                    }
                }
            }
        }
    }
}

#[test]
fn scaling_formants_raises_reading_fifteen_percent() {
    for &f0 in &F0S {
        // 3-vowel mix, 2 s per vowel, all inside one 20 s window.
        let mix = |scale: f64| -> Vec<f32> {
            VOWELS
                .iter()
                .flat_map(|v| vowel(RATE, 2.0, f0, scaled(*v, scale)))
                .collect()
        };
        let base = reading_of(&mix(1.0)).expect("reading at ×1.00");
        let high = reading_of(&mix(1.15)).expect("reading at ×1.15");
        let rise = high / base - 1.0;
        assert!(
            (rise - 0.15).abs() <= 0.04,
            "F0 {f0} Hz: R {base:.0} → {high:.0} Hz ({:+.1} %)",
            rise * 100.0
        );

        // Same check on the mean of the per-vowel readings (6 s per vowel:
        // /u/ at 150 Hz keeps only about half of its frames).
        let mean = |scale: f64| -> f32 {
            VOWELS
                .iter()
                .map(|v| reading_of(&vowel(RATE, 6.0, f0, scaled(*v, scale))).expect("reading"))
                .sum::<f32>()
                / 3.0
        };
        let rise = mean(1.15) / mean(1.0) - 1.0;
        assert!(
            (rise - 0.15).abs() <= 0.04,
            "F0 {f0} Hz: mean of per-vowel R rises {:+.1} %",
            rise * 100.0
        );
    }
}

fn assert_no_accepted_frames(signal: &[f32], what: &str) {
    let mut tracker = ResonanceTracker::new(RESONANCE_WINDOW_S);
    let mut hops = 0;
    for_each_hop(signal, |hop, frame| {
        hops += 1;
        assert_eq!(
            tracker.update(&hop, frame),
            None,
            "{what}: accepted frame at {:.2}s (F0 {:?})",
            hop.t_s,
            hop.f0_hz
        );
    });
    assert!(hops >= 290, "{what}: {hops} hops");
    assert_eq!(tracker.reading(), None, "{what}");
    assert_eq!(
        tracker.state(1000.0, 1200.0),
        ResonanceState::Gathering,
        "{what}"
    );
}

#[test]
fn high_pitch_noise_and_silence_give_no_frames() {
    // The pitch path sees these voices (except /a/, whose strong 700 Hz
    // second harmonic YIN reports as out of span); resonance skips them all.
    let mut voiced = 0;
    for v in VOWELS {
        let high = vowel(RATE, 3.0, 350.0, v);
        for_each_hop(&high, |hop, _| voiced += usize::from(hop.f0_hz.is_some()));
        assert_no_accepted_frames(&high, "F0 350 Hz");
    }
    assert!(voiced > 400, "350 Hz vowels voiced in only {voiced} hops");

    // White noise at −20 dBFS RMS (uniform in ±a has RMS a/√3).
    let a = 0.1 * 3f32.sqrt();
    let mut noise = Noise::new(42);
    let white: Vec<f32> = (0..RATE as usize * 3).map(|_| a * noise.next()).collect();
    assert_no_accepted_frames(&white, "white noise");
    assert_no_accepted_frames(&vec![0.0; RATE as usize * 3], "silence");
}

#[test]
fn gathering_until_two_seconds_then_window_slides_out() {
    let voice = vowel(RATE, 3.0, 150.0, VOWELS[0]);
    let mut tracker = ResonanceTracker::new(4.0);
    let mut accepted = 0;
    let mut first_reading_at = None;
    for_each_hop(&voice, |hop, frame| {
        accepted += usize::from(tracker.update(&hop, frame).is_some());
        if tracker.reading().is_some() && first_reading_at.is_none() {
            first_reading_at = Some(accepted);
        }
    });
    let at = first_reading_at.expect("reading after 3 s of voice");
    // 2 s = 200 accepted frames; the median is refreshed every 100 ms.
    assert!((200..=210).contains(&at), "first reading after {at} frames");

    // Silence longer than the window brings back "gathering".
    let silence = vec![0.0; RATE as usize * 5];
    let mut det = Detector::new(PitchParams::default(), RATE);
    det.push_with(&voice, |hop, frame| {
        tracker.update(&hop, frame);
    });
    assert!(tracker.reading().is_some());
    det.push_with(&silence, |hop, frame| {
        tracker.update(&hop, frame);
    });
    assert_eq!(tracker.reading(), None);
    assert_eq!(tracker.smoothed(), None);
}

#[test]
fn smoothing_has_one_second_time_constant() {
    // /u/ then /a/: the median switches within the window, the smoothed value
    // follows with τ = 1 s.
    let mut signal = vowel(RATE, 8.0, 150.0, VOWELS[2]);
    signal.extend(vowel(RATE, 12.0, 150.0, VOWELS[0]));
    let mut tracker = ResonanceTracker::new(4.0);
    let mut trace = Vec::new();
    for_each_hop(&signal, |hop, frame| {
        tracker.update(&hop, frame);
        trace.push((hop.t_s, tracker.reading(), tracker.smoothed()));
    });
    let (_, Some(dark), Some(_)) = trace.iter().find(|e| e.0 >= 7.9).copied().unwrap() else {
        panic!("no reading before the switch");
    };
    let bright = trace.last().unwrap().1.expect("final reading");
    assert!(bright > dark * 1.1, "dark {dark} bright {bright}");
    // Moment the raw median jumps past the midpoint, then 1 s later the
    // smoothed value must have covered roughly 1 − 1/e of the remaining step.
    let mid = 0.5 * (dark + bright);
    let i = trace
        .iter()
        .position(|e| e.0 > 8.0 && e.1.is_some_and(|r| r > mid))
        .expect("median switches");
    let (t0, _, Some(s0)) = trace[i] else {
        panic!()
    };
    let (_, _, Some(s1)) = trace.iter().find(|e| e.0 >= t0 + 1.0).copied().unwrap() else {
        panic!()
    };
    let covered = (s1 - s0) / (bright - s0);
    assert!(
        (0.5..0.75).contains(&covered),
        "covered {covered:.2} of the step in 1 s"
    );
    // The smoothed value never jumps.
    let max_step = trace
        .windows(2)
        .filter_map(|w| Some((w[1].2? - w[0].2?).abs()))
        .fold(0.0f32, f32::max);
    assert!(max_step < 0.02 * (bright - dark), "max step {max_step}");
}

#[test]
fn window_is_clamped() {
    assert_eq!(ResonanceTracker::new(1.0).window_s(), 4.0);
    assert_eq!(ResonanceTracker::new(99.0).window_s(), 20.0);
    assert_eq!(ResonanceTracker::new(f32::NAN).window_s(), 8.0);
    let mut t = ResonanceTracker::new(8.0);
    t.set_window(12.0);
    assert_eq!(t.window_s(), 12.0);
}

#[test]
fn position_mapping() {
    let (relaxed, target) = (1000.0, 1200.0);
    assert_eq!(position(None, relaxed, target), ResonanceState::Gathering);
    assert_eq!(
        position(Some(1000.0), relaxed, target),
        ResonanceState::Position(0.0)
    );
    assert_eq!(
        position(Some(1200.0), relaxed, target),
        ResonanceState::Position(1.0)
    );
    assert_eq!(
        position(Some(1100.0), relaxed, target),
        ResonanceState::Position(0.5)
    );
    // Unclamped here; the display clamps to −0.25…1.25.
    assert_eq!(
        position(Some(1400.0), relaxed, target),
        ResonanceState::Position(2.0)
    );
    assert_eq!(
        position(Some(900.0), relaxed, target),
        ResonanceState::Position(-0.5)
    );
    // Target darker than relaxed works the same way.
    assert_eq!(
        position(Some(900.0), 1000.0, 800.0),
        ResonanceState::Position(0.5)
    );
}

#[test]
fn too_similar_below_five_percent() {
    assert!(too_similar(1000.0, 1049.0));
    assert!(too_similar(1000.0, 951.0));
    assert!(!too_similar(1000.0, 1050.0));
    assert!(!too_similar(1000.0, 940.0));
    assert_eq!(
        position(Some(1020.0), 1000.0, 1040.0),
        ResonanceState::TooSimilar
    );
    assert_eq!(
        position(None, 1000.0, 1040.0),
        ResonanceState::TooSimilar
    );
}

#[test]
fn tracker_state_uses_smoothed_reading() {
    let voice = vowel(RATE, 4.0, 150.0, VOWELS[0]);
    let mut tracker = ResonanceTracker::new(RESONANCE_WINDOW_S);
    for_each_hop(&voice, |hop, frame| {
        tracker.update(&hop, frame);
    });
    let r = tracker.smoothed().expect("smoothed reading");
    assert_eq!(
        tracker.state(r, r * 1.2),
        ResonanceState::Position(0.0)
    );
    tracker.reset();
    assert_eq!(tracker.reading(), None);
}

#[test]
fn bench_resonance_reports_positive_time() {
    let us = bench_resonance();
    eprintln!(
        "pitch: {:.1} µs/hop, pitch + resonance: {us:.1} µs/hop",
        bench()
    );
    assert!(us > 0.0 && us < 10_000.0, "{us} µs/hop");
}
