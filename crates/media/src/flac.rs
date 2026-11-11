//! Encoding integer WAV files as FLAC, for collected projects: FLAC is
//! lossless and about half the size, and canopy decodes it like any WAV.
//!
//! Only 16 and 24-bit integer PCM is taken. FLAC has no floating point, so
//! a float WAV (what canopy freezes and consolidates to) would lose what
//! lies above full scale; those, and anything that isn't a WAV, are left
//! for the caller to copy as they are. The samples go from the file's bytes
//! straight to the encoder a block at a time, never decoded to floats, so
//! the FLAC holds exactly the WAV's numbers, at its rate and length.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use flacenc::component::BitRepr;
use flacenc::error::{SourceError, Verify};
use flacenc::source::{Fill, Source};

/// The most channels a FLAC stream holds.
const MAX_CHANNELS: u16 = 8;

/// The highest rate a FLAC stream's header can state.
const MAX_RATE: u32 = 655_350;

/// `WAVE_FORMAT_PCM`, as a format tag or the start of an extensible
/// format's sub-format GUID.
const PCM: u16 = 1;

/// `WAVE_FORMAT_EXTENSIBLE`: the real format is in the sub-format GUID.
const EXTENSIBLE: u16 = 0xFFFE;

/// The integer WAV at `src` encoded as a FLAC file's bytes. `None` if
/// `src` isn't a 16 or 24-bit integer WAV FLAC can hold, or if the FLAC
/// would be no smaller (noise doesn't compress, and a FLAC of it can come
/// out larger than the WAV). `keep_going` is asked between blocks, and an
/// error is returned once it says no.
pub fn wav_to_flac(src: &Path, keep_going: &dyn Fn() -> bool) -> io::Result<Option<Vec<u8>>> {
    let mut file = BufReader::new(File::open(src)?);
    let Some(layout) = read_layout(&mut file)? else {
        return Ok(None);
    };
    file.seek(SeekFrom::Start(layout.data_start))?;
    let source = WavSource {
        file,
        layout,
        remaining: layout.data_frames * layout.frame_bytes(),
        buffer: Vec::new(),
        keep_going,
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, err)| io::Error::other(format!("FLAC settings: {err}")))?;
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|err| io::Error::other(format!("FLAC encoding: {err}")))?;
    // The encoder states the short last block as the smallest, which the
    // format leaves out of that count; decoders (symphonia among them)
    // then take the stream for one of variable block sizes and fail.
    stream
        .stream_info_mut()
        .set_block_sizes(config.block_size, config.block_size)
        .map_err(|err| io::Error::other(format!("FLAC encoding: {err}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|err| io::Error::other(format!("FLAC encoding: {err}")))?;
    let bytes = sink.into_inner();
    let smaller = (bytes.len() as u64) < layout.data_frames * layout.frame_bytes();
    Ok(smaller.then_some(bytes))
}

/// Where a WAV's samples are and how they're laid out.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    channels: u16,
    rate: u32,
    /// Bytes per sample: 2 or 3.
    bytes: u16,
    data_start: u64,
    data_frames: u64,
}

impl Layout {
    fn frame_bytes(&self) -> u64 {
        u64::from(self.channels) * u64::from(self.bytes)
    }
}

/// The layout of a WAV FLAC can hold, `None` for any other file.
fn read_layout(file: &mut (impl Read + Seek)) -> io::Result<Option<Layout>> {
    let length = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 12];
    if file.read_exact(&mut header).is_err() || &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Ok(None);
    }
    let mut format = None;
    let mut position = 12u64;
    while position + 8 <= length {
        let mut chunk = [0; 8];
        file.read_exact(&mut chunk)?;
        let size = u64::from(u32::from_le_bytes(chunk[4..].try_into().unwrap()));
        let body = position + 8;
        match &chunk[..4] {
            b"fmt " => {
                let mut fmt = vec![0; size.min(64) as usize];
                file.read_exact(&mut fmt)?;
                format = parse_format(&fmt);
                if format.is_none() {
                    return Ok(None);
                }
            }
            b"data" => {
                let Some((channels, rate, bytes)) = format else {
                    return Ok(None);
                };
                // Files written while recording can leave the size at 0 or
                // at its maximum; what's in the file is what there is.
                let available = length - body;
                let size = if size == 0 || size > available {
                    available
                } else {
                    size
                };
                let frame_bytes = u64::from(channels) * u64::from(bytes);
                let data_frames = size / frame_bytes;
                if data_frames == 0 {
                    return Ok(None);
                }
                return Ok(Some(Layout {
                    channels,
                    rate,
                    bytes,
                    data_start: body,
                    data_frames,
                }));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        position = body + size + (size & 1);
        file.seek(SeekFrom::Start(position))?;
    }
    Ok(None)
}

/// Channels, rate and bytes per sample from a `fmt ` chunk, if FLAC can
/// hold the samples it describes.
fn parse_format(fmt: &[u8]) -> Option<(u16, u32, u16)> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(fmt.get(at..at + 2)?.try_into().ok()?));
    let tag = u16_at(0)?;
    let channels = u16_at(2)?;
    let rate = u32::from_le_bytes(fmt.get(4..8)?.try_into().ok()?);
    let block_align = u16_at(12)?;
    let bits = u16_at(14)?;
    let integer = match tag {
        PCM => true,
        EXTENSIBLE => u16_at(24)? == PCM,
        _ => false,
    };
    // Extensible files may have fewer valid bits than their container (20
    // in 24, say); the spare bits are zeros and FLAC keeps them as they are.
    let bytes = bits / 8;
    let fits = integer
        && matches!(bits, 16 | 24)
        && (1..=MAX_CHANNELS).contains(&channels)
        && (1..=MAX_RATE).contains(&rate)
        && block_align == channels * bytes;
    fits.then_some((channels, rate, bytes))
}

