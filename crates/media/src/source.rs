use std::fmt;
use std::path::Path;
use std::sync::Arc;

use crate::{AudioBuffer, MediaError, Peaks, decode, resample};

/// Everything known about a loaded file.
pub struct Source {
    /// The file's own sample rate.
    pub native_rate: u32,
    /// Length at the native rate.
    pub native_frames: u64,
    /// Channels in the file, before folding to stereo.
    pub channels: u16,
    /// Waveform summary, indexed by native frame so it doesn't depend on
    /// the session rate and survives a device change.
    pub peaks: Peaks,
    /// The audio converted to the rate it was loaded for, ready to play.
    pub audio: Arc<AudioBuffer>,
}

impl fmt::Debug for Source {
    // The derived version would print every sample.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("native_rate", &self.native_rate)
            .field("native_frames", &self.native_frames)
            .field("channels", &self.channels)
            .field("audio_rate", &self.audio.sample_rate())
            .field("audio_frames", &self.audio.frames())
            .finish()
    }
}

/// Decode `path`, summarize it for drawing, and convert it to `target_rate`.
/// Slow (disk, decoding, resampling), so call it off the UI thread; `Loader`
/// does that for you.
pub fn load(path: &Path, target_rate: u32) -> Result<Source, MediaError> {
    let (native, channels) = decode(path)?;
    let peaks = Peaks::new(&native);
    let native_rate = native.sample_rate();
    let native_frames = native.frames() as u64;
    let audio = if native_rate == target_rate {
        native
    } else {
        resample(&native, target_rate)
    };
    Ok(Source {
        native_rate,
        native_frames,
        channels,
        peaks,
        audio: Arc::new(audio),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{TempDir, WavFormat, sine, write_wav};

    #[test]
    fn converts_to_target_rate() {
        let dir = TempDir::new("load");
        let path = dir.join("tone.wav");
        write_wav(
            &path,
            44100,
            1,
            WavFormat::Float32,
            &sine(44100, 440.0, 4410),
        );

        let source = load(&path, 48000).unwrap();
        assert_eq!(source.native_rate, 44100);
        assert_eq!(source.native_frames, 4410);
        assert_eq!(source.channels, 1);
        assert_eq!(source.peaks.frames(), 4410);
        assert_eq!(source.audio.sample_rate(), 48000);
        assert_eq!(source.audio.frames(), 4800);
        let (min, max) = source.peaks.min_max(0.0, 4410.0);
        assert!(min < -0.99 && max > 0.99);
    }

    #[test]
    fn same_rate_keeps_samples() {
        let dir = TempDir::new("load-same");
        let path = dir.join("stereo.wav");
        write_wav(&path, 48000, 2, WavFormat::Float32, &[0.1, 0.2, 0.3, 0.4]);

        let source = load(&path, 48000).unwrap();
        assert_eq!(source.audio.left(), &[0.1, 0.3]);
        assert_eq!(source.audio.right(), &[0.2, 0.4]);
    }

    #[test]
    fn errors_pass_through() {
        let dir = TempDir::new("load-missing");
        assert!(matches!(
            load(&dir.join("missing.flac"), 48000),
            Err(MediaError::Io(_))
        ));
    }
}
