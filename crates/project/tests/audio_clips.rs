//! Reversing, looping, stretching, slipping, chopping and crossfading audio
//! clips.

use midi::Tick;
use project::*;

/// One second of audio is 1920 ticks at 120 BPM.
const SECOND: i64 = 1920;

fn setup() -> (Project, TrackId, SourceId) {
    let mut project = Project::new();
    project.set_tempo_bpm(120.0);
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/a.wav".into(), 48_000, 48_000, 2);
    (project, track, source)
}

fn audio(project: &Project, id: ClipId) -> AudioClip {
    match project.clip(id).unwrap().1 {
        ClipRef::Audio(clip) => clip.clone(),
        ClipRef::Midi(_) => panic!("not audio"),
    }
}

fn span(project: &Project, id: ClipId) -> (i64, i64) {
    let clip = project.clip(id).unwrap().1;
    (clip.start().get(), project.clip_end(clip).get())
}

#[test]
fn speed_and_tempo_sync_set_the_length() {
    let (mut p, track, source) = setup();
    let clip = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    assert_eq!(span(&p, clip), (0, SECOND));
    p.edit_audio_clip(clip, |c| c.stretch.speed = 2.0).unwrap();
    assert_eq!(span(&p, clip), (0, SECOND / 2));
    // Recorded at 60 BPM, synced to 120: twice as fast again.
    p.edit_audio_clip(clip, |c| {
        c.stretch.original_bpm = Some(60.0);
        c.stretch.sync = true;
    })
    .unwrap();
    assert_eq!(span(&p, clip), (0, SECOND / 4));
    // Out-of-range speeds are clamped rather than producing huge clips.
    p.edit_audio_clip(clip, |c| {
        c.stretch.sync = false;
        c.stretch.speed = 0.0;
    })
    .unwrap();
    assert_eq!(
        span(&p, clip),
        (0, (SECOND as f64 / Stretch::MIN_SPEED) as i64)
    );
}

#[test]
fn trimming_the_start_of_a_reversed_clip_cuts_the_end_of_the_file() {
    let (mut p, track, source) = setup();
    let clip = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    p.edit_audio_clip(clip, |c| c.reversed = true).unwrap();
    p.set_clip_bounds(clip, Tick::new(SECOND / 2), Tick::new(SECOND))
        .unwrap();
    let c = audio(&p, clip);
    assert_eq!((c.source_offset, c.source_len), (0, 24_000));
    // The clip's first sample is now the middle of the file, played backwards.
    assert_eq!(c.source_position(0.0), 24_000.0);
    // And it can be dragged back out to the end of the file, no further.
    let (earliest, _) = p.audio_clip_limits(&c);
    assert_eq!(earliest, Tick::ZERO);
}

#[test]
fn looped_clips_repeat_past_the_end_of_the_file() {
    let (mut p, track, source) = setup();
    let clip = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    p.edit_audio_clip(clip, |c| c.looped = true).unwrap();
    p.set_clip_bounds(clip, Tick::ZERO, Tick::new(3 * SECOND))
        .unwrap();
    let c = audio(&p, clip);
    assert_eq!(c.played(), 3 * 48_000);
    assert_eq!(c.source_len, 48_000, "the loop region itself is unchanged");
    assert_eq!(c.source_position(50_000.0), 2_000.0);
    // Trimming the start moves where in the loop playback begins.
    p.set_clip_bounds(clip, Tick::new(SECOND / 4), Tick::new(3 * SECOND))
        .unwrap();
    let c = audio(&p, clip);
    assert_eq!(c.loop_phase, 12_000);
    assert_eq!(c.source_position(0.0), 12_000.0);
    assert_eq!(span(&p, clip), (SECOND / 4, 3 * SECOND));
    // Turning the loop off goes back to one pass.
    p.edit_audio_clip(clip, |c| c.looped = false).unwrap();
    assert_eq!(span(&p, clip), (SECOND / 4, SECOND / 4 + SECOND));
}

#[test]
fn slipping_moves_the_audio_not_the_clip() {
    let (mut p, track, source) = setup();
    let clip = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    p.set_clip_bounds(clip, Tick::new(SECOND / 2), Tick::new(SECOND))
        .unwrap();
    assert_eq!(audio(&p, clip).source_offset, 24_000);
    // Audio moving later means hearing earlier parts of the file.
    p.slip_clip(clip, 12_000).unwrap();
    assert_eq!(audio(&p, clip).source_offset, 12_000);
    assert_eq!(span(&p, clip), (SECOND / 2, SECOND));
    // It stops at the start of the file.
    p.slip_clip(clip, 100_000).unwrap();
    assert_eq!(audio(&p, clip).source_offset, 0);
    // And at its end.
    p.slip_clip(clip, -100_000).unwrap();
    assert_eq!(audio(&p, clip).source_offset, 24_000);
}

