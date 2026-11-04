//! Patchers in the engine: a chain holding a Patcher plays the effects
//! inside it where the Patcher stands, through its graph, and a macro's
//! automation moves the knobs it drives. Every block is played under
//! `assert_no_alloc`.

use std::collections::HashMap;
use std::sync::Arc;

use engine::{Command, Compiler, Engine, EngineHandle};
use media::AudioBuffer;
use midi::Tick;
use project::{
    AutomationLane, AutomationPoint, AutomationTarget, Chain, DeviceId, EffectRef, NewTrack,
    PATCHER, PortRef, Project, SourceId, TrackId, TrackPosition,
};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 48_000;
const SECOND: usize = RATE as usize;

/// An audio track playing noise from the start.
struct Rig {
    project: Project,
    track: TrackId,
    sources: HashMap<SourceId, Arc<AudioBuffer>>,
}

impl Rig {
    fn new() -> Rig {
        let input = noise(SECOND, 11);
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/patchers.wav".into(), RATE, SECOND as u64, 2);
        project.add_audio_clip(track, Tick::ZERO, source).unwrap();
        let sources = HashMap::from([(
            source,
            Arc::new(AudioBuffer::new(RATE, input.clone(), input)),
        )]);
        Rig {
            project,
            track,
            sources,
        }
    }

    fn add(&mut self, chain: Chain, effect: &str, params: &[(&str, f32)]) -> DeviceId {
        let id = self
            .project
            .add_device(chain, EffectRef::builtin(effect), None)
            .unwrap();
        for (param, value) in params {
            self.project.set_device_param(id, param, *value).unwrap();
        }
        id
    }

    fn patcher(&mut self) -> DeviceId {
        self.add(Chain::Track(self.track), PATCHER, &[])
    }

    /// A second of the track, played as a sound card would.
    fn render(&self) -> Vec<f32> {
        let (mut handle, mut engine): (EngineHandle, Engine) = engine::create(RATE as f32);
        let mut compiler = Compiler::new(RATE);
        let sources = &self.sources;
        let lookup = |id: SourceId| sources.get(&id).cloned();
        let arrangement = compiler.compile(&self.project, lookup);
        handle.send(Command::Arrangement(arrangement)).unwrap();
        handle.send(Command::SetPlaying(true)).unwrap();
        let mut out = vec![0.0; SECOND * 2];
        assert_no_alloc::assert_no_alloc(|| {
            for chunk in out.chunks_mut(256 * 2) {
                engine.process_interleaved(chunk, 2);
            }
        });
        out.chunks(2).map(|f| f[0]).collect()
    }
}

fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.max(1);
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.25
        })
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// How much louder `a` is than `b` over their second halves, in dB.
fn gain_db(a: &[f32], b: &[f32]) -> f32 {
    let half = a.len() / 2;
    20.0 * (rms(&a[half..]) / rms(&b[half..])).log10()
}

#[test]
fn a_patcher_sounds_exactly_like_its_effects_in_the_chain() {
    let mut plain = Rig::new();
    let track = Chain::Track(plain.track);
    plain.add(track, "canopy.eq", &[]);
    plain.add(track, "canopy.utility", &[("gain", -6.0)]);
    plain.add(track, "canopy.reverb", &[("mix", 30.0)]);

    let mut patched = Rig::new();
    let track = Chain::Track(patched.track);
    patched.add(track, "canopy.eq", &[]);
    let patcher = patched.patcher();
    patched.add(Chain::Patch(patcher), "canopy.utility", &[("gain", -6.0)]);
    patched.add(Chain::Patch(patcher), "canopy.reverb", &[("mix", 30.0)]);

    assert_eq!(plain.render(), patched.render());
}

#[test]
fn a_bypassed_patcher_lets_the_sound_through() {
    let dry = Rig::new().render();
    let mut rig = Rig::new();
    let patcher = rig.patcher();
    rig.add(Chain::Patch(patcher), "canopy.utility", &[("gain", -24.0)]);
    rig.project.set_device_bypassed(patcher, true).unwrap();
    assert_eq!(rig.render(), dry);
}

#[test]
fn a_dry_path_inside_a_patcher_sums_with_the_effects() {
    let dry = Rig::new().render();
    let mut rig = Rig::new();
    let patcher = rig.patcher();
    let chain = Chain::Patch(patcher);
    rig.add(chain, "canopy.utility", &[("mute", 1.0)]);
    // Input straight to Output, at -6 dB, beside the muted utility.
    let graph = rig.project.ensure_routing(chain).unwrap().clone();
    let (input, output) = (graph.input().unwrap(), graph.output().unwrap());
    let key = rig
        .project
        .connect(
            chain,
            PortRef {
                node: input,
                port: 0,
            },
            PortRef {
                node: output,
                port: 0,
            },
        )
        .unwrap();
    rig.project.set_cable(chain, key, -6.0, false).unwrap();
    let wet = rig.render();
    assert!((gain_db(&wet, &dry) + 6.0).abs() < 0.05, "{}", gain_db(&wet, &dry));
}

#[test]
fn a_macros_automation_moves_the_knob_it_drives() {
    // Macro 1 sweeps the utility's gain over its whole range (-60..24 dB);
    // held at 50%, that's -18 dB.
    let mut rig = Rig::new();
    let patcher = rig.patcher();
    let utility = rig.add(Chain::Patch(patcher), "canopy.utility", &[]);
    rig.project
        .link_macro(patcher, 0, utility, "gain", 0.0, 1.0)
        .unwrap();
    let lane = AutomationLane {
        target: AutomationTarget::DeviceParam {
            device: patcher,
            param: "macro_1".into(),
        },
        points: vec![AutomationPoint {
            tick: Tick::ZERO,
            value: 0.5,
            curve: 0.0,
        }]
        .into(),
    };
    rig.project.track_mut(rig.track).unwrap().automation.push(lane);
    // The knob itself sits elsewhere: the lane is what plays.
    rig.project.set_device_param(utility, "gain", 0.0).unwrap();
    let automated = rig.render();

    let mut plain = Rig::new();
    plain.add(Chain::Track(plain.track), "canopy.utility", &[("gain", -18.0)]);
    let expected = plain.render();
    assert!(
        gain_db(&automated, &expected).abs() < 0.05,
        "{}",
        gain_db(&automated, &expected)
    );
}
