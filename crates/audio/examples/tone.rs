//! Manual smoke test for [`fvp_audio::Tone`]: plays the SPEC §5 reminder tone (300 ms, 440 Hz,
//! volume 0.5) and exits, leaving no PipeWire node behind.
//!
//! ```text
//! cargo run -p fvp-audio --example tone
//! ```

use std::time::Duration;

use fvp_audio::Tone;

fn main() -> Result<(), String> {
    let tone = Tone::new()?;
    tone.status()?;
    tone.play(440.0, 0.5, 300);
    // Long enough for the stream to be created, linked and to render the burst.
    std::thread::sleep(Duration::from_millis(900));
    tone.status()?;
    drop(tone);
    println!("tone: 440 Hz, 300 ms, volume 0.5 played");
    Ok(())
}
