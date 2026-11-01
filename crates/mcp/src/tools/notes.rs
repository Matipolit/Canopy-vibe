//! Tools for a MIDI clip's notes: writing them, changing or deleting some,
//! the piano roll's tools (`project::note_tools`), and pitch bends. Times
//! are beats from the clip's start, as `get_clip` reads them; all of these
//! are edits a `batch` can hold.

use midi::{Pitch, Tick, Velocity};
use project::note_tools::{self, ArpPattern, Arpeggio, ChordKind, Naturalize, Quantize, Strum};
use project::{BendPoint, ClipId, ClipRef, EditError, MidiClip, Note, NoteId, Project};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tracks::parse_key;
use super::{Kind, MAX_NOTES, Plan, Tool, ToolError, args, explain, schema};
use crate::units::{beats_to_tick, parse_pitch, pitch_name};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "write_notes",
        description: "Writes notes into a MIDI clip: `add` puts them beside the notes there, \
                      `replace` makes them the clip's only notes. Start and length in beats, \
                      start counted from the clip's start (as get_clip reads them); pitch a \
                      MIDI number or a name (\"C3\" = 60); velocity 1–127 (100 if not given). \
                      A note's `bend` is its pitch curve: points at beats from the note's \
                      start, in semitones (±48), each with the curve (-1–1, 0 straight) of the \
                      segment leading to it. At most 10,000 notes a call. Returns the new \
                      notes' ids.",
        schema: write_notes_schema,
        examples: &[
            r#"{"clip": 31, "mode": "add", "notes": [{"start": 0, "length": 1, "pitch": "C3"}, {"start": 1, "length": 0.5, "pitch": 64, "velocity": 90}]}"#,
            r#"{"clip": 31, "mode": "replace", "notes": [{"start": 0, "length": 2, "pitch": "A2", "bend": [{"at": 1, "semitones": 2, "curve": 0.5}]}]}"#,
        ],
        kind: Kind::Edit(write_notes),
    },
    Tool {
        name: "edit_notes",
        description: "Changes some of a MIDI clip's notes by id (only the fields given change) \
                      and deletes others. Start is in beats from the clip's start.",
        schema: edit_notes_schema,
        examples: &[
            r#"{"clip": 31, "change": [{"id": 40, "pitch": "D3", "velocity": 110}, {"id": 41, "start": 2.5}]}"#,
            r#"{"clip": 31, "delete": [42, 43]}"#,
        ],
        kind: Kind::Edit(edit_notes),
    },
    Tool {
        name: "transform_notes",
        description: "Runs one of the piano roll's tools on a MIDI clip's notes (`notes`, or \
                      all of them): quantize {grid in beats (0.25), strength 0–1 (1), starts \
                      (true), ends (false)}; naturalize {timing in beats (1/64), velocity and \
                      length as fractions 0–1 (0.15, 0.1), seed}; chop {step in beats (0.25)}; \
                      arpeggiate {step (0.25), pattern up|down|updown|random, gate 0.05–1 (0.9), \
                      octaves 1–4, seed}; strum {delay in beats (1/48), upward (true)}; legato; \
                      glue; fit_to_scale {key, the song's if not given}; reverse; flip; \
                      transpose {semitones}; chord {kind triad|seventh|ninth|sus2|sus4|power|\
                      octave, built from the song's key, C major without one}. Returns the ids \
                      of notes it made.",
        schema: transform_notes_schema,
        examples: &[
            r#"{"clip": 31, "op": "quantize", "options": {"grid": 0.25}}"#,
            r#"{"clip": 31, "op": "transpose", "notes": [40, 41], "options": {"semitones": -12}}"#,
            r#"{"clip": 31, "op": "chord", "options": {"kind": "seventh"}}"#,
            r#"{"clip": 31, "op": "legato"}"#,
        ],
        kind: Kind::Edit(transform_notes),
    },
    Tool {
        name: "set_note_bends",
        description: "Sets notes' pitch curves: for each note, its points (at: beats from the \
                      note's start; semitones ±48; curve -1–1 of the segment leading to the \
                      point, 0 straight). No points clears a note's bend.",
        schema: set_note_bends_schema,
        examples: &[
            r#"{"clip": 31, "bends": [{"note": 40, "points": [{"at": 0, "semitones": -2}, {"at": 0.5, "semitones": 0, "curve": 0.6}]}]}"#,
            r#"{"clip": 31, "bends": [{"note": 41, "points": []}]}"#,
        ],
        kind: Kind::Edit(set_note_bends),
    },
];

/// MIDI clip `id`, or why it isn't one.
fn midi_clip(project: &Project, id: ClipId) -> Result<&MidiClip, ToolError> {
    match project.clip(id) {
        Some((_, ClipRef::Midi(clip))) => Ok(clip),
        Some((_, ClipRef::Audio(_))) => Err(ToolError(format!(
            "clip {id} is an audio clip; notes are in MIDI clips"
        ))),
        None => Err(ToolError(explain(&EditError::NoSuchClip(id), project))),
    }
}

/// Checks every id in `ids` is a note of `clip`.
fn check_notes(project: &Project, clip: &MidiClip, ids: &[NoteId]) -> Result<(), ToolError> {
    for &id in ids {
        if !clip.notes.iter().any(|n| n.id == id) {
            return Err(ToolError(
                match super::what_is(id.0, project) {
                    Some("note") => format!("note {id} isn't in clip {}", clip.id),
                    Some(other) => format!("{id} is a {other}, not a note"),
                    None => format!("note {id} doesn't exist"),
                } + "; get_clip lists the clip's notes",
            ));
        }
    }
    Ok(())
}

