//! PipeWire audio I/O for frame-vocal-pitch.
//!
//! - [`Capture`]: 48 kHz mono capture thread on an autoconnected stream from the default source
//!   (Valve's mic processing applies, SPEC §6.3).
//! - [`Tone`]: on-demand reminder tone playback (SPEC §5).
//! - [`probe`]: one-shot read-only snapshot of the default source for `--probe` (SPEC §6.4).
//!
//! Nothing in this crate writes PipeWire/WirePlumber configuration or metadata.
#![deny(unsafe_op_in_unsafe_fn)]

mod capture;
mod format;
mod graph;
mod mute;
mod probe;
mod pwgraph;
mod tone;

pub use capture::{Capture, CAPTURE_NODE_NAME, SAMPLE_RATE_HZ};
pub use probe::{probe, ProbeReport};
pub use tone::{Tone, TONE_NODE_NAME, TONE_RATE_HZ};

/// What the capture thread is doing, for the settings status line (SPEC §4.2 / §6.3).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureStatus {
    /// Default source the stream records from, human readable.
    pub source_description: Option<String>,
    /// Current problem, e.g. "PipeWire connection lost"; `None` while the stream exists.
    pub error: Option<String>,
    /// User mute of the default source.
    pub muted: bool,
    /// Stream connected and buffers arriving.
    pub healthy: bool,
}
