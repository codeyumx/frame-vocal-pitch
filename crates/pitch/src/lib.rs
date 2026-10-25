//! Pitch detection for frame-vocal-pitch (SPEC §5).
//!
//! Pipeline: 48 kHz mono input → 3:1 low-pass decimation to 16 kHz → YIN
//! (de Cheveigné & Kawahara 2002) every 10 ms → voicing decision against an
//! adaptive noise floor → [`Tracker`] (median of 5, octave-jump guard,
//! classification hysteresis, `no_voice` timeout).
//!
//! [`ReminderTimer`] and [`VocalRest`] implement the two time-based features
//! that consume the tracker output. All tunable constants live in [`params`].
//!
//! Nothing in this crate allocates per hop after construction; the caller owns
//! the `Vec<Hop>` passed to [`Detector::push`] and should reuse it.

#![forbid(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod analyze;
mod decimate;
pub mod params;
mod reminder;
mod tracker;
mod vocal_rest;
mod yin;

pub use analyze::{analyze, bench};
pub use params::PitchParams;
pub use reminder::ReminderTimer;
pub use tracker::{Class, PitchState, Tracker};
pub use vocal_rest::VocalRest;
pub use yin::{Detector, Hop};

#[cfg(test)]
mod tests;
