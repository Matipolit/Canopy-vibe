//! Tools that drive the app rather than the project: undo and redo, and
//! the transport.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Kind, Outcome, Output, Tool, ToolError, args, schema};
use crate::host::Host;
use crate::units::{bar_beat, beats_to_tick, tick_to_beats};

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "history",
        description: "Undo, redo, or jump to a step of the undo history (get_history lists the \
                      steps). Undoes the user's edits as well as yours: check get_history first.",
        schema: history_schema,
        examples: &[
            r#"{"op": "undo"}"#,
            r#"{"op": "redo"}"#,
            r#"{"op": "jump", "to": 12}"#,
        ],
        kind: Kind::Session(history),
    },
    Tool {
        name: "transport",
        description: "Start or stop playback, or move the playhead (in beats).",
        schema: transport_schema,
        examples: &[r#"{"op": "play"}"#, r#"{"op": "seek", "at": 16}"#],
        kind: Kind::Session(transport),
    },
];

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum HistoryOp {
    Undo,
    Redo,
    Jump,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    op: HistoryOp,
    to: Option<usize>,
}

fn history_schema() -> Value {
    schema::object(
        json!({
            "op": schema::choice(&["undo", "redo", "jump"], "What to do."),
            "to": schema::count(
                "For jump: how many steps are done afterwards, a step's `step` from \
                 get_history (0 undoes them all)."
            ),
        }),
        &["op"],
    )
}

fn history(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: HistoryArgs = args(v)?;
    let done = match (a.op, a.to) {
        (HistoryOp::Undo, None) => {
            let label = host.undo().ok_or("there's nothing to undo")?;
            json!({ "undid": label })
        }
        (HistoryOp::Redo, None) => {
            let label = host.redo().ok_or("there's nothing to redo")?;
            json!({ "redid": label })
        }
        (HistoryOp::Jump, Some(to)) => {
            let steps = host.history().steps().count();
            if to > steps {
                return Err(ToolError(format!(
                    "there are {steps} steps; `to` is 0–{steps}"
                )));
            }
            host.jump(to);
            json!({})
        }
        (HistoryOp::Jump, None) => return Err("jump needs `to`".into()),
        (_, Some(_)) => return Err("`to` is only for jump".into()),
    };
    let mut done = done;
    let history = host.history();
    done["position"] = json!(history.position());
    done["summary"] = json!(match history.undo_label() {
        Some(label) => format!("{} steps done; the last is {label:?}", history.position()),
        None => "no steps done".to_owned(),
    });
    Ok(Outcome::Done(Output::json(done)))
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum TransportOp {
    Play,
    Stop,
    Seek,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportArgs {
    op: TransportOp,
    at: Option<f64>,
}

fn transport_schema() -> Value {
    schema::object(
        json!({
            "op": schema::choice(&["play", "stop", "seek"], "What to do."),
            "at": schema::beats("For seek: where to move the playhead, in beats."),
        }),
        &["op"],
    )
}

fn transport(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: TransportArgs = args(v)?;
    match (a.op, a.at) {
        (TransportOp::Play, None) => host.set_playing(true),
        (TransportOp::Stop, None) => host.set_playing(false),
        (TransportOp::Seek, Some(at)) => host.seek(beats_to_tick(at)?),
        (TransportOp::Seek, None) => return Err("seek needs `at`, in beats".into()),
        (_, Some(_)) => return Err("`at` is only for seek".into()),
    }
    let now = host.transport();
    let tempo = host.project().tempo;
    let at = bar_beat(now.playhead, &tempo);
    Ok(Outcome::Done(Output::json(json!({
        "playing": now.playing,
        "playhead": tick_to_beats(now.playhead),
        "playhead_bar": at,
        "summary": format!("{} at {at}", if now.playing { "playing" } else { "stopped" }),
    }))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use crate::tools::call;
    use project::{NewTrack, Project, TrackPosition};

    fn done(outcome: Result<Outcome, ToolError>) -> Value {
        match outcome {
            Ok(Outcome::Done(output)) => output.json,
            Ok(Outcome::Waiting(_)) => panic!("answers at once"),
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn history_undo_redo_and_jump() {
        let mut host = FakeHost::new(Project::new());
        for _ in 0..3 {
            host.history
                .edit(&mut host.project, "Add Track", |p| {
                    p.add_track(NewTrack::Midi, TrackPosition::END)
                })
                .unwrap();
        }
        let undone = done(call("history", &json!({ "op": "undo" }), &mut host));
        assert_eq!(undone["undid"], json!("Add Track"));
        assert_eq!(undone["position"], json!(2));
        assert_eq!(host.project.tracks.len(), 2);

        let redone = done(call("history", &json!({ "op": "redo" }), &mut host));
        assert_eq!(redone["position"], json!(3));
        assert_eq!(host.project.tracks.len(), 3);

        let jumped = done(call(
            "history",
            &json!({ "op": "jump", "to": 0 }),
            &mut host,
        ));
        assert_eq!(jumped["position"], json!(0));
        assert!(host.project.tracks.is_empty());
        let Err(error) = call("history", &json!({ "op": "undo" }), &mut host) else {
            panic!("nothing left to undo");
        };
        assert!(error.0.contains("nothing to undo"), "{error}");

        done(call(
            "history",
            &json!({ "op": "jump", "to": 3 }),
            &mut host,
        ));
        assert_eq!(host.project.tracks.len(), 3);
        let Err(error) = call("history", &json!({ "op": "jump", "to": 4 }), &mut host) else {
            panic!("past the last step");
        };
        assert!(error.0.contains("0–3"), "{error}");
        assert!(call("history", &json!({ "op": "jump" }), &mut host).is_err());
    }

    #[test]
    fn transport_seeks_in_beats() {
        let mut host = FakeHost::new(Project::new());
        let moved = done(call(
            "transport",
            &json!({ "op": "seek", "at": 6.5 }),
            &mut host,
        ));
        assert_eq!(host.transport_calls, ["seek 6240"]);
        assert_eq!(moved["playhead"], json!(6.5));
        assert_eq!(moved["playhead_bar"], json!("2.3.50"));

        let playing = done(call("transport", &json!({ "op": "play" }), &mut host));
        assert_eq!(playing["playing"], json!(true));
        done(call("transport", &json!({ "op": "stop" }), &mut host));
        assert_eq!(host.transport_calls, ["seek 6240", "play", "stop"]);

        let Err(error) = call("transport", &json!({ "op": "seek", "at": -1 }), &mut host) else {
            panic!("a negative time");
        };
        assert!(error.0.contains("-1"), "{error}");
        assert!(call("transport", &json!({ "op": "seek" }), &mut host).is_err());
        assert_eq!(host.transport_calls.len(), 3, "failed calls do nothing");
    }
}
