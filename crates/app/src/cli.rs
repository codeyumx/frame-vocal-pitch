//! Command line (SPEC §6.4) and mode dispatch.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::{commands, exit, runtime, VERSION_LINE};

/// Text printed by `--help`.
pub const USAGE: &str = "\
Usage:
  frame-vocal-pitch                   run (normal mode)
  frame-vocal-pitch --version         print the version
  frame-vocal-pitch --verbose         debug logging (combines with any mode)
  frame-vocal-pitch --probe           print SteamVR, OpenVR and microphone environment; exit
  frame-vocal-pitch --analyze F.wav   print time, F0 and state per hop of a WAV file; exit
  frame-vocal-pitch --bench           time the analysis on synthetic audio (µs/hop); exit
  frame-vocal-pitch --render-png DIR  render HUD states and settings panel to PNGs; exit
  frame-vocal-pitch --help            print this help
";

/// What the process does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Run,
    Version,
    Help,
    Probe,
    Analyze(PathBuf),
    Bench,
    RenderPng(PathBuf),
    /// Hidden diagnostic `--check-update[=X.Y.Z]`: ask GitHub for the latest release now and
    /// print the Updates status line, comparing against `X.Y.Z` instead of the running version.
    CheckUpdate(Option<String>),
}

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub mode: Mode,
    pub verbose: bool,
}

/// Invalid command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for UsageError {}

impl Args {
    /// Parses the arguments after the program name. Accepts `--opt VALUE` and `--opt=VALUE`;
    /// at most one mode option; `--verbose` combines with any mode.
    pub fn parse<I>(args: I) -> Result<Args, UsageError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut mode: Option<(String, Mode)> = None;
        let mut verbose = false;
        let mut it = args.into_iter();

        while let Some(arg) = it.next() {
            let text = arg
                .to_str()
                .ok_or_else(|| UsageError(format!("invalid argument {arg:?}")))?
                .to_owned();
            let (name, inline) = match text.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(OsString::from(v))),
                _ => (text.clone(), None),
            };
            let takes_value = matches!(name.as_str(), "--analyze" | "--render-png");
            let optional_value = name == "--check-update";
            if !takes_value && !optional_value && inline.is_some() {
                return Err(UsageError(format!("option {name} takes no value")));
            }
            let mut value = || -> Result<PathBuf, UsageError> {
                let v = match inline.clone() {
                    Some(v) => v,
                    None => it
                        .next()
                        .ok_or_else(|| UsageError(format!("option {name} needs a value")))?,
                };
                if v.is_empty() {
                    return Err(UsageError(format!("option {name} needs a value")));
                }
                Ok(PathBuf::from(v))
            };
            let selected = match name.as_str() {
                "--verbose" => {
                    verbose = true;
                    continue;
                }
                "--version" => Mode::Version,
                "-h" | "--help" => Mode::Help,
                "--probe" => Mode::Probe,
                "--bench" => Mode::Bench,
                "--analyze" => Mode::Analyze(value()?),
                "--render-png" => Mode::RenderPng(value()?),
                "--check-update" => {
                    Mode::CheckUpdate(inline.as_ref().map(|v| v.to_string_lossy().into_owned()))
                }
                _ => return Err(UsageError(format!("unknown argument '{text}'"))),
            };
            if let Some((previous, _)) = &mode {
                return Err(UsageError(format!(
                    "{previous} and {name} cannot be combined"
                )));
            }
            mode = Some((name, selected));
        }

        Ok(Args {
            mode: mode.map_or(Mode::Run, |(_, m)| m),
            verbose,
        })
    }
}

/// Runs the selected mode and returns the process exit status.
pub fn run(args: &Args) -> Result<u8, Box<dyn Error>> {
    match &args.mode {
        Mode::Version => {
            println!("{VERSION_LINE}");
            Ok(exit::OK)
        }
        Mode::Help => {
            print!("{USAGE}");
            Ok(exit::OK)
        }
        Mode::Probe => commands::probe(),
        Mode::Analyze(path) => commands::analyze(path),
        Mode::Bench => commands::bench(),
        Mode::RenderPng(dir) => commands::render_png(dir),
        Mode::CheckUpdate(against) => commands::check_update(against.as_deref()),
        Mode::Run => runtime::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, UsageError> {
        Args::parse(args.iter().map(OsString::from))
    }

    fn mode(args: &[&str]) -> Mode {
        parse(args).unwrap().mode
    }

    #[test]
    fn no_args_runs() {
        assert_eq!(
            parse(&[]).unwrap(),
            Args {
                mode: Mode::Run,
                verbose: false
            }
        );
        assert_eq!(
            parse(&["--verbose"]).unwrap(),
            Args {
                mode: Mode::Run,
                verbose: true
            }
        );
    }

    #[test]
    fn modes() {
        assert_eq!(mode(&["--version"]), Mode::Version);
        assert_eq!(mode(&["--help"]), Mode::Help);
        assert_eq!(mode(&["-h"]), Mode::Help);
        assert_eq!(mode(&["--probe"]), Mode::Probe);
        assert_eq!(mode(&["--bench"]), Mode::Bench);
        assert_eq!(
            mode(&["--analyze", "a b.wav"]),
            Mode::Analyze("a b.wav".into())
        );
        assert_eq!(mode(&["--analyze=x.wav"]), Mode::Analyze("x.wav".into()));
        assert_eq!(
            mode(&["--render-png", "out"]),
            Mode::RenderPng("out".into())
        );
        assert_eq!(
            mode(&["--render-png=out=1"]),
            Mode::RenderPng("out=1".into())
        );
        assert_eq!(mode(&["--check-update"]), Mode::CheckUpdate(None));
        assert_eq!(
            mode(&["--check-update=0.2.0"]),
            Mode::CheckUpdate(Some("0.2.0".into()))
        );
    }

    #[test]
    fn verbose_combines_in_any_position() {
        let a = parse(&["--verbose", "--probe"]).unwrap();
        assert_eq!((a.mode, a.verbose), (Mode::Probe, true));
        let a = parse(&["--analyze", "f.wav", "--verbose"]).unwrap();
        assert_eq!((a.mode, a.verbose), (Mode::Analyze("f.wav".into()), true));
    }

    #[test]
    fn errors() {
        assert!(parse(&["--analyze"]).is_err());
        assert!(parse(&["--analyze="]).is_err());
        assert!(parse(&["--render-png"]).is_err());
        assert!(parse(&["--probe", "--bench"]).is_err());
        assert!(parse(&["--version", "--version"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
        assert!(parse(&["file.wav"]).is_err());
        assert!(parse(&["--probe=1"]).is_err());
    }

    #[test]
    fn missing_analyze_file_is_a_runtime_error() {
        // Not a usage error: the command line parsed, so `run` reports the missing file.
        let err = analyze_missing().unwrap_err();
        assert!(err.to_string().contains("cannot read"), "{err}");
    }

    fn analyze_missing() -> Result<u8, Box<dyn Error>> {
        super::run(&Args {
            mode: Mode::Analyze(PathBuf::from("/nonexistent/fvp-missing.wav")),
            verbose: false,
        })
    }
}
