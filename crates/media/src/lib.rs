//! Audio files: decoding, sample-rate conversion and waveform peaks.
//!
//! The usual entry point is `Loader`, which runs `load` on a worker thread:
//! decode the file, build `Peaks` for drawing from the original audio, and
//! `resample` it to the session rate for the engine.

mod beats;
mod buffer;
mod decode;
mod error;
mod loader;
mod peaks;
mod process;
mod renderer;
mod resample;
mod source;
mod stretch;
#[cfg(test)]
mod test_util;
mod transients;
mod wav;

pub use buffer::AudioBuffer;
pub use decode::{decode, is_supported_extension};
pub use error::MediaError;
pub use loader::{Loaded, Loader};
pub use peaks::Peaks;
pub use process::{ClipProcess, StretchMode, detect_transients, peak_level, process_clip};
pub use renderer::{Rendered, Renderer};
pub use resample::resample;
pub use source::{Source, load};
pub use wav::{WavFormat, write_wav, write_wav_as};
