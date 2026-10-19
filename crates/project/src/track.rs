use std::collections::BTreeMap;
use std::path::PathBuf;

use effects::EffectRef;
use instruments::InstrumentRef;
use serde::{Deserialize, Serialize};

use crate::{
    AudioClip, AutomationLane, AutomationTarget, ClipRef, Color, DeviceId, MidiClip, Mixer,
    ModRoute, Modulator, PluginData, Routing, TrackId,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub color: Color,
    pub mixer: Mixer,
    /// Effects, after the instrument or clips (or, for a group, its
    /// children) and before the fader. Without `routing`, this is the
    /// order the sound goes through them; with it, the order sound reaches
    /// them (kept topological, see `routing.rs`).
    pub devices: Vec<Device>,
    /// How the sound flows through `devices` when it isn't a straight line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<Routing>,
    /// Standalone modulators (LFOs, envelopes, macros...); see
    /// `modulation.rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modulators: Vec<Modulator>,
    /// What modulates what on this track.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modulations: Vec<ModRoute>,
    pub automation: Vec<AutomationLane>,
    pub kind: TrackKind,
}

// A MIDI track's kind is the big one; tracks are few, so boxing it (or its
// plugin data) would save little.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Audio {
        clips: Vec<AudioClip>,
    },
    Midi {
        instrument: InstrumentRef,
        /// Parameters the user has set, by id. The rest are at their defaults.
        params: BTreeMap<String, f32>,
        clips: Vec<MidiClip>,
        /// What the instrument plays from, for instruments that need a file
        /// (a sampler's .sfz).
        #[serde(default)]
        file: Option<PathBuf>,
        /// User wavetables (WAV files) for the Wavetable's oscillators, by
        /// oscillator: 0 is osc A, 1 osc B, 2 osc C; `None` (or past the
        /// end) for none. An oscillator plays its file when its table
        /// choice is "User". Kept when the instrument changes, like `file`,
        /// and loaded only while the track plays the Wavetable. See
        /// [`crate::Project::set_wavetable_file`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tables: Vec<Option<PathBuf>>,
        /// For a plugin instrument: its name, vendor and saved state. Its
        /// parameters are in `params`, like a built-in's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<PluginData>,
    },
    /// Holds tracks instead of clips. Groups nest, and a group's children are
    /// contiguous on screen because they're stored inside it.
    Group {
        children: Vec<Track>,
        folded: bool,
    },
}

/// An effect in a chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub effect: EffectRef,
    /// Parameters the user has set, by id. The rest are at their defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// Passes the sound through untouched.
    #[serde(default)]
    pub bypassed: bool,
    /// The track this effect listens to, for effects that take a sidechain
    /// (a compressor ducking under the kick, a vocoder's voice): that
    /// track's sound after its effects, before its fader.
    #[serde(default)]
    pub sidechain: Option<TrackId>,
    /// Shown as just its name, to save room in a long chain. A view
    /// setting: not an undo step.
    #[serde(default)]
    pub folded: bool,
    /// For a plugin effect: its name, vendor and saved state. Its
    /// parameters are in `params`, like a built-in's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginData>,
}

impl Device {
    /// The current value of parameter `id`, or its default; `None` if the
    /// effect has no such parameter (or isn't available).
    pub fn param(&self, id: &str) -> Option<f32> {
        let descriptor = self
            .effect
            .descriptor()?
            .params
            .iter()
            .find(|p| p.id == id)?;
        Some(
            self.params
                .get(id)
                .map_or(descriptor.default_value(), |v| descriptor.clamp(*v)),
        )
    }

    /// Every parameter's value, in the effect's order.
    pub fn values(&self) -> Vec<f32> {
        self.effect.descriptor().map_or_else(Vec::new, |d| {
            d.params
                .iter()
                .map(|p| {
                    self.params
                        .get(p.id)
                        .map_or(p.default_value(), |v| p.clamp(*v))
                })
                .collect()
        })
    }

    /// The current value of UI setting `id` (an analyzer's range), or its
    /// default; `None` if the effect has no such setting.
    pub fn setting(&self, id: &str) -> Option<f32> {
        let descriptor = self
            .effect
            .descriptor()?
            .settings
            .iter()
            .find(|p| p.id == id)?;
        Some(
            self.params
                .get(id)
                .map_or(descriptor.default_value(), |v| descriptor.clamp(*v)),
        )
    }

    /// The effect's name; for a plugin that isn't installed, the name it
    /// had when it was saved.
    pub fn name(&self) -> &str {
        match (self.effect.descriptor(), &self.plugin) {
            (Some(d), _) => d.name,
            (None, Some(plugin)) => &plugin.name,
            (None, None) => &self.effect.id,
        }
    }
}

