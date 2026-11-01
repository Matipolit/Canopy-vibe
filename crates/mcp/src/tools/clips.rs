//! Tools that make and change clips: a MIDI clip with its notes or an
//! audio clip from a file; clips moved, trimmed, renamed, muted and their
//! audio settings changed; and clips split, deleted, duplicated, copied,
//! chopped, or processed (consolidated, normalized, frozen).

use std::path::PathBuf;

use midi::{Pitch, Tick, Velocity};
use project::{
    BendPoint, ChannelMode, ClipId, ClipMove, ClipRef, EditError, Note, NoteId, Project,
    SILENCE_DB, Stretch, StretchMode, Tempo, TrackId, TrackKind,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Kind, MAX_NOTES, Outcome, Output, Plan, Tool, ToolError, Waiter, args, schema};
use crate::host::{Apply, ClipProcess, Host, Poll};
use crate::units::{bar_beat, beats_to_offset, beats_to_tick, parse_pitch, tick_to_beats};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "add_clip",
        description: "Adds a clip. A MIDI clip on a MIDI track at `start` (beats), `length` \
                      beats long (enough whole bars for its notes if left out), with `notes` \
                      if given (times in beats from the clip's start), as one undo step. Or an \
                      audio clip from an audio `file` (its full path) on an audio track, or on \
                      a new one without `track`: the answer comes once the file has loaded.",
        schema: add_clip_schema,
        examples: &[
            r#"{"track": 12, "start": 16, "kind": "midi", "length": 4, "name": "Hook", "notes": [{"start": 0, "length": 0.5, "pitch": "C3", "velocity": 100}, {"start": 0.5, "length": 0.5, "pitch": "Eb3"}]}"#,
            r#"{"track": 14, "start": 0, "kind": "audio", "file": "/home/me/samples/vocal.wav"}"#,
            r#"{"start": 8, "kind": "audio", "file": "/home/me/samples/break.wav"}"#,
        ],
        kind: Kind::Session(add_clip),
    },
    Tool {
        name: "edit_clips",
        description: "Changes clips, all in one undo step: moves them (`move_by` beats, or \
                      `start` for the earliest, keeping their layout; `track` for another track \
                      of the same kind), sets their `length` in beats, `name` or `muted`. Audio \
                      clips only: `gain_db` (-60–12), fades in seconds and their curves (-1–1, \
                      negative rises fast first), `reversed`, `looped`, `stretch` (how speed \
                      changes, the speed, the material's own tempo, tempo sync), `pitch` \
                      (semitones, -48–48, in the \"stretch\" mode), `channels`, `polarity` \
                      (true inverts it) and `slip` (seconds to slide the audio within the clip, \
                      later if positive).",
        schema: edit_clips_schema,
        examples: &[
            r#"{"clips": [31, 32], "move_by": 4}"#,
            r#"{"clips": [31], "start": 32, "track": 15, "name": "Verse vocal"}"#,
            r#"{"clips": [40], "gain_db": -3, "fade_in": 0.01, "fade_out": 0.5, "fade_out_curve": 0.5}"#,
            r#"{"clips": [40], "stretch": {"mode": "stretch", "original_bpm": 92, "sync": true}, "pitch": -2}"#,
        ],
        kind: Kind::Edit(edit_clips),
    },
    Tool {
        name: "clip_ops",
        description: "Does something to clips. `split` at `at` (beats, song time): returns the \
                      new right-hand halves. `delete`. `duplicate` `times` times (1–64), copies \
                      end to end after them. `copy_to` `at` (beats), keeping their layout, onto \
                      `track` if given. `chop` into pieces: on the song's grid, `division` to a \
                      bar (mode \"grid\", the default), or an audio clip at its transients (mode \
                      \"transients\", `sensitivity` 0–1, higher finds more). Audio clips: \
                      `consolidate` (into one new clip on their track) and `normalize`; MIDI \
                      clips: `freeze` (into audio). Every op but `delete` returns the clips it \
                      made.",
        schema: clip_ops_schema,
        examples: &[
            r#"{"op": "split", "clips": [31], "at": 18}"#,
            r#"{"op": "duplicate", "clips": [31, 32], "times": 3}"#,
            r#"{"op": "copy_to", "clips": [31], "at": 64, "track": 15}"#,
            r#"{"op": "chop", "clips": [40], "division": 8}"#,
            r#"{"op": "chop", "clips": [40], "mode": "transients", "sensitivity": 0.6}"#,
            r#"{"op": "normalize", "clips": [40, 41]}"#,
        ],
        kind: Kind::Session(clip_ops),
    },
];

/// Clips one call may change, at most.
const MAX_CLIPS: usize = 1000;
/// Copies one `duplicate` may make of each clip, at most.
const MAX_TIMES: u32 = 64;
/// Pieces a grid chop may cut a bar into, at most.
const MAX_DIVISION: u32 = 64;
/// The loudest a clip's gain goes, as the timeline's gain handle allows.
const MAX_CLIP_GAIN_DB: f64 = 12.0;

/// Applies `plan` as one undo step, from a tool that isn't an `Edit`
/// (because some of what it does needs the host), as [`super::call`] does
/// for edits.
pub(super) fn apply(host: &mut dyn Host, plan: Plan) -> Result<Outcome, ToolError> {
    let label = format!("LLM: {}", plan.label);
    let json = host
        .edit(&label, plan.apply)
        .map_err(|e| ToolError(super::explain(&e, host.project())))?;
    Ok(Outcome::Done(Output::json(json)))
}

/// `id` as a track, or the error that says what it is instead.
pub(super) fn track_of(project: &Project, id: TrackId) -> Result<&project::Track, ToolError> {
    project
        .track(id)
        .ok_or_else(|| ToolError(super::explain(&EditError::NoSuchTrack(id), project)))
}

/// Every clip in `ids` (checked to exist, at most [`MAX_CLIPS`], none
/// twice) with the track it is on.
fn find_clips<'p>(
    project: &'p Project,
    ids: &[ClipId],
) -> Result<Vec<(TrackId, ClipRef<'p>)>, ToolError> {
    if ids.is_empty() {
        return Err("`clips` is empty; give the ids of the clips (get_track lists them)".into());
    }
    if ids.len() > MAX_CLIPS {
        return Err(ToolError(format!(
            "{} clips is more than one call takes ({MAX_CLIPS})",
            ids.len()
        )));
    }
    let mut found = Vec::with_capacity(ids.len());
    for (i, &id) in ids.iter().enumerate() {
        if ids[..i].contains(&id) {
            return Err(ToolError(format!("clip {id} is listed twice")));
        }
        let clip = project
            .clip(id)
            .ok_or_else(|| ToolError(super::explain(&EditError::NoSuchClip(id), project)))?;
        found.push(clip);
    }
    Ok(found)
}

fn invalid(message: impl Into<String>) -> EditError {
    EditError::Invalid(message.into())
}

