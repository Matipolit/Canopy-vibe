//! A plugin as canopy describes its devices: a leaked `&'static`
//! descriptor (what the UI, automation, presets and the engine read), and
//! the map from each descriptor parameter to the plugin's own.
//!
//! # Values
//!
//! A parameter's value in the project is the plugin's own value in its
//! format's host-side units (CLAP plain values, VST3 and VST2 normalized
//! ones), except for parameters the plugin enumerates (a list of choices):
//! those are shown as choices, so the project keeps the index of the
//! choice, as it does for built-ins' enum parameters. [`ParamMap`] turns one
//! into the other. The mapping is part of the file format: a parameter
//! stays a choice or a number for as long as the plugin describes it so.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use effects::{Category, EffectDescriptor};
use instruments::{BuiltInInstrumentDescriptor, DataKind, Format, ParamDescriptor, ParameterKind};
use plugins::{ParamId, ParamInfo, PluginFormat, PluginInfo, PluginInstance, PluginKind};

/// The most choices a parameter may have to be shown as a list.
const MAX_CHOICES: u32 = 128;

/// Plugins with more parameters than this get them grouped in menus.
const GROUPED: usize = 24;

/// A group needs this many parameters to be a submenu of its own.
const GROUP_SIZE: usize = 3;

/// How one descriptor parameter maps onto the plugin's parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamMap {
    pub id: ParamId,
    /// For a list of choices: the plugin's first and last values and how
    /// many steps lie between them. Canopy's value is the step's index.
    pub steps: Option<Steps>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Steps {
    pub min: f64,
    pub max: f64,
    pub count: u32,
}

impl ParamMap {
    /// The plugin's value for canopy's `value`.
    pub fn to_plugin(&self, value: f32) -> f64 {
        match self.steps {
            None => f64::from(value),
            Some(Steps { min, max, count }) => {
                let index = f64::from(value).round().clamp(0.0, f64::from(count));
                min + (max - min) * index / f64::from(count.max(1))
            }
        }
    }

    /// Canopy's value for the plugin's `value`.
    pub fn from_plugin(&self, value: f64) -> f32 {
        match self.steps {
            None => value as f32,
            Some(Steps { min, max, count }) => {
                if max == min {
                    return 0.0;
                }
                let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
                (t * f64::from(count)).round() as f32
            }
        }
    }
}

/// A plugin's registered description, shared by its instances.
#[derive(Debug)]
pub struct Described {
    pub info: PluginInfo,
    /// By descriptor index.
    pub params: Arc<[ParamMap]>,
    /// The same parameters as canopy describes them.
    pub descriptors: &'static [ParamDescriptor],
}

impl Described {
    /// The descriptor index of the plugin's parameter `id`.
    pub fn index_of(&self, id: ParamId) -> Option<usize> {
        self.params.iter().position(|p| p.id == id)
    }

    /// Canopy's id of the plugin's parameter `id` (its number, in decimal).
    pub fn param_key(&self, id: ParamId) -> Option<&'static str> {
        self.index_of(id).map(|i| self.descriptors[i].id)
    }
}

/// The format canopy's project names for a plugin format.
pub fn format(format: PluginFormat) -> Format {
    match format {
        PluginFormat::Clap => Format::Clap,
        PluginFormat::Vst3 => Format::Vst3,
        PluginFormat::Vst2 => Format::Vst2,
    }
}

/// The plugin format of a project's format; `None` for built-ins.
pub fn plugin_format(format: Format) -> Option<PluginFormat> {
    match format {
        Format::BuiltIn => None,
        Format::Clap => Some(PluginFormat::Clap),
        Format::Vst3 => Some(PluginFormat::Vst3),
        Format::Vst2 => Some(PluginFormat::Vst2),
    }
}

