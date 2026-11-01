//! Tools for the sound: a track's instrument and the file it plays, any
//! device's parameters, effects added, removed, moved, bypassed and
//! sidechained, presets, the Slicer's slices, and automation.

use std::path::{Path, PathBuf};

use instruments::{DataKind, Format, InstrumentRef, ParamDescriptor};
use project::{
    AutomationPoint, AutomationTarget, DeviceId, EditError, EffectRef, Generate, Project, TrackId,
    TrackKind, Wave,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::clips::{apply, track_of};
use super::{Kind, Outcome, Output, Plan, Tool, ToolError, Waiter, args, schema};
use crate::host::{Host, SlicerOp, Target};
use crate::units::{
    Units, beats_to_offset, beats_to_tick, describe_value, param_value, parse_target, target_json,
    tick_to_beats,
};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "set_instrument",
        description: "Sets what a MIDI track plays: a built-in instrument by id or name \
                      (list_devices lists them), with `params` in their own units, as one undo \
                      step. A track already playing it keeps its settings, changing only the \
                      `params` given. `file` gives the Sampler an .sfz or the Slicer an audio \
                      loop (its full path): the Slicer's answer comes once the loop has loaded \
                      and been sliced at its transients.",
        schema: set_instrument_schema,
        examples: &[
            r#"{"track": 12, "instrument": "canopy.wavetable"}"#,
            r#"{"track": 12, "instrument": "canopy.poly_synth", "params": {"cutoff": 1200, "wave": "Saw"}}"#,
            r#"{"track": 13, "instrument": "canopy.slicer", "file": "/home/me/loops/amen.wav"}"#,
        ],
        kind: Kind::Session(set_instrument),
    },
    Tool {
        name: "set_params",
        description: "Sets parameters of an effect (`device`) or a track's instrument \
                      (`track`), many at once, as one undo step. `values` maps each parameter's \
                      id (or name) to a value in its own units, or a choice's label; get_device \
                      lists them with their ranges. One value out of range rejects the whole \
                      call.",
        schema: set_params_schema,
        examples: &[
            r#"{"device": 40, "values": {"b3_freq": 6500, "b3_gain": -4, "b3_dyn": "On"}}"#,
            r#"{"track": 12, "values": {"Filter cutoff": 800}}"#,
        ],
        kind: Kind::Edit(set_params),
    },
    Tool {
        name: "add_effect",
        description: "Adds a built-in effect (by id or name; list_devices lists them) to a \
                      track's chain or, with `master`, the master's: at `position` (0 is first; \
                      the end if left out), with `params` in their own units, and the track its \
                      `sidechain` listens to if it has one. One undo step; returns the effect's \
                      id.",
        schema: add_effect_schema,
        examples: &[
            r#"{"track": 12, "effect": "canopy.eq", "params": {"b5_freq": 7000, "b5_dyn": "On", "b5_threshold": -30}}"#,
            r#"{"master": true, "effect": "canopy.limiter"}"#,
            r#"{"track": 14, "effect": "canopy.ducker", "sidechain": 9, "position": 0}"#,
        ],
        kind: Kind::Edit(add_effect),
    },
    Tool {
        name: "edit_effects",
        description: "Changes effects in chains, one undo step: `remove` them; `move` them to \
                      `position` in their chain, or in `track`'s or (`master`) the master's; \
                      `bypass` them (`on`: true bypasses, false brings them back); `sidechain`: \
                      the track they listen to (`track`, another track), or none without it.",
        schema: edit_effects_schema,
        examples: &[
            r#"{"op": "remove", "devices": [40]}"#,
            r#"{"op": "move", "devices": [41], "position": 0}"#,
            r#"{"op": "move", "devices": [40, 41], "master": true}"#,
            r#"{"op": "bypass", "devices": [40], "on": true}"#,
            r#"{"op": "sidechain", "devices": [42], "track": 9}"#,
        ],
        kind: Kind::Edit(edit_effects),
    },
    Tool {
        name: "load_preset",
        description: "Loads a preset, by name (any case), into an effect (`device`) or a \
                      track's instrument (`track`), as one undo step. list_presets lists them.",
        schema: load_preset_schema,
        examples: &[
            r#"{"device": 40, "preset": "Small room"}"#,
            r#"{"track": 12, "preset": "Supersaw lead"}"#,
        ],
        kind: Kind::Session(load_preset),
    },
    Tool {
        name: "slicer",
        description: "A Slicer track's slices (where in its loop each starts, in seconds, and \
                      the key that plays it). `get` reads them; `set` puts them at `slices` \
                      (seconds); `transients` slices at the loop's hits (`sensitivity` 0–1, \
                      higher finds more); `grid` cuts it `division` to a bar (1–64) at its own \
                      tempo; `to_piano_roll` writes a MIDI clip at `at` (beats) that plays every \
                      slice in order. The track needs its loop first: set_instrument with a \
                      `file`.",
        schema: slicer_schema,
        examples: &[
            r#"{"track": 13, "op": "get"}"#,
            r#"{"track": 13, "op": "transients", "sensitivity": 0.6}"#,
            r#"{"track": 13, "op": "grid", "division": 16}"#,
            r#"{"track": 13, "op": "set", "slices": [0, 0.25, 0.5, 0.875]}"#,
            r#"{"track": 13, "op": "to_piano_roll", "at": 16}"#,
        ],
        kind: Kind::Session(slicer),
    },
    Tool {
        name: "set_automation",
        description: "Writes one automation lane of a track: `points` (each `at` in beats, a \
                      `value` in the target's own units and a `curve` -1–1) replace the lane; \
                      `generate` fills `from`–`to` with a shape between `low` and `high` (in the \
                      target's units) repeating every `period` beats, replacing only the points \
                      there; `clear` removes the lane. One undo step.",
        schema: set_automation_schema,
        examples: &[
            r#"{"track": 12, "target": "volume", "points": [{"at": 0, "value": -12}, {"at": 16, "value": 0, "curve": 0.3}]}"#,
            r#"{"track": 12, "target": {"device": 40, "param": "b3_freq"}, "generate": {"wave": "sine", "from": 0, "to": 32, "period": 4, "low": 300, "high": 3000}}"#,
            r#"{"track": 12, "target": "pan", "clear": true}"#,
        ],
        kind: Kind::Edit(set_automation),
    },
];

