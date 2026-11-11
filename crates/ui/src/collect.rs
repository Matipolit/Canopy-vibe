//! File → Collect and Save, and File → Import Collected: a project with
//! every file it plays from, to take to another computer.
//!
//! Collecting copies the project's audio files into `Samples/`, its
//! Wavetable user tables into `Tables/` and each Sampler instrument, with
//! its includes and samples in their own layout, into `Instruments/<name>/`,
//! beside a copy of the project that names them relative to itself (see
//! `project::collect`). Compressed, all of it goes into one zip file,
//! `<name>.canopyz`, written entry by entry with nothing unpacked on disk
//! first, and integer WAVs become FLAC, half the size and the same samples.
//! Float WAVs (what canopy freezes to) stay WAVs, so nothing above full
//! scale is lost. The open project is left as it is, like Save a Copy.
//!
//! Files the project names that aren't there are left out and listed, and
//! keep their old paths so Locate File… still finds them. Plugins aren't
//! files a project can carry; the summary lists the ones it needs.
//!
//! Importing unpacks a `.canopyz` into a folder of its own and opens the
//! project in it. Either job runs on a worker thread, writes into a hidden
//! folder or file beside its destination, and only moves it into place
//! once everything is written, so a failure or Cancel leaves nothing
//! half-made behind.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use project::{FileRef, NeededPlugin, Project};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::theme::space;
use crate::widgets::chrome;

/// A collected project packed into one file: a zip.
pub const ARCHIVE_EXTENSION: &str = "canopyz";

/// Where the audio files clips and Slicers play from go.
const SAMPLES: &str = "Samples";
/// Wavetable user tables.
const TABLES: &str = "Tables";
/// Sampler instruments, one folder each.
const INSTRUMENTS: &str = "Instruments";

/// Formats that are compressed already; zipping them again only costs time.
const COMPRESSED: [&str; 5] = ["flac", "mp3", "ogg", "oga", "opus"];

/// How often a running job wakes the UI to redraw its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Where a collected project goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A folder named after the project, made inside this one.
    Folder(PathBuf),
    /// This `.canopyz` file.
    Archive(PathBuf),
}

/// What collecting did.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// The folder or `.canopyz` file written.
    pub written: PathBuf,
    /// Files copied or encoded, the project file not counted.
    pub files: usize,
    /// What those files took before, and what they take now (or in the
    /// archive, compressed).
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Files the project names that weren't found, so were left out.
    pub missing: Vec<String>,
    /// Plugins the project uses, which the other computer needs installed.
    pub plugins: Vec<NeededPlugin>,
}

/// What a finished job made.
#[derive(Debug, Clone, PartialEq)]
pub enum Finished {
    Collected(Summary),
    /// The project file of an unpacked archive, to open.
    Unpacked(PathBuf),
}

/// A collect or unpack in progress, on its own thread.
pub struct Running {
    /// `0..=1`, as `f32` bits.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Finished, String>>,
    pub collecting: bool,
}

impl Running {
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Stops it soon; what it wrote is removed, and the result says
    /// "cancelled".
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// What it made, or what went wrong, once it's over.
    pub fn poll(&self) -> Option<Result<Finished, String>> {
        self.result.try_recv().ok()
    }
}

