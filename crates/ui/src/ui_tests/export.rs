//! Exporting the mix and tracks to WAV files.

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use media::WavFormat;
use midi::{Pitch, Tick, Velocity};
use project::Note;

use super::{Rig, peak, temp_dir, write_sine_wav};
use crate::export::{Settings, What};

#[test]
fn export_the_mix_and_each_track() {
    let mut rig = Rig::new("export");
    let dir = temp_dir("export");
    let wav = dir.join("tone.wav");
    write_sine_wav(&wav, 2, 220.0);
    let ids = rig.app().session.project.track_ids();
    let (midi, audio) = (ids[0], ids[2]);
    rig.app_mut().session.import_files(
        vec![wav],
        Some(audio),
        Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("the import", |app| !app.session.project.sources.is_empty());
    rig.app_mut().session.edit("Add", |p| {
        let clip = p.add_midi_clip(midi, Tick::ZERO, p.tempo.bar_len())?;
        let note = Note {
            id: p.new_note_id(),
            start: Tick::ZERO,
            len: Tick::new(960),
            pitch: Pitch::A4,
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        };
        p.set_notes(clip, vec![note])
    });
    // A master fader below unity: in the mix, but not in the track files.
    let mut master = rig.app().session.project.master;
    master.volume_db = -6.0;
    rig.app_mut().session.project.master = master;
    rig.frames(2);

    // Ctrl+R opens the dialog; shortcuts wait until it's closed.
    rig.key(Modifiers::COMMAND, Key::R);
    assert!(rig.app().export_dialog().is_some());
    rig.screenshot("dialog");
    rig.harness.get_by_label("Each track");
    rig.harness.get_by_label("Cancel").click();
    rig.frames(2);
    assert!(rig.app().export_dialog().is_none());

    // What "Export…" would do once a folder is picked.
    let settings = Settings {
        format: WavFormat::Float32,
        tail: true,
        loop_only: false,
    };
    let project = rig.app().session.project.clone();
    let mut dialog = crate::export::Dialog::new(What::Mix, vec![], settings);
    let mut jobs = dialog.jobs(&project, "song", dir.join("song.wav"));
    dialog.what = What::EachTrack;
    jobs.extend(dialog.jobs(&project, "song", dir.clone()));
    let names: Vec<String> = jobs
        .iter()
        .map(|j| j.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "song.wav",
            "song - 1 MIDI.wav",
            "song - 2 MIDI.wav",
            "song - 3 Audio.wav",
            "song - 4 Audio.wav"
        ]
    );
    rig.app_mut().session.export(jobs, settings);
    rig.wait_for("the export", |app| app.session.export_progress().is_none());
    rig.screenshot("exported");

    let read = |name: &str| media::decode(&dir.join(name)).unwrap().0;
    let mix = read("song.wav");
    let synth = read("song - 1 MIDI.wav");
    let tone = read("song - 3 Audio.wav");
    let silent = read("song - 2 MIDI.wav");
    assert!(peak(synth.left()) > 0.05, "the synth plays");
    assert!(peak(tone.left()) > 0.5, "the tone plays");
    assert_eq!(peak(silent.left()), 0.0, "an empty track is silent");
    // The song is two bars (the tone); the synth's file rings on no longer
    // than it has to, but still covers the song.
    assert_eq!(tone.frames(), 2 * 48_000);
    assert!(synth.frames() >= tone.frames());
    // The tracks add up to the mix, which also has the master's -6 dB.
    let master = dsp::db_to_gain(-6.0);
    for i in (0..tone.frames()).step_by(997) {
        let sum = synth.left()[i] + tone.left()[i];
        assert!(
            (mix.left()[i] - sum * master).abs() < 1e-4,
            "{i}: {} vs {}",
            mix.left()[i],
            sum * master
        );
    }

    // Just the loop (the second half of the tone), through a limiter:
    // exactly the loop's length, and in time, the limiter's lookahead taken
    // out.
    let tone_track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.edit("Add Effect", |p| {
        p.add_device(
            Some(tone_track),
            project::EffectRef::builtin("canopy.limiter"),
            None,
        )
    });
    let bar = rig.app().session.project.tempo.bar_len();
    rig.app_mut().session.set_loop(Some(project::LoopRegion {
        start: midi::Tick::new(bar.get() / 2),
        end: bar,
        on: true,
    }));
    let looped = Settings {
        loop_only: true,
        tail: false,
        ..settings
    };
    let job = crate::export::Job {
        target: crate::export::Target::Track(tone_track),
        path: dir.join("loop.wav"),
    };
    rig.app_mut().session.export(vec![job], looped);
    rig.wait_for("the loop's export", |app| {
        app.session.export_progress().is_none()
    });
    let loop_file = read("loop.wav");
    assert_eq!(loop_file.frames(), 48_000, "half a bar");
    assert!(
        loop_file.left()[..8].iter().any(|s| s.abs() > 0.01),
        "sound from the very first samples"
    );
    let _ = std::fs::remove_dir_all(dir);
}
