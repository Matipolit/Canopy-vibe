//! Damaged or hand-edited files, odd paths, and rounding at clip edges.

use midi::{Pitch, Tick, Velocity};
use project::*;

#[test]
fn saving_a_non_utf8_path_is_an_error_not_a_crash() {
    let mut project = Project::with_default_tracks();
    let source = project.add_source(non_unicode_path(), 44_100, 44_100, 2);
    let track = project.track_ids()[2];
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    // Reported as an error rather than a crash that loses the session.
    let saved = std::panic::catch_unwind(|| project.to_file_string());
    assert!(saved.expect("to_file_string panicked").is_err());
}

/// A path that isn't valid Unicode: a stray Latin-1 byte on Unix.
#[cfg(unix)]
fn non_unicode_path() -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.wav").into()
}

/// A path that isn't valid Unicode: an unpaired surrogate on Windows.
#[cfg(windows)]
fn non_unicode_path() -> std::path::PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let name: Vec<u16> = "C:\\caf"
        .encode_utf16()
        .chain([0xD800])
        .chain(".wav".encode_utf16())
        .collect();
    std::ffi::OsString::from_wide(&name).into()
}

/// A big project (a few MB of text, like a large imported song) loads in
/// time proportional to its size. Reading the format version on its own
/// once took 89 s for this one (it now loads in 40 ms); the bound is loose,
/// so only that comes close.
#[test]
fn a_big_project_loads_quickly() {
    let mut project = Project::with_default_tracks();
    let track = project.track_ids()[0];
    let bar = project.tempo.bar_len();
    for i in 0..200 {
        let clip = project
            .add_midi_clip(track, bar * i, bar)
            .expect("an empty bar");
        let notes = (0..100)
            .map(|n| Note {
                id: project.new_note_id(),
                start: Tick::new(n * 38),
                len: Tick::new(30),
                pitch: Pitch::new(36 + (n % 48) as u8).unwrap(),
                velocity: Velocity::from_midi1(100),
                release_velocity: Velocity::CENTER,
                muted: false,
            })
            .collect();
        project.set_notes(clip, notes).unwrap();
    }
    let text = project.to_file_string().unwrap();
    assert!(text.len() > 2_000_000, "{} bytes", text.len());
    let started = std::time::Instant::now();
    let loaded = Project::from_file_string(&text).unwrap();
    let took = started.elapsed();
    assert_eq!(loaded.track_ids(), project.track_ids());
    assert!(took.as_secs_f32() < 10.0, "took {took:?}");
}

fn audio_project() -> (Project, TrackId, ClipId) {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/a.wav".into(), 48_000, 96_000, 2);
    let clip = project
        .add_audio_clip(track, Tick::new(3840), source)
        .unwrap();
    (project, track, clip)
}

#[test]
fn hand_edited_tempo_zero() {
    let (project, _, _) = audio_project();
    let text = project
        .to_file_string()
        .unwrap()
        .replace("tempo: 500000", "tempo: 0");
    assert!(text.contains("tempo: 0"));
    let loaded = Project::from_file_string(&text).unwrap();
    // An impossible tempo falls back to the default, and lengths are sane.
    assert_eq!(loaded.tempo.tempo.to_bpm(), 120.0);
    assert_eq!(
        loaded.end(),
        Tick::new(3840 * 2),
        "2 s of audio is one bar at 120 BPM"
    );
}

#[test]
fn hand_edited_empty_audio_clip() {
    let (project, _, clip) = audio_project();
    let text = project
        .to_file_string()
        .unwrap()
        .replace("source_len: 96000", "source_len: 0");
    let loaded = Project::from_file_string(&text).unwrap();
    assert!(
        loaded.clip(clip).is_none(),
        "an empty audio clip is dropped on load"
    );
}

#[test]
fn hand_edited_duplicate_track_ids() {
    let mut project = Project::new();
    let a = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let b = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let clip = project
        .add_midi_clip(b, Tick::ZERO, Tick::new(960))
        .unwrap();
    let text = project.to_file_string().unwrap();
    // Give the second track the first one's id, as a careless copy-paste would.
    let needle = format!("id: {}", b.0);
    let pos = text.find(&needle).unwrap();
    let text = format!("{}id: {}{}", &text[..pos], a.0, &text[pos + needle.len()..]);
    let mut loaded = Project::from_file_string(&text).unwrap();
    let ids = loaded.track_ids();
    assert_eq!(ids[0], a);
    assert_ne!(ids[1], a, "the duplicate got a fresh id");
    loaded.delete_clips(&[clip]).unwrap();
}

#[test]
fn split_halves_get_their_own_note_ids() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let clip = project
        .add_midi_clip(track, Tick::ZERO, Tick::new(3840))
        .unwrap();
    let notes: Vec<Note> = [0, 2000]
        .into_iter()
        .map(|s| Note {
            id: project.new_note_id(),
            start: Tick::new(s),
            len: Tick::new(100),
            pitch: Pitch::new(60).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
        })
        .collect();
    project.set_notes(clip, notes).unwrap();
    let right = project.split_clips(&[clip], Tick::new(1920)).unwrap()[0];
    let ids = |c| match project.clip(c).unwrap().1 {
        ClipRef::Midi(m) => m.notes.iter().map(|n| n.id).collect::<Vec<_>>(),
        _ => unreachable!(),
    };
    let (l, r) = (ids(clip), ids(right));
    assert!(
        l.iter().all(|id| !r.contains(id)),
        "left {l:?} right {r:?} share note ids"
    );
}

#[test]
fn trimming_an_audio_clip_never_pushes_its_end_later() {
    let mut found = 0;
    'outer: for frames in 44_100u64..44_400 {
        for x in [100i64, 333, 961, 1500] {
            let mut p = Project::new();
            let t = p.add_track(NewTrack::Audio, TrackPosition::END).unwrap();
            let long = p.add_source("/tmp/long.wav".into(), 44_100, frames, 2);
            let b = p.add_audio_clip(t, Tick::ZERO, long).unwrap();
            let b_end = p.clip_end(p.clip(b).unwrap().1);
            let c = p.add_audio_clip(t, b_end, long).unwrap();
            // A short clip dropped over the start of B trims B's left edge.
            let short = p.add_source("/tmp/short.wav".into(), 48_000, (x as u64) * 25, 2);
            p.add_audio_clip(t, Tick::ZERO, short).unwrap();
            let b_end2 = p.clip_end(p.clip(b).unwrap().1);
            let c_start = p.clip(c).unwrap().1.start();
            if b_end2 > c_start {
                eprintln!(
                    "frames {frames} x {x}: B end {b_end:?} -> {b_end2:?}, C starts {c_start:?}"
                );
                found += 1;
                if found > 3 {
                    break 'outer;
                }
            }
        }
    }
    assert_eq!(found, 0);
}
