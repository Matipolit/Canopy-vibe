//! Finding plugins and reading what they are, without risking the app.
//!
//! [`default_paths`] lists the standard folders of each format, and
//! [`find_plugin_files`] the plugin files and bundles under some folders.
//! Reading a plugin means loading its code, and plugins do crash or hang
//! while loading, so [`scan`] reads each file in a separate process: the
//! command in [`ScanOptions::scanner`] (this program, if its `main` starts
//! with [`run_scanner_if_requested`], or the `canopy-plugin-scanner` binary
//! this crate builds), killed after a timeout. Several run at once.
//!
//! Results, failures included, are cached in a JSON file by each file's
//! size and modification time ([`default_cache_path`]), so only new and
//! changed plugins are read again. For yabridge's bridged plugins the
//! Windows file they point to counts too, so updating the Windows plugin
//! rescans it.
//!
//! # Windows plugins on Linux
//!
//! yabridge (<https://github.com/robbert-vdh/yabridge>) makes Windows
//! VST2, VST3 and CLAP plugins loadable by Linux hosts through Wine.
//! `yabridgectl sync` puts a small Linux library for each Windows plugin
//! in `~/.vst3/yabridge` (VST3 bundles with the Windows module linked
//! inside, under `Contents/x86_64-win`), `~/.clap/yabridge` (a `.clap`
//! next to a `.clap-win` link to the Windows plugin) and `~/.vst/yabridge`
//! (a `.so` next to a `.dll` link to the Windows plugin, folders mirroring
//! the Windows ones; in yabridge's older "inline" setup the `.so` sits next
//! to the Windows `.dll` itself). All are inside the default folders, which
//! are searched recursively; such plugins are marked
//! [`crate::PluginInfo::bridged`]. See the crate's README.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, PluginFormat, PluginInfo};

/// The argument that makes a program a scanner (see
/// [`run_scanner_if_requested`]); followed by the format key and the path.
pub const SCAN_ARG: &str = "--canopy-plugin-scan";

/// What a scanner prints before its result, so whatever the plugin itself
/// prints on stdout doesn't get in the way.
const RESULT_MARKER: &str = "\u{1}canopy-plugin-scan-result:";

/// The folders each format's plugins are installed in, by convention:
///
/// - CLAP: the folders in `CLAP_PATH`, then `~/.clap`, `/usr/lib/clap` and
///   `/usr/local/lib/clap` on Linux; `%COMMONPROGRAMFILES%\CLAP` and
///   `%LOCALAPPDATA%\Programs\Common\CLAP` on Windows.
/// - VST3: `~/.vst3`, `/usr/lib/vst3` and `/usr/local/lib/vst3` on Linux;
///   `%COMMONPROGRAMFILES%\VST3` (normally `C:\Program Files\Common
///   Files\VST3`) and `%LOCALAPPDATA%\Programs\Common\VST3` on Windows.
/// - VST2: the folders in `VST_PATH` (and `LXVST_PATH` on Linux, which
///   some hosts use), then `~/.vst`, `/usr/lib/vst`, `/usr/local/lib/vst`,
///   `/usr/lib/lxvst`, `/usr/local/lib/lxvst` and `~/.lxvst` on Linux;
///   `%PROGRAMFILES%\VSTPlugins`, `%PROGRAMFILES%\Steinberg\VSTPlugins`,
///   `%COMMONPROGRAMFILES%\VST2` and `%COMMONPROGRAMFILES%\Steinberg\VST2`
///   on Windows (VST2 has no one standard folder there).
///
/// yabridge's folders (`~/.clap/yabridge`, `~/.vst3/yabridge`,
/// `~/.vst/yabridge`) are inside these.
pub fn default_paths(format: PluginFormat) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let variables: &[&str] = match format {
        PluginFormat::Clap => &["CLAP_PATH"],
        PluginFormat::Vst3 => &[],
        PluginFormat::Vst2 if cfg!(windows) => &["VST_PATH"],
        PluginFormat::Vst2 => &["VST_PATH", "LXVST_PATH"],
    };
    for variable in variables {
        if let Some(list) = env(variable) {
            paths.extend(std::env::split_paths(&list));
        }
    }
    let home = env("HOME").map(PathBuf::from);
    let folder = match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Vst2 => "VST2",
    };
    if format == PluginFormat::Vst2 {
        if cfg!(windows) {
            let program_files =
                PathBuf::from(env("PROGRAMFILES").unwrap_or_else(|| r"C:\Program Files".into()));
            let common = PathBuf::from(
                env("COMMONPROGRAMFILES")
                    .unwrap_or_else(|| r"C:\Program Files\Common Files".into()),
            );
            paths.push(program_files.join("VSTPlugins"));
            paths.push(program_files.join("Steinberg").join("VSTPlugins"));
            paths.push(common.join("VST2"));
            paths.push(common.join("Steinberg").join("VST2"));
        } else {
            if let Some(home) = &home {
                paths.push(home.join(".vst"));
            }
            for folder in [
                "/usr/lib/vst",
                "/usr/local/lib/vst",
                "/usr/lib/lxvst",
                "/usr/local/lib/lxvst",
            ] {
                paths.push(PathBuf::from(folder));
            }
            if let Some(home) = &home {
                paths.push(home.join(".lxvst"));
            }
        }
    } else if cfg!(windows) {
        let common =
            env("COMMONPROGRAMFILES").unwrap_or_else(|| r"C:\Program Files\Common Files".into());
        paths.push(PathBuf::from(common).join(folder));
        if let Some(local) = env("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Common")
                    .join(folder),
            );
        }
    } else {
        let name = format.key();
        if let Some(home) = &home {
            paths.push(home.join(format!(".{name}")));
        }
        paths.push(PathBuf::from("/usr/lib").join(name));
        paths.push(PathBuf::from("/usr/local/lib").join(name));
    }
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    paths
}

