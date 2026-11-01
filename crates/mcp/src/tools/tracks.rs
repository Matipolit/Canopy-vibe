//! Tools for the song and its tracks: adding them, naming, coloring and
//! mixing them, arranging them into groups, and the song's tempo, key and
//! loop. All are edits a `batch` can hold.

use instruments::InstrumentRef;
use project::{
    LoopRegion, NOTE_NAMES, NewTrack, Project, Scale, ScaleMode, Tempo, TrackId, TrackPosition,
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{Kind, MAX_TRACKS, Plan, Tool, ToolError, args, explain, schema};
use crate::units::{Units, bar_beat, beats_to_tick, color};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "add_track",
        description: "Adds a MIDI, audio or group track, at the end of the song or `after` a \
                      track or `inside` a group (at its end). A MIDI track can start with a \
                      built-in `instrument` (list_devices lists them; plugins go on with \
                      set_instrument). Returns the new track's id.",
        schema: add_track_schema,
        examples: &[
            r#"{"kind": "midi", "name": "Bass", "color": "blue", "instrument": "canopy.wavetable"}"#,
            r#"{"kind": "audio", "name": "Vocal", "after": 12}"#,
            r#"{"kind": "group", "name": "Drums"}"#,
        ],
        kind: Kind::Edit(add_track),
    },
    Tool {
        name: "edit_track",
        description: "Changes a track's name, color or mixer (volume in dB, pan -1 left to 1 \
                      right, mute, solo), or folds a group. Only what is given changes.",
        schema: edit_track_schema,
        examples: &[
            r##"{"track": 12, "name": "Sub Bass", "color": "#3070e0"}"##,
            r#"{"track": 12, "volume_db": -6, "pan": -0.3}"#,
            r#"{"track": 7, "mute": true}"#,
        ],
        kind: Kind::Edit(edit_track),
    },
    Tool {
        name: "arrange_tracks",
        description: "Moves tracks (`after` a track or `inside` a group; neither moves them to \
                      the end), groups them into a new group (`name` names it; returns its \
                      id), ungroups groups (their tracks take their place) or deletes tracks \
                      with everything on them. At most 64 tracks at a time.",
        schema: arrange_tracks_schema,
        examples: &[
            r#"{"op": "group", "tracks": [12, 13, 14], "name": "Drums"}"#,
            r#"{"op": "move", "tracks": [20], "inside": 15}"#,
            r#"{"op": "move", "tracks": [20, 21], "after": 12}"#,
            r#"{"op": "ungroup", "tracks": [15]}"#,
            r#"{"op": "delete", "tracks": [9]}"#,
        ],
        kind: Kind::Edit(arrange_tracks),
    },
    Tool {
        name: "edit_song",
        description: "Sets the song's tempo (BPM), its key (\"A minor\", \"F# Dorian\"; null \
                      clears it) or its loop (from and to in beats; null clears it).",
        schema: edit_song_schema,
        examples: &[
            r#"{"tempo_bpm": 128}"#,
            r#"{"key": "A minor"}"#,
            r#"{"loop": {"from": 16, "to": 32}}"#,
            r#"{"key": null, "loop": null}"#,
        ],
        kind: Kind::Edit(edit_song),
    },
];