/// A parameter's new value, checked: its descriptor and the value in its
/// own units.
type Checked = (&'static ParamDescriptor, f32);

/// Finds each of `values`' parameters in `params` (by id, then by name,
/// any case) and checks its value. Every problem is reported at once; any
/// one rejects the lot.
fn check_values(
    params: impl Iterator<Item = &'static ParamDescriptor> + Clone,
    values: &Map<String, Value>,
    owner: &str,
) -> Result<Vec<Checked>, ToolError> {
    let mut checked = Vec::with_capacity(values.len());
    let mut problems = Vec::new();
    for (key, value) in values {
        match find(params.clone(), key, owner).and_then(|d| Ok((d, param_value(d, value)?))) {
            Ok(found) => checked.push(found),
            Err(problem) => problems.push(problem),
        }
    }
    if !problems.is_empty() {
        return Err(ToolError(format!(
            "{}; nothing was changed",
            problems.join("; ")
        )));
    }
    for (i, (d, _)) in checked.iter().enumerate() {
        if checked[..i].iter().any(|(other, _)| other.id == d.id) {
            return Err(ToolError(format!(
                "{} is given twice; nothing was changed",
                d.id
            )));
        }
    }
    Ok(checked)
}

/// Parameter `key` of `owner`: by id, else by name, any case.
fn find(
    mut params: impl Iterator<Item = &'static ParamDescriptor> + Clone,
    key: &str,
    owner: &str,
) -> Result<&'static ParamDescriptor, String> {
    if let Some(d) = params.clone().find(|d| d.id == key) {
        return Ok(d);
    }
    let named: Vec<&'static ParamDescriptor> = params
        .clone()
        .filter(|d| d.id.eq_ignore_ascii_case(key) || d.name.eq_ignore_ascii_case(key))
        .collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "{owner} has no parameter {key:?}; get_device lists its parameters{}",
            if params.any(|d| d.id.contains(&key.to_lowercase())) {
                " (pass `filter` to it)"
            } else {
                ""
            }
        )),
        several => Err(format!(
            "{owner} has several parameters called {key:?}; use an id: {}",
            several.iter().map(|d| d.id).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// "freq = 2000 Hz, mode = \"Notch\"", for summaries.
fn values_text(values: &[Checked]) -> String {
    const SHOWN: usize = 8;
    let mut parts: Vec<String> = values
        .iter()
        .take(SHOWN)
        .map(|(d, v)| {
            let unit = match d.kind {
                instruments::ParameterKind::Float { unit, .. } if !unit.is_empty() => {
                    format!(" {unit}")
                }
                _ => String::new(),
            };
            format!("{} = {}{unit}", d.id, describe_value(d, *v))
        })
        .collect();
    if values.len() > SHOWN {
        parts.push(format!("and {} more", values.len() - SHOWN));
    }
    parts.join(", ")
}

/// Each value as reads show it, by id.
fn values_json(values: &[Checked]) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(d, v)| (d.id.to_owned(), describe_value(d, *v)))
            .collect(),
    )
}

fn values_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": { "type": ["number", "string", "boolean"] },
        "description": description,
    })
}

/// A built-in instrument by id or name.
fn find_instrument(
    name: &str,
) -> Result<&'static instruments::BuiltInInstrumentDescriptor, ToolError> {
    instruments::find_builtin(name)
        .or_else(|| {
            instruments::builtins()
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(name) || d.id.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| {
            ToolError(format!(
                "there's no built-in instrument {name:?}; there are: {} (plugins can't be put on \
                 tracks from here yet)",
                instruments::builtins()
                    .iter()
                    .map(|d| d.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// A built-in effect by id or name.
fn find_effect(name: &str) -> Result<&'static effects::EffectDescriptor, ToolError> {
    effects::find_builtin(name)
        .or_else(|| {
            effects::builtins()
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(name) || d.id.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| {
            ToolError(format!(
                "there's no built-in effect {name:?}; list_devices lists them (plugins can't be \
                 added from here yet)"
            ))
        })
}

/// The MIDI track `track`, or why it isn't one.
fn midi_track(project: &Project, track: TrackId) -> Result<&project::Track, ToolError> {
    let t = track_of(project, track)?;
    if matches!(t.kind, TrackKind::Midi { .. }) {
        Ok(t)
    } else {
        Err(ToolError(format!(
            "track {track} ({:?}) is {}, without an instrument; {}",
            t.name,
            if t.is_group() {
                "a group"
            } else {
                "an audio track"
            },
            super::explain(&EditError::WrongTrackKind(track), project)
        )))
    }
}

fn device_of(project: &Project, id: DeviceId) -> Result<&project::Device, ToolError> {
    project
        .device(id)
        .ok_or_else(|| ToolError(super::explain(&EditError::NoSuchDevice(id), project)))
}

/// An effect's (`device`) or a track's instrument's (`track`), checked to
/// exist; exactly one of them.
fn target(
    project: &Project,
    device: Option<DeviceId>,
    track: Option<TrackId>,
) -> Result<Target, ToolError> {
    match (device, track) {
        (Some(device), None) => {
            device_of(project, device)?;
            Ok(Target::Device(device))
        }
        (None, Some(track)) => {
            midi_track(project, track)?;
            Ok(Target::Instrument(track))
        }
        _ => Err("give `device` (an effect) or `track` (its instrument), one of them".into()),
    }
}

// set_instrument

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetInstrumentArgs {
    track: TrackId,
    instrument: String,
    file: Option<String>,
    params: Option<Map<String, Value>>,
}

fn set_instrument_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("A MIDI track."),
            "instrument": schema::string("A built-in instrument's id or name: \"canopy.wavetable\", \"Sampler\"."),
            "file": schema::string("The Sampler's .sfz or the Slicer's audio loop, as a full path."),
            "params": values_schema("Parameters to set, by id or name, in their own units."),
        }),
        &["track", "instrument"],
    )
}

fn set_instrument(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: SetInstrumentArgs = args(v)?;
    let project = host.project();
    let track = midi_track(project, a.track)?;
    let descriptor = find_instrument(&a.instrument)?;
    let values = check_values(
        descriptor.params.iter(),
        &a.params.unwrap_or_default(),
        descriptor.name,
    )?;
    let file = a.file.map(PathBuf::from);
    if let Some(file) = &file {
        check_instrument_file(descriptor, file)?;
    }
    let reference = InstrumentRef::builtin(descriptor.id);
    let same = matches!(&track.kind, TrackKind::Midi { instrument, .. }
        if instrument.format == Format::BuiltIn && instrument.id == reference.id);
    let id = a.track;
    let name = descriptor.name;
    let mut summary = if same {
        format!("track {id} already plays the {name}")
    } else {
        format!("track {id} plays the {name}")
    };
    if !values.is_empty() {
        summary.push_str(&format!("; set {}", values_text(&values)));
    }
    let mut out = json!({ "track": id.0, "instrument": descriptor.id, "summary": summary });
    if !same || !values.is_empty() {
        let changed = values_json(&values);
        let plan = Plan {
            label: format!("Set Instrument: {name}"),
            apply: Box::new(move |p| {
                if !same {
                    p.set_instrument(id, reference)?;
                }
                for (d, value) in &values {
                    p.set_instrument_param(id, d.id, *value)?;
                }
                Ok(Value::Null)
            }),
        };
        apply(host, plan)?;
        if !changed.as_object().is_some_and(Map::is_empty) {
            out["params"] = changed;
        }
    }
    let Some(file) = file else {
        return Ok(Outcome::Done(Output::json(out)));
    };
    host.set_instrument_file(id, file.clone())?;
    let waiter: Waiter = Box::new(move |host| {
        let loaded = host.instrument_file_loaded(id, &file)?;
        Some(loaded.map_err(ToolError).map(|()| {
            let mut out = out.clone();
            out["file"] = json!(file.display().to_string());
            let summary = out["summary"].as_str().unwrap_or_default().to_owned();
            out["summary"] = json!(match host.slicer(id) {
                Some(info) => {
                    out["slices"] = json!(info.slices.len());
                    format!(
                        "{summary}, from {}, cut into {} slices at its transients",
                        file.display(),
                        info.slices.len()
                    )
                }
                None => format!("{summary}, from {}", file.display()),
            });
            Output::json(out)
        }))
    });
    Ok(Outcome::Waiting(waiter))
}

/// Checks `file` is something `descriptor` plays from.
fn check_instrument_file(
    descriptor: &instruments::BuiltInInstrumentDescriptor,
    file: &Path,
) -> Result<(), ToolError> {
    if !file.is_absolute() {
        return Err(ToolError(format!(
            "{} isn't a full path; give the file's full path",
            file.display()
        )));
    }
    let extension = |e: &str| file.extension().is_some_and(|x| x.eq_ignore_ascii_case(e));
    match descriptor.data {
        DataKind::Sfz if !extension("sfz") => Err(ToolError(format!(
            "the {} plays .sfz files; {} isn't one",
            descriptor.name,
            file.display()
        ))),
        DataKind::Slices if !media::is_supported_extension(file) => Err(ToolError(format!(
            "the {} plays an audio file (WAV, FLAC, MP3, OGG, AIFF…); {} isn't one",
            descriptor.name,
            file.display()
        ))),
        DataKind::Sfz | DataKind::Slices if !file.is_file() => {
            Err(ToolError(format!("{} isn't a file", file.display())))
        }
        DataKind::Sfz | DataKind::Slices => Ok(()),
        DataKind::None | DataKind::Wavetables => Err(ToolError(format!(
            "the {} doesn't play a file; `file` is for the Sampler (.sfz) and the Slicer (an \
             audio loop)",
            descriptor.name
        ))),
    }
}

// set_params

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParamsArgs {
    device: Option<DeviceId>,
    track: Option<TrackId>,
    values: Map<String, Value>,
}

fn set_params_schema() -> Value {
    schema::object(
        json!({
            "device": schema::id("An effect."),
            "track": schema::id("A MIDI track, for its instrument."),
            "values": values_schema("Each parameter's id (or name) and its value in its own units, or a choice's label."),
        }),
        &["values"],
    )
}

fn set_params(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: SetParamsArgs = args(v)?;
    let target = target(project, a.device, a.track)?;
    if a.values.is_empty() {
        return Err("`values` is empty: give the parameters to set".into());
    }
    match target {
        Target::Device(id) => {
            let device = device_of(project, id)?;
            let descriptor = device.effect.descriptor().ok_or_else(|| {
                ToolError(format!(
                    "device {id} ({}) isn't loaded, so its parameters aren't known",
                    device.name()
                ))
            })?;
            let owner = format!("device {id} ({})", descriptor.name);
            let values = check_values(
                descriptor.params.iter().chain(descriptor.settings),
                &a.values,
                &owner,
            )?;
            let summary = format!("set {} of {owner}", values_text(&values));
            let changed = values_json(&values);
            Ok(Plan {
                label: format!("Set {}", descriptor.name),
                apply: Box::new(move |p| {
                    for (d, value) in &values {
                        p.set_device_param(id, d.id, *value)?;
                    }
                    Ok(json!({ "device": id.0, "values": changed, "summary": summary }))
                }),
            })
        }
        Target::Instrument(track) => {
            let t = midi_track(project, track)?;
            let TrackKind::Midi { instrument, .. } = &t.kind else {
                unreachable!("checked to be a MIDI track");
            };
            let descriptor = instrument.descriptor().ok_or_else(|| {
                ToolError(format!(
                    "track {track}'s instrument isn't loaded, so its parameters aren't known"
                ))
            })?;
            let owner = format!("track {track}'s {}", descriptor.name);
            let values = check_values(descriptor.params.iter(), &a.values, &owner)?;
            let summary = format!("set {} of {owner}", values_text(&values));
            let changed = values_json(&values);
            Ok(Plan {
                label: format!("Set {}", descriptor.name),
                apply: Box::new(move |p| {
                    for (d, value) in &values {
                        p.set_instrument_param(track, d.id, *value)?;
                    }
                    Ok(json!({ "track": track.0, "values": changed, "summary": summary }))
                }),
            })
        }
    }
}

// add_effect

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddEffectArgs {
    track: Option<TrackId>,
    master: Option<bool>,
    effect: String,
    position: Option<usize>,
    params: Option<Map<String, Value>>,
    sidechain: Option<TrackId>,
}

fn add_effect_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("The track whose chain it goes in."),
            "master": schema::boolean("true puts it in the master's chain instead."),
            "effect": schema::string("A built-in effect's id or name: \"canopy.compressor\", \"EQ\"."),
            "position": schema::count("Where in the chain, 0 first; the end if left out."),
            "params": values_schema("Parameters to set, by id or name, in their own units."),
            "sidechain": schema::id("The track its sidechain listens to (effects with a sidechain only)."),
        }),
        &["effect"],
    )
}