/// Collects `project`, named `name`, to `to` on a worker thread.
pub fn start_collect(
    project: Project,
    name: String,
    to: Destination,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Running {
    spawn(true, wake, move |job| {
        collect(&project, &name, &to, job).map(Finished::Collected)
    })
}

/// Unpacks the `.canopyz` at `archive` into a new folder inside `into` on
/// a worker thread.
pub fn start_unpack(archive: PathBuf, into: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Running {
    spawn(false, wake, move |job| {
        unpack(&archive, &into, job).map(Finished::Unpacked)
    })
}

fn spawn(
    collecting: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
    work: impl FnOnce(&Job) -> Result<Finished, String> + Send + 'static,
) -> Running {
    let progress = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, result) = mpsc::channel();
    let job = Job {
        progress: progress.clone(),
        cancel: cancel.clone(),
        wake: wake.clone(),
        last_wake: std::sync::Mutex::new(Instant::now()),
    };
    let spawned = thread::Builder::new()
        .name("canopy-collect".to_owned())
        .spawn(move || {
            let outcome = work(&job);
            let _ = sender.send(outcome);
            wake();
        });
    if let Err(err) = spawned {
        // The thread never ran, so nothing waits on the channel's other
        // end: report it through a channel of its own.
        let (sender, result) = mpsc::channel();
        let _ = sender.send(Err(format!("couldn't start: {err}")));
        return Running {
            progress,
            cancel,
            result,
            collecting,
        };
    }
    Running {
        progress,
        cancel,
        result,
        collecting,
    }
}

/// The worker's side of a [`Running`].
pub struct Job {
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
    last_wake: std::sync::Mutex<Instant>,
}

impl Job {
    /// A job no one watches or cancels, for tests.
    #[cfg(test)]
    pub fn detached() -> Job {
        Job {
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(|| {}),
            last_wake: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Cancels it, as [`Running::cancel`] would, for tests.
    #[cfg(test)]
    pub fn cancel_now(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn keep_going(&self) -> bool {
        !self.cancel.load(Ordering::Relaxed)
    }

    fn check(&self) -> io::Result<()> {
        if self.keep_going() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        }
    }

    fn report(&self, done: u64, total: u64) {
        let fraction = if total == 0 {
            1.0
        } else {
            (done as f64 / total as f64).min(1.0) as f32
        };
        self.progress.store(fraction.to_bits(), Ordering::Relaxed);
        let mut last = self.last_wake.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() >= PROGRESS_INTERVAL {
            *last = Instant::now();
            (self.wake)();
        }
    }
}

/// "38 files, 214 MB → 131 MB", for the status bar.
pub fn size_change(summary: &Summary) -> String {
    let files = match summary.files {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    };
    format!(
        "{files}, {} → {}",
        megabytes(summary.bytes_before),
        megabytes(summary.bytes_after)
    )
}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1e6;
    if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{mb:.0} MB")
    }
}

/// What the Collect and Save dialog decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Open,
    Cancel,
    Collect(Destination),
}

/// File → Collect and Save: compress or not, then where.
pub struct Dialog {
    pub compress: bool,
}

impl Dialog {
    /// `name` is the project's; `folder`, where to start looking.
    pub fn show(&mut self, ctx: &egui::Context, name: &str, folder: Option<&Path>) -> Outcome {
        let mut outcome = Outcome::Open;
        egui::Window::new("Collect and Save")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("The project and every file it uses, in one place.");
                ui.checkbox(&mut self.compress, "Compress")
                    .on_hover_text("One .canopyz file; WAVs stored as FLAC");
                let buttons = chrome::dialog_buttons(ui, "Collect…", true, None, "Cancel");
                if buttons.primary.clicked()
                    && let Some(to) = self.pick(name, folder)
                {
                    outcome = Outcome::Collect(to);
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

    fn pick(&self, name: &str, folder: Option<&Path>) -> Option<Destination> {
        let mut dialog = rfd::FileDialog::new();
        if let Some(folder) = folder {
            dialog = dialog.set_directory(folder);
        }
        if self.compress {
            let file = dialog
                .set_title("Collect into")
                .add_filter("Collected canopy project", &[ARCHIVE_EXTENSION])
                .set_file_name(format!("{}.{ARCHIVE_EXTENSION}", file_name_safe(name)))
                .save_file()?;
            Some(Destination::Archive(with_archive_extension(&file)))
        } else {
            let parent = dialog.set_title("Collect into a folder in").pick_folder()?;
            Some(Destination::Folder(parent))
        }
    }
}

/// Adds `.canopyz` unless it's there already.
pub fn with_archive_extension(path: &Path) -> PathBuf {
    if is_archive(path) {
        return path.to_owned();
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(ARCHIVE_EXTENSION);
    PathBuf::from(name)
}

pub fn is_archive(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ARCHIVE_EXTENSION))
}

/// Asks where to unpack `archive`: a folder is made inside the one picked.
pub fn pick_unpack_folder(archive: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Unpack into");
    if let Some(folder) = archive.parent() {
        dialog = dialog.set_directory(folder);
    }
    dialog.pick_folder()
}

/// Asks for a `.canopyz` to import.
pub fn pick_archive() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import collected project")
        .add_filter("Collected canopy project", &[ARCHIVE_EXTENSION])
        .pick_file()
}