/// Reads a field that may be given as `null`: absent is `None`, `null` is
/// `Some(None)`, which says "clear it".
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum NewKind {
    Midi,
    Audio,
    Group,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddTrackArgs {
    kind: NewKind,
    name: Option<String>,
    color: Option<String>,
    instrument: Option<String>,
    after: Option<TrackId>,
    inside: Option<TrackId>,
}

fn add_track_schema() -> Value {
    schema::object(
        json!({
            "kind": schema::choice(&["midi", "audio", "group"], "What kind of track."),
            "name": schema::string("Its name (canopy numbers it if not given)."),
            "color": schema::string("\"#rrggbb\" or a name: red, orange, yellow, green, teal, cyan, blue, indigo, purple, pink, brown, grey."),
            "instrument": schema::string("For a MIDI track: a built-in instrument's id, as list_devices gives it."),
            "after": schema::id("Put it right after this track, in the same group."),
            "inside": schema::id("Put it at the end of this group."),
        }),
        &["kind"],
    )
}

/// Where a track goes: `after` a track, `inside` a group, or the end.
fn position(
    project: &Project,
    after: Option<TrackId>,
    inside: Option<TrackId>,
) -> Result<TrackPosition, ToolError> {
    match (after, inside) {
        (Some(_), Some(_)) => Err("give `after` or `inside`, not both".into()),
        (Some(after), None) => {
            existing(project, after)?;
            Ok(TrackPosition::after(project, after))
        }
        (None, Some(group)) => {
            if !existing(project, group)?.is_group() {
                return Err(ToolError(format!(
                    "track {group} isn't a group; only groups hold tracks ({})",
                    groups(project)
                )));
            }
            Ok(TrackPosition {
                parent: Some(group),
                before: None,
            })
        }
        (None, None) => Ok(TrackPosition::END),
    }
}

fn existing(project: &Project, id: TrackId) -> Result<&project::Track, ToolError> {
    project
        .track(id)
        .ok_or_else(|| ToolError(explain(&project::EditError::NoSuchTrack(id), project)))
}

/// "groups: 4 Drums, 9 Vox", or "there are no groups".
fn groups(project: &Project) -> String {
    let mut found = Vec::new();
    project.visit(|t, _| {
        if t.is_group() {
            found.push(format!("{} {}", t.id, t.name));
        }
    });
    if found.is_empty() {
        "there are no groups; arrange_tracks makes one".to_owned()
    } else {
        format!("groups: {}", found.join(", "))
    }
}

fn track_name(name: &str) -> Result<String, ToolError> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a track's name can't be empty".into());
    }
    Ok(name.to_owned())
}

fn add_track(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: AddTrackArgs = args(v)?;
    let at = position(project, a.after, a.inside)?;
    let name = a.name.as_deref().map(track_name).transpose()?;
    let color = a.color.map(|c| color(&json!(c))).transpose()?;
    let instrument = match (a.instrument, a.kind) {
        (None, _) => None,
        (Some(id), NewKind::Midi) => {
            if instruments::builtin::find_builtin(&id).is_none() {
                let ids: Vec<&str> = instruments::builtin::builtins()
                    .iter()
                    .map(|d| d.id)
                    .collect();
                return Err(ToolError(format!(
                    "there's no built-in instrument {id:?}; they are {} (plugins go on with \
                     set_instrument)",
                    ids.join(", ")
                )));
            }
            Some(InstrumentRef::builtin(&id))
        }
        (Some(_), _) => return Err("only MIDI tracks have an instrument".into()),
    };
    let (kind, what) = match a.kind {
        NewKind::Midi => (NewTrack::Midi, "MIDI track"),
        NewKind::Audio => (NewTrack::Audio, "audio track"),
        NewKind::Group => (NewTrack::Group, "group"),
    };
    let label = match &name {
        Some(name) => format!("Add {what} {name}"),
        None => format!("Add {what}"),
    };
    Ok(Plan {
        label,
        apply: Box::new(move |p| {
            let id = p.add_track(kind, at)?;
            if let Some(name) = &name {
                p.rename_track(id, name)?;
            }
            if let Some(color) = color {
                p.set_track_color(id, color)?;
            }
            if let Some(instrument) = instrument {
                p.set_instrument(id, instrument)?;
            }
            let name = &p.track(id).expect("just added").name;
            Ok(json!({
                "id": id.0,
                "summary": format!("added {what} {id} {name:?}"),
            }))
        }),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditTrackArgs {
    track: TrackId,
    name: Option<String>,
    color: Option<String>,
    volume_db: Option<f64>,
    pan: Option<f64>,
    mute: Option<bool>,
    solo: Option<bool>,
    folded: Option<bool>,
}

fn edit_track_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("The track's id."),
            "name": schema::string("A new name."),
            "color": schema::string("\"#rrggbb\" or a name: red, orange, yellow, green, teal, cyan, blue, indigo, purple, pink, brown, grey."),
            "volume_db": schema::number("The fader, -60–6 dB (0 is unity)."),
            "pan": schema::number("-1 (left) to 1 (right), 0 the middle."),
            "mute": schema::boolean("Silences the track."),
            "solo": schema::boolean("Silences every track not soloed."),
            "folded": schema::boolean("For a group: hides its tracks in the arrangement."),
        }),
        &["track"],
    )
}

