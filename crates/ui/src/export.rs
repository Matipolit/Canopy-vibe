//! Exporting the mix, or tracks one by one, to WAV files. Everything after
//! gathering the audio runs on a worker thread, so the UI stays responsive.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use engine::{Arrangement, AudioLookup, Bounce, Compiler};
use hosting::{Offline, PluginManager, WithPlugins};
use media::{AudioBuffer, WavFormat};
use project::{AudioClip, ClipId, Mixer, Project, SourceId, Track, TrackId, TrackKind};

use crate::widgets::chrome;

/// What one file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Everything, through the master.
    Mix,
    /// One track (a group: with everything in it), through its own fader
    /// and its groups' but not the master's, as if soloed.
    Track(TrackId),
}

#[derive(Debug, Clone)]
pub struct Job {
    pub target: Target,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub format: WavFormat,
    /// Keep going past the end until reverb and release tails die away.
    pub tail: bool,
    /// Just the loop region, rather than the whole song.
    pub loop_only: bool,
}

/// The audio an export plays: decoded sources, and the clips' current
/// renders. Clips missing one are rendered by the export itself.
#[derive(Default)]
pub struct Audio {
    pub sources: HashMap<SourceId, Arc<AudioBuffer>>,
    pub renders: HashMap<ClipId, Arc<AudioBuffer>>,
    /// Instruments' loaded files (samplers' banks, Wavetables' user
    /// tables), by what they were loaded from.
    pub banks: HashMap<crate::banks::DataKey, instruments::InstrumentData>,
}

/// An export in progress.
pub struct Exporting {
    /// Overall progress, `0..=1`, as `f32` bits.
    progress: Arc<AtomicU32>,
    result: Receiver<Result<Vec<PathBuf>, String>>,
    /// For a project with plugins, which can only be made on the UI
    /// thread: the export thread asks for each job's snapshot here.
    compiles: Option<Compiles>,
    /// The plugins of the job being rendered, kept until the next job's
    /// are made or the export ends, and dropped on the UI thread.
    plugins: Option<Offline>,
}

/// The UI thread's end of the export's requests for snapshots.
struct Compiles {
    audio: Arc<Audio>,
    rate: u32,
    requests: Receiver<Request>,
    replies: Sender<Compiled>,
}

/// One job's project, and renders of its clips that had none, made on
/// the export thread so the UI thread doesn't.
struct Request {
    version: Project,
    renders: HashMap<ClipId, Arc<AudioBuffer>>,
}

/// A job's snapshot and how late its sound is.
type Compiled = (Box<Arrangement>, usize);

impl Exporting {
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// The files written, or what went wrong, once it's over.
    pub fn poll(&self) -> Option<Result<Vec<PathBuf>, String>> {
        self.result.try_recv().ok()
    }

    /// Compiles the snapshots the export thread asked for, with plugin
    /// instances of their own; UI thread, every frame.
    pub fn serve(&mut self, manager: &mut PluginManager) {
        let Some(compiles) = &self.compiles else {
            return;
        };
        while let Ok(request) = compiles.requests.try_recv() {
            // The last job's plugins go first: one job's at a time.
            self.plugins = None;
            let plugins = manager.offline(&request.version);
            let lookup = WithPlugins {
                audio: Lookup {
                    project: &request.version,
                    audio: &compiles.audio,
                    renders: &request.renders,
                    rate: compiles.rate,
                },
                plugins: &plugins,
            };
            let mut compiler = Compiler::new(compiles.rate);
            let arrangement = compiler.compile(&request.version, lookup);
            let _ = compiles.replies.send((arrangement, compiler.latency()));
            self.plugins = Some(plugins);
        }
    }
}