/// After a collect that left files out or needs plugins: what to know
/// before taking it elsewhere. Returns whether it's still open.
pub fn report(ctx: &egui::Context, summary: &Summary) -> bool {
    let mut open = true;
    egui::Window::new("Collected")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(summary.written.display().to_string());
            ui.weak(size_change(summary));
            if !summary.plugins.is_empty() {
                ui.add_space(space::M);
                ui.strong("Needs plugins");
                for plugin in &summary.plugins {
                    let label = ui.label(&plugin.name);
                    if !plugin.vendor.is_empty() {
                        label.on_hover_text(&plugin.vendor);
                    }
                }
            }
            if !summary.missing.is_empty() {
                ui.add_space(space::M);
                ui.strong(format!("Not found, left out: {}", summary.missing.len()));
                // Eight names, then it scrolls.
                let lines = ui.text_style_height(&egui::TextStyle::Body) * 8.0;
                egui::ScrollArea::vertical()
                    .max_height(lines)
                    .show(ui, |ui| {
                        for missing in &summary.missing {
                            ui.weak(missing);
                        }
                    });
            }
            let buttons = chrome::dialog_buttons(ui, "OK", true, None, "Close");
            if buttons.primary.clicked() || buttons.cancel.clicked() {
                open = false;
            }
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter)) {
        open = false;
    }
    open
}

// Collecting.

/// What one file of the project becomes.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    from: PathBuf,
    /// Where it goes, relative to the collected folder, `/` between folders.
    to: String,
    /// An audio file to store as FLAC if it can be.
    flac: bool,
}

/// Everything to copy, and where the project's paths now point.
#[derive(Debug, Default)]
struct Plan {
    items: Vec<Item>,
    /// Each file the project names that was found, by its old path, to the
    /// item that it now names.
    retarget: HashMap<PathBuf, usize>,
    missing: Vec<String>,
}

/// Names that are taken within the collected folder, compared as Windows
/// and macOS would, ignoring case.
#[derive(Default)]
struct Names(HashSet<String>);

impl Names {
    /// `folder/stem.ext`, or `folder/stem 2.ext` and so on if a file there
    /// already has that stem. Stems are kept apart, not whole names, so a
    /// file that turns into FLAC can't land on another's name.
    fn claim(&mut self, folder: &str, path: &Path) -> (String, String) {
        let stem = path
            .file_stem()
            .map_or_else(|| "file".to_owned(), |s| s.to_string_lossy().into_owned());
        let ext = path
            .extension()
            .map_or_else(String::new, |e| e.to_string_lossy().into_owned());
        let mut candidate = stem.clone();
        let mut n = 2;
        while !self
            .0
            .insert(format!("{folder}/{candidate}").to_lowercase())
        {
            candidate = format!("{stem} {n}");
            n += 1;
        }
        (format!("{folder}/{candidate}"), ext)
    }
}

fn plan(project: &Project) -> Plan {
    let mut plan = Plan::default();
    let mut names = Names::default();
    for (what, path) in project.files() {
        if plan.retarget.contains_key(path) {
            continue;
        }
        if !path.is_file() {
            let shown = path.display().to_string();
            if !plan.missing.contains(&shown) {
                plan.missing.push(shown);
            }
            continue;
        }
        let sfz = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sfz"));
        let index = match what {
            FileRef::Instrument(_) if sfz => match plan_instrument(path, &mut names, &mut plan) {
                Some(index) => index,
                None => continue,
            },
            FileRef::Wavetable(..) => {
                let (to, ext) = names.claim(TABLES, path);
                push(&mut plan, path, with_extension(to, &ext), false)
            }
            FileRef::Source(_) | FileRef::Instrument(_) => {
                let (to, ext) = names.claim(SAMPLES, path);
                push(&mut plan, path, with_extension(to, &ext), true)
            }
        };
        plan.retarget.insert(path.to_owned(), index);
    }
    plan
}

