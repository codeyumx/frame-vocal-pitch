//! Manual smoke test for [`fvp_audio::probe`]: prints the audio part of `--probe` (SPEC §6.4).
//!
//! ```text
//! cargo run -p fvp-audio --example probe
//! ```

fn main() {
    println!("{}", fvp_audio::probe());
}
