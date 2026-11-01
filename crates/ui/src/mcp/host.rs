//! The app as the tools see it: `mcp::host::Host` over the session, the
//! timeline's selection and the piano roll's, for one frame's calls.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ::mcp::host::{
    Apply, ClipProcess, Host, PluginEntry, Poll, PresetEntry, RenderRequest, Selection, SlicerInfo,
    SlicerOp, Target, Ticket, Transport,
};
use midi::Tick;
use project::{ClipId, ClipRef, EditError, History, NoteId, Project, TrackId, TrackKind};
use serde_json::{Value, json};

use super::renders::Renders;
use crate::session::{PresetTarget, Session, Unchoppable};
use crate::slicers::FileState;

const SAMPLER: &str = "canopy.sampler";
const SLICER: &str = "canopy.slicer";

pub struct AppHost<'a> {
    pub session: &'a mut Session,
    /// The timeline's selection.
    pub selection: &'a crate::timeline::Selection,
    /// The piano roll's selected notes, of `selection.open_clip`.
    pub notes: &'a [NoteId],
    /// A dialog is open (export, FL import, unsaved changes): edits wait.
    pub modal: bool,
    /// Where a seek this frame put the playhead, which the engine reports
    /// only once it has played on from there.
    sought: Option<Tick>,
    /// Where renders for analysis are made; `None` renders nothing (the
    /// pump lends the server's).
    pub renders: Option<&'a mut Renders>,
    /// Renders started since [`AppHost::take_started`] was last called,
    /// so the pump knows which to cancel with their call.
    started: Vec<Ticket>,
}

