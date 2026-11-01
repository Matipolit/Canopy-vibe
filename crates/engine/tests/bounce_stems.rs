//! Each track's output kept from one offline pass (`Bounce::with_stems`),
//! for analysing tracks side by side.

use engine::{Bounce, Compiler};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::*;

const RATE: u32 = 48_000;

fn notes(project: &mut Project, track: TrackId, every: i64, low: u8) {
    let bar = project.tempo.bar_len();
    let clip = project.add_midi_clip(track, Tick::ZERO, bar).unwrap();
    let notes: Vec<Note> = (0..bar.get() / every)
        .map(|n| Note {
            id: project.new_note_id(),
            start: Tick::new(n * every),
            len: Tick::new(every / 2),
            pitch: Pitch::new(low + (n % 12) as u8).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        })
        .collect();
    project.set_notes(clip, notes).unwrap();
}

fn midi_track(project: &mut Project, every: i64, low: u8) -> TrackId {
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    notes(project, track, every, low);
    track
}

fn bounce(project: &Project, stems: &[TrackId]) -> (AudioBuffer, Vec<(TrackId, AudioBuffer)>) {
    let mut compiler = Compiler::new(RATE);
    let arrangement = compiler.compile(project, |_| None);
    let end = project.tempo.tick_to_sample(project.end(), RATE) as u64;
    let mut bounce =
        Bounce::range(arrangement, RATE, 0..end, true, compiler.latency()).with_stems(stems);
    while !bounce.step(4096) {}
    bounce.finish_with_stems()
}

/// Two tracks and a group of two, the second track a little quieter.
fn song() -> (Project, [TrackId; 3]) {
    let mut project = Project::new();
    let a = midi_track(&mut project, 240, 48);
    let b = midi_track(&mut project, 360, 60);
    let c = midi_track(&mut project, 480, 36);
    let d = midi_track(&mut project, 120, 72);
    let group = project.group_tracks(&[c, d]).unwrap();
    let quieter = Mixer {
        volume_db: -5.0,
        pan: 0.4,
        ..Mixer::default()
    };
    project.set_mixer(b, quieter).unwrap();
    (project, [a, b, group])
}

#[test]
fn top_level_stems_sum_to_the_mix_bit_for_bit() {
    let (project, top) = song();
    let (mix, stems) = bounce(&project, &top);
    assert_eq!(stems.len(), 3);
    assert!(mix.left().iter().any(|s| s.abs() > 0.01), "it plays");
    for (_, stem) in &stems {
        assert_eq!(stem.frames(), mix.frames());
    }
    for i in 0..mix.frames() {
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (_, stem) in &stems {
            left += stem.left()[i];
            right += stem.right()[i];
        }
        assert_eq!((left, right), (mix.left()[i], mix.right()[i]), "frame {i}");
    }
}

#[test]
fn a_groups_stem_is_its_children_through_its_fader() {
    let (mut project, [_, _, group]) = song();
    let half = Mixer {
        volume_db: -6.0,
        ..Mixer::default()
    };
    project.set_mixer(group, half).unwrap();
    let TrackKind::Group { children, .. } = &project.track(group).unwrap().kind else {
        unreachable!()
    };
    let children: Vec<TrackId> = children.iter().map(|t| t.id).collect();
    let (_, stems) = bounce(&project, &[group, children[0], children[1]]);
    let gain = volume_gain(-6.0);
    let (group_stem, c, d) = (&stems[0].1, &stems[1].1, &stems[2].1);
    for i in 0..group_stem.frames() {
        let expected = (c.left()[i] + d.left()[i]) * gain;
        assert!(
            (group_stem.left()[i] - expected).abs() < 1e-6,
            "frame {i}: {} vs {expected}",
            group_stem.left()[i]
        );
    }
}

#[test]
fn stems_skip_the_latency_like_the_mix() {
    let mut project = Project::new();
    let track = midi_track(&mut project, 240, 48);
    project
        .add_device(Some(track), EffectRef::builtin("canopy.limiter"), None)
        .unwrap();
    let mut compiler = Compiler::new(RATE);
    compiler.compile(&project, |_| None);
    assert!(compiler.latency() > 0, "the limiter looks ahead");
    let (mix, stems) = bounce(&project, &[track]);
    let stem = &stems[0].1;
    assert_eq!(stem.frames(), mix.frames());
    assert!(stem.left() == mix.left() && stem.right() == mix.right());
}

#[test]
fn a_track_not_in_the_song_has_no_stem() {
    let (project, [a, ..]) = song();
    let (_, stems) = bounce(&project, &[a, TrackId(999_999)]);
    assert_eq!(stems.len(), 1);
}
