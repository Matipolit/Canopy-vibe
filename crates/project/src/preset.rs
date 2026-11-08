//! The file formats of presets: an effect's or instrument's settings saved
//! under a name, to load into any copy of it ([`PresetFile`]), and a whole
//! rack, a track's instrument and its effects, to load onto any track
//! ([`RackFile`]). Where they're kept is the app's business.

use std::collections::BTreeMap;
use std::path::PathBuf;

use instruments::InstrumentRef;
use serde::{Deserialize, Serialize};

use crate::edit::EditResult;
use crate::{
    Blob, CopiedDevice, Device, DeviceId, EditError, EffectRef, PluginData, Project, TrackId,
    TrackKind,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetFile {
    /// The effect's or instrument's id, so a preset is only ever loaded
    /// into the kind of device it was saved from.
    pub device: String,
    /// Settings by id, as a device stores them.
    pub params: BTreeMap<String, f32>,
    /// A plugin's own state (see `PluginData::state`), for plugins whose
    /// sound isn't all in their parameters. Empty for built-ins.
    #[serde(default, skip_serializing_if = "Blob::is_empty")]
    pub state: Blob,
    /// The built-in's version the settings were saved from; presets from
    /// before canopy noted it have none, and count as the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl PresetFile {
    /// A preset of `device`'s settings, at the device's current version
    /// if it's a built-in effect.
    pub fn new(device: &str, params: BTreeMap<String, f32>, state: Blob) -> PresetFile {
        PresetFile {
            device: device.to_owned(),
            params,
            state,
            version: effects::find_builtin(device).map(|d| d.version.to_owned()),
        }
    }

    pub fn to_text(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())
    }

    /// Reads a preset, its settings brought up to the device's current
    /// version (see `migrate`).
    pub fn from_text(text: &str) -> Result<PresetFile, String> {
        let mut file: PresetFile = ron::from_str(text).map_err(|e| e.to_string())?;
        crate::migrate::preset_params(&file.device, file.version.as_deref(), &mut file.params);
        Ok(file)
    }
}

/// A rack preset: what makes a track's sound, its instrument (if it has
/// one) and its effects in order, without what belongs to the song
/// (automation, sidechains, modulation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<RackInstrument>,
    #[serde(default)]
    pub effects: Vec<RackEffect>,
}

/// A rack's instrument, as a MIDI track keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackInstrument {
    pub instrument: InstrumentRef,
    /// Settings the user had set, by id; the rest are at their defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginData>,
    /// The files it plays from (a sampler's .sfz, a Slicer's loop, user
    /// wavetables) and where the Slicer cuts: without them some
    /// instruments are silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Option<PathBuf>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<u64>,
}

/// One effect of a rack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackEffect {
    pub effect: EffectRef,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypassed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginData>,
    /// For a Patcher, what's inside it. Loading gives it all new ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<Box<crate::Patch>>,
}

impl RackFile {
    pub fn to_text(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())
    }

    /// Reads a rack, its effects brought up to their current versions
    /// (see `migrate`).
    pub fn from_text(text: &str) -> Result<RackFile, String> {
        let mut rack: RackFile = ron::from_str(text).map_err(|e| e.to_string())?;
        for effect in &mut rack.effects {
            crate::migrate::preset_params(
                &effect.effect.id,
                Some(&effect.effect.version),
                &mut effect.params,
            );
            crate::migrate::preset_version(&effect.effect.id, &mut effect.effect.version);
            if let Some(patch) = &mut effect.patch {
                crate::migrate::patch(patch);
            }
        }
        Ok(rack)
    }
}

