//! PipeWire audio I/O for frame-vocal-pitch.
//!
//! - [`Capture`]: 48 kHz mono capture thread. In `bypass` mode it owns manual links from the
//!   hardware mic to its own (virtual, non-autoconnected) stream, so Valve's processing keeps
//!   running for every other app while the trainer reads the raw signal (SPEC §6.3).
//! - [`Tone`]: on-demand reminder tone playback (SPEC §5).
//! - [`probe`]: one-shot read-only snapshot of the graph for `--probe` (SPEC §6.4).
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
pub use graph::MicFilter;
pub use probe::{probe, HardwareSource, ProbeReport};
pub use tone::{Tone, TONE_NODE_NAME, TONE_RATE_HZ};

/// Requested input path (`audio.mic_mode`, SPEC §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicMode {
    /// Read the hardware mic directly through links this app owns.
    Bypass,
    /// Autoconnect to the default source and let WirePlumber insert Valve's filters.
    Processed,
}

/// What the capture thread is actually doing, for the settings status line (SPEC §4.2 / §6.3).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureStatus {
    /// Mode the config asked for.
    pub requested: MicMode,
    /// Mode currently in effect (differs after a bypass fallback).
    pub active: MicMode,
    /// Node the samples come from, human readable.
    pub source_description: Option<String>,
    /// Why bypass is not in effect, e.g. "Bypass unavailable: … — using processed mic".
    pub fallback_reason: Option<String>,
    /// Whether Valve's mic filters are currently enabled for other apps (`None`: no such nodes).
    pub filters_active_for_others: Option<bool>,
    /// User mute of the default source (or of the hardware node in bypass mode).
    pub muted: bool,
    /// Stream connected and buffers arriving.
    pub healthy: bool,
}
