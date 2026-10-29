use midi::Tick;
use serde::{Deserialize, Serialize};

use crate::{ClipId, Note, Shared, SourceId, TempoMap};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    pub id: ClipId,
    pub name: String,
    /// Position on the timeline.
    pub start: Tick,
    pub length: Tick,
    /// Where in the notes' timeline the clip begins. Trimming the left edge
    /// moves this instead of deleting notes, so the edit can be dragged back.
    pub offset: Tick,
    pub notes: Shared<Vec<Note>>,
    /// Kept but not played, e.g. after freezing it to audio.
    #[serde(default)]
    pub muted: bool,
}

impl MidiClip {
    pub fn end(&self) -> Tick {
        self.start + self.length
    }

    /// Notes that start inside the visible part of the clip, which are the
    /// only ones that play.
    pub fn audible_notes(&self) -> impl Iterator<Item = &Note> {
        let window = self.offset..self.offset + self.length;
        self.notes
            .iter()
            .filter(move |n| !n.muted && window.contains(&n.start))
    }
}

/// A region of an audio file. Unwarped: its length is fixed in samples, and
/// its length in beats follows from the tempo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    pub id: ClipId,
    pub name: String,
    pub start: Tick,
    pub source: SourceId,
    /// First frame played, at the source's own rate.
    pub source_offset: u64,
    /// Frames played, at the source's own rate.
    pub source_len: u64,
    pub gain_db: f32,
    #[serde(default)]
    pub muted: bool,
    /// Fade lengths in frames at the source's rate, from each edge. They
    /// stay attached to their edge when the clip is trimmed.
    #[serde(default)]
    pub fade_in: u64,
    #[serde(default)]
    pub fade_out: u64,
    /// Fade shapes, `-1..=1`: negative rises (or falls) fast first, zero is
    /// a straight line. The same curve as envelope stages.
    #[serde(default)]
    pub fade_in_curve: f32,
    #[serde(default)]
    pub fade_out_curve: f32,
    /// Whether each fade was made by dropping the clip over another one.
    /// Such a fade goes away again once nothing overlaps that edge.
    #[serde(default)]
    pub crossfade_in: bool,
    #[serde(default)]
    pub crossfade_out: bool,
    /// Plays the region backwards.
    #[serde(default)]
    pub reversed: bool,
    /// Repeats the region for as long as the clip is.
    #[serde(default)]
    pub looped: bool,
    /// Looped clips: where in the (possibly reversed) region playback
    /// starts, in source frames.
    #[serde(default)]
    pub loop_phase: u64,
    /// Looped clips: how much material the clip plays in all, in source
    /// frames, before stretching. Unlooped clips play `source_len`.
    #[serde(default)]
    pub play_len: u64,
    #[serde(default)]
    pub stretch: Stretch,
    /// Flips the waveform upside down.
    #[serde(default)]
    pub polarity_inverted: bool,
    #[serde(default)]
    pub channels: ChannelMode,
}

/// How a clip's audio changes speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StretchMode {
    /// Varispeed, like FL Studio's Resample: faster is higher pitched.
    #[default]
    Resample,
    /// Speed and pitch change independently.
    Stretch,
    /// Cuts at transients and moves the slices; for drums.
    Beats,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stretch {
    pub mode: StretchMode,
    /// How much faster than the original the material plays; with tempo
    /// sync, on top of the tempo ratio.
    pub speed: f64,
    /// Semitones; Stretch mode only (in Resample, pitch follows speed).
    pub transpose: f64,
    /// The material's own tempo, as typed in.
    pub original_bpm: Option<f64>,
    /// Follow the project tempo: with an original tempo set, the clip keeps
    /// its length in beats and its speed changes with the project's tempo.
    pub sync: bool,
}

impl Default for Stretch {
    fn default() -> Self {
        Stretch {
            mode: StretchMode::Resample,
            speed: 1.0,
            transpose: 0.0,
            original_bpm: None,
            sync: false,
        }
    }
}

impl Stretch {
    pub const MIN_SPEED: f64 = 0.125;
    pub const MAX_SPEED: f64 = 8.0;

    /// The speed the material actually plays at.
    pub fn effective_speed(&self, tempo: &TempoMap) -> f64 {
        let speed = self.speed.clamp(Self::MIN_SPEED, Self::MAX_SPEED);
        match self.original_bpm {
            Some(bpm) if self.sync && bpm > 0.0 => {
                (speed * tempo.tempo.to_bpm() / bpm).clamp(Self::MIN_SPEED, Self::MAX_SPEED)
            }
            _ => speed,
        }
    }
}

/// What happens to the two channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChannelMode {
    #[default]
    Stereo,
    /// Both sides get the sum.
    Mono,
    /// Left and right trade places.
    Swap,
}

impl AudioClip {
    /// Frames of material the clip plays, at the source's rate, before
    /// stretching.
    pub fn played(&self) -> u64 {
        if self.looped {
            self.play_len.max(1)
        } else {
            self.source_len
        }
    }

    /// True if the clip plays its region as is: nothing to render.
    pub fn is_plain(&self, tempo: &TempoMap) -> bool {
        let pitched = self.stretch.mode == StretchMode::Stretch && self.stretch.transpose != 0.0;
        !self.reversed && !self.looped && !pitched && self.stretch.effective_speed(tempo) == 1.0
    }

    /// The source frame heard `material` frames into the clip (fractional,
    /// for drawing).
    pub fn source_position(&self, material: f64) -> f64 {
        let len = self.source_len.max(1) as f64;
        let mut x = if self.looped {
            (self.loop_phase as f64 + material).rem_euclid(len)
        } else {
            material
        };
        if self.reversed {
            x = len - x;
        }
        self.source_offset as f64 + x
    }

    /// Keeps the fades inside the clip after its length changed.
    pub(crate) fn clamp_fades(&mut self) {
        let played = self.played();
        self.fade_in = self.fade_in.min(played);
        self.fade_out = self.fade_out.min(played - self.fade_in);
    }
}

/// A clip of either kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipRef<'a> {
    Midi(&'a MidiClip),
    Audio(&'a AudioClip),
}

impl ClipRef<'_> {
    pub fn id(&self) -> ClipId {
        match self {
            ClipRef::Midi(c) => c.id,
            ClipRef::Audio(c) => c.id,
        }
    }

    pub fn start(&self) -> Tick {
        match self {
            ClipRef::Midi(c) => c.start,
            ClipRef::Audio(c) => c.start,
        }
    }

    pub fn muted(&self) -> bool {
        match self {
            ClipRef::Midi(c) => c.muted,
            ClipRef::Audio(c) => c.muted,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ClipRef::Midi(c) => &c.name,
            ClipRef::Audio(c) => &c.name,
        }
    }
}
