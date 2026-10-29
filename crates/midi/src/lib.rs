//! MIDI primitives wrapped in Rust types. No dependencies, no I/O.
//!
//! Shared by `project` (notes in the document) and `instruments` (events on the
//! audio thread), so both speak the same units.

mod event;
mod message;
mod pitch;
mod tick;
mod velocity;

pub use event::{BEND_STEP_FRAMES, MOD_WHEEL, NoteEvent, NoteEventKind, PITCH_BEND_RANGE};
pub use message::{MidiMessage, SUSTAIN_PEDAL};
pub use pitch::Pitch;
pub use tick::{PPQ, Tick};
pub use velocity::Velocity;
