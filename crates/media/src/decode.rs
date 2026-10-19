use std::fs::File;
use std::io;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::AudioBuffer;
use crate::error::{MediaError, from_symphonia};

/// Extensions of the containers enabled in our symphonia features. WebM and
/// Matroska usually carry Opus, which symphonia can't decode, but they're
/// still worth offering for the Vorbis, FLAC and PCM ones.
const EXTENSIONS: &[&str] = &[
    "wav", "wave", "flac", "mp3", "ogg", "oga", "aif", "aiff", "aifc", "mka", "mkv", "webm",
];

/// Don't trust a header's frame count with more than this much up-front
/// allocation (about 23 minutes at 48 kHz); beyond it the buffers just grow.
const MAX_RESERVE_FRAMES: u64 = 1 << 26;

/// Whether the file's extension is one we can probably decode. Only a hint
/// for file pickers; `decode` sniffs the contents and ignores the name.
pub fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

/// Decode a whole file at its own sample rate. Mono goes to both channels and
/// anything past the first two channels is dropped. Also returns the file's
/// channel count, for display.
///
/// Corrupt packets are skipped and a file that ends early keeps what decoded,
/// so a slightly damaged file still loads. Chained Ogg streams stop at the end
/// of the first one.
pub fn decode(path: &Path) -> Result<(AudioBuffer, u16), MediaError> {
    let file = File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(from_symphonia)?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or(MediaError::NoAudioTrack)?;
    let track_id = track.id;
    let expected_frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or(MediaError::NoAudioTrack)?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(from_symphonia)?;

    let mut left = Vec::new();
    let mut right = Vec::new();
    if let Some(frames) = expected_frames {
        let frames = frames.min(MAX_RESERVE_FRAMES) as usize;
        // A failed reservation is fine; the vectors grow as needed instead.
        let _ = left.try_reserve_exact(frames);
        let _ = right.try_reserve_exact(frames);
    }

    // Rate and channel count come from the first decoded buffer: the
    // container's codec parameters are allowed to leave them out.
    let mut spec: Option<(u32, u16)> = None;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut last_error = None;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(from_symphonia(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(audio) => audio,
            Err(e @ (SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_))) => {
                last_error = Some(e);
                continue;
            }
            Err(e) => return Err(from_symphonia(e)),
        };
        if audio.frames() == 0 || audio.num_planes() == 0 {
            continue;
        }
        if spec.is_none() {
            let rate = audio.spec().rate();
            if rate == 0 {
                return Err(MediaError::Decode("sample rate is zero".into()));
            }
            let channels = u16::try_from(audio.spec().channels().count()).unwrap_or(u16::MAX);
            spec = Some((rate, channels));
        }

        audio.copy_to_vecs_planar::<f32>(&mut planes);
        let l = &planes[0];
        let r = planes.get(1).unwrap_or(l);
        // Growing a huge Vec can fail; report that instead of aborting the process.
        if left.try_reserve(l.len()).is_err() || right.try_reserve(r.len()).is_err() {
            return Err(MediaError::Decode("not enough memory for this file".into()));
        }
        left.extend_from_slice(l);
        right.extend_from_slice(r);
    }

    match spec {
        Some((rate, channels)) => Ok((AudioBuffer::new(rate, left, right), channels)),
        // Every packet failed: say why rather than calling the file empty.
        None => Err(last_error.map_or(MediaError::Empty, from_symphonia)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{TempDir, WavFormat, write_wav};

    fn i16_value(v: i16) -> f32 {
        v as f32 / 32768.0
    }

    #[test]
    fn extensions() {
        assert!(is_supported_extension(Path::new("a/b/loop.wav")));
        assert!(is_supported_extension(Path::new("Take 1.FLAC")));
        assert!(is_supported_extension(Path::new("x.aif")));
        assert!(!is_supported_extension(Path::new("notes.txt")));
        assert!(!is_supported_extension(Path::new("wav")));
        assert!(!is_supported_extension(Path::new("")));
    }

    #[test]
    fn mono_pcm16_goes_to_both_sides() {
        let dir = TempDir::new("mono16");
        let path = dir.join("mono.wav");
        let values = [0, 16384, -16384, 32767, -32768, 1];
        let samples: Vec<f32> = values.iter().map(|&v| i16_value(v)).collect();
        write_wav(&path, 22050, 1, WavFormat::Pcm16, &samples);

        let (buffer, channels) = decode(&path).unwrap();
        assert_eq!(channels, 1);
        assert_eq!(buffer.sample_rate(), 22050);
        assert_eq!(buffer.frames(), values.len());
        assert_eq!(buffer.left(), &samples[..]);
        assert_eq!(buffer.right(), &samples[..]);
    }

    #[test]
    fn stereo_pcm16_keeps_channels_apart() {
        let dir = TempDir::new("stereo16");
        let path = dir.join("stereo.wav");
        let left = [i16_value(100), i16_value(-200), i16_value(300)];
        let right = [i16_value(-1), i16_value(2), i16_value(-3)];
        let interleaved: Vec<f32> = left
            .iter()
            .zip(&right)
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        write_wav(&path, 44100, 2, WavFormat::Pcm16, &interleaved);

        let (buffer, channels) = decode(&path).unwrap();
        assert_eq!(channels, 2);
        assert_eq!(buffer.sample_rate(), 44100);
        assert_eq!(buffer.left(), &left[..]);
        assert_eq!(buffer.right(), &right[..]);
    }

    #[test]
    fn float32_is_exact() {
        let dir = TempDir::new("float");
        let mono = dir.join("mono.wav");
        let samples = [0.0, 0.25, -0.75, 1.0, -1.0, 0.123_456_7];
        write_wav(&mono, 48000, 1, WavFormat::Float32, &samples);
        let (buffer, channels) = decode(&mono).unwrap();
        assert_eq!((channels, buffer.sample_rate()), (1, 48000));
        assert_eq!(buffer.left(), &samples[..]);
        assert_eq!(buffer.right(), &samples[..]);

        let stereo = dir.join("stereo.wav");
        write_wav(
            &stereo,
            96000,
            2,
            WavFormat::Float32,
            &[0.5, -0.5, 0.1, -0.1],
        );
        let (buffer, channels) = decode(&stereo).unwrap();
        assert_eq!((channels, buffer.sample_rate()), (2, 96000));
        assert_eq!(buffer.left(), &[0.5, 0.1]);
        assert_eq!(buffer.right(), &[-0.5, -0.1]);
    }

    #[test]
    fn extra_channels_are_dropped() {
        let dir = TempDir::new("quad");
        let path = dir.join("quad.wav");
        let frames: Vec<f32> = (0..3)
            .flat_map(|i| [0.1, 0.2, 0.3, 0.4].map(|c| c * (i + 1) as f32))
            .collect();
        write_wav(&path, 44100, 4, WavFormat::Float32, &frames);

        let (buffer, channels) = decode(&path).unwrap();
        assert_eq!(channels, 4);
        assert_eq!(buffer.left(), &[0.1, 0.2, 0.3]);
        assert_eq!(buffer.right(), &[0.2, 0.4, 0.6]);
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let dir = TempDir::new("missing");
        let result = decode(&dir.join("nope.wav"));
        assert!(matches!(result, Err(MediaError::Io(_))), "{result:?}");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let dir = TempDir::new("garbage");

        let text = dir.join("notes.wav");
        std::fs::write(
            &text,
            "this is not audio, just some text pretending to be".repeat(50),
        )
        .unwrap();
        assert!(decode(&text).is_err());

        let empty = dir.join("empty.wav");
        std::fs::write(&empty, []).unwrap();
        assert!(decode(&empty).is_err());

        // A valid WAV header cut off in the middle of the fmt chunk.
        let truncated = dir.join("truncated.wav");
        let full = dir.join("full.wav");
        write_wav(&full, 44100, 2, WavFormat::Pcm16, &[0.0; 64]);
        let bytes = std::fs::read(&full).unwrap();
        std::fs::write(&truncated, &bytes[..24]).unwrap();
        assert!(decode(&truncated).is_err());

        // Pseudo-random bytes that happen to start like a RIFF file.
        let noise = dir.join("noise.wav");
        let mut rng = crate::test_util::Rng::new(7);
        let mut bytes = b"RIFF\x00\x10\x00\x00WAVE".to_vec();
        bytes.extend((0..4096).map(|_| rng.next_u64() as u8));
        std::fs::write(&noise, bytes).unwrap();
        assert!(decode(&noise).is_err());
    }

    #[test]
    fn no_frames_is_empty() {
        let dir = TempDir::new("silent");
        let path = dir.join("silent.wav");
        write_wav(&path, 44100, 2, WavFormat::Pcm16, &[]);
        assert_eq!(decode(&path), Err(MediaError::Empty));
    }

    #[test]
    fn data_cut_short_keeps_what_decoded() {
        let dir = TempDir::new("cut");
        let full = dir.join("full.wav");
        let samples: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        write_wav(&full, 44100, 1, WavFormat::Float32, &samples);
        // Drop the last 100 samples but leave the header claiming all 1000.
        let bytes = std::fs::read(&full).unwrap();
        let cut = dir.join("cut.wav");
        std::fs::write(&cut, &bytes[..bytes.len() - 400]).unwrap();

        let (buffer, _) = decode(&cut).unwrap();
        assert!(buffer.frames() <= 900, "{}", buffer.frames());
        assert!(buffer.frames() > 0);
        assert_eq!(buffer.left(), &samples[..buffer.frames()]);
    }
}
