//! Loading: SFZ text and the host's decoded samples into an immutable
//! [`SfzBank`].

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::parse::{self, Warnings};
use super::region::{self, LoopMode, Region, Trigger};

/// Silent frames stored before and after each sample, so cubic
/// interpolation can read around any playing position without bounds checks.
pub(super) const PAD: usize = 4;

/// Crossfade given to a loop whose end doesn't join its start smoothly,
/// when the instrument sets no `loop_crossfade` of its own.
const CLICK_FIX_CROSSFADE_SECONDS: f32 = 0.01;

/// A sample file as the host decoded it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoded {
    pub left: Vec<f32>,
    /// As long as `left`, or empty for a mono file.
    pub right: Vec<f32>,
    /// The file's own rate; the sampler converts as it plays.
    pub sample_rate: u32,
    /// The file's own loop (a WAV `smpl` chunk), first and last frame, if
    /// the decoder reads it. Regions without `loop_start`/`loop_end` use it,
    /// and it makes `loop_continuous` their default `loop_mode`, as in
    /// sforzando.
    pub loop_points: Option<(usize, usize)>,
}

impl Decoded {
    pub fn mono(samples: Vec<f32>, sample_rate: u32) -> Decoded {
        Decoded {
            left: samples,
            right: Vec::new(),
            sample_rate,
            loop_points: None,
        }
    }

    pub fn stereo(left: Vec<f32>, right: Vec<f32>, sample_rate: u32) -> Decoded {
        Decoded {
            left,
            right,
            sample_rate,
            loop_points: None,
        }
    }
}

/// Why nothing could be loaded. Lesser problems don't fail a load; they
/// end up in [`SfzBank::warnings`].
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// The `.sfz` file itself couldn't be read.
    Read { path: PathBuf, message: String },
    /// Nothing to play: no regions, or none whose sample loaded. The
    /// warnings say why.
    NoRegions { warnings: Vec<String> },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Read { path, message } => {
                write!(f, "Couldn't read {}: {message}", path.display())
            }
            LoadError::NoRegions { warnings } => {
                write!(f, "Nothing playable in this instrument")?;
                match warnings.first() {
                    Some(first) => write!(f, " ({first})"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// One region as the UI sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionInfo {
    pub lokey: u8,
    pub hikey: u8,
    pub lovel: u8,
    pub hivel: u8,
    /// The key at which the sample plays at its own pitch (`pitch_keycenter`).
    pub root_key: u8,
    /// The `sample` opcode as written (slashes made forward).
    pub sample: String,
    pub trigger: Trigger,
    /// Round robin: plays on the `seq_position`th of every `seq_length` notes.
    pub seq_position: u32,
    pub seq_length: u32,
    /// The keyswitch that selects it (`sw_last`), if any.
    pub keyswitch: Option<u8>,
    /// Loops while it sounds (`loop_continuous` or `loop_sustain` with valid loop points).
    pub looped: bool,
    /// Its sample's length in seconds.
    pub seconds: f32,
}

/// A loaded SFZ instrument: regions and their samples, immutable once
/// built. Pass it to the sampler as `Arc<SfzBank>` through
/// [`crate::Instrument::set_data`].
pub struct SfzBank {
    name: String,
    path: PathBuf,
    pub(super) regions: Vec<BankRegion>,
    infos: Vec<RegionInfo>,
    pub(super) samples: Vec<Sample>,
    /// Per key, the regions a note-on can start, in file order.
    pub(super) attack_regions: Vec<Vec<usize>>,
    /// Per key, the regions a note-off can start.
    pub(super) release_regions: Vec<Vec<usize>>,
    /// Keys that select a keyswitched articulation.
    pub(super) keyswitches: [bool; 128],
    pub(super) default_keyswitch: Option<u8>,
    warnings: Vec<String>,
    memory: usize,
}

impl fmt::Debug for SfzBank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SfzBank")
            .field("name", &self.name)
            .field("regions", &self.regions.len())
            .field("samples", &self.samples.len())
            .field("memory", &self.memory)
            .finish_non_exhaustive()
    }
}