/// Starts exporting `project` as `jobs` describe, at `rate`. A project
/// with plugins has its snapshots compiled on the UI thread (see
/// [`Exporting::serve`]); any other compiles on the export thread.
pub fn start(
    project: Project,
    audio: Audio,
    rate: u32,
    jobs: Vec<Job>,
    settings: Settings,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Exporting {
    let progress = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let (sender, result) = mpsc::channel();
    let shared = progress.clone();
    let audio = Arc::new(audio);
    let (compiles, asker) = if hosting::uses_plugins(&project) {
        let (ask, requests) = mpsc::channel();
        let (replies, answers) = mpsc::channel();
        let compiles = Compiles {
            audio: audio.clone(),
            rate,
            requests,
            replies,
        };
        (Some(compiles), Some((ask, answers)))
    } else {
        (None, None)
    };
    let waker = wake.clone();
    thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let report = |done: f32| {
                shared.store(done.to_bits(), Ordering::Relaxed);
                wake();
            };
            let compile = |version: Project| -> Option<Compiled> {
                match &asker {
                    Some((ask, answers)) => {
                        let renders = render_missing(&version, &audio, rate);
                        ask.send(Request { version, renders }).ok()?;
                        waker();
                        answers.recv().ok()
                    }
                    None => {
                        let renders = HashMap::new();
                        let lookup = Lookup {
                            project: &version,
                            audio: &audio,
                            renders: &renders,
                            rate,
                        };
                        let mut compiler = Compiler::new(rate);
                        let arrangement = compiler.compile(&version, lookup);
                        Some((arrangement, compiler.latency()))
                    }
                }
            };
            let outcome = run(&project, rate, &jobs, settings, &compile, &report);
            let _ = sender.send(outcome);
            wake();
        })
        .expect("spawning the export thread");
    Exporting {
        progress,
        result,
        compiles,
        plugins: None,
    }
}

fn run(
    project: &Project,
    rate: u32,
    jobs: &[Job],
    settings: Settings,
    compile: &dyn Fn(Project) -> Option<Compiled>,
    report: &dyn Fn(f32),
) -> Result<Vec<PathBuf>, String> {
    let sample = |tick| u64::try_from(project.tempo.tick_to_sample(tick, rate)).unwrap_or(0);
    let range = match project.loop_region.filter(|_| settings.loop_only) {
        Some(region) => sample(region.start)..sample(region.end),
        None => 0..sample(project.end()),
    };
    if range.is_empty() {
        return Err("there's nothing to export".into());
    }
    let mut written = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        let mut version = match job.target {
            Target::Mix => project.clone(),
            Target::Track(track) => soloed(project, track),
        };
        // Playback's loop has no place in a file.
        version.loop_region = None;
        let (arrangement, latency) = compile(version).ok_or("the export was cancelled")?;
        let mut bounce = Bounce::range(arrangement, rate, range.clone(), settings.tail, latency);
        // About a tenth of a second at a time, reporting as it goes.
        while !bounce.step(rate as usize / 10) {
            report((index as f32 + bounce.progress()) / jobs.len() as f32);
        }
        let rendered = bounce.finish();
        media::write_wav_as(&job.path, &rendered, settings.format)
            .map_err(|err| format!("{}: {err}", job.path.display()))?;
        written.push(job.path.clone());
    }
    report(1.0);
    Ok(written)
}

/// Renders the clips of `project` that need processing and have no
/// render yet, as the compiler would ask for them.
fn render_missing(
    project: &Project,
    audio: &Audio,
    rate: u32,
) -> HashMap<ClipId, Arc<AudioBuffer>> {
    let mut renders = HashMap::new();
    project.visit(|track, _| {
        let TrackKind::Audio { clips } = &track.kind else {
            return;
        };
        for clip in clips {
            if audio.renders.contains_key(&clip.id) {
                continue;
            }
            let Some(spec) = engine::clip_process(project, clip, rate) else {
                continue;
            };
            if let Some(input) = audio.sources.get(&clip.source) {
                renders.insert(clip.id, Arc::new(media::process_clip(input, &spec)));
            }
        }
    });
    renders
}