/// The chain `track` or `master` names: `Some(track)`, or `None` for the
/// master's.
fn chain(
    project: &Project,
    track: Option<TrackId>,
    master: Option<bool>,
) -> Result<Option<Option<TrackId>>, ToolError> {
    match (track, master.unwrap_or(false)) {
        (Some(_), true) => Err("give `track` or `master`, not both".into()),
        (Some(track), false) => {
            track_of(project, track)?;
            Ok(Some(Some(track)))
        }
        (None, true) => Ok(Some(None)),
        (None, false) => Ok(None),
    }
}

/// "track 12" or "the master".
fn chain_name(track: Option<TrackId>) -> String {
    track.map_or_else(|| "the master".to_owned(), |t| format!("track {t}"))
}

/// Checks `source` can feed a sidechain on `owner`'s chain.
fn check_sidechain(
    project: &Project,
    descriptor: &effects::EffectDescriptor,
    owner: Option<TrackId>,
    source: TrackId,
) -> Result<(), ToolError> {
    if !descriptor.sidechain {
        let with: Vec<&str> = effects::builtins()
            .iter()
            .filter(|d| d.sidechain)
            .map(|d| d.id)
            .collect();
        return Err(ToolError(format!(
            "the {} has no sidechain; these do: {}",
            descriptor.name,
            with.join(", ")
        )));
    }
    track_of(project, source)?;
    if owner == Some(source) {
        return Err(ToolError(format!(
            "a sidechain listens to another track; track {source} is the one the effect is on"
        )));
    }
    Ok(())
}

fn add_effect(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: AddEffectArgs = args(v)?;
    let owner = chain(project, a.track, a.master)?
        .ok_or("give `track` for a track's chain, or `master`: true for the master's")?;
    let descriptor = find_effect(&a.effect)?;
    let len = project.devices(owner).map_or(0, <[_]>::len);
    if let Some(position) = a.position
        && position > len
    {
        return Err(ToolError(format!(
            "position is 0–{len} in {}'s chain of {len}; {position} is past its end",
            chain_name(owner)
        )));
    }
    let values = check_values(
        descriptor.params.iter().chain(descriptor.settings),
        &a.params.unwrap_or_default(),
        descriptor.name,
    )?;
    if let Some(source) = a.sidechain {
        check_sidechain(project, descriptor, owner, source)?;
    }
    let (position, sidechain) = (a.position, a.sidechain);
    let name = descriptor.name;
    let mut summary = format!("added the {name} to {}'s chain", chain_name(owner));
    if !values.is_empty() {
        summary.push_str(&format!(", with {}", values_text(&values)));
    }
    if let Some(source) = sidechain {
        summary.push_str(&format!(", listening to track {source}"));
    }
    let reference = EffectRef::builtin(descriptor.id);
    Ok(Plan {
        label: format!("Add {name}"),
        apply: Box::new(move |p| {
            let id = p.add_device(owner, reference, position)?;
            for (d, value) in &values {
                p.set_device_param(id, d.id, *value)?;
            }
            if sidechain.is_some() {
                p.set_device_sidechain(id, sidechain)?;
            }
            let (_, at) = p.find_device(id).ok_or(EditError::NoSuchDevice(id))?;
            Ok(json!({
                "id": id.0,
                "position": at,
                "summary": format!("{summary} (device {id})"),
            }))
        }),
    })
}

