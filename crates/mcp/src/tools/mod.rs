//! The tools an LLM calls: each a name, a description, a JSON schema, a few
//! example calls and a body. The table is shared by the bridge (which
//! lists the tools without the app) and the app (which runs them).
//!
//! A tool's arguments are a `#[serde(deny_unknown_fields)]` struct read by
//! [`args`], beside the schema that describes it; the tests check every
//! schema against its struct and every example against both, so the two
//! can't drift apart. Bodies come in four kinds (see [`Kind`]): reads,
//! edits planned against the project and applied as one undo step (the
//! only kind a `batch` can hold), edits that need more of the app than the
//! project, and jobs that answer on a later frame.

pub mod analysis;
pub mod batch;
pub mod clips;
pub mod control;
pub mod notes;
pub mod read;
pub mod sound;
pub mod tracks;

use std::sync::OnceLock;

use project::{EditError, Project, TrackId, TrackKind};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::host::{Apply, Host};
use crate::protocol::{self, Content};

/// Notes a write may carry, at most.
pub const MAX_NOTES: usize = 10_000;
/// Notes a read returns at a time; the rest are paged with a cursor.
pub const NOTES_PAGE: usize = 2000;
/// Operations a batch may hold, at most.
pub const MAX_BATCH: usize = 200;
/// Tracks one call may arrange, at most.
pub const MAX_TRACKS: usize = 64;

#[derive(Clone, Copy)]
pub struct Tool {
    pub name: &'static str,
    /// What the tool does, for the model: the first sentence is a summary.
    pub description: &'static str,
    /// The arguments' JSON schema (an object).
    pub schema: fn() -> Value,
    /// Example arguments, as JSON text. [`list`] adds them to the
    /// description; the tests check that each one parses.
    pub examples: &'static [&'static str],
    pub kind: Kind,
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// Looks and changes nothing.
    Read(fn(&Value, &mut dyn Host) -> Result<Output, ToolError>),
    /// Plans an edit against the project as it is; [`call`] applies the
    /// plan as one undo step. A batch holds only these.
    Edit(fn(&Value, &Project) -> Result<Plan, ToolError>),
    /// Changes things through the host rather than the project alone
    /// (undo, the transport, loading files). Not batchable.
    Session(fn(&Value, &mut dyn Host) -> Result<Outcome, ToolError>),
    /// Starts a job that answers later (a render for analysis).
    Pending(fn(&Value, &mut dyn Host) -> Result<Outcome, ToolError>),
}

/// An edit, planned: checked against the project, and ready to apply.
pub struct Plan {
    /// The undo step's name, without the "LLM: " that [`call`] adds.
    pub label: String,
    /// Applies the edit, returning the result to send back (with `id` for
    /// anything it made, and a `summary`). An error leaves the project as
    /// it was: the host rolls it back.
    pub apply: Apply<'static>,
}

/// What a tool answers: JSON, and any images (PNGs).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Output {
    pub json: Value,
    pub images: Vec<Vec<u8>>,
}

impl Output {
    pub fn json(json: Value) -> Output {
        Output {
            json,
            images: Vec::new(),
        }
    }
}

pub enum Outcome {
    Done(Output),
    /// Not yet: the host calls the waiter every frame until it answers.
    Waiting(Waiter),
}

/// Polled every frame until it returns the answer.
pub type Waiter = Box<dyn FnMut(&mut dyn Host) -> Option<Result<Output, ToolError>>>;

/// Why a tool failed, said so the model can fix the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

impl From<String> for ToolError {
    fn from(message: String) -> ToolError {
        ToolError(message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> ToolError {
        ToolError(message.to_owned())
    }
}

/// Every tool, in the order `tools/list` shows them.
pub fn all() -> &'static [Tool] {
    static ALL: OnceLock<Vec<Tool>> = OnceLock::new();
    ALL.get_or_init(|| {
        [
            read::TOOLS,
            tracks::TOOLS,
            clips::TOOLS,
            notes::TOOLS,
            sound::TOOLS,
            control::TOOLS,
            analysis::TOOLS,
            batch::TOOLS,
        ]
        .concat()
    })
}

pub fn find(name: &str) -> Option<&'static Tool> {
    all().iter().find(|t| t.name == name)
}

