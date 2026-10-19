//! User wavetables: an oscillator's own table, read from a WAV file and
//! band-limited like the built-in ones.
//!
//! A wavetable file is single-cycle frames one after the other, in the
//! file's first channel (16- or 24-bit PCM or 32-bit float, whatever the
//! host's decoder reads). Frames are [`DEFAULT_FRAME_SIZE`] samples long
//! unless the file has Serum's `clm ` chunk (`<!>2048 …`), which gives the
//! size. A file shorter than one frame is one single cycle, whatever its
//! length. Up to [`MAX_USER_FRAMES`](super::MAX_USER_FRAMES) frames are
//! kept: a full 256-frame table takes about 8 MB once band-limited, its
//! harmonics (for the spectral morphs) included (a 64-frame built-in,
//! 2 MB).
//!
//! Loading reads and decodes the file and takes every frame apart by FFT,
//! so it runs on a worker thread, never the audio thread: the host builds
//! [`UserTables`] there and hands them to the synth with
//! [`crate::Instrument::set_data`].

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use super::OSC_COUNT;
use super::osc;
use super::tables::Table;

/// Samples per frame in a wavetable file that doesn't say otherwise: what
/// Serum and Vital write.
pub const DEFAULT_FRAME_SIZE: usize = 2048;
/// Frame sizes a `clm ` chunk may give; anything else is ignored.
const FRAME_SIZES: std::ops::RangeInclusive<usize> = 8..=65_536;

/// An oscillator's own wavetable, made from sampled single-cycle frames.
pub struct UserTable {
    table: Table,
}

impl UserTable {
    /// A table from `samples` (one channel), cut into frames of
    /// `frame_size` samples (a trailing partial frame is dropped); fewer
    /// samples than one frame make one single-cycle frame of them all.
    /// More than [`MAX_USER_FRAMES`](super::MAX_USER_FRAMES) frames are
    /// thinned out evenly. Each
    /// frame keeps its harmonics up to the 512th without DC, and the
    /// loudest frame peaks at 1 like the built-in tables' frames. Fails
    /// for no samples or silence. Takes a while for big tables: build it
    /// off the audio thread.
    pub fn from_samples(samples: &[f32], frame_size: usize) -> Result<UserTable, String> {
        if samples.is_empty() {
            return Err("the file has no audio".to_owned());
        }
        let frame_size = if samples.len() < frame_size.max(1) {
            samples.len()
        } else {
            frame_size.max(1)
        };
        Table::from_cycles(samples, frame_size)
            .map(|table| UserTable { table })
            .ok_or_else(|| "the file is silent".to_owned())
    }

    /// Frames in the table, which the position knob moves through.
    pub fn frames(&self) -> usize {
        self.table.frames
    }

    /// The bytes its samples take, every band limit included.
    pub fn memory_bytes(&self) -> usize {
        self.table.memory_bytes()
    }

    /// Draws one cycle at `position` (`0..=1`) through both warp slots,
    /// `(mode, amount)` each, into `out` (one point per element), as
    /// [`super::frame_warped`] does for the built-in tables.
    pub fn draw(&self, position: f32, warps: [(usize, f32); 2], out: &mut [f32]) {
        let n = out.len().max(1) as f32;
        for (i, sample) in out.iter_mut().enumerate() {
            let p = i as f32 / n;
            *sample = osc::warped_sample(
                &self.table,
                position,
                warps,
                &osc::STRAIGHT_REMAP,
                p,
                osc::fast_sin(p),
            );
        }
    }

    pub(super) fn table(&self) -> &Table {
        &self.table
    }
}

impl std::fmt::Debug for UserTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserTable")
            .field("frames", &self.frames())
            .finish()
    }
}

/// What a Wavetable track loads ([`crate::DataKind::Wavetables`]): each
/// oscillator's user table, by oscillator (0 = A, 1 = B, 2 = C), `None`
/// where it has none. An oscillator plays its table when its table choice
/// is [`super::USER_TABLE`]. Hand it to [`crate::Instrument::set_data`] as
/// an `Arc<UserTables>`.
#[derive(Debug, Clone, Default)]
pub struct UserTables {
    pub tables: [Option<Arc<UserTable>>; OSC_COUNT],
}

/// The frame size in a Serum-style `clm ` chunk's text (`<!>2048 …`).
pub fn clm_frame_size(chunk: &[u8]) -> Option<usize> {
    let text = chunk.trim_ascii_start().strip_prefix(b"<!>")?;
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    let size: usize = std::str::from_utf8(&text[..digits]).ok()?.parse().ok()?;
    FRAME_SIZES.contains(&size).then_some(size)
}

/// The frame size a WAV file's `clm ` chunk gives, if it has one. Reads
/// only the chunk headers (and that chunk), skipping the audio.
pub fn wav_frame_size(path: &Path) -> io::Result<Option<usize>> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 12];
    if file.read_exact(&mut header).is_err()
        || !matches!(&header[..4], b"RIFF" | b"RF64")
        || &header[8..] != b"WAVE"
    {
        return Ok(None);
    }
    loop {
        let mut chunk = [0u8; 8];
        if file.read_exact(&mut chunk).is_err() {
            return Ok(None);
        }
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        if &chunk[..4] == b"clm " {
            let mut text = vec![0; size.min(256) as usize];
            return Ok(file
                .read_exact(&mut text)
                .ok()
                .and_then(|()| clm_frame_size(&text)));
        }
        // Chunks are padded to an even length.
        let skip = size + size % 2;
        if file.seek(SeekFrom::Current(skip as i64)).is_err() {
            return Ok(None);
        }
    }
}

