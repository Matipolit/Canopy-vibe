use std::collections::VecDeque;
use std::time::SystemTime;

use crate::{EditError, Project};

/// Undo and redo by keeping whole copies of the project.
///
/// Audio lives outside the project, so a copy is just the tracks, clips and
/// notes. That's small, and copying is always correct, which no hand-written
/// inverse edit can promise. Make it smarter only if profiling says so.
///
/// Positions count steps done: 0 is the oldest state kept, and
/// [`History::position`] is the current one. [`History::jump`] goes to any of
/// them.
#[derive(Debug, Default)]
pub struct History {
    /// Oldest first; the back is the next to undo.
    undo: VecDeque<Entry>,
    /// The back is the next to redo.
    redo: Vec<Entry>,
    /// Counts recorded steps, ever. Lets callers tell whether an edit did
    /// anything even once old steps start being forgotten.
    generation: u64,
    /// Whether steps beyond [`MAX_STEPS`] were dropped since the last
    /// [`History::clear`], so position 0 isn't where the project started.
    forgot: bool,
}

#[derive(Debug)]
struct Entry {
    label: String,
    /// When the step was made.
    at: SystemTime,
    /// The project on the other side of the step: before it while it can be
    /// undone, after it while it can be redone.
    project: Project,
}

/// One step of [`History::steps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step<'a> {
    pub label: &'a str,
    pub at: SystemTime,
}

/// Undo steps kept. Older ones are forgotten. A step keeps only what its
/// edit changed (see [`crate::Shared`]): a thousand one-clip edits of a
/// 33-track song hold a few tens of MB.
pub const MAX_STEPS: usize = 1000;

impl History {
    /// Runs `edit` on `project` as one undo step named `label`. On error the
    /// project is restored, so edits don't need to be all-or-nothing
    /// themselves. An edit that changes nothing leaves no undo step.
    pub fn edit<T>(
        &mut self,
        project: &mut Project,
        label: &str,
        edit: impl FnOnce(&mut Project) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let before = project.clone();
        match edit(project) {
            Ok(value) => {
                if *project != before {
                    self.record(label, before);
                }
                Ok(value)
            }
            Err(err) => {
                *project = before;
                Err(err)
            }
        }
    }

    /// Records `before` as the state to return to, for edits applied by hand
    /// (e.g. ones previewed live while dragging).
    pub fn record(&mut self, label: &str, before: Project) {
        self.undo.push_back(Entry {
            label: label.to_owned(),
            at: SystemTime::now(),
            project: before,
        });
        if self.undo.len() > MAX_STEPS {
            self.undo.pop_front();
            self.forgot = true;
        }
        self.redo.clear();
        self.generation += 1;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Steps back. Returns the label of the undone edit.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let entry = self.undo.pop_back()?;
        let (entry, label) = Self::swap(project, entry);
        self.redo.push(entry);
        Some(label)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let entry = self.redo.pop()?;
        let (entry, label) = Self::swap(project, entry);
        self.undo.push_back(entry);
        Some(label)
    }

    /// Puts `entry`'s project in place and returns the entry to keep on the
    /// other stack, holding the project it replaced.
    fn swap(project: &mut Project, entry: Entry) -> (Entry, String) {
        // Ids are never handed out twice, even across undo, so anything that
        // still remembers an undone track or clip can't mistake a new one for it.
        let next_id = project.next_id.max(entry.project.next_id);
        let previous = std::mem::replace(project, entry.project);
        project.next_id = next_id;
        let label = entry.label;
        let kept = Entry {
            label: label.clone(),
            at: entry.at,
            project: previous,
        };
        (kept, label)
    }

    /// Undoes or redoes until `position` steps are done (clamped to the steps
    /// there are). Returns how many steps it moved; each can be undone or
    /// redone again on its own, so a jump loses nothing.
    pub fn jump(&mut self, project: &mut Project, position: usize) -> usize {
        let mut moved = 0;
        while self.undo.len() > position && self.undo(project).is_some() {
            moved += 1;
        }
        while self.undo.len() < position && self.redo(project).is_some() {
            moved += 1;
        }
        moved
    }