fn push(plan: &mut Plan, from: &Path, to: String, flac: bool) -> usize {
    plan.items.push(Item {
        from: from.to_owned(),
        to,
        flac,
    });
    plan.items.len() - 1
}

fn with_extension(stem: String, ext: &str) -> String {
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

/// A Sampler instrument: its `.sfz`, includes and samples, laid out under
/// `Instruments/<name>/` as they are under the folder all of them share,
/// so the `.sfz` finds everything where it looks. Returns the `.sfz`'s item.
fn plan_instrument(sfz: &Path, names: &mut Names, plan: &mut Plan) -> Option<usize> {
    let found = match instruments::builtin::sampler::dependencies(sfz) {
        Ok(found) => found,
        Err(err) => {
            plan.missing.push(format!("{}: {err}", sfz.display()));
            return None;
        }
    };
    let bank = sfz.file_stem().map_or_else(
        || "Instrument".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    let (folder, _) = names.claim(INSTRUMENTS, Path::new(&bank));
    let mut files = vec![sfz.to_owned()];
    files.extend(found.included);
    files.extend(found.samples);
    let root = common_folder(&files);
    let mut sfz_item = None;
    for file in &files {
        let inside = relative_parts(file, &root);
        let index = push(plan, file, format!("{folder}/{inside}"), false);
        sfz_item.get_or_insert(index);
    }
    plan.missing.extend(
        found
            .missing
            .into_iter()
            .map(|sample| format!("{bank}: {sample}")),
    );
    sfz_item
}

/// The deepest folder every one of `files` is inside.
fn common_folder(files: &[PathBuf]) -> PathBuf {
    let mut common: Option<Vec<std::path::Component>> = None;
    for file in files {
        let parts: Vec<_> = file
            .parent()
            .unwrap_or(Path::new(""))
            .components()
            .collect();
        common = Some(match common {
            None => parts,
            Some(so_far) => so_far
                .into_iter()
                .zip(parts)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    common.unwrap_or_default().into_iter().collect()
}

/// `file` relative to `root`, `/` between folders. Anything in the way
/// that isn't a plain name (a drive, `..`) is dropped: the result is always
/// inside the folder it's joined to.
fn relative_parts(file: &Path, root: &Path) -> String {
    let inside = file.strip_prefix(root).unwrap_or(file);
    inside
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Where collected files are written.
trait Sink {
    /// Starts the file at `to` (relative, `/` between folders).
    fn create(&mut self, to: &str, size: u64, compress: bool) -> io::Result<&mut dyn Write>;
    /// Moves what was written into place, returning where.
    fn finish(self: Box<Self>) -> io::Result<(PathBuf, u64)>;
    /// Removes what was written.
    fn abandon(self: Box<Self>);
}

/// A folder: written as a hidden one beside it, renamed when done.
struct FolderSink {
    staging: PathBuf,
    target: PathBuf,
    open: Option<io::BufWriter<File>>,
    written: u64,
}

impl FolderSink {
    fn new(parent: &Path, name: &str) -> io::Result<FolderSink> {
        let staging = parent.join(format!(".{name}.collecting"));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        Ok(FolderSink {
            staging,
            target: unique_folder(parent, name),
            open: None,
            written: 0,
        })
    }

    fn close(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.open.take() {
            file.flush()?;
            self.written += file.get_ref().metadata()?.len();
        }
        Ok(())
    }
}

impl Sink for FolderSink {
    fn create(&mut self, to: &str, _size: u64, _compress: bool) -> io::Result<&mut dyn Write> {
        self.close()?;
        let path = to
            .split('/')
            .fold(self.staging.clone(), |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(self.open.insert(io::BufWriter::new(File::create(path)?)))
    }

    fn finish(mut self: Box<Self>) -> io::Result<(PathBuf, u64)> {
        self.close()?;
        fs::rename(&self.staging, &self.target)?;
        Ok((self.target.clone(), self.written))
    }

    fn abandon(mut self: Box<Self>) {
        self.open = None;
        let _ = fs::remove_dir_all(&self.staging);
    }
}

/// A zip file, everything inside one folder named after the project:
/// written beside its destination, renamed over it when done.
struct ZipSink {
    zip: Option<ZipWriter<io::BufWriter<File>>>,
    folder: String,
    partial: PathBuf,
    target: PathBuf,
}

impl ZipSink {
    fn new(target: &Path, folder: &str) -> io::Result<ZipSink> {
        let mut partial = target.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        let file = io::BufWriter::new(File::create(&partial)?);
        Ok(ZipSink {
            zip: Some(ZipWriter::new(file)),
            folder: folder.to_owned(),
            partial,
            target: target.to_owned(),
        })
    }
}

impl Sink for ZipSink {
    fn create(&mut self, to: &str, size: u64, compress: bool) -> io::Result<&mut dyn Write> {
        let method = if compress {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .large_file(size >= u64::from(u32::MAX));
        let zip = self
            .zip
            .as_mut()
            .expect("the zip is open until it's finished");
        zip.start_file(format!("{}/{to}", self.folder), options)
            .map_err(io::Error::other)?;
        Ok(zip)
    }

    fn finish(mut self: Box<Self>) -> io::Result<(PathBuf, u64)> {
        let zip = self
            .zip
            .take()
            .expect("the zip is open until it's finished");
        let mut file = zip.finish().map_err(io::Error::other)?;
        file.flush()?;
        let file = file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        let size = file.metadata()?.len();
        drop(file);
        fs::rename(&self.partial, &self.target)?;
        Ok((self.target.clone(), size))
    }

    fn abandon(mut self: Box<Self>) {
        self.zip = None;
        let _ = fs::remove_file(&self.partial);
    }
}

/// Collects `project` to `to`, named `name` (the folder, and the project
/// file in it).
pub fn collect(
    project: &Project,
    name: &str,
    to: &Destination,
    job: &Job,
) -> Result<Summary, String> {
    let name = file_name_safe(name);
    let plan = plan(project);
    let compressing = matches!(to, Destination::Archive(_));
    let made: io::Result<Box<dyn Sink>> = match to {
        Destination::Folder(parent) => FolderSink::new(parent, &name).map(|s| Box::new(s) as _),
        Destination::Archive(file) => ZipSink::new(file, &name).map(|s| Box::new(s) as _),
    };
    let mut sink = made.map_err(|err| format!("couldn't write there: {err}"))?;
    match write_all(&plan, project, &name, compressing, sink.as_mut(), job) {
        Ok((files, bytes_before)) => {
            let (written, bytes_after) = sink.finish().map_err(|err| err.to_string())?;
            Ok(Summary {
                written,
                files,
                bytes_before,
                bytes_after,
                missing: plan.missing,
                plugins: project.plugins_needed(),
            })
        }
        Err(err) => {
            sink.abandon();
            Err(if err.kind() == io::ErrorKind::Interrupted {
                "cancelled".to_owned()
            } else {
                err.to_string()
            })
        }
    }
}

/// Writes every item and then the project file. Returns how many files
/// were written and what they took before.
fn write_all(
    plan: &Plan,
    project: &Project,
    name: &str,
    compressing: bool,
    sink: &mut dyn Sink,
    job: &Job,
) -> io::Result<(usize, u64)> {
    let sizes: Vec<u64> = plan
        .items
        .iter()
        .map(|item| fs::metadata(&item.from).map_or(0, |m| m.len()))
        .collect();
    let total: u64 = sizes.iter().sum();
    let mut done = 0;
    let mut destinations = Vec::with_capacity(plan.items.len());
    for (item, &size) in plan.items.iter().zip(&sizes) {
        job.check()?;
        let fail =
            |err: io::Error| io::Error::new(err.kind(), format!("{}: {err}", item.from.display()));
        let flac = if compressing && item.flac {
            media::wav_to_flac(&item.from, &|| job.keep_going()).map_err(fail)?
        } else {
            None
        };
        let to = match &flac {
            Some(bytes) => {
                let to = replace_extension(&item.to, "flac");
                sink.create(&to, bytes.len() as u64, false)?
                    .write_all(bytes)?;
                to
            }
            None => {
                let compress = compressing && !is_compressed(&item.from);
                let out = sink.create(&item.to, size, compress)?;
                copy(&item.from, out, job).map_err(fail)?;
                item.to.clone()
            }
        };
        destinations.push(to);
        done += size;
        job.report(done, total);
    }

    let mut collected = project.clone();
    collected.map_files(|_, path| {
        if let Some(&index) = plan.retarget.get(path.as_path()) {
            *path = PathBuf::from(&destinations[index]);
        }
    });
    let text = collected
        .to_file_string()
        .map_err(|err| io::Error::other(err.to_string()))?;
    let file = format!("{name}.{}", project::FILE_EXTENSION);
    sink.create(&file, text.len() as u64, true)?
        .write_all(text.as_bytes())?;
    Ok((plan.items.len(), total))
}

fn copy(from: &Path, to: &mut dyn Write, job: &Job) -> io::Result<()> {
    let mut file = File::open(from)?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        job.check()?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        to.write_all(&buffer[..read])?;
    }
}

fn is_compressed(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| COMPRESSED.iter().any(|c| e.eq_ignore_ascii_case(c)))
}

fn replace_extension(to: &str, ext: &str) -> String {
    let (folder, file) = to.rsplit_once('/').unwrap_or(("", to));
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    if folder.is_empty() {
        format!("{stem}.{ext}")
    } else {
        format!("{folder}/{stem}.{ext}")
    }
}

/// `name` with what Windows refuses in file names replaced.
fn file_name_safe(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.trim().trim_end_matches('.').to_owned();
    if safe.is_empty() {
        "Project".to_owned()
    } else {
        safe
    }
}

/// `parent/name`, or `parent/name 2` and so on if that's taken.
fn unique_folder(parent: &Path, name: &str) -> PathBuf {
    let mut candidate = parent.join(name);
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{name} {n}"));
        n += 1;
    }
    candidate
}

// Unpacking.

/// Unpacks `archive` into a new folder inside `into`, named after the
/// folder the archive holds (or the archive, if it holds files loose).
/// Returns the project file in it.
pub fn unpack(archive: &Path, into: &Path, job: &Job) -> Result<PathBuf, String> {
    let file = File::open(archive).map_err(|err| format!("couldn't open it: {err}"))?;
    let mut zip = ZipArchive::new(io::BufReader::new(file))
        .map_err(|err| format!("not a collected project: {err}"))?;
    let contents = contents(&mut zip)?;
    let folder = match &contents.top {
        Some(top) => top.clone(),
        None => archive.file_stem().map_or_else(
            || "Project".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        ),
    };
    let staging = into.join(format!(".{folder}.unpacking"));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|err| err.to_string())?;
    }
    let target = unique_folder(into, &folder);
    let result =
        extract(&mut zip, &contents, &staging, job).and_then(|()| fs::rename(&staging, &target));
    match result {
        Ok(()) => Ok(target.join(&contents.project)),
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            Err(if err.kind() == io::ErrorKind::Interrupted {
                "cancelled".to_owned()
            } else {
                err.to_string()
            })
        }
    }
}

/// What an archive holds, checked before anything is written.
struct Contents {
    /// Each entry's path inside the folder it unpacks to, `None` for a
    /// folder entry.
    entries: Vec<Option<PathBuf>>,
    /// The one folder everything is in, if there is one.
    top: Option<String>,
    /// The project file, inside the folder it unpacks to.
    project: PathBuf,
    total: u64,
}

fn contents<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Contents, String> {
    let mut paths = Vec::with_capacity(zip.len());
    let mut total = 0;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|err| err.to_string())?;
        // A name that climbs out with `..` or starts at a root would write
        // outside the folder: refuse the whole archive.
        let Some(path) = entry.enclosed_name() else {
            return Err(format!(
                "it holds a file outside its folder: {}",
                entry.name()
            ));
        };
        total += entry.size();
        paths.push((path, entry.is_dir()));
    }
    let first = |p: &Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
    };
    let tops: HashSet<Option<String>> = paths.iter().map(|(p, _)| first(p)).collect();
    let all_inside = paths
        .iter()
        .all(|(p, dir)| *dir || p.components().count() > 1);
    let top = match (tops.len(), all_inside) {
        (1, true) => tops.into_iter().next().flatten(),
        _ => None,
    };
    let entries: Vec<Option<PathBuf>> = paths
        .iter()
        .map(|(path, dir)| {
            let inside = match &top {
                Some(top) => path.strip_prefix(top).unwrap_or(path).to_owned(),
                None => path.clone(),
            };
            (!dir && !inside.as_os_str().is_empty()).then_some(inside)
        })
        .collect();
    let project = entries
        .iter()
        .flatten()
        .filter(|p| p.extension().is_some_and(|e| e == project::FILE_EXTENSION))
        .min_by_key(|p| p.components().count())
        .cloned()
        .ok_or("it holds no canopy project")?;
    Ok(Contents {
        entries,
        top,
        project,
        total,
    })
}