/// "clip 31" or "3 clips", for summaries.
fn clips_text(n: usize, first: ClipId) -> String {
    if n == 1 {
        format!("clip {first}")
    } else {
        format!("{n} clips")
    }
}

// Notes, as add_clip takes them.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoteArgs {
    start: f64,
    length: f64,
    pitch: Value,
    velocity: Option<i64>,
    release_velocity: Option<i64>,
    muted: Option<bool>,
    bend: Option<Vec<BendArgs>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BendArgs {
    at: f64,
    semitones: f64,
    curve: Option<f64>,
}

/// A note's schema: start and length in beats from the clip's start.
pub(super) fn note_schema() -> Value {
    schema::object(
        json!({
            "start": schema::beats("When it starts, in beats from the clip's start."),
            "length": schema::number("How long it lasts, in beats (more than 0)."),
            "pitch": schema::pitch("A MIDI number or a name, \"C3\" = 60."),
            "velocity": schema::integer("1–127; 100 if left out."),
            "release_velocity": schema::integer("0–127, the note off's velocity; most instruments ignore it."),
            "muted": schema::boolean("Kept but not played."),
            "bend": schema::array(
                schema::object(
                    json!({
                        "at": schema::beats("From the note's start, in beats."),
                        "semitones": schema::number("How far from the note's pitch, -48–48."),
                        "curve": schema::number("How the segment into this point bends, -1–1; 0 is straight."),
                    }),
                    &["at", "semitones"],
                ),
                "The note's own pitch curve.",
            ),
        }),
        &["start", "length", "pitch"],
    )
}

/// Checks notes and makes them, still without ids (see [`with_ids`]).
pub(super) fn parse_notes(notes: &[NoteArgs]) -> Result<Vec<Note>, ToolError> {
    if notes.len() > MAX_NOTES {
        return Err(ToolError(format!(
            "{} notes is more than one call takes ({MAX_NOTES}); write them in parts",
            notes.len()
        )));
    }
    notes
        .iter()
        .enumerate()
        .map(|(i, n)| parse_note(n).map_err(|e| ToolError(format!("note {i}: {e}"))))
        .collect()
}

fn parse_note(n: &NoteArgs) -> Result<Note, String> {
    let start = beats_to_tick(n.start)?;
    let len = beats_to_offset(n.length)?;
    if len <= Tick::ZERO {
        return Err(format!(
            "length {} is too short; a note lasts more than 0 beats",
            n.length
        ));
    }
    let pitch: Pitch = parse_pitch(&n.pitch)?;
    let velocity = match n.velocity {
        None => 100,
        Some(v @ 1..=127) => v as u8,
        Some(v) => return Err(format!("velocity is 1–127; {v} is out of range")),
    };
    let release_velocity = match n.release_velocity {
        None => Velocity::CENTER,
        Some(v @ 0..=127) => Velocity::from_midi1(v as u8),
        Some(v) => return Err(format!("release_velocity is 0–127; {v} is out of range")),
    };
    let mut bend = Vec::new();
    for b in n.bend.iter().flatten() {
        let limit = f64::from(BendPoint::MAX_SEMITONES);
        if !(-limit..=limit).contains(&b.semitones) {
            return Err(format!(
                "a bend is -{limit}–{limit} semitones; {} is out of range",
                b.semitones
            ));
        }
        let curve = b.curve.unwrap_or(0.0);
        if !(-1.0..=1.0).contains(&curve) {
            return Err(format!("a bend's curve is -1–1; {curve} is out of range"));
        }
        bend.push(BendPoint {
            at: beats_to_tick(b.at)?,
            semitones: b.semitones as f32,
            curve: curve as f32,
        });
    }
    bend.sort_by_key(|p| p.at);
    Ok(Note {
        id: NoteId(0),
        start,
        len,
        pitch,
        velocity: Velocity::from_midi1(velocity),
        release_velocity,
        muted: n.muted.unwrap_or(false),
        bend,
    })
}

/// Gives each note a fresh id from the project's counter.
pub(super) fn with_ids(project: &mut Project, mut notes: Vec<Note>) -> Vec<Note> {
    for note in &mut notes {
        note.id = project.new_note_id();
    }
    notes
}

