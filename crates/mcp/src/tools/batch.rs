//! `batch`: many edits as one undo step, all or nothing. Later operations
//! can name what earlier ones made: `"$2"` anywhere in an operation's
//! arguments stands for operation 2's `id`.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Kind, MAX_BATCH, Outcome, Output, Tool, ToolError, args, explain, find, schema};
use crate::host::Host;
use project::EditError;

pub const TOOLS: &[Tool] = &[Tool {
    name: "batch",
    description: "Runs up to 200 edits in order as one undo step: if any fails, none happen, \
                  and the error says which (counting from 0) and why. Each operation is a tool \
                  name and its arguments; only edit tools can be batched, not reads, history, \
                  transport or analysis. In an operation's arguments, the string \"$N\" stands \
                  for the id operation N made (e.g. add_track's track), so later operations \
                  can use what earlier ones made.",
    schema: batch_schema,
    examples: &[
        r#"{"operations": [{"tool": "add_track", "arguments": {"kind": "midi", "name": "Bass"}}, {"tool": "edit_track", "arguments": {"track": "$0", "volume_db": -6}}]}"#,
        r#"{"operations": [{"tool": "edit_track", "arguments": {"track": 12, "color": "red"}}, {"tool": "edit_track", "arguments": {"track": 13, "color": "red"}}]}"#,
    ],
    kind: Kind::Session(batch),
}];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    operations: Vec<Operation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

fn batch_schema() -> Value {
    schema::object(
        json!({
            "operations": schema::array(
                schema::object(
                    json!({
                        "tool": schema::string("An edit tool's name."),
                        "arguments": {
                            "type": "object",
                            "description": "Its arguments, as the tool takes them; \"$N\" stands for operation N's id.",
                        },
                    }),
                    &["tool", "arguments"],
                ),
                "The edits, in order, at most 200.",
            ),
        }),
        &["operations"],
    )
}

type Planner = fn(&Value, &project::Project) -> Result<super::Plan, ToolError>;

fn batch(v: &Value, host: &mut dyn Host) -> Result<Outcome, ToolError> {
    let a: BatchArgs = args(v)?;
    let count = a.operations.len();
    if count == 0 {
        return Err("the batch is empty; give at least one operation".into());
    }
    if count > MAX_BATCH {
        return Err(ToolError(format!(
            "{count} operations is too many for one batch; at most {MAX_BATCH}"
        )));
    }
    let mut planners: Vec<Planner> = Vec::with_capacity(count);
    let mut refused = Vec::new();
    for (i, op) in a.operations.iter().enumerate() {
        match find(&op.tool).map(|t| t.kind) {
            Some(Kind::Edit(plan)) => planners.push(plan),
            Some(_) => refused.push(format!("{i} ({})", op.tool)),
            None => refused.push(format!("{i} ({}, which isn't a tool)", op.tool)),
        }
    }
    if !refused.is_empty() {
        return Err(ToolError(format!(
            "only edits can be batched; call the others on their own. Not edits: operation {}",
            refused.join(", ")
        )));
    }

    // The step is named after the first edit, which can't refer to any
    // other, so it plans now against the project as it is.
    let tool = &a.operations[0].tool;
    let arguments =
        substitute(&a.operations[0].arguments, &[], 0).map_err(|e| failed(0, tool, &e))?;
    let first = planners[0](&arguments, host.project()).map_err(|e| failed(0, tool, &e.0))?;
    let label = match count {
        1 => format!("LLM: {}", first.label),
        n => format!("LLM: {} and {} more", first.label, n - 1),
    };
    drop(first);

    let operations = a.operations;
    let json = host
        .edit(
            &label,
            Box::new(move |project| {
                let mut results: Vec<Value> = Vec::with_capacity(count);
                let mut done = Vec::with_capacity(count);
                for (i, (op, plan)) in operations.iter().zip(planners).enumerate() {
                    let arguments = substitute(&op.arguments, &results, i)
                        .map_err(|e| EditError::Invalid(failed(i, &op.tool, &e)))?;
                    let planned = plan(&arguments, project)
                        .map_err(|e| EditError::Invalid(failed(i, &op.tool, &e.0)))?;
                    done.push(planned.label);
                    let result = (planned.apply)(project).map_err(|e| {
                        EditError::Invalid(failed(i, &op.tool, &explain(&e, project)))
                    })?;
                    results.push(result);
                }
                Ok(json!({
                    "results": results,
                    "summary": format!("{count} edits as one undo step: {}", done.join("; ")),
                }))
            }),
        )
        .map_err(|e| ToolError(explain(&e, host.project())))?;
    Ok(Outcome::Done(Output::json(json)))
}

/// "operation 2 (edit_track) failed, so nothing was done: …".
fn failed(index: usize, tool: &str, why: &str) -> String {
    format!("operation {index} ({tool}) failed, so nothing in the batch was done: {why}")
}

