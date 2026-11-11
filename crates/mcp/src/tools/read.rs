//! Tools that read: the project, a track, a clip's notes or audio, a
//! device's parameters, what devices exist, automation, presets and the
//! undo history. They change nothing, so they answer even while the user
//! is mid-gesture.

use instruments::{Format, ParamDescriptor};
use midi::{Tick, Velocity};
use project::{
    AudioClip, ChannelMode, ClipId, ClipRef, Device, DeviceId, MidiClip, Mixer, Note, Project,
    StretchMode, Track, TrackId, TrackKind,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Kind, NOTES_PAGE, Output, Tool, ToolError, args, schema};
use crate::host::{Host, Target};
use crate::units::{
    Units, bar_beat, beats_to_tick, color_hex, describe_param, number, parse_target, pitch_name,
    target_json, tick_to_beats,
};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "get_project",
        description: "The whole project at a glance: tempo, key, loop, length and playhead; \
                      the track tree (ids, names, colors, kinds, instruments, mixers, devices, \
                      clip counts, automated targets); the master's effects; and what the user \
                      has selected. Start here.",
        schema: no_args_schema,
        examples: &["{}"],
        kind: Kind::Read(get_project),
    },
    Tool {
        name: "get_track",
        description: "Everything about one track: its mixer, its instrument and effects with \
                      every parameter, its clips (id, start, length), its automation lanes and \
                      what its effects' sidechains listen to.",
        schema: get_track_schema,
        examples: &[r#"{"track": 12}"#],
        kind: Kind::Read(get_track),
    },
    Tool {
        name: "get_clip",
        description: "One clip. A MIDI clip's notes (id, start and length in beats from the \
                      clip's start, pitch, velocity 0–127, bends), 2000 at a time: pass the \
                      returned `cursor` to read on. An audio clip's file, region, gain, fades \
                      and processing. On a Slicer track, also where the slices are and the \
                      keys that play them.",
        schema: get_clip_schema,
        examples: &[
            r#"{"clip": 31}"#,
            r#"{"clip": 31, "from": 4, "to": 8}"#,
            r#"{"clip": 31, "cursor": 2000}"#,
        ],
        kind: Kind::Read(get_clip),
    },
    Tool {
        name: "get_device",
        description: "An effect's parameters, or a track's instrument's: each one's id, name, \
                      value, unit, range, scale (log or linear) and default, or a choice's \
                      labels; and an effect's bypass and sidechain. Give `device` for an \
                      effect or `track` for the instrument. Instruments with hundreds of \
                      parameters list the changed ones and their groups: pass `filter` for \
                      the rest.",
        schema: get_device_schema,
        examples: &[
            r#"{"device": 40}"#,
            r#"{"track": 12}"#,
            r#"{"track": 12, "filter": "Osc A"}"#,
        ],
        kind: Kind::Read(get_device),
    },
    Tool {
        name: "list_devices",
        description: "Every instrument and effect canopy has, and every installed plugin, with \
                      what each is for. With `id`, that one's parameters and their ranges.",
        schema: list_devices_schema,
        examples: &[
            "{}",
            r#"{"kind": "effect"}"#,
            r#"{"id": "canopy.compressor"}"#,
        ],
        kind: Kind::Read(list_devices),
    },
    Tool {
        name: "get_automation",
        description: "A track's automation lanes and their points (time in beats, value in the \
                      target's own units, curve -1–1). With `target`, that lane alone.",
        schema: get_automation_schema,
        examples: &[
            r#"{"track": 12}"#,
            r#"{"track": 12, "target": "volume"}"#,
            r#"{"track": 12, "target": {"device": 40, "param": "cutoff"}}"#,
        ],
        kind: Kind::Read(get_automation),
    },
    Tool {
        name: "list_presets",
        description: "The presets, factory and the user's, for an effect (`device`) or a \
                      track's instrument (`track`).",
        schema: device_or_track_schema,
        examples: &[r#"{"track": 12}"#, r#"{"device": 40}"#],
        kind: Kind::Read(list_presets),
    },
    Tool {
        name: "get_history",
        description: "The undo history: each step's label, how long ago it was made and who \
                      made it (`origin` \"user\", or \"llm\" with the `client`'s name), and how \
                      many are done (`position`). The history tool undoes, redoes or jumps.",
        schema: get_history_schema,
        examples: &["{}", r#"{"last": 10}"#],
        kind: Kind::Read(get_history),
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn no_args_schema() -> Value {
    schema::object(json!({}), &[])
}

fn get_project(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let NoArgs {} = args(v)?;
    let project = host.project();
    let tempo = &project.tempo;
    let transport = host.transport();
    let selection = host.selection();
    let ids = |ids: &[u32]| json!(ids);
    let length = project.end();
    let tracks: Vec<Value> = project.tracks.iter().map(track_summary).collect();
    Ok(Output::json(json!({
        "name": host.project_name(),
        "tempo_bpm": (tempo.tempo.to_bpm() * 1000.0).round() / 1000.0,
        "time_signature": format!(
            "{}/{}",
            tempo.time_signature.numerator, tempo.time_signature.denominator
        ),
        "key": project.scale.map(|s| s.name()),
        "loop": project.loop_region.map(|l| json!({
            "from": tick_to_beats(l.start),
            "to": tick_to_beats(l.end),
            "from_bar": bar_beat(l.start, tempo),
            "to_bar": bar_beat(l.end, tempo),
            "on": l.on,
        })),
        "length": tick_to_beats(length),
        "end_bar": bar_beat(length, tempo),
        "playing": transport.playing,
        "playhead": tick_to_beats(transport.playhead),
        "playhead_bar": bar_beat(transport.playhead, tempo),
        "tracks": tracks,
        "master": {
            "mixer": mixer_json(&project.master),
            "devices": project.master_devices.iter().map(device_summary).collect::<Vec<_>>(),
        },
        "selection": {
            "tracks": ids(&selection.tracks.iter().map(|t| t.0).collect::<Vec<_>>()),
            "clips": ids(&selection.clips.iter().map(|c| c.0).collect::<Vec<_>>()),
            "open_clip": selection.open_clip.map(|c| c.0),
            "notes": ids(&selection.notes.iter().map(|n| n.0).collect::<Vec<_>>()),
            "master": selection.master,
            "devices": ids(&selection.devices.iter().map(|d| d.0).collect::<Vec<_>>()),
        },
    })))
}

fn kind_name(track: &Track) -> &'static str {
    match track.kind {
        TrackKind::Audio { .. } => "audio",
        TrackKind::Midi { .. } => "midi",
        TrackKind::Group { .. } => "group",
    }
}

fn mixer_json(mixer: &Mixer) -> Value {
    json!({
        "volume_db": number(mixer.volume_db),
        "pan": number(mixer.pan),
        "mute": mixer.mute,
        "solo": mixer.solo,
    })
}

/// A MIDI track's instrument, by id and name.
fn instrument_summary(track: &Track) -> Option<Value> {
    let TrackKind::Midi {
        instrument, plugin, ..
    } = &track.kind
    else {
        return None;
    };
    let name = match (instrument.descriptor(), plugin) {
        (Some(d), _) => d.name.to_owned(),
        (None, Some(plugin)) => plugin.name.clone(),
        (None, None) => instrument.id.clone(),
    };
    let mut summary = json!({ "id": instrument.id, "name": name });
    if instrument.format != Format::BuiltIn {
        summary["format"] = json!(instrument.format.label());
    }
    if instrument.descriptor().is_none() {
        summary["missing"] = json!(true);
    }
    Some(summary)
}

fn device_summary(device: &Device) -> Value {
    let mut summary = json!({
        "id": device.id.0,
        "name": device.name(),
        "effect": device.effect.id,
    });
    if device.bypassed {
        summary["bypassed"] = json!(true);
    }
    // A Patcher's effects, which its macros (its own parameters) drive.
    if let Some(patch) = &device.patch {
        summary["inside"] = json!(patch.devices.iter().map(device_summary).collect::<Vec<_>>());
    }
    summary
}

/// A track in [`get_project`]'s tree: who it is and what's on it, without
/// parameters or clips' contents.
fn track_summary(track: &Track) -> Value {
    let mut summary = Map::new();
    summary.insert("id".into(), json!(track.id.0));
    summary.insert("name".into(), json!(track.name));
    summary.insert("color".into(), json!(color_hex(track.color)));
    summary.insert("kind".into(), json!(kind_name(track)));
    if let Some(instrument) = instrument_summary(track) {
        summary.insert("instrument".into(), instrument);
    }
    summary.insert("mixer".into(), mixer_json(&track.mixer));
    summary.insert(
        "devices".into(),
        json!(track.devices.iter().map(device_summary).collect::<Vec<_>>()),
    );
    if !track.is_group() {
        summary.insert("clips".into(), json!(track.clips().len()));
    }
    if !track.automation.is_empty() {
        summary.insert(
            "automated".into(),
            json!(
                track
                    .automation
                    .iter()
                    .map(|lane| target_json(&lane.target))
                    .collect::<Vec<_>>()
            ),
        );
    }
    if let TrackKind::Group { children, folded } = &track.kind {
        summary.insert("folded".into(), json!(folded));
        summary.insert(
            "children".into(),
            json!(children.iter().map(track_summary).collect::<Vec<_>>()),
        );
    }
    Value::Object(summary)
}

/// Which parameters a read lists.
#[derive(Clone, Copy)]
enum Listing<'a> {
    /// Those changed from their defaults, and how many there are in all:
    /// a track's overview, which can't afford the Drums' 2579.
    Changed,
    /// Every one, or those matching a filter; past [`PARAMS_SHOWN`], the
    /// changed ones and the groups to filter by instead.
    All(Option<&'a str>),
}

/// The most parameters a read lists in full: every effect's (the EQ has
/// the most, 219) and the Slicer's, not the Wavetable's or the Drums'.
const PARAMS_SHOWN: usize = 300;

/// Lists `params` (at `values`) into `out` as `listing` says. `groups`
/// names each parameter's section (see
/// `BuiltInInstrumentDescriptor::groups`), which a filter also matches.
fn list_params(
    out: &mut Value,
    params: &'static [ParamDescriptor],
    values: &[f32],
    groups: fn(usize) -> [&'static str; 2],
    listing: Listing<'_>,
) {
    let group = |i: usize| {
        let [outer, inner] = groups(i);
        match (outer.is_empty(), inner.is_empty()) {
            (true, _) => String::new(),
            (false, true) => outer.to_owned(),
            (false, false) => format!("{outer} / {inner}"),
        }
    };
    let changed = |i: usize| {
        values
            .get(i)
            .is_some_and(|v| *v != params[i].default_value())
    };
    let describe = |i: usize| {
        let mut param = describe_param(&params[i], values.get(i).copied().unwrap_or_default());
        let group = group(i);
        if !group.is_empty() {
            param["group"] = json!(group);
        }
        param
    };
    out["param_count"] = json!(params.len());
    let filter = match listing {
        Listing::Changed => {
            out["params_changed"] = json!(
                (0..params.len())
                    .filter(|&i| changed(i))
                    .map(describe)
                    .collect::<Vec<_>>()
            );
            return;
        }
        Listing::All(filter) => filter.map(str::to_lowercase),
    };
    let matching: Vec<usize> = (0..params.len())
        .filter(|&i| {
            filter.as_ref().is_none_or(|f| {
                params[i].id.to_lowercase().contains(f)
                    || params[i].name.to_lowercase().contains(f)
                    || group(i).to_lowercase().contains(f)
            })
        })
        .collect();
    if matching.len() <= PARAMS_SHOWN {
        out["params"] = json!(matching.into_iter().map(describe).collect::<Vec<_>>());
        return;
    }
    let mut sections: Vec<(String, usize)> = Vec::new();
    for &i in &matching {
        let name = group(i);
        match sections.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => sections.push((name, 1)),
        }
    }
    let changed: Vec<Value> = matching
        .iter()
        .copied()
        .filter(|&i| changed(i))
        .take(PARAMS_SHOWN)
        .map(describe)
        .collect();
    out["params_changed"] = json!(changed);
    out["param_groups"] = json!(
        sections
            .into_iter()
            .map(|(name, count)| json!({ "group": name, "params": count }))
            .collect::<Vec<_>>()
    );
    out["note"] = json!(format!(
        "{} parameters match, too many to list: these are the ones changed from their \
         defaults. Pass `filter` (a group, or a word of the parameters' names) to list others.",
        matching.len()
    ));
}

/// A track's instrument and its parameters.
fn instrument_full(track: &Track, listing: Listing<'_>) -> Option<Value> {
    let TrackKind::Midi {
        instrument, file, ..
    } = &track.kind
    else {
        return None;
    };
    let mut full = instrument_summary(track)?;
    if let Some(file) = file {
        full["file"] = json!(file.display().to_string());
    }
    if let Some(descriptor) = instrument.descriptor() {
        list_params(
            &mut full,
            descriptor.params,
            &track.instrument_values(),
            descriptor.groups,
            listing,
        );
    }
    Some(full)
}

/// An effect, its parameters, its bypass and its sidechain.
fn device_full(project: &Project, device: &Device, listing: Listing<'_>) -> Value {
    let mut full = device_summary(device);
    full["bypassed"] = json!(device.bypassed);
    if device.effect.format != Format::BuiltIn {
        full["format"] = json!(device.effect.format.label());
    }
    match device.effect.descriptor() {
        Some(descriptor) => {
            list_params(
                &mut full,
                descriptor.params,
                &device.values(),
                instruments::no_groups,
                listing,
            );
            if descriptor.sidechain {
                full["sidechain"] = match device.sidechain {
                    Some(source) => json!({
                        "track": source.0,
                        "name": project.track(source).map(|t| t.name.as_str()),
                    }),
                    None => Value::Null,
                };
            }
        }
        None => full["missing"] = json!(true),
    }
    full
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackArgs {
    track: TrackId,
}

fn get_track_schema() -> Value {
    schema::object(
        json!({ "track": schema::id("The track's id.") }),
        &["track"],
    )
}

fn find_track(project: &Project, id: TrackId) -> Result<&Track, ToolError> {
    project.track(id).ok_or_else(|| {
        ToolError(super::explain(
            &project::EditError::NoSuchTrack(id),
            project,
        ))
    })
}

fn get_track(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let TrackArgs { track: id } = args(v)?;
    let project = host.project();
    let track = find_track(project, id)?;
    let mut out = Map::new();
    out.insert("id".into(), json!(track.id.0));
    out.insert("name".into(), json!(track.name));
    out.insert("color".into(), json!(color_hex(track.color)));
    out.insert("kind".into(), json!(kind_name(track)));
    out.insert("parent".into(), json!(project.parent(id).map(|p| p.0)));
    out.insert("mixer".into(), mixer_json(&track.mixer));
    if let Some(instrument) = instrument_full(track, Listing::Changed) {
        out.insert("instrument".into(), instrument);
    }
    out.insert(
        "devices".into(),
        json!(
            track
                .devices
                .iter()
                .map(|d| device_full(project, d, Listing::Changed))
                .collect::<Vec<_>>()
        ),
    );
    if let TrackKind::Group { children, folded } = &track.kind {
        out.insert("folded".into(), json!(folded));
        out.insert(
            "children".into(),
            json!(
                children
                    .iter()
                    .map(|c| json!({ "id": c.id.0, "name": c.name, "kind": kind_name(c) }))
                    .collect::<Vec<_>>()
            ),
        );
    } else {
        out.insert(
            "clips".into(),
            json!(
                track
                    .clips()
                    .into_iter()
                    .map(|clip| clip_summary(project, clip))
                    .collect::<Vec<_>>()
            ),
        );
    }
    out.insert(
        "automation".into(),
        json!(
            track
                .automation
                .iter()
                .map(|lane| json!({
                    "target": target_json(&lane.target),
                    "points": lane.points.len(),
                }))
                .collect::<Vec<_>>()
        ),
    );
    // Effects elsewhere that listen to this track.
    let mut listened = Vec::new();
    let mut listen = |owner: Option<&Track>, devices: &[Device]| {
        for d in devices.iter().filter(|d| d.sidechain == Some(id)) {
            listened.push(json!({
                "device": d.id.0,
                "name": d.name(),
                "track": owner.map(|t| t.id.0),
            }));
        }
    };
    listen(None, &project.master_devices);
    project.visit(|t, _| listen(Some(t), &t.devices));
    out.insert("sidechain_to".into(), json!(listened));
    Ok(Output::json(Value::Object(out)))
}

/// A clip in a track's list: where it is and how long, not what's in it.
fn clip_summary(project: &Project, clip: ClipRef<'_>) -> Value {
    let tempo = &project.tempo;
    let end = project.clip_end(clip);
    let mut summary = json!({
        "id": clip.id().0,
        "name": clip.name(),
        "kind": match clip { ClipRef::Midi(_) => "midi", ClipRef::Audio(_) => "audio" },
        "start": tick_to_beats(clip.start()),
        "length": tick_to_beats(end - clip.start()),
        "start_bar": bar_beat(clip.start(), tempo),
        "end_bar": bar_beat(end, tempo),
    });
    if clip.muted() {
        summary["muted"] = json!(true);
    }
    if let ClipRef::Midi(midi) = clip {
        summary["notes"] = json!(midi.notes.len());
    }
    summary
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipArgs {
    clip: ClipId,
    from: Option<f64>,
    to: Option<f64>,
    cursor: Option<usize>,
}

fn get_clip_schema() -> Value {
    schema::object(
        json!({
            "clip": schema::id("The clip's id."),
            "from": schema::beats("Only notes starting at or after this, in beats from the clip's start."),
            "to": schema::beats("Only notes starting before this, in beats from the clip's start."),
            "cursor": schema::count("Where to read on from: the `cursor` the previous page returned."),
        }),
        &["clip"],
    )
}

fn get_clip(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: ClipArgs = args(v)?;
    let project = host.project();
    let (track_id, clip) = project.clip(a.clip).ok_or_else(|| {
        ToolError(super::explain(
            &project::EditError::NoSuchClip(a.clip),
            project,
        ))
    })?;
    let mut out = clip_summary(project, clip);
    out["track"] = json!(track_id.0);
    match clip {
        ClipRef::Midi(midi) => {
            let track = project.track(track_id).expect("the clip's track exists");
            if let Some(slices) = slicer_json(host, track) {
                out["slicer"] = slices;
            }
            midi_contents(&mut out, midi, &a)?;
        }
        ClipRef::Audio(audio) => {
            if a.from.is_some() || a.to.is_some() || a.cursor.is_some() {
                return Err("`from`, `to` and `cursor` are for a MIDI clip's notes".into());
            }
            audio_contents(&mut out, project, audio);
        }
    }
    Ok(Output::json(out))
}

fn midi_contents(out: &mut Value, clip: &MidiClip, a: &ClipArgs) -> Result<(), ToolError> {
    let from = a.from.map(beats_to_tick).transpose()?;
    let to = a.to.map(beats_to_tick).transpose()?;
    if let (Some(from), Some(to)) = (from, to)
        && to <= from
    {
        return Err("`to` must come after `from`".into());
    }
    out["offset"] = json!(tick_to_beats(clip.offset));
    // In the clip's time: 0 where the clip starts on the timeline.
    let at = |n: &Note| n.start - clip.offset;
    let mut notes: Vec<&Note> = clip
        .notes
        .iter()
        .filter(|n| from.is_none_or(|from| at(n) >= from) && to.is_none_or(|to| at(n) < to))
        .collect();
    notes.sort_by_key(|n| (n.start, n.pitch, n.id));
    let total = notes.len();
    let cursor = a.cursor.unwrap_or(0);
    if cursor > total {
        return Err(ToolError(format!(
            "the cursor is past the last note: there are {total}"
        )));
    }
    let page = &notes[cursor..total.min(cursor + NOTES_PAGE)];
    out["total"] = json!(total);
    out["notes"] = json!(page.iter().map(|n| note_json(n, at(n))).collect::<Vec<_>>());
    if cursor + page.len() < total {
        out["cursor"] = json!(cursor + page.len());
    }
    Ok(())
}

/// A note as reads show it; `muted`, `release_velocity` and `bend` only
/// when they aren't the usual (unmuted, unmeasured, unbent).
fn note_json(note: &Note, start: Tick) -> Value {
    let mut out = json!({
        "id": note.id.0,
        "start": tick_to_beats(start),
        "length": tick_to_beats(note.len),
        "pitch": note.pitch.get(),
        "name": pitch_name(note.pitch),
        "velocity": note.velocity.to_midi1(),
    });
    if note.release_velocity != Velocity::CENTER {
        out["release_velocity"] = json!(note.release_velocity.to_midi1());
    }
    if note.muted {
        out["muted"] = json!(true);
    }
    if !note.bend.is_empty() {
        out["bend"] = json!(
            note.bend
                .iter()
                .map(|p| json!({
                    "at": tick_to_beats(p.at),
                    "semitones": number(p.semitones),
                    "curve": number(p.curve),
                }))
                .collect::<Vec<_>>()
        );
    }
    out
}

fn audio_contents(out: &mut Value, project: &Project, clip: &AudioClip) {
    let source = project.source(clip.source);
    let rate = source.map_or(48_000, |s| s.sample_rate).max(1);
    let seconds = |frames: u64| (frames as f64 / rate as f64 * 1e4).round() / 1e4;
    out["file"] = json!(source.map(|s| s.path.display().to_string()));
    if let Some(source) = source {
        out["source"] = json!({
            "rate": source.sample_rate,
            "channels": source.channels,
            "seconds": seconds(source.frames),
        });
    }
    out["offset_seconds"] = json!(seconds(clip.source_offset));
    out["region_seconds"] = json!(seconds(clip.source_len));
    out["gain_db"] = number(clip.gain_db);
    out["fade_in"] = json!(seconds(clip.fade_in));
    out["fade_out"] = json!(seconds(clip.fade_out));
    out["fade_in_curve"] = number(clip.fade_in_curve);
    out["fade_out_curve"] = number(clip.fade_out_curve);
    out["reversed"] = json!(clip.reversed);
    out["looped"] = json!(clip.looped);
    out["stretch"] = json!({
        "mode": match clip.stretch.mode {
            StretchMode::Resample => "resample",
            StretchMode::Stretch => "stretch",
            StretchMode::Beats => "beats",
        },
        "speed": clip.stretch.speed,
        "transpose": clip.stretch.transpose,
        "original_bpm": clip.stretch.original_bpm,
        "sync": clip.stretch.sync,
    });
    out["channels"] = json!(match clip.channels {
        ChannelMode::Stereo => "stereo",
        ChannelMode::Mono => "mono",
        ChannelMode::Swap => "swap",
    });
    out["polarity_inverted"] = json!(clip.polarity_inverted);
}

/// A Slicer track's slices: where each starts in its loop and the key that
/// plays it (slice k on the root key plus k).
fn slicer_json(host: &dyn Host, track: &Track) -> Option<Value> {
    let TrackKind::Midi { instrument, .. } = &track.kind else {
        return None;
    };
    if instrument.format != Format::BuiltIn || instrument.id != "canopy.slicer" {
        return None;
    }
    let info = host.slicer(track.id);
    let rate = info.as_ref().map(|i| i.rate);
    let starts = info
        .as_ref()
        .map_or_else(|| track.slices().to_vec(), |i| i.slices.clone());
    let root = track.instrument_param("root").unwrap_or(60.0) as u32;
    Some(json!({
        "file": track.instrument_file().map(|f| f.display().to_string()),
        "slices": starts
            .iter()
            .enumerate()
            .map(|(k, &frame)| {
                let key = midi::Pitch::new((root + k as u32).min(127) as u8)
                    .expect("clamped to the MIDI range");
                json!({
                    "index": k,
                    "key": pitch_name(key),
                    "start_seconds": rate.map(|r| (frame as f64 / r.max(1) as f64 * 1e4).round() / 1e4),
                    "start_frame": frame,
                })
            })
            .collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceOrTrack {
    device: Option<DeviceId>,
    track: Option<TrackId>,
}

fn device_or_track_schema() -> Value {
    schema::object(
        json!({
            "device": schema::id("An effect's id."),
            "track": schema::id("A MIDI track's id, for its instrument."),
        }),
        &[],
    )
}

/// The effect or instrument a [`DeviceOrTrack`] names, checked to exist.
fn target(project: &Project, a: &DeviceOrTrack) -> Result<Target, ToolError> {
    match (a.device, a.track) {
        (Some(device), None) => {
            project.device(device).ok_or_else(|| {
                ToolError(super::explain(
                    &project::EditError::NoSuchDevice(device),
                    project,
                ))
            })?;
            Ok(Target::Device(device))
        }
        (None, Some(track)) => {
            let t = find_track(project, track)?;
            if !matches!(t.kind, TrackKind::Midi { .. }) {
                let devices: Vec<String> = t
                    .devices
                    .iter()
                    .map(|d| format!("{} {}", d.id, d.name()))
                    .collect();
                return Err(ToolError(format!(
                    "track {track} is {} track, without an instrument; give `device` for one of \
                     its effects ({})",
                    if t.is_group() { "a group" } else { "an audio" },
                    if devices.is_empty() {
                        "it has none".to_owned()
                    } else {
                        devices.join(", ")
                    }
                )));
            }
            Ok(Target::Instrument(track))
        }
        _ => Err("give `device` (an effect) or `track` (its instrument), one of them".into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceArgs {
    device: Option<DeviceId>,
    track: Option<TrackId>,
    filter: Option<String>,
}

fn get_device_schema() -> Value {
    schema::object(
        json!({
            "device": schema::id("An effect's id."),
            "track": schema::id("A MIDI track's id, for its instrument."),
            "filter": schema::string(
                "Only parameters whose id, name or group contains this (any case), for \
                 instruments with hundreds: \"Osc A\", \"LFO 1\", \"cutoff\"."
            ),
        }),
        &[],
    )
}

fn get_device(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: DeviceArgs = args(v)?;
    let project = host.project();
    let which = DeviceOrTrack {
        device: a.device,
        track: a.track,
    };
    let listing = Listing::All(a.filter.as_deref());
    match target(project, &which)? {
        Target::Device(id) => {
            let device = project.device(id).expect("checked by `target`");
            let (track, position) = project.find_device(id).expect("checked by `target`");
            let mut out = device_full(project, device, listing);
            out["chain"] = json!({
                "track": track.map(|t| t.0),
                "master": track.is_none(),
                "position": position,
            });
            Ok(Output::json(out))
        }
        Target::Instrument(id) => {
            let track = project.track(id).expect("checked by `target`");
            let mut out = instrument_full(track, listing).expect("a MIDI track");
            out["track"] = json!(id.0);
            Ok(Output::json(out))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum DeviceKind {
    Instrument,
    Effect,
    Plugin,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDevicesArgs {
    kind: Option<DeviceKind>,
    id: Option<String>,
    filter: Option<String>,
}

fn list_devices_schema() -> Value {
    schema::object(
        json!({
            "kind": schema::choice(&["instrument", "effect", "plugin"], "Only these."),
            "id": schema::string("One instrument's, effect's or plugin's id: its parameters."),
            "filter": schema::string(
                "With `id`: only parameters whose id, name or group contains this (any case)."
            ),
        }),
        &[],
    )
}

fn list_devices(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: ListDevicesArgs = args(v)?;
    let plugins = host.plugins();
    if a.filter.is_some() && a.id.is_none() {
        return Err("`filter` is for one device's parameters: give `id` too".into());
    }
    if let Some(id) = &a.id {
        if a.kind.is_some() {
            return Err("give `kind` or `id`, not both".into());
        }
        let listing = Listing::All(a.filter.as_deref());
        let defaults = |params: &[ParamDescriptor]| -> Vec<f32> {
            params.iter().map(|p| p.default_value()).collect()
        };
        if let Some(d) = instruments::find_builtin(id) {
            let mut out = json!({
                "id": d.id,
                "name": d.name,
                "kind": "instrument",
                "description": d.description,
                "plays_a_file": !matches!(d.data, instruments::DataKind::None),
            });
            list_params(&mut out, d.params, &defaults(d.params), d.groups, listing);
            return Ok(Output::json(out));
        }
        if let Some(d) = effects::find_builtin(id) {
            let mut out = json!({
                "id": d.id,
                "name": d.name,
                "kind": "effect",
                "category": d.category.name(),
                "description": d.description,
                "sidechain": d.sidechain,
            });
            list_params(
                &mut out,
                d.params,
                &defaults(d.params),
                instruments::no_groups,
                listing,
            );
            return Ok(Output::json(out));
        }
        if let Some(plugin) = plugins.iter().find(|p| &p.id == id) {
            let format = match plugin.format.as_str() {
                "CLAP" => Format::Clap,
                "VST3" => Format::Vst3,
                _ => Format::Vst2,
            };
            let params = if plugin.instrument {
                instruments::find_external(format, id).map(|d| d.params)
            } else {
                effects::find_external(format, id).map(|d| d.params)
            };
            let mut out = plugin_json(plugin);
            match params {
                Some(params) => list_params(
                    &mut out,
                    params,
                    &defaults(params),
                    instruments::no_groups,
                    listing,
                ),
                None => {
                    out["params"] =
                        json!("known once the plugin is on a track: get_device lists them then")
                }
            }
            return Ok(Output::json(out));
        }
        return Err(ToolError(format!(
            "there's no instrument, effect or plugin with id {id:?}; list_devices without `id` \
             lists them"
        )));
    }
    let wants = |kind: DeviceKind| {
        a.kind
            .as_ref()
            .is_none_or(|k| std::mem::discriminant(k) == std::mem::discriminant(&kind))
    };
    let mut out = Map::new();
    if wants(DeviceKind::Instrument) {
        out.insert(
            "instruments".into(),
            json!(
                instruments::builtins()
                    .iter()
                    .map(|d| json!({ "id": d.id, "name": d.name, "description": d.description }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    if wants(DeviceKind::Effect) {
        out.insert(
            "effects".into(),
            json!(
                effects::builtins()
                    .iter()
                    .map(|d| json!({
                        "id": d.id,
                        "name": d.name,
                        "category": d.category.name(),
                        "description": d.description,
                        "sidechain": d.sidechain,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    if wants(DeviceKind::Plugin) {
        out.insert(
            "plugins".into(),
            json!(plugins.iter().map(plugin_json).collect::<Vec<_>>()),
        );
    }
    Ok(Output::json(Value::Object(out)))
}

fn plugin_json(plugin: &crate::host::PluginEntry) -> Value {
    json!({
        "id": plugin.id,
        "name": plugin.name,
        "vendor": plugin.vendor,
        "format": plugin.format,
        "kind": if plugin.instrument { "instrument" } else { "effect" },
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomationArgs {
    track: TrackId,
    target: Option<Value>,
}

fn get_automation_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("The track's id."),
            "target": schema::target(
                "Only this lane: \"volume\", \"pan\", {\"instrument\": param}, \
                 {\"device\": id, \"param\": param} or {\"modulator\": id, \"param\": param}."
            ),
        }),
        &["track"],
    )
}

fn get_automation(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: AutomationArgs = args(v)?;
    let project = host.project();
    let track = find_track(project, a.track)?;
    let target = a.target.as_ref().map(parse_target).transpose()?;
    let lanes: Vec<_> = track
        .automation
        .iter()
        .filter(|lane| target.as_ref().is_none_or(|t| *t == lane.target))
        .collect();
    if let Some(target) = &target
        && lanes.is_empty()
    {
        let existing: Vec<String> = track
            .automation
            .iter()
            .map(|l| target_json(&l.target).to_string())
            .collect();
        return Err(ToolError(format!(
            "track {} has no automation for {}; {}",
            a.track,
            target_json(target),
            if existing.is_empty() {
                "it has none".to_owned()
            } else {
                format!("it has lanes for {}", existing.join(", "))
            }
        )));
    }
    let tempo = &project.tempo;
    let lanes: Vec<Value> = lanes
        .iter()
        .map(|lane| {
            let units = Units::of(project, a.track, &lane.target).unwrap_or(Units::Normalized);
            json!({
                "target": target_json(&lane.target),
                "unit": units.unit(),
                "points": lane
                    .points
                    .iter()
                    .map(|p| json!({
                        "at": tick_to_beats(p.tick),
                        "bar": bar_beat(p.tick, tempo),
                        "value": units.read(p.value),
                        "curve": number(p.curve),
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Output::json(json!({ "track": a.track.0, "lanes": lanes })))
}

fn list_presets(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: DeviceOrTrack = args(v)?;
    let target = target(host.project(), &a)?;
    let presets: Vec<Value> = host
        .presets(target)
        .iter()
        .map(|p| json!({ "name": p.name, "factory": p.factory }))
        .collect();
    Ok(Output::json(json!({ "presets": presets })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    last: Option<usize>,
}

/// Steps [`get_history`] lists when not told how many.
const HISTORY_SHOWN: usize = 50;

fn get_history_schema() -> Value {
    schema::object(
        json!({
            "last": schema::count("How many of the latest steps to list (50 if not given)."),
        }),
        &[],
    )
}

fn get_history(v: &Value, host: &mut dyn Host) -> Result<Output, ToolError> {
    let a: HistoryArgs = args(v)?;
    let history = host.history();
    let position = history.position();
    let now = std::time::SystemTime::now();
    let steps: Vec<_> = history.steps().collect();
    let shown = a.last.unwrap_or(HISTORY_SHOWN).min(steps.len());
    let first = steps.len() - shown;
    let listed: Vec<Value> = steps[first..]
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let number = first + i + 1;
            let age = now.duration_since(step.at).map_or(0, |d| d.as_secs());
            let mut out = json!({
                "step": number,
                "label": step.label,
                "age_seconds": age,
                "origin": "user",
                "done": number <= position,
            });
            if let Some(client) = step.origin.client() {
                out["origin"] = json!("llm");
                out["client"] = json!(client);
            }
            out
        })
        .collect();
    Ok(Output::json(json!({
        "position": position,
        "steps_total": steps.len(),
        "older_forgotten": history.forgot_steps(),
        "steps": listed,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{PluginEntry, Selection};
    use crate::testing::FakeHost;
    use crate::tools::{Outcome, call};
    use midi::{PPQ, Pitch};
    use project::{EffectRef, NewTrack, NoteId, TrackPosition};

    fn read(tool: &str, args: Value, host: &mut FakeHost) -> Value {
        match call(tool, &args, host) {
            Ok(Outcome::Done(output)) => output.json,
            Ok(Outcome::Waiting(_)) => panic!("reads answer at once"),
            Err(error) => panic!("{tool}: {error}"),
        }
    }

    fn failure(tool: &str, args: Value, host: &mut FakeHost) -> String {
        match call(tool, &args, host) {
            Err(error) => error.0,
            Ok(_) => panic!("{tool} with {args} should fail"),
        }
    }

    #[test]
    fn get_project_lists_the_track_tree_with_ids() {
        let mut project = Project::new();
        let bass = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project.rename_track(bass, "Bass").unwrap();
        let vocal = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        project.rename_track(vocal, "Vocal").unwrap();
        let group = project.group_tracks(&[bass, vocal]).unwrap();
        let eq = project
            .add_device(Some(vocal), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let limiter = project
            .add_device(None, EffectRef::builtin("canopy.limiter"), None)
            .unwrap();
        let clip = project
            .add_midi_clip(bass, Tick::new(4 * PPQ), Tick::new(4 * PPQ))
            .unwrap();
        let mut host = FakeHost::new(project);
        host.selection = Selection {
            tracks: vec![vocal],
            clips: vec![clip],
            open_clip: Some(clip),
            notes: vec![NoteId(99)],
            master: false,
            devices: vec![eq],
        };
        host.name = Some("Song".into());

        let out = read("get_project", json!({}), &mut host);
        assert_eq!(out["name"], json!("Song"));
        assert_eq!(out["tempo_bpm"], json!(120.0));
        assert_eq!(out["length"], json!(8.0));
        assert_eq!(out["end_bar"], json!("3.1"));
        let tracks = out["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1, "one group at the top");
        assert_eq!(tracks[0]["id"], json!(group.0));
        assert_eq!(tracks[0]["kind"], json!("group"));
        let children = tracks[0]["children"].as_array().unwrap();
        assert_eq!(children[0]["id"], json!(bass.0));
        assert_eq!(children[0]["name"], json!("Bass"));
        assert_eq!(children[0]["kind"], json!("midi"));
        assert_eq!(children[0]["instrument"]["name"], json!("Poly Synth"));
        assert_eq!(children[0]["clips"], json!(1));
        assert_eq!(children[1]["kind"], json!("audio"));
        assert_eq!(children[1]["devices"][0]["id"], json!(eq.0));
        assert_eq!(children[1]["devices"][0]["name"], json!("EQ"));
        assert_eq!(out["master"]["devices"][0]["id"], json!(limiter.0));
        assert_eq!(out["selection"]["tracks"], json!([vocal.0]));
        assert_eq!(out["selection"]["open_clip"], json!(clip.0));
        assert_eq!(out["selection"]["notes"], json!([99]));
        assert_eq!(out["selection"]["devices"], json!([eq.0]));
    }

    #[test]
    fn get_track_lists_clips_devices_and_sidechains() {
        let mut project = Project::new();
        let kick = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let bass = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let ducker = project
            .add_device(Some(bass), EffectRef::builtin("canopy.compressor"), None)
            .unwrap();
        project.set_device_sidechain(ducker, Some(kick)).unwrap();
        project
            .add_midi_clip(bass, Tick::new(2 * PPQ), Tick::new(PPQ))
            .unwrap();
        let mut host = FakeHost::new(project);

        let out = read("get_track", json!({ "track": bass.0 }), &mut host);
        assert_eq!(out["clips"][0]["start"], json!(2.0));
        assert_eq!(out["clips"][0]["start_bar"], json!("1.3"));
        assert_eq!(out["clips"][0]["length"], json!(1.0));
        assert_eq!(out["devices"][0]["sidechain"]["track"], json!(kick.0));
        // An overview lists only what was changed, and says how many there are.
        let compressor = &out["devices"][0];
        assert_eq!(compressor["param_count"], json!(12));
        assert_eq!(compressor["params_changed"], json!([]));
        assert!(out["instrument"]["param_count"].as_u64().unwrap() > 5);
        let kick = read("get_track", json!({ "track": kick.0 }), &mut host);
        assert_eq!(kick["sidechain_to"][0]["device"], json!(ducker.0));

        let error = failure("get_track", json!({ "track": 999 }), &mut host);
        assert!(error.contains("999"), "{error}");
    }

    fn note(id: u32, start: i64, pitch: u8) -> Note {
        Note {
            id: NoteId(id),
            start: Tick::new(start),
            len: Tick::new(PPQ / 4),
            pitch: Pitch::new(pitch).unwrap(),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        }
    }

    #[test]
    fn get_clip_pages_large_clips() {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let clip = project
            .add_midi_clip(track, Tick::ZERO, Tick::new(4500 * PPQ / 4))
            .unwrap();
        let notes: Vec<Note> = (0..4500)
            .map(|i| note(100_000 + i as u32, i * PPQ / 4, 60))
            .collect();
        project.set_notes(clip, notes).unwrap();
        let mut host = FakeHost::new(project);

        let first = read("get_clip", json!({ "clip": clip.0 }), &mut host);
        assert_eq!(first["notes"].as_array().unwrap().len(), 2000);
        assert_eq!(first["cursor"], json!(2000));
        assert_eq!(first["total"], json!(4500));
        let n = &first["notes"][1];
        assert_eq!(n["pitch"], json!(60));
        assert_eq!(n["name"], json!("C3"));
        assert_eq!(n["start"], json!(0.25));
        assert_eq!(n["length"], json!(0.25));
        assert_eq!(n["velocity"], json!(100));

        let second = read(
            "get_clip",
            json!({ "clip": clip.0, "cursor": 2000 }),
            &mut host,
        );
        assert_eq!(second["notes"].as_array().unwrap().len(), 2000);
        assert_eq!(second["cursor"], json!(4000));
        assert_eq!(second["notes"][0]["start"], json!(500.0));
        let third = read(
            "get_clip",
            json!({ "clip": clip.0, "cursor": 4000 }),
            &mut host,
        );
        assert_eq!(third["notes"].as_array().unwrap().len(), 500);
        assert!(third.get("cursor").is_none(), "the last page has no cursor");

        let window = read(
            "get_clip",
            json!({ "clip": clip.0, "from": 1, "to": 2 }),
            &mut host,
        );
        assert_eq!(window["total"], json!(4));
        assert_eq!(window["notes"][0]["start"], json!(1.0));

        let error = failure(
            "get_clip",
            json!({ "clip": clip.0, "cursor": 5000 }),
            &mut host,
        );
        assert!(error.contains("4500"), "{error}");
    }

    #[test]
    fn get_device_reports_units_ranges_and_choices() {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let eq = project
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let mut host = FakeHost::new(project);

        let out = read("get_device", json!({ "device": eq.0 }), &mut host);
        assert_eq!(out["name"], json!("EQ"));
        assert_eq!(out["chain"]["track"], json!(track.0));
        let params = out["params"].as_array().unwrap();
        let hz = params
            .iter()
            .find(|p| p["unit"] == json!("Hz"))
            .expect("an EQ band has a frequency");
        assert!(hz["min"].as_f64().unwrap() < hz["max"].as_f64().unwrap());
        assert_eq!(hz["log"], json!(true));
        assert!(hz["value"].is_number() && hz["default"].is_number());
        let choice = params
            .iter()
            .find(|p| p.get("choices").is_some())
            .expect("an EQ band has a shape");
        let labels = choice["choices"].as_array().unwrap();
        assert!(
            labels.contains(&choice["value"]),
            "a choice reads as its label"
        );

        let error = failure("get_device", json!({ "track": track.0 }), &mut host);
        assert!(
            error.contains("audio") && error.contains(&eq.0.to_string()),
            "{error}"
        );
        let error = failure("get_device", json!({}), &mut host);
        assert!(error.contains("`device`"), "{error}");
    }

    #[test]
    fn get_device_of_a_track_reports_its_instrument() {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let mut host = FakeHost::new(project);
        let out = read("get_device", json!({ "track": track.0 }), &mut host);
        assert_eq!(out["track"], json!(track.0));
        assert_eq!(out["id"], json!(instruments::DEFAULT_INSTRUMENT));
        let expected = instruments::find_builtin(instruments::DEFAULT_INSTRUMENT)
            .unwrap()
            .params
            .len();
        assert_eq!(out["params"].as_array().unwrap().len(), expected);
    }

    #[test]
    fn a_big_instrument_lists_its_groups_and_filters_by_them() {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project
            .set_instrument(
                track,
                instruments::InstrumentRef::builtin("canopy.wavetable"),
            )
            .unwrap();
        let descriptor = instruments::find_builtin("canopy.wavetable").unwrap();
        let (changed, min, max) = descriptor
            .params
            .iter()
            .find_map(|p| match p.kind {
                instruments::ParameterKind::Float { min, max, .. } => Some((p, min, max)),
                _ => None,
            })
            .expect("the Wavetable has knobs");
        project
            .set_instrument_params(track, [(changed.id.to_owned(), (min + max) / 2.0)].into())
            .unwrap();
        let mut host = FakeHost::new(project);

        let out = read("get_device", json!({ "track": track.0 }), &mut host);
        assert!(out.get("params").is_none(), "1468 are too many to list");
        assert_eq!(out["params_changed"][0]["id"], json!(changed.id));
        let groups = out["param_groups"].as_array().unwrap();
        assert!(
            groups.iter().any(|g| g["group"] == json!("Osc A")),
            "{groups:?}"
        );
        assert!(out["note"].as_str().unwrap().contains("filter"));

        let osc = read(
            "get_device",
            json!({ "track": track.0, "filter": "osc a" }),
            &mut host,
        );
        let params = osc["params"].as_array().unwrap();
        assert!(!params.is_empty() && params.len() <= PARAMS_SHOWN);
        assert!(params.iter().all(|p| p["group"] == json!("Osc A")
            || p["id"].as_str().unwrap().contains("osc_a")
            || p["name"].as_str().unwrap().to_lowercase().contains("osc a")));
    }

    #[test]
    fn every_builtin_effect_lists_all_its_parameters() {
        for effect in effects::builtins() {
            assert!(effect.params.len() <= PARAMS_SHOWN, "{}", effect.id);
        }
    }

    #[test]
    fn list_devices_includes_every_builtin() {
        let mut host = FakeHost::new(Project::new());
        host.plugins = vec![PluginEntry {
            name: "Vital".into(),
            vendor: "Vital Audio".into(),
            format: "CLAP".into(),
            instrument: true,
            id: "audio.vital.synth".into(),
        }];
        let out = read("list_devices", json!({}), &mut host);
        assert_eq!(
            out["instruments"].as_array().unwrap().len(),
            instruments::builtins().len()
        );
        assert_eq!(
            out["effects"].as_array().unwrap().len(),
            effects::builtins().len()
        );
        assert_eq!(out["plugins"].as_array().unwrap().len(), 1);

        let only = read("list_devices", json!({ "kind": "effect" }), &mut host);
        assert!(only.get("instruments").is_none() && only.get("plugins").is_none());

        let one = read(
            "list_devices",
            json!({ "id": "canopy.compressor" }),
            &mut host,
        );
        assert_eq!(one["kind"], json!("effect"));
        assert!(
            one["params"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == json!("threshold"))
        );
        let vital = read(
            "list_devices",
            json!({ "id": "audio.vital.synth" }),
            &mut host,
        );
        assert_eq!(vital["vendor"], json!("Vital Audio"));
        let error = failure("list_devices", json!({ "id": "canopy.kazoo" }), &mut host);
        assert!(error.contains("canopy.kazoo"), "{error}");
    }

    #[test]
    fn get_automation_reads_in_the_targets_units() {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project
            .set_automation(
                track,
                project::AutomationTarget::Volume,
                vec![project::AutomationPoint {
                    tick: Tick::new(PPQ),
                    value: project::volume_to_normalized(-6.0),
                    curve: 0.0,
                }],
            )
            .unwrap();
        let mut host = FakeHost::new(project);
        let out = read(
            "get_automation",
            json!({ "track": track.0, "target": "volume" }),
            &mut host,
        );
        let lane = &out["lanes"][0];
        assert_eq!(lane["unit"], json!("dB"));
        assert_eq!(lane["points"][0]["at"], json!(1.0));
        assert!((lane["points"][0]["value"].as_f64().unwrap() + 6.0).abs() < 1e-4);
        let error = failure(
            "get_automation",
            json!({ "track": track.0, "target": "pan" }),
            &mut host,
        );
        assert!(error.contains("\"volume\""), "{error}");
    }

    #[test]
    fn get_history_lists_steps_and_the_position() {
        let mut host = FakeHost::new(Project::new());
        for label in ["Add Track", "Rename"] {
            host.history
                .edit(&mut host.project, label, |p| {
                    p.add_track(NewTrack::Midi, TrackPosition::END)
                })
                .unwrap();
        }
        host.undo();
        let out = read("get_history", json!({}), &mut host);
        assert_eq!(out["position"], json!(1));
        let steps = out["steps"].as_array().unwrap();
        assert_eq!(steps[0]["label"], json!("Add Track"));
        assert_eq!(steps[0]["done"], json!(true));
        assert_eq!(steps[0]["origin"], json!("user"));
        assert_eq!(steps[1]["step"], json!(2));
        assert_eq!(steps[1]["done"], json!(false));
        let last = read("get_history", json!({ "last": 1 }), &mut host);
        assert_eq!(last["steps"].as_array().unwrap().len(), 1);
        assert_eq!(last["steps"][0]["label"], json!("Rename"));
    }

    #[test]
    fn get_history_says_which_steps_an_llm_made() {
        let mut host = FakeHost::new(Project::new());
        host.history
            .edit(&mut host.project, "Add Track", |p| {
                p.add_track(NewTrack::Midi, TrackPosition::END)
            })
            .unwrap();
        let llm = project::Origin::Llm {
            client: "claude-code".into(),
        };
        host.history
            .edit_from(llm, &mut host.project, "LLM: Add Track", |p| {
                p.add_track(NewTrack::Audio, TrackPosition::END)
            })
            .unwrap();
        let out = read("get_history", json!({}), &mut host);
        assert_eq!(out["steps"][0]["origin"], json!("user"));
        assert!(out["steps"][0].get("client").is_none());
        assert_eq!(out["steps"][1]["origin"], json!("llm"));
        assert_eq!(out["steps"][1]["client"], json!("claude-code"));
    }
}
