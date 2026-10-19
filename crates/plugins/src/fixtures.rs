//! The test plugins, built and laid out for tests in any crate.
//!
//! `test-plugins/clap-gain`, `test-plugins/vst3-gain` and
//! `test-plugins/vst2-gain` are workspace members, but not default ones,
//! and cargo can't depend on a library's built `cdylib`. So [`build`] runs
//! `cargo build` for them (once per test process, in a target folder of
//! their own so it doesn't wait on the outer build's lock) and copies them
//! where a host expects plugins: a `.clap` file, a `.vst3` bundle, and the
//! VST2 library twice (a VST2 file holds one plugin; the library becomes
//! the one its file name says).
//!
//! Set `CANOPY_TEST_PLUGINS` to a folder already laid out like
//! [`Fixtures::dir`] to skip building (for a cross-compiled run, say).
//!
//! Each format has two plugins, with the same parameters ([`params`]):
//!
//! - a gain effect ([`CLAP_GAIN`], [`VST3_GAIN`], [`VST2_GAIN`]): stereo in
//!   plus a stereo sidechain (added to the input), stereo out, a note
//!   input. Output is input delayed by "Latency" samples and scaled by
//!   "Gain", changing at the exact sample of each change. Latency is
//!   reported as the plugin's latency; changing it while active asks for a
//!   restart (CLAP `request_restart`, VST3
//!   `restartComponent(kLatencyChanged)`, VST2 `audioMasterIOChanged`).
//! - a sine instrument ([`CLAP_SINE`], [`VST3_SINE`], [`VST2_SINE`]): a
//!   sine per held key, velocity-scaled, through Gain.
//!
//! Values: CLAP Gain is in dB (-60 to +12, modulatable), Latency in
//! samples (0 to 1024, stepped). VST3 and VST2 have them normalized: Gain
//! 0..1 over the same dB range (0 dB at 60/72), Latency 0..1 over 1024
//! steps.
//!
//! What they report back, for tests of the host's return paths:
//!
//! - "Tempo seen": the transport's tempo, sent as an output parameter
//!   change (CLAP: BPM; VST3: BPM / 1000).
//! - CLAP: "Mod seen", the last `PARAM_MOD` amount on Gain (also an output
//!   change); MIDI CC 7 makes the plugin set its own Gain as its editor
//!   would (gesture begin, value, gesture end); "Thread errors" counts
//!   calls on the wrong thread, as the host's thread-check extension
//!   tells the plugin.
//! - VST3: MIDI CC 1 is mapped to Gain (`IMidiMapping`); setting "Reset"
//!   makes the controller `performEdit` Gain back to 0 dB, as an editor
//!   button would; the component messages the controller on activation
//!   through the host's connection, counted by "Messages seen" (in
//!   thousandths).
//! - VST2: setting "Reset" makes the plugin set Gain back to 0 dB with
//!   begin and end edit around `audioMasterAutomate`, as an editor would
//!   (on whichever thread set it); MIDI CC 7 does the same on the audio
//!   thread with Gain at CC 7's value; it reports the tempo with
//!   `audioMasterAutomate` from the audio thread; "Thread errors" counts
//!   calls on the wrong kind of thread, as the host's
//!   `audioMasterGetCurrentProcessLevel` answers; the sine echoes notes an
//!   octave up (`audioMasterProcessEvents`), has two programs (a MIDI
//!   program change picks one; "Program" shows it) and keeps no chunk, so
//!   the host saves its parameters and program. The gain's state is a
//!   chunk. "Made since loaded" counts the plugins the library made since
//!   it was loaded. A copy of the library whose name contains "shell"
//!   calls itself a shell plugin.
//!
//! On Linux they have a small X11 editor that runs only from the host's
//! timers and file-descriptor callbacks (CLAP timer-support and
//! posix-fd-support, VST3 `IRunLoop`) or idle calls (VST2 `effEditIdle`),
//! asks to be resized to 400 × 250 on its first tick, and reports what it
//! saw in parameters 7 to 10 (raw numbers in every format).
//!
//! State is Gain and Latency (VST3: the component's; the controller saves
//! a marker byte). With `CANOPY_TEST_CLAP_CRASH` set in its environment,
//! the CLAP file crashes when loaded, for testing scanners.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The CLAP id of the test gain effect.
pub const CLAP_GAIN: &str = "org.canopy.test.gain";
/// The CLAP id of the test sine instrument.
pub const CLAP_SINE: &str = "org.canopy.test.sine";
/// The VST3 class id of the test gain effect.
pub const VST3_GAIN: &str = "C0FFEE01CA0E4A11B0B0000000000001";
/// The VST3 class id of the test sine instrument.
pub const VST3_SINE: &str = "C0FFEE01CA0E4A11B0B0000000000002";
/// The VST2 unique id (`'CnyG'`) of the test gain effect.
pub const VST2_GAIN: &str = "436E7947";
/// The VST2 unique id (`'CnyS'`) of the test sine instrument.
pub const VST2_SINE: &str = "436E7953";

/// Parameter ids, the same in every test plugin.
pub mod params {
    use crate::ParamId;

