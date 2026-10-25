//! Minimal `log` backend writing one line per record to stderr (journald adds timestamps when
//! running under the systemd unit). Default level is info; `--verbose` enables debug.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct StderrLogger {
    level: LevelFilter,
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        };
        // One write per line so lines from different threads do not interleave.
        let line = format!("{level}: {}\n", record.args());
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Installs the stderr logger. Calling it again has no effect.
pub fn init(verbose: bool) {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    if log::set_boxed_logger(Box::new(StderrLogger { level })).is_ok() {
        log::set_max_level(level);
    }
}
