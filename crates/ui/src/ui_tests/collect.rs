//! File → Collect and Save and File → Import Collected: a project and
//! every file it plays from, taken somewhere else and opened there.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui_kittest::kittest::Queryable;
use midi::Tick;
use project::{FileRef, TrackPosition};

use super::{Rig, temp_dir, write_sine_wav};
use crate::collect::{self, Destination, Job};
use crate::session::SourceState;

/// Files around a project, as a real one would have them: two clips'
/// files with one name in different folders, a Sampler instrument whose
/// samples are in a folder of their own, a Wavetable user table, a Slicer
/// loop, and one file that's gone.
struct Setup {
    dir: PathBuf,
    missing: PathBuf,
}

fn setup(rig: &mut Rig, name: &str) -> Setup {
    let dir = temp_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    let files = dir.join("files");
    std::fs::create_dir_all(files.join("other")).unwrap();
    write_sine_wav(&files.join("tone.wav"), 1, 220.0);
    write_sine_wav(&files.join("other/tone.wav"), 1, 330.0);
    write_sine_wav(&files.join("loop.wav"), 1, 110.0);
    write_sine_wav(&files.join("table.wav"), 1, 55.0);
    let kit = files.join("Kit");
    std::fs::create_dir_all(kit.join("Samples")).unwrap();
    std::fs::write(
        kit.join("kit.sfz"),
        "<control> default_path=Samples/\n<region> sample=hit.wav key=60\n",
    )
    .unwrap();
    write_sine_wav(&kit.join("Samples/hit.wav"), 1, 440.0);

    // One at a time: imports land in whichever order they decode, and the
    // first collected keeps the name.
    let audio = rig.app().session.project.track_ids()[2];
    for (n, file) in ["tone.wav", "other/tone.wav"].into_iter().enumerate() {
        rig.app_mut().session.import_files(
            vec![files.join(file)],
            Some(audio),
            Tick::ZERO,
            TrackPosition::END,
        );
        rig.wait_for("the import", |app| {
            app.session.project.sources.len() == n + 1
        });
    }
    let missing = dir.join("gone.wav");
    let (sampler, synth) = {
        let ids = rig.app().session.project.track_ids();
        (ids[0], ids[1])
    };
    let (kit_sfz, table, loop_wav, gone) = (
        kit.join("kit.sfz"),
        files.join("table.wav"),
        files.join("loop.wav"),
        missing.clone(),
    );
    rig.app_mut()
        .session
        .edit("Files", |p| {
            p.set_instrument_file(sampler, Some(kit_sfz))?;
            p.set_wavetable_file(synth, 1, Some(table))?;
            let slicer = p.add_track(project::NewTrack::Midi, TrackPosition::END)?;
            p.set_instrument_file(slicer, Some(loop_wav))?;
            p.add_source(gone, 44_100, 1, 1);
            Ok(())
        })
        .unwrap();
    let project = dir.join("song.canopy");
    assert!(rig.app_mut().session.save_as(&project));
    Setup { dir, missing }
}

fn decoded(rig: &Rig, name: &str) -> Arc<media::Source> {
    let session = &rig.app().session;
    let source = session
        .project
        .sources
        .iter()
        .find(|s| {
            s.path
                .file_name()
                .is_some_and(|f| f.to_string_lossy().starts_with(name))
        })
        .unwrap_or_else(|| panic!("no source {name}"));
    match session.source_state(source.id) {
        Some(SourceState::Ready(audio)) => audio.clone(),
        other => panic!("{name} isn't loaded: {:?}", other.map(|_| ())),
    }
}

fn wait_for_collecting(rig: &mut Rig) {
    rig.wait_for("collecting", |app| app.session.collect_progress().is_none());
}

