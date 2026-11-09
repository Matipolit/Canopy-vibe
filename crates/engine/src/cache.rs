//! Tracks rendered ahead of time: while the song plays, a track with a
//! cache reads it instead of running its instrument and effects (see
//! [`TrackCache`]). This module says which tracks can be cached, what a
//! render depends on ([`render_key`]), and makes the render ([`render`]).
//! Deciding when to render and keeping the renders is the UI's business.
//!
//! A render is the track's sound after its effects and before its fader,
//! from the start of the song, so everything past that point stays live:
//! fader, pan, mute, solo, volume and pan automation, the groups and the
//! master. What comes before it is in the key: the track's clips, notes,
//! instrument, effects and their automation, the audio and data it plays,
//! the tracks its effects listen to, the tempo and the sample rate. A
//! change to any of them is a different key, whichever edit made it, so
//! nothing has to say what invalidates a render.

use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use instruments::{DataKind, InstrumentData};
use media::AudioBuffer;
use project::{
    AudioClip, AutomationTarget, ClipId, Mixer, Project, SourceId, Track, TrackId, TrackKind,
};

use crate::arrangement::{AudioLookup, Compiler, TrackCache};
use crate::bounce::Bounce;

/// What a render depends on. The hash covers the project by value, and
/// the audio and instrument data it plays by address; `inputs` holds that
/// audio and data, so the addresses can't be reused by something else
/// while the key lives, and so a render can be made from exactly them on
/// another thread.
#[derive(Clone)]
pub struct RenderKey {
    pub hash: u64,
    pub inputs: RenderInputs,
}

/// The audio and instrument data a render plays, as the compiler asked
/// for them when the key was made: a lookup to render with.
#[derive(Clone, Default)]
pub struct RenderInputs {
    sources: HashMap<SourceId, Arc<AudioBuffer>>,
    renders: HashMap<ClipId, Arc<AudioBuffer>>,
    data: HashMap<TrackId, InstrumentData>,
}

impl AudioLookup for RenderInputs {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.sources.get(&id).cloned()
    }

    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        self.renders.get(&clip.id).cloned()
    }

    fn instrument_data(&self, track: &Track) -> Option<InstrumentData> {
        self.data.get(&track.id).cloned()
    }
}

impl PartialEq for RenderKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl std::fmt::Debug for RenderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RenderKey({:016x})", self.hash)
    }
}

/// Why a track can't be cached, for the UI to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uncacheable {
    /// A group: its sound is its children's, which stay live.
    Group,
    /// Nothing on it to play.
    Empty,
    /// A plugin, whose state can change without the project knowing.
    Plugin,
    /// Modulators or a routed chain, whose state a render can't take over.
    Modulated,
    /// An effect listens to a group, or to a track listening back.
    Sidechain,
}

/// The tracks whose sound `track`'s render depends on, `track` first,
/// then the tracks its effects listen to, and theirs, by id.
fn closure(project: &Project, track: TrackId) -> Result<Vec<TrackId>, Uncacheable> {
    let mut order = vec![track];
    let mut seen = BTreeSet::from([track]);
    let mut next = 0;
    while let Some(&id) = order.get(next) {
        next += 1;
        let current = project.track(id).ok_or(Uncacheable::Empty)?;
        let mut sources = Vec::new();
        project::walk_devices(&current.devices, &mut |device| {
            let listens = device.effect.descriptor().is_some_and(|d| d.sidechain);
            sources.extend(device.sidechain.filter(|_| listens));
        });
        for source in sources {
            if source == track {
                return Err(Uncacheable::Sidechain);
            }
            if seen.insert(source) {
                order.push(source);
            }
        }
    }
    Ok(order)
}

/// Whether one track could be cached at all, its data aside.
fn check_track(track: &Track) -> Result<(), Uncacheable> {
    let clips = match &track.kind {
        TrackKind::Group { .. } => return Err(Uncacheable::Group),
        TrackKind::Audio { clips } => clips.iter().filter(|c| !c.muted).count(),
        TrackKind::Midi {
            instrument, clips, ..
        } => {
            if !instrument.format.is_builtin() {
                return Err(Uncacheable::Plugin);
            }
            clips.iter().filter(|c| !c.muted).count()
        }
    };
    let mut plugin = false;
    project::walk_devices(&track.devices, &mut |d| {
        plugin |= !d.effect.format.is_builtin()
    });
    if plugin {
        return Err(Uncacheable::Plugin);
    }
    if !track.modulators.is_empty() || !track.modulations.is_empty() || track.routing.is_some() {
        return Err(Uncacheable::Modulated);
    }
    if clips == 0 {
        return Err(Uncacheable::Empty);
    }
    Ok(())
}

/// Whether `track` can be cached, as far as the project says.
pub fn cacheable(project: &Project, track: TrackId) -> Result<(), Uncacheable> {
    let order = closure(project, track)?;
    for (i, id) in order.iter().enumerate() {
        let current = project.track(*id).ok_or(Uncacheable::Empty)?;
        match check_track(current) {
            // A source that plays nothing still feeds its listener silence.
            Err(Uncacheable::Empty) if i > 0 => {}
            Err(Uncacheable::Group) if i > 0 => return Err(Uncacheable::Sidechain),
            Err(why) => return Err(why),
            Ok(()) => {}
        }
    }
    Ok(())
}

