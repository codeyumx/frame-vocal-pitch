//! The one-shot CLI modes of SPEC §6.4: `--probe`, `--analyze F.wav`, `--bench`
//! and `--render-png DIR`, plus the hidden `--check-update` diagnostic. None of them touches
//! SteamVR or a GPU (`--probe` only asks whether SteamVR is running), so they all work on a
//! desktop machine.

use std::error::Error;
use std::path::Path;

use fvp_pitch::{Class, PitchParams};

use crate::{exit, update, wav};

/// `--probe`: OpenVR library and SteamVR state, then the audio graph report.
pub fn probe() -> Result<u8, Box<dyn Error>> {
    println!("{}", crate::VERSION_LINE);

    match fvp_openvr::find_library() {
        Ok(path) => println!("OpenVR library: {}", path.display()),
        Err(e) => println!("OpenVR library: unavailable ({e})"),
    }
    // Probes as VRApplication_Background, which never launches SteamVR (SPEC §3.1).
    match fvp_openvr::is_steamvr_running() {
        Ok(true) => println!("SteamVR: running"),
        Ok(false) => println!("SteamVR: not running"),
        Err(e) => println!("SteamVR: unknown ({e})"),
    }

    let report = fvp_audio::probe();
    println!("{report}");
    Ok(exit::OK)
}

/// `--analyze F.wav`: a `time  F0  state` table plus a summary.
pub fn analyze(path: &Path) -> Result<u8, Box<dyn Error>> {
    let audio = wav::read_mono(path)?;
    if audio.samples.is_empty() {
        return Err(format!("{} contains no audio samples", path.display()).into());
    }
    if audio.sample_rate != fvp_pitch::params::INPUT_RATE_HZ {
        log::warn!(
            "{} is {} Hz; the detector is tuned for {} Hz",
            path.display(),
            audio.sample_rate,
            fvp_pitch::params::INPUT_RATE_HZ
        );
    }

    let cfg = fvp_config::load_or_init(&fvp_config::default_path());
    let params = PitchParams::new(
        cfg.audio.detect_min_hz,
        cfg.audio.detect_max_hz,
        cfg.audio.noise_floor_min_dbfs,
    );
    let band = cfg.band();
    let hops = fvp_pitch::analyze(&audio.samples, audio.sample_rate, params, band);

    println!("{}", crate::VERSION_LINE);
    println!(
        "File: {}  {} Hz, {} channel{}, {:.2} s",
        path.display(),
        audio.sample_rate,
        audio.channels,
        if audio.channels == 1 { "" } else { "s" },
        audio.duration_s()
    );
    println!(
        "Target: {}–{} Hz ({}), detection {}–{} Hz",
        band.0.round(),
        band.1.round(),
        cfg.target.preset.as_str(),
        params.detect_min_hz.round(),
        params.detect_max_hz.round()
    );
    println!();
    println!("   time       F0  state");
    for (t_s, f0_hz, class) in &hops {
        match f0_hz {
            Some(f0) => println!("{t_s:7.3}  {f0:7.1}  {}", class_name(*class)),
            None => println!("{t_s:7.3}        —  {}", class_name(*class)),
        }
    }
    println!();
    println!("{}", summary(&hops, band));
    Ok(exit::OK)
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::InRange => "in_range",
        Class::OutOfRange => "out_of_range",
        Class::NoVoice => "no_voice",
    }
}