    pub const GAIN: ParamId = ParamId(0);
    pub const LATENCY: ParamId = ParamId(1);
    pub const MOD_SEEN: ParamId = ParamId(2);
    pub const TEMPO_SEEN: ParamId = ParamId(3);
    /// VST3 and VST2: a switch that makes the plugin reset Gain itself.
    pub const RESET: ParamId = ParamId(4);
    /// CLAP and VST2: calls the plugin got on the wrong thread.
    pub const THREAD_ERRORS: ParamId = ParamId(5);
    /// VST3 only: messages from the component, in thousandths.
    pub const MESSAGES: ParamId = ParamId(6);
    /// VST2 only (the id VST3 uses for [`MESSAGES`]): the current program.
    pub const PROGRAM: ParamId = ParamId(6);
    /// The test editor's timer ticks so far.
    pub const GUI_TICKS: ParamId = ParamId(7);
    /// X11 events the test editor read when its descriptor was ready.
    pub const GUI_FD_EVENTS: ParamId = ParamId(8);
    /// The width of the host's editor window, as the test editor saw it.
    pub const GUI_PARENT_WIDTH: ParamId = ParamId(9);
    /// 1 while the test editor is open.
    pub const GUI_OPEN: ParamId = ParamId(10);
    /// VST2 only: plugins the library made since it was loaded.
    pub const MADE: ParamId = ParamId(11);
}

/// Where the built test plugins are.
#[derive(Debug, Clone)]
pub struct Fixtures {
    /// The folder holding `clap/`, `vst3/` and `vst2/`.
    pub dir: PathBuf,
    /// `clap/canopy-test.clap`.
    pub clap: PathBuf,
    /// `vst3/canopy-test.vst3` (a bundle).
    pub vst3: PathBuf,
    /// `vst2/canopy-test-gain.so` (`.dll` on Windows).
    pub vst2_gain: PathBuf,
    /// `vst2/canopy-test-sine.so` (`.dll` on Windows).
    pub vst2_sine: PathBuf,
}

impl Fixtures {
    /// The layout under `dir`.
    pub fn at(dir: &Path) -> Fixtures {
        let vst2 = |name: &str| {
            dir.join("vst2")
                .join(format!("canopy-test-{name}"))
                .with_extension(crate::PluginFormat::Vst2.extension())
        };
        Fixtures {
            dir: dir.to_owned(),
            clap: dir.join("clap").join("canopy-test.clap"),
            vst3: dir.join("vst3").join("canopy-test.vst3"),
            vst2_gain: vst2("gain"),
            vst2_sine: vst2("sine"),
        }
    }
}

/// Builds the test plugins (once per process) and lays them out; an error
/// says why not.
pub fn build() -> Result<&'static Fixtures, String> {
    static FIXTURES: OnceLock<Result<Fixtures, String>> = OnceLock::new();
    FIXTURES
        .get_or_init(build_now)
        .as_ref()
        .map_err(Clone::clone)
}

fn build_now() -> Result<Fixtures, String> {
    if let Some(dir) = std::env::var_os("CANOPY_TEST_PLUGINS") {
        let fixtures = Fixtures::at(Path::new(&dir));
        if !fixtures.clap.is_file()
            || !fixtures.vst3.is_dir()
            || !fixtures.vst2_gain.is_file()
            || !fixtures.vst2_sine.is_file()
        {
            return Err(format!(
                "CANOPY_TEST_PLUGINS={}: no test plugins there",
                fixtures.dir.display()
            ));
        }
        return Ok(fixtures);
    }
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_dir.join("../..");
    let target = workspace.join("target").join("test-plugins");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(&cargo)
        .current_dir(&workspace)
        .args([
            "build",
            "--quiet",
            "-p",
            "clap-gain",
            "-p",
            "vst3-gain",
            "-p",
            "vst2-gain",
            "--target-dir",
        ])
        .arg(&target)
        .output()
        .map_err(|e| {
            format!(
                "couldn't run {} in {}: {e}",
                Path::new(&cargo).display(),
                workspace.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "building the test plugins failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let built = |name: &str| target.join("debug").join(library_name(name));
    let fixtures = Fixtures::at(&target.join("plugins"));
    install(&built("clap_gain"), &fixtures.clap)?;
    let arch = std::env::consts::ARCH;
    let binary = if cfg!(windows) {
        fixtures
            .vst3
            .join("Contents")
            .join(format!("{arch}-win"))
            .join("canopy-test.vst3")
    } else {
        fixtures
            .vst3
            .join("Contents")
            .join(format!("{arch}-linux"))
            .join("canopy-test.so")
    };
    install(&built("vst3_gain"), &binary)?;
    install(&built("vst2_gain"), &fixtures.vst2_gain)?;
    install(&built("vst2_gain"), &fixtures.vst2_sine)?;
    Ok(fixtures)
}

/// `name`'s shared library file name on this OS.
fn library_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{name}.dylib")
    } else {
        format!("lib{name}.so")
    }
}

/// Copies `from` to `to` through a temporary name, so a process that has
/// the old file loaded keeps its copy.
fn install(from: &Path, to: &Path) -> Result<(), String> {
    let fail =
        |e: std::io::Error| format!("installing {} as {}: {e}", from.display(), to.display());
    std::fs::create_dir_all(to.parent().unwrap()).map_err(fail)?;
    let temporary = to.with_extension(format!("tmp{}", std::process::id()));
    std::fs::copy(from, &temporary).map_err(fail)?;
    std::fs::rename(&temporary, to).map_err(fail)
}
