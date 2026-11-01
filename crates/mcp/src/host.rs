//! What the tools need from the app, as a trait: the project and its
//! undo history, the selection, the transport, the device registries and
//! presets, and the jobs that take longer than a frame (importing a file,
//! rendering for analysis). The app implements it on its UI thread over the
//! `Session`; tests implement it with a fake (`testing.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use midi::Tick;
use project::{ClipId, DeviceId, EditError, History, NoteId, Project, TrackId};
use serde_json::Value;

/// An edit to apply as one undo step (see [`Host::edit`]): it returns the
/// result to send back, or an error after which the project is restored.
pub type Apply<'a> = Box<dyn FnOnce(&mut Project) -> Result<Value, EditError> + 'a>;

pub trait Host {
    /// The project as it is shown.
    fn project(&self) -> &Project;

    /// Runs `apply` on the project as one undo step called `label`. On an
    /// error the project is left as it was and no step is recorded (as
    /// `History::edit` does).
    fn edit(&mut self, label: &str, apply: Apply<'_>) -> Result<Value, EditError>;

    fn history(&self) -> &History;
    /// Steps back; the undone step's label, or `None` with nothing to undo.
    fn undo(&mut self) -> Option<String>;
    fn redo(&mut self) -> Option<String>;
    /// Undoes or redoes until `position` steps are done (see
    /// `History::jump`).
    fn jump(&mut self, position: usize);

    /// What the user has selected.
    fn selection(&self) -> Selection;
    fn transport(&self) -> Transport;
    fn set_playing(&mut self, playing: bool);
    fn seek(&mut self, at: Tick);
    /// The project file's name, if it has been saved.
    fn project_name(&self) -> Option<String>;

    /// Every installed plugin the scan found.
    fn plugins(&self) -> Vec<PluginEntry>;
    /// The presets for the device type of `target`, factory ones first.
    fn presets(&self, target: Target) -> Vec<PresetEntry>;
    fn load_preset(&mut self, target: Target, name: &str) -> Result<(), String>;

    /// A Slicer track's loaded loop and its slices; `None` if the track
    /// isn't a Slicer or its file hasn't loaded.
    fn slicer(&self, track: TrackId) -> Option<SlicerInfo>;
    fn slicer_op(&mut self, track: TrackId, op: SlicerOp) -> Result<Value, String>;
    /// Gives a track's instrument a file to play from (a Sampler's .sfz, a
    /// Slicer's loop).
    fn set_instrument_file(&mut self, track: TrackId, path: PathBuf) -> Result<(), String>;
    /// How a file given with [`Host::set_instrument_file`] is coming
    /// along: `None` while it loads (a Slicer's loop is decoded first),
    /// then `Ok` once the track plays it, or why it couldn't.
    fn instrument_file_loaded(&self, track: TrackId, path: &Path) -> Option<Result<(), String>>;
    /// Starts loading an audio file to become a clip at `start` on `track`
    /// (a new audio track if `None`); [`Host::poll`] answers
    /// [`Poll::Imported`] once it has.
    fn import_audio(
        &mut self,
        path: PathBuf,
        track: Option<TrackId>,
        start: Tick,
    ) -> Result<Ticket, String>;
    /// Clip operations that need the audio (chopping at transients,
    /// consolidating, normalizing, freezing). Returns the clips made.
    fn process_clips(&mut self, op: ClipProcess, clips: &[ClipId]) -> Result<Vec<ClipId>, String>;

    /// Starts rendering for analysis, off the UI thread.
    fn render(&mut self, request: RenderRequest) -> Result<Ticket, String>;
    /// How a job started by [`Host::import_audio`] or [`Host::render`] is
    /// doing. A finished job is answered once.
    fn poll(&mut self, ticket: Ticket) -> Poll;
    fn cancel(&mut self, ticket: Ticket);
}

/// What the user has selected, so "this clip" and "these notes" resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    /// The clip open in the piano roll, if any.
    pub open_clip: Option<ClipId>,
    /// Selected notes of `open_clip`.
    pub notes: Vec<NoteId>,
    /// The master channel is selected.
    pub master: bool,
    pub devices: Vec<DeviceId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Transport {
    pub playing: bool,
    pub playhead: Tick,
}

/// Whose parameters or presets: an effect, or a track's instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Device(DeviceId),
    Instrument(TrackId),
}

/// An installed plugin, as the catalogue lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    pub name: String,
    pub vendor: String,
    /// "CLAP", "VST3" or "VST2".
    pub format: String,
    /// An instrument rather than an effect.
    pub instrument: bool,
    /// The id it is stored under (a CLAP id, a VST3 class id, a VST2
    /// unique id), as in `InstrumentRef::id`.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    pub name: String,
    /// Ships with canopy, rather than saved by the user.
    pub factory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerInfo {
    pub file: PathBuf,
    /// The file's own rate, which `slices` count frames at.
    pub rate: u32,
    pub frames: u64,
    /// Each slice's first frame, sorted, the first at 0.
    pub slices: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlicerOp {
    /// Slices starting at these frames.
    Set(Vec<u64>),
    Transients {
        sensitivity: f32,
    },
    /// Equal slices, `division` to a bar.
    Grid {
        division: u32,
    },
    /// A MIDI clip at `at` that plays every slice in order.
    ToPianoRoll {
        at: Tick,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipProcess {
    ChopTransients { sensitivity: f32 },
    Consolidate,
    Normalize,
    Freeze,
}

/// Names a job that finishes on a later frame.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ticket(pub u64);

#[derive(Debug, Clone)]
pub enum Poll {
    /// Still going; `progress` is `0..=1`.
    Waiting {
        progress: f32,
    },
    Imported(Result<ClipId, String>),
    Rendered(Result<Arc<Renders>, String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderRequest {
    pub targets: Vec<RenderTarget>,
    /// From and to; `None` for the whole song.
    pub range: Option<(Tick, Tick)>,
    pub variant: Variant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderTarget {
    /// A track (a group with everything in it).
    Track(TrackId),
    Master,
}

/// Which point of a channel is heard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Variant {
    /// After the effects and the fader.
    #[default]
    Mix,
    /// After the effects, before the fader.
    PreFader,
    /// Without the effects.
    Dry,
}

/// Rendered audio, one buffer per target.
#[derive(Debug, Clone)]
pub struct Renders {
    /// The history generation the render was made from.
    pub version: u64,
    /// The project changed while it rendered: the audio is of an older
    /// version than the one shown.
    pub stale: bool,
    /// Where the audio starts in the song.
    pub start: Tick,
    pub audio: Vec<(RenderTarget, Arc<media::AudioBuffer>)>,
}