impl<'a> AppHost<'a> {
    pub fn new(
        session: &'a mut Session,
        selection: &'a crate::timeline::Selection,
        notes: &'a [NoteId],
        modal: bool,
    ) -> AppHost<'a> {
        AppHost {
            session,
            selection,
            notes,
            modal,
            sought: None,
            renders: None,
            started: Vec::new(),
        }
    }

    /// The renders started since this was last called.
    pub fn take_started(&mut self) -> Vec<Ticket> {
        std::mem::take(&mut self.started)
    }

    /// How far along render `ticket` is, `0..=1`, while it is one.
    pub fn progress(&self, ticket: Ticket) -> Option<f32> {
        self.renders.as_deref()?.progress(ticket)
    }

    /// Runs one of the session's own operations, which report failures in
    /// the status bar rather than return them: the status bar's new error,
    /// if `run` gave nothing back.
    fn reported<T>(&mut self, run: impl FnOnce(&mut Session) -> Option<T>) -> Result<T, String> {
        let before = self.session.message.as_ref().map(|m| m.at);
        match run(self.session) {
            Some(value) => Ok(value),
            None => Err(self
                .new_error(before)
                .unwrap_or_else(|| "nothing changed".to_owned())),
        }
    }

    /// The status bar's error, if one was set since `before`.
    fn new_error(&self, before: Option<Instant>) -> Option<String> {
        self.session
            .message
            .as_ref()
            .filter(|m| m.error && Some(m.at) != before)
            .map(|m| m.text.clone())
    }

    /// The instrument track `track`, or why it isn't one.
    fn instrument_track(&self, track: TrackId) -> Result<&project::Track, String> {
        let t =
            self.session.project.track(track).ok_or_else(|| {
                format!("track {track} doesn't exist; get_project lists the tracks")
            })?;
        match t.kind {
            TrackKind::Midi { .. } => Ok(t),
            _ => Err(format!(
                "track {track} ({:?}) has no instrument: it isn't a MIDI track",
                t.name
            )),
        }
    }

    /// The Slicer track `track`, or why it isn't one.
    fn slicer_track(&self, track: TrackId) -> Result<&project::Track, String> {
        let t = self.instrument_track(track)?;
        if plays(t, SLICER) {
            Ok(t)
        } else {
            Err(format!(
                "track {track} ({:?}) doesn't play the Slicer",
                t.name
            ))
        }
    }

    /// A Slicer track's loaded loop: its frames and rate.
    fn slicer_audio(&self, track: TrackId) -> Result<(u64, u32), String> {
        self.slicer_track(track)?;
        match self.session.slicer_file(track) {
            Some(FileState::Ready(source)) => Ok((source.native_frames, source.native_rate)),
            Some(FileState::Loading) => Err("the Slicer's audio is still loading".to_owned()),
            Some(FileState::Failed(err)) => Err(format!("the Slicer's audio didn't load: {err}")),
            None => Err("the Slicer has no audio file; give it one first".to_owned()),
        }
    }

    /// Checks `target` exists and can take presets.
    fn preset_target(&self, target: Target) -> Result<PresetTarget, String> {
        match target {
            Target::Device(id) => {
                if self.session.project.device(id).is_none() {
                    return Err(format!(
                        "device {id} doesn't exist; get_track lists a track's devices"
                    ));
                }
                Ok(PresetTarget::Device(id))
            }
            Target::Instrument(track) => {
                self.instrument_track(track)?;
                Ok(PresetTarget::Instrument(track))
            }
        }
    }

    /// Checks every clip in `clips` exists and is audio (or MIDI, with
    /// `midi`).
    fn check_clips(&self, clips: &[ClipId], midi: bool) -> Result<(), String> {
        if clips.is_empty() {
            return Err("no clips given".to_owned());
        }
        for &id in clips {
            match self.session.project.clip(id) {
                None => {
                    return Err(format!(
                        "clip {id} doesn't exist; get_track lists a track's clips"
                    ));
                }
                Some((_, ClipRef::Audio(_))) if midi => {
                    return Err(format!("clip {id} is an audio clip; this needs MIDI clips"));
                }
                Some((_, ClipRef::Midi(_))) if !midi => {
                    return Err(format!("clip {id} is a MIDI clip; this needs audio clips"));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// Whether `track` plays built-in instrument `id`.
fn plays(track: &project::Track, id: &str) -> bool {
    matches!(
        &track.kind,
        TrackKind::Midi { instrument, .. }
            if instrument.format == instruments::Format::BuiltIn && instrument.id == id
    )
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

impl Host for AppHost<'_> {
    fn project(&self) -> &Project {
        &self.session.project
    }

    fn edit(&mut self, label: &str, apply: Apply<'_>) -> Result<Value, EditError> {
        // An edit that panics halfway mustn't leave half of itself behind.
        let before = self.session.project.clone();
        let session = &mut *self.session;
        match panic::catch_unwind(AssertUnwindSafe(|| session.try_edit(label, apply))) {
            Ok(result) => result,
            Err(panic) => {
                self.session.project = before;
                self.session.request_rebuild();
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Err(EditError::Invalid(format!(
                    "canopy hit a bug making this edit ({why}); nothing was changed"
                )))
            }
        }
    }

    fn history(&self) -> &History {
        self.session.history()
    }

    fn undo(&mut self) -> Option<String> {
        let label = self.session.undo_label()?.to_owned();
        self.session.undo();
        Some(label)
    }

    fn redo(&mut self) -> Option<String> {
        let label = self.session.redo_label()?.to_owned();
        self.session.redo();
        Some(label)
    }

    fn jump(&mut self, position: usize) {
        self.session.jump_to(position);
    }

    fn selection(&self) -> Selection {
        let s = self.selection;
        Selection {
            tracks: s.tracks.clone(),
            clips: s.clips.clone(),
            open_clip: s.open_clip,
            notes: if s.open_clip.is_some() {
                self.notes.to_vec()
            } else {
                Vec::new()
            },
            master: s.master,
            devices: s.devices.clone(),
        }
    }

    fn transport(&self) -> Transport {
        Transport {
            playing: self.session.is_playing(),
            playhead: self
                .sought
                .unwrap_or_else(|| Tick::new(self.session.playhead().round() as i64)),
        }
    }

    fn set_playing(&mut self, playing: bool) {
        self.session.set_playing(playing);
    }

    fn seek(&mut self, at: Tick) {
        self.session.seek(at);
        self.sought = Some(at);
    }

    fn project_name(&self) -> Option<String> {
        let path = self.session.path.as_ref()?;
        Some(path.file_stem()?.to_string_lossy().into_owned())
    }

    fn plugins(&self) -> Vec<PluginEntry> {
        self.session
            .plugins
            .manager
            .catalog()
            .plugins()
            .iter()
            .filter(|p| hosting::usable(p))
            .map(|p| PluginEntry {
                name: p.name.clone(),
                vendor: p.vendor.clone(),
                format: p.format.name().to_owned(),
                instrument: p.kind == ::plugins::PluginKind::Instrument,
                id: p.id.clone(),
            })
            .collect()
    }

    fn presets(&self, target: Target) -> Vec<PresetEntry> {
        let Ok(target) = self.preset_target(target) else {
            return Vec::new();
        };
        let factory = self
            .session
            .factory_presets(target)
            .into_iter()
            .map(|(name, _)| PresetEntry {
                name: name.to_owned(),
                factory: true,
            });
        let saved = self
            .session
            .presets(target)
            .into_iter()
            .map(|p| PresetEntry {
                name: p.name,
                factory: false,
            });
        factory.chain(saved).collect()
    }

    fn load_preset(&mut self, target: Target, name: &str) -> Result<(), String> {
        let target = self.preset_target(target)?;
        let factory = self.session.factory_presets(target);
        let saved = self.session.presets(target);
        // The exact name first, then any case.
        let pick = |same: &dyn Fn(&str) -> bool| {
            factory
                .iter()
                .position(|(n, _)| same(n))
                .map(Ok)
                .or_else(|| saved.iter().position(|p| same(&p.name)).map(Err))
        };
        let found = pick(&|n| n == name).or_else(|| pick(&|n| n.eq_ignore_ascii_case(name)));
        let before = self.session.message.as_ref().map(|m| m.at);
        match found {
            Some(Ok(i)) => {
                let values = factory[i].1.clone();
                self.session.load_factory_preset(target, values);
            }
            Some(Err(i)) => self.session.load_preset(target, &saved[i]),
            None => {
                let names: Vec<&str> = factory
                    .iter()
                    .map(|(n, _)| *n)
                    .chain(saved.iter().map(|p| p.name.as_str()))
                    .collect();
                return Err(if names.is_empty() {
                    "there are no presets for this device".to_owned()
                } else {
                    format!(
                        "there's no preset called {name:?}; there are: {}",
                        names.join(", ")
                    )
                });
            }
        }
        self.new_error(before).map_or(Ok(()), Err)
    }

    fn slicer(&self, track: TrackId) -> Option<SlicerInfo> {
        let t = self.slicer_track(track).ok()?;
        let file = t.instrument_file()?.to_owned();
        let slices = t.slices().to_vec();
        let (frames, rate) = self.slicer_audio(track).ok()?;
        Some(SlicerInfo {
            file,
            rate,
            frames,
            slices,
        })
    }

    fn slicer_op(&mut self, track: TrackId, op: SlicerOp) -> Result<Value, String> {
        let (frames, rate) = self.slicer_audio(track)?;
        let count = |host: &AppHost<'_>| {
            host.session
                .project
                .track(track)
                .map_or(0, |t| t.slices().len())
        };
        match op {
            SlicerOp::Set(starts) => {
                if let Some(&bad) = starts.iter().find(|&&s| s >= frames) {
                    return Err(format!(
                        "slices start at frames 0–{} of the {rate} Hz file; {bad} is past its end",
                        frames.saturating_sub(1)
                    ));
                }
                self.session
                    .try_edit("Set Slices", |p| p.set_slices(track, starts))
                    .map_err(|e| e.to_string())?;
            }
            SlicerOp::Transients { sensitivity } => {
                if !(0.0..=1.0).contains(&sensitivity) {
                    return Err(format!("sensitivity is 0–1; {sensitivity} is out of range"));
                }
                self.session.slice_at_transients(track, sensitivity);
            }
            SlicerOp::Grid { division } => {
                if !(1..=64).contains(&division) {
                    return Err(format!(
                        "division is 1–64 a bar; {division} is out of range"
                    ));
                }
                self.session.slice_by_grid(track, division);
            }
            SlicerOp::ToPianoRoll { at } => {
                let clip = self.reported(|s| s.slices_to_piano_roll(track, at))?;
                let notes = match self.session.project.clip(clip) {
                    Some((_, ClipRef::Midi(m))) => m.notes.len(),
                    _ => 0,
                };
                return Ok(json!({
                    "id": clip.0,
                    "summary": format!("clip {clip} plays the {notes} slices in order"),
                }));
            }
        }
        let n = count(self);
        Ok(json!({ "slices": n, "summary": format!("{n} slices") }))
    }

    fn set_instrument_file(&mut self, track: TrackId, path: PathBuf) -> Result<(), String> {
        let t = self.instrument_track(track)?;
        let (sampler, slicer) = (plays(t, SAMPLER), plays(t, SLICER));
        if !path.is_file() {
            return Err(format!("{} isn't a file", path.display()));
        }
        if sampler {
            if !has_extension(&path, "sfz") {
                return Err("the Sampler plays .sfz files".to_owned());
            }
            self.session
                .try_edit("Load Instrument", |p| {
                    p.set_instrument_file(track, Some(path))
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        } else if slicer {
            if !media::is_supported_extension(&path) {
                return Err(format!(
                    "{}: not an audio file canopy can read",
                    path.display()
                ));
            }
            // Decoded off the UI thread; the track takes it once loaded.
            self.session
                .load_slicer(path, Some(track), project::TrackPosition::END);
            Ok(())
        } else {
            Err(format!(
                "track {track} ({:?}) plays an instrument that doesn't play files; \
                 the Sampler and the Slicer do",
                t.name
            ))
        }
    }

    fn instrument_file_loaded(&self, track: TrackId, path: &Path) -> Option<Result<(), String>> {
        let t = self.instrument_track(track);
        match t {
            Err(why) => Some(Err(why)),
            Ok(t) if plays(t, SLICER) => self.session.slicer_loaded(track, path),
            // A Sampler's file is the track's as soon as it is given.
            Ok(_) => Some(Ok(())),
        }
    }

    fn import_audio(
        &mut self,
        path: PathBuf,
        track: Option<TrackId>,
        start: Tick,
    ) -> Result<Ticket, String> {
        if let Some(track) = track
            && !matches!(
                self.session.project.track(track).map(|t| &t.kind),
                Some(TrackKind::Audio { .. })
            )
        {
            return Err(format!("track {track} isn't an audio track"));
        }
        if !path.is_file() {
            return Err(format!("{} isn't a file", path.display()));
        }
        self.session.import_file_for(path, track, start).map(Ticket)
    }

    fn process_clips(&mut self, op: ClipProcess, clips: &[ClipId]) -> Result<Vec<ClipId>, String> {
        match op {
            ClipProcess::ChopTransients { sensitivity } => {
                self.check_clips(clips, false)?;
                if !(0.0..=1.0).contains(&sensitivity) {
                    return Err(format!("sensitivity is 0–1; {sensitivity} is out of range"));
                }
                let mut cuts = Vec::new();
                for &id in clips {
                    match self.session.transient_cuts(id, sensitivity) {
                        Ok(at) => cuts.push((id, at)),
                        Err(Unchoppable::Loading) => {
                            return Err(format!("clip {id}'s audio is still loading"));
                        }
                        Err(Unchoppable::NotAudio) => {
                            return Err(format!("clip {id} isn't an audio clip"));
                        }
                    }
                }
                if cuts.iter().all(|(_, at)| at.is_empty()) {
                    return Err("no transients found; a higher sensitivity finds more".to_owned());
                }
                self.session
                    .try_edit("Chop", |p| {
                        let mut made = Vec::new();
                        for (id, at) in cuts {
                            if at.is_empty() {
                                made.push(id);
                            } else {
                                made.extend(p.chop_clip(id, &at)?);
                            }
                        }
                        Ok(made)
                    })
                    .map_err(|e| e.to_string())
            }
            ClipProcess::Consolidate => {
                self.check_clips(clips, false)?;
                let track = |id| self.session.project.clip(id).map(|(t, _)| t);
                if clips.iter().any(|&c| track(c) != track(clips[0])) {
                    return Err("consolidated clips must all be on one track".to_owned());
                }
                self.reported(|s| s.consolidate_clips(clips))
                    .map(|clip| vec![clip])
            }
            ClipProcess::Normalize => {
                self.check_clips(clips, false)?;
                let before = self.session.message.as_ref().map(|m| m.at);
                self.session.normalize_clips(clips);
                self.new_error(before).map_or(Ok(clips.to_vec()), Err)
            }
            ClipProcess::Freeze => {
                self.check_clips(clips, true)?;
                // One undo step a clip, as freezing them by hand makes.
                let mut made = Vec::new();
                for &clip in clips {
                    made.push(self.reported(|s| s.freeze_clip(clip))?);
                }
                Ok(made)
            }
        }
    }

    fn render(&mut self, request: RenderRequest) -> Result<Ticket, String> {
        let renders = self
            .renders
            .as_deref_mut()
            .ok_or("canopy can't render for analysis here")?;
        let ticket = renders.request(self.session, request)?;
        self.started.push(ticket);
        Ok(ticket)
    }

    fn poll(&mut self, ticket: Ticket) -> Poll {
        if Renders::owns(ticket) {
            let current = self.session.revision();
            return match self.renders.as_deref_mut() {
                Some(renders) => renders.poll(ticket, current),
                None => Poll::Rendered(Err("canopy has no such render".to_owned())),
            };
        }
        match self.session.import_result(ticket.0) {
            None => Poll::Waiting { progress: 0.0 },
            Some(result) => Poll::Imported(result),
        }
    }

    /// Stops a render no one waits for any more. An import, once started,
    /// finishes: its file becomes a clip the user can see and undo.
    fn cancel(&mut self, ticket: Ticket) {
        if Renders::owns(ticket)
            && let Some(renders) = self.renders.as_deref_mut()
        {
            renders.cancel(ticket);
        }
    }
}