impl Project {
    /// The rack of a track (or, for `None`, the master's effects); `None`
    /// if there's no such track.
    pub fn rack(&self, track: Option<TrackId>) -> Option<RackFile> {
        let instrument = match track {
            Some(id) => match &self.track(id)?.kind {
                TrackKind::Midi {
                    instrument,
                    params,
                    file,
                    tables,
                    slices,
                    plugin,
                    ..
                } => Some(RackInstrument {
                    instrument: instrument.clone(),
                    params: params.clone(),
                    plugin: plugin.clone(),
                    file: file.clone(),
                    tables: tables.clone(),
                    slices: slices.clone(),
                }),
                _ => None,
            },
            None => None,
        };
        let effects = self
            .devices(track)?
            .iter()
            .map(|d| RackEffect {
                effect: d.effect.clone(),
                params: d.params.clone(),
                bypassed: d.bypassed,
                folded: d.folded,
                plugin: d.plugin.clone(),
                patch: d.patch.clone(),
            })
            .collect();
        Some(RackFile {
            instrument,
            effects,
        })
    }

    /// Replaces a track's rack (or, for `None`, the master's effects) with
    /// `rack`: its effects take the place of the chain and its automation,
    /// and its instrument, if it has one, that of the track. A rack
    /// without an instrument keeps the track's; one with an instrument
    /// goes only on a MIDI track. A built-in effect this build doesn't
    /// have is an error; plugins load as a project's do. Returns the new
    /// effects.
    pub fn load_rack(
        &mut self,
        track: Option<TrackId>,
        rack: &RackFile,
    ) -> EditResult<Vec<DeviceId>> {
        if let Some(missing) = rack
            .effects
            .iter()
            .find(|e| e.effect.format.is_builtin() && e.effect.descriptor().is_none())
        {
            return Err(EditError::UnknownEffect(missing.effect.id.clone()));
        }
        let old: Vec<DeviceId> = match self.devices(track) {
            Some(devices) => devices.iter().map(|d| d.id).collect(),
            None => return Err(EditError::NoSuchTrack(track.unwrap_or(TrackId(0)))),
        };
        if let Some(new) = &rack.instrument {
            let Some(id) = track else {
                return Err(EditError::Invalid("the master has no instrument".into()));
            };
            self.set_instrument(id, new.instrument.clone())?;
            if let Some(TrackKind::Midi {
                params,
                file,
                tables,
                slices,
                plugin,
                ..
            }) = self.track_mut(id).map(|t| &mut t.kind)
            {
                // As stored, like a project's: a plugin's descriptor may
                // not be registered until the host loads it.
                params.clone_from(&new.params);
                plugin.clone_from(&new.plugin);
                file.clone_from(&new.file);
                tables.clone_from(&new.tables);
                *slices = crate::slicer::normalize(new.slices.clone());
            }
        }
        self.remove_devices(&old)?;
        let copied: Vec<CopiedDevice> = rack
            .effects
            .iter()
            .map(|e| CopiedDevice {
                device: Device {
                    // `insert_devices` gives each its own.
                    id: DeviceId(0),
                    effect: e.effect.clone(),
                    params: e.params.clone(),
                    bypassed: e.bypassed,
                    sidechain: None,
                    folded: e.folded,
                    plugin: e.plugin.clone(),
                    patch: e.patch.clone(),
                },
                automation: Vec::new(),
                routes: Vec::new(),
            })
            .collect();
        self.insert_devices(track, None, &copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutomationPoint, AutomationTarget};

    fn point() -> AutomationPoint {
        AutomationPoint {
            tick: midi::Tick::ZERO,
            value: 0.5,
            curve: 0.0,
        }
    }

    /// A MIDI track playing the Slicer, its cutoff set, through a bypassed
    /// EQ and an automated reverb.
    fn source(p: &mut Project) -> TrackId {
        let track = p.track_ids()[0];
        p.set_instrument(track, InstrumentRef::builtin("canopy.slicer"))
            .unwrap();
        p.set_instrument_file(track, Some("loops/break.wav".into()))
            .unwrap();
        p.set_slices(track, vec![0, 4800, 9600]).unwrap();
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        p.set_device_bypassed(eq, true).unwrap();
        let reverb = p
            .add_device(Some(track), EffectRef::builtin("canopy.reverb"), None)
            .unwrap();
        p.set_device_param(reverb, "dry", 40.0).unwrap();
        let target = AutomationTarget::DeviceParam {
            device: reverb,
            param: "dry".into(),
        };
        p.set_automation(track, target, vec![point()]).unwrap();
        track
    }

    fn names(p: &Project, track: Option<TrackId>) -> Vec<&'static str> {
        p.devices(track)
            .unwrap()
            .iter()
            .map(|d| d.effect.descriptor().unwrap().id)
            .collect()
    }

