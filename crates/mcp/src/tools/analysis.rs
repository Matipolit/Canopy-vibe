//! Tools that measure and look at the sound: `analyze` (spectrum,
//! loudness, dynamics, stereo, transients, sibilance, pitch, key),
//! `find_clashes`, `plot` (a spectrum or spectrogram image) and
//! `render_audio` (a WAV file).
//!
//! Each asks the host for a render ([`Host::render`]) and answers once it
//! has come back. The measuring then runs on a thread of its own, so a
//! long render's analysis doesn't hold up the app's frames, and inside
//! `catch_unwind`: a bug in a measure is a tool error, never a crash. A
//! render of a project that changed while it rendered is answered with
//! `stale: true`.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use analysis::Signal;
use analysis::images::{self, Marker, Plot};
use media::AudioBuffer;
use midi::Tick;
use project::{Project, TempoMap, TrackId, TrackKind};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Kind, MAX_TRACKS, Outcome, Output, Tool, ToolError, Waiter, args, schema};
use crate::host::{Host, Poll, RenderRequest, RenderTarget, Renders, Variant};
use crate::units::{bar_beat, beats_to_tick, tick_to_beats};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "analyze",
        description: "Measures a track's, a clip's or the master's sound over a range: \
                      `spectrum` (third-octave bands, resonant peaks, centroid, roll-off, tilt; \
                      eight bands per bar with `bands_per_bar`), `loudness` (LUFS, LRA, true \
                      peak, RMS, crest), `dynamics` (per bar), `stereo` (correlation, width, mono \
                      loss), `transients` (onsets, tempo, timing against the grid), `sibilance` \
                      (ess events and their band), `pitch` (a single voice's notes) and `key`. \
                      A track renders alone (soloed, after its fader, without the master); a \
                      clip renders its track over the clip's span. `variant` \"pre_fader\" puts \
                      its fader at unity, \"dry\" bypasses its effects. The answer comes once \
                      the render is done; measuring is not listening, so measure again after \
                      an edit.",
        schema: analyze_schema,
        examples: &[
            r#"{"target": {"track": 12}, "measures": ["spectrum", "sibilance"]}"#,
            r#"{"target": {"master": true}, "range": {"from": 32, "to": 64}, "measures": ["loudness", "dynamics", "stereo"]}"#,
            r#"{"target": {"clip": 40}, "measures": ["transients", "pitch", "key"], "variant": "dry"}"#,
        ],
        kind: Kind::Pending(analyze),
    },
    Tool {
        name: "find_clashes",
        description: "Which tracks fight for the same frequencies: every pair that is within \
                      6 dB of each other in a band (24 Bark bands, 100 ms windows) while both \
                      are within 40 dB of the mix there, so neither masks the other. The worst \
                      `top` pairs (5 by default) with their bands, share of the time, worst \
                      bars and the louder track. `tracks` defaults to every track that has \
                      clips; they render together as the song plays (muted ones are silent). \
                      It reports; what to do about it (EQ, ducking, panning, arranging) is \
                      yours.",
        schema: clashes_schema,
        examples: &[
            r#"{}"#,
            r#"{"tracks": [12, 14, 18], "range": {"from": 64, "to": 96}, "top": 3}"#,
        ],
        kind: Kind::Pending(find_clashes),
    },
    Tool {
        name: "plot",
        description: "Draws the sound as a PNG image, with JSON giving its axes. `spectrum`: \
                      the average spectrum on a log frequency axis, several targets overlaid \
                      in their own colors (up to 6), the strongest peaks marked. `spectrogram`: \
                      one target's level over time (in bars) and frequency, with `markers`: \
                      \"sibilance\", \"transients\", \"loop\", \"selection\", or spans and \
                      moments in beats ({\"from\", \"to\", \"label\"}, {\"at\", \"label\"}). \
                      `width` 320–1600 by `height` 200–1000 pixels (1024×512 by default; an \
                      image costs about width×height/750 tokens). `freq_range` [low, high] in \
                      Hz, `db_range` [floor, ceiling].",
        schema: plot_schema,
        examples: &[
            r#"{"kind": "spectrum", "targets": [{"track": 12}, {"track": 14}]}"#,
            r#"{"kind": "spectrogram", "targets": [{"track": 12}], "range": {"from": 16, "to": 32}, "markers": ["sibilance"], "freq_range": [2000, 16000]}"#,
            r#"{"kind": "spectrum", "targets": [{"master": true}], "width": 800, "height": 400, "db_range": [-80, -10]}"#,
        ],
        kind: Kind::Pending(plot),
    },
    Tool {
        name: "render_audio",
        description: "Renders a track, a clip or the master to a WAV file (32-bit float, at \
                      the session's rate) at `path`, a full path that doesn't exist yet: an \
                      existing file is never overwritten. Renders as `analyze` does.",
        schema: render_schema,
        examples: &[
            r#"{"target": {"track": 12}, "path": "/home/me/renders/vocal.wav"}"#,
            r#"{"target": {"master": true}, "range": {"from": 0, "to": 32}, "path": "/home/me/renders/intro.wav"}"#,
        ],
        kind: Kind::Pending(render_audio),
    },
];

/// Pairs `find_clashes` reports by default, and at most.
const DEFAULT_TOP: usize = 5;
const MAX_TOP: usize = 50;
/// Targets one spectrum may overlay: one color each.
const MAX_CURVES: usize = 6;
/// Peaks marked on each curve of a spectrum.
const PEAKS: usize = 3;
/// The spectrogram's transform: about 43 ms at 48 kHz, fine enough in time
/// for an ess and in frequency for a bass line's octave.
const SPECTROGRAM_FFT: usize = 2048;

