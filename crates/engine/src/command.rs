use midi::NoteEventKind;
use project::{CableKey, DeviceId, ModRouteId, ModulatorId, TrackId};

use crate::Arrangement;

/// Messages from the UI to the audio thread, applied in the order sent.
///
/// Nothing may be freed on the audio thread, so the one command that owns
/// memory, [`Command::Arrangement`], is never dropped there: the engine
/// sends the snapshot it replaces back to the UI to be dropped.
#[derive(Debug)]
pub enum Command {
    /// Start or stop the transport. Stopping returns to where playback last
    /// started; stopping again while stopped returns to the very start.
    SetPlaying(bool),
    /// Move the playhead to a sample. Stop returns here from now on.
    Seek(u64),
    /// Play this snapshot from now on.
    Arrangement(Box<Arrangement>),
    /// Live fader moves, sent while dragging so nothing gets rebuilt. The
    /// snapshot sent when the drag ends carries the same values.
    SetTrackVolume {
        track: TrackId,
        db: f32,
    },
    SetTrackPan {
        track: TrackId,
        pan: f32,
    },
    SetTrackMute {
        track: TrackId,
        mute: bool,
    },
    SetMasterVolume(f32),
    SetInstrumentParam {
        track: TrackId,
        index: usize,
        value: f32,
    },
    /// Play or release a note on a track right away, outside the
    /// arrangement, e.g. from the piano roll's keyboard.
    PreviewNote {
        track: TrackId,
        event: NoteEventKind,
    },
    /// A live effect knob move, like [`Command::SetInstrumentParam`].
    SetDeviceParam {
        device: DeviceId,
        index: usize,
        value: f32,
    },
    /// Where live input (a MIDI keyboard) plays. Notes held on the previous
    /// target are released.
    SetLiveTarget(Option<TrackId>),
    /// A click on every beat while playing, higher on the first of a bar.
    SetMetronome(bool),
    /// A live drag of a modulation route's amount (a ring on a knob), on a
    /// track (`owner`; the master has no modulation yet).
    SetModAmount {
        owner: Option<TrackId>,
        route: ModRouteId,
        amount: f32,
    },
    /// A live knob move on a modulator's parameter, `index` in its
    /// descriptor's order, like [`Command::SetDeviceParam`].
    SetModulatorParam {
        modulator: ModulatorId,
        index: usize,
        value: f32,
    },
    /// A live drag of a graph cable's level, on a track's graph or
    /// (`owner: None`) the master's.
    SetCableGain {
        owner: Option<TrackId>,
        cable: CableKey,
        gain_db: f32,
    },
}
