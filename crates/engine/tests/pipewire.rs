//! canopy's own PipeWire stream, against the running daemon. Set
//! `CANOPY_TEST_PIPEWIRE=1` to run them (CI has no PipeWire); without it
//! they pass without doing anything.
//!
//! The allocator aborts the binary if the stream's callback allocates (in
//! debug builds), as `realtime_safety.rs` does for the engine.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use engine::device::{self, AudioOutput, AudioSettings};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn enabled() -> bool {
    std::env::var_os("CANOPY_TEST_PIPEWIRE").is_some()
}

/// Waits until the callback has run `count` more times.
fn wait_for_callbacks(output: &AudioOutput, count: u64) {
    let start = output.reading().callbacks;
    let begun = Instant::now();
    while output.reading().callbacks < start + count {
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "the stream stopped calling back: {:?}",
            output.problem()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn canopy_plays_through_a_pipewire_stream_of_its_own() {
    if !enabled() {
        return;
    }
    let settings = AudioSettings::new(48_000);
    let (_handle, output) = device::start(&settings).unwrap();
    assert_eq!(output.host_name, "PipeWire");
    assert_eq!(output.sample_rate, 48_000);
    wait_for_callbacks(&output, 20);
    let reading = output.reading();
    assert!(reading.frames > 0);
    assert!(reading.graph_rate.is_some());
    assert!(reading.latency > Duration::ZERO, "{reading:?}");
    assert_eq!(output.problem(), None);
}

#[test]
fn it_runs_at_the_rate_asked_for_whatever_the_graph_runs_at() {
    if !enabled() {
        return;
    }
    let (handle, output) = device::start(&AudioSettings::new(44_100)).unwrap();
    assert_eq!(handle.sample_rate(), 44_100.0);
    wait_for_callbacks(&output, 20);
}

#[test]
fn the_outputs_are_listed_and_one_can_be_pinned() {
    if !enabled() {
        return;
    }
    let (_handle, output) = device::start(&AudioSettings::new(48_000)).unwrap();
    // The registry fills in on PipeWire's thread, shortly after connecting.
    let begun = Instant::now();
    let mut outputs = output.outputs();
    while outputs.is_empty() && begun.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
        outputs = output.outputs();
    }
    assert!(!outputs.is_empty(), "this machine has an output");
    drop(output);

    let pinned = AudioSettings {
        output: Some(outputs[0].name.clone()),
        ..AudioSettings::new(48_000)
    };
    let (_handle, output) = device::start(&pinned).unwrap();
    assert_eq!(output.device_name, outputs[0].description);
    wait_for_callbacks(&output, 10);
}

#[test]
fn the_buffer_size_asked_for_is_what_the_graph_hands_over_when_its_free() {
    if !enabled() {
        return;
    }
    let settings = AudioSettings {
        buffer_frames: 128,
        ..AudioSettings::new(48_000)
    };
    let (_handle, output) = device::start(&settings).unwrap();
    wait_for_callbacks(&output, 20);
    // Another program may hold the graph at a larger size; never smaller.
    assert!(output.reading().frames >= 128, "{:?}", output.reading());
}

#[test]
fn starting_and_stopping_many_times_holds_up() {
    if !enabled() {
        return;
    }
    for _ in 0..20 {
        let (_handle, output) = device::start(&AudioSettings::new(48_000)).unwrap();
        wait_for_callbacks(&output, 2);
    }
}