/// What to render: a track (a group with everything in it), a clip (its
/// track over the clip's span), or the master.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    track: Option<TrackId>,
    clip: Option<project::ClipId>,
    master: Option<bool>,
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum VariantArg {
    #[default]
    Mix,
    PreFader,
    Dry,
}

impl VariantArg {
    fn variant(self) -> Variant {
        match self {
            VariantArg::Mix => Variant::Mix,
            VariantArg::PreFader => Variant::PreFader,
            VariantArg::Dry => Variant::Dry,
        }
    }

    fn name(self) -> &'static str {
        match self {
            VariantArg::Mix => "mix",
            VariantArg::PreFader => "pre_fader",
            VariantArg::Dry => "dry",
        }
    }
}

/// A target, checked against the project: what to render, what to call
/// it, and the span a clip covers.
#[derive(Clone)]
struct Target {
    render: RenderTarget,
    /// "Vocal", "Master", "clip 40 on Vocal".
    name: String,
    json: Value,
    span: Option<(Tick, Tick)>,
}

fn target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": { "track": { "type": "integer", "minimum": 1 } },
                "required": ["track"],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": { "clip": { "type": "integer", "minimum": 1 } },
                "required": ["clip"],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": { "master": { "type": "boolean", "enum": [true] } },
                "required": ["master"],
                "additionalProperties": false,
            },
        ],
        "description": "{\"track\": id}, {\"clip\": id} or {\"master\": true}.",
    })
}

fn range_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "from": { "type": "number", "minimum": 0 },
                    "to": { "type": "number", "minimum": 0 },
                },
                "required": ["from", "to"],
                "additionalProperties": false,
            },
            { "type": "string", "enum": ["selection", "loop"] },
        ],
        "description": "{\"from\", \"to\"} in beats, \"selection\" (the selected clips' span) \
                        or \"loop\". The whole song (or a clip target's span) if left out; at \
                        most 10 minutes.",
    })
}

fn variant_schema() -> Value {
    schema::choice(
        &["mix", "pre_fader", "dry"],
        "mix (the default): after the effects and fader; pre_fader: the fader at unity; \
         dry: the effects bypassed.",
    )
}

/// `args` as a target of `project`.
fn target(project: &Project, args: &TargetArgs) -> Result<Target, ToolError> {
    match (args.track, args.clip, args.master) {
        (Some(id), None, None) => {
            let track = super::clips::track_of(project, id)?;
            Ok(Target {
                render: RenderTarget::Track(id),
                name: track.name.clone(),
                json: json!({ "track": id.0, "name": track.name }),
                span: None,
            })
        }
        (None, Some(id), None) => {
            let (track, clip) = project.clip(id).ok_or_else(|| {
                ToolError(super::explain(&project::EditError::NoSuchClip(id), project))
            })?;
            let name = project.track(track).map_or("", |t| t.name.as_str());
            Ok(Target {
                render: RenderTarget::Track(track),
                name: format!("clip {id} on {name}"),
                json: json!({ "clip": id.0, "track": track.0, "name": name }),
                span: Some((clip.start(), project.clip_end(clip))),
            })
        }
        (None, None, Some(true)) => Ok(Target {
            render: RenderTarget::Master,
            name: "Master".to_owned(),
            json: json!({ "master": true }),
            span: None,
        }),
        (None, None, Some(false)) => Err("a target of {\"master\": false} names nothing; \
             give {\"track\": id}, {\"clip\": id} or {\"master\": true}"
            .into()),
        _ => Err("a target is one of {\"track\": id}, {\"clip\": id} or {\"master\": true}".into()),
    }
}

/// The range to render, in ticks: `range` as given, else the span of the
/// clip targets, else the whole song (`None`).
fn range(
    host: &dyn Host,
    range: Option<&Value>,
    targets: &[Target],
) -> Result<Option<(Tick, Tick)>, ToolError> {
    let project = host.project();
    let Some(range) = range else {
        let spans = targets.iter().filter_map(|t| t.span);
        return Ok(spans.reduce(|a, b| (a.0.min(b.0), a.1.max(b.1))));
    };
    let (from, to) = match range {
        Value::String(name) if name == "loop" => {
            let region = project
                .loop_region
                .ok_or("there's no loop; pass `range` as {\"from\", \"to\"} in beats")?;
            (region.start, region.end)
        }
        Value::String(name) if name == "selection" => {
            let selected = host.selection().clips;
            let spans = selected.iter().filter_map(|&id| {
                let (_, clip) = project.clip(id)?;
                Some((clip.start(), project.clip_end(clip)))
            });
            spans
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
                .ok_or("no clips are selected; pass `range` as {\"from\", \"to\"} in beats")?
        }
        Value::Object(map) => {
            let beats = |key: &str| -> Result<Tick, ToolError> {
                let value = map.get(key).and_then(Value::as_f64).ok_or_else(|| {
                    ToolError(format!("argument `range.{key}`: a time in beats is needed"))
                })?;
                Ok(beats_to_tick(value)?)
            };
            if let Some(other) = map.keys().find(|k| *k != "from" && *k != "to") {
                return Err(ToolError(format!(
                    "argument `range`: unknown field `{other}`, expected `from` and `to`"
                )));
            }
            (beats("from")?, beats("to")?)
        }
        _ => {
            return Err(
                "argument `range`: {\"from\", \"to\"} in beats, \"selection\" or \"loop\"".into(),
            );
        }
    };
    if to <= from {
        return Err(ToolError(format!(
            "the range is empty: it ends ({}) where or before it starts ({})",
            tick_to_beats(to),
            tick_to_beats(from)
        )));
    }
    Ok(Some((from, to)))
}

