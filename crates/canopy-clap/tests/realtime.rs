//! Aborts the test if the bridge allocates or frees memory while
//! processing, for every packaged instrument: notes, MIDI (the pedal, pitch
//! bend, controllers), automation, modulation, values sent from the main
//! thread, flushes and resets, in blocks of several sizes. Only
//! meaningful in debug builds: assert_no_alloc is a no-op under --release.

use std::sync::Arc;

use canopy_clap::bridge::{Bridge, Input, Transport};
use canopy_clap::values::Values;

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// A busy host block of `frames`: chords on and off, the pedal, bends and
/// the mod wheel, every parameter automated and the first modulated.
fn busy(frames: u32, params: usize) -> Vec<Input> {
    let mut events = Vec::new();
    for i in 0..32u32 {
        let frame = i * frames / 32;
        let key = 40 + (i * 7 % 40) as u8;
        events.push(Input::NoteOn {
            frame,
            key,
            velocity: 0.8,
        });
        if i >= 3 {
            events.push(Input::NoteOff {
                frame,
                key: Some(40 + ((i - 3) * 7 % 40) as u8),
                velocity: 0.5,
            });
        }
        let pedal = if i % 8 < 4 { 127 } else { 0 };
        events.push(Input::Midi {
            frame,
            data: [0xB0, 64, pedal],
        });
        events.push(Input::Midi {
            frame,
            data: [0xE0, 0, (i * 4) as u8],
        });
        events.push(Input::Midi {
            frame,
            data: [0xB0, 1, (i * 4) as u8],
        });
    }
    for index in 0..params {
        events.push(Input::Param {
            frame: frames - 1,
            index,
            value: 0.5,
        });
    }
    events.push(Input::ParamMod {
        frame: frames - 1,
        index: 0,
        amount: 0.1,
    });
    events.push(Input::Choke {
        frame: frames - 1,
        key: None,
    });
    events
}

#[test]
fn processing_never_allocates() {
    for instrument in canopy_clap::packaged() {
        let values = Arc::new(Values::new(instrument.params));
        let mut bridge = Bridge::new((instrument.create)(), values.clone());
        bridge.prepare(48_000.0);
        let transport = Some(Transport {
            tempo: Some(140.0),
            beat: Some(3.5),
            playing: true,
        });
        let mut left = vec![0.0f32; 1024];
        let mut right = vec![0.0f32; 1024];
        let blocks: Vec<(usize, Vec<Input>)> = [1usize, 64, 100, 1024]
            .into_iter()
            .map(|n| (n, busy(n as u32, instrument.params.len())))
            .collect();
        assert_no_alloc::assert_no_alloc(|| {
            for (frames, events) in &blocks {
                values.send(0, 0.25);
                bridge.process(
                    events.iter().copied(),
                    transport,
                    &mut left[..*frames],
                    &mut right[..*frames],
                );
                bridge.flush(events.iter().copied());
                bridge.reset();
            }
        });
        assert!(left.iter().all(|s| s.is_finite()), "{}", instrument.id);
    }
}
