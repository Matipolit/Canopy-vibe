//! Helpers shared by the tests: a scratch directory, a minimal WAV writer,
//! and test signals and measurements.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh directory under the system temp dir, removed again on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "canopy-media-{}-{unique}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn join(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy)]
pub enum WavFormat {
    Pcm16,
    Float32,
}

/// Writes interleaved samples as a canonical 44-byte-header WAV file.
pub fn write_wav(path: &Path, rate: u32, channels: u16, format: WavFormat, samples: &[f32]) {
    assert_eq!(samples.len() % channels as usize, 0);
    let (tag, bytes_per_sample): (u16, u16) = match format {
        WavFormat::Pcm16 => (1, 2),
        WavFormat::Float32 => (3, 4),
    };
    let data_len = (samples.len() * bytes_per_sample as usize) as u32;
    let block_align = channels * bytes_per_sample;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        match format {
            WavFormat::Pcm16 => {
                let v = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&v.to_le_bytes());
            }
            WavFormat::Float32 => out.extend_from_slice(&s.to_le_bytes()),
        }
    }
    std::fs::write(path, out).unwrap();
}

/// A small deterministic PRNG (xorshift64*), so tests don't need the rand crate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [lo, hi).
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }
}

/// A unit-amplitude sine at `freq` Hz, `frames` samples long.
pub fn sine(rate: u32, freq: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| (std::f64::consts::TAU * freq * n as f64 / rate as f64).sin() as f32)
        .collect()
}

/// Frequency estimated from rising zero crossings (linearly interpolated)
/// between `skip` samples from each end.
pub fn measure_frequency(samples: &[f32], rate: u32, skip: usize) -> f64 {
    let mut crossings = Vec::new();
    for n in skip.max(1)..samples.len() - skip {
        let (a, b) = (samples[n - 1], samples[n]);
        if a < 0.0 && b >= 0.0 {
            crossings.push(n as f64 - 1.0 + (a / (a - b)) as f64);
        }
    }
    let cycles = (crossings.len() - 1) as f64;
    cycles * rate as f64 / (crossings.last().unwrap() - crossings[0])
}

/// The largest absolute sample.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// The largest step from one sample to the next.
pub fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

/// A drum sound for onset and slicing tests, starting at full level at
/// frame 0 and decaying.
#[derive(Clone, Copy, Debug)]
pub enum Drum {
    /// A sine sweeping down from 150 to 50 Hz, with a short click.
    Kick,
    /// Noise over a 190 Hz tone.
    Snare,
    /// Short, bright noise.
    Hat,
    /// Plain noise with a 30 ms decay.
    Burst,
}

impl Drum {
    pub fn render(self, rate: u32, rng: &mut Rng) -> Vec<f32> {
        use std::f64::consts::TAU;
        let rate = rate as f64;
        let (length, decay) = match self {
            Drum::Kick => (0.4, 0.12),
            Drum::Snare => (0.3, 0.07),
            Drum::Hat => (0.08, 0.015),
            Drum::Burst => (0.2, 0.03),
        };
        let mut phase = 0.0;
        let mut previous = 0.0;
        (0..(length * rate) as usize)
            .map(|n| {
                let t = n as f64 / rate;
                let envelope = (-t / decay).exp();
                let noise = rng.range(-1.0, 1.0);
                let sample = match self {
                    Drum::Kick => {
                        phase += TAU * (50.0 + 100.0 * (-t / 0.03).exp()) / rate;
                        phase.sin() + 0.3 * noise * (-t / 0.002).exp()
                    }
                    Drum::Snare => 0.7 * noise + 0.5 * (TAU * 190.0 * t).sin(),
                    Drum::Hat => {
                        // First difference: a crude high-pass.
                        let high = noise - previous;
                        previous = noise;
                        0.7 * high
                    }
                    Drum::Burst => noise,
                };
                (sample * envelope) as f32
            })
            .collect()
    }
}

/// Mixes `hits` (start frame, drum, gain) into `frames` of silence.
pub fn drums(rate: u32, frames: usize, hits: &[(usize, Drum, f32)], seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut out = vec![0.0; frames];
    for &(start, drum, gain) in hits {
        for (o, s) in out[start.min(frames)..]
            .iter_mut()
            .zip(drum.render(rate, &mut rng))
        {
            *o += gain * s;
        }
    }
    out
}