/// Asks for `targets` to be rendered, then runs `then` on the renders on a
/// thread of its own; the waiter answers with what it returns.
fn render_then(
    host: &mut dyn Host,
    request: RenderRequest,
    then: impl FnOnce(Arc<Renders>) -> Result<Output, ToolError> + Send + 'static,
) -> Result<Outcome, ToolError> {
    let ticket = host.render(request)?;
    let mut then = Some(then);
    let mut working: Option<Receiver<Result<Output, ToolError>>> = None;
    let waiter: Waiter = Box::new(move |host| {
        if let Some(answer) = &working {
            return match answer.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("canopy hit a bug measuring this".into()))
                }
            };
        }
        match host.poll(ticket) {
            Poll::Waiting { .. } => None,
            Poll::Rendered(Ok(renders)) => {
                let then = then.take()?;
                working = Some(off_thread(move || then(renders)));
                None
            }
            Poll::Rendered(Err(error)) => Some(Err(ToolError(error))),
            Poll::Imported(_) => Some(Err("canopy answered the render with an import".into())),
        }
    });
    Ok(Outcome::Waiting(waiter))
}

/// Runs `work` on a thread of its own, a panic becoming an error.
fn off_thread(
    work: impl FnOnce() -> Result<Output, ToolError> + Send + 'static,
) -> Receiver<Result<Output, ToolError>> {
    let (send, answer) = mpsc::channel();
    let guarded = move || {
        panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".to_owned());
            Err(ToolError(format!(
                "canopy hit a bug measuring this ({why}); nothing was changed"
            )))
        })
    };
    let spawned = thread::Builder::new().name("mcp analysis".into()).spawn({
        let send = send.clone();
        move || {
            let _ = send.send(guarded());
        }
    });
    if let Err(error) = spawned {
        let _ = send.send(Err(ToolError(format!(
            "canopy couldn't start measuring: {error}"
        ))));
    }
    answer
}

