//! Process shell of frame-vocal-pitch: command line (SPEC §6.4), single instance and signals
//! (§6.2), logging, the autostart user unit (§7), the WAV reader behind `--analyze`, the
//! capture→pitch analysis thread (§3) and the overlay run loop (§3.1, §4).

#![forbid(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod analysis;
pub mod autostart;
pub mod cli;
pub mod commands;
pub mod instance;
pub mod logging;
pub mod runtime;
pub mod signals;
pub mod wav;

use std::process::ExitCode;

/// `frame-vocal-pitch <semver>`, printed by `--version`.
pub const VERSION_LINE: &str = concat!("frame-vocal-pitch ", env!("CARGO_PKG_VERSION"));

/// Process exit statuses.
pub mod exit {
    /// Normal exit.
    pub const OK: u8 = 0;
    /// Runtime failure.
    pub const FAILURE: u8 = 1;
    /// Invalid command line.
    pub const USAGE: u8 = 2;
    /// The user chose Quit; the systemd unit's `RestartPreventExitStatus=3` keeps it stopped.
    pub const QUIT: u8 = 3;
}

/// Parses `args` (without the program name), sets up logging and runs the selected mode.
pub fn main_with_args<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let args = match cli::Args::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("frame-vocal-pitch: {e}");
            eprintln!("Try 'frame-vocal-pitch --help'.");
            return ExitCode::from(exit::USAGE);
        }
    };
    logging::init(args.verbose);
    match cli::run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            log::error!("{e}");
            ExitCode::from(exit::FAILURE)
        }
    }
}