fn edit_track(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: EditTrackArgs = args(v)?;
    let track = existing(project, a.track)?;
    let id = a.track;
    let name = a.name.as_deref().map(track_name).transpose()?;
    let color = a.color.map(|c| color(&json!(c))).transpose()?;
    if let Some(db) = a.volume_db {
        Units::Volume.write(&json!(db))?;
    }
    if let Some(pan) = a.pan {
        Units::Pan.write(&json!(pan))?;
    }
    if a.folded.is_some() && !track.is_group() {
        return Err(ToolError(format!(
            "track {id} ({:?}) isn't a group; only groups fold",
            track.name
        )));
    }
    let mut changed = Vec::new();
    for (given, what) in [
        (name.is_some(), "name"),
        (color.is_some(), "color"),
        (a.volume_db.is_some(), "volume"),
        (a.pan.is_some(), "pan"),
        (a.mute.is_some(), "mute"),
        (a.solo.is_some(), "solo"),
        (a.folded.is_some(), "fold"),
    ] {
        if given {
            changed.push(what);
        }
    }
    if changed.is_empty() {
        return Err(
            "nothing to change: give a name, color, volume_db, pan, mute, solo or folded".into(),
        );
    }
    let changed = changed.join(", ");
    let label = format!("Edit {} ({changed})", track.name);
    let mut mixer = track.mixer;
    if let Some(db) = a.volume_db {
        mixer.volume_db = db as f32;
    }
    if let Some(pan) = a.pan {
        mixer.pan = pan as f32;
    }
    if let Some(mute) = a.mute {
        mixer.mute = mute;
    }
    if let Some(solo) = a.solo {
        mixer.solo = solo;
    }
    let (folded, mixer_changed) = (a.folded, mixer != track.mixer);
    Ok(Plan {
        label,
        apply: Box::new(move |p| {
            if let Some(name) = &name {
                p.rename_track(id, name)?;
            }
            if let Some(color) = color {
                p.set_track_color(id, color)?;
            }
            if mixer_changed {
                p.set_mixer(id, mixer)?;
            }
            if let Some(folded) = folded {
                p.set_folded(id, folded)?;
            }
            let name = &p.track(id).expect("checked").name;
            Ok(json!({ "summary": format!("set track {id} {name:?}'s {changed}") }))
        }),
    })
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ArrangeOp {
    Move,
    Group,
    Ungroup,
    Delete,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArrangeArgs {
    op: ArrangeOp,
    tracks: Vec<TrackId>,
    after: Option<TrackId>,
    inside: Option<TrackId>,
    name: Option<String>,
}

fn arrange_tracks_schema() -> Value {
    schema::object(
        json!({
            "op": schema::choice(&["move", "group", "ungroup", "delete"], "What to do."),
            "tracks": schema::ids("The tracks (for ungroup, the groups), at most 64."),
            "after": schema::id("For move: right after this track, in its group."),
            "inside": schema::id("For move: at the end of this group."),
            "name": schema::string("For group: the new group's name."),
        }),
        &["op", "tracks"],
    )
}

fn arrange_tracks(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: ArrangeArgs = args(v)?;
    if a.tracks.is_empty() {
        return Err("no tracks given".into());
    }
    if a.tracks.len() > MAX_TRACKS {
        return Err(ToolError(format!(
            "{} tracks is too many for one call; at most {MAX_TRACKS} (batch several calls for more)",
            a.tracks.len()
        )));
    }
    for &id in &a.tracks {
        existing(project, id)?;
    }
    if a.op != ArrangeOp::Move && (a.after.is_some() || a.inside.is_some()) {
        return Err("`after` and `inside` are for move".into());
    }
    if a.op != ArrangeOp::Group && a.name.is_some() {
        return Err("`name` is for group".into());
    }
    let tracks = a.tracks;
    let count = |n: usize| {
        if n == 1 {
            "1 track".to_owned()
        } else {
            format!("{n} tracks")
        }
    };
    let n = tracks.len();
    match a.op {
        ArrangeOp::Move => {
            let at = position(project, a.after, a.inside)?;
            Ok(Plan {
                label: format!("Move {}", count(n)),
                apply: Box::new(move |p| {
                    p.move_tracks(&tracks, at)?;
                    Ok(json!({ "summary": format!("moved {}", count(n)) }))
                }),
            })
        }
        ArrangeOp::Group => {
            let name = a.name.as_deref().map(track_name).transpose()?;
            Ok(Plan {
                label: match &name {
                    Some(name) => format!("Group {} as {name}", count(n)),
                    None => format!("Group {}", count(n)),
                },
                apply: Box::new(move |p| {
                    let group = p.group_tracks(&tracks)?;
                    if let Some(name) = &name {
                        p.rename_track(group, name)?;
                    }
                    let name = &p.track(group).expect("just made").name;
                    Ok(json!({
                        "id": group.0,
                        "summary": format!("grouped {} into group {group} {name:?}", count(n)),
                    }))
                }),
            })
        }
        ArrangeOp::Ungroup => {
            for &id in &tracks {
                let track = existing(project, id)?;
                if !track.is_group() {
                    return Err(ToolError(format!(
                        "track {id} ({:?}) isn't a group; {}",
                        track.name,
                        groups(project)
                    )));
                }
            }
            Ok(Plan {
                label: if n == 1 {
                    format!("Ungroup {}", existing(project, tracks[0])?.name)
                } else {
                    format!("Ungroup {n} groups")
                },
                apply: Box::new(move |p| {
                    for &id in &tracks {
                        p.ungroup(id)?;
                    }
                    Ok(json!({ "summary": format!("ungrouped {n}") }))
                }),
            })
        }
        ArrangeOp::Delete => Ok(Plan {
            label: if n == 1 {
                format!("Delete {}", existing(project, tracks[0])?.name)
            } else {
                format!("Delete {n} tracks")
            },
            apply: Box::new(move |p| {
                p.delete_tracks(&tracks)?;
                Ok(json!({ "summary": format!("deleted {}", count(n)) }))
            }),
        }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditSongArgs {
    tempo_bpm: Option<f64>,
    #[serde(default, deserialize_with = "nullable")]
    key: Option<Option<String>>,
    #[serde(default, rename = "loop", deserialize_with = "nullable")]
    loop_region: Option<Option<LoopArgs>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopArgs {
    from: f64,
    to: f64,
    on: Option<bool>,
}

fn edit_song_schema() -> Value {
    schema::object(
        json!({
            "tempo_bpm": schema::number("Beats per minute, 20–999."),
            "key": {
                "type": ["string", "null"],
                "description": "A root and a scale: \"A minor\", \"F# Dorian\", \"Eb major pentatonic\". null clears it.",
            },
            "loop": {
                "type": ["object", "null"],
                "properties": {
                    "from": schema::beats("Where it starts, in beats."),
                    "to": schema::beats("Where it ends, in beats."),
                    "on": schema::boolean("Whether playback goes round it (true if not given)."),
                },
                "required": ["from", "to"],
                "additionalProperties": false,
                "description": "The stretch playback goes round. null clears it.",
            },
        }),
        &[],
    )
}

/// A key from "A minor", "F# Dorian", "Bb major pentatonic": a note name,
/// then one of the scales canopy knows, in any case.
pub(super) fn parse_key(text: &str) -> Result<Scale, String> {
    let modes: Vec<&str> = ScaleMode::ALL.iter().map(|m| m.name()).collect();
    let help = || {
        format!(
            "a key is a root note and a scale, like \"A minor\" or \"F# Dorian\"; the scales are {}",
            modes.join(", ")
        )
    };
    let text = text.trim();
    let (root, mode) = text
        .split_once(char::is_whitespace)
        .ok_or_else(|| format!("{text:?} isn't a key; {}", help()))?;
    let root = root_class(root).ok_or_else(|| format!("{root:?} isn't a note name; {}", help()))?;
    let wanted = mode.split_whitespace().collect::<Vec<_>>().join(" ");
    let mode = ScaleMode::ALL
        .into_iter()
        .find(|m| m.name().eq_ignore_ascii_case(&wanted))
        .ok_or_else(|| format!("{wanted:?} isn't a scale canopy knows; {}", help()))?;
    Ok(Scale::new(root, mode))
}

/// A pitch class from a note name without an octave: "C" is 0, "F#" 6,
/// "Bb" 10.
fn root_class(name: &str) -> Option<u8> {
    let mut chars = name.chars();
    let letter = chars.next()?.to_ascii_uppercase().to_string();
    let natural = NOTE_NAMES.iter().position(|n| *n == letter)? as i32;
    let shift: i32 = chars
        .map(|c| match c {
            '#' | '♯' => Some(1),
            'b' | '♭' => Some(-1),
            _ => None,
        })
        .sum::<Option<i32>>()?;
    Some((natural + shift).rem_euclid(12) as u8)
}

fn edit_song(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: EditSongArgs = args(v)?;
    let mut changed = Vec::new();
    if let Some(bpm) = a.tempo_bpm
        && !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&bpm)
    {
        return Err(ToolError(format!(
            "the tempo is {}–{} BPM; {bpm} is out of range",
            Tempo::MIN_BPM,
            Tempo::MAX_BPM
        )));
    }
    if let Some(bpm) = a.tempo_bpm {
        changed.push(format!("tempo {bpm} BPM"));
    }
    let key = match a.key {
        None => None,
        Some(None) => Some(None),
        Some(Some(text)) => Some(Some(parse_key(&text)?)),
    };
    match key {
        Some(Some(scale)) => changed.push(format!("key {}", scale.name())),
        Some(None) => changed.push("no key".to_owned()),
        None => {}
    }
    let region = match a.loop_region {
        None => None,
        Some(None) => Some(None),
        Some(Some(l)) => {
            let (from, to) = (beats_to_tick(l.from)?, beats_to_tick(l.to)?);
            if to <= from {
                return Err("the loop's `to` must come after its `from`".into());
            }
            Some(Some(LoopRegion {
                start: from,
                end: to,
                on: l.on.unwrap_or(true),
            }))
        }
    };
    match region {
        Some(Some(l)) => changed.push(format!(
            "loop {}–{}",
            bar_beat(l.start, &project.tempo),
            bar_beat(l.end, &project.tempo)
        )),
        Some(None) => changed.push("no loop".to_owned()),
        None => {}
    }
    if changed.is_empty() {
        return Err("nothing to change: give tempo_bpm, key or loop".into());
    }
    let changed = changed.join(", ");
    Ok(Plan {
        label: format!("Song: {changed}"),
        apply: Box::new(move |p| {
            if let Some(bpm) = a.tempo_bpm {
                p.set_tempo_bpm(bpm);
            }
            if let Some(key) = key {
                p.set_scale(key)?;
            }
            if let Some(region) = region {
                p.set_loop(region)?;
            }
            Ok(json!({ "summary": format!("set the song's {changed}") }))
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use crate::tools::{Outcome, call};
    use midi::Tick;
    use project::TrackKind;

    fn beats(b: f64) -> Tick {
        beats_to_tick(b).unwrap()
    }

    fn done(tool: &str, args: Value, host: &mut FakeHost) -> Value {
        match call(tool, &args, host) {
            Ok(Outcome::Done(output)) => output.json,
            Ok(Outcome::Waiting(_)) => panic!("edits answer at once"),
            Err(error) => panic!("{tool}: {error}"),
        }
    }

    fn failure(tool: &str, args: Value, host: &mut FakeHost) -> String {
        match call(tool, &args, host) {
            Err(error) => error.0,
            Ok(_) => panic!("{tool} with {args} should fail"),
        }
    }

    fn id(v: &Value) -> TrackId {
        TrackId(v["id"].as_u64().expect("an id") as u32)
    }

    fn names(project: &Project) -> Vec<String> {
        project.tracks.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn add_track_puts_it_where_asked() {
        let mut host = FakeHost::new(Project::new());
        let drums = id(&done(
            "add_track",
            json!({ "kind": "group", "name": "Drums" }),
            &mut host,
        ));
        let bass = id(&done(
            "add_track",
            json!({ "kind": "midi", "name": "Bass", "color": "blue", "instrument": "canopy.wavetable" }),
            &mut host,
        ));
        let vocal = id(&done(
            "add_track",
            json!({ "kind": "audio", "name": "Vocal" }),
            &mut host,
        ));
        // Right after the group, before the bass.
        let pad = id(&done(
            "add_track",
            json!({ "kind": "midi", "name": "Pad", "after": drums.0 }),
            &mut host,
        ));
        let kick = id(&done(
            "add_track",
            json!({ "kind": "audio", "name": "Kick", "inside": drums.0 }),
            &mut host,
        ));
        let project = &host.project;
        assert_eq!(names(project), ["Drums", "Pad", "Bass", "Vocal"]);
        assert_eq!(project.parent(kick), Some(drums));
        let bass_track = project.track(bass).unwrap();
        assert_eq!(bass_track.color, project::Color::PALETTE[6]);
        let TrackKind::Midi { instrument, .. } = &bass_track.kind else {
            panic!("a MIDI track");
        };
        assert_eq!(instrument.id, "canopy.wavetable");
        assert!(project.track(pad).is_some() && project.track(vocal).is_some());
        assert_eq!(host.history.position(), 5, "one step each");
        assert_eq!(host.history.undo_label(), Some("LLM: Add audio track Kick"));

        let error = failure(
            "add_track",
            json!({ "kind": "midi", "inside": bass.0 }),
            &mut host,
        );
        assert!(
            error.contains("isn't a group") && error.contains("Drums"),
            "{error}"
        );
        let error = failure(
            "add_track",
            json!({ "kind": "audio", "instrument": "canopy.wavetable" }),
            &mut host,
        );
        assert!(error.contains("only MIDI tracks"), "{error}");
        let error = failure(
            "add_track",
            json!({ "kind": "midi", "instrument": "canopy.kazoo" }),
            &mut host,
        );
        assert!(error.contains("canopy.wavetable"), "{error}");
        let error = failure(
            "add_track",
            json!({ "kind": "midi", "after": 999 }),
            &mut host,
        );
        assert!(error.contains("999"), "{error}");
        assert_eq!(host.history.position(), 5, "failures add no steps");
    }

    #[test]
    fn edit_track_sets_only_what_is_given() {
        let mut host = FakeHost::new(Project::new());
        let track = id(&done("add_track", json!({ "kind": "midi" }), &mut host));
        let before = host.project.track(track).unwrap().clone();
        done(
            "edit_track",
            json!({ "track": track.0, "volume_db": -6, "mute": true }),
            &mut host,
        );
        let after = host.project.track(track).unwrap();
        assert_eq!(after.mixer.volume_db, -6.0);
        assert!(after.mixer.mute);
        assert_eq!(after.mixer.pan, before.mixer.pan);
        assert!(!after.mixer.solo);
        assert_eq!(after.name, before.name);
        assert_eq!(after.color, before.color);

        let out = done(
            "edit_track",
            json!({ "track": track.0, "name": "Lead", "color": "#ff8000" }),
            &mut host,
        );
        let after = host.project.track(track).unwrap();
        assert_eq!(after.name, "Lead");
        assert_eq!(after.color, project::Color::rgb(255, 128, 0));
        assert_eq!(after.mixer.volume_db, -6.0, "the mixer stays");
        assert!(out["summary"].as_str().unwrap().contains("Lead"));
        assert_eq!(host.history.position(), 3);

        let error = failure("edit_track", json!({ "track": track.0 }), &mut host);
        assert!(error.contains("nothing to change"), "{error}");
        let error = failure(
            "edit_track",
            json!({ "track": track.0, "folded": true }),
            &mut host,
        );
        assert!(error.contains("only groups fold"), "{error}");
    }

    #[test]
    fn colors_and_volumes_are_checked() {
        let mut host = FakeHost::new(Project::new());
        let track = id(&done("add_track", json!({ "kind": "audio" }), &mut host));
        let before = host.project.clone();
        let error = failure(
            "edit_track",
            json!({ "track": track.0, "pan": 2.0 }),
            &mut host,
        );
        assert!(error.contains("-1–1"), "{error}");
        let error = failure(
            "edit_track",
            json!({ "track": track.0, "volume_db": 12 }),
            &mut host,
        );
        assert!(error.contains("-60–6 dB"), "{error}");
        let error = failure(
            "edit_track",
            json!({ "track": track.0, "color": "mauve" }),
            &mut host,
        );
        assert!(error.contains("#rrggbb"), "{error}");
        let error = failure(
            "edit_track",
            json!({ "track": track.0, "name": "  " }),
            &mut host,
        );
        assert!(error.contains("empty"), "{error}");
        // A good change beside a bad one doesn't happen either.
        failure(
            "edit_track",
            json!({ "track": track.0, "name": "Kept?", "pan": -3 }),
            &mut host,
        );
        assert!(host.project == before);
    }

    #[test]
    fn arrange_groups_and_ungroups() {
        let mut host = FakeHost::new(Project::new());
        let tracks: Vec<TrackId> = ["Kick", "Snare", "Bass"]
            .iter()
            .map(|name| {
                id(&done(
                    "add_track",
                    json!({ "kind": "audio", "name": name }),
                    &mut host,
                ))
            })
            .collect();
        let group = id(&done(
            "arrange_tracks",
            json!({ "op": "group", "tracks": [tracks[0].0, tracks[1].0], "name": "Drums" }),
            &mut host,
        ));
        assert_eq!(names(&host.project), ["Drums", "Bass"]);
        assert_eq!(host.project.track(group).unwrap().children().len(), 2);

        done(
            "arrange_tracks",
            json!({ "op": "move", "tracks": [tracks[2].0], "inside": group.0 }),
            &mut host,
        );
        assert_eq!(host.project.parent(tracks[2]), Some(group));
        done(
            "arrange_tracks",
            json!({ "op": "move", "tracks": [tracks[2].0], "after": group.0 }),
            &mut host,
        );
        assert_eq!(names(&host.project), ["Drums", "Bass"]);

        let error = failure(
            "arrange_tracks",
            json!({ "op": "ungroup", "tracks": [tracks[2].0] }),
            &mut host,
        );
        assert!(error.contains("isn't a group"), "{error}");
        done(
            "arrange_tracks",
            json!({ "op": "ungroup", "tracks": [group.0] }),
            &mut host,
        );
        assert_eq!(names(&host.project), ["Kick", "Snare", "Bass"]);

        done(
            "arrange_tracks",
            json!({ "op": "delete", "tracks": [tracks[1].0] }),
            &mut host,
        );
        assert_eq!(names(&host.project), ["Kick", "Bass"]);
        let error = failure(
            "arrange_tracks",
            json!({ "op": "delete", "tracks": [tracks[0].0], "after": tracks[2].0 }),
            &mut host,
        );
        assert!(error.contains("for move"), "{error}");
    }

    #[test]
    fn more_than_64_tracks_is_an_error() {
        let mut host = FakeHost::new(Project::new());
        let mut ids = Vec::new();
        for _ in 0..65 {
            ids.push(
                host.project
                    .add_track(NewTrack::Audio, TrackPosition::END)
                    .unwrap()
                    .0,
            );
        }
        let error = failure(
            "arrange_tracks",
            json!({ "op": "delete", "tracks": ids }),
            &mut host,
        );
        assert!(error.contains("64"), "{error}");
        assert_eq!(host.project.tracks.len(), 65);
        done(
            "arrange_tracks",
            json!({ "op": "delete", "tracks": &ids[..64] }),
            &mut host,
        );
        assert_eq!(host.project.tracks.len(), 1);
    }

    #[test]
    fn edit_song_sets_tempo_key_and_loop() {
        let mut host = FakeHost::new(Project::new());
        done(
            "edit_song",
            json!({ "tempo_bpm": 128, "key": "f# dorian", "loop": { "from": 16, "to": 32 } }),
            &mut host,
        );
        let project = &host.project;
        assert!((project.tempo.tempo.to_bpm() - 128.0).abs() < 1e-3);
        assert_eq!(project.scale, Some(Scale::new(6, ScaleMode::Dorian)));
        let region = project.loop_region.unwrap();
        assert_eq!((region.start, region.end), (beats(16.0), beats(32.0)));
        assert!(region.on);
        assert_eq!(host.history.position(), 1, "one step for all three");

        done("edit_song", json!({ "key": "Bb minor" }), &mut host);
        assert_eq!(host.project.scale, Some(Scale::new(10, ScaleMode::Minor)));
        assert!(host.project.loop_region.is_some(), "the loop stays");
        done("edit_song", json!({ "key": null, "loop": null }), &mut host);
        assert_eq!(host.project.scale, None);
        assert_eq!(host.project.loop_region, None);

        let error = failure("edit_song", json!({ "tempo_bpm": 5000 }), &mut host);
        assert!(error.contains("20–999"), "{error}");
        let error = failure("edit_song", json!({ "key": "H major" }), &mut host);
        assert!(error.contains("\"H\""), "{error}");
        let error = failure("edit_song", json!({ "key": "C bebop" }), &mut host);
        assert!(error.contains("Dorian"), "{error}");
        let error = failure(
            "edit_song",
            json!({ "loop": { "from": 8, "to": 4 } }),
            &mut host,
        );
        assert!(error.contains("after"), "{error}");
        assert!(failure("edit_song", json!({}), &mut host).contains("nothing to change"));
    }

    #[test]
    fn keys_parse_in_any_case_with_flats_and_sharps() {
        assert_eq!(parse_key("A minor"), Ok(Scale::new(9, ScaleMode::Minor)));
        assert_eq!(
            parse_key("eb Major Pentatonic"),
            Ok(Scale::new(3, ScaleMode::MajorPentatonic))
        );
        assert_eq!(parse_key("C# blues"), Ok(Scale::new(1, ScaleMode::Blues)));
        assert_eq!(parse_key("Cb major"), Ok(Scale::new(11, ScaleMode::Major)));
        assert!(parse_key("minor").is_err());
    }
}