    #[test]
    fn a_rack_moves_its_instrument_and_effects_to_another_track() {
        let mut p = Project::with_default_tracks();
        let from = source(&mut p);
        let rack = RackFile::from_text(&p.rack(Some(from)).unwrap().to_text().unwrap()).unwrap();
        assert_eq!(Some(&rack), p.rack(Some(from)).as_ref());

        let to = p.track_ids()[1];
        let old = p
            .add_device(Some(to), EffectRef::builtin("canopy.delay"), None)
            .unwrap();
        let target = AutomationTarget::DeviceParam {
            device: old,
            param: "dry".into(),
        };
        p.set_automation(to, target, vec![point()]).unwrap();
        let ids = p.load_rack(Some(to), &rack).unwrap();

        assert_eq!(names(&p, Some(to)), ["canopy.eq", "canopy.reverb"]);
        let devices = p.devices(Some(to)).unwrap();
        assert_eq!(devices.iter().map(|d| d.id).collect::<Vec<_>>(), ids);
        assert!(devices[0].bypassed);
        assert_eq!(devices[1].param("dry"), Some(40.0));
        let TrackKind::Midi {
            instrument,
            file,
            slices,
            ..
        } = &p.track(to).unwrap().kind
        else {
            panic!("still a MIDI track");
        };
        assert_eq!(instrument.id, "canopy.slicer");
        assert_eq!(
            file.as_deref(),
            Some(std::path::Path::new("loops/break.wav"))
        );
        assert_eq!(slices, &[0, 4800, 9600]);
        // The song's parts stay: none of the source's automation comes
        // along, and the replaced delay's goes with it.
        assert!(p.track(to).unwrap().automation.is_empty());
        assert_eq!(p.track(from).unwrap().automation.len(), 1);
    }

    #[test]
    fn a_rack_of_effects_keeps_the_tracks_instrument() {
        let mut p = Project::with_default_tracks();
        let from = source(&mut p);
        let mut rack = p.rack(Some(from)).unwrap();
        rack.instrument = None;
        let to = p.track_ids()[1];
        p.load_rack(Some(to), &rack).unwrap();
        let TrackKind::Midi { instrument, .. } = &p.track(to).unwrap().kind else {
            panic!("still a MIDI track");
        };
        assert_eq!(instrument.id, instruments::DEFAULT_INSTRUMENT);
        assert_eq!(names(&p, Some(to)), ["canopy.eq", "canopy.reverb"]);
        p.load_rack(None, &rack).unwrap();
        assert_eq!(names(&p, None), ["canopy.eq", "canopy.reverb"]);
    }

    #[test]
    fn an_instruments_rack_needs_a_midi_track() {
        let mut p = Project::with_default_tracks();
        let from = source(&mut p);
        let rack = p.rack(Some(from)).unwrap();
        let audio = p.track_ids()[2];
        let before = p.clone();
        assert!(p.load_rack(Some(audio), &rack).is_err());
        assert!(p.load_rack(None, &rack).is_err());
        assert_eq!(p, before);
    }

    #[test]
    fn a_rack_with_an_unknown_effect_changes_nothing() {
        let mut p = Project::with_default_tracks();
        let from = source(&mut p);
        let mut rack = p.rack(Some(from)).unwrap();
        rack.effects[0].effect = EffectRef::builtin("canopy.no_such_effect");
        let before = p.clone();
        assert!(p.load_rack(Some(p.track_ids()[1]), &rack).is_err());
        assert_eq!(p, before);
    }
}
