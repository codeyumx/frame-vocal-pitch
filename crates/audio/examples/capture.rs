//! Manual smoke test for [`fvp_audio::Capture`]: records for 5 s in the mode given on the
//! command line, printing the status and the input level every 500 ms.
//!
//! ```text
//! cargo run -p fvp-audio --example capture -- bypass
//! cargo run -p fvp-audio --example capture -- processed
//! ```

use std::time::{Duration, Instant};

use fvp_audio::{Capture, CaptureStatus, MicMode};

const RUN: Duration = Duration::from_secs(5);
const REPORT: Duration = Duration::from_millis(500);

fn main() {
    let mode = match std::env::args().nth(1).as_deref() {
        Some("bypass") => MicMode::Bypass,
        Some("processed") => MicMode::Processed,
        other => {
            eprintln!("usage: capture <bypass|processed> (got {other:?})");
            std::process::exit(2);
        }
    };
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 4);
    let capture = Capture::start(mode, producer);

    let start = Instant::now();
    let mut reported = start;
    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut frames = 0u64;
    while start.elapsed() < RUN {
        std::thread::sleep(Duration::from_millis(20));
        while let Ok(sample) = consumer.pop() {
            sum_sq += f64::from(sample) * f64::from(sample);
            peak = peak.max(sample.abs());
            frames += 1;
        }
        if reported.elapsed() >= REPORT {
            reported = Instant::now();
            let rms = if frames > 0 {
                (sum_sq / frames as f64).sqrt()
            } else {
                0.0
            };
            let status = format_status(&capture.status());
            println!("{status} rms={rms:.5} peak={peak:.5} frames={frames}");
            sum_sq = 0.0;
            peak = 0.0;
            frames = 0;
        }
    }
    drop(capture);
    println!("stopped after {:.1} s", start.elapsed().as_secs_f32());
}

fn format_status(s: &CaptureStatus) -> String {
    let source = s.source_description.as_deref().unwrap_or("-");
    let fallback = s
        .fallback_reason
        .as_deref()
        .map(|r| format!(" fallback={r:?}"))
        .unwrap_or_default();
    let filters = match s.filters_active_for_others {
        Some(true) => "filters=on",
        Some(false) => "filters=off",
        None => "filters=n/a",
    };
    format!(
        "requested={:?} active={:?} source={source:?} {filters} muted={} healthy={}{fallback}",
        s.requested, s.active, s.muted, s.healthy
    )
}
