#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    frame_vocal_pitch::main_with_args(std::env::args_os().skip(1))
}