/// Where [`scan`] keeps its cache by default:
/// `$XDG_CACHE_HOME/canopy/plugins.json` (`~/.cache/...`) on Linux,
/// `%LOCALAPPDATA%\canopy\cache\plugins.json` on Windows.
pub fn default_cache_path() -> Option<PathBuf> {
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        Some(
            env("LOCALAPPDATA")?
                .join("canopy")
                .join("cache")
                .join("plugins.json"),
        )
    } else {
        let cache = env("XDG_CACHE_HOME").or_else(|| Some(env("HOME")?.join(".cache")))?;
        Some(cache.join("canopy").join("plugins.json"))
    }
}

/// The plugin files and bundles of `format` under `dirs`, searched
/// recursively (following links, at most 16 levels deep), sorted.
///
/// CLAP: `.clap` files. VST3: `.vst3` bundle folders that have a binary for
/// this platform, and on Windows also single `.vst3` files. VST2: `.so`
/// files on Linux, `.dll` files on Windows (not looking inside other
/// formats' bundles); on Linux also `.dll` files without a `.so` of the
/// same name, Windows plugins yabridge hasn't been set up for, so the scan
/// can say so. Libraries that turn out not to be plugins are skipped when
/// read.
pub fn find_plugin_files(format: PluginFormat, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited = HashSet::new();
    for dir in dirs {
        walk(format, dir, 0, &mut visited, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

fn walk(
    format: PluginFormat,
    dir: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    found: &mut Vec<PathBuf>,
) {
    if depth > 16 {
        return;
    }
    let Ok(canonical) = dir.canonicalize() else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let is_dir = path.is_dir();
        let has_extension = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(format.extension()));
        match (format, has_extension, is_dir) {
            (PluginFormat::Clap, true, false) => found.push(path),
            (PluginFormat::Vst3, true, true) => {
                if crate::vst3::binary_path(&path).is_file() {
                    found.push(path);
                }
            }
            (PluginFormat::Vst3, true, false) if cfg!(windows) => found.push(path),
            (PluginFormat::Vst2, true, false) => found.push(path),
            (PluginFormat::Vst2, false, false)
                if cfg!(not(windows)) && is_unbridged_windows_vst2(&path) =>
            {
                found.push(path)
            }
            (PluginFormat::Vst2, _, true) if is_bundle(&path) => {}
            (_, _, true) => walk(format, &path, depth + 1, visited, found),
            _ => {}
        }
    }
}

/// Whether the plugin at `path` is a yabridge bridge to a Windows plugin.
pub(crate) fn is_bridged(format: PluginFormat, path: &Path) -> bool {
    match format {
        PluginFormat::Clap => path.with_extension("clap-win").symlink_metadata().is_ok(),
        PluginFormat::Vst3 => cfg!(not(windows)) && windows_binaries(path).next().is_some(),
        PluginFormat::Vst2 => cfg!(not(windows)) && windows_vst2(path).is_some(),
    }
}

/// The Windows `.dll` (or a link to it) next to a VST2 `.so` of the same
/// name: how yabridge lays out bridged VST2 plugins.
fn windows_vst2(path: &Path) -> Option<PathBuf> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("so"))
    {
        return None;
    }
    ["dll", "DLL", "Dll"]
        .into_iter()
        .map(|extension| path.with_extension(extension))
        .find(|dll| dll.symlink_metadata().is_ok())
}

