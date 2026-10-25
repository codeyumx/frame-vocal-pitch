// TEMPORARY verification helper - removed before hand-off.
use std::time::Duration;
use fvp_audio::Tone;

fn main() -> Result<(), String> {
    let tone = Tone::new()?;
    tone.play(440.0, 0.5, 2000);
    std::thread::sleep(Duration::from_millis(4000));
    Ok(())
}
