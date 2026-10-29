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
    /// The note's own pitch curve, drawn in the piano roll: empty for none.
    /// Saved only when there is one, so files without bends don't change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bend: Vec<BendPoint>,
}

/// A point of a note's pitch curve (see [`Note::bend_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BendPoint {
    /// From the note's start.
    pub at: Tick,
    /// How far from the note's own pitch, within [`BendPoint::MAX_SEMITONES`]
    /// either way.
    pub semitones: f32,
    /// How the segment leading into this point bends, `-1..=1`: the same
    /// curve as envelope stages, zero a straight line.
    pub curve: f32,
}

impl BendPoint {
    /// The furthest a note bends either way, in semitones.
    pub const MAX_SEMITONES: f32 = 48.0;
}

impl Note {
    pub fn end(&self) -> Tick {
        self.start + self.len
    }

    /// How far the note is bent `at` from its start, in semitones: nothing
    /// before its first point, the last point's value after it, and along
    /// each segment the envelope curve of the point it leads to.
    pub fn bend_at(&self, at: Tick) -> f32 {
        let Some(next) = self.bend.iter().position(|p| p.at > at) else {
            return self.bend.last().map_or(0.0, |p| p.semitones);
        };
        if next == 0 {
            return 0.0;
        }
        let (from, to) = (self.bend[next - 1], self.bend[next]);
        let span = (to.at - from.at).get().max(1) as f32;
        let t = (at - from.at).get() as f32 / span;
        from.semitones + (to.semitones - from.semitones) * dsp::stage_shape(t, to.curve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(bend: Vec<BendPoint>) -> Note {
        Note {
            id: NoteId(1),
            start: Tick::ZERO,
            len: Tick::new(960),
            pitch: Pitch::new(60).unwrap(),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
            bend,
        }
    }

    fn point(at: i64, semitones: f32) -> BendPoint {
        BendPoint {
            at: Tick::new(at),
            semitones,
            curve: 0.0,
        }
    }

    #[test]
    fn a_note_without_a_bend_saves_as_before() {
        let text = ron::to_string(&note(Vec::new())).unwrap();
        assert!(!text.contains("bend"), "{text}");
    }

    #[test]
    fn a_bend_survives_save_and_load() {
        let bent = note(vec![point(0, 0.0), point(480, 2.0)]);
        let back: Note = ron::from_str(&ron::to_string(&bent).unwrap()).unwrap();
        assert_eq!(back, bent);
    }

    #[test]
    fn bend_at_follows_the_points_and_holds_the_last() {
        let bent = note(vec![point(240, 0.0), point(480, 2.0)]);
        assert_eq!(
            bent.bend_at(Tick::ZERO),
            0.0,
            "nothing before the first point"
        );
        assert_eq!(
            bent.bend_at(Tick::new(360)),
            1.0,
            "halfway along a straight segment"
        );
        assert_eq!(bent.bend_at(Tick::new(900)), 2.0, "the last point holds");
        assert_eq!(note(Vec::new()).bend_at(Tick::new(100)), 0.0);
        // A curved segment still ends where its point is.
        let curved = note(vec![
            point(0, 0.0),
            BendPoint {
                at: Tick::new(480),
                semitones: -12.0,
                curve: 0.8,
            },
        ]);
        assert_eq!(curved.bend_at(Tick::new(480)), -12.0);
        let middle = curved.bend_at(Tick::new(240));
        assert!(middle != -6.0 && middle < 0.0 && middle > -12.0, "{middle}");
    }
}