/// One-line statistics over the hop table.
fn summary(hops: &[(f64, Option<f32>, Class)], band: (f32, f32)) -> String {
    let voiced: Vec<f32> = hops.iter().filter_map(|(_, f0, _)| *f0).collect();
    let mut sorted = voiced.clone();
    sorted.sort_by(f32::total_cmp);
    let median = match sorted.len() {
        // An even count: average the two middle estimates.
        0 => None,
        n if n % 2 == 1 => Some(sorted[n / 2]),
        n => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    };
    let in_range = hops.iter().filter(|(_, _, c)| *c == Class::InRange).count();
    let unvoiced = hops.len() - voiced.len();
    let share = |n: usize| 100.0 * n as f32 / hops.len().max(1) as f32;
    let median = match median {
        Some(hz) => format!("{hz:.1} Hz"),
        None => "—".to_owned(),
    };
    let range = match (sorted.first(), sorted.last()) {
        (Some(lo), Some(hi)) => format!("{lo:.1}–{hi:.1} Hz"),
        _ => "—".to_owned(),
    };
    format!(
        "Summary: {} hops, {:.1} s | voiced {} ({:.1} %), in band {} ({:.1} %), unvoiced {} ({:.1} %) | \
         F0 median {median}, range {range} | band {}–{} Hz",
        hops.len(),
        hops.last().map_or(0.0, |(t, _, _)| *t + fvp_pitch::params::HOP_S),
        voiced.len(),
        share(voiced.len()),
        in_range,
        share(in_range),
        unvoiced,
        share(unvoiced),
        band.0.round(),
        band.1.round(),
    )
}

/// `--bench`: analysis cost per 10 ms hop, with and without resonance feedback (SPEC §5.1).
pub fn bench() -> Result<u8, Box<dyn Error>> {
    let hop_us = fvp_pitch::params::HOP_S * 1e6;
    let pitch = fvp_pitch::bench();
    let total = fvp_pitch::bench_resonance();
    println!("{}", crate::VERSION_LINE);
    println!(
        "Analysis: {pitch:.1} µs/hop ({:.2} % of one core)",
        pitch / hop_us * 100.0
    );
    println!(
        "Analysis + resonance: {total:.1} µs/hop ({:.2} % of one core)",
        total / hop_us * 100.0
    );
    Ok(exit::OK)
}

/// `--render-png DIR`: every HUD state and the settings panel as PNGs.
pub fn render_png(dir: &Path) -> Result<u8, Box<dyn Error>> {
    let written = fvp_ui::render_all_png(dir)?;
    for path in &written {
        println!("{}", path.display());
    }
    println!("{} files written to {}", written.len(), dir.display());
    Ok(exit::OK)
}

/// `--check-update[=X.Y.Z]`: asks GitHub for the latest release now (the real `curl` request of
/// §7.1, without touching the cache) and prints the Updates status line for the running version
/// or `against`.
pub fn check_update(against: Option<&str>) -> Result<u8, Box<dyn Error>> {
    let current = match against {
        Some(text) => update::Version::parse(text)
            .ok_or_else(|| format!("invalid version '{text}' (expected X.Y.Z)"))?,
        None => update::current_version(),
    };
    println!("{}", crate::VERSION_LINE);
    println!("compared with: {current}");
    let result = update::fetch_latest(&update::Programs::default().curl);
    if let Ok(latest) = &result {
        println!("latest release: {}", latest.tag());
    }
    let ok = result.is_ok();
    let record = update::CheckRecord::new(result, None, 0);
    let (status, _) = update::check_status(Some(&record), current, 0);
    println!("status: {}", fvp_ui::update_status_text(&status));
    Ok(if ok { exit::OK } else { exit::FAILURE })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_states() {
        let hops = vec![
            (0.01, None, Class::NoVoice),
            (0.02, Some(200.0), Class::InRange),
            (0.03, Some(100.0), Class::OutOfRange),
        ];
        let text = summary(&hops, (165.0, 255.0));
        assert!(text.contains("3 hops"), "{text}");
        assert!(text.contains("voiced 2 (66.7 %)"), "{text}");
        assert!(text.contains("in band 1 (33.3 %)"), "{text}");
        assert!(text.contains("F0 median 150.0 Hz"), "{text}");
        assert!(text.contains("100.0–200.0 Hz"), "{text}");
    }

    #[test]
    fn summary_without_voicing() {
        let hops = vec![(0.01, None, Class::NoVoice)];
        let text = summary(&hops, (165.0, 255.0));
        assert!(text.contains("F0 median —"), "{text}");
    }
}
