//! A [`Host`] for tests: a real project and history, and everything else
//! scripted or recorded, so a test can check what a tool asked the app for.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use midi::Tick;
use project::{ClipId, EditError, History, Project, TrackId};
use serde_json::Value;

use crate::host::{
    Apply, ClipProcess, Host, PluginEntry, Poll, PresetEntry, RenderRequest, Selection, SlicerInfo,
    SlicerOp, Target, Ticket, Transport,
};

pub struct FakeHost {
    pub project: Project,
    pub history: History,
    pub selection: Selection,
    pub transport: Transport,
    pub name: Option<String>,
    pub plugins: Vec<PluginEntry>,
    /// Offered for every target.
    pub presets: Vec<PresetEntry>,
    pub slicers: HashMap<TrackId, SlicerInfo>,
    /// What the tools asked of the transport, in order: "play", "stop",
    /// "seek 1920".
    pub transport_calls: Vec<String>,
    pub loaded_presets: Vec<(Target, String)>,
    pub slicer_ops: Vec<(TrackId, SlicerOp)>,
    pub instrument_files: Vec<(TrackId, PathBuf)>,
    /// Files given to instruments that are still loading.
    pub loading_files: Vec<PathBuf>,
    pub imports: Vec<(PathBuf, Option<TrackId>, Tick)>,
    pub processed: Vec<(ClipProcess, Vec<ClipId>)>,
    pub renders: Vec<RenderRequest>,
    pub cancelled: Vec<Ticket>,
    /// Answers to [`Host::poll`] by ticket, in order; a ticket with none
    /// left is still waiting.
    pub polls: HashMap<Ticket, VecDeque<Poll>>,
    next_ticket: u64,
}

impl FakeHost {
    pub fn new(project: Project) -> FakeHost {
        FakeHost {
            project,
            history: History::default(),
            selection: Selection::default(),
            transport: Transport::default(),
            name: None,
            plugins: Vec::new(),
            presets: Vec::new(),
            slicers: HashMap::new(),
            transport_calls: Vec::new(),
            loaded_presets: Vec::new(),
            slicer_ops: Vec::new(),
            instrument_files: Vec::new(),
            loading_files: Vec::new(),
            imports: Vec::new(),
            processed: Vec::new(),
            renders: Vec::new(),
            cancelled: Vec::new(),
            polls: HashMap::new(),
            next_ticket: 1,
        }
    }

    /// Queues what [`Host::poll`] answers for `ticket`.
    pub fn script(&mut self, ticket: Ticket, answers: impl IntoIterator<Item = Poll>) {
        self.polls.entry(ticket).or_default().extend(answers);
    }

    /// The ticket the next job will get.
    pub fn next_ticket(&self) -> Ticket {
        Ticket(self.next_ticket)
    }

    fn ticket(&mut self) -> Ticket {
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        ticket
    }
}

impl Host for FakeHost {
    fn project(&self) -> &Project {
        &self.project
    }

    fn edit(&mut self, label: &str, apply: Apply<'_>) -> Result<Value, EditError> {
        self.history.edit(&mut self.project, label, apply)
    }

    fn history(&self) -> &History {
        &self.history
    }

    fn undo(&mut self) -> Option<String> {
        self.history.undo(&mut self.project)
    }

    fn redo(&mut self) -> Option<String> {
        self.history.redo(&mut self.project)
    }

    fn jump(&mut self, position: usize) {
        self.history.jump(&mut self.project, position);
    }

    fn selection(&self) -> Selection {
        self.selection.clone()
    }

    fn transport(&self) -> Transport {
        self.transport
    }

    fn set_playing(&mut self, playing: bool) {
        self.transport_calls
            .push(if playing { "play" } else { "stop" }.to_owned());
        self.transport.playing = playing;
    }

    fn seek(&mut self, at: Tick) {
        self.transport_calls.push(format!("seek {}", at.get()));
        self.transport.playhead = at;
    }

    fn project_name(&self) -> Option<String> {
        self.name.clone()
    }

    fn plugins(&self) -> Vec<PluginEntry> {
        self.plugins.clone()
    }

    fn presets(&self, _target: Target) -> Vec<PresetEntry> {
        self.presets.clone()
    }

    fn load_preset(&mut self, target: Target, name: &str) -> Result<(), String> {
        if !self.presets.iter().any(|p| p.name == name) {
            return Err(format!("there's no preset called {name:?}"));
        }
        self.loaded_presets.push((target, name.to_owned()));
        Ok(())
    }

    fn slicer(&self, track: TrackId) -> Option<SlicerInfo> {
        self.slicers.get(&track).cloned()
    }

    fn slicer_op(&mut self, track: TrackId, op: SlicerOp) -> Result<Value, String> {
        self.slicer_ops.push((track, op));
        Ok(Value::Null)
    }

    fn set_instrument_file(&mut self, track: TrackId, path: PathBuf) -> Result<(), String> {
        self.instrument_files.push((track, path));
        Ok(())
    }

    fn instrument_file_loaded(&self, _track: TrackId, path: &Path) -> Option<Result<(), String>> {
        (!self.loading_files.iter().any(|f| f == path)).then_some(Ok(()))
    }

    fn import_audio(
        &mut self,
        path: PathBuf,
        track: Option<TrackId>,
        start: Tick,
    ) -> Result<Ticket, String> {
        self.imports.push((path, track, start));
        Ok(self.ticket())
    }

    fn process_clips(&mut self, op: ClipProcess, clips: &[ClipId]) -> Result<Vec<ClipId>, String> {
        self.processed.push((op, clips.to_vec()));
        Ok(Vec::new())
    }

    fn render(&mut self, request: RenderRequest) -> Result<Ticket, String> {
        self.renders.push(request);
        Ok(self.ticket())
    }

    fn poll(&mut self, ticket: Ticket) -> Poll {
        self.polls
            .get_mut(&ticket)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Poll::Waiting { progress: 0.0 })
    }

    fn cancel(&mut self, ticket: Ticket) {
        self.cancelled.push(ticket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_polls_answer_in_order_then_wait() {
        let mut host = FakeHost::new(Project::new());
        let ticket = host.next_ticket();
        host.script(
            ticket,
            [
                Poll::Waiting { progress: 0.5 },
                Poll::Imported(Ok(ClipId(7))),
            ],
        );
        assert_eq!(
            host.import_audio("a.wav".into(), None, Tick::ZERO),
            Ok(ticket)
        );
        assert!(matches!(host.poll(ticket), Poll::Waiting { progress } if progress == 0.5));
        assert!(matches!(host.poll(ticket), Poll::Imported(Ok(ClipId(7)))));
        assert!(matches!(host.poll(ticket), Poll::Waiting { .. }));
    }
}