impl Track {
    /// A copy without the clips, for a view that reads everything else
    /// every frame (the device view): the clips hold the notes, which are
    /// most of a track, and copying them every frame would be the frame.
    pub fn without_clips(&self) -> Track {
        let kind = match &self.kind {
            TrackKind::Audio { clips: _ } => TrackKind::Audio { clips: Vec::new() },
            TrackKind::Midi {
                instrument,
                params,
                clips: _,
                file,
                tables,
                plugin,
            } => TrackKind::Midi {
                instrument: instrument.clone(),
                params: params.clone(),
                clips: Vec::new(),
                file: file.clone(),
                tables: tables.clone(),
                plugin: plugin.clone(),
            },
            TrackKind::Group { children, folded } => TrackKind::Group {
                children: children.iter().map(Track::without_clips).collect(),
                folded: *folded,
            },
        };
        Track {
            id: self.id,
            name: self.name.clone(),
            color: self.color,
            mixer: self.mixer,
            devices: self.devices.clone(),
            routing: self.routing.clone(),
            modulators: self.modulators.clone(),
            modulations: self.modulations.clone(),
            automation: self.automation.clone(),
            kind,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, TrackKind::Group { .. })
    }

    pub fn children(&self) -> &[Track] {
        match &self.kind {
            TrackKind::Group { children, .. } => children,
            _ => &[],
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Track>> {
        match &mut self.kind {
            TrackKind::Group { children, .. } => Some(children),
            _ => None,
        }
    }

    pub fn is_folded(&self) -> bool {
        matches!(self.kind, TrackKind::Group { folded: true, .. })
    }

    /// This track's own clips (none for a group), sorted by start.
    pub fn clips(&self) -> Vec<ClipRef<'_>> {
        match &self.kind {
            TrackKind::Audio { clips } => clips.iter().map(ClipRef::Audio).collect(),
            TrackKind::Midi { clips, .. } => clips.iter().map(ClipRef::Midi).collect(),
            TrackKind::Group { .. } => Vec::new(),
        }
    }

    pub fn automation_lane(&self, target: &AutomationTarget) -> Option<&AutomationLane> {
        self.automation.iter().find(|lane| &lane.target == target)
    }

    /// The current value of instrument parameter `id`, or its default.
    pub fn instrument_param(&self, id: &str) -> Option<f32> {
        let TrackKind::Midi {
            instrument, params, ..
        } = &self.kind
        else {
            return None;
        };
        let descriptor = instrument
            .descriptor()?
            .params
            .iter()
            .find(|p| p.id == id)?;
        Some(
            params
                .get(id)
                .map_or(descriptor.default_value(), |v| descriptor.clamp(*v)),
        )
    }

    /// Every instrument parameter's value (or default), in the order its
    /// descriptor lists them; empty if the track has no instrument this
    /// build knows. One lookup each, where [`Track::instrument_param`]
    /// would search the descriptor for every one: instruments can have
    /// thousands.
    pub fn instrument_values(&self) -> Vec<f32> {
        let TrackKind::Midi {
            instrument, params, ..
        } = &self.kind
        else {
            return Vec::new();
        };
        let Some(descriptor) = instrument.descriptor() else {
            return Vec::new();
        };
        descriptor
            .params
            .iter()
            .map(|p| params.get(p.id).map_or(p.default_value(), |v| p.clamp(*v)))
            .collect()
    }

    /// The file a MIDI track's instrument plays from, if it has one.
    pub fn instrument_file(&self) -> Option<&std::path::Path> {
        match &self.kind {
            TrackKind::Midi { file, .. } => file.as_deref(),
            _ => None,
        }
    }

    /// The user wavetable (a WAV file) of the Wavetable's oscillator `osc`
    /// (0 = A, 1 = B, 2 = C) on this MIDI track, if it has one.
    pub fn wavetable_file(&self, osc: usize) -> Option<&std::path::Path> {
        self.wavetable_files().get(osc)?.as_deref()
    }

    /// Every oscillator's user wavetable, by oscillator (see
    /// [`Track::wavetable_file`]); empty for none, and never ending in
    /// `None`.
    pub fn wavetable_files(&self) -> &[Option<PathBuf>] {
        match &self.kind {
            TrackKind::Midi { tables, .. } => tables,
            _ => &[],
        }
    }

    /// Visits this track and everything under it, parents before children.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Track)) {
        f(self);
        for child in self.children() {
            child.walk(f);
        }
    }
}

/// Finds a track anywhere in a tree.
pub(crate) fn find_mut(tracks: &mut [Track], id: TrackId) -> Option<&mut Track> {
    for track in tracks {
        if track.id == id {
            return Some(track);
        }
        if let Some(children) = track.children_mut()
            && let Some(found) = find_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}
