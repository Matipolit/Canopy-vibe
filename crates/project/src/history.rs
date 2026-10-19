use crate::{EditError, Project};

/// Undo and redo by keeping whole copies of the project.
///
/// Audio lives outside the project, so a copy is just the tracks, clips and
/// notes. That's small, and copying is always correct, which no hand-written
/// inverse edit can promise. Make it smarter only if profiling says so.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// Counts recorded steps, ever. Lets callers tell whether an edit did
    /// anything even once old steps start being forgotten.
    generation: u64,
}

#[derive(Debug)]
struct Entry {
    label: String,
    project: Project,
}

/// Undo steps kept. Older ones are forgotten.
const MAX_STEPS: usize = 200;

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
        self.undo.push(Entry {
            label: label.to_owned(),
            project: before,
        });
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.generation += 1;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Steps back. Returns the label of the undone edit.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let entry = self.undo.pop()?;
        Some(Self::swap(project, entry, &mut self.redo))
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let entry = self.redo.pop()?;
        Some(Self::swap(project, entry, &mut self.undo))
    }

    fn swap(project: &mut Project, entry: Entry, other: &mut Vec<Entry>) -> String {
        // Ids are never handed out twice, even across undo, so anything that
        // still remembers an undone track or clip can't mistake a new one for it.
        let next_id = project.next_id.max(entry.project.next_id);
        let previous = std::mem::replace(project, entry.project);
        project.next_id = next_id;
        other.push(Entry {
            label: entry.label.clone(),
            project: previous,
        });
        entry.label
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
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
}