/// `project` with only `track` heard: soloed, and unmuted along with the
/// groups it plays through. The master is left out, at unity.
fn soloed(project: &Project, track: TrackId) -> Project {
    /// True if `tracks` holds `target`; unmutes it and its groups on the way.
    fn solo(tracks: &mut [Track], target: TrackId) -> bool {
        let mut found = false;
        for t in tracks {
            t.mixer.solo = t.id == target;
            let inside = t.children_mut().is_some_and(|c| solo(c, target));
            if t.id == target || inside {
                t.mixer.mute = false;
                found = true;
            }
        }
        found
    }
    let mut version = project.clone();
    solo(&mut version.tracks, track);
    version.master = Mixer::default();
    version
}

struct Lookup<'a> {
    project: &'a Project,
    audio: &'a Audio,
    /// Renders made for this job, of clips `audio` has none of.
    renders: &'a HashMap<ClipId, Arc<AudioBuffer>>,
    rate: u32,
}

impl AudioLookup for Lookup<'_> {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.audio.sources.get(&id).cloned()
    }

    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        if let Some(audio) = self
            .audio
            .renders
            .get(&clip.id)
            .or(self.renders.get(&clip.id))
        {
            return Some(audio.clone());
        }
        let spec = engine::clip_process(self.project, clip, self.rate)?;
        let input = self.source(clip.source)?;
        Some(Arc::new(media::process_clip(&input, &spec)))
    }

    fn instrument_data(&self, track: &Track) -> Option<instruments::InstrumentData> {
        let key = crate::banks::DataKey::of(track)?;
        self.audio.banks.get(&key).cloned()
    }
}

/// A file name made of `parts`, without characters file systems dislike.
pub fn file_name(parts: &[&str]) -> String {
    let name: String = parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" - ")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.wav", name.trim())
}

/// What the Export dialog writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Mix,
    /// The tracks selected when it opened, a file each.
    Selected,
    /// Every track that isn't a group, a file each.
    EachTrack,
}

/// The Export Audio window.
pub struct Dialog {
    pub what: What,
    pub selected: Vec<TrackId>,
    pub settings: Settings,
    /// Files that are already there, waiting for the go-ahead to replace them.
    confirm: Option<Vec<Job>>,
}

pub enum Outcome {
    Open,
    Cancel,
    Export(Vec<Job>, Settings),
}

impl Dialog {
    pub fn new(what: What, selected: Vec<TrackId>, settings: Settings) -> Dialog {
        let what = if what == What::Selected && selected.is_empty() {
            What::Mix
        } else {
            what
        };
        Dialog {
            what,
            selected,
            settings,
            confirm: None,
        }
    }

    /// The files to write into `place`: the file itself for the mix, the
    /// folder for tracks. Track files are named after the project and track.
    pub fn jobs(&self, project: &Project, name: &str, place: PathBuf) -> Vec<Job> {
        let tracks: Vec<(TrackId, String)> = match self.what {
            What::Mix => {
                return vec![Job {
                    target: Target::Mix,
                    path: place,
                }];
            }
            What::Selected => {
                let mut tracks = Vec::new();
                project.visit(|t, _| {
                    if self.selected.contains(&t.id) {
                        tracks.push((t.id, t.name.clone()));
                    }
                });
                tracks
            }
            What::EachTrack => {
                let mut tracks = Vec::new();
                project.visit(|t, _| {
                    if !t.is_group() {
                        tracks.push((t.id, t.name.clone()));
                    }
                });
                tracks
            }
        };
        let mut used = std::collections::HashSet::new();
        tracks
            .into_iter()
            .map(|(id, track_name)| {
                // Two tracks with one name mustn't write one file.
                let mut file = file_name(&[name, &track_name]);
                let mut n = 2;
                while !used.insert(file.clone()) {
                    file = file_name(&[name, &format!("{track_name} {n}")]);
                    n += 1;
                }
                Job {
                    target: Target::Track(id),
                    path: place.join(file),
                }
            })
            .collect()
    }