/// A Windows VST2 `.dll` with no yabridge `.so` next to it.
fn is_unbridged_windows_vst2(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
        && path.with_extension("so").symlink_metadata().is_err()
}

/// A folder that is another format's bundle (or an app), which VST2's
/// search doesn't look into: a VST3 bundle's Linux binary is a `.so` too.
fn is_bundle(dir: &Path) -> bool {
    dir.extension().is_some_and(|e| {
        ["vst3", "clap", "lv2", "vst", "component", "app"]
            .iter()
            .any(|b| e.eq_ignore_ascii_case(b))
    })
}

/// The Windows modules inside a VST3 bundle (`Contents/*-win/*`).
fn windows_binaries(bundle: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(bundle.join("Contents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with("-win"))
        .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
}

/// What's in the plugin file at `path`, read in this process.
///
/// Loading a plugin runs its code: a plugin that crashes takes this
/// process with it. Use [`scan`] for files you don't trust; this is what
/// its scanner processes run.
pub fn read_plugin_file(format: PluginFormat, path: &Path) -> Result<Vec<PluginInfo>, Error> {
    if cfg!(not(windows)) {
        let binary = match format {
            PluginFormat::Clap | PluginFormat::Vst2 => path.to_owned(),
            PluginFormat::Vst3 => crate::vst3::binary_path(path),
        };
        let mut magic = [0u8; 2];
        let windows = std::fs::File::open(&binary)
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok_and(|_| magic == *b"MZ");
        if windows {
            return Err(Error::Load(format!(
                "{} is a Windows plugin; bridge it with yabridge",
                path.display()
            )));
        }
    }
    match format {
        PluginFormat::Clap => crate::clap::read_file(path),
        PluginFormat::Vst3 => crate::vst3::read_file(path),
        PluginFormat::Vst2 => crate::vst2::read_file(path),
    }
}

/// How [`scan`] starts a scanner: `program`, then `args`, then
/// [`SCAN_ARG`], the format key and the file's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Extra environment variables for the scanner.
    pub envs: Vec<(OsString, OsString)>,
}

impl ScanCommand {
    /// The running program, which must call [`run_scanner_if_requested`]
    /// first thing in `main`.
    pub fn current_exe() -> std::io::Result<ScanCommand> {
        Ok(ScanCommand::program(std::env::current_exe()?))
    }

    /// A program that only scans, like `canopy-plugin-scanner`.
    pub fn program(path: impl Into<PathBuf>) -> ScanCommand {
        ScanCommand {
            program: path.into(),
            args: Vec::new(),
            envs: Vec::new(),
        }
    }
}

/// What and how to scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// The folders to search, per format. [`ScanOptions::new`] fills in
    /// [`default_paths`].
    pub paths: Vec<(PluginFormat, PathBuf)>,
    pub scanner: ScanCommand,
    /// How long one file may take (yabridge plugins start Wine, which can
    /// take a while the first time).
    pub timeout: Duration,
    /// How many scanners run at once.
    pub jobs: usize,
    /// The cache file, if any.
    pub cache: Option<PathBuf>,
    /// Read files again that failed last time even if they haven't
    /// changed (a plugin whose license was missing).
    pub retry_failures: bool,
}

