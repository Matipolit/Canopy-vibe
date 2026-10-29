//! Reading plugin files, in process and out: what the test plugins say
//! about themselves, the scan cache, and scanners that crash or hang.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use plugins::scan::{self, ScanCommand, ScanOptions};
use plugins::{PluginFormat, PluginKind};

#[test]
fn the_test_plugins_describe_themselves() {
    for format in FORMATS {
        let infos: Vec<_> = format
            .files()
            .into_iter()
            .flat_map(|path| scan::read_plugin_file(format.format, path).unwrap())
            .collect();
        assert_eq!(infos.len(), 2, "{format:?}");
        let gain = format.info(format.gain);
        assert_eq!(
            (gain.name.as_str(), gain.vendor.as_str()),
            ("Canopy Test Gain", "canopy")
        );
        assert_eq!(gain.version, "1.0.0");
        assert_eq!(gain.kind, PluginKind::Effect);
        assert_eq!(gain.audio_inputs.len(), 2);
        assert!(gain.main_input().is_some_and(|p| p.channels == 2 && p.main));
        assert!(gain.aux_input().is_some_and(|p| p.name == "Sidechain"));
        assert_eq!(gain.main_output().map(|p| p.channels), Some(2));
        assert_eq!(gain.note_inputs.len(), 1);
        assert!(!gain.bridged);
        let sine = format.info(format.sine);
        assert_eq!(sine.kind, PluginKind::Instrument, "{format:?}");
        assert!(sine.audio_inputs.is_empty());
        match format.format {
            PluginFormat::Clap => assert!(sine.categories.contains(&"synthesizer".to_owned())),
            PluginFormat::Vst3 => assert_eq!(sine.categories, ["Instrument", "Synth"]),
            PluginFormat::Vst2 => {
                assert_eq!(sine.categories, ["Synth"]);
                assert_eq!(gain.categories, ["Effect"]);
                assert_eq!(sine.note_outputs.len(), 1, "it says it sends MIDI");
                assert!(gain.note_outputs.is_empty());
            }
        }
    }
}

/// A folder for one test, emptied first.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canopy-plugins-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scanner() -> ScanCommand {
    ScanCommand::program(env!("CARGO_BIN_EXE_canopy-plugin-scanner"))
}

fn options(dirs: &[&Path], cache: &Path) -> ScanOptions {
    let mut options = ScanOptions::new(scanner());
    options.paths = PluginFormat::ALL
        .into_iter()
        .flat_map(|f| dirs.iter().map(move |d| (f, d.to_path_buf())))
        .collect();
    options.cache = Some(cache.to_owned());
    options.timeout = Duration::from_secs(30);
    options
}

#[test]
fn scans_out_of_process_and_caches() {
    let fixtures = fixtures();
    let dir = scratch("cache");
    let cache = dir.join("plugins.json");
    let options = options(&[&fixtures.dir], &cache);
    let report = scan::scan(&options);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    // The CLAP file, the VST3 bundle and two VST2 files; the VST3
    // bundle's library isn't taken for a VST2 one.
    assert_eq!((report.read, report.cached), (4, 0));
    let mut ids: Vec<&str> = report.plugins.iter().map(|p| p.id.as_str()).collect();
    ids.sort();
    let mut expected = [
        CLAP.gain, CLAP.sine, VST3.gain, VST3.sine, VST2.gain, VST2.sine,
    ];
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(
        report.plugins.iter().find(|p| p.id == CLAP.gain),
        Some(&CLAP.info(CLAP.gain))
    );

    // A scanner that can't even start proves the second scan reads nothing.
    let mut cached = options.clone();
    cached.scanner = ScanCommand::program("/nonexistent/scanner");
    let again = scan::scan(&cached);
    assert_eq!((again.read, again.cached), (0, 4));
    assert_eq!(again.plugins, report.plugins);

    // Scanning another folder keeps these in the cache.
    let empty = scratch("cache-other");
    let mut other = options.clone();
    other.paths = vec![(PluginFormat::Clap, empty.clone())];
    assert!(scan::scan(&other).plugins.is_empty());
    let back = scan::scan(&cached);
    assert_eq!(back.cached, 4);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&empty).unwrap();
}