/// Reads a wavetable file: `decode` gives its first channel's samples (the
/// host's decoder: WAV, and whatever else it reads), and a `clm ` chunk
/// the frame size. See the module docs. Slow for big tables: call it on a
/// worker thread.
pub fn load_user_table(
    path: &Path,
    decode: &dyn Fn(&Path) -> Result<Vec<f32>, String>,
) -> Result<UserTable, String> {
    let frame_size = wav_frame_size(path)
        .map_err(|e| e.to_string())?
        .unwrap_or(DEFAULT_FRAME_SIZE);
    let samples = decode(path)?;
    UserTable::from_samples(&samples, frame_size)
}

#[cfg(test)]
mod tests {
    use super::super::MAX_USER_FRAMES;
    use super::*;

    #[test]
    fn reads_the_frame_size_from_a_clm_chunk() {
        assert_eq!(
            clm_frame_size(b"<!>2048 01000000 wavetable (www.xferrecords.com)"),
            Some(2048)
        );
        assert_eq!(clm_frame_size(b"<!>256 10000000"), Some(256));
        assert_eq!(clm_frame_size(b"  <!>1024"), Some(1024));
        assert_eq!(clm_frame_size(b"2048"), None);
        assert_eq!(clm_frame_size(b"<!>"), None);
        assert_eq!(clm_frame_size(b"<!>0 "), None);
        assert_eq!(clm_frame_size(b"<!>99999999999999999999999"), None);
    }

    /// A 16-bit mono WAV of `samples`, with a `clm ` chunk if given.
    fn wav(samples: &[i16], clm: Option<&str>) -> Vec<u8> {
        let mut chunks = Vec::new();
        let mut chunk = |id: &[u8; 4], body: &[u8]| {
            chunks.extend_from_slice(id);
            chunks.extend_from_slice(&(body.len() as u32).to_le_bytes());
            chunks.extend_from_slice(body);
            if body.len() % 2 == 1 {
                chunks.push(0);
            }
        };
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt.extend_from_slice(&44_100u32.to_le_bytes());
        fmt.extend_from_slice(&88_200u32.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        chunk(b"fmt ", &fmt);
        // An odd-sized chunk before the one wanted, to check the padding.
        chunk(b"junk", b"abc");
        if let Some(text) = clm {
            chunk(b"clm ", text.as_bytes());
        }
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        chunk(b"data", &data);
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(4 + chunks.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&chunks);
        file
    }

    #[test]
    fn finds_the_clm_chunk_in_a_wav_file() {
        let dir = std::env::temp_dir().join(format!("canopy-wt-clm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let with = dir.join("serum.wav");
        std::fs::write(&with, wav(&[0; 512], Some("<!>256 10000000 wavetable"))).unwrap();
        assert_eq!(wav_frame_size(&with).unwrap(), Some(256));
        let without = dir.join("plain.wav");
        std::fs::write(&without, wav(&[0; 512], None)).unwrap();
        assert_eq!(wav_frame_size(&without).unwrap(), None);
        let text = dir.join("text.wav");
        std::fs::write(&text, b"not a wav file at all").unwrap();
        assert_eq!(wav_frame_size(&text).unwrap(), None);
        assert!(wav_frame_size(&dir.join("missing.wav")).is_err());

        // Loading uses the chunk's frame size: 512 samples are two frames.
        let decode = |_: &Path| -> Result<Vec<f32>, String> {
            Ok((0..512)
                .map(|i| (std::f32::consts::TAU * i as f32 / 256.0).sin())
                .collect())
        };
        assert_eq!(load_user_table(&with, &decode).unwrap().frames(), 2);
        // Without one, 512 samples are less than a frame: one cycle.
        assert_eq!(load_user_table(&without, &decode).unwrap().frames(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frames_are_cut_thinned_and_checked() {
        let saw = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| 2.0 * ((i as f32 / n as f32) + 0.5).fract() - 1.0)
                .collect()
        };
        let cycles: Vec<f32> = (0..3).flat_map(|_| saw(2048)).collect();
        assert_eq!(UserTable::from_samples(&cycles, 2048).unwrap().frames(), 3);
        // A partial frame at the end is dropped.
        let longer: Vec<f32> = cycles.iter().copied().chain(saw(100)).collect();
        assert_eq!(UserTable::from_samples(&longer, 2048).unwrap().frames(), 3);
        // Too many frames: thinned out to the most kept.
        let many: Vec<f32> = (0..300).flat_map(|_| saw(64)).collect();
        let thinned = UserTable::from_samples(&many, 64).unwrap();
        assert_eq!(thinned.frames(), MAX_USER_FRAMES);
        assert!(UserTable::from_samples(&[], 2048).is_err());
        assert!(UserTable::from_samples(&[0.0; 4096], 2048).is_err());
        assert!(UserTable::from_samples(&[f32::NAN; 4096], 2048).is_err());
        let mut out = [0.0; 64];
        thinned.draw(0.5, [(0, 0.0), (0, 0.0)], &mut out);
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.01));
        assert!(out.iter().any(|s| s.abs() > 0.5));
    }

    #[test]
    fn a_full_table_fits_in_memory() {
        // 256 frames of 2048 samples: what the biggest Serum tables hold.
        let samples: Vec<f32> = (0..256 * 2048)
            .map(|i| {
                let frame = (i / 2048) as f32 / 255.0;
                let p = (i % 2048) as f32 / 2048.0;
                (std::f32::consts::TAU * p).sin() * (1.0 - frame) + (2.0 * p - 1.0) * frame
            })
            .collect();
        let table = UserTable::from_samples(&samples, 2048).unwrap();
        assert_eq!(table.frames(), 256);
        let mb = table.memory_bytes() as f64 / 1_048_576.0;
        println!("a 256-frame user table takes {mb:.2} MB");
        assert!(mb < 9.0, "{mb} MB");
    }
}