impl ScanOptions {
    /// The default folders of every format, with the default cache.
    pub fn new(scanner: ScanCommand) -> ScanOptions {
        ScanOptions {
            paths: PluginFormat::ALL
                .into_iter()
                .flat_map(|format| default_paths(format).into_iter().map(move |p| (format, p)))
                .collect(),
            scanner,
            timeout: Duration::from_secs(60),
            jobs: std::thread::available_parallelism().map_or(2, |n| n.get().min(8)),
            cache: default_cache_path(),
            retry_failures: false,
        }
    }
}

/// A file a scan couldn't read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanFailure {
    pub format: PluginFormat,
    pub path: PathBuf,
    /// What went wrong: the plugin's error, or how the scanner died
    /// (crashed, timed out) and the end of what it printed.
    pub error: String,
}

/// What a scan found.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub plugins: Vec<PluginInfo>,
    pub failures: Vec<ScanFailure>,
    /// How many files were read this time, and how many came from the cache.
    pub read: usize,
    pub cached: usize,
}

/// What identifies a version of a plugin file: sizes and modification
/// times of the files it's made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    size: u64,
    modified_ns: u64,
}

fn stamp(format: PluginFormat, path: &Path) -> Option<Stamp> {
    let mut files = vec![match format {
        PluginFormat::Clap | PluginFormat::Vst2 => path.to_owned(),
        PluginFormat::Vst3 => crate::vst3::binary_path(path),
    }];
    match format {
        PluginFormat::Clap => files.push(path.with_extension("clap-win")),
        PluginFormat::Vst3 => {
            files.push(path.join("Contents").join("moduleinfo.json"));
            files.extend(windows_binaries(path));
        }
        PluginFormat::Vst2 => files.extend(windows_vst2(path)),
    }
    let mut stamp = Stamp {
        size: 0,
        modified_ns: 0,
    };
    for (i, file) in files.iter().enumerate() {
        // Follows links: a yabridge link stands for the Windows file.
        let Ok(meta) = std::fs::metadata(file) else {
            if i == 0 {
                return None;
            }
            continue;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        stamp.size = stamp.size.wrapping_add(meta.len());
        stamp.modified_ns = stamp.modified_ns.max(modified);
    }
    Some(stamp)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    format: PluginFormat,
    path: PathBuf,
    stamp: Stamp,
    result: Result<Vec<PluginInfo>, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: Vec<CacheEntry>,
}

const CACHE_VERSION: u32 = 1;

fn load_cache(path: &Path) -> HashMap<(PluginFormat, PathBuf), CacheEntry> {
    let cache: CacheFile = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|c: &CacheFile| c.version == CACHE_VERSION)
        .unwrap_or_default();
    cache
        .entries
        .into_iter()
        .map(|e| ((e.format, e.path.clone()), e))
        .collect()
}

fn save_cache(path: &Path, entries: Vec<CacheEntry>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let cache = CacheFile {
        version: CACHE_VERSION,
        entries,
    };
    let json = serde_json::to_vec_pretty(&cache).map_err(std::io::Error::other)?;
    // Written aside and renamed, so a crash never leaves half a cache.
    let temporary = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&temporary, json)?;
    std::fs::rename(&temporary, path)
}