// edit_effects

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum EffectsOp {
    Remove,
    Move,
    Bypass,
    Sidechain,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditEffectsArgs {
    op: EffectsOp,
    devices: Vec<DeviceId>,
    position: Option<usize>,
    on: Option<bool>,
    track: Option<TrackId>,
    master: Option<bool>,
}

fn edit_effects_schema() -> Value {
    schema::object(
        json!({
            "op": schema::choice(&["remove", "move", "bypass", "sidechain"], "What to do."),
            "devices": schema::ids("The effects."),
            "position": schema::count("move: where in the chain, 0 first; the end if left out."),
            "on": schema::boolean("bypass: true bypasses them, false brings them back (true if left out)."),
            "track": schema::id("move: the track whose chain they go to; sidechain: the track they listen to (none if left out)."),
            "master": schema::boolean("move: true moves them to the master's chain."),
        }),
        &["op", "devices"],
    )
}

fn edit_effects(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: EditEffectsArgs = args(v)?;
    if a.devices.is_empty() {
        return Err("`devices` is empty: give the effects' ids (get_track lists them)".into());
    }
    let mut devices = Vec::with_capacity(a.devices.len());
    for (i, &id) in a.devices.iter().enumerate() {
        if a.devices[..i].contains(&id) {
            return Err(ToolError(format!("device {id} is listed twice")));
        }
        let device = device_of(project, id)?;
        let (owner, at) = project.find_device(id).expect("the device exists");
        devices.push((device, owner, at));
    }
    let takes: &[&str] = match a.op {
        EffectsOp::Remove => &[],
        EffectsOp::Move => &["position", "track", "master"],
        EffectsOp::Bypass => &["on"],
        EffectsOp::Sidechain => &["track"],
    };
    let given = [
        ("position", a.position.is_some()),
        ("on", a.on.is_some()),
        ("track", a.track.is_some()),
        ("master", a.master.is_some()),
    ];
    if let Some((stray, _)) = given.iter().find(|(n, g)| *g && !takes.contains(n)) {
        return Err(ToolError(format!("`{stray}` isn't for {}", op_name(a.op))));
    }
    let ids = a.devices.clone();
    let n = ids.len();
    let them = if n == 1 {
        format!("device {}", ids[0])
    } else {
        format!("{n} effects")
    };
    match a.op {
        EffectsOp::Remove => Ok(Plan {
            label: if n == 1 {
                "Remove Effect"
            } else {
                "Remove Effects"
            }
            .to_owned(),
            apply: Box::new(move |p| {
                p.remove_devices(&ids)?;
                Ok(json!({ "summary": format!("removed {them}") }))
            }),
        }),
        EffectsOp::Move => {
            let to = match chain(project, a.track, a.master)? {
                Some(to) => to,
                None => {
                    let from = devices[0].1;
                    if devices.iter().any(|(_, owner, _)| *owner != from) {
                        return Err(
                            "these effects are in different chains; give `track` or `master` \
                             for the one they go to"
                                .into(),
                        );
                    }
                    from
                }
            };
            // Counted without the moving ones, as `move_devices` does.
            let staying = project
                .devices(to)
                .map_or(0, |d| d.iter().filter(|d| !ids.contains(&d.id)).count());
            if let Some(position) = a.position
                && position > staying
            {
                return Err(ToolError(format!(
                    "position is 0–{staying} in {}'s chain; {position} is past its end",
                    chain_name(to)
                )));
            }
            let position = a.position.unwrap_or(staying);
            // `move_devices` counts its index among the chain as it is.
            let index = project.devices(to).map_or(0, |d| {
                let mut seen = 0;
                d.iter()
                    .position(|d| {
                        let here = seen == position && !ids.contains(&d.id);
                        if !ids.contains(&d.id) {
                            seen += 1;
                        }
                        here
                    })
                    .unwrap_or(d.len())
            });
            Ok(Plan {
                label: if n == 1 {
                    "Move Effect"
                } else {
                    "Move Effects"
                }
                .to_owned(),
                apply: Box::new(move |p| {
                    p.move_devices(&ids, to, index)?;
                    Ok(json!({
                        "summary": format!(
                            "moved {them} to position {position} of {}'s chain",
                            chain_name(to)
                        ),
                    }))
                }),
            })
        }
        EffectsOp::Bypass => {
            let on = a.on.unwrap_or(true);
            Ok(Plan {
                label: if on { "Bypass" } else { "Enable" }.to_owned(),
                apply: Box::new(move |p| {
                    for &id in &ids {
                        p.set_device_bypassed(id, on)?;
                    }
                    Ok(json!({
                        "summary": format!(
                            "{} {them}",
                            if on { "bypassed" } else { "brought back" }
                        ),
                    }))
                }),
            })
        }
        EffectsOp::Sidechain => {
            let source = a.track;
            for (device, owner, _) in &devices {
                let descriptor = device
                    .effect
                    .descriptor()
                    .ok_or_else(|| ToolError(format!("device {} isn't loaded", device.id)))?;
                if let Some(source) = source {
                    check_sidechain(project, descriptor, *owner, source)?;
                } else if !descriptor.sidechain {
                    return Err(ToolError(format!(
                        "the {} (device {}) has no sidechain",
                        descriptor.name, device.id
                    )));
                }
            }
            Ok(Plan {
                label: "Route Sidechain".to_owned(),
                apply: Box::new(move |p| {
                    for &id in &ids {
                        p.set_device_sidechain(id, source)?;
                    }
                    Ok(json!({
                        "summary": match source {
                            Some(source) => format!("{them} now listen to track {source}"),
                            None => format!("{them} listen to nothing"),
                        },
                    }))
                }),
            })
        }
    }
}

fn op_name(op: EffectsOp) -> &'static str {
    match op {
        EffectsOp::Remove => "remove",
        EffectsOp::Move => "move",
        EffectsOp::Bypass => "bypass",
        EffectsOp::Sidechain => "sidechain",
    }
}

// load_preset

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadPresetArgs {
    device: Option<DeviceId>,
    track: Option<TrackId>,
    preset: String,
}

fn load_preset_schema() -> Value {
    schema::object(
        json!({
            "device": schema::id("An effect."),
            "track": schema::id("A MIDI track, for its instrument."),
            "preset": schema::string("The preset's name, as list_presets gives it."),
        }),
        &["preset"],
    )
}

fn load_preset(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: LoadPresetArgs = args(v)?;
    let target = target(host.project(), a.device, a.track)?;
    host.load_preset(target, &a.preset)?;
    let whose = match target {
        Target::Device(id) => format!("device {id}"),
        Target::Instrument(track) => format!("track {track}'s instrument"),
    };
    Ok(Outcome::Done(Output::json(json!({
        "summary": format!("loaded {:?} into {whose}", a.preset),
    }))))
}

