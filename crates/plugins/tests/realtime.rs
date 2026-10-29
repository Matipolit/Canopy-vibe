//! Aborts the test if the host allocates or frees memory on the audio
//! thread: through every processor call, with notes, raw MIDI, parameter
//! changes and modulation in the blocks, changes queued from the main
//! thread, sidechains, split blocks, plugin output events, resets and
//! stopping and starting. Allocation there can block on a lock inside the
//! allocator, which shows up as dropouts.
//!
//! Only the host's code is checked: each plugin is a separate library with
//! its own allocator. Only meaningful in debug builds: assert_no_alloc is a
//! no-op under --release.

mod common;

use common::*;
use plugins::{AudioConfig, Event, Host, Transport};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BLOCK: usize = 256;

/// A busy block: notes on, bent and off on several channels, controllers,
/// pitch bend, parameters, modulation, and all notes off at the end.
fn busy(format: Format) -> Vec<Event> {
    let mut events = Vec::new();
    for i in 0..32u32 {
        let frame = i * 8;
        let key = 40 + (i * 7 % 40) as u8;
        events.push(Event::NoteOn {
            frame,
            channel: (i % 3) as u8,
            key,
            velocity: 0.8,
        });
        if i >= 3 {
            events.push(Event::NoteOff {
                frame,
                channel: ((i - 3) % 3) as u8,
                key: 40 + ((i - 3) * 7 % 40) as u8,
                velocity: 0.5,
            });
        }
        events.push(Event::NoteTuning {
            frame,
            channel: (i % 3) as u8,
            key,
            semitones: f64::from(i) / 8.0 - 2.0,
        });
        events.push(Event::Midi {
            frame,
            data: [0xB0, 1, (i * 4) as u8],
        });
        events.push(Event::Midi {
            frame,
            data: [0xE0, 0, (i * 4) as u8],
        });
        events.push(param(GAIN, frame, format.db(-(i as f64) / 4.0)));
        events.push(Event::ParamMod {
            frame,
            id: GAIN,
            amount: format.db(-1.0) - format.db(0.0),
        });
    }
    // CC 7: the CLAP plugin answers with output events.
    events.push(Event::Midi {
        frame: 200,
        data: [0xB0, 7, 100],
    });
    events.push(Event::AllNotesOff {
        frame: BLOCK as u32 - 1,
    });
    events
}

#[test]
fn processing_never_allocates() {
    for format in FORMATS {
        for id in [format.gain, format.sine] {
            let host = Host::new();
            let mut instance = format.load(&host, id);
            let mut processor = instance
                .activate(AudioConfig::new(48_000.0, BLOCK as u32))
                .unwrap();
            let events = busy(format);
            let transport = Transport {
                playing: true,
                tempo: 140.0,
                loop_range: Some((0.0, 16.0)),
                ..Transport::STOPPED
            };
            let (mut left, mut right) = (vec![0.1f32; BLOCK * 3], vec![0.1f32; BLOCK * 3]);
            let side = vec![0.2f32; BLOCK];
            let mut out = [vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]];
            // Main-thread changes waiting in the queue.
            for i in 0..100 {
                instance.set_param(GAIN, format.db(-(i as f64) / 10.0));
            }
            assert_no_alloc::assert_no_alloc(|| {
                for _ in 0..20 {
                    processor.process_stereo(
                        &mut left[..BLOCK],
                        &mut right[..BLOCK],
                        None,
                        &events,
                        &transport,
                    );
                    let _ = processor.output_events();
                }
                // Longer than a block: split, with the events rebased.
                processor.process_stereo(
                    &mut left,
                    &mut right,
                    Some((&side, &side)),
                    &events,
                    &transport,
                );
                let [a, b] = &mut out;
                let mut outputs = [&mut a[..], &mut b[..]];
                processor.process(&[&side], &[&side, &side], &mut outputs, &events, &transport);
                if let Some(input) = processor.input_mut(0, 0) {
                    input[0] = 1.0;
                }
                processor.process_ports(BLOCK, &events, &transport);
                let _ = processor.output(0, 1);
                processor.reset();
                processor.process_stereo(&mut left[..0], &mut right[..0], None, &[], &transport);
                processor.stop_processing();
                processor.start_processing();
                processor.process_stereo(
                    &mut left[..BLOCK],
                    &mut right[..BLOCK],
                    None,
                    &[],
                    &transport,
                );
                processor.stop_processing();
            });
            host.idle();
            instance.deactivate(processor);
        }
    }
}