#[test]
fn a_compressed_project_unpacks_elsewhere_and_plays_the_same() {
    let mut rig = Rig::new("collect_compressed");
    let setup = setup(&mut rig, "collect-compressed");
    rig.wait_for("the sources", |app| {
        app.session
            .project
            .sources
            .iter()
            .filter(|s| s.path.exists())
            .all(|s| matches!(app.session.source_state(s.id), Some(SourceState::Ready(_))))
    });
    let (tone, other_tone) = (decoded(&rig, "tone"), {
        let session = &rig.app().session;
        let other = session
            .project
            .sources
            .iter()
            .find(|s| s.path.ends_with("other/tone.wav"))
            .unwrap();
        match session.source_state(other.id) {
            Some(SourceState::Ready(audio)) => audio.clone(),
            _ => panic!("not loaded"),
        }
    });

    let archive = setup.dir.join("out").join("song.canopyz");
    std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
    rig.app_mut()
        .session
        .collect(Destination::Archive(archive.clone()));
    wait_for_collecting(&mut rig);
    let summary = rig
        .app()
        .session
        .collect_summary
        .clone()
        .expect("a summary: a file was missing");
    assert_eq!(summary.written, archive);
    assert_eq!(
        summary.files, 6,
        "two tones, the kit and its sample, a table, a loop"
    );
    assert_eq!(summary.missing, vec![setup.missing.display().to_string()]);
    assert!(summary.bytes_after < summary.bytes_before);
    assert!(archive.is_file());
    // The project collected is left as it was.
    assert_eq!(
        rig.app().session.path.as_deref(),
        Some(setup.dir.join("song.canopy").as_path())
    );
    rig.app_mut().session.collect_summary = None;

    let into = setup.dir.join("elsewhere");
    std::fs::create_dir_all(&into).unwrap();
    rig.app_mut().session.unpack(archive, into.clone());
    wait_for_collecting(&mut rig);
    let opened = into.join("song").join("song.canopy");
    assert_eq!(rig.app().session.path.as_deref(), Some(opened.as_path()));

    // Every file it names is inside the new folder, and there, but the
    // missing one, which keeps its old path for Locate File….
    let folder = into.join("song");
    let project = rig.app().session.project.clone();
    let files: Vec<_> = project
        .files()
        .into_iter()
        .map(|(_, p)| p.to_owned())
        .filter(|p| *p != setup.missing)
        .collect();
    assert_eq!(files.len() + 1, project.files().len());
    for path in &files {
        assert!(path.is_file(), "{}", path.display());
    }
    let names: Vec<_> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&folder)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        names,
        [
            "Samples/tone.flac",
            "Samples/tone 2.flac",
            "Instruments/kit/kit.sfz",
            "Tables/table.wav",
            "Samples/loop.flac",
        ]
    );
    let sfz = folder.join("Instruments/kit/kit.sfz");
    let found = instruments::builtin::sampler::dependencies(&sfz).unwrap();
    assert!(found.missing.is_empty(), "{:?}", found.missing);
    assert_eq!(found.samples.len(), 1);

    // The FLACs play exactly what the WAVs did.
    rig.wait_for("the unpacked sources", |app| {
        app.session
            .project
            .sources
            .iter()
            .filter(|s| s.path.exists())
            .all(|s| matches!(app.session.source_state(s.id), Some(SourceState::Ready(_))))
    });
    assert_eq!(decoded(&rig, "tone.flac").audio, tone.audio);
    assert_eq!(decoded(&rig, "tone 2").audio, other_tone.audio);

    // Saved again, it stays relative to itself.
    assert!(rig.app_mut().session.save_as(&opened));
    let text = std::fs::read_to_string(&opened).unwrap();
    assert!(text.contains("\"Samples/tone.flac\""), "{text}");
}

#[test]
fn a_folder_keeps_the_files_as_they_were() {
    let mut rig = Rig::new("collect_folder");
    let setup = setup(&mut rig, "collect-folder");
    let parent = setup.dir.join("out");
    std::fs::create_dir_all(parent.join("song")).unwrap();
    rig.app_mut()
        .session
        .collect(Destination::Folder(parent.clone()));
    wait_for_collecting(&mut rig);

    // A folder of that name is there already, so it gets a number.
    let folder = parent.join("song 2");
    assert!(folder.join("Samples/tone.wav").is_file());
    assert!(folder.join("Samples/tone 2.wav").is_file());
    assert!(folder.join("Instruments/kit/Samples/hit.wav").is_file());
    assert_eq!(
        std::fs::read(folder.join("Samples/tone.wav")).unwrap(),
        std::fs::read(setup.dir.join("files/tone.wav")).unwrap(),
    );
    assert!(!parent.join(".song.collecting").exists());

    let text = std::fs::read_to_string(folder.join("song.canopy")).unwrap();
    let mut loaded = project::Project::from_file_string(&text).unwrap();
    assert!(loaded.resolve_relative_paths(&folder));
    let sources: Vec<_> = loaded
        .files()
        .into_iter()
        .filter(|(what, _)| matches!(what, FileRef::Source(_)))
        .map(|(_, p)| p.to_owned())
        .collect();
    assert_eq!(
        sources,
        [
            folder.join("Samples").join("tone.wav"),
            folder.join("Samples").join("tone 2.wav"),
            setup.missing.clone(),
        ]
    );
}

