//! Runs the engine on a real audio device.
//!
//! The audio system depends on the platform. On Linux canopy is its own
//! PipeWire node (`crate::pipewire`), or, without PipeWire (or with
//! `CANOPY_AUDIO=alsa`), an ALSA client through cpal. On Windows, WASAPI
//! through cpal. cpal opens WASAPI in shared
//! mode only, so Windows mixes canopy with other programs and converts its
//! sample rate to the mixer's if they differ. With the `asio` feature, an
//! installed ASIO driver is used instead, for lower latency.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, FromSample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize,
    SupportedStreamConfig,
};

use crate::EngineHandle;

/// Buffer size to ask the device for unless told otherwise. 256 frames is
/// about 5 ms at 48 kHz.
pub const DEFAULT_BUFFER_FRAMES: u32 = 256;

/// The buffer sizes worth offering.
pub const BUFFER_SIZES: [u32; 5] = [64, 128, 256, 512, 1024];

/// How to open the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    pub sample_rate: u32,
    /// Frames per buffer to ask for; the device may run at another size.
    pub buffer_frames: u32,
    /// The output to play through, by the name the audio system knows it
    /// by (see [`outputs`]), or `None` for the system's default.
    pub output: Option<String>,
}

impl AudioSettings {
    pub fn new(sample_rate: u32) -> AudioSettings {
        AudioSettings {
            sample_rate,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            output: None,
        }
    }
}

/// What the stream's callback last saw, written there and read anywhere.
#[derive(Debug, Default)]
pub(crate) struct Readings {
    frames: AtomicU32,
    latency_us: AtomicU32,
    graph_rate: AtomicU32,
    callbacks: AtomicU64,
}

impl Readings {
    /// Called from the audio callback: atomics only.
    pub(crate) fn record(&self, frames: usize, latency: Duration, graph_rate: u32) {
        self.frames.store(frames as u32, Ordering::Relaxed);
        let latency = latency.as_micros().min(u128::from(u32::MAX)) as u32;
        self.latency_us.store(latency, Ordering::Relaxed);
        self.graph_rate.store(graph_rate, Ordering::Relaxed);
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn read(&self) -> Reading {
        Reading {
            frames: self.frames.load(Ordering::Relaxed),
            latency: Duration::from_micros(u64::from(self.latency_us.load(Ordering::Relaxed))),
            graph_rate: Some(self.graph_rate.load(Ordering::Relaxed)).filter(|r| *r > 0),
            callbacks: self.callbacks.load(Ordering::Relaxed),
        }
    }
}

/// How a running stream is doing, as its callback last saw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Frames the last callback asked for.
    pub frames: u32,
    /// How long until what the last callback wrote is heard, as the audio
    /// system predicts it (zero if it doesn't say).
    pub latency: Duration,
    /// The rate the audio system's graph runs at, where it has one
    /// (PipeWire); it converts if that isn't the stream's.
    pub graph_rate: Option<u32>,
    /// Callbacks so far.
    pub callbacks: u64,
}

/// Sample formats the engine can play, best first: its own 32-bit float, then
/// integer formats it converts to. ASIO drivers mostly take 32-bit integers.
const FORMATS: [SampleFormat; 4] = [
    SampleFormat::F32,
    SampleFormat::I32,
    SampleFormat::I24,
    SampleFormat::I16,
];

/// How many frames the engine renders at a time for a device that doesn't
/// take floats, before they're converted.
const CONVERT_FRAMES: usize = 1024;

/// A running output stream. Audio stops when this is dropped.
pub struct AudioOutput {
    _stream: Stream,
    pub host_name: &'static str,
    pub device_name: String,
    pub sample_rate: u32,
    /// The size asked for; `None` when the device picks its own.
    pub buffer_frames: Option<u32>,
    readings: Arc<Readings>,
}

impl AudioOutput {
    /// How the stream is doing now.
    pub fn reading(&self) -> Reading {
        self.readings.read()
    }

    /// The outputs it could be pinned to ([`AudioSettings::output`]). Only
    /// PipeWire lists them; elsewhere this is empty.
    pub fn outputs(&self) -> Vec<OutputDevice> {
        match &self._stream {
            #[cfg(target_os = "linux")]
            Stream::PipeWire(connection) => connection.outputs(),
            Stream::Cpal(_) => Vec::new(),
        }
    }

    /// Why the sound stopped, if the audio system said.
    pub fn problem(&self) -> Option<String> {
        match &self._stream {
            #[cfg(target_os = "linux")]
            Stream::PipeWire(connection) => connection.problem(),
            Stream::Cpal(_) => None,
        }
    }
}

/// What keeps the sound going.
enum Stream {
    Cpal(#[allow(dead_code)] cpal::Stream),
    #[cfg(target_os = "linux")]
    PipeWire(crate::pipewire::Connection),
}

/// An output the audio system offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    /// What [`AudioSettings::output`] names it by.
    pub name: String,
    /// What to show.
    pub description: String,
}

