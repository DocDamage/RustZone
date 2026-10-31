//! Mastering Studio: a standalone audio mastering workstation.

// DSP code indexes several buffers in step; explicit index loops read better there
#![allow(clippy::needless_range_loop)]

pub mod audio;
pub mod dsp;
pub mod midi;
pub mod session;
pub mod ui;
pub mod vst3;