fn extract<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    contents: &Contents,
    staging: &Path,
    job: &Job,
) -> io::Result<()> {
    fs::create_dir_all(staging)?;
    let mut done = 0;
    let mut buffer = vec![0; 1 << 20];
    for (index, inside) in contents.entries.iter().enumerate() {
        job.check()?;
        let Some(inside) = inside else {
            continue;
        };
        let path = staging.join(inside);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut entry = zip.by_index(index).map_err(io::Error::other)?;
        let mut out = io::BufWriter::new(File::create(&path)?);
        loop {
            job.check()?;
            let read = entry.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            out.write_all(&buffer[..read])?;
            done += read as u64;
            job.report(done, contents.total);
        }
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_clash_get_numbers_ignoring_case() {
        let mut names = Names::default();
        let claim = |names: &mut Names, path: &str| {
            let (to, ext) = names.claim(SAMPLES, Path::new(path));
            with_extension(to, &ext)
        };
        assert_eq!(claim(&mut names, "/a/kick.wav"), "Samples/kick.wav");
        assert_eq!(claim(&mut names, "/b/Kick.wav"), "Samples/Kick 2.wav");
        // A FLAC of the first mustn't land on the third.
        assert_eq!(claim(&mut names, "/c/kick.flac"), "Samples/kick 3.flac");
        assert_eq!(claim(&mut names, "/c/snare.wav"), "Samples/snare.wav");
    }

    #[test]
    fn instruments_keep_their_layout_under_the_folder_they_share() {
        let files = [
            PathBuf::from("/libs/Kit/kit.sfz"),
            PathBuf::from("/libs/Kit/Samples/kick.wav"),
            PathBuf::from("/libs/Shared/hat.wav"),
        ];
        let root = common_folder(&files);
        assert_eq!(root, PathBuf::from("/libs"));
        let laid_out: Vec<_> = files.iter().map(|f| relative_parts(f, &root)).collect();
        assert_eq!(
            laid_out,
            ["Kit/kit.sfz", "Kit/Samples/kick.wav", "Shared/hat.wav"]
        );
    }

    #[test]
    fn extensions_are_replaced_on_the_file_only() {
        assert_eq!(
            replace_extension("Samples/a.b/kick.wav", "flac"),
            "Samples/a.b/kick.flac"
        );
        assert_eq!(
            replace_extension("Samples/kick", "flac"),
            "Samples/kick.flac"
        );
    }

    #[test]
    fn project_names_become_safe_file_names() {
        assert_eq!(file_name_safe("mix: v2/final?"), "mix_ v2_final_");
        assert_eq!(file_name_safe("  ..  "), "Project");
    }
}