/// Reads `instance`'s parameters and registers its description, as an
/// instrument or an effect by its kind, replacing an earlier one.
pub fn register(instance: &PluginInstance) -> Described {
    let info = instance.info().clone();
    let shown: Vec<ParamInfo> = instance
        .params()
        .into_iter()
        .filter(|p| !p.flags.hidden && !p.flags.read_only)
        .collect();
    let mut maps = Vec::new();
    let mut descriptors = Vec::new();
    for param in &shown {
        let (map, descriptor) = describe(param, |value| instance.param_text(param.id, value));
        maps.push(map);
        descriptors.push(descriptor);
    }
    let descriptors: &'static [ParamDescriptor] = Box::leak(descriptors.into_boxed_slice());
    GROUPS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_default()
        .insert((format(info.format), info.id.clone()), groups_of(&shown));
    let id = leak(&info.id);
    let name = leak(&info.name);
    let description = leak(&format!("{} · {}", info.vendor, info.format.name()));
    let version = leak(&info.version);
    match info.kind {
        PluginKind::Instrument => {
            instruments::register_external(
                format(info.format),
                &info.id,
                BuiltInInstrumentDescriptor {
                    id,
                    name,
                    description,
                    version,
                    author: leak(&info.vendor),
                    params: descriptors,
                    envelopes: &[],
                    create: || Box::new(crate::adapter::Silence),
                    data: DataKind::None,
                    groups: instruments::no_groups,
                },
            );
        }
        PluginKind::Effect | PluginKind::NoteEffect => {
            effects::register_external(
                format(info.format),
                &info.id,
                EffectDescriptor {
                    id,
                    name,
                    description,
                    version,
                    category: category(&info.categories),
                    params: descriptors,
                    sidechain: info.aux_input().is_some(),
                    create: || Box::new(crate::adapter::Passthrough),
                    ..EffectDescriptor::BASE
                },
            );
        }
    }
    Described {
        info,
        params: maps.into(),
        descriptors,
    }
}

/// One parameter as canopy describes it. `text` is how the plugin shows a
/// value, for naming choices.
fn describe(
    param: &ParamInfo,
    text: impl Fn(f64) -> Option<String>,
) -> (ParamMap, ParamDescriptor) {
    let id = leak(&param.id.0.to_string());
    let name = leak(if param.name.is_empty() {
        id
    } else {
        &param.name
    });
    let choices = param.step_count > 0
        && param.step_count < MAX_CHOICES
        && (param.flags.enumerated || param.step_count == 1);
    if choices {
        let steps = Steps {
            min: param.min,
            max: param.max,
            count: param.step_count,
        };
        let map = ParamMap {
            id: param.id,
            steps: Some(steps),
        };
        let labels: Vec<&'static str> = (0..=param.step_count)
            .map(|i| {
                let value = param.step_value(i);
                let label = text(value).filter(|t| !t.trim().is_empty());
                leak(label.as_deref().unwrap_or(&format_number(value)))
            })
            .collect();
        let default = map.from_plugin(param.default) as usize;
        let descriptor = ParamDescriptor {
            id,
            name,
            kind: ParameterKind::Enum {
                labels: Box::leak(labels.into_boxed_slice()),
                default,
            },
        };
        (map, descriptor)
    } else {
        let map = ParamMap {
            id: param.id,
            steps: None,
        };
        let (min, max) = (param.min.min(param.max), param.max.max(param.min));
        let descriptor = ParamDescriptor {
            id,
            name,
            kind: ParameterKind::Float {
                min: min as f32,
                max: max as f32,
                default: param.default.clamp(min, max) as f32,
                unit: leak(&param.unit),
                logarithmic: false,
            },
        };
        (map, descriptor)
    }
}

/// Where each registered plugin's parameters sit in menus that list them
/// all (automation's), by descriptor index: up to two levels of submenus,
/// outermost first, "" for none. Built-ins say this through their
/// descriptors' `groups`; a plugin's can't, being a function without the
/// plugin to ask.
static GROUPS: RwLock<Option<HashMap<(Format, String), Groups>>> = RwLock::new(None);