// slicer

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SlicerArgsOp {
    Get,
    Set,
    Transients,
    Grid,
    ToPianoRoll,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlicerArgs {
    track: TrackId,
    op: SlicerArgsOp,
    slices: Option<Vec<f64>>,
    sensitivity: Option<f64>,
    division: Option<i64>,
    at: Option<f64>,
}

fn slicer_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("A track playing the Slicer."),
            "op": schema::choice(&["get", "set", "transients", "grid", "to_piano_roll"], "What to do."),
            "slices": schema::array(
                json!({ "type": "number", "minimum": 0 }),
                "set: where each slice starts, in seconds into the loop."
            ),
            "sensitivity": schema::number("transients: 0–1, higher finds more (0.5 if left out)."),
            "division": schema::integer("grid: slices to a bar, 1–64 (16 if left out)."),
            "at": schema::beats("to_piano_roll: where the clip starts, in beats (0 if left out)."),
        }),
        &["track", "op"],
    )
}

fn slicer(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: SlicerArgs = args(v)?;
    let project = host.project();
    let t = midi_track(project, a.track)?;
    let plays_slicer = matches!(&t.kind, TrackKind::Midi { instrument, .. }
        if instrument.descriptor().is_some_and(|d| d.data == DataKind::Slices));
    if !plays_slicer {
        return Err(ToolError(format!(
            "track {} ({:?}) doesn't play the Slicer; set_instrument with \"canopy.slicer\" and a \
             `file` gives it one",
            a.track, t.name
        )));
    }
    let Some(info) = host.slicer(a.track) else {
        return Err(ToolError(if t.instrument_file().is_some() {
            format!(
                "track {}'s loop hasn't loaded yet; try again in a moment",
                a.track
            )
        } else {
            format!(
                "track {}'s Slicer has no loop; set_instrument with a `file` gives it one",
                a.track
            )
        }));
    };
    let takes: &[&str] = match a.op {
        SlicerArgsOp::Get => &[],
        SlicerArgsOp::Set => &["slices"],
        SlicerArgsOp::Transients => &["sensitivity"],
        SlicerArgsOp::Grid => &["division"],
        SlicerArgsOp::ToPianoRoll => &["at"],
    };
    let given = [
        ("slices", a.slices.is_some()),
        ("sensitivity", a.sensitivity.is_some()),
        ("division", a.division.is_some()),
        ("at", a.at.is_some()),
    ];
    if let Some((stray, _)) = given.iter().find(|(n, g)| *g && !takes.contains(n)) {
        return Err(ToolError(format!("`{stray}` isn't for this op")));
    }
    let rate = f64::from(info.rate.max(1));
    let op = match a.op {
        SlicerArgsOp::Get => None,
        SlicerArgsOp::Set => {
            let seconds = a.slices.ok_or("set needs `slices`, in seconds")?;
            let length = info.frames as f64 / rate;
            if seconds.is_empty() {
                return Err("`slices` is empty; a loop has at least one slice, at 0".into());
            }
            if let Some(bad) = seconds
                .iter()
                .find(|s| !(s.is_finite() && **s >= 0.0 && **s < length))
            {
                return Err(ToolError(format!(
                    "slices start within the loop, 0–{length:.3} s; {bad} isn't"
                )));
            }
            Some(SlicerOp::Set(
                seconds.iter().map(|s| (s * rate).round() as u64).collect(),
            ))
        }
        SlicerArgsOp::Transients => {
            let sensitivity = a.sensitivity.unwrap_or(0.5);
            if !(0.0..=1.0).contains(&sensitivity) {
                return Err(ToolError(format!(
                    "sensitivity is 0–1; {sensitivity} is out of range"
                )));
            }
            Some(SlicerOp::Transients {
                sensitivity: sensitivity as f32,
            })
        }
        SlicerArgsOp::Grid => {
            let division = a.division.unwrap_or(16);
            if !(1..=64).contains(&division) {
                return Err(ToolError(format!(
                    "division is 1–64 slices a bar; {division} is out of range"
                )));
            }
            Some(SlicerOp::Grid {
                division: division as u32,
            })
        }
        SlicerArgsOp::ToPianoRoll => Some(SlicerOp::ToPianoRoll {
            at: beats_to_tick(a.at.unwrap_or(0.0))?,
        }),
    };
    let mut out = match op {
        Some(op) => host.slicer_op(a.track, op)?,
        None => Value::Null,
    };
    if !out.is_object() {
        out = json!({});
    }
    let info = host.slicer(a.track).unwrap_or(info);
    let root = host
        .project()
        .track(a.track)
        .and_then(|t| t.instrument_param("root"))
        .unwrap_or(60.0) as u32;
    out["file"] = json!(info.file.display().to_string());
    out["seconds"] = json!((info.frames as f64 / rate * 1e4).round() / 1e4);
    out["slices"] = json!(
        info.slices
            .iter()
            .enumerate()
            .map(|(k, &frame)| {
                let key = midi::Pitch::new((root + k as u32).min(127) as u8)
                    .expect("held within the MIDI range");
                json!({
                    "index": k,
                    "start_seconds": (frame as f64 / rate * 1e4).round() / 1e4,
                    "key": crate::units::pitch_name(key),
                })
            })
            .collect::<Vec<_>>()
    );
    if out.get("summary").is_none() {
        out["summary"] = json!(format!(
            "track {}'s loop is cut into {} slices",
            a.track,
            info.slices.len()
        ));
    }
    Ok(Outcome::Done(Output::json(out)))
}

// set_automation

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointArgs {
    at: f64,
    value: Value,
    curve: Option<f64>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum WaveArg {
    Sine,
    Triangle,
    SawUp,
    SawDown,
    Square,
    RandomSteps,
    RandomSmooth,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateArgs {
    wave: WaveArg,
    from: f64,
    to: f64,
    period: f64,
    low: Value,
    high: Value,
    phase: Option<f64>,
    seed: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAutomationArgs {
    track: TrackId,
    target: Value,
    points: Option<Vec<PointArgs>>,
    generate: Option<GenerateArgs>,
    clear: Option<bool>,
}

/// Points one lane may be given, at most.
const MAX_POINTS: usize = 10_000;

fn set_automation_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("The track whose lane it is."),
            "target": schema::target(
                "\"volume\", \"pan\", {\"instrument\": param}, {\"device\": id, \"param\": param} or \
                 {\"modulator\": id, \"param\": param}."
            ),
            "points": schema::array(
                schema::object(
                    json!({
                        "at": schema::beats("When, in beats from the song's start."),
                        "value": json!({
                            "type": ["number", "string", "boolean"],
                            "description": "In the target's own units (dB for volume, -1–1 for pan), or a choice's label.",
                        }),
                        "curve": schema::number("How the segment to the next point bends, -1–1; 0 is straight."),
                    }),
                    &["at", "value"],
                ),
                "The lane's new points, replacing all it had (at most 10000).",
            ),
            "generate": schema::object(
                json!({
                    "wave": schema::choice(
                        &["sine", "triangle", "saw_up", "saw_down", "square", "random_steps", "random_smooth"],
                        "The shape, repeating every `period`; random ones pick a new value each period."
                    ),
                    "from": schema::beats("Where it starts, in beats."),
                    "to": schema::beats("Where it ends, in beats."),
                    "period": schema::number("One cycle every this many beats."),
                    "low": json!({ "type": ["number", "string", "boolean"], "description": "The lowest value, in the target's units." }),
                    "high": json!({ "type": ["number", "string", "boolean"], "description": "The highest value, in the target's units." }),
                    "phase": schema::number("Where in the cycle it starts, 0–1."),
                    "seed": schema::count("For the random waves: the same seed makes the same values."),
                }),
                &["wave", "from", "to", "period", "low", "high"],
            ),
            "clear": schema::boolean("true removes the lane."),
        }),
        &["track", "target"],
    )
}

