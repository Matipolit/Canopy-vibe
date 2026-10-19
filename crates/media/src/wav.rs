//! Writing audio to disk: 32-bit float for freezing and consolidating, so
//! nothing an instrument produces is clipped or rounded, and 16 or 24-bit
//! integer too for exports.

use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::AudioBuffer;

/// How samples are stored in a WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WavFormat {
    /// CD quality, dithered.
    Pcm16,
    Pcm24,
    /// Lossless, even above full scale.
    #[default]
    Float32,
}

impl WavFormat {
    pub const ALL: [WavFormat; 3] = [WavFormat::Pcm16, WavFormat::Pcm24, WavFormat::Float32];

    pub fn label(self) -> &'static str {
        match self {
            WavFormat::Pcm16 => "16-bit",
            WavFormat::Pcm24 => "24-bit",
            WavFormat::Float32 => "32-bit float",
        }
    }
}

/// Writes `audio` as a stereo 32-bit float WAV, creating parent folders.
pub fn write_wav(path: &Path, audio: &AudioBuffer) -> io::Result<()> {
    write_wav_as(path, audio, WavFormat::Float32)
}

/// Writes `audio` as a stereo WAV in `format`, creating parent folders.
/// Integer formats clip at full scale; 16-bit is dithered.
pub fn write_wav_as(path: &Path, audio: &AudioBuffer, format: WavFormat) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if format != WavFormat::Float32 {
        return write_pcm(path, audio, format);
    }
    let channels = 2u16;
    let bytes_per_frame = u32::from(channels) * 4;
    let data_len = u32::try_from(audio.frames() as u64 * u64::from(bytes_per_frame))
        .map_err(|_| io::Error::other("too long for a WAV file"))?;
    let rate = audio.sample_rate();
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(4 + 26 + 12 + 8 + data_len).to_le_bytes())?;
    out.write_all(b"WAVE")?;
    // fmt: WAVE_FORMAT_IEEE_FLOAT, with the (empty) extension size field.
    out.write_all(b"fmt ")?;
    out.write_all(&18u32.to_le_bytes())?;
    out.write_all(&3u16.to_le_bytes())?;
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * bytes_per_frame).to_le_bytes())?;
    out.write_all(&(bytes_per_frame as u16).to_le_bytes())?;
    out.write_all(&32u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    // Non-PCM formats should say how many frames there are.
    out.write_all(b"fact")?;
    out.write_all(&4u32.to_le_bytes())?;
    out.write_all(&(audio.frames() as u32).to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for (l, r) in audio.left().iter().zip(audio.right()) {
        out.write_all(&l.to_le_bytes())?;
        out.write_all(&r.to_le_bytes())?;
    }
    out.flush()
}

fn write_pcm(path: &Path, audio: &AudioBuffer, format: WavFormat) -> io::Result<()> {
    let channels = 2u16;
    let bytes = if format == WavFormat::Pcm16 { 2 } else { 3 };
    let bytes_per_frame = u32::from(channels) * bytes;
    let data_len = u32::try_from(audio.frames() as u64 * u64::from(bytes_per_frame))
        .map_err(|_| io::Error::other("too long for a WAV file"))?;
    let rate = audio.sample_rate();
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(4 + 24 + 8 + data_len).to_le_bytes())?;
    out.write_all(b"WAVE")?;
    out.write_all(b"fmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * bytes_per_frame).to_le_bytes())?;
    out.write_all(&(bytes_per_frame as u16).to_le_bytes())?;
    out.write_all(&(bytes as u16 * 8).to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    // Triangular dither for 16-bit: two uniform noises of one step each.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut uniform = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for (l, r) in audio.left().iter().zip(audio.right()) {
        for sample in [*l, *r] {
            let sample = f64::from(sample);
            if format == WavFormat::Pcm16 {
                let dither = uniform() - uniform();
                let value = (sample * 32_767.0 + dither)
                    .round()
                    .clamp(-32_768.0, 32_767.0);
                out.write_all(&(value as i16).to_le_bytes())?;
            } else {
                let value = (sample * 8_388_607.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0);
                out.write_all(&(value as i32).to_le_bytes()[..3])?;
            }
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_formats_decode_close_to_the_original() {
        let dir = std::env::temp_dir().join(format!("canopy-wav-pcm-{}", std::process::id()));
        let left: Vec<f32> = (0..1000).map(|i| (i as f32 / 500.0) - 1.0).collect();
        let right: Vec<f32> = left.iter().map(|s| -s * 1.5).collect();
        let audio = AudioBuffer::new(44_100, left.clone(), right);
        for (format, step) in [
            (WavFormat::Pcm16, 1.0 / 32_767.0),
            (WavFormat::Pcm24, 1.0 / 8_388_607.0),
        ] {
            let path = dir.join(format!("{format:?}.wav"));
            write_wav_as(&path, &audio, format).unwrap();
            let (decoded, channels) = crate::decode(&path).unwrap();
            assert_eq!(channels, 2);
            assert_eq!(decoded.sample_rate(), 44_100);
            assert_eq!(decoded.frames(), 1000);
            for (a, b) in decoded.left().iter().zip(&left) {
                // Dither (up to a step), rounding, and decoders dividing by 2^n.
                assert!((a - b).abs() <= 3.0 * step as f32, "{format:?}: {a} vs {b}");
            }
            // Past full scale, integer formats clip.
            assert!(decoded.right().iter().all(|s| s.abs() <= 1.0));
            assert!(decoded.right()[0] > 0.99);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn written_files_decode_to_the_same_samples() {
        let dir = std::env::temp_dir().join(format!("canopy-wav-{}", std::process::id()));
        let path = dir.join("nested").join("out.wav");
        let left: Vec<f32> = (0..1000).map(|i| (i as f32 / 500.0) - 1.0).collect();
        let right: Vec<f32> = left.iter().map(|s| -s * 1.5).collect();
        let audio = AudioBuffer::new(48_000, left, right);
        write_wav(&path, &audio).unwrap();
        let (decoded, channels) = crate::decode(&path).unwrap();
        assert_eq!(channels, 2);
        assert_eq!(decoded, audio, "float WAV is lossless, even above 1.0");
        let _ = std::fs::remove_dir_all(dir);
    }
}
