//! How long a frame of the UI takes on a big project, view by view.
//! Timing-dependent and in need of a project, so ignored by default:
//!
//! ```sh
//! cargo run --release -p engine --example bench -- --save target/bench
//! CANOPY_BENCH_PROJECT=target/bench/song.canopy \
//!     cargo test --release -p ui perf -- --ignored --nocapture
//! ```
//!
//! A frame here is the app's `update` plus egui's layout and tessellation,
//! at 1920×1080; the GPU isn't involved. Nothing is asserted: the numbers
//! are for comparing before and after a change.

use std::path::Path;
use std::time::Instant;

use eframe::egui::{Key, Modifiers, vec2};

use super::Rig;
use crate::session::SourceState;

/// Frames per measurement; `CANOPY_BENCH_FRAMES` overrides it (more for
/// a profiler to sample).
fn frames() -> usize {
    std::env::var("CANOPY_BENCH_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

/// Average and worst frame, in milliseconds (the audio rendered between
/// frames isn't counted). While `playing`, each frame comes with a device
/// callback's worth of audio, as it would with the transport rolling.
fn measure(rig: &mut Rig, what: &str, playing: bool) {
    let frames = frames();
    let mut worst = 0.0f64;
    let mut total = 0.0f64;
    for _ in 0..frames {
        if playing {
            let mut out = [0.0f32; 512 * 2];
            rig.engine.process_interleaved(&mut out, 2);
        }
        let frame = Instant::now();
        rig.frames(1);
        let took = frame.elapsed().as_secs_f64() * 1e3;
        total += took;
        worst = worst.max(took);
    }
    let average = total / frames as f64;
    println!("{what:<40} {average:>7.2} ms average {worst:>7.2} ms worst");
}

#[test]
#[ignore]
fn frame_times_on_a_big_project() {
    let Some(path) = std::env::var_os("CANOPY_BENCH_PROJECT") else {
        println!("set CANOPY_BENCH_PROJECT to a .canopy file (see the engine's bench example)");
        return;
    };
    let mut rig = Rig::with_size("perf", vec2(1920.0, 1080.0));
    rig.app_mut().session.open(Path::new(&path));
    rig.frames(2);
    rig.wait_for("audio to load", |app| {
        app.session.project.sources.iter().all(|s| {
            matches!(
                app.session.source_state(s.id),
                Some(SourceState::Ready(_) | SourceState::Failed(_))
            )
        })
    });
    rig.frames(5);
    let tracks = rig.app().session.project.track_ids().len();
    println!("{tracks} tracks at 1920×1080, {} frames each:", frames());

    measure(&mut rig, "arrangement, idle", false);
    rig.key(Modifiers::SHIFT, Key::Z);
    rig.frames(20);
    measure(&mut rig, "arrangement, whole song in view", false);
    rig.key(Modifiers::NONE, Key::Space);
    measure(&mut rig, "arrangement, playing", true);

    // The piano roll on the first clip of the first track that has one.
    let (track, clip) = {
        let project = &rig.app().session.project;
        let mut found = None;
        project.visit(|track, _| {
            if found.is_none()
                && let Some(clip) = track.clips().first()
            {
                found = Some((track.id, clip.id()));
            }
        });
        found.expect("a clip")
    };
    rig.app_mut().timeline.selection.open_clip = Some(clip);
    rig.app_mut().timeline.open_requested = true;
    rig.frames(5);
    measure(&mut rig, "piano roll, playing", true);

    // The device view: the first track's instrument and its effects.
    rig.app_mut().timeline.selection.tracks = vec![track];
    rig.key(Modifiers::SHIFT, Key::Tab);
    rig.frames(5);
    measure(&mut rig, "device view, playing", true);
    rig.key(Modifiers::NONE, Key::Space);
    rig.frames(5);
    measure(&mut rig, "device view, stopped", false);
}