// add_clip

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ClipKind {
    Midi,
    Audio,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddClipArgs {
    track: Option<TrackId>,
    start: f64,
    kind: ClipKind,
    length: Option<f64>,
    name: Option<String>,
    notes: Option<Vec<NoteArgs>>,
    file: Option<String>,
}

fn add_clip_schema() -> Value {
    schema::object(
        json!({
            "track": schema::id("The track: a MIDI track for a MIDI clip, an audio track for an audio clip (a new audio track if left out)."),
            "start": schema::beats("Where the clip starts, in beats from the song's start."),
            "kind": schema::choice(&["midi", "audio"], "A MIDI clip of notes, or an audio clip of a file."),
            "length": schema::number("MIDI: how long the clip is, in beats."),
            "name": schema::string("MIDI: the clip's name (the track's if left out)."),
            "notes": schema::array(note_schema(), "MIDI: the clip's notes, at most 10000."),
            "file": schema::string("Audio: the file's full path (WAV, FLAC, MP3, OGG, AIFF…)."),
        }),
        &["start", "kind"],
    )
}

fn add_clip(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: AddClipArgs = args(v)?;
    let start = beats_to_tick(a.start)?;
    match a.kind {
        ClipKind::Midi => {
            if a.file.is_some() {
                return Err("`file` is for audio clips (kind \"audio\")".into());
            }
            let plan = plan_midi_clip(a, start, host.project())?;
            apply(host, plan)
        }
        ClipKind::Audio => {
            let midi_only: Vec<&str> = [
                ("length", a.length.is_some()),
                ("name", a.name.is_some()),
                ("notes", a.notes.is_some()),
            ]
            .into_iter()
            .filter_map(|(name, given)| given.then_some(name))
            .collect();
            if !midi_only.is_empty() {
                return Err(ToolError(format!(
                    "{} {} for MIDI clips; an audio clip is as long as its file (edit_clips \
                     trims and renames it once it's added)",
                    midi_only.join(", "),
                    if midi_only.len() == 1 { "is" } else { "are" },
                )));
            }
            let file = a
                .file
                .ok_or("an audio clip needs `file`, the audio file's full path")?;
            add_audio_clip(host, PathBuf::from(file), a.track, start)
        }
    }
}

/// The edit that adds a MIDI clip with its notes, checked first.
fn plan_midi_clip(a: AddClipArgs, start: Tick, project: &Project) -> Result<Plan, ToolError> {
    let track = a
        .track
        .ok_or("a MIDI clip needs `track`, a MIDI track's id")?;
    if !matches!(track_of(project, track)?.kind, TrackKind::Midi { .. }) {
        return Err(ToolError(super::explain(
            &EditError::WrongTrackKind(track),
            project,
        )));
    }
    let notes = parse_notes(a.notes.as_deref().unwrap_or_default())?;
    let bar = project.tempo.bar_len().get().max(1);
    let length = match a.length {
        Some(beats) => {
            let length = beats_to_offset(beats)?;
            if length <= Tick::ZERO {
                return Err(ToolError(format!(
                    "length {beats} is too short; a clip lasts more than 0 beats"
                )));
            }
            length
        }
        // Whole bars, enough for every note.
        None => {
            let end = notes.iter().map(|n| (n.start + n.len).get()).max();
            let bars = end.map_or(1, |end| (end + bar - 1) / bar).max(1);
            Tick::new(bars * bar)
        }
    };
    let name = a.name;
    let count = notes.len();
    let at = bar_beat(start, &project.tempo);
    Ok(Plan {
        label: "Add MIDI Clip".to_owned(),
        apply: Box::new(move |p| {
            let clip = p.add_midi_clip(track, start, length)?;
            if let Some(name) = &name {
                p.rename_clip(clip, name)?;
            }
            if count > 0 {
                let notes = with_ids(p, notes);
                p.set_notes(clip, notes)?;
            }
            Ok(json!({
                "id": clip.0,
                "track": track.0,
                "start": tick_to_beats(start),
                "length": tick_to_beats(length),
                "summary": format!(
                    "added MIDI clip {clip} on track {track} at {at}, {} beats long, with {count} notes",
                    tick_to_beats(length)
                ),
            }))
        }),
    })
}

/// Starts loading `file` to become a clip; answers once it has.
fn add_audio_clip(
    host: &mut dyn Host,
    file: PathBuf,
    track: Option<TrackId>,
    start: Tick,
) -> Result<Outcome, ToolError> {
    if !file.is_absolute() {
        return Err(ToolError(format!(
            "{} isn't a full path; give the file's full path (canopy runs in its own folder)",
            file.display()
        )));
    }
    if !file.is_file() {
        return Err(ToolError(format!("{} isn't a file", file.display())));
    }
    if !media::is_supported_extension(&file) {
        return Err(ToolError(format!(
            "{} isn't an audio file canopy can read (WAV, FLAC, MP3, OGG, AIFF…)",
            file.display()
        )));
    }
    let project = host.project();
    if let Some(track) = track
        && !matches!(track_of(project, track)?.kind, TrackKind::Audio { .. })
    {
        return Err(ToolError(super::explain(
            &EditError::WrongTrackKind(track),
            project,
        )));
    }
    let ticket = host.import_audio(file.clone(), track, start)?;
    let waiter: Waiter = Box::new(move |host| match host.poll(ticket) {
        Poll::Waiting { .. } => None,
        Poll::Imported(Ok(clip)) => Some(Ok(imported(host.project(), clip))),
        Poll::Imported(Err(error)) => Some(Err(ToolError(format!(
            "{} wasn't added: {error}",
            file.display()
        )))),
        Poll::Rendered(_) => Some(Err("canopy answered the import with a render".into())),
    });
    Ok(Outcome::Waiting(waiter))
}

/// What add_clip answers for an imported file's clip.
fn imported(project: &Project, clip: ClipId) -> Output {
    let Some((track, found)) = project.clip(clip) else {
        // Undone already, before the answer went out.
        return Output::json(json!({
            "id": clip.0,
            "summary": format!("added audio clip {clip}, which has since been undone"),
        }));
    };
    let (start, end) = (found.start(), project.clip_end(found));
    Output::json(json!({
        "id": clip.0,
        "track": track.0,
        "name": found.name(),
        "start": tick_to_beats(start),
        "length": tick_to_beats(end - start),
        "summary": format!(
            "added audio clip {clip} ({:?}) on track {track} at {}, {} beats long",
            found.name(),
            bar_beat(start, &project.tempo),
            tick_to_beats(end - start),
        ),
    }))
}

// edit_clips

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Resample,
    Stretch,
    Beats,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Channels {
    Stereo,
    Mono,
    Swap,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct StretchArgs {
    mode: Option<Mode>,
    speed: Option<f64>,
    original_bpm: Option<f64>,
    sync: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditClipsArgs {
    clips: Vec<ClipId>,
    move_by: Option<f64>,
    start: Option<f64>,
    track: Option<TrackId>,
    length: Option<f64>,
    name: Option<String>,
    muted: Option<bool>,
    gain_db: Option<f64>,
    fade_in: Option<f64>,
    fade_out: Option<f64>,
    fade_in_curve: Option<f64>,
    fade_out_curve: Option<f64>,
    reversed: Option<bool>,
    looped: Option<bool>,
    stretch: Option<StretchArgs>,
    pitch: Option<f64>,
    channels: Option<Channels>,
    polarity: Option<bool>,
    slip: Option<f64>,
}

impl EditClipsArgs {
    /// The audio-only fields given.
    fn audio_fields(&self) -> Vec<&'static str> {
        [
            ("gain_db", self.gain_db.is_some()),
            ("fade_in", self.fade_in.is_some()),
            ("fade_out", self.fade_out.is_some()),
            ("fade_in_curve", self.fade_in_curve.is_some()),
            ("fade_out_curve", self.fade_out_curve.is_some()),
            ("reversed", self.reversed.is_some()),
            ("looped", self.looped.is_some()),
            ("stretch", self.stretch.is_some()),
            ("pitch", self.pitch.is_some()),
            ("channels", self.channels.is_some()),
            ("polarity", self.polarity.is_some()),
            ("slip", self.slip.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, given)| given.then_some(name))
        .collect()
    }

    fn fades(&self) -> bool {
        self.fade_in.is_some()
            || self.fade_out.is_some()
            || self.fade_in_curve.is_some()
            || self.fade_out_curve.is_some()
    }

    fn audio_settings(&self) -> bool {
        self.reversed.is_some()
            || self.looped.is_some()
            || self.stretch.is_some()
            || self.pitch.is_some()
            || self.channels.is_some()
            || self.polarity.is_some()
    }
}

fn edit_clips_schema() -> Value {
    schema::object(
        json!({
            "clips": schema::ids("The clips to change."),
            "move_by": schema::number("Moves them by this many beats (negative is earlier)."),
            "start": schema::beats("Moves them so the earliest starts here, in beats, keeping their layout."),
            "track": schema::id("Moves them to this track, which must be of their kind."),
            "length": schema::number("Each clip's new length in beats, from where it starts."),
            "name": schema::string("Each clip's new name."),
            "muted": schema::boolean("Mutes or unmutes them."),
            "gain_db": schema::number("Audio: the clip's gain, -60–12 dB."),
            "fade_in": schema::number("Audio: the fade in's length in seconds."),
            "fade_out": schema::number("Audio: the fade out's length in seconds."),
            "fade_in_curve": schema::number("Audio: the fade in's shape, -1–1 (0 straight, negative rises fast first)."),
            "fade_out_curve": schema::number("Audio: the fade out's shape, -1–1."),
            "reversed": schema::boolean("Audio: plays the clip backwards."),
            "looped": schema::boolean("Audio: repeats the clip's audio for as long as the clip is."),
            "stretch": schema::object(
                json!({
                    "mode": schema::choice(
                        &["resample", "stretch", "beats"],
                        "resample: varispeed, pitch follows speed; stretch: speed and pitch apart; beats: cut at transients, for drums."
                    ),
                    "speed": schema::number("How much faster than the original it plays, 0.125–8."),
                    "original_bpm": schema::number("The material's own tempo, for `sync`."),
                    "sync": schema::boolean("Follows the song's tempo (needs `original_bpm`)."),
                }),
                &[],
            ),
            "pitch": schema::number("Audio: semitones, -48–48, in the \"stretch\" mode."),
            "channels": schema::choice(&["stereo", "mono", "swap"], "Audio: as is, both sides summed, or left and right swapped."),
            "polarity": schema::boolean("Audio: true turns the waveform upside down."),
            "slip": schema::number("Audio: slides the audio within the clip by this many seconds (later if positive), the clip staying put."),
        }),
        &["clips"],
    )
}

/// What edit_clips does to an audio clip's settings, checked.
#[derive(Clone, Copy)]
struct AudioChange {
    reversed: Option<bool>,
    looped: Option<bool>,
    stretch: Option<StretchArgs>,
    pitch: Option<f64>,
    channels: Option<Channels>,
    polarity: Option<bool>,
}

impl AudioChange {
    fn apply(self, clip: &mut project::AudioClip) {
        if let Some(reversed) = self.reversed {
            clip.reversed = reversed;
        }
        if let Some(looped) = self.looped {
            clip.looped = looped;
        }
        if let Some(stretch) = self.stretch {
            if let Some(mode) = stretch.mode {
                clip.stretch.mode = match mode {
                    Mode::Resample => StretchMode::Resample,
                    Mode::Stretch => StretchMode::Stretch,
                    Mode::Beats => StretchMode::Beats,
                };
            }
            if let Some(speed) = stretch.speed {
                clip.stretch.speed = speed;
            }
            if let Some(bpm) = stretch.original_bpm {
                clip.stretch.original_bpm = Some(bpm);
            }
            if let Some(sync) = stretch.sync {
                clip.stretch.sync = sync;
            }
        }
        if let Some(pitch) = self.pitch {
            clip.stretch.transpose = pitch;
        }
        if let Some(channels) = self.channels {
            clip.channels = match channels {
                Channels::Stereo => ChannelMode::Stereo,
                Channels::Mono => ChannelMode::Mono,
                Channels::Swap => ChannelMode::Swap,
            };
        }
        if let Some(polarity) = self.polarity {
            clip.polarity_inverted = polarity;
        }
    }
}

/// Checks `value` is a finite number in `min..=max`.
fn in_range(name: &str, value: f64, min: f64, max: f64, unit: &str) -> Result<(), ToolError> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ToolError(format!(
            "{name} is {min}–{max}{unit}; {value} is out of range"
        )))
    }
}