/// The result of `tools/list`: every tool with its schema, its examples
/// appended to its description.
pub fn list() -> Value {
    let tools: Vec<Value> = all()
        .iter()
        .map(|tool| {
            let mut description = tool.description.to_owned();
            if !tool.examples.is_empty() {
                description.push_str("\n\nExamples:");
                for example in tool.examples {
                    description.push('\n');
                    description.push_str(example);
                }
            }
            json!({
                "name": tool.name,
                "description": description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Runs tool `name`. Edits are applied as one undo step labelled
/// "LLM: …"; a failed one leaves the project as it was.
pub fn call(name: &str, args: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let tool = find(name).ok_or_else(|| {
        ToolError(format!(
            "there's no tool called {name:?}; tools/list lists them"
        ))
    })?;
    match tool.kind {
        Kind::Read(read) => read(args, host).map(Outcome::Done),
        Kind::Edit(plan) => {
            let plan = plan(args, host.project())?;
            let label = format!("LLM: {}", plan.label);
            let json = host
                .edit(&label, plan.apply)
                .map_err(|e| ToolError(explain(&e, host.project())))?;
            Ok(Outcome::Done(Output::json(json)))
        }
        Kind::Session(run) | Kind::Pending(run) => run(args, host),
    }
}

/// A finished call as the result of `tools/call`: its JSON as text and its
/// images, or its error flagged `isError`.
pub fn tool_result(result: &Result<Output, ToolError>) -> Value {
    match result {
        Ok(output) => {
            let mut content = vec![Content::Text(output.json.to_string())];
            content.extend(
                output
                    .images
                    .iter()
                    .map(|png| Content::Image { png: png.clone() }),
            );
            protocol::tool_result(&content, false)
        }
        Err(error) => protocol::tool_result(&[Content::Text(error.0.clone())], true),
    }
}

/// Reads a tool's arguments. A malformed call is an error naming the
/// argument at fault ("argument `notes[3].pitch`: …"); every such error
/// starts with "argument", which tells the model the call itself is wrong.
pub fn args<T: DeserializeOwned>(v: &Value) -> Result<T, ToolError> {
    let empty = json!({});
    let v = if v.is_null() { &empty } else { v };
    serde_path_to_error::deserialize(v).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.into_inner();
        if path == "." {
            ToolError(format!("arguments: {inner}"))
        } else {
            ToolError(format!("argument `{path}`: {inner}"))
        }
    })
}

/// An edit's error as a sentence that says what to do about it: what the
/// id named instead, which tracks would do.
pub fn explain(error: &EditError, project: &Project) -> String {
    match error {
        EditError::NoSuchTrack(id) => {
            missing("track", id.0, project, "get_project lists the tracks")
        }
        EditError::NoSuchClip(id) => {
            missing("clip", id.0, project, "get_track lists a track's clips")
        }
        EditError::NoSuchNote(id) => {
            missing("note", id.0, project, "get_clip lists a clip's notes")
        }
        EditError::NoSuchDevice(id) => {
            missing("device", id.0, project, "get_track lists a track's devices")
        }
        EditError::UnknownEffect(id) => {
            format!("there's no effect called {id:?}; list_devices lists them")
        }
        EditError::WrongTrackKind(id) => wrong_kind(*id, project),
        EditError::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

/// "track 7 doesn't exist (7 is a clip); …".
fn missing(kind: &str, id: u32, project: &Project, hint: &str) -> String {
    match what_is(id, project) {
        Some(other) if other != kind => format!("{id} is a {other}, not a {kind}; {hint}"),
        _ => format!("{kind} {id} doesn't exist; {hint}"),
    }
}

/// What kind of thing id `id` names in the project, if anything.
pub fn what_is(id: u32, project: &Project) -> Option<&'static str> {
    if project.track(TrackId(id)).is_some() {
        return Some("track");
    }
    if project.clip(project::ClipId(id)).is_some() {
        return Some("clip");
    }
    if project.device(project::DeviceId(id)).is_some() {
        return Some("device");
    }
    let mut note = false;
    project.visit(|track, _| {
        if let TrackKind::Midi { clips, .. } = &track.kind {
            note |= clips.iter().any(|c| c.notes.iter().any(|n| n.id.0 == id));
        }
    });
    note.then_some("note")
}

/// Why a track can't take what was asked, and which tracks can.
fn wrong_kind(id: TrackId, project: &Project) -> String {
    let Some(track) = project.track(id) else {
        return missing("track", id.0, project, "get_project lists the tracks");
    };
    let named = format!("track {id} ({:?})", track.name);
    match track.kind {
        TrackKind::Audio { .. } => format!(
            "{named} is an audio track; MIDI clips need a MIDI track ({}), \
             and only groups hold tracks ({})",
            tracks_of("MIDI tracks", project, |k| matches!(
                k,
                TrackKind::Midi { .. }
            )),
            tracks_of("groups", project, |k| matches!(k, TrackKind::Group { .. })),
        ),
        TrackKind::Midi { .. } => format!(
            "{named} is a MIDI track; audio clips need an audio track ({}), \
             and only groups hold tracks ({})",
            tracks_of("audio tracks", project, |k| matches!(
                k,
                TrackKind::Audio { .. }
            )),
            tracks_of("groups", project, |k| matches!(k, TrackKind::Group { .. })),
        ),
        TrackKind::Group { .. } => format!(
            "{named} is a group: it holds tracks, not clips or an instrument ({}; {})",
            tracks_of("MIDI tracks", project, |k| matches!(
                k,
                TrackKind::Midi { .. }
            )),
            tracks_of("audio tracks", project, |k| matches!(
                k,
                TrackKind::Audio { .. }
            )),
        ),
    }
}

/// "MIDI tracks: 12 Bass, 18 Lead", at most a dozen; "no MIDI tracks".
fn tracks_of(what: &str, project: &Project, kind: impl Fn(&TrackKind) -> bool) -> String {
    const SHOWN: usize = 12;
    let mut found = Vec::new();
    project.visit(|track, _| {
        if kind(&track.kind) {
            found.push(format!("{} {}", track.id, track.name));
        }
    });
    match found.len() {
        0 => format!("no {what}"),
        n if n > SHOWN => format!(
            "{what}: {}, and {} more",
            found[..SHOWN].join(", "),
            n - SHOWN
        ),
        _ => format!("{what}: {}", found.join(", ")),
    }
}

/// What the server tells the model when it connects: the conventions every
/// tool follows.
pub const INSTRUCTIONS: &str = "\
canopy is a DAW; these tools read and edit the project open in it, and measure its sound.

Ids: every track, clip, note, device and modulator has a number from one counter, never \
reused, so a number alone names it. Reads always include ids; get_project is the place to start, \
and it includes what the user has selected, so \"this clip\" or \"these tracks\" resolve.

Time is in beats (quarter notes, fractions allowed), 0 at the song's start; reads add \"bar.beat\" \
(\"5.3\" is bar 5, beat 3, counted from 1) to make them easy to read. Notes' times are beats from \
their clip's start. Positions inside an audio file are in seconds.

Pitch is a MIDI number or a name with middle C as \"C3\" (= 60), as canopy's piano roll names \
them: \"F#2\", \"Bb3\".

Parameters are set in their own units (Hz, dB, ms, %), as get_device lists them with their ranges; \
choices by their label. A value out of range is an error that states the range; nothing is \
clamped.

Every edit is one undo step the user can see and undo, labelled \"LLM: ...\", and returns the \
ids of what it made and a one-line summary. A failed edit changes nothing and says what to fix. \
Saving, opening and closing projects are the user's alone.

Analysis is measurement, not listening: to check an edit, measure again.";

/// Pieces of the tools' JSON schemas.
pub mod schema {
    use serde_json::{Value, json};

    /// An object with these properties (a `json!` object) of which
    /// `required` must be given, and no others.
    pub fn object(properties: Value, required: &[&str]) -> Value {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    pub fn id(description: &str) -> Value {
        json!({ "type": "integer", "minimum": 1, "description": description })
    }

    pub fn ids(description: &str) -> Value {
        json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "description": description })
    }

    pub fn integer(description: &str) -> Value {
        json!({ "type": "integer", "description": description })
    }

    pub fn count(description: &str) -> Value {
        json!({ "type": "integer", "minimum": 0, "description": description })
    }

    pub fn number(description: &str) -> Value {
        json!({ "type": "number", "description": description })
    }

    /// A time in beats from the song's (or a clip's) start.
    pub fn beats(description: &str) -> Value {
        json!({ "type": "number", "minimum": 0, "description": description })
    }

    pub fn string(description: &str) -> Value {
        json!({ "type": "string", "description": description })
    }

    pub fn boolean(description: &str) -> Value {
        json!({ "type": "boolean", "description": description })
    }

    pub fn choice(options: &[&str], description: &str) -> Value {
        json!({ "type": "string", "enum": options, "description": description })
    }

    pub fn array(items: Value, description: &str) -> Value {
        json!({ "type": "array", "items": items, "description": description })
    }

    /// A MIDI number or a name ("C3" = 60).
    pub fn pitch(description: &str) -> Value {
        json!({ "type": ["integer", "string"], "description": description })
    }

    /// An automation target (see `units::parse_target`).
    pub fn target(description: &str) -> Value {
        json!({
            "oneOf": [
                { "type": "string", "enum": ["volume", "pan"] },
                {
                    "type": "object",
                    "properties": { "instrument": { "type": "string" } },
                    "required": ["instrument"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": { "device": { "type": "integer" }, "param": { "type": "string" } },
                    "required": ["device", "param"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": { "modulator": { "type": "integer" }, "param": { "type": "string" } },
                    "required": ["modulator", "param"],
                    "additionalProperties": false,
                },
            ],
            "description": description,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use project::{NewTrack, TrackPosition};
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Example {
        track: TrackId,
        name: Option<String>,
    }

    #[test]
    fn an_unknown_argument_is_an_error() {
        let error = args::<Example>(&json!({ "track": 3, "colour": "red" })).unwrap_err();
        assert!(error.0.starts_with("argument"), "{error}");
        assert!(error.0.contains("colour"), "{error}");
        let error = args::<Example>(&json!({ "track": "three" })).unwrap_err();
        assert!(error.0.starts_with("argument `track`"), "{error}");
        let error = args::<Example>(&json!({})).unwrap_err();
        assert!(
            error.0.contains("track"),
            "a missing argument is named: {error}"
        );
        assert!(args::<Example>(&json!({ "track": 3 })).is_ok());
    }

    #[test]
    fn explain_names_the_fix() {
        let mut project = Project::new();
        let bass = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project.rename_track(bass, "Bass").unwrap();
        let lead = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project.rename_track(lead, "Lead").unwrap();
        let vocal = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        project.rename_track(vocal, "Vocal").unwrap();

        let message = explain(&EditError::WrongTrackKind(vocal), &project);
        assert!(
            message.contains("MIDI clips need a MIDI track"),
            "{message}"
        );
        assert!(message.contains(&format!("{bass} Bass")), "{message}");
        assert!(message.contains(&format!("{lead} Lead")), "{message}");
        assert!(message.contains(&format!("track {vocal}")), "{message}");

        let message = explain(&EditError::NoSuchClip(project::ClipId(bass.0)), &project);
        assert!(message.contains("is a track, not a clip"), "{message}");
        assert_eq!(
            explain(&EditError::Invalid("pan is -1–1".into()), &project),
            "pan is -1–1"
        );
    }

    #[test]
    fn an_unknown_tool_is_an_error() {
        let mut host = FakeHost::new(Project::new());
        let Err(error) = call("make_hit", &json!({}), &mut host) else {
            panic!("no such tool");
        };
        assert!(error.0.contains("make_hit"), "{error}");
    }

    #[test]
    fn every_tool_has_a_description_schema_and_examples() {
        let mut names = std::collections::HashSet::new();
        for tool in all() {
            assert!(names.insert(tool.name), "{} is listed twice", tool.name);
            assert!(
                tool.description.len() > 20,
                "{} has a description",
                tool.name
            );
            assert!(!tool.examples.is_empty(), "{} has examples", tool.name);
            let schema = (tool.schema)();
            assert_eq!(schema["type"], json!("object"), "{}", tool.name);
            assert_eq!(
                schema["additionalProperties"],
                json!(false),
                "{}",
                tool.name
            );
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    schema["properties"]
                        .get(required.as_str().unwrap())
                        .is_some(),
                    "{}: required {required} is a property",
                    tool.name
                );
            }
        }
        let listed = list();
        assert_eq!(listed["tools"].as_array().unwrap().len(), all().len());
        assert!(listed["tools"][0]["inputSchema"].is_object());
    }

    /// A project every tool can be called on: a MIDI track with a clip and
    /// an effect, an audio track, a group.
    fn rig() -> FakeHost {
        let mut project = Project::new();
        let midi = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        project
            .add_track(NewTrack::Group, TrackPosition::END)
            .unwrap();
        project
            .add_midi_clip(midi, midi::Tick::ZERO, midi::Tick::new(3840))
            .unwrap();
        project
            .add_device(Some(midi), project::EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        FakeHost::new(project)
    }

    /// Whether `result` failed because the call's arguments didn't parse,
    /// rather than because of what they asked.
    fn malformed(result: &Result<Outcome, ToolError>) -> Option<&str> {
        match result {
            Err(ToolError(message)) if message.starts_with("argument") => Some(message),
            _ => None,
        }
    }

    #[test]
    fn every_example_parses() {
        for tool in all() {
            for example in tool.examples {
                let value: Value = serde_json::from_str(example)
                    .unwrap_or_else(|e| panic!("{}: {example} isn't JSON: {e}", tool.name));
                let result = call(tool.name, &value, &mut rig());
                assert_eq!(malformed(&result), None, "{}: {example}", tool.name);
            }
        }
    }

    /// A value of the shape `schema` describes: the first choice, the first
    /// alternative, every property of an object.
    fn sample(schema: &Value) -> Value {
        if let Some(first) = schema.get("oneOf").and_then(|o| o.get(0)) {
            return sample(first);
        }
        if let Some(first) = schema.get("enum").and_then(|e| e.get(0)) {
            return first.clone();
        }
        let kind = match &schema["type"] {
            Value::Array(kinds) => kinds[0].as_str().unwrap_or("integer"),
            Value::String(kind) => kind.as_str(),
            _ => "integer",
        };
        match kind {
            "object" => Value::Object(
                schema["properties"]
                    .as_object()
                    .map(|p| p.iter().map(|(k, v)| (k.clone(), sample(v))).collect())
                    .unwrap_or_default(),
            ),
            "array" => json!([sample(&schema["items"])]),
            "string" => json!("C3"),
            "number" => json!(1.0),
            "boolean" => json!(false),
            _ => json!(1),
        }
    }

    #[test]
    fn the_schema_lists_exactly_the_accepted_fields() {
        for tool in all() {
            let schema = (tool.schema)();
            let full = sample(&schema);
            let result = call(tool.name, &full, &mut rig());
            assert_eq!(
                malformed(&result),
                None,
                "{} with every property: {full}",
                tool.name
            );

            let required: serde_json::Map<String, Value> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let r = r.as_str().unwrap();
                    (r.to_owned(), sample(&schema["properties"][r]))
                })
                .collect();
            let minimal = Value::Object(required);
            let result = call(tool.name, &minimal, &mut rig());
            assert_eq!(
                malformed(&result),
                None,
                "{} with only what's required: {minimal}",
                tool.name
            );

            let mut extra = full.clone();
            extra["not_in_the_schema"] = json!(1);
            let result = call(tool.name, &extra, &mut rig());
            assert!(
                malformed(&result).is_some(),
                "{} refuses a property it doesn't list",
                tool.name
            );
        }
    }
}