#[test]
fn a_changed_file_is_read_again() {
    let fixtures = fixtures();
    let dir = scratch("changed");
    let plugin = dir.join("copy.clap");
    std::fs::copy(&fixtures.clap, &plugin).unwrap();
    let cache = dir.join("cache.json");
    let options = options(&[&dir], &cache);
    assert_eq!(scan::scan(&options).read, 1);
    assert_eq!(scan::scan(&options).cached, 1);
    // Same bytes, new time. Set by hand: copying keeps the source's time
    // on Windows.
    let copy = dir.join("tmp");
    std::fs::copy(&fixtures.clap, &copy).unwrap();
    let modified = std::fs::metadata(&plugin).unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&copy)
        .unwrap()
        .set_modified(modified + Duration::from_secs(1))
        .unwrap();
    std::fs::rename(&copy, &plugin).unwrap();
    let report = scan::scan(&options);
    assert_eq!((report.read, report.plugins.len()), (1, 2));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn crashing_plugins_are_reported_and_remembered() {
    let fixtures = fixtures();
    let dir = scratch("crash");
    let plugin = dir.join("crash.clap");
    std::fs::copy(&fixtures.clap, &plugin).unwrap();
    // A file that isn't a plugin at all next to it.
    std::fs::write(dir.join("junk.clap"), b"not a library").unwrap();
    let cache = dir.join("cache.json");
    let mut options = options(&[&dir], &cache);
    options.scanner.envs = vec![("CANOPY_TEST_CLAP_CRASH".into(), "1".into())];
    let report = scan::scan(&options);
    assert!(report.plugins.is_empty());
    assert_eq!(report.failures.len(), 2, "{:?}", report.failures);
    let crash = report.failures.iter().find(|f| f.path == plugin).unwrap();
    assert!(crash.error.contains("crashed"), "{}", crash.error);
    let junk = report.failures.iter().find(|f| f.path != plugin).unwrap();
    assert!(junk.error.contains("couldn't load"), "{}", junk.error);

    // Failures are cached too, until asked to retry.
    options.scanner.envs.clear();
    assert_eq!(scan::scan(&options).failures.len(), 2);
    options.retry_failures = true;
    let retried = scan::scan(&options);
    assert_eq!(retried.plugins.len(), 2, "{:?}", retried.failures);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn hanging_scanners_are_killed() {
    let fixtures = fixtures();
    let dir = scratch("hang");
    std::fs::copy(&fixtures.clap, dir.join("slow.clap")).unwrap();
    let mut options = options(&[&dir], &dir.join("cache.json"));
    options.scanner = ScanCommand {
        program: "sh".into(),
        args: vec!["-c".into(), "sleep 10".into(), "sh".into()],
        envs: Vec::new(),
    };
    options.timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let report = scan::scan(&options);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0].error.contains("timed out"),
        "{}",
        report.failures[0].error
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn windows_plugins_on_linux_point_to_yabridge() {
    let dir = scratch("windows");
    let plugin = dir.join("Serum.clap");
    std::fs::write(&plugin, b"MZ\x90\x00 a Windows DLL").unwrap();
    let error = scan::read_plugin_file(PluginFormat::Clap, &plugin).unwrap_err();
    assert!(error.to_string().contains("yabridge"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}

// yabridge is Linux's; on Windows the loaded test plugin couldn't be
// deleted afterwards either (libraries are never unloaded).
#[cfg(unix)]
#[test]
fn yabridge_folders_are_searched() {
    // yabridgectl's layout under the default folders: found, and marked.
    let fixtures = fixtures();
    let home = scratch("yabridge");
    let clap = home.join(".clap/yabridge/Serum.clap");
    std::fs::create_dir_all(clap.parent().unwrap()).unwrap();
    std::fs::copy(&fixtures.clap, &clap).unwrap();
    std::fs::write(clap.with_extension("clap-win"), b"MZ").unwrap();
    let found = scan::find_plugin_files(PluginFormat::Clap, &[home.join(".clap")]);
    assert_eq!(found, std::slice::from_ref(&clap));
    let infos = scan::read_plugin_file(PluginFormat::Clap, &clap).unwrap();
    assert!(infos.iter().all(|p| p.bridged));
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
#[test]
fn bridged_vst2_plugins_are_marked_and_unbridged_ones_reported() {
    // yabridgectl's VST2 layout: its library next to a link to the Windows
    // plugin, in folders mirroring the Windows ones.
    let fixtures = fixtures();
    let home = scratch("yabridge-vst2");
    let library = home.join(".vst/yabridge/Xfer/Serum_x64.so");
    std::fs::create_dir_all(library.parent().unwrap()).unwrap();
    std::fs::copy(&fixtures.vst2_gain, &library).unwrap();
    let windows = home.join("Serum_x64.dll");
    std::fs::write(&windows, b"MZ").unwrap();
    std::os::unix::fs::symlink(&windows, library.with_extension("dll")).unwrap();
    // A Windows plugin yabridge wasn't set up for.
    let lonely = home.join(".vst/Lonely.dll");
    std::fs::write(&lonely, b"MZ\x90\x00").unwrap();

    let found = scan::find_plugin_files(PluginFormat::Vst2, &[home.join(".vst")]);
    assert_eq!(found, vec![lonely.clone(), library.clone()]);
    let infos = scan::read_plugin_file(PluginFormat::Vst2, &library).unwrap();
    assert_eq!(infos.len(), 1);
    assert!(infos[0].bridged);
    let error = scan::read_plugin_file(PluginFormat::Vst2, &lonely).unwrap_err();
    assert!(error.to_string().contains("yabridge"), "{error}");
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn libraries_without_a_vst2_entry_are_not_plugins_or_failures() {
    // The VST3 test plugin's library: loadable, but no VSTPluginMain.
    let fixtures = fixtures();
    let dir = scratch("not-vst2");
    let library = dir
        .join("helper")
        .with_extension(PluginFormat::Vst2.extension());
    let vst3_binary = std::fs::read_dir(fixtures.vst3.join("Contents"))
        .unwrap()
        .flatten()
        .flat_map(|arch| std::fs::read_dir(arch.path()).unwrap().flatten())
        .map(|e| e.path())
        .next()
        .unwrap();
    std::fs::copy(vst3_binary, &library).unwrap();
    assert_eq!(
        scan::read_plugin_file(PluginFormat::Vst2, &library).unwrap(),
        []
    );
    let report = scan::scan(&options(&[&dir], &dir.join("cache.json")));
    assert!(report.plugins.is_empty() && report.failures.is_empty());
    assert_eq!(report.read, 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn vst2_shells_are_reported_as_unsupported() {
    let fixtures = fixtures();
    let dir = scratch("vst2-shell");
    let shell = dir
        .join("canopy-test-shell")
        .with_extension(PluginFormat::Vst2.extension());
    std::fs::copy(&fixtures.vst2_gain, &shell).unwrap();
    let report = scan::scan(&options(&[&dir], &dir.join("cache.json")));
    assert!(report.plugins.is_empty());
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(
        report.failures[0].error.contains("shell"),
        "{}",
        report.failures[0].error
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