fn edit_clips(v: &Value, project: &Project) -> Result<Plan, ToolError> {
    let a: EditClipsArgs = args(v)?;
    let found = find_clips(project, &a.clips)?;
    let audio_fields = a.audio_fields();
    if !audio_fields.is_empty()
        && let Some((_, midi)) = found.iter().find(|(_, c)| matches!(c, ClipRef::Midi(_)))
    {
        return Err(ToolError(format!(
            "clip {} is a MIDI clip; {} {} for audio clips only",
            midi.id(),
            audio_fields.join(", "),
            if audio_fields.len() == 1 { "is" } else { "are" },
        )));
    }
    if a.move_by.is_some() && a.start.is_some() {
        return Err("give `move_by` or `start`, not both".into());
    }
    if let Some(name) = &a.name
        && name.trim().is_empty()
    {
        return Err("a clip's name can't be empty".into());
    }

    // Where they go.
    let earliest = found
        .iter()
        .map(|(_, c)| c.start())
        .min()
        .expect("not empty");
    let shift = match (a.move_by, a.start) {
        (Some(by), None) => Some(beats_to_offset(by)?),
        (None, Some(start)) => Some(beats_to_tick(start)? - earliest),
        _ => None,
    };
    if let Some(shift) = shift
        && earliest + shift < Tick::ZERO
    {
        return Err(ToolError(format!(
            "that would move clip {} to before the song's start (beat {})",
            found
                .iter()
                .find(|(_, c)| c.start() == earliest)
                .expect("the earliest is one of them")
                .1
                .id(),
            tick_to_beats(earliest + shift)
        )));
    }
    if let Some(track) = a.track {
        let kind = &track_of(project, track)?.kind;
        for (_, clip) in &found {
            let fits = matches!(
                (kind, clip),
                (TrackKind::Midi { .. }, ClipRef::Midi(_))
                    | (TrackKind::Audio { .. }, ClipRef::Audio(_))
            );
            if !fits {
                return Err(ToolError(format!(
                    "clip {} can't go on track {track}: {}",
                    clip.id(),
                    super::explain(&EditError::WrongTrackKind(track), project)
                )));
            }
        }
    }
    let length = match a.length {
        Some(beats) => {
            let length = beats_to_offset(beats)?;
            if length <= Tick::ZERO {
                return Err(ToolError(format!(
                    "length {beats} is too short; a clip lasts more than 0 beats"
                )));
            }
            Some(length)
        }
        None => None,
    };

    // Audio settings.
    if let Some(gain) = a.gain_db {
        in_range(
            "gain_db",
            gain,
            f64::from(SILENCE_DB),
            MAX_CLIP_GAIN_DB,
            " dB",
        )?;
    }
    for (name, curve) in [
        ("fade_in_curve", a.fade_in_curve),
        ("fade_out_curve", a.fade_out_curve),
    ] {
        if let Some(curve) = curve {
            in_range(name, curve, -1.0, 1.0, "")?;
        }
    }
    for (name, seconds) in [("fade_in", a.fade_in), ("fade_out", a.fade_out)] {
        if let Some(seconds) = seconds
            && !(seconds.is_finite() && seconds >= 0.0)
        {
            return Err(ToolError(format!(
                "{name} is a length in seconds, 0 or more; not {seconds}"
            )));
        }
    }
    if let Some(stretch) = a.stretch {
        if let Some(speed) = stretch.speed {
            in_range("speed", speed, Stretch::MIN_SPEED, Stretch::MAX_SPEED, "")?;
        }
        if let Some(bpm) = stretch.original_bpm {
            in_range("original_bpm", bpm, Tempo::MIN_BPM, Tempo::MAX_BPM, " BPM")?;
        }
    }
    if let Some(pitch) = a.pitch {
        in_range("pitch", pitch, -48.0, 48.0, " semitones")?;
    }
    if let Some(slip) = a.slip
        && !slip.is_finite()
    {
        return Err(ToolError(format!(
            "slip is in seconds; {slip} isn't a number"
        )));
    }
    // Per clip: fades in the source's frames, pitch only where it applies.
    let mut fades = Vec::new();
    let mut slips = Vec::new();
    for (_, clip) in &found {
        let ClipRef::Audio(audio) = clip else {
            continue;
        };
        let rate = project
            .source(audio.source)
            .map_or(48_000, |s| s.sample_rate)
            .max(1);
        let to_frames = |seconds: f64| (seconds * f64::from(rate)).round() as u64;
        if a.fades() {
            let fade_in = a.fade_in.map_or(audio.fade_in, to_frames);
            let fade_out = a.fade_out.map_or(audio.fade_out, to_frames);
            let room = if audio.looped && !matches!(a.looped, Some(false)) {
                audio.play_len
            } else {
                audio.source_len
            };
            if fade_in.saturating_add(fade_out) > room {
                return Err(ToolError(format!(
                    "clip {}'s fades would be {:.3} s together; it is {:.3} s long",
                    audio.id,
                    (fade_in + fade_out) as f64 / f64::from(rate),
                    room as f64 / f64::from(rate),
                )));
            }
            fades.push((
                audio.id,
                fade_in,
                fade_out,
                a.fade_in_curve.map_or(audio.fade_in_curve, |c| c as f32),
                a.fade_out_curve.map_or(audio.fade_out_curve, |c| c as f32),
            ));
        }
        if a.pitch.is_some() {
            let mode = a
                .stretch
                .and_then(|s| s.mode)
                .map_or(audio.stretch.mode == StretchMode::Stretch, |m| {
                    matches!(m, Mode::Stretch)
                });
            if !mode {
                return Err(ToolError(format!(
                    "clip {} isn't in the \"stretch\" mode, where `pitch` applies (in \
                     \"resample\" the pitch follows the speed); give stretch: {{\"mode\": \
                     \"stretch\"}} too",
                    audio.id
                )));
            }
        }
        if let Some(sync) = a.stretch.and_then(|s| s.sync)
            && sync
            && a.stretch.and_then(|s| s.original_bpm).is_none()
            && audio.stretch.original_bpm.is_none()
        {
            return Err(ToolError(format!(
                "clip {} has no original tempo to sync from; give stretch.original_bpm too",
                audio.id
            )));
        }
        if let Some(slip) = a.slip {
            slips.push((audio.id, (slip * f64::from(rate)).round() as i64));
        }
    }

    let ids = a.clips.clone();
    let audio = a.audio_settings().then_some(AudioChange {
        reversed: a.reversed,
        looped: a.looped,
        stretch: a.stretch,
        pitch: a.pitch,
        channels: a.channels,
        polarity: a.polarity,
    });
    let (name, muted, gain, track) = (a.name.clone(), a.muted, a.gain_db, a.track);
    let mut changes = Vec::new();
    if shift.is_some() || track.is_some() {
        changes.push("moved");
    }
    if length.is_some() {
        changes.push("resized");
    }
    if name.is_some() {
        changes.push("renamed");
    }
    match muted {
        Some(true) => changes.push("muted"),
        Some(false) => changes.push("unmuted"),
        None => {}
    }
    if !audio_fields.is_empty() {
        changes.push("audio settings changed");
    }
    if changes.is_empty() {
        return Err("nothing to change: give what to change besides `clips`".into());
    }
    let summary = format!("{}: {}", clips_text(ids.len(), ids[0]), changes.join(", "));
    Ok(Plan {
        label: if ids.len() == 1 {
            "Edit Clip".to_owned()
        } else {
            "Edit Clips".to_owned()
        },
        apply: Box::new(move |p| {
            for &id in &ids {
                if let Some(name) = &name {
                    p.rename_clip(id, name)?;
                }
                if let Some(gain) = gain {
                    p.set_clip_gain(id, gain as f32)?;
                }
                if let Some(change) = audio {
                    p.edit_audio_clip(id, |clip| change.apply(clip))?;
                }
            }
            if let Some(muted) = muted {
                p.set_clips_muted(&ids, muted)?;
            }
            for (id, fade_in, fade_out, in_curve, out_curve) in fades {
                p.set_clip_fades(id, fade_in, fade_out, in_curve, out_curve)?;
            }
            for (id, frames) in slips {
                p.slip_clip(id, frames)?;
            }
            if let Some(length) = length {
                for &id in &ids {
                    let (_, clip) = p.clip(id).ok_or(EditError::NoSuchClip(id))?;
                    let start = clip.start();
                    p.set_clip_bounds(id, start, start + length)?;
                }
            }
            if shift.is_some() || track.is_some() {
                let mut moves = Vec::with_capacity(ids.len());
                for &id in &ids {
                    let (on, clip) = p.clip(id).ok_or(EditError::NoSuchClip(id))?;
                    moves.push(ClipMove {
                        clip: id,
                        track: track.unwrap_or(on),
                        start: clip.start() + shift.unwrap_or(Tick::ZERO),
                    });
                }
                p.move_clips(&moves)?;
            }
            Ok(json!({ "ids": ids.iter().map(|c| c.0).collect::<Vec<_>>(), "summary": summary }))
        }),
    })
}