#[test]
fn chopping_cuts_one_clip_into_contiguous_pieces() {
    let (mut p, track, source) = setup();
    let clip = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let cuts = [SECOND / 4, SECOND / 2, 3 * SECOND / 4, 5 * SECOND];
    let pieces = p.chop_clip(clip, &cuts.map(Tick::new)).unwrap();
    assert_eq!(pieces.len(), 4, "a cut outside the clip does nothing");
    let spans: Vec<_> = pieces.iter().map(|&c| span(&p, c)).collect();
    assert_eq!(spans, [(0, 480), (480, 960), (960, 1440), (1440, SECOND)]);
    let frames: u64 = pieces.iter().map(|&c| audio(&p, c).source_len).sum();
    assert_eq!(frames, 48_000);
}

#[test]
fn dropping_a_clip_over_another_ones_edge_crossfades_them() {
    let (mut p, track, source) = setup();
    let a = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let b = p
        .add_audio_clip(track, Tick::new(4 * SECOND), source)
        .unwrap();
    let to = |start| {
        [ClipMove {
            clip: b,
            track,
            start: Tick::new(start),
        }]
    };
    p.move_clips(&to(3 * SECOND / 4)).unwrap();
    // Both stay whole, and fade over the quarter second they share.
    assert_eq!(span(&p, a), (0, SECOND));
    assert_eq!(span(&p, b), (3 * SECOND / 4, 7 * SECOND / 4));
    let (ca, cb) = (audio(&p, a), audio(&p, b));
    assert_eq!((ca.fade_out, cb.fade_in), (12_000, 12_000));
    assert!(ca.crossfade_out && cb.crossfade_in);

    // Changing something that doesn't move the clip leaves the fade be.
    p.edit_audio_clip(b, |c| c.polarity_inverted = true)
        .unwrap();
    assert_eq!(audio(&p, b).fade_in, 12_000);

    // Pulled apart, the crossfade goes away.
    p.move_clips(&to(2 * SECOND)).unwrap();
    assert_eq!((audio(&p, a).fade_out, audio(&p, b).fade_in), (0, 0));

    // A hand-made fade stays, though.
    p.move_clips(&to(3 * SECOND / 4)).unwrap();
    p.set_clip_fades(b, 6_000, 0, 0.0, 0.0).unwrap();
    p.move_clips(&to(2 * SECOND)).unwrap();
    assert_eq!(audio(&p, b).fade_in, 6_000);
}

#[test]
fn covering_a_clip_still_trims_it() {
    let (mut p, track, source) = setup();
    let a = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let short = p.add_source("/tmp/b.wav".into(), 48_000, 12_000, 2);
    // Inside it: no crossfade, the long clip is cut around the short one.
    let b = p
        .add_audio_clip(track, Tick::new(SECOND / 2), short)
        .unwrap();
    let spans: Vec<_> = p
        .track(track)
        .unwrap()
        .clips()
        .iter()
        .map(|c| (c.start().get(), p.clip_end(*c).get()))
        .collect();
    assert_eq!(spans, [(0, 960), (960, 1440), (1440, SECOND)]);
    assert_eq!(audio(&p, b).fade_in, 0);
    assert_eq!(audio(&p, a).fade_out, 0);
}

#[test]
fn a_clip_dropped_into_a_crossfade_trims_rather_than_layering_three_deep() {
    let (mut p, track, source) = setup();
    let a = p.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let b = p
        .add_audio_clip(track, Tick::new(SECOND / 2), source)
        .unwrap();
    assert_eq!(audio(&p, a).fade_out, 24_000);
    let short = p.add_source("/tmp/b.wav".into(), 48_000, 4_800, 2);
    let c = p
        .add_audio_clip(track, Tick::new(3 * SECOND / 4), short)
        .unwrap();
    let (c_start, c_end) = span(&p, c);
    // Nothing else sounds under the new clip.
    for clip in p.track(track).unwrap().clips() {
        if clip.id() != c {
            let (s, e) = (clip.start().get(), p.clip_end(clip).get());
            assert!(
                e <= c_start || s >= c_end,
                "{s}..{e} under {c_start}..{c_end}"
            );
        }
    }
    // What's left of the crossfade before it still fades.
    assert!(audio(&p, b).fade_in > 0);
}