/// Each parameter's two levels of menu groups, by descriptor index.
type Groups = &'static [[&'static str; 2]];

/// The menu groups of registered plugin `id`'s parameters, by descriptor
/// index; `None` for built-ins and plugins not registered.
pub fn param_groups(format: Format, id: &str) -> Option<Groups> {
    GROUPS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()?
        .get(&(format, id.to_owned()))
        .copied()
}

/// Groups for a plugin's parameters: the plugin's own (CLAP modules, VST3
/// units), or else the words their names begin with, as most plugins name
/// them ("Oscillator 2 Unison Detune" is in Oscillator ▸ Oscillator 2).
/// Groups too small to be worth a submenu are left out, and plugins with
/// few parameters get none.
fn groups_of(params: &[ParamInfo]) -> Groups {
    if params.len() <= GROUPED {
        return Box::leak(vec![["", ""]; params.len()].into_boxed_slice());
    }
    let named: Vec<[String; 2]> = params
        .iter()
        .map(|p| {
            if !p.module.is_empty() {
                let mut levels = p.module.split('/').map(str::trim);
                let outer = levels.next().unwrap_or_default().to_owned();
                let inner = levels.next().unwrap_or_default().to_owned();
                return [outer, inner];
            }
            let mut words = p.name.split_whitespace();
            let first = words.next().unwrap_or_default();
            match words.next() {
                Some(number) if number.chars().all(|c| c.is_ascii_digit()) => {
                    [first.to_owned(), format!("{first} {number}")]
                }
                Some(_) => [first.to_owned(), String::new()],
                None => [String::new(), String::new()],
            }
        })
        .collect();
    let mut count = [HashMap::new(), HashMap::new()];
    for groups in &named {
        for level in 0..2 {
            *count[level].entry(&groups[level]).or_insert(0usize) += 1;
        }
    }
    let mut interned: HashMap<String, &'static str> = HashMap::new();
    let mut intern = |text: &str| -> &'static str {
        if text.is_empty() {
            return "";
        }
        interned
            .entry(text.to_owned())
            .or_insert_with(|| leak(text))
    };
    let groups: Vec<[&'static str; 2]> = named
        .iter()
        .map(|[outer, inner]| {
            if count[0][outer] < GROUP_SIZE {
                return ["", ""];
            }
            let inner = if count[1][inner] < 2 {
                ""
            } else {
                inner.as_str()
            };
            [intern(outer), intern(inner)]
        })
        .collect();
    Box::leak(groups.into_boxed_slice())
}

/// Where the effect would sit among the built-ins, from the categories
/// the plugin names (CLAP features, VST3 subcategories).
fn category(categories: &[String]) -> Category {
    let names: Vec<String> = categories.iter().map(|c| c.to_lowercase()).collect();
    let any = |words: &[&str]| names.iter().any(|n| words.iter().any(|w| n.contains(w)));
    if any(&["analy", "meter", "scope"]) {
        Category::Analysis
    } else if any(&["eq", "filter"]) {
        Category::Eq
    } else if any(&[
        "dynamic",
        "compress",
        "limit",
        "gate",
        "expand",
        "transient",
    ]) {
        Category::Dynamics
    } else if any(&["distort", "saturat", "crush", "amp"]) {
        Category::Distortion
    } else if any(&["reverb", "delay", "spatial", "room"]) {
        Category::Space
    } else {
        Category::Modulation
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    }
}

/// Descriptors are `&'static` everywhere, so their strings are leaked: as
/// many as plugins registered, and registered again when they change.
fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

#[cfg(test)]
mod tests {
    use plugins::ParamFlags;

    use super::*;

    fn param(min: f64, max: f64, steps: u32, enumerated: bool) -> ParamInfo {
        ParamInfo {
            id: ParamId(42),
            name: "Mode".into(),
            module: String::new(),
            unit: "dB".into(),
            min,
            max,
            default: max,
            step_count: steps,
            flags: ParamFlags {
                stepped: steps > 0,
                enumerated,
                automatable: true,
                ..ParamFlags::default()
            },
        }
    }