// clip_ops

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ClipOp {
    Split,
    Delete,
    Duplicate,
    CopyTo,
    Chop,
    Consolidate,
    Normalize,
    Freeze,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ChopMode {
    Grid,
    Transients,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipOpsArgs {
    op: ClipOp,
    clips: Vec<ClipId>,
    at: Option<f64>,
    times: Option<i64>,
    track: Option<TrackId>,
    mode: Option<ChopMode>,
    sensitivity: Option<f64>,
    division: Option<i64>,
}

fn clip_ops_schema() -> Value {
    schema::object(
        json!({
            "op": schema::choice(
                &["split", "delete", "duplicate", "copy_to", "chop", "consolidate", "normalize", "freeze"],
                "What to do."
            ),
            "clips": schema::ids("The clips."),
            "at": schema::beats("split: where to cut; copy_to: where the copies start. Beats from the song's start."),
            "times": schema::integer("duplicate: how many copies, 1–64 (1 if left out)."),
            "track": schema::id("copy_to: the track the copies go on (clips from one track only)."),
            "mode": schema::choice(&["grid", "transients"], "chop: on the song's grid (the default) or at an audio clip's transients."),
            "sensitivity": schema::number("chop at transients: 0–1, higher finds more (0.5 if left out)."),
            "division": schema::integer("chop on the grid: pieces to a bar, 1–64 (4, quarter notes, if left out)."),
        }),
        &["op", "clips"],
    )
}

fn clip_ops(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: ClipOpsArgs = args(v)?;
    let project = host.project();
    let found = find_clips(project, &a.clips)?;
    // Which of the optional fields each op takes.
    let takes: &[&str] = match a.op {
        ClipOp::Split => &["at"],
        ClipOp::Duplicate => &["times"],
        ClipOp::CopyTo => &["at", "track"],
        ClipOp::Chop => match a.mode.unwrap_or(ChopMode::Grid) {
            ChopMode::Grid => &["mode", "division"],
            ChopMode::Transients => &["mode", "sensitivity"],
        },
        ClipOp::Delete | ClipOp::Consolidate | ClipOp::Normalize | ClipOp::Freeze => &[],
    };
    let given = [
        ("at", a.at.is_some()),
        ("times", a.times.is_some()),
        ("track", a.track.is_some()),
        ("mode", a.mode.is_some()),
        ("sensitivity", a.sensitivity.is_some()),
        ("division", a.division.is_some()),
    ];
    if let Some((stray, _)) = given
        .iter()
        .find(|(name, given)| *given && !takes.contains(name))
    {
        return Err(ToolError(format!(
            "`{stray}` isn't for this op{}",
            if takes.is_empty() {
                String::new()
            } else {
                format!(
                    " (it takes {})",
                    takes
                        .iter()
                        .map(|t| format!("`{t}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        )));
    }
    let ids = a.clips.clone();
    let first = ids[0];
    let plan = match a.op {
        ClipOp::Split => {
            let at = beats_to_tick(a.at.ok_or("split needs `at`, where to cut")?)?;
            if !found
                .iter()
                .any(|(_, c)| c.start() < at && project.clip_end(*c) > at)
            {
                return Err(ToolError(format!(
                    "no clip given spans beat {} (bar {}), so nothing would be cut",
                    tick_to_beats(at),
                    bar_beat(at, &project.tempo)
                )));
            }
            Plan {
                label: "Split".to_owned(),
                apply: made("split", move |p| p.split_clips(&ids, at)),
            }
        }
        ClipOp::Delete => {
            let n = ids.len();
            Plan {
                label: if n == 1 {
                    "Delete Clip"
                } else {
                    "Delete Clips"
                }
                .to_owned(),
                apply: Box::new(move |p| {
                    p.delete_clips(&ids)?;
                    Ok(json!({ "summary": format!("deleted {}", clips_text(n, first)) }))
                }),
            }
        }
        ClipOp::Duplicate => {
            let times = a.times.unwrap_or(1);
            if !(1..=i64::from(MAX_TIMES)).contains(&times) {
                return Err(ToolError(format!(
                    "times is 1–{MAX_TIMES}; {times} is out of range"
                )));
            }
            Plan {
                label: "Duplicate".to_owned(),
                apply: made("duplicated", move |p| {
                    let mut made = Vec::new();
                    let mut last = ids;
                    for _ in 0..times {
                        last = p.duplicate_clips(&last)?;
                        made.extend(&last);
                    }
                    Ok(made)
                }),
            }
        }
        ClipOp::CopyTo => {
            let at = beats_to_tick(a.at.ok_or("copy_to needs `at`, where the copies start")?)?;
            let onto = a.track;
            if let Some(track) = onto {
                if found.iter().any(|(t, _)| *t != found[0].0) {
                    return Err(
                        "`track` is for clips from one track; these are on several, and their \
                         copies go back on the tracks they came from without it"
                            .into(),
                    );
                }
                let kind = &track_of(project, track)?.kind;
                let fits = matches!(
                    (kind, &found[0].1),
                    (TrackKind::Midi { .. }, ClipRef::Midi(_))
                        | (TrackKind::Audio { .. }, ClipRef::Audio(_))
                );
                if !fits {
                    return Err(ToolError(super::explain(
                        &EditError::WrongTrackKind(track),
                        project,
                    )));
                }
            }
            Plan {
                label: "Copy Clips".to_owned(),
                apply: made("copied", move |p| {
                    let copied = p.copy_clips(&ids);
                    p.paste_clips(&copied, at, onto)
                }),
            }
        }
        ClipOp::Chop if a.mode == Some(ChopMode::Transients) => {
            let sensitivity = a.sensitivity.unwrap_or(0.5);
            in_range("sensitivity", sensitivity, 0.0, 1.0, "")?;
            let op = ClipProcess::ChopTransients {
                sensitivity: sensitivity as f32,
            };
            return process(host, op, &ids, "chopped");
        }
        ClipOp::Chop => {
            let division = a.division.unwrap_or(4);
            if !(1..=i64::from(MAX_DIVISION)).contains(&division) {
                return Err(ToolError(format!(
                    "division is 1–{MAX_DIVISION} pieces a bar; {division} is out of range"
                )));
            }
            let step = (project.tempo.bar_len().get() / division).max(1);
            let cuts: Vec<(ClipId, Vec<Tick>)> = found
                .iter()
                .map(|(_, c)| {
                    let (start, end) = (c.start().get(), project.clip_end(*c).get());
                    // The first grid line after the start, then every step.
                    let first = (start / step + 1) * step;
                    let at = (first..end).step_by(step as usize).map(Tick::new).collect();
                    (c.id(), at)
                })
                .collect();
            if cuts.iter().all(|(_, at)| at.is_empty()) {
                return Err(ToolError(format!(
                    "no grid line falls inside {}; a larger division cuts finer",
                    clips_text(ids.len(), first)
                )));
            }
            Plan {
                label: "Chop".to_owned(),
                apply: made("chopped into", move |p| {
                    let mut pieces = Vec::new();
                    for (id, at) in &cuts {
                        pieces.extend(p.chop_clip(*id, at)?);
                    }
                    Ok(pieces)
                }),
            }
        }
        ClipOp::Consolidate => return process(host, ClipProcess::Consolidate, &ids, "made"),
        ClipOp::Normalize => return process(host, ClipProcess::Normalize, &ids, "normalized"),
        ClipOp::Freeze => return process(host, ClipProcess::Freeze, &ids, "froze into"),
    };
    apply(host, plan)
}

/// An edit that makes clips, answering with their ids: "split: 2 clips".
fn made(
    verb: &'static str,
    edit: impl FnOnce(&mut Project) -> Result<Vec<ClipId>, EditError> + 'static,
) -> Apply<'static> {
    Box::new(move |p| {
        let ids = edit(p)?;
        if ids.is_empty() {
            return Err(invalid("that made no clips, so nothing was changed"));
        }
        Ok(json!({
            "ids": ids.iter().map(|c| c.0).collect::<Vec<_>>(),
            "summary": format!("{verb} {}", clips_text(ids.len(), ids[0])),
        }))
    })
}

/// A clip operation the host does, needing the audio.
fn process(
    host: &mut dyn Host,
    op: ClipProcess,
    clips: &[ClipId],
    verb: &str,
) -> Result<Outcome, ToolError> {
    let ids = host.process_clips(op, clips)?;
    let summary = match ids.first() {
        Some(&first) => format!("{verb} {}", clips_text(ids.len(), first)),
        None => format!("{verb} nothing"),
    };
    Ok(Outcome::Done(Output::json(json!({
        "ids": ids.iter().map(|c| c.0).collect::<Vec<_>>(),
        "summary": summary,
    }))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use crate::tools::call;
    use midi::PPQ;
    use project::{NewTrack, TrackPosition};

    /// The JSON a tool answered at once; panics on an error.
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

    fn ids(v: &Value) -> Vec<ClipId> {
        v["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| ClipId(id.as_u64().unwrap() as u32))
            .collect()
    }

    const BAR: i64 = 4 * PPQ;

    /// A MIDI track and an audio track with a one-second clip of a 48 kHz
    /// file.
    fn rig() -> (FakeHost, TrackId, TrackId, ClipId) {
        let mut project = Project::new();
        let midi = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let audio = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/hit.wav".into(), 48_000, 48_000, 2);
        let clip = project.add_audio_clip(audio, Tick::ZERO, source).unwrap();
        (FakeHost::new(project), midi, audio, clip)
    }

    fn start(host: &FakeHost, clip: ClipId) -> Tick {
        host.project.clip(clip).unwrap().1.start()
    }

    #[test]
    fn a_midi_clip_with_notes_in_one_step() {
        let (mut host, midi, _, _) = rig();
        let steps = host.history.position();
        let added = done(
            "add_clip",
            json!({
                "track": midi.0, "start": 8, "kind": "midi", "name": "Hook",
                "notes": [
                    { "start": 0, "length": 0.5, "pitch": "C3", "velocity": 90 },
                    { "start": 4.5, "length": 1, "pitch": 64, "bend": [{ "at": 0.5, "semitones": 2 }] },
                ],
            }),
            &mut host,
        );
        assert_eq!(host.history.position(), steps + 1, "one undo step");
        assert_eq!(host.history.undo_label(), Some("LLM: Add MIDI Clip"));
        let clip = ClipId(added["id"].as_u64().unwrap() as u32);
        let (track, ClipRef::Midi(m)) = host.project.clip(clip).unwrap() else {
            panic!("a MIDI clip");
        };
        assert_eq!(track, midi);
        assert_eq!(m.name, "Hook");
        assert_eq!(m.start, Tick::new(8 * PPQ));
        // Notes reach into the second bar: two whole bars.
        assert_eq!(m.length, Tick::new(2 * BAR));
        assert_eq!(added["length"], json!(8.0));
        assert_eq!(m.notes.len(), 2);
        assert_eq!(m.notes[0].pitch.get(), 60);
        assert_eq!(m.notes[0].velocity.to_midi1(), 90);
        assert_eq!(m.notes[1].start, Tick::new(4 * PPQ + PPQ / 2));
        assert_eq!(m.notes[1].velocity.to_midi1(), 100, "the default");
        assert_eq!(m.notes[1].bend[0].semitones, 2.0);
        assert_ne!(m.notes[0].id, m.notes[1].id);
        assert!(added["summary"].as_str().unwrap().contains("2 notes"));

        // A wrong track or pitch changes nothing.
        let before = host.project.clone();
        let error = failure(
            "add_clip",
            json!({ "track": midi.0, "start": 0, "kind": "midi", "notes": [{ "start": 0, "length": 1, "pitch": "H3" }] }),
            &mut host,
        );
        assert!(error.contains("note 0") && error.contains("H3"), "{error}");
        assert!(host.project == before);
    }

    #[test]
    fn midi_clips_go_on_midi_tracks() {
        let (mut host, _, audio, _) = rig();
        let error = failure(
            "add_clip",
            json!({ "track": audio.0, "start": 0, "kind": "midi", "length": 4 }),
            &mut host,
        );
        assert!(error.contains("MIDI clips need a MIDI track"), "{error}");
        let error = failure(
            "add_clip",
            json!({ "track": audio.0, "start": 0, "kind": "audio", "file": "/tmp/x.wav", "length": 4 }),
            &mut host,
        );
        assert!(error.contains("length"), "{error}");
    }

    #[test]
    fn an_audio_file_waits_for_its_import() {
        let (mut host, _, audio, clip) = rig();
        let dir = std::env::temp_dir().join(format!("canopy-mcp-clips-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hit.wav");
        std::fs::write(&file, b"RIFF").unwrap();
        let ticket = host.next_ticket();
        let Ok(Outcome::Waiting(mut waiter)) = call(
            "add_clip",
            &json!({ "track": audio.0, "start": 4, "kind": "audio", "file": file }),
            &mut host,
        ) else {
            panic!("an import waits");
        };
        assert_eq!(
            host.imports,
            [(file.clone(), Some(audio), Tick::new(4 * PPQ))]
        );
        assert!(waiter(&mut host).is_none());
        // The rig's clip stands for the one the import would add.
        host.script(ticket, [Poll::Imported(Ok(clip))]);
        let answer = waiter(&mut host).expect("answered").unwrap();
        assert_eq!(answer.json["id"], json!(clip.0));
        assert_eq!(answer.json["track"], json!(audio.0));

        let relative = failure(
            "add_clip",
            json!({ "start": 0, "kind": "audio", "file": "hit.wav" }),
            &mut host,
        );
        assert!(relative.contains("full path"), "{relative}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_fields_on_a_midi_clip_are_an_error() {
        let (mut host, midi, _, _) = rig();
        let clip = host
            .project
            .add_midi_clip(midi, Tick::ZERO, Tick::new(BAR))
            .unwrap();
        let before = host.project.clone();
        let error = failure(
            "edit_clips",
            json!({ "clips": [clip.0], "gain_db": -3, "reversed": true, "name": "x" }),
            &mut host,
        );
        assert!(error.contains("MIDI clip"), "{error}");
        assert!(
            error.contains("gain_db") && error.contains("reversed"),
            "{error}"
        );
        assert!(host.project == before);
    }

    #[test]
    fn fades_are_in_seconds() {
        let (mut host, _, _, clip) = rig();
        done(
            "edit_clips",
            json!({ "clips": [clip.0], "fade_in": 0.01, "fade_out": 0.25, "fade_out_curve": 0.5, "gain_db": -3 }),
            &mut host,
        );
        let Some((_, ClipRef::Audio(audio))) = host.project.clip(clip) else {
            panic!("an audio clip");
        };
        // At the file's own 48 kHz.
        assert_eq!(audio.fade_in, 480);
        assert_eq!(audio.fade_out, 12_000);
        assert_eq!(audio.fade_out_curve, 0.5);
        assert_eq!(audio.gain_db, -3.0);
        // Longer than the clip is an error, not a shorter fade.
        let error = failure(
            "edit_clips",
            json!({ "clips": [clip.0], "fade_in": 0.8, "fade_out": 0.8 }),
            &mut host,
        );
        assert!(error.contains("1.000 s long"), "{error}");
        let error = failure(
            "edit_clips",
            json!({ "clips": [clip.0], "gain_db": 20 }),
            &mut host,
        );
        assert!(error.contains("-60–12"), "{error}");
    }

    #[test]
    fn clips_move_together_and_change_their_audio() {
        let (mut host, midi, audio, clip) = rig();
        let a = host
            .project
            .add_midi_clip(midi, Tick::new(BAR), Tick::new(BAR))
            .unwrap();
        let b = host
            .project
            .add_midi_clip(midi, Tick::new(3 * BAR), Tick::new(BAR))
            .unwrap();
        done(
            "edit_clips",
            json!({ "clips": [a.0, b.0], "start": 16, "muted": true }),
            &mut host,
        );
        assert_eq!(start(&host, a), Tick::new(4 * BAR));
        assert_eq!(start(&host, b), Tick::new(6 * BAR), "the layout is kept");
        assert!(host.project.clip(b).unwrap().1.muted());
        let error = failure(
            "edit_clips",
            json!({ "clips": [a.0], "move_by": -100 }),
            &mut host,
        );
        assert!(error.contains("before the song's start"), "{error}");
        let error = failure(
            "edit_clips",
            json!({ "clips": [a.0], "track": audio.0 }),
            &mut host,
        );
        assert!(error.contains("can't go on track"), "{error}");

        let error = failure(
            "edit_clips",
            json!({ "clips": [clip.0], "pitch": 3 }),
            &mut host,
        );
        assert!(error.contains("\"stretch\" mode"), "{error}");
        done(
            "edit_clips",
            json!({
                "clips": [clip.0], "stretch": { "mode": "stretch", "speed": 2 }, "pitch": -2,
                "channels": "mono", "polarity": true, "reversed": true,
            }),
            &mut host,
        );
        let Some((_, ClipRef::Audio(c))) = host.project.clip(clip) else {
            panic!("an audio clip");
        };
        assert_eq!(c.stretch.mode, StretchMode::Stretch);
        assert_eq!(c.stretch.speed, 2.0);
        assert_eq!(c.stretch.transpose, -2.0);
        assert_eq!(c.channels, ChannelMode::Mono);
        assert!(c.polarity_inverted && c.reversed);
        let error = failure(
            "edit_clips",
            json!({ "clips": [clip.0], "stretch": { "speed": 10 } }),
            &mut host,
        );
        assert!(error.contains("0.125–8"), "{error}");
    }

    #[test]
    fn duplicate_three_times_lays_copies_end_to_end() {
        let (mut host, midi, _, _) = rig();
        let clip = host
            .project
            .add_midi_clip(midi, Tick::new(BAR), Tick::new(BAR))
            .unwrap();
        let steps = host.history.position();
        let made = done(
            "clip_ops",
            json!({ "op": "duplicate", "clips": [clip.0], "times": 3 }),
            &mut host,
        );
        assert_eq!(host.history.position(), steps + 1, "one undo step");
        let copies = ids(&made);
        assert_eq!(copies.len(), 3);
        let starts: Vec<Tick> = copies.iter().map(|&c| start(&host, c)).collect();
        assert_eq!(
            starts,
            [Tick::new(2 * BAR), Tick::new(3 * BAR), Tick::new(4 * BAR)]
        );
        let error = failure(
            "clip_ops",
            json!({ "op": "duplicate", "clips": [clip.0], "times": 0 }),
            &mut host,
        );
        assert!(error.contains("1–64"), "{error}");
        let error = failure(
            "clip_ops",
            json!({ "op": "delete", "clips": [clip.0], "times": 2 }),
            &mut host,
        );
        assert!(error.contains("`times` isn't for this op"), "{error}");
    }

    #[test]
    fn split_returns_the_new_clips() {
        let (mut host, midi, _, _) = rig();
        let clip = host
            .project
            .add_midi_clip(midi, Tick::ZERO, Tick::new(2 * BAR))
            .unwrap();
        let made = done(
            "clip_ops",
            json!({ "op": "split", "clips": [clip.0], "at": 6 }),
            &mut host,
        );
        let right = ids(&made);
        assert_eq!(right.len(), 1);
        assert_eq!(start(&host, right[0]), Tick::new(6 * PPQ));
        let (_, left) = host.project.clip(clip).unwrap();
        assert_eq!(host.project.clip_end(left), Tick::new(6 * PPQ));
        let error = failure(
            "clip_ops",
            json!({ "op": "split", "clips": [clip.0], "at": 100 }),
            &mut host,
        );
        assert!(error.contains("spans"), "{error}");
    }

    #[test]
    fn chop_to_a_grid() {
        let (mut host, midi, _, _) = rig();
        let clip = host
            .project
            .add_midi_clip(midi, Tick::new(BAR), Tick::new(BAR))
            .unwrap();
        let made = done(
            "clip_ops",
            json!({ "op": "chop", "clips": [clip.0], "division": 4 }),
            &mut host,
        );
        let pieces = ids(&made);
        assert_eq!(pieces.len(), 4);
        assert_eq!(pieces[0], clip);
        let starts: Vec<i64> = pieces.iter().map(|&c| start(&host, c).get()).collect();
        assert_eq!(starts, [BAR, BAR + PPQ, BAR + 2 * PPQ, BAR + 3 * PPQ]);
    }

    #[test]
    fn copies_go_where_asked_and_processing_goes_to_the_host() {
        let (mut host, midi, audio, clip) = rig();
        let made = done(
            "clip_ops",
            json!({ "op": "copy_to", "clips": [clip.0], "at": 32 }),
            &mut host,
        );
        let copy = ids(&made)[0];
        assert_eq!(host.project.clip(copy).unwrap().0, audio);
        assert_eq!(start(&host, copy), Tick::new(32 * PPQ));
        let error = failure(
            "clip_ops",
            json!({ "op": "copy_to", "clips": [clip.0], "at": 32, "track": midi.0 }),
            &mut host,
        );
        assert!(error.contains("audio clips need an audio track"), "{error}");

        done(
            "clip_ops",
            json!({ "op": "chop", "clips": [clip.0], "mode": "transients", "sensitivity": 0.7 }),
            &mut host,
        );
        done(
            "clip_ops",
            json!({ "op": "normalize", "clips": [clip.0] }),
            &mut host,
        );
        assert_eq!(
            host.processed,
            [
                (ClipProcess::ChopTransients { sensitivity: 0.7 }, vec![clip]),
                (ClipProcess::Normalize, vec![clip]),
            ]
        );
        let error = failure(
            "clip_ops",
            json!({ "op": "chop", "clips": [clip.0], "mode": "transients", "division": 4 }),
            &mut host,
        );
        assert!(error.contains("`division` isn't for this op"), "{error}");
    }
}