/// Finds the plugins in [`ScanOptions::paths`] and reads the new and
/// changed ones in scanner processes (see the module docs). Updates the
/// cache.
pub fn scan(options: &ScanOptions) -> ScanReport {
    let mut files = Vec::new();
    for format in PluginFormat::ALL {
        let dirs: Vec<PathBuf> = options
            .paths
            .iter()
            .filter(|(f, _)| *f == format)
            .map(|(_, p)| p.clone())
            .collect();
        files.extend(
            find_plugin_files(format, &dirs)
                .into_iter()
                .map(|p| (format, p)),
        );
    }
    let cache = options.cache.as_deref().map(load_cache).unwrap_or_default();
    let mut report = ScanReport::default();
    let mut entries = Vec::new();
    let mut to_read = VecDeque::new();
    for (format, path) in files {
        let Some(stamp) = stamp(format, &path) else {
            continue;
        };
        match cache.get(&(format, path.clone())) {
            Some(entry)
                if entry.stamp == stamp && (entry.result.is_ok() || !options.retry_failures) =>
            {
                report.cached += 1;
                entries.push(entry.clone());
            }
            _ => to_read.push_back((format, path, stamp)),
        }
    }
    report.read = to_read.len();
    let queue = Mutex::new(to_read);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..options.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                    let Some((format, path, stamp)) = next else {
                        break;
                    };
                    let result = scan_one(options, format, &path);
                    results
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(CacheEntry {
                            format,
                            path,
                            stamp,
                            result,
                        });
                }
            });
        }
    });
    entries.extend(results.into_inner().unwrap_or_else(|e| e.into_inner()));
    entries.sort_by(|a, b| (a.format, &a.path).cmp(&(b.format, &b.path)));
    for entry in &entries {
        match &entry.result {
            Ok(plugins) => report.plugins.extend(plugins.iter().cloned()),
            Err(error) => report.failures.push(ScanFailure {
                format: entry.format,
                path: entry.path.clone(),
                error: error.clone(),
            }),
        }
    }
    if let Some(path) = &options.cache {
        // Keep what other scans (of other folders) found, while it exists.
        let seen: HashSet<(PluginFormat, PathBuf)> =
            entries.iter().map(|e| (e.format, e.path.clone())).collect();
        let others = cache.into_values().filter(|e| {
            !seen.contains(&(e.format, e.path.clone())) && stamp(e.format, &e.path).is_some()
        });
        entries.extend(others);
        let _ = save_cache(path, entries);
    }
    report
}

/// Reads one file in a scanner process.
pub fn scan_file(
    options: &ScanOptions,
    format: PluginFormat,
    path: &Path,
) -> Result<Vec<PluginInfo>, String> {
    scan_one(options, format, path)
}

fn scan_one(
    options: &ScanOptions,
    format: PluginFormat,
    path: &Path,
) -> Result<Vec<PluginInfo>, String> {
    let command = &options.scanner;
    let mut scanner = Command::new(&command.program);
    scanner
        .args(&command.args)
        .arg(SCAN_ARG)
        .arg(format.key())
        .arg(path)
        .envs(command.envs.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a timeout also ends what it started (a
    // bridged plugin's Wine host, say).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut scanner, 0);
    let mut child = scanner.spawn().map_err(|e| {
        format!(
            "couldn't start the scanner {}: {e}",
            command.program.display()
        )
    })?;
    let stdout = Capture::start(child.stdout.take());
    let stderr = Capture::start(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > options.timeout => {
                #[cfg(unix)]
                // SAFETY: signals the scanner's own process group.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("waiting for the scanner: {e}")),
        }
    };
    // Processes the scanner started may hold its pipes open after it's
    // gone: take what came within a moment rather than wait for them.
    let deadline = Instant::now() + Duration::from_secs(1);
    while !(stdout.finished() && stderr.finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (stdout, stderr) = (stdout.text(), stderr.text());
    if let Some(at) = stdout.rfind(RESULT_MARKER) {
        let json = stdout[at + RESULT_MARKER.len()..]
            .lines()
            .next()
            .unwrap_or("");
        if let Ok(result) = serde_json::from_str::<Result<Vec<PluginInfo>, String>>(json) {
            return result;
        }
    }
    let what = match status {
        None => format!("timed out after {} s", options.timeout.as_secs_f32()),
        Some(status) => describe_exit(status),
    };
    let tail: String = {
        let tail = stderr.trim_end();
        let start = tail.char_indices().rev().nth(500).map_or(0, |(i, _)| i);
        tail[start..].to_owned()
    };
    Err(if tail.is_empty() {
        format!("the scanner {what}")
    } else {
        format!("the scanner {what}: {tail}")
    })
}

/// A pipe read on a thread of its own, so a chatty plugin can't fill it
/// and stall the scanner.
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Capture {
        let capture = Capture {
            bytes: Arc::default(),
            done: Arc::default(),
        };
        let (bytes, done) = (capture.bytes.clone(), capture.done.clone());
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0u8; 4096];
                while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                    bytes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk[..n]);
                }
            }
            done.store(true, Ordering::Release);
        });
        capture
    }

    fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                libc::SIGSEGV => " (SIGSEGV)",
                libc::SIGABRT => " (SIGABRT)",
                libc::SIGBUS => " (SIGBUS)",
                libc::SIGILL => " (SIGILL)",
                libc::SIGFPE => " (SIGFPE)",
                libc::SIGKILL => " (SIGKILL)",
                _ => "",
            };
            return format!("crashed with signal {signal}{name}");
        }
    }
    // On Windows a crash is an exit code too: the NTSTATUS of the unhandled
    // exception, whose top two bits say "error". Programs that exit on
    // their own never use those codes.
    #[cfg(windows)]
    if let Some(code) = status.code().map(|c| c as u32)
        && code >= 0xC000_0000
    {
        let name = match code {
            0xC000_0005 => " (access violation)",
            0xC000_001D => " (illegal instruction)",
            0xC000_0094 => " (integer division by zero)",
            0xC000_00FD => " (stack overflow)",
            0xC000_0374 => " (heap corruption)",
            // `abort`, `__fastfail` and the C runtime's buffer checks.
            0xC000_0409 => " (fail fast)",
            _ => "",
        };
        return format!("crashed with exception {code:#010X}{name}");
    }
    match status.code() {
        Some(code) => format!("exited with code {code} without a result"),
        None => "crashed".to_owned(),
    }
}