/// The WAV's samples, a block at a time, as the encoder asks for them.
struct WavSource<'a, R> {
    file: R,
    layout: Layout,
    /// Bytes of samples not read yet.
    remaining: u64,
    buffer: Vec<u8>,
    keep_going: &'a dyn Fn() -> bool,
}

impl<R: Read> Source for WavSource<'_, R> {
    fn channels(&self) -> usize {
        usize::from(self.layout.channels)
    }

    fn bits_per_sample(&self) -> usize {
        usize::from(self.layout.bytes) * 8
    }

    fn sample_rate(&self) -> usize {
        self.layout.rate as usize
    }

    fn read_samples<F: Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> Result<usize, SourceError> {
        if !(self.keep_going)() {
            return Err(SourceError::from_io_error(io::Error::new(
                io::ErrorKind::Interrupted,
                "cancelled",
            )));
        }
        let frame_bytes = self.layout.frame_bytes();
        let bytes = (block_size as u64 * frame_bytes).min(self.remaining);
        if bytes == 0 {
            return Ok(0);
        }
        self.buffer.resize(bytes as usize, 0);
        self.file
            .read_exact(&mut self.buffer)
            .map_err(SourceError::from_io_error)?;
        self.remaining -= bytes;
        dest.fill_le_bytes(&self.buffer, usize::from(self.layout.bytes))?;
        Ok((bytes / frame_bytes) as usize)
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.layout.data_frames as usize)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::test_util::TempDir;

    /// A WAV of integer samples, `bytes` per sample, with a chunk before
    /// `fmt ` and one between it and `data`, as real files have.
    fn wav(channels: u16, rate: u32, bytes: u16, samples: &[i32]) -> Vec<u8> {
        let mut data = Vec::new();
        for &s in samples {
            data.extend_from_slice(&s.to_le_bytes()[..usize::from(bytes)]);
        }
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&PCM.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        let align = channels * bytes;
        fmt.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        fmt.extend_from_slice(&align.to_le_bytes());
        fmt.extend_from_slice(&(bytes * 8).to_le_bytes());
        let mut body = b"WAVE".to_vec();
        for (id, chunk) in [
            (b"JUNK", &b"odd"[..]),
            (b"fmt ", &fmt[..]),
            (b"LIST", &b"INFOxxxx"[..]),
            (b"data", &data[..]),
        ] {
            body.extend_from_slice(id);
            body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            body.extend_from_slice(chunk);
            if chunk.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    /// Something like music: a few sines and a little noise, full range
    /// for `bits`.
    fn signal(frames: usize, channels: usize, bits: u32) -> Vec<i32> {
        noisy(frames, channels, bits, 0.002)
    }

    fn noisy(frames: usize, channels: usize, bits: u32, hiss: f64) -> Vec<i32> {
        let peak = ((1i64 << (bits - 1)) - 1) as f64;
        let mut noise = 0x1234_5678u32;
        (0..frames * channels)
            .map(|i| {
                noise = next_noise(noise);
                let t = (i / channels) as f64;
                let tone = (t * 0.031).sin() * 0.6 + (t * 0.0071 + i as f64 % 2.0).sin() * 0.3;
                let hiss = (noise as f64 / u32::MAX as f64 - 0.5) * hiss;
                ((tone + hiss) * peak).round().clamp(-peak - 1.0, peak) as i32
            })
            .collect()
    }

    fn next_noise(mut x: u32) -> u32 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^ (x << 5)
    }

    #[test]
    fn integer_wavs_become_flacs_that_decode_to_the_same_samples() {
        let dir = TempDir::new("flac");
        for (bytes, channels, rate) in [(2u16, 2u16, 44_100u32), (3, 2, 48_000), (2, 1, 22_050)] {
            // Not a whole number of encoder blocks, so the last one is short.
            let frames = 10_000;
            let samples = signal(frames, usize::from(channels), u32::from(bytes) * 8);
            let wav_path = dir.join(&format!("{bytes}-{channels}.wav"));
            let flac_path = dir.join(&format!("{bytes}-{channels}.flac"));
            std::fs::write(&wav_path, wav(channels, rate, bytes, &samples)).unwrap();

            let flac = wav_to_flac(&wav_path, &|| true).unwrap().unwrap();
            std::fs::write(&flac_path, flac).unwrap();

            let (from_wav, _) = crate::decode(&wav_path).unwrap();
            let (from_flac, file_channels) = crate::decode(&flac_path).unwrap();
            assert_eq!(file_channels, channels);
            assert_eq!(from_flac.sample_rate(), rate);
            assert_eq!(from_flac.frames(), frames);
            assert_eq!(from_flac, from_wav, "{bytes} bytes, {channels} channels");
            let wav_size = std::fs::metadata(&wav_path).unwrap().len();
            let flac_size = std::fs::metadata(&flac_path).unwrap().len();
            assert!(flac_size < wav_size, "{flac_size} vs {wav_size}");
        }
    }

    #[test]
    fn float_and_non_wav_files_are_left_alone() {
        let dir = TempDir::new("flac-skip");
        let float = dir.join("float.wav");
        crate::test_util::write_wav(
            &float,
            48_000,
            2,
            crate::test_util::WavFormat::Float32,
            &[0.0, 0.5, -0.5, 1.5],
        );
        let text = dir.join("notes.wav");
        std::fs::write(&text, "not audio").unwrap();
        for path in [float, text] {
            assert!(wav_to_flac(&path, &|| true).unwrap().is_none());
        }
    }

    #[test]
    fn noise_stays_a_wav() {
        let dir = TempDir::new("flac-noise");
        let path = dir.join("noise.wav");
        std::fs::write(&path, wav(2, 48_000, 3, &noisy(10_000, 2, 24, 2.0))).unwrap();
        assert!(wav_to_flac(&path, &|| true).unwrap().is_none());
    }

    #[test]
    fn a_data_size_past_the_end_reads_what_is_there() {
        let mut bytes = wav(2, 44_100, 2, &[1, 2, 3, 4, 5, 6]);
        let at = bytes.len() - 12 - 4;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let layout = read_layout(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(layout.data_frames, 3);
    }

    #[test]
    fn cancelling_stops_the_encoder() {
        let dir = TempDir::new("flac-cancel");
        let path = dir.join("long.wav");
        std::fs::write(&path, wav(1, 8_000, 2, &signal(20_000, 1, 16))).unwrap();
        assert!(wav_to_flac(&path, &|| false).is_err());
    }
}