impl SfzBank {
    /// The `.sfz` file's name without its extension.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every playable region, in file order. The sampler's "last region"
    /// meters index into this.
    pub fn regions(&self) -> &[RegionInfo] {
        &self.infos
    }

    /// Problems found while loading (unsupported opcodes, missing samples,
    /// bad values), for the UI to list.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Bytes of decoded audio held.
    pub fn sample_memory_bytes(&self) -> usize {
        self.memory
    }

    /// Distinct sample files loaded.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }
}

/// Decoded audio, padded with [`PAD`] silent frames at both ends.
pub(super) struct Sample {
    pub left: Box<[f32]>,
    /// `None` for mono.
    pub right: Option<Box<[f32]>>,
    pub frames: usize,
    pub rate: f32,
    loop_points: Option<(usize, usize)>,
}

impl Sample {
    fn new(decoded: Decoded) -> Option<Sample> {
        let Decoded {
            left,
            right,
            sample_rate,
            loop_points,
        } = decoded;
        if left.is_empty() || sample_rate == 0 {
            return None;
        }
        // Decoders often copy a mono file to both sides; keep one.
        let right = (right.len() == left.len() && right != left).then(|| pad(&right));
        Some(Sample {
            left: pad(&left),
            right,
            frames: left.len(),
            rate: sample_rate as f32,
            loop_points,
        })
    }

    fn memory(&self) -> usize {
        let floats = self.left.len() + self.right.as_ref().map_or(0, |r| r.len());
        floats * size_of::<f32>()
    }

    /// Frame `i` of each channel (the same twice for mono).
    fn frame(&self, i: usize) -> [f32; 2] {
        let left = self.left[i + PAD];
        [left, self.right.as_ref().map_or(left, |r| r[i + PAD])]
    }
}

fn pad(samples: &[f32]) -> Box<[f32]> {
    let mut padded = Vec::with_capacity(samples.len() + 2 * PAD);
    padded.extend_from_slice(&[0.0; PAD]);
    // Garbage in a file must not reach the output as NaN.
    padded.extend(samples.iter().map(|s| if s.is_finite() { *s } else { 0.0 }));
    padded.extend_from_slice(&[0.0; PAD]);
    padded.into_boxed_slice()
}

/// A loop in frames: `start..end`, with the last `crossfade` frames before
/// `end` blended with the frames before `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LoopRange {
    pub start: usize,
    pub end: usize,
    pub crossfade: usize,
}

impl LoopRange {
    pub fn length(&self) -> usize {
        self.end - self.start
    }
}

/// A region with its sample resolved and its play range in frames.
pub(super) struct BankRegion {
    pub desc: Region,
    pub sample: usize,
    /// One past the last frame played.
    pub end: usize,
    pub loop_mode: LoopMode,
    /// Set exactly when `loop_mode` loops.
    pub loop_range: Option<LoopRange>,
}

impl BankRegion {
    fn new(
        desc: Region,
        index: usize,
        sample: &Sample,
        location: &str,
        warnings: &mut Warnings,
    ) -> BankRegion {
        let frames = sample.frames;
        let end = desc.end.map_or(frames, |e| {
            usize::try_from(e)
                .unwrap_or(usize::MAX)
                .saturating_add(1)
                .min(frames)
        });
        let mut loop_mode = desc.loop_mode.unwrap_or(if sample.loop_points.is_some() {
            LoopMode::Continuous
        } else {
            LoopMode::NoLoop
        });
        let mut loop_range = None;
        if matches!(loop_mode, LoopMode::Continuous | LoopMode::Sustain) {
            let (file_start, file_end) = sample.loop_points.unwrap_or((0, frames - 1));
            let start = desc.loop_start.map_or(file_start, |s| s as usize);
            let last = desc.loop_end.map_or(file_end, |e| e as usize);
            let loop_end = last.saturating_add(1).min(end);
            if start < loop_end {
                let crossfade = crossfade(&desc, sample, start, loop_end);
                loop_range = Some(LoopRange {
                    start,
                    end: loop_end,
                    crossfade,
                });
            } else {
                warnings.add(format!(
                    "{location}: loop {start}..{last} is outside the sample; not looped"
                ));
                loop_mode = LoopMode::NoLoop;
            }
        }
        BankRegion {
            desc,
            sample: index,
            end,
            loop_mode,
            loop_range,
        }
    }
}