/// `arguments` with every string `"$N"` replaced by operation N's `id`
/// (N before `index`, the operation these are for).
fn substitute(arguments: &Value, results: &[Value], index: usize) -> Result<Value, String> {
    Ok(match arguments {
        Value::String(s) => match s.strip_prefix('$').map(str::parse::<usize>) {
            Some(Ok(n)) if n >= index => {
                return Err(format!(
                    "{s:?} names operation {n}, which hasn't run yet; \"$N\" names an \
                     earlier operation's id"
                ));
            }
            Some(Ok(n)) => match results[n].get("id") {
                Some(id) => id.clone(),
                None => {
                    return Err(format!(
                        "{s:?} names operation {n}, which made nothing with an id"
                    ));
                }
            },
            _ => arguments.clone(),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| substitute(v, results, index))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), substitute(v, results, index)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use crate::tools::call;
    use project::{NewTrack, Project, TrackPosition};

    fn run(args: Value, host: &mut FakeHost) -> Result<Value, String> {
        match call("batch", &args, host) {
            Ok(Outcome::Done(output)) => Ok(output.json),
            Ok(Outcome::Waiting(_)) => panic!("a batch answers at once"),
            Err(error) => Err(error.0),
        }
    }

    fn add(name: &str) -> Value {
        json!({ "tool": "add_track", "arguments": { "kind": "midi", "name": name } })
    }

    #[test]
    fn a_batch_is_one_undo_step() {
        let mut host = FakeHost::new(Project::new());
        let out = run(
            json!({ "operations": [add("Bass"), add("Lead"), add("Pad")] }),
            &mut host,
        )
        .unwrap();
        assert_eq!(host.project.tracks.len(), 3);
        assert_eq!(host.history.position(), 1);
        assert_eq!(
            host.history.undo_label(),
            Some("LLM: Add MIDI track Bass and 2 more")
        );
        assert_eq!(out["results"].as_array().unwrap().len(), 3);
        assert!(out["results"][2]["id"].is_u64());
        host.undo();
        assert!(host.project.tracks.is_empty());
    }

    #[test]
    fn a_failing_operation_undoes_the_whole_batch() {
        let mut host = FakeHost::new(Project::new());
        host.project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let before = host.project.clone();
        let error = run(
            json!({ "operations": [
                add("Bass"),
                add("Lead"),
                { "tool": "edit_track", "arguments": { "track": "$1", "pan": 2 } },
            ] }),
            &mut host,
        )
        .unwrap_err();
        assert!(error.contains("operation 2 (edit_track)"), "{error}");
        assert!(error.contains("-1–1"), "{error}");
        assert!(host.project == before, "no tracks added");
        assert_eq!(host.history.position(), 0, "no undo step");

        // An edit's own error (not the planning's) is named the same way.
        let error = run(
            json!({ "operations": [
                add("Bass"),
                { "tool": "arrange_tracks", "arguments": { "op": "group", "tracks": ["$0", 1] } },
                { "tool": "edit_track", "arguments": { "track": 999, "mute": true } },
            ] }),
            &mut host,
        )
        .unwrap_err();
        assert!(error.contains("operation 2"), "{error}");
        assert!(host.project == before);
    }

    #[test]
    fn later_operations_refer_to_earlier_ids() {
        let mut host = FakeHost::new(Project::new());
        let out = run(
            json!({ "operations": [
                add("Bass"),
                { "tool": "edit_track", "arguments": { "track": "$0", "volume_db": -6 } },
                add("Lead"),
                { "tool": "arrange_tracks", "arguments": { "op": "group", "tracks": ["$0", "$2"], "name": "Synths" } },
                { "tool": "edit_track", "arguments": { "track": "$3", "folded": true } },
            ] }),
            &mut host,
        )
        .unwrap();
        let bass = project::TrackId(out["results"][0]["id"].as_u64().unwrap() as u32);
        let group = project::TrackId(out["results"][3]["id"].as_u64().unwrap() as u32);
        assert_eq!(host.project.track(bass).unwrap().mixer.volume_db, -6.0);
        assert_eq!(host.project.parent(bass), Some(group));
        assert!(host.project.track(group).unwrap().is_folded());
        assert_eq!(host.history.position(), 1);

        let error = run(
            json!({ "operations": [
                { "tool": "edit_track", "arguments": { "track": "$1", "mute": true } },
                add("Pad"),
            ] }),
            &mut host,
        )
        .unwrap_err();
        assert!(error.contains("hasn't run yet"), "{error}");
        let error = run(
            json!({ "operations": [
                { "tool": "edit_song", "arguments": { "tempo_bpm": 100 } },
                { "tool": "edit_track", "arguments": { "track": "$0", "mute": true } },
            ] }),
            &mut host,
        )
        .unwrap_err();
        assert!(error.contains("nothing with an id"), "{error}");
        assert_eq!(host.history.position(), 1);
    }

    #[test]
    fn reads_are_not_allowed_in_a_batch() {
        let mut host = FakeHost::new(Project::new());
        let error = run(
            json!({ "operations": [
                add("Bass"),
                { "tool": "get_project", "arguments": {} },
                { "tool": "history", "arguments": { "op": "undo" } },
                { "tool": "make_hit", "arguments": {} },
            ] }),
            &mut host,
        )
        .unwrap_err();
        assert!(error.contains("1 (get_project)"), "{error}");
        assert!(error.contains("2 (history)"), "{error}");
        assert!(error.contains("3 (make_hit"), "{error}");
        assert!(host.project.tracks.is_empty());

        let nested = run(
            json!({ "operations": [{ "tool": "batch", "arguments": { "operations": [] } }] }),
            &mut host,
        );
        assert!(nested.unwrap_err().contains("0 (batch)"));
        let many: Vec<Value> = (0..201).map(|i| add(&i.to_string())).collect();
        let error = run(json!({ "operations": many }), &mut host).unwrap_err();
        assert!(error.contains("200"), "{error}");
        assert!(run(json!({ "operations": [] }), &mut host).is_err());
    }
}