/// Makes this program a scanner when started with [`SCAN_ARG`]: reads
/// the plugin file, prints the result for [`scan`] and exits. Returns at
/// once otherwise. Call it first thing in `main` of a program that scans
/// with [`ScanCommand::current_exe`].
pub fn run_scanner_if_requested() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(at) = args.iter().position(|a| a == SCAN_ARG) else {
        return;
    };
    let result = match (args.get(at + 1), args.get(at + 2)) {
        (Some(format), Some(path)) => match format.to_str().and_then(PluginFormat::from_key) {
            Some(format) => read_plugin_file(format, Path::new(path)).map_err(|e| e.to_string()),
            None => Err(format!(
                "unknown plugin format {}",
                format.to_string_lossy()
            )),
        },
        _ => Err(format!("usage: {SCAN_ARG} <clap|vst3|vst2> <path>")),
    };
    let json = serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"Err\":\"{e}\"}}"));
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "\n{RESULT_MARKER}{json}");
    let _ = stdout.flush();
    drop(stdout);
    exit_now();
}

/// Exits without running destructors or `atexit` handlers: plugins'
/// teardown code is a common place to hang or crash, and the result is out.
fn exit_now() -> ! {
    #[cfg(unix)]
    // SAFETY: _exit ends the process at once; nothing runs after it.
    unsafe {
        libc::_exit(0)
    }
    #[cfg(not(unix))]
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bundles_files_and_yabridge_layouts() {
        let root = std::env::temp_dir().join(format!("canopy-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let touch = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        };
        let arch = if cfg!(windows) {
            "x86_64-win"
        } else {
            "x86_64-linux"
        };
        let ext = if cfg!(windows) { "vst3" } else { "so" };
        let clap = root.join(".clap");
        let vst3 = root.join(".vst3");
        touch(&clap.join("Vendor/Synth.clap"));
        touch(&clap.join("yabridge/Serum.clap"));
        touch(&clap.join("yabridge/Serum.clap-win"));
        touch(&clap.join("notes.txt"));
        touch(&vst3.join(format!("Gain.vst3/Contents/{arch}/Gain.{ext}")));
        touch(&vst3.join(format!("yabridge/OTT.vst3/Contents/{arch}/OTT.{ext}")));
        touch(&vst3.join("yabridge/OTT.vst3/Contents/x86_64-win/OTT.vst3"));
        // A bundle with no binary for this platform isn't a plugin here.
        touch(&vst3.join("Mac.vst3/Contents/MacOS/Mac"));

        let claps = find_plugin_files(PluginFormat::Clap, std::slice::from_ref(&clap));
        assert_eq!(
            claps,
            vec![
                clap.join("Vendor/Synth.clap"),
                clap.join("yabridge/Serum.clap")
            ]
        );
        assert!(is_bridged(PluginFormat::Clap, &claps[1]));
        assert!(!is_bridged(PluginFormat::Clap, &claps[0]));

        let vst3s = find_plugin_files(PluginFormat::Vst3, std::slice::from_ref(&vst3));
        assert_eq!(
            vst3s,
            vec![vst3.join("Gain.vst3"), vst3.join("yabridge/OTT.vst3")]
        );
        if cfg!(not(windows)) {
            assert!(is_bridged(PluginFormat::Vst3, &vst3s[1]));
        }
        assert!(!is_bridged(PluginFormat::Vst3, &vst3s[0]));

        // The Windows file counts towards a bridged plugin's stamp.
        let before = stamp(PluginFormat::Clap, &claps[1]).unwrap();
        std::fs::write(clap.join("yabridge/Serum.clap-win"), b"longer").unwrap();
        assert_ne!(stamp(PluginFormat::Clap, &claps[1]).unwrap(), before);

        // VST2: libraries, but not those inside other formats' bundles.
        let vst = root.join(".vst");
        let library = if cfg!(windows) { "dll" } else { "so" };
        touch(&vst.join(format!("Vendor/Delay.{library}")));
        touch(&vst.join(format!("Inside.vst3/Contents/{arch}/Inside.{library}")));
        touch(&vst.join("yabridge/Sub/Serum.so"));
        touch(&vst.join("yabridge/Sub/Serum.dll"));
        touch(&vst.join("Prefix/Lonely.dll"));
        let vst2s = find_plugin_files(PluginFormat::Vst2, std::slice::from_ref(&vst));
        if cfg!(windows) {
            assert_eq!(
                vst2s,
                vec![
                    vst.join("Prefix/Lonely.dll"),
                    vst.join("Vendor/Delay.dll"),
                    vst.join("yabridge/Sub/Serum.dll")
                ]
            );
        } else {
            // A Windows plugin without yabridge's library is found, to be
            // reported; one with it is the library's.
            assert_eq!(
                vst2s,
                vec![
                    vst.join("Prefix/Lonely.dll"),
                    vst.join("Vendor/Delay.so"),
                    vst.join("yabridge/Sub/Serum.so")
                ]
            );
            assert!(is_bridged(PluginFormat::Vst2, &vst2s[2]));
            assert!(!is_bridged(PluginFormat::Vst2, &vst2s[1]));
            let before = stamp(PluginFormat::Vst2, &vst2s[2]).unwrap();
            std::fs::write(vst.join("yabridge/Sub/Serum.dll"), b"longer").unwrap();
            assert_ne!(stamp(PluginFormat::Vst2, &vst2s[2]).unwrap(), before);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn default_paths_cover_the_standard_folders() {
        let clap = default_paths(PluginFormat::Clap);
        let vst3 = default_paths(PluginFormat::Vst3);
        let vst2 = default_paths(PluginFormat::Vst2);
        if cfg!(target_os = "linux") {
            assert!(clap.contains(&PathBuf::from("/usr/lib/clap")));
            assert!(vst3.contains(&PathBuf::from("/usr/local/lib/vst3")));
            assert!(vst2.contains(&PathBuf::from("/usr/lib/vst")));
            assert!(vst2.contains(&PathBuf::from("/usr/local/lib/lxvst")));
            if let Some(home) = std::env::var_os("HOME") {
                assert!(clap.contains(&PathBuf::from(&home).join(".clap")));
                assert!(vst3.contains(&PathBuf::from(&home).join(".vst3")));
                assert!(vst2.contains(&PathBuf::from(&home).join(".vst")));
                assert!(vst2.contains(&PathBuf::from(&home).join(".lxvst")));
            }
        }
        if cfg!(windows) {
            assert!(vst2.iter().any(|p| p.ends_with("Steinberg/VSTPlugins")));
            assert!(vst2.iter().any(|p| p.ends_with("VST2")));
        }
        assert!(default_cache_path().is_some_and(|p| p.ends_with("plugins.json")));
    }
}