/// A hash of what in `track` shapes its sound before the fader: all of it
/// but its name, color, fader values and volume and pan automation. The
/// channel's parameter-ramp policy does affect its sound and stays. Costs a
/// serialization of the track, so callers keep it while the track is
/// unchanged.
pub fn track_fingerprint(track: &Track) -> u64 {
    let mut trimmed = track.clone();
    trimmed.name.clear();
    trimmed.color = project::Color { r: 0, g: 0, b: 0 };
    trimmed.mixer = Mixer {
        raw_parameters: track.mixer.raw_parameters,
        ..Mixer::default()
    };
    trimmed.automation.retain(|lane| {
        !matches!(
            lane.target,
            AutomationTarget::Volume | AutomationTarget::Pan
        )
    });
    let text = ron::to_string(&trimmed).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// What `track`'s render depends on, at `sample_rate`, with the audio and
/// data `lookup` would give the compiler, or why it can't be cached. `fingerprint` hashes a track ([`track_fingerprint`], or a
/// remembered one).
pub fn render_key(
    project: &Project,
    track: TrackId,
    sample_rate: u32,
    lookup: &dyn AudioLookup,
    fingerprint: &mut dyn FnMut(&Track) -> u64,
) -> Result<RenderKey, Uncacheable> {
    cacheable(project, track)?;
    let order = closure(project, track)?;
    let mut hasher = DefaultHasher::new();
    let mut inputs = RenderInputs::default();
    sample_rate.hash(&mut hasher);
    ron::to_string(&project.tempo)
        .unwrap_or_default()
        .hash(&mut hasher);
    for id in &order {
        let current = project.track(*id).ok_or(Uncacheable::Empty)?;
        id.hash(&mut hasher);
        fingerprint(current).hash(&mut hasher);
        match &current.kind {
            TrackKind::Audio { clips } => {
                for clip in clips.iter().filter(|c| !c.muted) {
                    if let Some(source) = project.source(clip.source) {
                        source.path.hash(&mut hasher);
                        source.sample_rate.hash(&mut hasher);
                    }
                    // Whatever the compiler would play, missing audio
                    // (silent) included: when it arrives the key changes.
                    let audio = if clip.is_plain(&project.tempo) {
                        let audio = lookup.source(clip.source);
                        if let Some(audio) = &audio {
                            inputs.sources.insert(clip.source, audio.clone());
                        }
                        audio
                    } else {
                        let audio = lookup.rendered(clip);
                        if let Some(audio) = &audio {
                            inputs.renders.insert(clip.id, audio.clone());
                        }
                        audio
                    };
                    audio.as_ref().map(Arc::as_ptr).hash(&mut hasher);
                }
            }
            TrackKind::Midi { instrument, .. } => {
                let needs_data = instrument
                    .descriptor()
                    .is_some_and(|d| d.data != DataKind::None);
                let data = needs_data
                    .then(|| lookup.instrument_data(current))
                    .flatten();
                data.as_ref()
                    .map(|d| Arc::as_ptr(d).cast::<()>())
                    .hash(&mut hasher);
                if let Some(data) = data {
                    inputs.data.insert(*id, data);
                }
            }
            TrackKind::Group { .. } => {}
        }
    }
    Ok(RenderKey {
        hash: hasher.finish(),
        inputs,
    })
}

/// The project a render of `track` plays: the track, and the tracks it
/// listens to (muted, so only their sends are heard), alone at the top,
/// through an empty master, with nothing soloed and no loop.
pub fn render_project(project: &Project, track: TrackId) -> Option<Project> {
    let order = closure(project, track).ok()?;
    let mut alone = project.clone();
    alone.tracks = order
        .iter()
        .map(|id| {
            let mut copy = project.track(*id)?.clone();
            copy.mixer = Mixer {
                mute: *id != track,
                raw_parameters: copy.mixer.raw_parameters,
                ..Mixer::default()
            };
            Some(copy)
        })
        .collect::<Option<_>>()?;
    alone.master = Mixer::default();
    alone.master_devices.clear();
    alone.master_routing = None;
    alone.loop_region = None;
    Some(alone)
}

/// Renders `track`'s cache: its sound before its fader from the start of
/// the song to its end and its tail, as [`TrackCache`] wants it. Runs on
/// the calling thread, as fast as it can; `cancel` stops it between
/// blocks (giving `None`), and `progress` hears how far along it is. Not
/// for tracks with plugins, which [`cacheable`] refuses.
pub fn render(
    project: &Project,
    track: TrackId,
    sample_rate: u32,
    lookup: impl AudioLookup,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Option<AudioBuffer> {
    let alone = render_project(project, track)?;
    let mut compiler = Compiler::new(sample_rate);
    let arrangement = compiler.compile(&alone, lookup);
    let end = alone.tempo.tick_to_sample(alone.end(), sample_rate).max(1) as u64;
    let mut bounce = Bounce::new(arrangement, sample_rate, end, true).capturing(track);
    // Many blocks at a time between looks at `cancel`: a fraction of a
    // millisecond's work.
    while !bounce.step(4 * crate::MAX_BLOCK) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        progress(bounce.progress());
    }
    bounce.take_capture(track)
}

/// A finished render, made into what the compiler plays.
pub fn active(audio: Arc<AudioBuffer>) -> TrackCache {
    TrackCache {
        audio,
        active: true,
    }
}