/// Frames of crossfade for the loop `start..end`: the instrument's
/// `loop_crossfade`, or a short one when the loop points would click. It
/// blends in the audio before `start`, so it can't be longer than that, nor
/// than half the loop.
fn crossfade(desc: &Region, sample: &Sample, start: usize, end: usize) -> usize {
    let seconds = match desc.loop_crossfade {
        Some(seconds) => seconds,
        None if loop_clicks(sample, start, end) => CLICK_FIX_CROSSFADE_SECONDS,
        None => 0.0,
    };
    ((seconds * sample.rate).round() as usize)
        .min(start)
        .min((end - start) / 2)
}

/// Whether jumping from the loop's end back to its start would click: the
/// waveform bends more sharply across the jump (its second difference)
/// than anywhere just before it.
fn loop_clicks(sample: &Sample, start: usize, end: usize) -> bool {
    if end - start < 4 || end < 3 {
        return false;
    }
    let bend = |a: [f32; 2], b: [f32; 2], c: [f32; 2], channel: usize| {
        (a[channel] - 2.0 * b[channel] + c[channel]).abs()
    };
    let bend_at = |i: usize, channel| {
        bend(
            sample.frame(i - 2),
            sample.frame(i - 1),
            sample.frame(i),
            channel,
        )
    };
    let (before, last, first) = (
        sample.frame(end - 2),
        sample.frame(end - 1),
        sample.frame(start),
    );
    let window = end.saturating_sub(64).max(2)..end;
    (0..2).any(|channel| {
        let across = bend(before, last, first, channel);
        let usual = window
            .clone()
            .map(|i| bend_at(i, channel))
            .fold(0.0f32, f32::max);
        across > 1e-4 && across > 2.0 * usual
    })
}

/// Loads the instrument at `sfz_path`, decoding each sample it uses once
/// through `decode`. Meant for a worker thread: it reads files and can take
/// a while for big instruments.
pub fn load(
    sfz_path: &Path,
    decode: &dyn Fn(&Path) -> Result<Decoded, String>,
) -> Result<SfzBank, LoadError> {
    let text = parse::read_text(sfz_path).map_err(|error| LoadError::Read {
        path: sfz_path.to_path_buf(),
        message: error.to_string(),
    })?;
    load_str(&text, sfz_path, decode)
}

