use crate::{Pitch, Velocity};

/// A MIDI channel message
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteEvent {
    /// Sample offset within the current block, `0..block_len`.
    pub frame: u32,
    pub kind: NoteEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoteEventKind {
    /// MIDI CC 123. Sent on seek, stop, and when a snapshot changes the track.
    AllNotesOff,
    /// Matched to its Note On by pitch, as in MIDI. `velocity` is the release velocity.
    Off {
        pitch: Pitch,
        velocity: Velocity,
    },
    On {
        pitch: Pitch,
        velocity: Velocity,
    },
    /// The sounding note of `pitch` (matched as a Note Off is) is now
    /// `cents` from its own pitch: the note's own bend, drawn in the piano
    /// roll. Declared after `On`, so a bend at a note's start sorts after it.
    NoteBend {
        pitch: Pitch,
        cents: i16,
    },
    /// The pitch wheel: `-8192..=8191`, centered at 0. Instruments bend by
    /// up to [`PITCH_BEND_RANGE`] semitones either way.
    PitchBend(i16),
    /// A controller an instrument may react to itself, like the mod wheel
    /// ([`MOD_WHEEL`]): `value` is `0..=127`. Controllers linked to controls
    /// and the sustain pedal are handled before instruments see anything.
    Control {
        controller: u8,
        value: u8,
    },
}

/// The mod wheel's controller number.
pub const MOD_WHEEL: u8 = 1;

/// How far a full pitch wheel bends, in semitones.
pub const PITCH_BEND_RANGE: f32 = 2.0;

impl NoteEventKind {
    /// The frequency ratio a pitch wheel position bends by.
    pub fn bend_ratio(value: i16) -> f32 {
        let semitones = f32::from(value.clamp(-8192, 8191)) / 8192.0 * PITCH_BEND_RANGE;
        (semitones / 12.0).exp2()
    }

    /// The frequency ratio a [`NoteEventKind::NoteBend`] of `cents` bends by.
    pub fn note_bend_ratio(cents: i16) -> f32 {
        (f32::from(cents) / 1200.0).exp2()
    }
}

/// How often a note's bend is sent while it moves, in frames: once an
/// engine block. Instruments glide to each value over that long.
pub const BEND_STEP_FRAMES: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;

    fn event(frame: u32, kind: NoteEventKind) -> NoteEvent {
        NoteEvent { frame, kind }
    }

    #[test]
    fn sorts_by_frame_then_releases_first() {
        let pitch = Pitch::MIDDLE_C;
        let velocity = Velocity::CENTER;
        let on = NoteEventKind::On { pitch, velocity };
        let off = NoteEventKind::Off { pitch, velocity };

        let mut events = [
            event(10, on),
            event(10, off),
            event(3, on),
            event(10, NoteEventKind::AllNotesOff),
        ];
        events.sort_unstable();
        assert_eq!(
            events,
            [
                event(3, on),
                event(10, NoteEventKind::AllNotesOff),
                event(10, off),
                event(10, on),
            ]
        );
    }
}