/// The variable that picks the audio system on Linux: `alsa` skips PipeWire.
pub const BACKEND_VAR: &str = "CANOPY_AUDIO";

#[derive(Debug)]
pub enum DeviceError {
    NoOutputDevice,
    NoSupportedFormat,
    Cpal(cpal::Error),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputDevice => write!(f, "no audio output device found"),
            Self::NoSupportedFormat => {
                write!(f, "output device doesn't take float or integer PCM audio")
            }
            Self::Cpal(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<cpal::Error> for DeviceError {
    fn from(err: cpal::Error) -> Self {
        Self::Cpal(err)
    }
}

/// Open the default output device at `sample_rate` and start an engine on it.
///
/// Built with the `asio` feature, Windows uses an ASIO driver if one is
/// installed and starts (see the `asio` module), and WASAPI otherwise.
///
/// If the device can't run at `sample_rate` it opens at its own default rate
/// instead; check [`AudioOutput::sample_rate`] for the rate actually in use.
pub fn start(settings: &AudioSettings) -> Result<(EngineHandle, AudioOutput), DeviceError> {
    #[cfg(target_os = "linux")]
    if std::env::var(BACKEND_VAR).is_ok_and(|v| v.eq_ignore_ascii_case("alsa")) {
        // Asked for ALSA.
    } else {
        match start_pipewire(settings) {
            Ok(started) => return Ok(started),
            Err(err) => eprintln!("PipeWire unavailable, using ALSA: {err}"),
        }
    }
    #[cfg(all(windows, feature = "asio"))]
    match asio::start(settings) {
        Ok(started) => return Ok(started),
        // No driver installed, or another program has it: WASAPI still works.
        Err(err) => eprintln!("ASIO unavailable, using WASAPI: {err}"),
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(DeviceError::NoOutputDevice)?;
    start_on(&host, &device, settings)
}

/// Starts an engine on a PipeWire stream of its own.
#[cfg(target_os = "linux")]
fn start_pipewire(settings: &AudioSettings) -> Result<(EngineHandle, AudioOutput), String> {
    let readings = Arc::new(Readings::default());
    let (handle, connection) = crate::pipewire::start(settings, Arc::clone(&readings))?;
    let device_name = match &settings.output {
        None => "default output".to_owned(),
        Some(name) => connection
            .outputs()
            .into_iter()
            .find(|o| &o.name == name)
            .map_or_else(|| name.clone(), |o| o.description),
    };
    let output = AudioOutput {
        _stream: Stream::PipeWire(connection),
        host_name: "PipeWire",
        device_name,
        sample_rate: settings.sample_rate,
        buffer_frames: Some(settings.buffer_frames),
        readings,
    };
    Ok((handle, output))
}

/// Starts an engine on `device`, one of `host`'s.
fn start_on(
    host: &cpal::Host,
    device: &cpal::Device,
    settings: &AudioSettings,
) -> Result<(EngineHandle, AudioOutput), DeviceError> {
    let supported = pick_config(device, settings.sample_rate)?;

    let fixed = match supported.buffer_size() {
        SupportedBufferSize::Range { min, max } => Some(BufferSize::Fixed(
            settings.buffer_frames.max(*min).min(*max),
        )),
        SupportedBufferSize::Unknown => None,
    };
    // Some backends advertise a range and then reject a fixed size anyway,
    // so fall back to letting the device choose.
    let mut last_err = None;
    for buffer_size in fixed.into_iter().chain([BufferSize::Default]) {
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size,
        };
        let readings = Arc::new(Readings::default());
        match open_stream(device, config, supported.sample_format(), &readings) {
            Ok((handle, stream)) => {
                let output = AudioOutput {
                    _stream: Stream::Cpal(stream),
                    readings,
                    host_name: host.id().name(),
                    device_name: device
                        .description()
                        .map(|d| d.name().to_owned())
                        .unwrap_or_else(|_| "unknown device".to_owned()),
                    sample_rate: config.sample_rate,
                    buffer_frames: match buffer_size {
                        BufferSize::Fixed(frames) => Some(frames),
                        BufferSize::Default => None,
                    },
                };
                return Ok((handle, output));
            }
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.expect("tried at least one config").into())
}

/// Starts a stream the engine renders into, in `format`.
fn open_stream(
    device: &cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    readings: &Arc<Readings>,
) -> Result<(EngineHandle, cpal::Stream), cpal::Error> {
    let readings = Arc::clone(readings);
    match format {
        SampleFormat::F32 => open_float_stream(device, config, readings),
        SampleFormat::I32 => open_converting_stream::<i32>(device, config, readings),
        SampleFormat::I24 => open_converting_stream::<cpal::I24>(device, config, readings),
        SampleFormat::I16 => open_converting_stream::<i16>(device, config, readings),
        other => Err(cpal::Error::with_message(
            cpal::ErrorKind::UnsupportedConfig,
            format!("can't play {other} samples"),
        )),
    }
}

/// The engine renders straight into the device's buffer.
fn open_float_stream(
    device: &cpal::Device,
    config: StreamConfig,
    readings: Arc<Readings>,
) -> Result<(EngineHandle, cpal::Stream), cpal::Error> {
    let channels = usize::from(config.channels);
    let (handle, mut engine) = crate::create(config.sample_rate as f32);

    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
            record(&readings, out.len() / channels.max(1), info);
            // In debug builds this aborts if the engine allocates, catching
            // real-time violations as soon as they're written. No-op in release.
            assert_no_alloc::assert_no_alloc(|| engine.process_interleaved(out, channels));
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;
    Ok((handle, stream))
}

/// The engine renders floats into a buffer of its own, a block at a time,
/// and they're converted to the device's samples.
fn open_converting_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    readings: Arc<Readings>,
) -> Result<(EngineHandle, cpal::Stream), cpal::Error>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let channels = usize::from(config.channels).max(1);
    let (handle, mut engine) = crate::create(config.sample_rate as f32);
    // Allocated here, since the callback mustn't.
    let mut rendered = vec![0.0f32; CONVERT_FRAMES * channels];

    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            record(&readings, out.len() / channels, info);
            assert_no_alloc::assert_no_alloc(|| {
                for chunk in out.chunks_mut(rendered.len()) {
                    let rendered = &mut rendered[..chunk.len()];
                    engine.process_interleaved(rendered, channels);
                    for (sample, &value) in chunk.iter_mut().zip(rendered.iter()) {
                        *sample = T::from_sample(value);
                    }
                }
            });
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;
    Ok((handle, stream))
}

/// Notes a callback's size and the delay cpal predicts before it's heard.
fn record(readings: &Readings, frames: usize, info: &cpal::OutputCallbackInfo) {
    let time = info.timestamp();
    readings.record(frames, time.playback.duration_since(time.callback), 0);
}

/// The engine works in f32. Ask for an f32 config at `sample_rate`, and if the
/// device doesn't offer one, settle for an f32 config at its default rate.
/// Failing both, the same for each integer format in [`FORMATS`].
///
/// Don't just take `default_output_config()`: cpal's default prefers 48 kHz
/// whenever the device supports it, and PipeWire reports its graph rate, so the
/// device would never be asked for anything else.
fn pick_config(
    device: &cpal::Device,
    sample_rate: u32,
) -> Result<SupportedStreamConfig, DeviceError> {
    let default = device.default_output_config()?;
    let config_at = |format, rate| -> Result<Option<SupportedStreamConfig>, DeviceError> {
        let mut configs: Vec<_> = device
            .supported_output_configs()?
            .filter(|c| c.sample_format() == format)
            .collect();
        // Keep the device's usual channel layout if it's available at this rate.
        configs.sort_by_key(|c| c.channels() != default.channels());
        Ok(configs
            .into_iter()
            .find_map(|c| c.try_with_sample_rate(rate)))
    };

    for format in FORMATS {
        if let Some(config) = config_at(format, sample_rate)? {
            return Ok(config);
        }
        if default.sample_format() == format {
            return Ok(default);
        }
        if let Some(config) = config_at(format, default.sample_rate())? {
            return Ok(config);
        }
    }
    Err(DeviceError::NoSupportedFormat)
}

/// ASIO drivers, which talk to the sound card directly: lower latency than
/// WASAPI's shared mode, at the card's own buffer sizes and rates. Most
/// cards come with one; ASIO4ALL and FlexASIO wrap the others.
///
/// ASIO has no default device, and a computer often has several drivers
/// (FL Studio installs its own), so `CANOPY_ASIO_DRIVER` picks one by name,
/// or part of it in any case ("focusrite"). Without it, the first one found.
#[cfg(all(windows, feature = "asio"))]
mod asio {
    use cpal::traits::{DeviceTrait, HostTrait};

    use super::{AudioOutput, AudioSettings, DeviceError, start_on};
    use crate::EngineHandle;

    /// The variable that names the driver to use.
    const DRIVER_VAR: &str = "CANOPY_ASIO_DRIVER";

    /// Starts on an ASIO driver, or says why not.
    pub(super) fn start(
        settings: &AudioSettings,
    ) -> Result<(EngineHandle, AudioOutput), DeviceError> {
        let host = cpal::host_from_id(cpal::HostId::Asio)?;
        let wanted = std::env::var(DRIVER_VAR)
            .ok()
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty());
        let device = host
            .output_devices()?
            .find(|device| {
                wanted.as_deref().is_none_or(|wanted| {
                    device
                        .description()
                        .is_ok_and(|d| d.name().to_lowercase().contains(wanted))
                })
            })
            .ok_or(DeviceError::NoOutputDevice)?;
        start_on(&host, &device, settings)
    }
}