#[test]
fn cancelling_leaves_nothing_behind() {
    let mut rig = Rig::new("collect_cancel");
    let setup = setup(&mut rig, "collect-cancel");
    let archive = setup.dir.join("song.canopyz");
    let job = Job::detached();
    job.cancel_now();
    let project = rig.app().session.project.clone();
    let result = collect::collect(
        &project,
        "song",
        &Destination::Archive(archive.clone()),
        &job,
    );
    assert_eq!(result, Err("cancelled".to_owned()));
    assert!(!archive.exists());
    assert!(!setup.dir.join("song.canopyz.partial").exists());
}

/// A zip with `entries` (name, contents).
fn zip(path: &Path, entries: &[(&str, &str)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, contents) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn archives_that_reach_outside_their_folder_are_refused() {
    let dir = temp_dir("collect-unsafe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("evil.canopyz");
    zip(
        &archive,
        &[("song/song.canopy", "x"), ("song/../../escaped.txt", "x")],
    );
    let into = dir.join("into");
    std::fs::create_dir_all(&into).unwrap();
    let result = collect::unpack(&archive, &into, &Job::detached());
    assert!(result.is_err(), "{result:?}");
    assert!(!dir.join("escaped.txt").exists());
    assert_eq!(std::fs::read_dir(&into).unwrap().count(), 0);

    let plain = dir.join("plain.canopyz");
    zip(&plain, &[("readme.txt", "no project here")]);
    let result = collect::unpack(&plain, &into, &Job::detached());
    assert_eq!(result, Err("it holds no canopy project".to_owned()));
}

#[test]
fn loose_files_unpack_into_a_folder_named_after_the_archive() {
    let dir = temp_dir("collect-loose");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("Demo.canopyz");
    zip(&archive, &[("song.canopy", "x"), ("Samples/a.wav", "y")]);
    let opened = collect::unpack(&archive, &dir, &Job::detached()).unwrap();
    assert_eq!(opened, dir.join("Demo").join("song.canopy"));
    assert!(dir.join("Demo/Samples/a.wav").is_file());
}

#[test]
fn the_file_menu_opens_collect_and_save() {
    let mut rig = Rig::new("collect_menu");
    rig.harness.get_by_label("File").click();
    rig.frames(2);
    rig.harness.get_by_label("Collect and Save…").click();
    rig.frames(2);
    assert!(rig.app().collect_dialog.is_some());
    rig.harness.get_by_label("Compress").click();
    rig.frames(2);
    assert_eq!(
        rig.app().collect_dialog.as_ref().map(|d| d.compress),
        Some(false)
    );
    rig.screenshot("dialog");
    rig.harness.get_by_label("Cancel").click();
    rig.frames(2);
    assert!(rig.app().collect_dialog.is_none());
}

#[test]
fn an_edit_while_unpacking_keeps_the_project_open() {
    let mut rig = Rig::new("collect_edit_meanwhile");
    let dir = temp_dir("collect-meanwhile");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("song.canopyz");
    let text = project::Project::with_default_tracks()
        .to_file_string()
        .unwrap();
    zip(&archive, &[("song/song.canopy", &text)]);

    rig.app_mut().session.unpack(archive.clone(), dir.clone());
    let track = rig.app().session.project.track_ids()[0];
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(track, "Mine"))
        .unwrap();
    wait_for_collecting(&mut rig);
    assert_eq!(rig.app().session.project.track(track).unwrap().name, "Mine");
    assert!(dir.join("song/song.canopy").is_file(), "still unpacked");

    // Left alone, it opens.
    rig.app_mut().session.unpack(archive, dir.clone());
    wait_for_collecting(&mut rig);
    let opened = dir.join("song 2").join("song.canopy");
    assert_eq!(rig.app().session.path.as_deref(), Some(opened.as_path()));
}
