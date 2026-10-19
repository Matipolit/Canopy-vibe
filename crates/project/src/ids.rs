use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// Stable for the life of the track. Edits and engine commands refer to
    /// tracks by id, never by position, so reordering can't misdirect them.
    TrackId
);
id_type!(ClipId);
id_type!(NoteId);
id_type!(SourceId);
id_type!(
    /// An effect in a track's or the master's chain. Stable while it lives,
    /// so the engine keeps its state (a reverb's tail) across edits.
    DeviceId
);
id_type!(
    /// A node in a track's or the master's effect graph ([`crate::Routing`]).
    NodeId
);
id_type!(
    /// A track's standalone modulator (an LFO, an envelope, a macro).
    ModulatorId
);
id_type!(
    /// One modulation route: a source moving a parameter.
    ModRouteId
);