    /// Every step kept, in the order they were made: the first
    /// [`History::position`] are done, the rest undone.
    pub fn steps(&self) -> impl Iterator<Item = Step<'_>> {
        fn step(e: &Entry) -> Step<'_> {
            Step {
                label: &e.label,
                at: e.at,
            }
        }
        self.undo
            .iter()
            .map(step)
            .chain(self.redo.iter().rev().map(step))
    }

    /// How many of [`History::steps`] are done.
    pub fn position(&self) -> usize {
        self.undo.len()
    }

    /// Whether the oldest steps were forgotten (more than [`MAX_STEPS`]).
    pub fn forgot_steps(&self) -> bool {
        self.forgot
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.forgot = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewTrack, TrackPosition};

    #[test]
    fn undo_redo_and_failed_edits() {
        let mut project = Project::new();
        let mut history = History::default();
        let id = history
            .edit(&mut project, "Add Track", |p| {
                p.add_track(NewTrack::Midi, TrackPosition::END)
            })
            .unwrap();
        history
            .edit(&mut project, "Rename", |p| p.rename_track(id, "Bass"))
            .unwrap();

        // A failing edit rolls back what it already did and adds no step.
        let failed = history.edit(&mut project, "Broken", |p| {
            p.rename_track(id, "half done")?;
            p.ungroup(id)
        });
        assert!(failed.is_err());
        assert_eq!(project.track(id).unwrap().name, "Bass");
        assert_eq!(history.undo_label(), Some("Rename"));

        // No-op edits leave no step either.
        history
            .edit(&mut project, "Rename", |p| p.rename_track(id, "Bass"))
            .unwrap();

        assert_eq!(history.undo(&mut project).as_deref(), Some("Rename"));
        assert_eq!(project.track(id).unwrap().name, "1 MIDI");
        assert_eq!(history.undo(&mut project).as_deref(), Some("Add Track"));
        assert!(project.tracks.is_empty());
        assert_eq!(history.undo(&mut project), None);
        assert_eq!(history.redo(&mut project).as_deref(), Some("Add Track"));
        assert_eq!(history.redo(&mut project).as_deref(), Some("Rename"));
        assert_eq!(project.track(id).unwrap().name, "Bass");
    }

    #[test]
    fn ids_are_not_reused_after_undo() {
        let mut project = Project::new();
        let mut history = History::default();
        let first = history
            .edit(&mut project, "Add", |p| {
                p.add_track(NewTrack::Audio, TrackPosition::END)
            })
            .unwrap();
        history.undo(&mut project);
        let second = history
            .edit(&mut project, "Add", |p| {
                p.add_track(NewTrack::Audio, TrackPosition::END)
            })
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(history.redo_label(), None, "a new edit clears redo");
    }

    /// A project and its history after renaming one track to each of `names`.
    fn renamed(names: &[&str]) -> (Project, History, crate::TrackId) {
        let mut project = Project::new();
        let id = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let mut history = History::default();
        for name in names {
            history
                .edit(&mut project, &format!("Rename {name}"), |p| {
                    p.rename_track(id, name)
                })
                .unwrap();
        }
        (project, history, id)
    }

    fn labels(history: &History) -> Vec<&str> {
        history.steps().map(|s| s.label).collect()
    }

    #[test]
    fn steps_list_done_then_undone_in_the_order_they_were_made() {
        let (mut project, mut history, _) = renamed(&["a", "b", "c"]);
        history.undo(&mut project);
        assert_eq!(labels(&history), ["Rename a", "Rename b", "Rename c"]);
        assert_eq!(history.position(), 2);
        assert!(history.steps().all(|s| s.at <= SystemTime::now()));
    }

    #[test]
    fn jumping_back_and_forth_reaches_every_state() {
        let (mut project, mut history, id) = renamed(&["a", "b", "c", "d"]);
        let name = |p: &Project| p.track(id).unwrap().name.clone();

        assert_eq!(history.jump(&mut project, 1), 3);
        assert_eq!(name(&project), "a");
        assert_eq!(history.position(), 1);
        assert_eq!(history.redo_label(), Some("Rename b"));

        assert_eq!(history.jump(&mut project, 0), 1);
        assert_eq!(name(&project), "1 MIDI");

        assert_eq!(history.jump(&mut project, 3), 3);
        assert_eq!(name(&project), "c");
        // Past the end stops at the latest step.
        assert_eq!(history.jump(&mut project, 99), 1);
        assert_eq!(name(&project), "d");
        assert_eq!(history.jump(&mut project, 4), 0);
        assert_eq!(labels(&history).len(), 4, "jumping loses no steps");
    }

    #[test]
    fn an_edit_after_a_jump_replaces_the_undone_steps() {
        let (mut project, mut history, id) = renamed(&["a", "b", "c"]);
        history.jump(&mut project, 1);
        history
            .edit(&mut project, "Rename z", |p| p.rename_track(id, "z"))
            .unwrap();
        assert_eq!(labels(&history), ["Rename a", "Rename z"]);
        assert_eq!(history.position(), 2);
    }

    #[test]
    fn ids_are_not_reused_after_a_jump() {
        let mut project = Project::new();
        let mut history = History::default();
        let add = |project: &mut Project, history: &mut History| {
            history
                .edit(project, "Add", |p| {
                    p.add_track(NewTrack::Audio, TrackPosition::END)
                })
                .unwrap()
        };
        let first = add(&mut project, &mut history);
        add(&mut project, &mut history);
        history.jump(&mut project, 0);
        let again = add(&mut project, &mut history);
        assert_ne!(first, again);
    }

    #[test]
    fn the_oldest_steps_are_forgotten_beyond_the_limit() {
        let names: Vec<String> = (0..MAX_STEPS + 3).map(|i| i.to_string()).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (_, history, _) = renamed(&names);
        assert_eq!(history.steps().count(), MAX_STEPS);
        assert_eq!(history.steps().next().unwrap().label, "Rename 3");
        assert!(history.forgot_steps());
        assert!(!renamed(&["a"]).1.forgot_steps());
    }
}
