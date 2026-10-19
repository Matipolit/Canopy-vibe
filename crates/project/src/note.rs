use midi::{Pitch, Tick, Velocity};
use serde::{Deserialize, Serialize};

use crate::NoteId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    /// From the start of the clip's content (not the clip's position on the
    /// timeline), so moving a clip never touches its notes.
    pub start: Tick,
    pub len: Tick,
    pub pitch: Pitch,
    pub velocity: Velocity,
    /// Note Off velocity. [`Velocity::CENTER`] if the source didn't measure it.
    pub release_velocity: Velocity,
    pub muted: bool,
}

impl Note {
    pub fn end(&self) -> Tick {
        self.start + self.len
    }
}
