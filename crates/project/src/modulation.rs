//! Modulation: sources that move parameters continuously, on top of where
//! their knobs (or automation) put them.
//!
//! It's track-local. A track has standalone modulators
//! (`Track::modulators`: LFOs, envelopes, followers, macros, random, MIDI),
//! and routes (`Track::modulations`), each from a source (one of those
//! modulators, or a modulator the track's instrument exports, like the
//! Wavetable's "lfo1") to a target on the same track. Targets are
//! [`AutomationTarget`]s, the addresses automation and controller links
//! use: volume, pan, an instrument or effect parameter, or another
//! modulator's parameter.
//!
//! The engine adds each route's contribution to the parameter's base value
//! (its knob, or its automation while playing) on the `0..=1` scale
//! automation uses: `amount` is a share of the parameter's range, signed.
//! Modulators' own descriptions live in the `modulators` crate; here
//! they're plain data, like effects are `EffectRef`s.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AutomationTarget, ModRouteId, ModulatorId, PortRef, Track, TrackId, TrackKind};

/// A standalone modulator on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Modulator {
    pub id: ModulatorId,
    /// Which modulator it is ("canopy.lfo").
    pub kind: ModulatorRef,
    /// Parameters the user has set, by id. The rest are at their defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// A name the user gave it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What a follower listens to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<Listen>,
    /// Shown as just its name. A view setting: not an undo step.
    #[serde(default)]
    pub folded: bool,
}

/// Which modulator, as stored in a project file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModulatorRef {
    pub id: String,
    pub version: String,
}

/// The sound a follower measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Listen {
    /// The track's own sound before its effects.
    Input,
    /// An output port in the track's graph.
    Node(PortRef),
    /// Another track's sound after its effects, before its fader (as a
    /// sidechain hears it).
    Track(TrackId),
}

/// Where a route's signal comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModSource {
    /// Output `output` of one of the track's modulators.
    Modulator {
        id: ModulatorId,
        #[serde(default)]
        output: u8,
    },
    /// A modulator the track's instrument exports, by its id ("lfo1"; see
    /// `instruments::ModOutput`).
    Instrument(String),
}

/// How a source's signal is read, as in the Wavetable's matrix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Polarity {
    /// As the source is: `0..=1` or `-1..=1`.
    #[default]
    Source,
    /// Mapped onto `0..=1`.
    Unipolar,
    /// Mapped onto `-1..=1`.
    Bipolar,
}

/// A source moving a target, by a signed share of its range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModRoute {
    pub id: ModRouteId,
    pub source: ModSource,
    pub target: AutomationTarget,
    /// `-1..=1` of the target's normalized range.
    pub amount: f32,
    #[serde(default)]
    pub polarity: Polarity,
    /// `-1..=1`: how the source's signal bends before it's scaled, as the
    /// Wavetable matrix's curve.
    #[serde(default)]
    pub curve: f32,
    /// Another source the amount is multiplied by (Serum's "aux").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_by: Option<ModSource>,
    #[serde(default)]
    pub bypassed: bool,
}

impl crate::Project {
    /// Every modulator's fold, for carrying it over undo and redo.
    pub fn modulator_folds(&self) -> Vec<(ModulatorId, bool)> {
        let mut folds = Vec::new();
        self.visit(|track, _| folds.extend(track.modulators.iter().map(|m| (m.id, m.folded))));
        folds
    }

    /// Shows a modulator as just its name, or in full. A view setting: not
    /// an undo step.
    pub fn set_modulator_folded(
        &mut self,
        id: ModulatorId,
        folded: bool,
    ) -> Result<(), crate::EditError> {
        let mut found = false;
        self.visit_mut(|track| {
            if let Some(m) = track.modulators.iter_mut().find(|m| m.id == id) {
                m.folded = folded;
                found = true;
            }
        });
        if found {
            Ok(())
        } else {
            Err(crate::EditError::NoSuchModulator(id))
        }
    }
}

impl Track {
    pub fn modulator(&self, id: ModulatorId) -> Option<&Modulator> {
        self.modulators.iter().find(|m| m.id == id)
    }

    /// Whether `source` exists on this track: one of its modulators, or
    /// (on a MIDI track) its instrument. An instrument's output that turns
    /// out not to exist is the engine's to skip, so a route survives the
    /// instrument going missing for a while.
    pub fn has_source(&self, source: &ModSource) -> bool {
        match source {
            ModSource::Modulator { id, .. } => self.modulator(*id).is_some(),
            ModSource::Instrument(_) => matches!(self.kind, TrackKind::Midi { .. }),
        }
    }

    /// Whether `target` is something on this track a route can move.
    pub fn has_target(&self, target: &AutomationTarget) -> bool {
        match target {
            AutomationTarget::Volume | AutomationTarget::Pan => true,
            AutomationTarget::InstrumentParam(_) => matches!(self.kind, TrackKind::Midi { .. }),
            AutomationTarget::DeviceParam { device, .. } => {
                self.devices.iter().any(|d| d.id == *device)
            }
            AutomationTarget::ModulatorParam { modulator, .. } => {
                self.modulator(*modulator).is_some()
            }
        }
    }

    /// Drops routes whose source or target isn't on the track any more,
    /// and `scale_by`s that aren't; followers listening to a graph port
    /// that's gone listen to nothing. After any edit that removes devices,
    /// modulators, graph nodes or the instrument.
    pub(crate) fn prune_modulation(&mut self) {
        let mut routes = std::mem::take(&mut self.modulations);
        routes.retain_mut(|route| {
            if route.scale_by.as_ref().is_some_and(|s| !self.has_source(s)) {
                route.scale_by = None;
            }
            self.has_source(&route.source) && self.has_target(&route.target)
        });
        self.modulations = routes;
        let routing = self.routing.as_ref();
        for modulator in &mut self.modulators {
            if let Some(Listen::Node(port)) = modulator.listen
                && routing.is_none_or(|r| r.node(port.node).is_none())
            {
                modulator.listen = None;
            }
        }
    }
}