    /// `folder` is where to start looking: the project's own, if saved.
    pub fn show(
        &mut self,
        ctx: &eframe::egui::Context,
        project: &Project,
        name: &str,
        folder: Option<&std::path::Path>,
    ) -> Outcome {
        use eframe::egui;
        let mut outcome = Outcome::Open;
        egui::Window::new("Export Audio")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if let Some(jobs) = self.confirm.clone() {
                    let existing = jobs.iter().filter(|j| j.path.exists()).count();
                    ui.label(format!(
                        "{existing} of the files are already there. Replace them?"
                    ));
                    let buttons = chrome::dialog_buttons(ui, "Replace", true, None, "Back");
                    if buttons.primary.clicked() {
                        outcome = Outcome::Export(jobs, self.settings);
                    }
                    if buttons.cancel.clicked() {
                        self.confirm = None;
                    }
                    return;
                }
                ui.radio_value(&mut self.what, What::Mix, "Mix");
                let selected = format!("Selected tracks ({})", self.selected.len());
                ui.add_enabled_ui(!self.selected.is_empty(), |ui| {
                    ui.radio_value(&mut self.what, What::Selected, selected);
                });
                ui.radio_value(&mut self.what, What::EachTrack, "Each track");
                ui.separator();
                egui::Grid::new("export_settings")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Format");
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(format!("WAV {}", self.settings.format.label()))
                            .show_ui(ui, |ui| {
                                for format in WavFormat::ALL {
                                    let label = format!("WAV {}", format.label());
                                    ui.selectable_value(&mut self.settings.format, format, label);
                                }
                            });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut self.settings.tail, "Include tails")
                            .on_hover_text("Keep going until reverbs and releases die away");
                        ui.end_row();
                        ui.label("");
                        let region = project.loop_region;
                        let label = match region {
                            Some(r) => {
                                let bar = |t: i64| project.tempo.bar_beat(midi::Tick::new(t)).bar;
                                // The last bar it covers any of.
                                let last = bar(r.end.get() - 1);
                                format!("Just the loop (bars {}–{last})", bar(r.start.get()))
                            }
                            None => "Just the loop".to_owned(),
                        };
                        ui.add_enabled_ui(region.is_some(), |ui| {
                            ui.checkbox(&mut self.settings.loop_only, label)
                                .on_disabled_hover_text("Shift+drag on the ruler to set a loop");
                        });
                        if region.is_none() {
                            self.settings.loop_only = false;
                        }
                        ui.end_row();
                    });
                ui.separator();
                let buttons = chrome::dialog_buttons(ui, "Export…", true, None, "Cancel");
                if buttons.primary.clicked() {
                    outcome = self.pick(project, name, folder);
                }
                if buttons.cancel.clicked() {
                    outcome = Outcome::Cancel;
                }
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    /// Asks where to write, then what to replace.
    fn pick(&mut self, project: &Project, name: &str, folder: Option<&std::path::Path>) -> Outcome {
        let mut dialog = rfd::FileDialog::new();
        if let Some(folder) = folder {
            dialog = dialog.set_directory(folder);
        }
        let place = if self.what == What::Mix {
            // The system dialog asks about replacing the file itself.
            dialog
                .set_title("Export mix")
                .add_filter("WAV", &["wav"])
                .set_file_name(file_name(&[name]))
                .save_file()
        } else {
            dialog.set_title("Export tracks to").pick_folder()
        };
        let Some(place) = place else {
            return Outcome::Open;
        };
        let jobs = self.jobs(project, name, place);
        if self.what != What::Mix && jobs.iter().any(|j| j.path.exists()) {
            self.confirm = Some(jobs);
            return Outcome::Open;
        }
        Outcome::Export(jobs, self.settings)
    }
}