/// A whole number `name` takes within `min..=max`.
fn whole(name: &str, v: f64, min: i64, max: i64) -> Result<i64, String> {
    if v.fract() != 0.0 || !(min as f64..=max as f64).contains(&v) {
        return Err(format!("{name} is a whole number {min}–{max}; {v} isn't"));
    }
    Ok(v as i64)
}

/// A number `name` takes within `min..=max`.
fn within(name: &str, v: f64, min: f64, max: f64) -> Result<f64, String> {
    if !(min..=max).contains(&v) {
        return Err(format!("{name} is {min}–{max}; {v} is out of range"));
    }
    Ok(v)
}

/// A length or a step in beats: more than nothing.
fn duration(name: &str, beats: f64) -> Result<Tick, String> {
    let ticks = beats_to_tick(beats)?;
    if ticks <= Tick::ZERO {
        return Err(format!("{name} must be more than 0 beats; {beats} isn't"));
    }
    Ok(ticks)
}

fn velocity(name: &str, v: f64, min: i64) -> Result<Velocity, String> {
    Ok(Velocity::from_midi1(whole(name, v, min, 127)? as u8))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BendArgs {
    at: f64,
    semitones: f64,
    curve: Option<f64>,
}

fn bend_point_schema() -> Value {
    schema::object(
        json!({
            "at": schema::beats("Beats from the note's start."),
            "semitones": schema::number("How far from the note's pitch, -48–48."),
            "curve": schema::number("How the segment leading here bends, -1–1 (0, straight, if not given)."),
        }),
        &["at", "semitones"],
    )
}

fn bend(points: &[BendArgs]) -> Result<Vec<BendPoint>, String> {
    let limit = f64::from(BendPoint::MAX_SEMITONES);
    let mut out = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let at = beats_to_tick(p.at).map_err(|e| format!("bend point {i}: {e}"))?;
            let semitones = within("a bend", p.semitones, -limit, limit)
                .map_err(|e| format!("bend point {i}: {e} semitones"))?;
            let curve = within("a curve", p.curve.unwrap_or(0.0), -1.0, 1.0)
                .map_err(|e| format!("bend point {i}: {e}"))?;
            Ok(BendPoint {
                at,
                semitones: semitones as f32,
                curve: curve as f32,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    out.sort_by_key(|p| p.at);
    Ok(out)
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum WriteMode {
    Add,
    Replace,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    clip: ClipId,
    mode: WriteMode,
    notes: Vec<NoteArgs>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteArgs {
    start: f64,
    length: f64,
    pitch: Value,
    velocity: Option<f64>,
    release_velocity: Option<f64>,
    muted: Option<bool>,
    bend: Option<Vec<BendArgs>>,
}

fn write_notes_schema() -> Value {
    schema::object(
        json!({
            "clip": schema::id("The MIDI clip's id."),
            "mode": schema::choice(&["add", "replace"], "add: beside the notes there; replace: instead of them."),
            "notes": schema::array(
                schema::object(
                    json!({
                        "start": schema::beats("Beats from the clip's start."),
                        "length": schema::number("In beats, more than 0."),
                        "pitch": schema::pitch("A MIDI number 0–127 or a name (\"C3\" = 60)."),
                        "velocity": json!({ "type": "integer", "minimum": 1, "maximum": 127, "description": "1–127 (100 if not given)." }),
                        "release_velocity": json!({ "type": "integer", "minimum": 0, "maximum": 127, "description": "Note-off velocity, 0–127 (64 if not given)." }),
                        "muted": schema::boolean("Kept but not played."),
                        "bend": schema::array(bend_point_schema(), "The note's pitch curve."),
                    }),
                    &["start", "length", "pitch"],
                ),
                "At most 10,000.",
            ),
        }),
        &["clip", "mode", "notes"],
    )
}

/// A note to write, checked, waiting for its id.
struct NewNote {
    start: Tick,
    len: Tick,
    pitch: Pitch,
    velocity: Velocity,
    release_velocity: Velocity,
    muted: bool,
    bend: Vec<BendPoint>,
}

fn new_note(n: &NoteArgs, offset: Tick) -> Result<NewNote, String> {
    Ok(NewNote {
        start: beats_to_tick(n.start)? + offset,
        len: duration("length", n.length)?,
        pitch: parse_pitch(&n.pitch)?,
        velocity: velocity("velocity", n.velocity.unwrap_or(100.0), 1)?,
        release_velocity: match n.release_velocity {
            Some(v) => velocity("release_velocity", v, 0)?,
            None => Velocity::CENTER,
        },
        muted: n.muted.unwrap_or(false),
        bend: bend(n.bend.as_deref().unwrap_or_default())?,
    })
}

fn write_notes(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: WriteArgs = args(v)?;
    let clip = midi_clip(project, a.clip)?;
    if a.notes.len() > MAX_NOTES {
        return Err(ToolError(format!(
            "{} notes is more than one call writes; at most {MAX_NOTES} (write the rest with \
             mode add)",
            a.notes.len()
        )));
    }
    let new: Vec<NewNote> = a
        .notes
        .iter()
        .enumerate()
        .map(|(i, n)| new_note(n, clip.offset).map_err(|e| ToolError(format!("notes[{i}]: {e}"))))
        .collect::<Result<_, _>>()?;
    let id = a.clip;
    let count = new.len();
    let (mode, name) = (a.mode, clip.name.clone());
    // Notes past the clip's end are kept but don't play: worth saying.
    let end = clip.offset + clip.length;
    let past = new.iter().filter(|n| n.start >= end).count();
    Ok(Plan {
        label: match mode {
            WriteMode::Add => format!("Write {count} notes in {name}"),
            WriteMode::Replace => format!("Replace the notes in {name}"),
        },
        apply: Box::new(move |p| {
            let mut notes = match mode {
                WriteMode::Add => midi_clip(p, id)
                    .map_err(|e| EditError::Invalid(e.0))?
                    .notes
                    .to_vec(),
                WriteMode::Replace => Vec::new(),
            };
            let mut ids = Vec::with_capacity(count);
            for n in new {
                let note_id = p.new_note_id();
                ids.push(note_id.0);
                notes.push(Note {
                    id: note_id,
                    start: n.start,
                    len: n.len,
                    pitch: n.pitch,
                    velocity: n.velocity,
                    release_velocity: n.release_velocity,
                    muted: n.muted,
                    bend: n.bend,
                });
            }
            let total = notes.len();
            p.set_notes(id, notes)?;
            let mut summary = format!("wrote {count} notes; clip {id} has {total}");
            if past > 0 {
                summary.push_str(&format!(
                    "; {past} start after the clip ends and won't play until it's longer"
                ));
            }
            Ok(json!({ "ids": ids, "summary": summary }))
        }),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditNotesArgs {
    clip: ClipId,
    #[serde(default)]
    change: Vec<ChangeArgs>,
    #[serde(default)]
    delete: Vec<NoteId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeArgs {
    id: NoteId,
    start: Option<f64>,
    length: Option<f64>,
    pitch: Option<Value>,
    velocity: Option<f64>,
    muted: Option<bool>,
}

fn edit_notes_schema() -> Value {
    schema::object(
        json!({
            "clip": schema::id("The MIDI clip's id."),
            "change": schema::array(
                schema::object(
                    json!({
                        "id": schema::id("The note's id."),
                        "start": schema::beats("Beats from the clip's start."),
                        "length": schema::number("In beats, more than 0."),
                        "pitch": schema::pitch("A MIDI number or a name (\"C3\" = 60)."),
                        "velocity": json!({ "type": "integer", "minimum": 1, "maximum": 127, "description": "1–127." }),
                        "muted": schema::boolean("Kept but not played."),
                    }),
                    &["id"],
                ),
                "Notes to change; only the fields given change.",
            ),
            "delete": schema::ids("Notes to delete."),
        }),
        &["clip"],
    )
}

/// A change to one note, checked.
#[derive(Clone)]
struct Change {
    start: Option<Tick>,
    len: Option<Tick>,
    pitch: Option<Pitch>,
    velocity: Option<Velocity>,
    muted: Option<bool>,
}

fn edit_notes(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: EditNotesArgs = args(v)?;
    let clip = midi_clip(project, a.clip)?;
    if a.change.is_empty() && a.delete.is_empty() {
        return Err("nothing to do: give `change` or `delete`".into());
    }
    let changed: Vec<NoteId> = a.change.iter().map(|c| c.id).collect();
    check_notes(project, clip, &changed)?;
    check_notes(project, clip, &a.delete)?;
    let mut seen = std::collections::HashSet::new();
    for &id in &changed {
        if !seen.insert(id) {
            return Err(ToolError(format!(
                "note {id} is changed twice; give each note once"
            )));
        }
    }
    if let Some(both) = a.delete.iter().find(|id| seen.contains(id)) {
        return Err(ToolError(format!(
            "note {both} is both changed and deleted; give it once"
        )));
    }
    let offset = clip.offset;
    let changes: Vec<(NoteId, Change)> = a
        .change
        .iter()
        .map(|c| {
            let at = |e: String| ToolError(format!("note {}: {e}", c.id));
            Ok((
                c.id,
                Change {
                    start: c
                        .start
                        .map(|s| beats_to_tick(s).map(|t| t + offset))
                        .transpose()
                        .map_err(at)?,
                    len: c
                        .length
                        .map(|l| duration("length", l))
                        .transpose()
                        .map_err(at)?,
                    pitch: c.pitch.as_ref().map(parse_pitch).transpose().map_err(at)?,
                    velocity: c
                        .velocity
                        .map(|v| velocity("velocity", v, 1))
                        .transpose()
                        .map_err(at)?,
                    muted: c.muted,
                },
            ))
        })
        .collect::<Result<_, ToolError>>()?;
    let (id, delete) = (a.clip, a.delete);
    let (n_changed, n_deleted) = (changes.len(), delete.len());
    let label = match (n_changed, n_deleted) {
        (0, d) => format!("Delete {d} notes in {}", clip.name),
        (c, 0) => format!("Edit {c} notes in {}", clip.name),
        (c, d) => format!("Edit {c} and delete {d} notes in {}", clip.name),
    };
    Ok(Plan {
        label,
        apply: Box::new(move |p| {
            let notes: Vec<Note> = midi_clip(p, id)
                .map_err(|e| EditError::Invalid(e.0))?
                .notes
                .iter()
                .filter(|n| !delete.contains(&n.id))
                .map(|n| {
                    let Some((_, c)) = changes.iter().find(|(id, _)| *id == n.id) else {
                        return n.clone();
                    };
                    Note {
                        start: c.start.unwrap_or(n.start),
                        len: c.len.unwrap_or(n.len),
                        pitch: c.pitch.unwrap_or(n.pitch),
                        velocity: c.velocity.unwrap_or(n.velocity),
                        muted: c.muted.unwrap_or(n.muted),
                        ..n.clone()
                    }
                })
                .collect();
            p.set_notes(id, notes)?;
            Ok(json!({
                "summary": format!("changed {n_changed} notes and deleted {n_deleted} in clip {id}"),
            }))
        }),
    })
}

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
enum TransformOp {
    Quantize,
    Naturalize,
    Chop,
    Arpeggiate,
    Strum,
    Legato,
    Glue,
    FitToScale,
    Reverse,
    Flip,
    Transpose,
    Chord,
}

impl TransformOp {
    const NAMES: [&str; 12] = [
        "quantize",
        "naturalize",
        "chop",
        "arpeggiate",
        "strum",
        "legato",
        "glue",
        "fit_to_scale",
        "reverse",
        "flip",
        "transpose",
        "chord",
    ];

    fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// The options this op takes.
    fn options(self) -> &'static [&'static str] {
        match self {
            TransformOp::Quantize => &["grid", "strength", "starts", "ends"],
            TransformOp::Naturalize => &["timing", "velocity", "length", "seed"],
            TransformOp::Chop => &["step"],
            TransformOp::Arpeggiate => &["step", "pattern", "gate", "octaves", "seed"],
            TransformOp::Strum => &["delay", "upward"],
            TransformOp::FitToScale => &["key"],
            TransformOp::Transpose => &["semitones"],
            TransformOp::Chord => &["kind"],
            TransformOp::Legato | TransformOp::Glue | TransformOp::Reverse | TransformOp::Flip => {
                &[]
            }
        }
    }

    /// The undo step's verb.
    fn verb(self) -> &'static str {
        match self {
            TransformOp::Quantize => "Quantize",
            TransformOp::Naturalize => "Naturalize",
            TransformOp::Chop => "Chop",
            TransformOp::Arpeggiate => "Arpeggiate",
            TransformOp::Strum => "Strum",
            TransformOp::Legato => "Legato",
            TransformOp::Glue => "Glue",
            TransformOp::FitToScale => "Fit to scale",
            TransformOp::Reverse => "Reverse",
            TransformOp::Flip => "Flip",
            TransformOp::Transpose => "Transpose",
            TransformOp::Chord => "Chords from",
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum PatternArg {
    Up,
    Down,
    UpDown,
    Random,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ChordArg {
    Triad,
    Seventh,
    Ninth,
    Sus2,
    Sus4,
    Power,
    Octave,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Options {
    grid: Option<f64>,
    strength: Option<f64>,
    starts: Option<bool>,
    ends: Option<bool>,
    timing: Option<f64>,
    velocity: Option<f64>,
    length: Option<f64>,
    seed: Option<u64>,
    step: Option<f64>,
    pattern: Option<PatternArg>,
    gate: Option<f64>,
    octaves: Option<f64>,
    delay: Option<f64>,
    upward: Option<bool>,
    key: Option<String>,
    semitones: Option<f64>,
    kind: Option<ChordArg>,
}

impl Options {
    /// The names of the options given.
    fn given(&self) -> Vec<&'static str> {
        [
            ("grid", self.grid.is_some()),
            ("strength", self.strength.is_some()),
            ("starts", self.starts.is_some()),
            ("ends", self.ends.is_some()),
            ("timing", self.timing.is_some()),
            ("velocity", self.velocity.is_some()),
            ("length", self.length.is_some()),
            ("seed", self.seed.is_some()),
            ("step", self.step.is_some()),
            ("pattern", self.pattern.is_some()),
            ("gate", self.gate.is_some()),
            ("octaves", self.octaves.is_some()),
            ("delay", self.delay.is_some()),
            ("upward", self.upward.is_some()),
            ("key", self.key.is_some()),
            ("semitones", self.semitones.is_some()),
            ("kind", self.kind.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, given)| given.then_some(name))
        .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformArgs {
    clip: ClipId,
    notes: Option<Vec<NoteId>>,
    op: TransformOp,
    #[serde(default)]
    options: Options,
}

fn transform_notes_schema() -> Value {
    let beats = |d: &str| json!({ "type": "number", "exclusiveMinimum": 0, "description": d });
    schema::object(
        json!({
            "clip": schema::id("The MIDI clip's id."),
            "notes": schema::ids("The notes to work on; all of the clip's if not given."),
            "op": schema::choice(&TransformOp::NAMES, "Which tool."),
            "options": schema::object(
                json!({
                    "grid": beats("quantize: the grid, in beats."),
                    "strength": schema::number("quantize: how far to move, 0–1."),
                    "starts": schema::boolean("quantize: move the starts."),
                    "ends": schema::boolean("quantize: move the ends."),
                    "timing": schema::beats("naturalize: the most a start moves, in beats."),
                    "velocity": schema::number("naturalize: the most a velocity changes, a fraction 0–1."),
                    "length": schema::number("naturalize: the most a length changes, a fraction 0–1."),
                    "seed": schema::count("naturalize, arpeggiate: the random seed."),
                    "step": beats("chop, arpeggiate: the step, in beats."),
                    "pattern": schema::choice(&["up", "down", "updown", "random"], "arpeggiate: the order."),
                    "gate": schema::number("arpeggiate: how much of a step a note lasts, 0.05–1."),
                    "octaves": json!({ "type": "integer", "minimum": 1, "maximum": 4, "description": "arpeggiate: octaves climbed, 1–4." }),
                    "delay": schema::beats("strum: how much later each next note starts, in beats."),
                    "upward": schema::boolean("strum: lowest note first."),
                    "key": schema::string("fit_to_scale: a key like \"A minor\"; the song's if not given."),
                    "semitones": schema::integer("transpose: how far, in semitones."),
                    "kind": schema::choice(&["triad", "seventh", "ninth", "sus2", "sus4", "power", "octave"], "chord: which chord each note becomes."),
                }),
                &[],
            ),
        }),
        &["clip", "op"],
    )
}

/// A transform, its options checked.
enum Transform {
    Quantize(Quantize),
    Naturalize(Naturalize),
    Chop(Tick),
    Arpeggiate(Arpeggio),
    Strum(Strum),
    Legato,
    Glue,
    FitToScale(project::Scale),
    Reverse,
    Flip,
    Transpose(i32),
    Chord(ChordKind),
}

fn transform(
    op: TransformOp,
    o: Options,
    project: &Project,
    notes: &[&Note],
) -> Result<Transform, String> {
    let fraction = |name: &str, v: Option<f64>, default: f32| {
        v.map_or(Ok(default), |v| within(name, v, 0.0, 1.0).map(|v| v as f32))
    };
    let step = |name: &str, v: Option<f64>, default: f64| duration(name, v.unwrap_or(default));
    Ok(match op {
        TransformOp::Quantize => Transform::Quantize(Quantize {
            grid: step("grid", o.grid, 0.25)?,
            strength: fraction("strength", o.strength, 1.0)?,
            starts: o.starts.unwrap_or(true),
            ends: o.ends.unwrap_or(false),
        }),
        TransformOp::Naturalize => {
            let defaults = Naturalize::default();
            Transform::Naturalize(Naturalize {
                timing: o.timing.map_or(Ok(defaults.timing), beats_to_tick)?,
                velocity: fraction("velocity", o.velocity, defaults.velocity)?,
                length: fraction("length", o.length, defaults.length)?,
                seed: o.seed.unwrap_or(defaults.seed),
            })
        }
        TransformOp::Chop => Transform::Chop(step("step", o.step, 0.25)?),
        TransformOp::Arpeggiate => {
            let defaults = Arpeggio::default();
            Transform::Arpeggiate(Arpeggio {
                step: step("step", o.step, 0.25)?,
                pattern: match o.pattern {
                    None => defaults.pattern,
                    Some(PatternArg::Up) => ArpPattern::Up,
                    Some(PatternArg::Down) => ArpPattern::Down,
                    Some(PatternArg::UpDown) => ArpPattern::UpDown,
                    Some(PatternArg::Random) => ArpPattern::Random,
                },
                gate: o.gate.map_or(Ok(defaults.gate), |g| {
                    within("gate", g, 0.05, 1.0).map(|g| g as f32)
                })?,
                octaves: o.octaves.map_or(Ok(i64::from(defaults.octaves)), |n| {
                    whole("octaves", n, 1, 4)
                })? as u8,
                seed: o.seed.unwrap_or(defaults.seed),
            })
        }
        TransformOp::Strum => {
            let defaults = Strum::default();
            Transform::Strum(Strum {
                delay: o.delay.map_or(Ok(defaults.delay), beats_to_tick)?,
                upward: o.upward.unwrap_or(defaults.upward),
            })
        }
        TransformOp::Legato => Transform::Legato,
        TransformOp::Glue => Transform::Glue,
        TransformOp::FitToScale => Transform::FitToScale(match (o.key, project.scale) {
            (Some(key), _) => parse_key(&key)?,
            (None, Some(scale)) => scale,
            (None, None) => {
                return Err("the song has no key: give `key` (\"A minor\"), or set the \
                            song's with edit_song"
                    .to_owned());
            }
        }),
        TransformOp::Reverse => Transform::Reverse,
        TransformOp::Flip => Transform::Flip,
        TransformOp::Transpose => {
            let semitones = o
                .semitones
                .ok_or("transpose needs `semitones` in its options")?;
            let semitones = whole("semitones", semitones, -127, 127)? as i32;
            for n in notes {
                let to = i32::from(n.pitch.get()) + semitones;
                if !(0..=127).contains(&to) {
                    return Err(format!(
                        "transposing by {semitones} takes note {} ({}) to {to}, outside \
                         0–127",
                        n.id,
                        pitch_name(n.pitch)
                    ));
                }
            }
            Transform::Transpose(semitones)
        }
        TransformOp::Chord => Transform::Chord(match o.kind.unwrap_or(ChordArg::Triad) {
            ChordArg::Triad => ChordKind::Triad,
            ChordArg::Seventh => ChordKind::Seventh,
            ChordArg::Ninth => ChordKind::Ninth,
            ChordArg::Sus2 => ChordKind::Sus2,
            ChordArg::Sus4 => ChordKind::Sus4,
            ChordArg::Power => ChordKind::Power,
            ChordArg::Octave => ChordKind::Octave,
        }),
    })
}

fn transform_notes(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: TransformArgs = args(v)?;
    let clip = midi_clip(project, a.clip)?;
    let allowed = a.op.options();
    if let Some(stray) = a.options.given().into_iter().find(|o| !allowed.contains(o)) {
        return Err(ToolError(match allowed {
            [] => format!("{} takes no options; `{stray}` isn't one", a.op.name()),
            _ => format!(
                "`{stray}` isn't an option of {}; its options are {}",
                a.op.name(),
                allowed.join(", ")
            ),
        }));
    }
    let ids = match a.notes {
        Some(ids) if ids.is_empty() => {
            return Err("`notes` is empty; leave it out to work on every note".into());
        }
        Some(ids) => {
            check_notes(project, clip, &ids)?;
            ids
        }
        None => Vec::new(),
    };
    if clip.notes.is_empty() {
        return Err(ToolError(format!("clip {} has no notes", clip.id)));
    }
    let chosen: Vec<&Note> = clip
        .notes
        .iter()
        .filter(|n| ids.is_empty() || ids.contains(&n.id))
        .collect();
    let op = transform(a.op, a.options, project, &chosen)?;
    let id = a.clip;
    let label = format!("{} notes in {}", a.op.verb(), clip.name);
    let name = a.op.name();
    Ok(Plan {
        label,
        apply: Box::new(move |p| {
            let clip = midi_clip(p, id).map_err(|e| EditError::Invalid(e.0))?;
            let notes = clip.notes.to_vec();
            let origin = clip.start - clip.offset;
            let scale = p.scale;
            let new = match op {
                Transform::Quantize(q) => note_tools::quantize(&notes, &ids, q, origin),
                Transform::Naturalize(n) => note_tools::naturalize(&notes, &ids, n),
                Transform::Chop(step) => {
                    note_tools::chop(&notes, &ids, step, origin, &mut || p.new_note_id())
                }
                Transform::Arpeggiate(a) => {
                    note_tools::arpeggiate(&notes, &ids, a, &mut || p.new_note_id())
                }
                Transform::Strum(s) => note_tools::strum(&notes, &ids, s),
                Transform::Legato => note_tools::legato(&notes, &ids),
                Transform::Glue => note_tools::glue(&notes, &ids),
                Transform::FitToScale(scale) => note_tools::fit_to_scale(&notes, &ids, scale),
                Transform::Reverse => note_tools::reverse(&notes, &ids),
                Transform::Flip => note_tools::flip(&notes, &ids),
                Transform::Transpose(semitones) => transpose(&notes, &ids, semitones),
                Transform::Chord(kind) => {
                    chords(&notes, &ids, kind, scale, &mut || p.new_note_id())
                }
            };
            let made: Vec<u32> = new
                .iter()
                .filter(|n| !notes.iter().any(|old| old.id == n.id))
                .map(|n| n.id.0)
                .collect();
            let total = new.len();
            p.set_notes(id, new)?;
            let mut summary = format!("{name}: clip {id} has {total} notes");
            if !made.is_empty() {
                summary.push_str(&format!(", {} of them new", made.len()));
            }
            Ok(json!({ "made": made, "notes": total, "summary": summary }))
        }),
    })
}

/// The notes with `ids` (all if empty) moved by `semitones`, bends and all.
/// The range was checked when the edit was planned.
fn transpose(notes: &[Note], ids: &[NoteId], semitones: i32) -> Vec<Note> {
    notes
        .iter()
        .map(|n| {
            let mut n = n.clone();
            if ids.is_empty() || ids.contains(&n.id) {
                let to = (i32::from(n.pitch.get()) + semitones).clamp(0, 127);
                n.pitch = Pitch::new(to as u8).expect("clamped to the range");
            }
            n
        })
        .collect()
}

/// Each note with `ids` (all if empty) becomes a `kind` chord on it, built
/// from `scale` as the piano roll stamps chords: the first of its notes
/// keeps the note's id, the others get new ones.
fn chords(
    notes: &[Note],
    ids: &[NoteId],
    kind: ChordKind,
    scale: Option<project::Scale>,
    new_id: &mut dyn FnMut() -> NoteId,
) -> Vec<Note> {
    let mut out = Vec::with_capacity(notes.len());
    for n in notes {
        if !ids.is_empty() && !ids.contains(&n.id) {
            out.push(n.clone());
            continue;
        }
        for (i, pitch) in note_tools::chord(n.pitch, kind, scale)
            .into_iter()
            .enumerate()
        {
            out.push(Note {
                id: if i == 0 { n.id } else { new_id() },
                pitch,
                ..n.clone()
            });
        }
    }
    out.sort_by_key(|n| (n.start, n.pitch));
    out
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BendsArgs {
    clip: ClipId,
    bends: Vec<NoteBendArgs>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteBendArgs {
    note: NoteId,
    points: Vec<BendArgs>,
}

fn set_note_bends_schema() -> Value {
    schema::object(
        json!({
            "clip": schema::id("The MIDI clip's id."),
            "bends": schema::array(
                schema::object(
                    json!({
                        "note": schema::id("The note's id."),
                        "points": schema::array(bend_point_schema(), "Its pitch curve; none clears it."),
                    }),
                    &["note", "points"],
                ),
                "Each note's new curve.",
            ),
        }),
        &["clip", "bends"],
    )
}

fn set_note_bends(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: BendsArgs = args(v)?;
    let clip = midi_clip(project, a.clip)?;
    if a.bends.is_empty() {
        return Err("no bends given".into());
    }
    let notes: Vec<NoteId> = a.bends.iter().map(|b| b.note).collect();
    check_notes(project, clip, &notes)?;
    let bends: Vec<(NoteId, Vec<BendPoint>)> = a
        .bends
        .iter()
        .map(|b| {
            bend(&b.points)
                .map(|points| (b.note, points))
                .map_err(|e| ToolError(format!("note {}: {e}", b.note)))
        })
        .collect::<Result<_, _>>()?;
    let id = a.clip;
    let n = bends.len();
    Ok(Plan {
        label: format!("Bend {n} notes in {}", clip.name),
        apply: Box::new(move |p| {
            for (note, points) in bends {
                p.set_note_bend(id, note, points)?;
            }
            Ok(json!({ "summary": format!("set the bends of {n} notes in clip {id}") }))
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use crate::testing::FakeHost;
    use crate::tools::{Outcome, call};
    use project::{NewTrack, Scale, ScaleMode, TrackPosition};

    /// A host with a MIDI track and a 4-beat clip on it at beat 4; the
    /// clip's id.
    fn rig() -> (FakeHost, ClipId) {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let clip = project
            .add_midi_clip(track, Tick::new(3840), Tick::new(3840))
            .unwrap();
        (FakeHost::new(project), clip)
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

    fn notes(host: &FakeHost, clip: ClipId) -> Vec<Note> {
        midi_clip(&host.project, clip).unwrap().notes.to_vec()
    }

    fn pitches(host: &FakeHost, clip: ClipId) -> Vec<u8> {
        notes(host, clip).iter().map(|n| n.pitch.get()).collect()
    }

    fn write(host: &mut FakeHost, clip: ClipId, notes: Value) -> Vec<NoteId> {
        let out = done(
            "write_notes",
            json!({ "clip": clip.0, "mode": "add", "notes": notes }),
            host,
        );
        out["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| NoteId(id.as_u64().unwrap() as u32))
            .collect()
    }

    #[test]
    fn replace_writes_exactly_these_notes() {
        let (mut host, clip) = rig();
        write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": 48 }]),
        );
        let out = done(
            "write_notes",
            json!({ "clip": clip.0, "mode": "replace", "notes": [
                { "start": 0.5, "length": 0.25, "pitch": "C3", "velocity": 90 },
                { "start": 2, "length": 1, "pitch": "Bb3", "release_velocity": 10, "muted": true },
            ] }),
            &mut host,
        );
        let written = notes(&host, clip);
        assert_eq!(written.len(), 2);
        assert_eq!(out["ids"], json!([written[0].id.0, written[1].id.0]));
        assert_eq!(written[0].start, Tick::new(480));
        assert_eq!(written[0].len, Tick::new(240));
        assert_eq!(written[0].pitch.get(), 60);
        assert_eq!(written[0].velocity.to_midi1(), 90);
        assert_eq!(written[0].release_velocity, Velocity::CENTER);
        assert_eq!(written[1].pitch.get(), 70);
        assert_eq!(written[1].release_velocity.to_midi1(), 10);
        assert!(written[1].muted && !written[0].muted);
        assert_eq!(written[1].velocity.to_midi1(), 100, "the default");
        assert_eq!(host.history.position(), 2);

        // Reads see what was written, at the same times.
        let read = done("get_clip", json!({ "clip": clip.0 }), &mut host);
        assert_eq!(read["notes"][0]["start"], json!(0.5));
        assert_eq!(read["notes"][1]["name"], json!("A#3"));
    }

    #[test]
    fn note_times_count_from_the_clips_start_after_its_offset() {
        let (mut host, clip) = rig();
        // Trimmed from the left by a beat: the clip's content starts a
        // beat before the clip does.
        host.project
            .set_clip_bounds(clip, Tick::new(4800), Tick::new(7680))
            .unwrap();
        let offset = midi_clip(&host.project, clip).unwrap().offset;
        assert_eq!(offset, Tick::new(960));
        write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": 60 }]),
        );
        assert_eq!(notes(&host, clip)[0].start, offset);
        let read = done("get_clip", json!({ "clip": clip.0 }), &mut host);
        assert_eq!(read["notes"][0]["start"], json!(0.0));
    }

    #[test]
    fn ten_thousand_and_one_notes_is_an_error() {
        let (mut host, clip) = rig();
        let note = json!({ "start": 0, "length": 0.1, "pitch": 60 });
        let many = vec![note; MAX_NOTES + 1];
        let error = failure(
            "write_notes",
            json!({ "clip": clip.0, "mode": "add", "notes": many }),
            &mut host,
        );
        assert!(error.contains("10000"), "{error}");
        assert!(notes(&host, clip).is_empty());
        let out = done(
            "write_notes",
            json!({ "clip": clip.0, "mode": "add", "notes": &many[..MAX_NOTES] }),
            &mut host,
        );
        assert_eq!(out["ids"].as_array().unwrap().len(), MAX_NOTES);
    }

    #[test]
    fn bad_pitches_are_named() {
        let (mut host, clip) = rig();
        let error = failure(
            "write_notes",
            json!({ "clip": clip.0, "mode": "add", "notes": [
                { "start": 0, "length": 1, "pitch": 60 },
                { "start": 1, "length": 1, "pitch": "H3" },
            ] }),
            &mut host,
        );
        assert!(
            error.contains("notes[1]") && error.contains("H3"),
            "{error}"
        );
        for (note, wanted) in [
            (json!({ "start": 0, "length": 1, "pitch": 128 }), "0–127"),
            (
                json!({ "start": 0, "length": 0, "pitch": 60 }),
                "more than 0",
            ),
            (json!({ "start": -1, "length": 1, "pitch": 60 }), "-1"),
            (
                json!({ "start": 0, "length": 1, "pitch": 60, "velocity": 0 }),
                "1–127",
            ),
        ] {
            let error = failure(
                "write_notes",
                json!({ "clip": clip.0, "mode": "add", "notes": [note] }),
                &mut host,
            );
            assert!(error.contains(wanted), "{error} names {wanted}");
        }
        assert!(notes(&host, clip).is_empty());
        assert_eq!(host.history.position(), 0);
    }

    #[test]
    fn edit_notes_changes_and_deletes_by_id() {
        let (mut host, clip) = rig();
        let ids = write(
            &mut host,
            clip,
            json!([
                { "start": 0, "length": 1, "pitch": 60 },
                { "start": 1, "length": 1, "pitch": 62 },
                { "start": 2, "length": 1, "pitch": 64 },
            ]),
        );
        done(
            "edit_notes",
            json!({ "clip": clip.0, "change": [{ "id": ids[0].0, "pitch": "D3", "start": 3 }], "delete": [ids[1].0] }),
            &mut host,
        );
        let after = notes(&host, clip);
        assert_eq!(after.len(), 2);
        let moved = after.iter().find(|n| n.id == ids[0]).unwrap();
        assert_eq!((moved.pitch.get(), moved.start), (62, Tick::new(2880)));
        assert_eq!(moved.len, Tick::new(960), "the length stays");

        let error = failure(
            "edit_notes",
            json!({ "clip": clip.0, "delete": [ids[1].0] }),
            &mut host,
        );
        assert!(error.contains(&ids[1].0.to_string()), "{error}");
        let error = failure(
            "edit_notes",
            json!({ "clip": clip.0, "change": [{ "id": ids[2].0, "muted": true }], "delete": [ids[2].0] }),
            &mut host,
        );
        assert!(error.contains("both"), "{error}");
        let error = failure(
            "edit_notes",
            json!({ "clip": clip.0, "delete": [clip.0] }),
            &mut host,
        );
        assert!(error.contains("is a clip"), "{error}");
    }

    #[test]
    fn quantize_only_the_chosen_notes() {
        let (mut host, clip) = rig();
        let ids = write(
            &mut host,
            clip,
            json!([
                { "start": 0.1, "length": 1, "pitch": 60 },
                { "start": 1.1, "length": 1, "pitch": 62 },
            ]),
        );
        done(
            "transform_notes",
            json!({ "clip": clip.0, "op": "quantize", "notes": [ids[1].0], "options": { "grid": 1 } }),
            &mut host,
        );
        let after = notes(&host, clip);
        let start = |id: NoteId| after.iter().find(|n| n.id == id).unwrap().start;
        assert_eq!(start(ids[0]), Tick::new(96), "not chosen: unmoved");
        assert_eq!(start(ids[1]), Tick::new(960));
        assert_eq!(
            host.history.undo_label(),
            Some("LLM: Quantize notes in 1 MIDI")
        );

        let error = failure(
            "transform_notes",
            json!({ "clip": clip.0, "op": "quantize", "options": { "semitones": 2 } }),
            &mut host,
        );
        assert!(error.contains("grid, strength"), "{error}");
        let error = failure(
            "transform_notes",
            json!({ "clip": clip.0, "op": "quantize", "options": { "strength": 2 } }),
            &mut host,
        );
        assert!(error.contains("0–1"), "{error}");
        let error = failure(
            "transform_notes",
            json!({ "clip": clip.0, "op": "fit_to_scale" }),
            &mut host,
        );
        assert!(error.contains("no key"), "{error}");
    }

    #[test]
    fn chop_and_arpeggiate_return_the_notes_they_made() {
        let (mut host, clip) = rig();
        write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": 60 }]),
        );
        let out = done(
            "transform_notes",
            json!({ "clip": clip.0, "op": "chop", "options": { "step": 0.25 } }),
            &mut host,
        );
        assert_eq!(out["notes"], json!(4));
        assert_eq!(out["made"].as_array().unwrap().len(), 3);
        assert_eq!(notes(&host, clip).len(), 4);
    }

    #[test]
    fn transpose_keeps_bends() {
        let (mut host, clip) = rig();
        let ids = write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": 60, "bend": [{ "at": 0.5, "semitones": 2, "curve": 0.5 }] }]),
        );
        let bend = notes(&host, clip)[0].bend.clone();
        assert_eq!(bend.len(), 1);
        assert_eq!(bend[0].at, Tick::new(480));
        done(
            "transform_notes",
            json!({ "clip": clip.0, "op": "transpose", "options": { "semitones": -12 } }),
            &mut host,
        );
        let after = &notes(&host, clip)[0];
        assert_eq!(after.id, ids[0]);
        assert_eq!(after.pitch.get(), 48);
        assert_eq!(after.bend, bend);

        let error = failure(
            "transform_notes",
            json!({ "clip": clip.0, "op": "transpose", "options": { "semitones": 100 } }),
            &mut host,
        );
        assert!(error.contains("0–127"), "{error}");
        let error = failure(
            "transform_notes",
            json!({ "clip": clip.0, "op": "transpose" }),
            &mut host,
        );
        assert!(error.contains("semitones"), "{error}");
        assert_eq!(pitches(&host, clip), [48]);
    }

    #[test]
    fn chord_uses_the_song_key() {
        let (mut host, clip) = rig();
        let ids = write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": "D3" }]),
        );
        // Without a key, chords come from C major: D minor.
        done(
            "transform_notes",
            json!({ "clip": clip.0, "op": "chord" }),
            &mut host,
        );
        assert_eq!(pitches(&host, clip), [62, 65, 69]);
        assert!(notes(&host, clip).iter().any(|n| n.id == ids[0]));
        host.undo();

        host.project.scale = Some(Scale::new(2, ScaleMode::Major));
        let out = done(
            "transform_notes",
            json!({ "clip": clip.0, "op": "chord", "options": { "kind": "triad" } }),
            &mut host,
        );
        assert_eq!(pitches(&host, clip), [62, 66, 69], "D major: F#");
        assert_eq!(out["made"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn bends_out_of_range_are_an_error() {
        let (mut host, clip) = rig();
        let ids = write(
            &mut host,
            clip,
            json!([{ "start": 0, "length": 1, "pitch": 60 }]),
        );
        let note = ids[0].0;
        let error = failure(
            "set_note_bends",
            json!({ "clip": clip.0, "bends": [{ "note": note, "points": [{ "at": 0, "semitones": 60 }] }] }),
            &mut host,
        );
        assert!(error.contains("-48–48"), "{error}");
        let error = failure(
            "set_note_bends",
            json!({ "clip": clip.0, "bends": [{ "note": note, "points": [{ "at": 0, "semitones": 1, "curve": 2 }] }] }),
            &mut host,
        );
        assert!(error.contains("-1–1"), "{error}");
        let error = failure(
            "write_notes",
            json!({ "clip": clip.0, "mode": "add", "notes": [{ "start": 0, "length": 1, "pitch": 60, "bend": [{ "at": 0, "semitones": -49 }] }] }),
            &mut host,
        );
        assert!(error.contains("-48–48"), "{error}");
        assert!(notes(&host, clip)[0].bend.is_empty());

        done(
            "set_note_bends",
            json!({ "clip": clip.0, "bends": [{ "note": note, "points": [{ "at": 0.5, "semitones": 12 }, { "at": 0, "semitones": -2 }] }] }),
            &mut host,
        );
        let bend = &notes(&host, clip)[0].bend;
        assert_eq!(bend.len(), 2);
        assert_eq!(bend[0].semitones, -2.0, "sorted by time");
        assert_eq!(bend[1].at, Tick::new(480));
        done(
            "set_note_bends",
            json!({ "clip": clip.0, "bends": [{ "note": note, "points": [] }] }),
            &mut host,
        );
        assert!(notes(&host, clip)[0].bend.is_empty());
    }

    #[test]
    fn notes_go_only_in_midi_clips() {
        let (mut host, _) = rig();
        let track = host.project.track_ids()[0];
        let error = failure(
            "write_notes",
            json!({ "clip": track.0, "mode": "add", "notes": [] }),
            &mut host,
        );
        assert!(error.contains("is a track"), "{error}");
    }
}