fn set_automation(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: SetAutomationArgs = args(v)?;
    track_of(project, a.track)?;
    let target: AutomationTarget = parse_target(&a.target)?;
    let units = Units::of(project, a.track, &target)?;
    let track = a.track;
    let shown = target_json(&target);
    let existing = project
        .track(track)
        .and_then(|t| t.automation.iter().find(|l| l.target == target))
        .map(|l| l.points.to_vec())
        .unwrap_or_default();
    let (points, summary) = match (a.points, a.generate, a.clear) {
        (Some(points), None, None) => {
            if points.len() > MAX_POINTS {
                return Err(ToolError(format!(
                    "{} points is more than one lane takes ({MAX_POINTS})",
                    points.len()
                )));
            }
            if points.is_empty() {
                return Err("`points` is empty; `clear`: true removes a lane".into());
            }
            let mut checked = Vec::with_capacity(points.len());
            for (i, point) in points.iter().enumerate() {
                let check = || -> Result<AutomationPoint, String> {
                    let curve = point.curve.unwrap_or(0.0);
                    if !(-1.0..=1.0).contains(&curve) {
                        return Err(format!("curve is -1–1; {curve} is out of range"));
                    }
                    Ok(AutomationPoint {
                        tick: beats_to_tick(point.at)?,
                        value: units.write(&point.value)?,
                        curve: curve as f32,
                    })
                };
                checked.push(check().map_err(|e| ToolError(format!("point {i}: {e}")))?);
            }
            let summary = format!("wrote {} points to {shown}", checked.len());
            (checked, summary)
        }
        (None, Some(g), None) => {
            let start = beats_to_tick(g.from)?;
            let end = beats_to_tick(g.to)?;
            if end <= start {
                return Err("`to` must come after `from`".into());
            }
            let period = beats_to_offset(g.period)?;
            if period.get() < 2 {
                return Err(ToolError(format!(
                    "period is a length in beats, more than 0; not {}",
                    g.period
                )));
            }
            let phase = g.phase.unwrap_or(0.0);
            if !(0.0..=1.0).contains(&phase) {
                return Err(ToolError(format!("phase is 0–1; {phase} is out of range")));
            }
            let low = units
                .write(&g.low)
                .map_err(|e| ToolError(format!("low: {e}")))?;
            let high = units
                .write(&g.high)
                .map_err(|e| ToolError(format!("high: {e}")))?;
            let wave = match g.wave {
                WaveArg::Sine => Wave::Sine,
                WaveArg::Triangle => Wave::Triangle,
                WaveArg::SawUp => Wave::SawUp,
                WaveArg::SawDown => Wave::SawDown,
                WaveArg::Square => Wave::Square,
                WaveArg::RandomSteps => Wave::RandomSteps,
                WaveArg::RandomSmooth => Wave::RandomSmooth,
            };
            let made = Generate {
                wave,
                start,
                end,
                period,
                low,
                high,
                phase: phase as f32,
                seed: g.seed.unwrap_or(1),
            }
            .points();
            let summary = format!(
                "generated {} points of a {} on {shown} from beat {} to {}",
                made.len(),
                wave.label().to_lowercase(),
                tick_to_beats(start),
                tick_to_beats(end)
            );
            let points = project::replace_range(&existing, start, end, made);
            (points, summary)
        }
        (None, None, Some(true)) => {
            if existing.is_empty() {
                return Err(ToolError(format!(
                    "track {track} has no automation for {shown}"
                )));
            }
            (Vec::new(), format!("removed the lane for {shown}"))
        }
        (None, None, Some(false)) => {
            return Err("`clear`: false does nothing; give `points` or `generate`".into());
        }
        _ => return Err("give one of `points`, `generate` or `clear`".into()),
    };
    let count = points.len();
    let unit = units.unit();
    let first_last = points.first().zip(points.last()).map(|(f, l)| {
        json!({
            "from": { "at": tick_to_beats(f.tick), "value": units.read(f.value) },
            "to": { "at": tick_to_beats(l.tick), "value": units.read(l.value) },
        })
    });
    Ok(Plan {
        label: "Automation".to_owned(),
        apply: Box::new(move |p| {
            p.set_automation(track, target, points)?;
            let mut out = json!({
                "track": track.0,
                "target": shown,
                "points": count,
                "unit": unit,
                "summary": summary,
            });
            if let Some(span) = first_last {
                out["span"] = span;
            }
            Ok(out)
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{PresetEntry, SlicerInfo};
    use crate::testing::FakeHost;
    use crate::tools::call;
    use midi::PPQ;
    use project::{NewTrack, TrackPosition, volume_to_normalized};

    /// A lane's points as stored: ticks and `0..=1` values.
    fn stored(project: &Project, track: TrackId, target: &AutomationTarget) -> Vec<(i64, f32)> {
        project
            .track(track)
            .and_then(|t| t.automation.iter().find(|l| l.target == *target))
            .map(|l| l.points.iter().map(|p| (p.tick.get(), p.value)).collect())
            .unwrap_or_default()
    }

    fn done(tool: &str, args: Value, host: &mut FakeHost) -> Value {
        match call(tool, &args, host) {
            Ok(Outcome::Done(output)) => output.json,
            Ok(Outcome::Waiting(_)) => panic!("{tool} answers at once"),
            Err(error) => panic!("{tool}: {error}"),
        }
    }

    fn failure(tool: &str, args: Value, host: &mut FakeHost) -> String {
        match call(tool, &args, host) {
            Err(error) => error.0,
            Ok(_) => panic!("{tool} with {args} should fail"),
        }
    }

    /// Two MIDI tracks and an audio track.
    fn rig() -> (FakeHost, TrackId, TrackId, TrackId) {
        let mut project = Project::new();
        let mut add = |kind| project.add_track(kind, TrackPosition::END).unwrap();
        let (a, b, c) = (
            add(NewTrack::Midi),
            add(NewTrack::Midi),
            add(NewTrack::Audio),
        );
        (FakeHost::new(project), a, b, c)
    }

    #[test]
    fn an_eq_with_a_dynamic_band_in_one_call() {
        let (mut host, track, _, _) = rig();
        let steps = host.history.position();
        let added = done(
            "add_effect",
            json!({
                "track": track.0, "effect": "EQ",
                "params": { "b5_freq": 7000, "b5_dyn": "On", "b5_threshold": -30, "Band 5 gain": -4 },
            }),
            &mut host,
        );
        assert_eq!(host.history.position(), steps + 1);
        assert_eq!(host.history.undo_label(), Some("LLM: Add EQ"));
        let id = DeviceId(added["id"].as_u64().unwrap() as u32);
        let device = done(
            "get_device",
            json!({ "device": id.0, "filter": "b5_" }),
            &mut host,
        );
        let value = |param: &str| {
            device["params"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == json!(param))
                .map(|p| p["value"].clone())
                .unwrap()
        };
        assert_eq!(value("b5_freq"), json!(7000.0));
        assert_eq!(value("b5_dyn"), json!("On"));
        assert_eq!(value("b5_threshold"), json!(-30.0));
        assert_eq!(value("b5_gain"), json!(-4.0));
        assert_eq!(device["chain"]["track"], json!(track.0));

        // Into the master's chain, first.
        let limiter = done(
            "add_effect",
            json!({ "master": true, "effect": "canopy.limiter", "position": 0 }),
            &mut host,
        );
        assert_eq!(limiter["position"], json!(0));
        assert_eq!(host.project.master_devices.len(), 1);
        let error = failure(
            "add_effect",
            json!({ "track": track.0, "effect": "canopy.eq", "position": 5 }),
            &mut host,
        );
        assert!(error.contains("0–1"), "{error}");
        let error = failure(
            "add_effect",
            json!({ "track": track.0, "effect": "canopy.flanger9000" }),
            &mut host,
        );
        assert!(error.contains("list_devices"), "{error}");
    }

    #[test]
    fn set_params_rejects_the_whole_call_on_one_bad_value() {
        let (mut host, track, _, _) = rig();
        let eq = host
            .project
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let before = host.project.clone();
        let error = failure(
            "set_params",
            json!({ "device": eq.0, "values": { "b3_freq": 2000, "b3_gain": 40, "b3_type": "Wobble" } }),
            &mut host,
        );
        assert!(
            error.contains("b3_gain") && error.contains("-24–24 dB"),
            "{error}"
        );
        assert!(error.contains("b3_type"), "every problem is named: {error}");
        assert!(host.project == before, "nothing changed");
        assert_eq!(host.history.position(), 0);
        let error = failure(
            "set_params",
            json!({ "device": eq.0, "values": { "wobble": 1 } }),
            &mut host,
        );
        assert!(error.contains("no parameter \"wobble\""), "{error}");

        let set = done(
            "set_params",
            json!({ "device": eq.0, "values": { "b3_freq": 2000, "Band 3 Q": 4 } }),
            &mut host,
        );
        assert_eq!(set["values"]["b3_q"], json!(4.0));
        let device = host.project.device(eq).unwrap();
        assert_eq!(device.param("b3_freq"), Some(2000.0));
        assert_eq!(host.history.position(), 1);

        // An instrument's, by the track.
        let synth = instruments::find_builtin(instruments::DEFAULT_INSTRUMENT).unwrap();
        let float = synth
            .params
            .iter()
            .find(|p| matches!(p.kind, instruments::ParameterKind::Float { .. }))
            .unwrap();
        let value = float.default_value();
        done(
            "set_params",
            json!({ "track": track.0, "values": { float.id: value } }),
            &mut host,
        );
        assert_eq!(
            host.project
                .track(track)
                .unwrap()
                .instrument_param(float.id),
            Some(value)
        );
        let error = failure("set_params", json!({ "values": { "x": 1 } }), &mut host);
        assert!(error.contains("`device`"), "{error}");
    }

    #[test]
    fn effects_move_and_bypass() {
        let (mut host, a, b, _) = rig();
        let add = |host: &mut FakeHost, id: &str| {
            host.project
                .add_device(Some(a), EffectRef::builtin(id), None)
                .unwrap()
        };
        let eq = add(&mut host, "canopy.eq");
        let comp = add(&mut host, "canopy.compressor");
        let reverb = add(&mut host, "canopy.reverb");
        done(
            "edit_effects",
            json!({ "op": "move", "devices": [reverb.0], "position": 0 }),
            &mut host,
        );
        let order = |host: &FakeHost, track| -> Vec<DeviceId> {
            host.project
                .devices(Some(track))
                .unwrap()
                .iter()
                .map(|d| d.id)
                .collect()
        };
        assert_eq!(order(&host, a), [reverb, eq, comp]);
        done(
            "edit_effects",
            json!({ "op": "move", "devices": [eq.0], "position": 2 }),
            &mut host,
        );
        assert_eq!(order(&host, a), [reverb, comp, eq]);
        done(
            "edit_effects",
            json!({ "op": "move", "devices": [comp.0], "track": b.0 }),
            &mut host,
        );
        assert_eq!(order(&host, a), [reverb, eq]);
        assert_eq!(order(&host, b), [comp]);

        done(
            "edit_effects",
            json!({ "op": "bypass", "devices": [eq.0, reverb.0] }),
            &mut host,
        );
        assert!(host.project.device(eq).unwrap().bypassed);
        done(
            "edit_effects",
            json!({ "op": "bypass", "devices": [eq.0], "on": false }),
            &mut host,
        );
        assert!(!host.project.device(eq).unwrap().bypassed);
        assert!(host.project.device(reverb).unwrap().bypassed);
        let error = failure(
            "edit_effects",
            json!({ "op": "bypass", "devices": [eq.0], "position": 1 }),
            &mut host,
        );
        assert!(error.contains("`position` isn't for bypass"), "{error}");

        done(
            "edit_effects",
            json!({ "op": "remove", "devices": [eq.0] }),
            &mut host,
        );
        assert_eq!(order(&host, a), [reverb]);
    }

    #[test]
    fn sidechain_must_be_another_track() {
        let (mut host, a, b, _) = rig();
        let error = failure(
            "add_effect",
            json!({ "track": a.0, "effect": "canopy.ducker", "sidechain": a.0 }),
            &mut host,
        );
        assert!(error.contains("another track"), "{error}");
        let error = failure(
            "add_effect",
            json!({ "track": a.0, "effect": "canopy.reverb", "sidechain": b.0 }),
            &mut host,
        );
        assert!(
            error.contains("no sidechain") && error.contains("canopy.ducker"),
            "{error}"
        );
        let added = done(
            "add_effect",
            json!({ "track": a.0, "effect": "canopy.ducker", "sidechain": b.0 }),
            &mut host,
        );
        let ducker = DeviceId(added["id"].as_u64().unwrap() as u32);
        assert_eq!(host.project.device(ducker).unwrap().sidechain, Some(b));
        let error = failure(
            "edit_effects",
            json!({ "op": "sidechain", "devices": [ducker.0], "track": a.0 }),
            &mut host,
        );
        assert!(error.contains("another track"), "{error}");
        done(
            "edit_effects",
            json!({ "op": "sidechain", "devices": [ducker.0] }),
            &mut host,
        );
        assert_eq!(host.project.device(ducker).unwrap().sidechain, None);
    }

    #[test]
    fn automation_points_are_in_the_targets_units() {
        let (mut host, track, _, _) = rig();
        done(
            "set_automation",
            json!({
                "track": track.0, "target": "volume",
                "points": [{ "at": 0, "value": -6 }, { "at": 4, "value": 0, "curve": 0.5 }],
            }),
            &mut host,
        );
        assert_eq!(
            stored(&host.project, track, &AutomationTarget::Volume),
            [
                (0, volume_to_normalized(-6.0)),
                (4 * PPQ, volume_to_normalized(0.0))
            ]
        );
        let lanes = done("get_automation", json!({ "track": track.0 }), &mut host);
        assert_eq!(lanes["lanes"][0]["points"][0]["value"], json!(-6.0));

        let before = host.project.clone();
        let error = failure(
            "set_automation",
            json!({ "track": track.0, "target": "volume", "points": [{ "at": 0, "value": 0 }, { "at": 1, "value": 12 }] }),
            &mut host,
        );
        assert!(
            error.contains("point 1") && error.contains("-60–6 dB"),
            "{error}"
        );
        assert!(host.project == before);

        // A device's parameter, in its own units.
        let eq = host
            .project
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let target = json!({ "device": eq.0, "param": "b3_freq" });
        done(
            "set_automation",
            json!({ "track": track.0, "target": target, "points": [{ "at": 0, "value": 1000 }] }),
            &mut host,
        );
        let freq = project::AutomationTarget::DeviceParam {
            device: eq,
            param: "b3_freq".into(),
        };
        let d = Units::of(&host.project, track, &freq).unwrap();
        let Units::Param(d) = d else {
            panic!("a parameter")
        };
        assert_eq!(
            stored(&host.project, track, &freq),
            [(0, d.to_normalized(1000.0))]
        );
        done(
            "set_automation",
            json!({ "track": track.0, "target": "volume", "clear": true }),
            &mut host,
        );
        assert!(stored(&host.project, track, &AutomationTarget::Volume).is_empty());
    }

    #[test]
    fn generated_automation_replaces_its_range() {
        let (mut host, track, _, _) = rig();
        done(
            "set_automation",
            json!({
                "track": track.0, "target": "pan",
                "points": [{ "at": 0, "value": -1 }, { "at": 8, "value": 0 }, { "at": 32, "value": 1 }],
            }),
            &mut host,
        );
        done(
            "set_automation",
            json!({
                "track": track.0, "target": "pan",
                "generate": { "wave": "square", "from": 4, "to": 16, "period": 4, "low": -0.5, "high": 0.5 },
            }),
            &mut host,
        );
        let points = stored(&host.project, track, &AutomationTarget::Pan);
        let (start, end) = (4 * PPQ, 16 * PPQ);
        // Outside the range, as they were; inside, only generated values.
        assert_eq!(points.first(), Some(&(0, project::pan_to_normalized(-1.0))));
        assert_eq!(
            points.last(),
            Some(&(32 * PPQ, project::pan_to_normalized(1.0)))
        );
        let inside: Vec<_> = points
            .iter()
            .filter(|(t, _)| (start..=end).contains(t))
            .collect();
        assert!(inside.len() >= 3, "{inside:?}");
        assert!(!points.contains(&(8 * PPQ, project::pan_to_normalized(0.0))));
        for (_, v) in inside {
            assert!(
                [
                    project::pan_to_normalized(-0.5),
                    project::pan_to_normalized(0.5)
                ]
                .iter()
                .any(|x| (x - v).abs() < 1e-6),
                "{v}"
            );
        }
        let error = failure(
            "set_automation",
            json!({
                "track": track.0, "target": "pan",
                "generate": { "wave": "sine", "from": 4, "to": 2, "period": 1, "low": 0, "high": 1 },
            }),
            &mut host,
        );
        assert!(error.contains("after"), "{error}");
    }

    #[test]
    fn set_instrument_keeps_settings_of_the_same_instrument() {
        let (mut host, track, _, audio) = rig();
        done(
            "set_instrument",
            json!({ "track": track.0, "instrument": "canopy.wavetable" }),
            &mut host,
        );
        let plays = |host: &FakeHost| match &host.project.track(track).unwrap().kind {
            TrackKind::Midi { instrument, .. } => instrument.id.clone(),
            _ => unreachable!(),
        };
        assert_eq!(plays(&host), "canopy.wavetable");
        let synth = instruments::find_builtin("canopy.wavetable").unwrap();
        let float = synth
            .params
            .iter()
            .find(|p| matches!(p.kind, instruments::ParameterKind::Float { min, max, .. } if max > min))
            .unwrap();
        let steps = host.history.position();
        done(
            "set_instrument",
            json!({ "track": track.0, "instrument": "Wavetable", "params": { float.id: float.default_value() } }),
            &mut host,
        );
        assert_eq!(host.history.position(), steps + 1);
        let error = failure(
            "set_instrument",
            json!({ "track": audio.0, "instrument": "canopy.wavetable" }),
            &mut host,
        );
        assert!(error.contains("audio track"), "{error}");
        let error = failure(
            "set_instrument",
            json!({ "track": track.0, "instrument": "canopy.wavetable", "file": "/tmp/x.wav" }),
            &mut host,
        );
        assert!(error.contains("doesn't play a file"), "{error}");
    }

    #[test]
    fn a_slicer_file_answers_once_loaded() {
        let (mut host, track, _, _) = rig();
        let dir = std::env::temp_dir().join(format!("canopy-mcp-sound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("amen.wav");
        std::fs::write(&file, b"RIFF").unwrap();
        host.loading_files.push(file.clone());
        let Ok(Outcome::Waiting(mut waiter)) = call(
            "set_instrument",
            &json!({ "track": track.0, "instrument": "canopy.slicer", "file": file }),
            &mut host,
        ) else {
            panic!("a Slicer's file waits for its loop");
        };
        assert_eq!(host.instrument_files, [(track, file.clone())]);
        assert!(waiter(&mut host).is_none());
        host.loading_files.clear();
        host.slicers.insert(
            track,
            SlicerInfo {
                file: file.clone(),
                rate: 48_000,
                frames: 96_000,
                slices: vec![0, 24_000, 48_000],
            },
        );
        let answer = waiter(&mut host).expect("answered").unwrap();
        assert_eq!(answer.json["slices"], json!(3));

        let read = done(
            "slicer",
            json!({ "track": track.0, "op": "get" }),
            &mut host,
        );
        assert_eq!(read["slices"][1]["start_seconds"], json!(0.5));
        assert_eq!(read["slices"][1]["key"], json!("C#3"));
        done(
            "slicer",
            json!({ "track": track.0, "op": "set", "slices": [0, 0.25, 1.5] }),
            &mut host,
        );
        done(
            "slicer",
            json!({ "track": track.0, "op": "transients", "sensitivity": 0.8 }),
            &mut host,
        );
        assert_eq!(
            host.slicer_ops,
            [
                (track, SlicerOp::Set(vec![0, 12_000, 72_000])),
                (track, SlicerOp::Transients { sensitivity: 0.8 }),
            ]
        );
        let error = failure(
            "slicer",
            json!({ "track": track.0, "op": "set", "slices": [0, 3] }),
            &mut host,
        );
        assert!(error.contains("0–2.000 s"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presets_load_by_name_through_the_host() {
        let (mut host, track, _, _) = rig();
        host.presets = vec![PresetEntry {
            name: "Bass".into(),
            factory: true,
        }];
        done(
            "load_preset",
            json!({ "track": track.0, "preset": "Bass" }),
            &mut host,
        );
        assert_eq!(
            host.loaded_presets,
            [(Target::Instrument(track), "Bass".to_owned())]
        );
        let error = failure(
            "load_preset",
            json!({ "track": track.0, "preset": "Lead" }),
            &mut host,
        );
        assert!(error.contains("Lead"), "{error}");
    }
}