    #[test]
    fn continuous_parameters_keep_the_plugins_own_values() {
        let (map, descriptor) = describe(&param(-60.0, 12.0, 0, false), |_| None);
        assert_eq!(descriptor.id, "42");
        assert_eq!(map.to_plugin(-6.0), -6.0);
        assert_eq!(map.from_plugin(-6.0), -6.0);
        let ParameterKind::Float { min, max, unit, .. } = descriptor.kind else {
            panic!("a float");
        };
        assert_eq!((min, max, unit), (-60.0, 12.0, "dB"));
    }

    #[test]
    fn enumerated_parameters_become_choices_named_by_the_plugin() {
        // A VST3 list: normalized, three steps.
        let names = ["Saw", "Square", "Sine", "Noise"];
        let (map, descriptor) = describe(&param(0.0, 1.0, 3, true), |v| {
            Some(names[(v * 3.0).round() as usize].to_owned())
        });
        let ParameterKind::Enum { labels, default } = descriptor.kind else {
            panic!("a choice");
        };
        assert_eq!(labels, names);
        assert_eq!(default, 3);
        assert!((map.to_plugin(1.0) - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(map.from_plugin(2.0 / 3.0), 2.0);
        assert_eq!(
            map.from_plugin(0.7),
            2.0,
            "between steps rounds to the nearest"
        );
    }

    #[test]
    fn stepped_numbers_stay_numbers_but_switches_are_choices() {
        let (_, semitones) = describe(&param(-24.0, 24.0, 48, false), |_| None);
        assert!(matches!(semitones.kind, ParameterKind::Float { .. }));
        let (map, switch) = describe(&param(0.0, 1.0, 1, false), |_| None);
        let ParameterKind::Enum { labels, .. } = switch.kind else {
            panic!("a choice");
        };
        assert_eq!(labels, ["0", "1"], "numbers when the plugin names nothing");
        assert_eq!(map.to_plugin(1.0), 1.0);
    }

    #[test]
    fn big_plugins_group_their_parameters_by_the_words_they_begin_with() {
        let named = |name: &str, module: &str| ParamInfo {
            name: name.into(),
            module: module.into(),
            ..param(0.0, 1.0, 0, false)
        };
        let mut params = Vec::new();
        for osc in 1..=3 {
            for what in ["Level", "Pan", "Unison Detune"] {
                params.push(named(&format!("Oscillator {osc} {what}"), ""));
            }
        }
        for what in ["Attack", "Release", "Ratio"] {
            params.push(named(what, "Dynamics/Compressor"));
        }
        params.push(named("Beats Per Minute", ""));
        while params.len() <= GROUPED {
            params.push(named(&format!("Macro {}", params.len()), ""));
        }
        let groups = groups_of(&params);
        assert_eq!(groups[0], ["Oscillator", "Oscillator 1"]);
        assert_eq!(groups[8], ["Oscillator", "Oscillator 3"]);
        assert_eq!(
            groups[9],
            ["Dynamics", "Compressor"],
            "the plugin's own groups win"
        );
        assert_eq!(groups[12], ["", ""], "too small a group to be one");
        assert_eq!(groups[13][1], "", "one macro per number isn't a group");
        // Few parameters: no groups at all.
        assert!(groups_of(&params[..4]).iter().all(|g| *g == ["", ""]));
    }

    #[test]
    fn categories_place_effects_among_the_built_ins() {
        let of =
            |names: &[&str]| category(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(of(&["audio-effect", "equalizer"]), Category::Eq);
        assert_eq!(of(&["Fx", "Dynamics"]), Category::Dynamics);
        assert_eq!(of(&["Fx", "Reverb"]), Category::Space);
        assert_eq!(of(&["Fx"]), Category::Modulation);
    }
}