/// Like [`load`] with the SFZ text already in hand. `sfz_path` names the
/// bank and anchors relative sample and `#include` paths.
pub fn load_str(
    text: &str,
    sfz_path: &Path,
    decode: &dyn Fn(&Path) -> Result<Decoded, String>,
) -> Result<SfzBank, LoadError> {
    let mut warnings = Warnings::default();
    let raw_regions = parse::parse(text, sfz_path, &mut warnings);
    let base = sfz_path.parent().unwrap_or(Path::new(""));

    let mut samples: Vec<Sample> = Vec::new();
    let mut by_path: HashMap<PathBuf, Option<usize>> = HashMap::new();
    let mut regions = Vec::new();
    for raw in &raw_regions {
        let Some(desc) = region::build(raw, &mut warnings) else {
            continue;
        };
        if desc.sample.starts_with('*') {
            warnings.count("Regions using a built-in generator (*sine, *noise...), skipped");
            continue;
        }
        let path = resolve_sample(base, &raw.default_path, &desc.sample);
        let index = match by_path.get(&path) {
            Some(index) => *index,
            None => {
                let index = match decode(&path) {
                    Ok(decoded) => match Sample::new(decoded) {
                        Some(sample) => {
                            samples.push(sample);
                            Some(samples.len() - 1)
                        }
                        None => {
                            warnings.add(format!("Sample `{}` is empty", desc.sample));
                            None
                        }
                    },
                    Err(error) => {
                        warnings.add(format!("Missing sample `{}`: {error}", desc.sample));
                        None
                    }
                };
                by_path.insert(path, index);
                index
            }
        };
        let Some(index) = index else {
            warnings.count("Regions skipped for a missing sample");
            continue;
        };
        regions.push(BankRegion::new(
            desc,
            index,
            &samples[index],
            &raw.location,
            &mut warnings,
        ));
    }
    if regions.is_empty() {
        return Err(LoadError::NoRegions {
            warnings: warnings.finish(),
        });
    }

    let mut attack_regions = vec![Vec::new(); 128];
    let mut release_regions = vec![Vec::new(); 128];
    let mut keyswitches = [false; 128];
    let mut default_keyswitch = None;
    for (index, region) in regions.iter().enumerate() {
        let desc = &region.desc;
        let by_key = match desc.trigger {
            Trigger::Release => &mut release_regions,
            Trigger::Attack | Trigger::First | Trigger::Legato => &mut attack_regions,
        };
        for key in desc.lokey..=desc.hikey {
            by_key[key as usize].push(index);
        }
        if let (Some(low), Some(high)) = (desc.sw_lokey, desc.sw_hikey) {
            for key in low..=high.min(127) {
                keyswitches[key as usize] = true;
            }
        }
        if let Some(key) = desc.sw_last {
            keyswitches[key as usize] = true;
        }
        default_keyswitch = default_keyswitch.or(desc.sw_default);
    }

    let infos = regions
        .iter()
        .map(|region| {
            let desc = &region.desc;
            let sample = &samples[region.sample];
            RegionInfo {
                lokey: desc.lokey as u8,
                hikey: desc.hikey as u8,
                lovel: desc.lovel,
                hivel: desc.hivel,
                root_key: desc.pitch_keycenter.clamp(0, 127) as u8,
                sample: desc.sample.clone(),
                trigger: desc.trigger,
                seq_position: desc.seq_position,
                seq_length: desc.seq_length,
                keyswitch: desc.sw_last,
                looped: region.loop_range.is_some(),
                seconds: sample.frames as f32 / sample.rate,
            }
        })
        .collect();
    let memory = samples.iter().map(Sample::memory).sum();
    Ok(SfzBank {
        name: sfz_path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
        path: sfz_path.to_path_buf(),
        regions,
        infos,
        samples,
        attack_regions,
        release_regions,
        keyswitches,
        default_keyswitch,
        warnings: warnings.finish(),
        memory,
    })
}

/// Where a region's sample is: `default_path` and `sample` joined, relative
/// to the `.sfz` file. Instruments made on Windows often spell a file with
/// different letter case than it has on disk; when the exact path doesn't
/// exist, a case-insensitive match is used.
fn resolve_sample(base: &Path, default_path: &str, sample: &str) -> PathBuf {
    let mut joined = default_path.replace('\\', "/");
    if !joined.is_empty() && !joined.ends_with('/') {
        joined.push('/');
    }
    joined.push_str(sample);
    let path = base.join(joined);
    if path.exists() {
        path
    } else {
        find_ignoring_case(&path).unwrap_or(path)
    }
}

fn find_ignoring_case(path: &Path) -> Option<PathBuf> {
    let mut found = PathBuf::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            found.push(component.as_os_str());
            continue;
        };
        let exact = found.join(name);
        if exact.exists() {
            found = exact;
            continue;
        }
        let wanted = name.to_string_lossy().to_lowercase();
        let dir = if found.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &found
        };
        let entry = fs::read_dir(dir)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)?;
        found.push(entry.file_name());
    }
    Some(found)
}
