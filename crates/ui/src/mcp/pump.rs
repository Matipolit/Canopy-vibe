//! Runs LLM clients' tool calls on the UI thread, once a frame.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::mcp::host::{Host, Ticket};
use ::mcp::protocol;
use ::mcp::tools::{self, Kind, Outcome, Output, ToolError, Waiter};
use project::Origin;
use serde_json::json;

use super::host::AppHost;
use super::listener::{Call, Server};
use crate::session::LLM_PREFIX;

/// How long a frame spends on calls, at most; the rest wait for the next.
pub const FRAME_BUDGET: Duration = Duration::from_millis(4);

/// How soon to look again at calls that are waiting (on a gesture, a
/// dialog, or a job), when nothing else asks for a frame.
const RECHECK: Duration = Duration::from_millis(50);

/// How often a waiting call whose client gave a progress token hears how
/// its job is going.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

const TURNED_OFF: &str = "canopy has LLM access turned off (View → Allow LLM connections)";

/// A call whose answer comes on a later frame.
pub struct Waiting {
    call: Call,
    waiter: Waiter,
    /// The renders it started, cancelled if the call is.
    tickets: Vec<Ticket>,
    /// When progress was last sent, and what it was.
    reported: (Instant, f32),
}

/// Who answers, and what the status bar says about it.
struct Status<'s> {
    last_action: &'s mut Option<String>,
    last_client: &'s mut Option<Arc<str>>,
}

/// Runs the calls that have arrived, in order, until `budget` is spent.
/// Reads answer at once; calls that change things wait while the user is
/// mid-gesture or a dialog is open (`app.modal`); with `allowed` false
/// every call is refused. Renders for analysis are looked after first.
/// Returns how soon the app should run another frame for calls still
/// waiting, if any are.
pub fn pump<'a>(
    server: &'a mut Server,
    mut app: AppHost<'a>,
    allowed: bool,
    budget: Duration,
) -> Option<Duration> {
    server.receive();
    let start = Instant::now();
    let Server {
        queue,
        waiting,
        renders,
        last_action,
        last_client,
        ..
    } = server;
    renders.update(app.session);
    app.renders = Some(renders);
    let mut status = Status {
        last_action,
        last_client,
    };

    for mut job in std::mem::take(waiting) {
        if job.call.is_cancelled() {
            // Whatever it was doing is dropped with its waiter.
            cancel(&mut app, &job.tickets);
            continue;
        }
        if !allowed {
            cancel(&mut app, &job.tickets);
            job.call
                .answer(tools::tool_result(&Err(ToolError(TURNED_OFF.into()))));
            continue;
        }
        app.session.set_origin(origin(&job.call));
        let polled = panic::catch_unwind(AssertUnwindSafe(|| (job.waiter)(&mut app)));
        app.session.set_origin(Origin::User);
        job.tickets.extend(app.take_started());
        match polled {
            Ok(None) => {
                report(&mut job, &app);
                waiting.push(job);
            }
            Ok(Some(result)) => {
                cancel(&mut app, &job.tickets);
                job.call.answer(tools::tool_result(&result));
            }
            Err(panic) => {
                cancel(&mut app, &job.tickets);
                let error = Err(crashed(&job.call.name, &panic));
                job.call.answer(tools::tool_result(&error));
            }
        }
    }

    let blocked = app.session.is_live() || app.modal;
    let mut later = std::mem::take(queue);
    let mut out_of_time = false;
    let mut ran = false;
    while let Some(call) = later.pop_front() {
        if call.is_cancelled() {
            continue;
        }
        if ran && start.elapsed() >= budget {
            out_of_time = true;
            queue.push_back(call);
            continue;
        }
        if !allowed {
            call.answer(tools::tool_result(&Err(ToolError(TURNED_OFF.into()))));
            continue;
        }
        if blocked && changes_things(&call.name) {
            queue.push_back(call);
            continue;
        }
        ran = true;
        run(waiting, &mut status, &mut app, call);
    }

    if out_of_time {
        Some(Duration::ZERO)
    } else if !queue.is_empty() || !waiting.is_empty() {
        Some(RECHECK)
    } else {
        None
    }
}

/// Stops the renders `tickets` name (finished ones are forgotten).
fn cancel(app: &mut AppHost<'_>, tickets: &[Ticket]) {
    for &ticket in tickets {
        app.cancel(ticket);
    }
}

/// Tells a waiting call's client how its renders are going, every
/// [`PROGRESS_EVERY`], if it gave a progress token and there is news.
fn report(job: &mut Waiting, app: &AppHost<'_>) {
    let Some(token) = &job.call.progress else {
        return;
    };
    let (at, sent) = job.reported;
    if at.elapsed() < PROGRESS_EVERY {
        return;
    }
    let Some(progress) = job
        .tickets
        .iter()
        .filter_map(|&t| app.progress(t))
        .reduce(f32::min)
    else {
        return;
    };
    // Progress only ever goes up.
    if progress <= sent {
        return;
    }
    job.reported = (Instant::now(), progress);
    let line = protocol::notification(
        "notifications/progress",
        json!({
            "progressToken": token,
            "progress": progress,
            "total": 1.0,
            "message": "rendering",
        }),
    );
    let _ = job.call.reply.send(line);
}

/// Whether tool `name` edits or drives the app, and so waits for the user
/// to finish a gesture. Reads, jobs and unknown names (an error) don't.
fn changes_things(name: &str) -> bool {
    tools::find(name).is_some_and(|tool| matches!(tool.kind, Kind::Edit(_) | Kind::Session(_)))
}

/// Who a call's undo steps are by: its client, named as it introduced
/// itself.
fn origin(call: &Call) -> Origin {
    Origin::Llm {
        client: call.client.as_deref().unwrap_or("LLM").to_owned(),
    }
}

fn run(waiting: &mut Vec<Waiting>, status: &mut Status<'_>, app: &mut AppHost<'_>, call: Call) {
    let generation = app.history().generation();
    app.session.set_origin(origin(&call));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        tools::call(&call.name, &call.args, app)
    }));
    app.session.set_origin(Origin::User);
    let tickets = app.take_started();
    let result: Result<Output, ToolError> = match outcome {
        Ok(Ok(Outcome::Done(output))) => Ok(output),
        Ok(Ok(Outcome::Waiting(waiter))) => {
            waiting.push(Waiting {
                call,
                waiter,
                tickets,
                reported: (Instant::now(), 0.0),
            });
            return;
        }
        Ok(Err(error)) => Err(error),
        Err(panic) => Err(crashed(&call.name, &panic)),
    };
    // Whatever it started and didn't wait for.
    cancel(app, &tickets);
    if result.is_ok() && changes_things(&call.name) {
        *status.last_client = call.client.clone();
        let history = app.history();
        *status.last_action = Some(if history.generation() != generation {
            let label = history.undo_label().unwrap_or(&call.name);
            label.strip_prefix(LLM_PREFIX).unwrap_or(label).to_owned()
        } else {
            call.name.clone()
        });
    }
    call.answer(tools::tool_result(&result));
}

/// A tool that panicked, as an error the model can read. Canopy stays up;
/// an edit that panicked was rolled back by the host.
fn crashed(tool: &str, panic: &Box<dyn std::any::Any + Send>) -> ToolError {
    let why = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned());
    ToolError(format!(
        "canopy hit a bug running {tool} ({why}); check with get_project before going on"
    ))
}
