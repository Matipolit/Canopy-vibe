//! Audio for clips that don't play their file as is (reversed, looped,
//! stretched), rendered on a worker thread and kept until no clip needs it.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use engine::AudioLookup;
use instruments::InstrumentData;
use media::{AudioBuffer, ClipProcess, Renderer};
use project::{AudioClip, ClipId, Project, SourceId, Track, TrackKind};

use crate::banks::{Banks, DataKey};
use crate::session::SourceState;

pub struct Renders {
    renderer: Renderer,
    /// Finished renders by tag, with the audio they were rendered from. That
    /// keeps the input alive, so its address can't be reused by other audio
    /// while the tag (which hashes it) is still around.
    done: HashMap<u64, (Arc<AudioBuffer>, Arc<AudioBuffer>)>,
    /// The tag each clip is waiting for.
    pending: HashMap<ClipId, u64>,
    /// The newest render each clip has. It plays until the one the clip
    /// needs now is ready, so dragging a control doesn't drop out.
    latest: HashMap<ClipId, u64>,
    /// The input of each render on its way, to keep with the result.
    inputs: HashMap<u64, Arc<AudioBuffer>>,
    /// Bumped when the project is replaced; renders for an older one are
    /// dropped when they arrive.
    generation: u32,
}

/// What a render answers: its input buffer and everything it does to it.
fn tag(input: &Arc<AudioBuffer>, spec: &ClipProcess) -> u64 {
    let mut hasher = DefaultHasher::new();
    (Arc::as_ptr(input) as usize).hash(&mut hasher);
    let ClipProcess {
        offset,
        len,
        reversed,
        looped,
        phase,
        play_len,
        mode,
        speed,
        transpose,
        out_len,
    } = spec;
    (offset, len, reversed, looped, phase, play_len, out_len).hash(&mut hasher);
    (*mode as u8, speed.to_bits(), transpose.to_bits()).hash(&mut hasher);
    hasher.finish()
}

impl Renders {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Renders {
        Renders {
            renderer: Renderer::new(wake),
            done: HashMap::new(),
            pending: HashMap::new(),
            latest: HashMap::new(),
            inputs: HashMap::new(),
            generation: 0,
        }
    }

    /// Forgets everything, for a new project or sample rate.
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.done.clear();
        self.pending.clear();
        self.latest.clear();
        self.inputs.clear();
    }

    fn slot(&self, clip: ClipId) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(clip.0)
    }

    /// Picks up finished renders. True if any arrived that a clip wants.
    pub fn receive(&mut self) -> bool {
        let mut any = false;
        while let Some(rendered) = self.renderer.try_recv() {
            let clip = ClipId(rendered.slot as u32);
            if rendered.slot != self.slot(clip) || self.pending.get(&clip) != Some(&rendered.tag) {
                continue; // superseded, or for another project
            }
            self.pending.remove(&clip);
            if let Some(input) = self.input_for(rendered.tag) {
                self.done.insert(rendered.tag, (input, rendered.audio));
                self.latest.insert(clip, rendered.tag);
                any = true;
            }
        }
        any
    }

    /// Asks for whatever the project's clips need that isn't rendered or on
    /// its way, and lets go of what no clip needs any more.
    pub fn update(
        &mut self,
        project: &Project,
        sources: &HashMap<SourceId, SourceState>,
        rate: u32,
    ) {
        let mut wanted = HashMap::new();
        project.visit(|track, _| {
            let TrackKind::Audio { clips } = &track.kind else {
                return;
            };
            for clip in clips {
                let Some(spec) = engine::clip_process(project, clip, rate) else {
                    continue;
                };
                let Some(SourceState::Ready(source)) = sources.get(&clip.source) else {
                    continue;
                };
                let tag = tag(&source.audio, &spec);
                wanted.insert(clip.id, tag);
                if self.done.contains_key(&tag) {
                    self.latest.insert(clip.id, tag);
                    self.pending.remove(&clip.id);
                } else if self.pending.get(&clip.id) != Some(&tag) {
                    self.pending.insert(clip.id, tag);
                    self.inputs.insert(tag, source.audio.clone());
                    self.renderer
                        .request(self.slot(clip.id), tag, source.audio.clone(), spec);
                }
            }
        });
        self.latest.retain(|clip, _| wanted.contains_key(clip));
        self.pending.retain(|clip, _| wanted.contains_key(clip));
        let keep: HashSet<u64> = self
            .latest
            .values()
            .chain(wanted.values())
            .copied()
            .collect();
        self.done.retain(|tag, _| keep.contains(tag));
        let waiting: HashSet<u64> = self.pending.values().copied().collect();
        self.inputs.retain(|tag, _| waiting.contains(tag));
    }

    fn input_for(&mut self, tag: u64) -> Option<Arc<AudioBuffer>> {
        self.inputs.remove(&tag)
    }

    /// True while `clip` is waiting for its audio.
    pub fn is_rendering(&self, clip: ClipId) -> bool {
        self.pending.contains_key(&clip)
    }

    /// The audio to play for `clip` now: its current render, or an older one
    /// while that's on its way.
    pub fn latest(&self, clip: ClipId) -> Option<Arc<AudioBuffer>> {
        let tag = self.latest.get(&clip)?;
        self.done.get(tag).map(|(_, audio)| audio.clone())
    }

    /// The render matching `clip` exactly as it is now, if there is one.
    pub fn current(
        &self,
        project: &Project,
        sources: &HashMap<SourceId, SourceState>,
        rate: u32,
        clip: &AudioClip,
    ) -> Option<Arc<AudioBuffer>> {
        let spec = engine::clip_process(project, clip, rate)?;
        let Some(SourceState::Ready(source)) = sources.get(&clip.source) else {
            return None;
        };
        let (_, audio) = self.done.get(&tag(&source.audio, &spec))?;
        Some(audio.clone())
    }
}

/// What the compiler plays: loaded sources, the latest renders, and
/// instruments' loaded files.
pub struct Lookup<'a> {
    pub sources: &'a HashMap<SourceId, SourceState>,
    pub renders: &'a Renders,
    pub banks: &'a Banks,
}

impl AudioLookup for Lookup<'_> {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        match self.sources.get(&id) {
            Some(SourceState::Ready(source)) => Some(source.audio.clone()),
            _ => None,
        }
    }

    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        self.renders.latest(clip.id)
    }

    fn instrument_data(&self, track: &Track) -> Option<InstrumentData> {
        self.banks.data(&DataKey::of(track)?)
    }
}

/// Like [`Lookup`], but every clip gets exactly its current audio, rendered
/// here and now if it isn't ready. For bouncing, where a stale render won't do.
pub struct ExactLookup<'a> {
    pub project: &'a Project,
    pub sources: &'a HashMap<SourceId, SourceState>,
    pub renders: &'a Renders,
    pub rate: u32,
}

impl AudioLookup for ExactLookup<'_> {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        match self.sources.get(&id) {
            Some(SourceState::Ready(source)) => Some(source.audio.clone()),
            _ => None,
        }
    }

    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        if let Some(audio) = self
            .renders
            .current(self.project, self.sources, self.rate, clip)
        {
            return Some(audio);
        }
        let spec = engine::clip_process(self.project, clip, self.rate)?;
        let input = self.source(clip.source)?;
        Some(Arc::new(media::process_clip(&input, &spec)))
    }
}