/// What every answer about a render says: where it is in the song, and
/// whether the project has changed since.
fn about(renders: &Renders, frames: usize, rate: u32, tempo: &TempoMap) -> Map<String, Value> {
    let signal_end = |frames: usize| {
        let from = tick_to_beats(renders.start);
        let ticks = tempo.frames_to_ticks(frames as f64, rate);
        from + ticks / midi::PPQ as f64
    };
    let from = tick_to_beats(renders.start);
    let to = signal_end(frames);
    let mut about = Map::new();
    about.insert(
        "range".into(),
        json!({
            "from": round3(from),
            "to": round3(to),
            "from_bar": bar_beat(renders.start, tempo),
            "to_bar": analysis::report::bar_beat(to, tempo),
            "seconds": round3(frames as f64 / f64::from(rate.max(1))),
        }),
    );
    if renders.stale {
        about.insert("stale".into(), json!(true));
        about.insert(
            "note".into(),
            json!(
                "the project changed while this rendered: it describes the version before \
                 the change; ask again for the current one"
            ),
        );
    }
    about
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The rendered audio of `target`.
fn audio_of(renders: &Renders, target: RenderTarget) -> Result<&Arc<AudioBuffer>, ToolError> {
    renders
        .audio
        .iter()
        .find(|(t, _)| *t == target)
        .map(|(_, audio)| audio)
        .ok_or_else(|| "canopy rendered something other than what was asked".into())
}

fn signal<'a>(audio: &'a AudioBuffer, start: Tick, tempo: &'a TempoMap) -> Signal<'a> {
    Signal::new(
        audio.left(),
        audio.right(),
        audio.sample_rate(),
        tick_to_beats(start),
        tempo,
    )
}

/// "…" with " (stale)" when the render is of an older version.
fn summarize(sentence: String, renders: &Renders) -> Value {
    if renders.stale {
        json!(format!(
            "{sentence} (Measured on the project as it was before its latest change.)"
        ))
    } else {
        json!(sentence)
    }
}

// analyze

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnalyzeArgs {
    target: TargetArgs,
    range: Option<Value>,
    #[serde(default)]
    variant: VariantArg,
    measures: Vec<String>,
    #[serde(default)]
    bands_per_bar: bool,
}

fn analyze_schema() -> Value {
    schema::object(
        json!({
            "target": target_schema(),
            "range": range_schema(),
            "variant": variant_schema(),
            "measures": schema::array(
                json!({ "type": "string", "enum": analysis::MEASURES }),
                "What to measure, at least one.",
            ),
            "bands_per_bar": schema::boolean(
                "With spectrum: eight bands' level for every bar too.",
            ),
        }),
        &["target", "measures"],
    )
}

fn analyze(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: AnalyzeArgs = args(v)?;
    if a.measures.is_empty() {
        return Err(ToolError(format!(
            "`measures` is empty; name at least one of {}",
            analysis::MEASURES.join(", ")
        )));
    }
    if let Some(unknown) = a
        .measures
        .iter()
        .find(|m| !analysis::MEASURES.contains(&m.as_str()))
    {
        return Err(ToolError(format!(
            "there's no measure called {unknown:?}; the measures are {}",
            analysis::MEASURES.join(", ")
        )));
    }
    let target = target(host.project(), &a.target)?;
    let range = range(host, a.range.as_ref(), std::slice::from_ref(&target))?;
    let tempo = host.project().tempo;
    let options = analysis::Options {
        bands_per_bar: a.bands_per_bar,
    };
    let request = RenderRequest {
        targets: vec![target.render],
        range,
        variant: a.variant.variant(),
    };
    let (variant, measures) = (a.variant, a.measures);
    render_then(host, request, move |renders| {
        let audio = audio_of(&renders, target.render)?;
        let signal = signal(audio, renders.start, &tempo);
        let mut out = about(&renders, audio.frames(), audio.sample_rate(), &tempo);
        let mut results = Map::new();
        let mut sentences = Vec::new();
        for name in &measures {
            let value = analysis::measure(name, &signal, &options)?;
            if let Some(sentence) = value["summary"].as_str() {
                sentences.push(sentence.to_owned());
            }
            results.insert(name.clone(), value);
        }
        out.insert("target".into(), target.json.clone());
        out.insert("variant".into(), json!(variant.name()));
        out.insert("measures".into(), Value::Object(results));
        let range = &out["range"];
        let sentence = format!(
            "{}, {}–{}: {}",
            target.name,
            range["from_bar"].as_str().unwrap_or("?"),
            range["to_bar"].as_str().unwrap_or("?"),
            sentences.join(" ")
        );
        out.insert("summary".into(), summarize(sentence, &renders));
        Ok(Output::json(Value::Object(out)))
    })
}

// find_clashes

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClashesArgs {
    tracks: Option<Vec<TrackId>>,
    range: Option<Value>,
    top: Option<usize>,
}

fn clashes_schema() -> Value {
    schema::object(
        json!({
            "tracks": schema::ids(
                "The tracks to compare (2–64; not a group and a track inside it). Every \
                 track with clips if left out.",
            ),
            "range": range_schema(),
            "top": schema::count("How many pairs to report, worst first: 1–50, 5 by default."),
        }),
        &[],
    )
}

fn find_clashes(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: ClashesArgs = args(v)?;
    let top = a.top.unwrap_or(DEFAULT_TOP);
    if !(1..=MAX_TOP).contains(&top) {
        return Err(ToolError(format!(
            "`top` is 1–{MAX_TOP}; {top} is out of range"
        )));
    }
    let project = host.project();
    let tracks = match a.tracks {
        Some(tracks) => {
            for &id in &tracks {
                super::clips::track_of(project, id)?;
            }
            tracks
        }
        None => {
            let mut sounding = Vec::new();
            project.visit(|track, _| {
                let has_clips = match &track.kind {
                    TrackKind::Audio { clips } => !clips.is_empty(),
                    TrackKind::Midi { clips, .. } => !clips.is_empty(),
                    TrackKind::Group { .. } => false,
                };
                if has_clips {
                    sounding.push(track.id);
                }
            });
            sounding
        }
    };
    let mut unique = Vec::new();
    for id in tracks {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    let tracks = unique;
    if tracks.len() < 2 {
        return Err(ToolError(format!(
            "clashes need two tracks or more; {} — name them in `tracks`",
            match tracks.len() {
                0 => "no track has clips".to_owned(),
                _ => "only one track has clips".to_owned(),
            }
        )));
    }
    if tracks.len() > MAX_TRACKS {
        return Err(ToolError(format!(
            "at most {MAX_TRACKS} tracks at once; {} were given",
            tracks.len()
        )));
    }
    // A group's sound has its tracks' in it: they'd clash with themselves.
    for &id in &tracks {
        let mut parent = project.parent(id);
        while let Some(group) = parent {
            if tracks.contains(&group) {
                return Err(ToolError(format!(
                    "track {id} is inside group {group}, which is also listed; name one or \
                     the other"
                )));
            }
            parent = project.parent(group);
        }
    }
    let named: Vec<(String, u32)> = tracks
        .iter()
        .map(|&id| {
            let name = project
                .track(id)
                .map_or_else(String::new, |t| t.name.clone());
            (name, id.0)
        })
        .collect();
    let range = range(host, a.range.as_ref(), &[])?;
    let tempo = host.project().tempo;
    let mut targets: Vec<RenderTarget> = tracks.iter().map(|&t| RenderTarget::Track(t)).collect();
    targets.push(RenderTarget::Master);
    let request = RenderRequest {
        targets,
        range,
        variant: Variant::Mix,
    };
    render_then(host, request, move |renders| {
        let mix = audio_of(&renders, RenderTarget::Master)?;
        let mut stems = Vec::new();
        for (name, id) in &named {
            let audio = audio_of(&renders, RenderTarget::Track(TrackId(*id)))?;
            stems.push((name.clone(), *id, signal(audio, renders.start, &tempo)));
        }
        let mix_signal = signal(mix, renders.start, &tempo);
        let mut out = about(&renders, mix.frames(), mix.sample_rate(), &tempo);
        let found = analysis::clashes::clashes(&stems, &mix_signal, top);
        let sentence = found["summary"].as_str().unwrap_or_default().to_owned();
        if let Value::Object(found) = found {
            out.extend(found);
        }
        out.insert("summary".into(), summarize(sentence, &renders));
        Ok(Output::json(Value::Object(out)))
    })
}

// plot

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PlotKind {
    Spectrum,
    Spectrogram,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotArgs {
    kind: PlotKind,
    targets: Vec<TargetArgs>,
    range: Option<Value>,
    #[serde(default)]
    variant: VariantArg,
    markers: Option<Vec<Value>>,
    width: Option<u32>,
    height: Option<u32>,
    freq_range: Option<Vec<f64>>,
    db_range: Option<Vec<f64>>,
}

fn plot_schema() -> Value {
    let pair = |description: &str| {
        json!({
            "type": "array",
            "items": { "type": "number" },
            "minItems": 2,
            "maxItems": 2,
            "description": description,
        })
    };
    schema::object(
        json!({
            "kind": schema::choice(&["spectrum", "spectrogram"], "What to draw."),
            "targets": schema::array(
                target_schema(),
                "What to draw: up to 6 for a spectrum, one for a spectrogram.",
            ),
            "range": range_schema(),
            "variant": variant_schema(),
            "markers": schema::array(
                json!({
                    "oneOf": [
                        { "type": "string", "enum": ["sibilance", "transients", "loop", "selection"] },
                        {
                            "type": "object",
                            "properties": {
                                "from": { "type": "number", "minimum": 0 },
                                "to": { "type": "number", "minimum": 0 },
                                "label": { "type": "string" },
                            },
                            "required": ["from", "to"],
                            "additionalProperties": false,
                        },
                        {
                            "type": "object",
                            "properties": {
                                "at": { "type": "number", "minimum": 0 },
                                "label": { "type": "string" },
                            },
                            "required": ["at"],
                            "additionalProperties": false,
                        },
                    ],
                }),
                "Spectrograms only: what to mark (times in beats).",
            ),
            "width": schema::count("Pixels across, 320–1600 (1024 by default)."),
            "height": schema::count("Pixels down, 200–1000 (512 by default)."),
            "freq_range": pair("The frequencies shown, [low, high] in Hz (20–20000 by default)."),
            "db_range": pair("The levels shown, [floor, ceiling] in dB (-90–0 by default)."),
        }),
        &["kind", "targets"],
    )
}

/// A marker asked for, before the render it may need is there.
enum MarkerArg {
    Sibilance,
    Transients,
    Span(Marker),
}

fn markers(host: &dyn Host, given: &[Value]) -> Result<Vec<MarkerArg>, ToolError> {
    let project = host.project();
    let mut found = Vec::new();
    for (i, marker) in given.iter().enumerate() {
        let wrong = || {
            ToolError(format!(
                "argument `markers[{i}]`: \"sibilance\", \"transients\", \"loop\", \
                 \"selection\", {{\"from\", \"to\", \"label\"}} or {{\"at\", \"label\"}}"
            ))
        };
        let beats = |v: Option<&Value>| -> Result<f64, ToolError> {
            let beats = v.and_then(Value::as_f64).ok_or_else(wrong)?;
            beats_to_tick(beats)?;
            Ok(beats)
        };
        let label = |map: &Map<String, Value>| -> Result<String, ToolError> {
            match map.get("label") {
                None => Ok(String::new()),
                Some(Value::String(label)) => Ok(label.clone()),
                Some(_) => Err(wrong()),
            }
        };
        let span = |(from, to): (Tick, Tick), label: &str| {
            MarkerArg::Span(Marker::Span {
                from: tick_to_beats(from),
                to: tick_to_beats(to),
                label: label.to_owned(),
            })
        };
        found.push(match marker {
            Value::String(name) => match name.as_str() {
                "sibilance" => MarkerArg::Sibilance,
                "transients" => MarkerArg::Transients,
                "loop" => {
                    let region = project.loop_region.ok_or("there's no loop to mark")?;
                    span((region.start, region.end), "loop")
                }
                "selection" => {
                    let range = range(host, Some(&json!("selection")), &[])?;
                    span(range.ok_or("no clips are selected")?, "selection")
                }
                _ => return Err(wrong()),
            },
            Value::Object(map) => {
                let keys: Vec<&str> = map.keys().map(String::as_str).collect();
                if keys.iter().all(|k| ["from", "to", "label"].contains(k))
                    && map.contains_key("from")
                {
                    let (from, to) = (beats(map.get("from"))?, beats(map.get("to"))?);
                    if to < from {
                        return Err(ToolError(format!(
                            "argument `markers[{i}]`: `to` ({to}) is before `from` ({from})"
                        )));
                    }
                    MarkerArg::Span(Marker::Span {
                        from,
                        to,
                        label: label(map)?,
                    })
                } else if keys.iter().all(|k| ["at", "label"].contains(k)) {
                    MarkerArg::Span(Marker::At {
                        beat: beats(map.get("at"))?,
                        label: label(map)?,
                    })
                } else {
                    return Err(wrong());
                }
            }
            _ => return Err(wrong()),
        });
    }
    Ok(found)
}

/// The plot's size and ranges, each checked.
fn plot_size(a: &PlotArgs) -> Result<Plot, ToolError> {
    let mut plot = Plot::default();
    let within = |name: &str, value: u32, min: u32, max: u32| -> Result<u32, ToolError> {
        if (min..=max).contains(&value) {
            Ok(value)
        } else {
            Err(ToolError(format!(
                "`{name}` is {min}–{max} pixels; {value} is out of range"
            )))
        }
    };
    if let Some(width) = a.width {
        plot.width = within("width", width, images::MIN_WIDTH, images::MAX_WIDTH)?;
    }
    if let Some(height) = a.height {
        plot.height = within("height", height, images::MIN_HEIGHT, images::MAX_HEIGHT)?;
    }
    if let Some(given) = &a.freq_range {
        let &[low, high] = given.as_slice() else {
            return Err("`freq_range` is [low, high], in Hz".into());
        };
        if !(low.is_finite() && high.is_finite() && low >= 1.0 && high <= 96_000.0) {
            return Err("`freq_range` is within 1–96000 Hz".into());
        }
        if high < 2.0 * low {
            return Err(ToolError(format!(
                "`freq_range` must span an octave at least; {low}–{high} Hz doesn't"
            )));
        }
        plot.freq = (low as f32, high as f32);
    }
    if let Some(given) = &a.db_range {
        let &[floor, ceiling] = given.as_slice() else {
            return Err("`db_range` is [floor, ceiling], in dB".into());
        };
        if !(floor.is_finite() && ceiling.is_finite() && (-200.0..=50.0).contains(&floor)) {
            return Err("`db_range` is within -200–50 dB".into());
        }
        if ceiling < floor + 6.0 {
            return Err(ToolError(format!(
                "`db_range` must span 6 dB at least; {floor}–{ceiling} dB doesn't"
            )));
        }
        plot.db = (floor as f32, ceiling as f32);
    }
    Ok(plot)
}

fn plot(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: PlotArgs = args(v)?;
    let plot = plot_size(&a)?;
    let targets = a
        .targets
        .iter()
        .map(|t| target(host.project(), t))
        .collect::<Result<Vec<_>, _>>()?;
    match (a.kind, targets.len()) {
        (_, 0) => return Err("`targets` is empty; name what to draw".into()),
        (PlotKind::Spectrum, n) if n > MAX_CURVES => {
            return Err(ToolError(format!(
                "a spectrum overlays at most {MAX_CURVES} targets; {n} were given"
            )));
        }
        (PlotKind::Spectrogram, n) if n > 1 => {
            return Err(ToolError(format!(
                "a spectrogram shows one target; {n} were given: plot them one at a time"
            )));
        }
        _ => {}
    }
    let markers = match (&a.markers, a.kind) {
        (Some(given), PlotKind::Spectrogram) => markers(host, given)?,
        (Some(given), PlotKind::Spectrum) if !given.is_empty() => {
            return Err("markers are for spectrograms; a spectrum has no time axis".into());
        }
        _ => Vec::new(),
    };
    let range = range(host, a.range.as_ref(), &targets)?;
    let tempo = host.project().tempo;
    let mut render_targets: Vec<RenderTarget> = Vec::new();
    for target in &targets {
        if !render_targets.contains(&target.render) {
            render_targets.push(target.render);
        }
    }
    let request = RenderRequest {
        targets: render_targets,
        range,
        variant: a.variant.variant(),
    };
    let kind = a.kind;
    render_then(host, request, move |renders| {
        let first = audio_of(&renders, targets[0].render)?;
        let mut out = about(&renders, first.frames(), first.sample_rate(), &tempo);
        let (png, axes) = match kind {
            PlotKind::Spectrum => {
                let mut curves = Vec::new();
                for target in &targets {
                    let audio = audio_of(&renders, target.render)?;
                    let signal = signal(audio, renders.start, &tempo);
                    curves.push((
                        target.name.clone(),
                        analysis::spectrum::average(&signal, analysis::spectrum::FFT),
                    ));
                }
                images::spectrum_png(&curves, &plot, PEAKS)
            }
            PlotKind::Spectrogram => {
                let signal = signal(first, renders.start, &tempo);
                let marks: Vec<Marker> = markers
                    .into_iter()
                    .flat_map(|marker| match marker {
                        MarkerArg::Span(marker) => vec![marker],
                        MarkerArg::Sibilance => analysis::sibilance::events(&signal)
                            .0
                            .iter()
                            .map(|e| Marker::Span {
                                from: signal.beat_at(e.start as f64),
                                to: signal.beat_at(e.end as f64),
                                label: "ess".to_owned(),
                            })
                            .collect(),
                        MarkerArg::Transients => analysis::transients::onsets(&signal)
                            .iter()
                            .map(|o| Marker::At {
                                beat: signal.beat_at(o.frame as f64),
                                label: String::new(),
                            })
                            .collect(),
                    })
                    .collect();
                let columns = images::spectrogram_columns(&plot);
                let grid = images::spectrogram(&signal, SPECTROGRAM_FFT, columns);
                images::spectrogram_png(&grid, &signal, &plot, &marks)
            }
        };
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        let sentence = format!(
            "{} of {}, {}–{}.",
            match kind {
                PlotKind::Spectrum => "The average spectrum",
                PlotKind::Spectrogram => "A spectrogram",
            },
            names.join(", "),
            out["range"]["from_bar"].as_str().unwrap_or("?"),
            out["range"]["to_bar"].as_str().unwrap_or("?"),
        );
        out.insert(
            "targets".into(),
            Value::Array(targets.iter().map(|t| t.json.clone()).collect()),
        );
        out.insert("axes".into(), axes);
        out.insert("summary".into(), summarize(sentence, &renders));
        Ok(Output {
            json: Value::Object(out),
            images: vec![png],
        })
    })
}

// render_audio

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderArgs {
    target: TargetArgs,
    range: Option<Value>,
    #[serde(default)]
    variant: VariantArg,
    path: PathBuf,
}

fn render_schema() -> Value {
    schema::object(
        json!({
            "target": target_schema(),
            "range": range_schema(),
            "variant": variant_schema(),
            "path": schema::string(
                "Where to write the WAV: a full path ending in .wav, in a folder that \
                 exists, to a file that doesn't.",
            ),
        }),
        &["target", "path"],
    )
}

/// Why `path` can't take a new WAV file, if it can't.
fn unwritable(path: &std::path::Path) -> Option<String> {
    let shown = path.display();
    if !path.is_absolute() {
        return Some(format!(
            "{shown} isn't a full path; give the file's full path (canopy runs in its own folder)"
        ));
    }
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        return Some(format!("{shown} doesn't end in .wav"));
    }
    if path.exists() {
        return Some(format!(
            "{shown} already exists, and canopy never overwrites a file: pick another name"
        ));
    }
    match path.parent() {
        Some(folder) if folder.is_dir() => None,
        _ => Some(format!("{shown}: its folder doesn't exist")),
    }
}

fn render_audio(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: RenderArgs = args(v)?;
    if let Some(why) = unwritable(&a.path) {
        return Err(ToolError(why));
    }
    let target = target(host.project(), &a.target)?;
    let range = range(host, a.range.as_ref(), std::slice::from_ref(&target))?;
    let tempo = host.project().tempo;
    let request = RenderRequest {
        targets: vec![target.render],
        range,
        variant: a.variant.variant(),
    };
    let (path, variant) = (a.path, a.variant);
    render_then(host, request, move |renders| {
        let audio = audio_of(&renders, target.render)?;
        // It may have appeared while the render ran.
        if let Some(why) = unwritable(&path) {
            return Err(ToolError(why));
        }
        media::write_wav(&path, audio)
            .map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        let peak = audio
            .left()
            .iter()
            .chain(audio.right())
            .fold(0.0f32, |m, s| m.max(s.abs()));
        let mut out = about(&renders, audio.frames(), audio.sample_rate(), &tempo);
        out.insert("path".into(), json!(path.display().to_string()));
        out.insert("target".into(), target.json.clone());
        out.insert("variant".into(), json!(variant.name()));
        out.insert("rate".into(), json!(audio.sample_rate()));
        out.insert(
            "peak_db".into(),
            json!((analysis::report::amplitude_db(f64::from(peak)) * 10.0).round() / 10.0),
        );
        let sentence = format!(
            "Wrote {} seconds of {} to {}.",
            out["range"]["seconds"],
            target.name,
            path.display()
        );
        out.insert("summary".into(), summarize(sentence, &renders));
        Ok(Output::json(Value::Object(out)))
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::testing::FakeHost;
    use crate::tools::call;
    use project::{NewTrack, TrackPosition};

    const RATE: u32 = 48_000;

    /// A song with a track and a clip, at 120 BPM.
    fn rig() -> (FakeHost, TrackId, project::ClipId) {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project.rename_track(track, "Lead").unwrap();
        let clip = project
            .add_midi_clip(track, Tick::new(3840), Tick::new(3840))
            .unwrap();
        (FakeHost::new(project), track, clip)
    }

    /// `seconds` of a sine at `hz` and `amplitude` on both channels.
    fn sine(hz: f64, amplitude: f64, seconds: f64) -> Arc<AudioBuffer> {
        let n = (seconds * f64::from(RATE)) as usize;
        let wave: Vec<f32> = (0..n)
            .map(|i| {
                (amplitude * (std::f64::consts::TAU * hz * i as f64 / f64::from(RATE)).sin()) as f32
            })
            .collect();
        Arc::new(AudioBuffer::new(RATE, wave.clone(), wave))
    }

    fn rendered(audio: Vec<(RenderTarget, Arc<AudioBuffer>)>, stale: bool) -> Poll {
        Poll::Rendered(Ok(Arc::new(Renders {
            version: 1,
            stale,
            start: Tick::ZERO,
            audio,
        })))
    }

    /// Calls `tool`, which must wait; scripts its render's answers, then
    /// polls the waiter until it answers.
    fn answer(
        host: &mut FakeHost,
        tool: &str,
        args: Value,
        polls: impl IntoIterator<Item = Poll>,
    ) -> Result<Output, ToolError> {
        let ticket = host.next_ticket();
        let mut waiter = match call(tool, &args, host)? {
            Outcome::Waiting(waiter) => waiter,
            Outcome::Done(_) => panic!("{tool} waits for its render"),
        };
        host.script(ticket, polls);
        let start = Instant::now();
        loop {
            if let Some(result) = waiter(host) {
                return result;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "{tool} answers");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn analyze_returns_each_measure_asked() {
        let (mut host, track, _) = rig();
        let out = answer(
            &mut host,
            "analyze",
            json!({ "target": { "track": track.0 }, "measures": ["spectrum", "loudness"],
                    "variant": "pre_fader", "range": { "from": 4, "to": 6 } }),
            [
                Poll::Waiting { progress: 0.5 },
                rendered(
                    vec![(RenderTarget::Track(track), sine(1000.0, 0.5, 1.0))],
                    false,
                ),
            ],
        )
        .unwrap()
        .json;
        assert_eq!(
            host.renders,
            [RenderRequest {
                targets: vec![RenderTarget::Track(track)],
                range: Some((Tick::new(3840), Tick::new(5760))),
                variant: Variant::PreFader,
            }]
        );
        let measures = out["measures"].as_object().unwrap();
        assert_eq!(measures.len(), 2, "{out}");
        let peak = out["measures"]["spectrum"]["peaks"][0]["hz"]
            .as_f64()
            .unwrap();
        assert!((peak - 1000.0).abs() < 15.0, "{out}");
        assert!(out["measures"]["loudness"]["summary"].is_string());
        assert_eq!(out["target"]["name"], json!("Lead"));
        assert_eq!(out["variant"], json!("pre_fader"));
        assert_eq!(out["range"]["seconds"], json!(1.0));
        assert!(out.get("stale").is_none());
        assert!(out["summary"].as_str().unwrap().starts_with("Lead, 1.1–"));

        // A clip renders its track over its span; a wrong name lists the right ones.
        let (mut host, track, clip) = rig();
        let Ok(Outcome::Waiting(_)) = call(
            "analyze",
            &json!({ "target": { "clip": clip.0 }, "measures": ["key"] }),
            &mut host,
        ) else {
            panic!("a clip renders");
        };
        assert_eq!(host.renders[0].targets, [RenderTarget::Track(track)]);
        assert_eq!(
            host.renders[0].range,
            Some((Tick::new(3840), Tick::new(7680)))
        );
        let Err(error) = call(
            "analyze",
            &json!({ "target": { "master": true }, "measures": ["tempo"] }),
            &mut host,
        ) else {
            panic!("an unknown measure");
        };
        assert!(error.0.contains("sibilance"), "{error}");
    }

    #[test]
    fn plot_returns_an_image_and_its_axes() {
        let (mut host, track, _) = rig();
        let out = answer(
            &mut host,
            "plot",
            json!({ "kind": "spectrum", "targets": [{ "track": track.0 }, { "master": true }],
                    "width": 640, "height": 320 }),
            [rendered(
                vec![
                    (RenderTarget::Track(track), sine(1000.0, 0.5, 1.0)),
                    (RenderTarget::Master, sine(200.0, 0.5, 1.0)),
                ],
                false,
            )],
        )
        .unwrap();
        assert_eq!(out.images.len(), 1);
        assert!(out.images[0].starts_with(b"\x89PNG"));
        let axes = &out.json["axes"];
        assert_eq!(axes["kind"], json!("spectrum"));
        assert_eq!(axes["width"], json!(640));
        assert_eq!(axes["curves"].as_array().unwrap().len(), 2);
        assert_eq!(axes["curves"][1]["name"], json!("Master"));

        let out = answer(
            &mut host,
            "plot",
            json!({ "kind": "spectrogram", "targets": [{ "track": track.0 }],
                    "markers": [{ "at": 1, "label": "here" }] }),
            [rendered(
                vec![(RenderTarget::Track(track), sine(1000.0, 0.5, 1.0))],
                false,
            )],
        )
        .unwrap();
        assert_eq!(out.json["axes"]["kind"], json!("spectrogram"));
        assert_eq!(out.json["axes"]["markers"].as_array().unwrap().len(), 1);

        // Sizes out of range, and two spectrogram targets, are refused.
        let refused = |host: &mut FakeHost, args: Value| match call("plot", &args, host) {
            Err(error) => error.0,
            Ok(_) => panic!("refused: {args}"),
        };
        let error = refused(
            &mut host,
            json!({ "kind": "spectrum", "targets": [{ "master": true }], "width": 4000 }),
        );
        assert!(error.contains("320–1600"), "{error}");
        let error = refused(
            &mut host,
            json!({ "kind": "spectrogram", "targets": [{ "master": true }, { "track": track.0 }] }),
        );
        assert!(error.contains("one target"), "{error}");
    }

    #[test]
    fn an_existing_file_is_not_overwritten() {
        let (mut host, track, _) = rig();
        let dir = std::env::temp_dir().join(format!("canopy-mcp-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("taken.wav");
        std::fs::write(&existing, b"mine").unwrap();
        let Err(error) = call(
            "render_audio",
            &json!({ "target": { "track": track.0 }, "path": existing }),
            &mut host,
        ) else {
            panic!("refused");
        };
        assert!(error.0.contains("never overwrites"), "{error}");
        assert_eq!(std::fs::read(&existing).unwrap(), b"mine");
        assert!(host.renders.is_empty(), "nothing rendered");

        let fresh = dir.join("lead.wav");
        let out = answer(
            &mut host,
            "render_audio",
            json!({ "target": { "track": track.0 }, "path": fresh }),
            [rendered(
                vec![(RenderTarget::Track(track), sine(440.0, 0.5, 0.5))],
                false,
            )],
        )
        .unwrap();
        assert_eq!(out.json["path"], json!(fresh.display().to_string()));
        let (written, _) = media::decode(&fresh).unwrap();
        assert_eq!(written.frames(), RATE as usize / 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_render_says_so() {
        let (mut host, track, _) = rig();
        let out = answer(
            &mut host,
            "analyze",
            json!({ "target": { "track": track.0 }, "measures": ["loudness"] }),
            [rendered(
                vec![(RenderTarget::Track(track), sine(1000.0, 0.5, 1.0))],
                true,
            )],
        )
        .unwrap()
        .json;
        assert_eq!(out["stale"], json!(true));
        assert!(out["note"].as_str().unwrap().contains("changed"), "{out}");
        assert!(
            out["summary"]
                .as_str()
                .unwrap()
                .contains("before its latest change")
        );
    }

    #[test]
    fn clashes_render_every_track_with_clips_and_the_mix() {
        let (mut host, lead, _) = rig();
        let bass = host
            .project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        host.project
            .add_midi_clip(bass, Tick::ZERO, Tick::new(3840))
            .unwrap();
        // No clips: left out.
        host.project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let out = answer(
            &mut host,
            "find_clashes",
            json!({}),
            [rendered(
                vec![
                    (RenderTarget::Track(lead), sine(300.0, 0.3, 2.0)),
                    (RenderTarget::Track(bass), sine(310.0, 0.3, 2.0)),
                    (RenderTarget::Master, sine(305.0, 0.6, 2.0)),
                ],
                false,
            )],
        )
        .unwrap()
        .json;
        assert_eq!(
            host.renders[0].targets,
            [
                RenderTarget::Track(lead),
                RenderTarget::Track(bass),
                RenderTarget::Master
            ]
        );
        assert_eq!(out["pairs"].as_array().unwrap().len(), 1, "{out}");
        assert!(out["summary"].as_str().unwrap().contains("clash"), "{out}");

        // A group and a track inside it would clash with themselves.
        let group = host
            .project
            .add_track(NewTrack::Group, TrackPosition::END)
            .unwrap();
        let inner = host
            .project
            .add_track(
                NewTrack::Midi,
                TrackPosition {
                    parent: Some(group),
                    before: None,
                },
            )
            .unwrap();
        let Err(error) = call(
            "find_clashes",
            &json!({ "tracks": [group.0, inner.0] }),
            &mut host,
        ) else {
            panic!("refused");
        };
        assert!(error.0.contains("inside group"), "{error}");
    }

    #[test]
    fn a_failed_render_or_a_bad_range_is_an_error() {
        let (mut host, track, _) = rig();
        let error = answer(
            &mut host,
            "analyze",
            json!({ "target": { "track": track.0 }, "measures": ["key"] }),
            [Poll::Rendered(Err("too long".into()))],
        )
        .unwrap_err();
        assert_eq!(error.0, "too long");
        for range in [
            json!({ "from": 8, "to": 4 }),
            json!("loop"),
            json!("selection"),
        ] {
            let Err(error) = call(
                "analyze",
                &json!({ "target": { "track": track.0 }, "measures": ["key"], "range": range }),
                &mut host,
            ) else {
                panic!("refused: {range}");
            };
            assert!(!error.0.is_empty());
        }
    }
}
