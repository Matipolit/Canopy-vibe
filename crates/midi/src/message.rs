//! Raw MIDI messages from hardware, as bytes, into something to act on.

use crate::{NoteEventKind, Pitch, Velocity};

/// The channel messages canopy uses. Channels are `0..16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    /// A Note On, Note Off, or pitch wheel move: what instruments play.
    Note { channel: u8, kind: NoteEventKind },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
}

/// The sustain pedal's controller number.
pub const SUSTAIN_PEDAL: u8 = 64;

impl MidiMessage {
    /// Reads one message. Anything canopy doesn't use (clock, aftertouch,
    /// program changes, SysEx, running status without a status byte) is
    /// `None`.
    pub fn parse(bytes: &[u8]) -> Option<MidiMessage> {
        let (&status, data) = bytes.split_first()?;
        let channel = status & 0x0F;
        let byte = |i: usize| data.get(i).copied().filter(|b| *b < 0x80);
        let note = |kind| Some(MidiMessage::Note { channel, kind });
        match status & 0xF0 {
            0x80 => note(NoteEventKind::Off {
                pitch: Pitch::new(byte(0)?)?,
                velocity: Velocity::from_midi1(byte(1)?),
            }),
            // A Note On with velocity 0 is a Note Off, by convention.
            0x90 => {
                let pitch = Pitch::new(byte(0)?)?;
                match byte(1)? {
                    0 => note(NoteEventKind::Off {
                        pitch,
                        velocity: Velocity::CENTER,
                    }),
                    v => note(NoteEventKind::On {
                        pitch,
                        velocity: Velocity::from_midi1(v),
                    }),
                }
            }
            0xB0 => Some(MidiMessage::ControlChange {
                channel,
                controller: byte(0)?,
                value: byte(1)?,
            }),
            0xE0 => {
                let value = i16::from(byte(0)?) | (i16::from(byte(1)?) << 7);
                note(NoteEventKind::PitchBend(value - 8192))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_keyboards_send() {
        let on = MidiMessage::parse(&[0x91, 60, 100]).unwrap();
        assert_eq!(
            on,
            MidiMessage::Note {
                channel: 1,
                kind: NoteEventKind::On {
                    pitch: Pitch::new(60).unwrap(),
                    velocity: Velocity::from_midi1(100)
                }
            }
        );
        assert!(matches!(
            MidiMessage::parse(&[0x90, 60, 0]),
            Some(MidiMessage::Note {
                kind: NoteEventKind::Off { .. },
                ..
            })
        ));
        assert!(matches!(
            MidiMessage::parse(&[0x80, 60, 64]),
            Some(MidiMessage::Note {
                kind: NoteEventKind::Off { .. },
                ..
            })
        ));
        assert_eq!(
            MidiMessage::parse(&[0xB0, 64, 127]),
            Some(MidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 127
            })
        );
        let bend = |lsb, msb| match MidiMessage::parse(&[0xE0, lsb, msb]) {
            Some(MidiMessage::Note {
                kind: NoteEventKind::PitchBend(v),
                ..
            }) => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(bend(0, 64), 0);
        assert_eq!(bend(0, 0), -8192);
        assert_eq!(bend(127, 127), 8191);
        // Not for us, or broken.
        assert_eq!(MidiMessage::parse(&[0xF8]), None);
        assert_eq!(MidiMessage::parse(&[0x90, 60]), None);
        assert_eq!(MidiMessage::parse(&[0x90, 200, 5]), None);
        assert_eq!(MidiMessage::parse(&[]), None);
    }

    #[test]
    fn bend_ratio_spans_two_semitones() {
        assert_eq!(NoteEventKind::bend_ratio(0), 1.0);
        assert!((NoteEventKind::bend_ratio(8191) - 2f32.powf(2.0 / 12.0)).abs() < 1e-3);
        assert!((NoteEventKind::bend_ratio(-8192) - 2f32.powf(-2.0 / 12.0)).abs() < 1e-6);
    }
}
