//! Reversed, stretched, re-channelled and consolidated audio clips.

use std::sync::Arc;

use engine::{AudioLookup, Command, Compiler, Engine, EngineHandle, clip_process, create};
use media::AudioBuffer;
use midi::Tick;
use project::*;

const RATE: u32 = 48_000;

/// Finds sources, and renders clips on the spot the way the UI's background
/// renderer would.
struct Lookup {
    source: SourceId,
    audio: Arc<AudioBuffer>,
    project: Project,
}

impl AudioLookup for Lookup {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        (id == self.source).then(|| self.audio.clone())
    }
    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        let spec = clip_process(&self.project, clip, RATE)?;
        Some(Arc::new(media::process_clip(&self.audio, &spec)))
    }
}

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    compiler: Compiler,
    project: Project,
    track: TrackId,
    source: SourceId,
    audio: Arc<AudioBuffer>,
}

impl Rig {
    /// One second of a stereo ramp: left rises 0..1, right falls 0..-1.
    fn new() -> Rig {
        let (handle, engine) = create(RATE as f32);
        let mut project = Project::new();
        project.set_tempo_bpm(120.0);
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/ramp.wav".into(), RATE, RATE as u64, 2);
        let left: Vec<f32> = (0..RATE).map(|i| i as f32 / RATE as f32).collect();
        let right = left.iter().map(|x| -x).collect();
        Rig {
            handle,
            engine,
            compiler: Compiler::new(RATE),
            project,
            track,
            source,
            audio: Arc::new(AudioBuffer::new(RATE, left, right)),
        }
    }

    fn lookup(&self) -> Lookup {
        Lookup {
            source: self.source,
            audio: self.audio.clone(),
            project: self.project.clone(),
        }
    }

    fn clip(&mut self) -> ClipId {
        self.project
            .add_audio_clip(self.track, Tick::ZERO, self.source)
            .unwrap()
    }

    /// Plays from the start, on a fresh engine: (left, right).
    fn play(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        (self.handle, self.engine) = create(RATE as f32);
        self.compiler = Compiler::new(RATE);
        let arrangement = self.compiler.compile(&self.project, self.lookup());
        self.handle.send(Command::Arrangement(arrangement)).unwrap();
        self.handle.send(Command::SetPlaying(true)).unwrap();
        let mut out = vec![0.0; frames * 2];
        self.engine.process_interleaved(&mut out, 2);
        self.handle.collect_garbage();
        (
            out.iter().step_by(2).copied().collect(),
            out.iter().skip(1).step_by(2).copied().collect(),
        )
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn reversed_clips_play_backwards() {
    let mut rig = Rig::new();
    let clip = rig.clip();
    rig.project
        .edit_audio_clip(clip, |c| c.reversed = true)
        .unwrap();
    let (left, right) = rig.play(RATE as usize);
    for i in [1000, 24_000, 47_000] {
        let expected = (RATE - 1 - i) as f32 / RATE as f32;
        assert!(
            close(left[i as usize], expected),
            "{i}: {} vs {expected}",
            left[i as usize]
        );
        assert!(close(right[i as usize], -expected));
    }
}

#[test]
fn resampled_clips_play_faster_and_fade_in_proportion() {
    let mut rig = Rig::new();
    let clip = rig.clip();
    rig.project
        .edit_audio_clip(clip, |c| {
            c.stretch.speed = 2.0;
            c.fade_in = 24_000;
        })
        .unwrap();
    let (left, _) = rig.play(RATE as usize);
    // Twice as fast: half a second long, reading two frames per frame...
    let ramp = |i: usize| (2 * i) as f32 / RATE as f32;
    assert!(
        close(left[18_000], ramp(18_000)),
        "{} vs {}",
        left[18_000],
        ramp(18_000)
    );
    assert_eq!(left[24_100], 0.0, "over after half a second");
    // ...and the 24000 frames of material fading in take 12000 frames.
    for i in [3_000, 6_000, 9_000] {
        let faded = ramp(i) * (i + 1) as f32 / 12_000.0;
        assert!(close(left[i], faded), "{i}: {} vs {faded}", left[i]);
    }
}

#[test]
fn polarity_and_channel_modes() {
    let mut rig = Rig::new();
    let clip = rig.clip();
    let at = 30_000;
    let x = at as f32 / RATE as f32;
    rig.project
        .edit_audio_clip(clip, |c| c.polarity_inverted = true)
        .unwrap();
    let (left, right) = rig.play(RATE as usize);
    assert!(close(left[at], -x) && close(right[at], x));

    rig.project
        .edit_audio_clip(clip, |c| {
            c.polarity_inverted = false;
            c.channels = ChannelMode::Swap;
        })
        .unwrap();
    let (left, right) = rig.play(RATE as usize);
    assert!(close(left[at], -x) && close(right[at], x));

    rig.project
        .edit_audio_clip(clip, |c| c.channels = ChannelMode::Mono)
        .unwrap();
    let (left, right) = rig.play(RATE as usize);
    assert!(
        close(left[at], 0.0) && close(right[at], 0.0),
        "the ramps cancel"
    );
}

#[test]
fn what_to_render_ignores_moves_and_unsynced_tempo_changes() {
    let mut rig = Rig::new();
    let clip = rig.clip();
    let spec = |p: &Project| {
        let ClipRef::Audio(c) = p.clip(clip).unwrap().1 else {
            unreachable!()
        };
        clip_process(p, c, RATE)
    };
    assert_eq!(spec(&rig.project), None, "a plain clip needs no rendering");
    rig.project
        .edit_audio_clip(clip, |c| c.stretch.speed = 1.5)
        .unwrap();
    let before = spec(&rig.project).unwrap();
    assert_eq!(before.out_len, 32_000);
    let track = rig.track;
    rig.project
        .move_clips(&[ClipMove {
            clip,
            track,
            start: Tick::new(3840),
        }])
        .unwrap();
    rig.project.set_tempo_bpm(90.0);
    assert_eq!(spec(&rig.project), Some(before.clone()));
    // Synced to the tempo, it does change.
    rig.project
        .edit_audio_clip(clip, |c| {
            c.stretch.original_bpm = Some(90.0);
            c.stretch.sync = true;
        })
        .unwrap();
    rig.project.set_tempo_bpm(180.0);
    assert!((spec(&rig.project).unwrap().speed - 3.0).abs() < 1e-4);
}

#[test]
fn consolidating_mixes_clips_and_gaps_into_one() {
    let mut rig = Rig::new();
    let a = rig.clip();
    // Half a second of silence after the first clip, then a reversed copy.
    let b = rig
        .project
        .add_audio_clip(rig.track, Tick::new(3 * 1920 / 2), rig.source)
        .unwrap();
    rig.project
        .edit_audio_clip(b, |c| c.reversed = true)
        .unwrap();
    let lookup = rig.lookup();
    let glued = rig
        .compiler
        .render_audio_clips(&rig.project, &[a, b], lookup)
        .unwrap();
    assert_eq!(glued.start, Tick::ZERO);
    let audio = glued.audio;
    assert_eq!(audio.frames(), (RATE * 5 / 2) as usize);
    let left = audio.left();
    assert!(close(left[30_000], 30_000.0 / RATE as f32));
    assert_eq!(left[60_000], 0.0, "the gap stays silent");
    assert!(close(
        left[72_000 + 1000],
        (RATE - 1 - 1000) as f32 / RATE as f32
    ));
}
