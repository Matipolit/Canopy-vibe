//! The test plugins, every format, through the whole host: activation,
//! sample-accurate parameters and notes, latency and restarts, state,
//! modulation, the plugin changing its own parameters, transport,
//! sidechains, and processors going back and forth between threads.

mod common;

use common::*;
use plugins::{
    AudioConfig, Event, Host, PluginEvent, PluginFormat, ProcessStatus, Tail, Transport, scan,
};

const RATE: f64 = 48_000.0;

fn config(max_block: u32) -> AudioConfig {
    AudioConfig::new(RATE, max_block)
}

#[test]
fn gain_changes_at_the_exact_sample() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let mut processor = gain.activate(config(256)).unwrap();
        let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
        let events = [param(GAIN, 100, format.db(-6.0))];
        let status =
            processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert_eq!(status, ProcessStatus::Continue, "{format:?}");
        assert!(
            close(left[99], 1.0) && close(right[99], 1.0),
            "{format:?}: {}",
            left[99]
        );
        assert!(close(left[100], linear(-6.0)), "{format:?}: {}", left[100]);
        assert!(close(right[255], linear(-6.0)), "{format:?}");
        gain.deactivate(processor);
    }
}

#[test]
fn long_blocks_are_split_and_keep_their_timing() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let mut processor = gain.activate(config(64)).unwrap();
        let (mut left, mut right) = (vec![1.0f32; 300], vec![1.0f32; 300]);
        let events = [param(GAIN, 200, format.db(-6.0))];
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert!(close(left[199], 1.0), "{format:?}");
        assert!(close(left[200], linear(-6.0)), "{format:?}: {}", left[200]);
        gain.deactivate(processor);
    }
}

/// Where the first sample louder than half is.
fn first_loud(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|s| s.abs() > 0.5)
}

fn impulse_delay(processor: &mut plugins::Processor) -> Option<usize> {
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    left[0] = 1.0;
    right[0] = 1.0;
    processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
    first_loud(&left)
}

#[test]
fn reported_latency_matches_the_delay() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        gain.set_param(LATENCY, format.samples(64));
        let mut processor = gain.activate(config(512)).unwrap();
        assert_eq!(gain.latency(), 64, "{format:?}");
        assert_eq!(impulse_delay(&mut processor), Some(64), "{format:?}");
        assert_eq!(gain.tail(), Tail::Samples(0));
        gain.deactivate(processor);
    }
}

#[test]
fn a_latency_change_while_active_asks_for_a_restart() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let mut processor = gain.activate(config(512)).unwrap();
        assert_eq!(gain.latency(), 0);
        gain.set_param(LATENCY, format.samples(128));
        // The change reaches the processor with the next block.
        assert_eq!(
            impulse_delay(&mut processor),
            Some(0),
            "{format:?}: latency holds until restart"
        );
        host.idle();
        let events = gain.take_events();
        assert!(
            events.contains(&PluginEvent::RestartRequested),
            "{format:?}: {events:?}"
        );
        assert!(gain.needs_restart());

        processor.stop_processing();
        let mut processor = gain.restart(processor).unwrap();
        assert!(!gain.needs_restart());
        assert_eq!(gain.latency(), 128, "{format:?}");
        assert!(
            gain.take_events()
                .contains(&PluginEvent::LatencyChanged(128)),
            "{format:?}"
        );
        assert_eq!(impulse_delay(&mut processor), Some(128), "{format:?}");
        gain.deactivate(processor);
    }
}

#[test]
fn changes_queued_but_not_processed_survive_a_restart() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let processor = gain.activate(config(512)).unwrap();
        gain.set_param(LATENCY, format.samples(32));
        // No block ran: the change is still in the processor's queue.
        let mut processor = gain.restart(processor).unwrap();
        assert_eq!(gain.latency(), 32, "{format:?}");
        assert_eq!(impulse_delay(&mut processor), Some(32), "{format:?}");
        gain.deactivate(processor);
    }
}

fn render(processor: &mut plugins::Processor, frames: usize) -> Vec<f32> {
    let (mut left, mut right) = (vec![0.5f32; frames], vec![0.5f32; frames]);
    left[0] = 1.0;
    processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
    left
}

#[test]
fn state_round_trips() {
    for format in FORMATS {
        let host = Host::new();
        let mut first = format.load(&host, format.gain);
        // Set while inactive, so saving must get them to the plugin first.
        first.set_param(GAIN, format.db(-12.0));
        first.set_param(LATENCY, format.samples(32));
        let state = first.save_state().unwrap();

        let mut second = format.load(&host, format.gain);
        assert!((second.param_value(GAIN).unwrap() - format.db(0.0)).abs() < format.eps());
        second.load_state(&state).unwrap();
        assert!(
            (second.param_value(GAIN).unwrap() - format.db(-12.0)).abs() < format.eps(),
            "{format:?}: the controller has the loaded state"
        );
        assert!(
            second
                .take_events()
                .contains(&PluginEvent::ParamsRescanned { infos: false })
        );

        let mut a = first.activate(config(256)).unwrap();
        let mut b = second.activate(config(256)).unwrap();
        assert_eq!((first.latency(), second.latency()), (32, 32), "{format:?}");
        assert_eq!(render(&mut a, 256), render(&mut b, 256), "{format:?}");
        assert!(close(render(&mut b, 256)[100], 0.5 * linear(-12.0)));

        // Saved while active it's the same.
        assert_eq!(second.save_state().unwrap(), state, "{format:?}");
        assert!(second.load_state(b"not a state").is_err());
        first.deactivate(a);
        second.deactivate(b);
    }
}

#[test]
fn parameters_are_described() {
    // VST2's are in `vst2_parameters_are_described`: no read-only flag, and
    // text for the current value only.
    for format in [CLAP, VST3] {
        let host = Host::new();
        let gain = format.load(&host, format.gain);
        let params = gain.params();
        let find = |id| params.iter().find(|p| p.id == id).unwrap();
        let g = find(GAIN);
        assert_eq!(g.name, "Gain");
        assert!(g.flags.automatable && !g.flags.read_only);
        assert_eq!(g.flags.modulatable, format.format == PluginFormat::Clap);
        assert!((g.default - format.db(0.0)).abs() < 1e-9);
        let latency = find(LATENCY);
        assert!(latency.flags.stepped);
        assert_eq!(latency.step_count, 1024, "{format:?}");
        assert!(find(TEMPO_SEEN).flags.read_only);
        let text = gain.param_text(GAIN, format.db(-6.0)).unwrap();
        assert_eq!(text, "-6.0 dB", "{format:?}");
        let back = gain.param_from_text(GAIN, "-6").unwrap();
        assert!((back - format.db(-6.0)).abs() < 1e-9, "{format:?}");
        match format.format {
            PluginFormat::Clap => assert_eq!(g.module, "Main"),
            PluginFormat::Vst3 | PluginFormat::Vst2 => assert_eq!(g.unit, "dB"),
        }
    }
}

#[test]
fn the_transport_reaches_the_plugin() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let mut processor = gain.activate(config(128)).unwrap();
        let transport = Transport {
            playing: true,
            tempo: 133.0,
            beat: 8.0,
            bar_start: 8.0,
            bar_number: 2,
            sample_position: 96_000,
            ..Transport::STOPPED
        };
        let (mut left, mut right) = (vec![0.0f32; 128], vec![0.0f32; 128]);
        processor.process_stereo(&mut left, &mut right, None, &[], &transport);
        host.idle();
        let seen = gain.param_value(TEMPO_SEEN).unwrap();
        assert!(
            (seen - format.bpm(133.0)).abs() < format.eps(),
            "{format:?}: {seen}"
        );
        let events = gain.take_events();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PluginEvent::ParamValue { id, .. } if *id == TEMPO_SEEN)),
            "{format:?}: {events:?}"
        );
        gain.deactivate(processor);
    }
}

#[test]
fn modulation_is_native_in_clap_and_emulated_elsewhere() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let mut processor = gain.activate(config(256)).unwrap();
        let amount = format.db(-6.0) - format.db(0.0);
        let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
        let events = [Event::ParamMod {
            frame: 64,
            id: GAIN,
            amount,
        }];
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert!(close(left[63], 1.0), "{format:?}");
        assert!(close(left[64], linear(-6.0)), "{format:?}: {}", left[64]);

        // Automation moves the base; the modulation stays on top.
        let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
        let events = [param(GAIN, 0, format.db(-12.0))];
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert!(close(left[0], linear(-18.0)), "{format:?}: {}", left[0]);

        // An amount of 0 ends it.
        let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
        let events = [Event::ParamMod {
            frame: 0,
            id: GAIN,
            amount: 0.0,
        }];
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert!(close(left[0], linear(-12.0)), "{format:?}: {}", left[0]);

        host.idle();
        if format.format == PluginFormat::Clap {
            // Non-destructive: the parameter never moved, and the plugin saw
            // the amounts.
            assert!((gain.param_value(GAIN).unwrap() + 12.0).abs() < 1e-9);
            assert!(gain.take_events().contains(&PluginEvent::ParamValue {
                id: MOD_SEEN,
                value: amount
            }));
        }
        gain.deactivate(processor);
    }
}

/// Leaves out the tempo the test plugins report on their first block.
fn without_tempo(mut events: Vec<PluginEvent>) -> Vec<PluginEvent> {
    events.retain(|e| !matches!(e, PluginEvent::ParamValue { id, .. } if *id == TEMPO_SEEN));
    events
}

#[test]
fn clap_plugins_change_their_own_parameters_in_gestures() {
    let host = Host::new();
    let mut gain = CLAP.load(&host, CLAP.gain);
    let mut processor = gain.activate(config(256)).unwrap();
    let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
    // CC 7 at full: the plugin sets its own gain to +12 dB.
    let events = [Event::Midi {
        frame: 10,
        data: [0xB0, 7, 127],
    }];
    processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
    assert!(close(left[9], 1.0) && close(left[10], linear(12.0)));
    host.idle();
    let events = without_tempo(gain.take_events());
    assert_eq!(
        events,
        vec![
            PluginEvent::ParamGesture {
                id: GAIN,
                begin: true
            },
            PluginEvent::ParamValue {
                id: GAIN,
                value: 12.0
            },
            PluginEvent::ParamGesture {
                id: GAIN,
                begin: false
            },
        ]
    );
    assert_eq!(gain.param_value(GAIN), Some(12.0));
    gain.deactivate(processor);
}

#[test]
fn vst3_editor_edits_reach_the_processor() {
    let host = Host::new();
    let mut gain = VST3.load(&host, VST3.gain);
    let mut processor = gain.activate(config(256)).unwrap();
    gain.set_param(GAIN, VST3.db(-12.0));
    assert!(close(render(&mut processor, 256)[100], 0.5 * linear(-12.0)));
    gain.take_events();
    // What an editor's reset button does: begin, perform, end.
    gain.set_param(RESET, 1.0);
    host.idle();
    let events = without_tempo(gain.take_events());
    assert_eq!(
        events,
        vec![
            PluginEvent::ParamGesture {
                id: GAIN,
                begin: true
            },
            PluginEvent::ParamValue {
                id: GAIN,
                value: VST3.db(0.0)
            },
            PluginEvent::ParamGesture {
                id: GAIN,
                begin: false
            },
        ]
    );
    // The host forwarded the edit to the processor.
    assert!(
        close(render(&mut processor, 256)[100], 0.5),
        "{}",
        render(&mut processor, 256)[100]
    );
    gain.deactivate(processor);
}

#[test]
fn vst3_midi_controllers_become_mapped_parameters() {
    let host = Host::new();
    let mut gain = VST3.load(&host, VST3.gain);
    let mut processor = gain.activate(config(256)).unwrap();
    let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
    let events = [Event::Midi {
        frame: 50,
        data: [0xB0, 1, 127],
    }];
    processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
    assert!(close(left[49], 1.0));
    assert!(close(left[50], linear(12.0)), "{}", left[50]);
    gain.deactivate(processor);
}

#[test]
fn vst3_component_and_controller_talk_through_the_host() {
    let host = Host::new();
    let mut gain = VST3.load(&host, VST3.gain);
    let processor = gain.activate(config(256)).unwrap();
    // The component messaged the controller when activated.
    assert!(gain.param_value(MESSAGES).unwrap() > 0.0);
    gain.deactivate(processor);
}

#[test]
fn the_sidechain_is_mixed_in() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        assert!(gain.info().aux_input().is_some());
        let mut processor = gain.activate(config(128)).unwrap();
        let (mut left, mut right) = (vec![0.5f32; 128], vec![0.5f32; 128]);
        let side = vec![0.25f32; 128];
        processor.process_stereo(
            &mut left,
            &mut right,
            Some((&side, &side)),
            &[],
            &Transport::STOPPED,
        );
        assert!(
            close(left[10], 0.75) && close(right[127], 0.75),
            "{format:?}: {}",
            left[10]
        );

        // The same through separate inputs and outputs; one input channel
        // feeds both of the plugin's.
        let input = vec![0.5f32; 128];
        let (mut out_l, mut out_r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
        let mut outputs = [&mut out_l[..], &mut out_r[..]];
        processor.process(
            &[&input],
            &[&side, &side],
            &mut outputs,
            &[],
            &Transport::STOPPED,
        );
        assert!(close(out_l[5], 0.75) && close(out_r[5], 0.75), "{format:?}");
        gain.deactivate(processor);
    }
}

/// Zero crossings per second of a signal: twice its frequency.
fn crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count()
}

#[test]
fn instruments_play_notes_at_their_frames() {
    for format in FORMATS {
        let host = Host::new();
        let mut sine = format.load(&host, format.sine);
        assert_eq!(sine.info().kind, plugins::PluginKind::Instrument);
        let mut processor = sine.activate(config(512)).unwrap();
        let frames = RATE as usize;
        let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let on = Event::NoteOn {
            frame: 32,
            channel: 0,
            key: 69,
            velocity: 1.0,
        };
        processor.process_stereo(&mut left, &mut right, None, &[on], &Transport::STOPPED);
        assert!(
            left[..32].iter().all(|s| *s == 0.0),
            "{format:?}: silent before the note"
        );
        assert!(left[33] != 0.0, "{format:?}: sounding from its frame");
        let hz = crossings(&left[32..]) as f32 / 2.0 / ((frames - 32) as f32 / RATE as f32);
        assert!((hz - 440.0).abs() < 2.0, "{format:?}: {hz} Hz");

        let off = Event::NoteOff {
            frame: 100,
            channel: 0,
            key: 69,
            velocity: 0.5,
        };
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        processor.process_stereo(&mut left, &mut right, None, &[off], &Transport::STOPPED);
        assert!(left[..100].iter().any(|s| *s != 0.0));
        assert!(
            left[100..].iter().all(|s| *s == 0.0),
            "{format:?}: silent after the note off"
        );

        // All notes off ends every held note, raw MIDI notes included.
        let events = [
            Event::NoteOn {
                frame: 0,
                channel: 0,
                key: 60,
                velocity: 1.0,
            },
            Event::Midi {
                frame: 0,
                data: [0x90, 64, 100],
            },
            Event::AllNotesOff { frame: 200 },
        ];
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert!(left[..200].iter().any(|s| *s != 0.0));
        assert!(
            left[200..].iter().all(|s| *s == 0.0),
            "{format:?}: all notes off"
        );
        assert_eq!(
            sine.tail(),
            if format.format == PluginFormat::Vst3 {
                Tail::Infinite
            } else {
                // VST2: the plugin doesn't say.
                Tail::Samples(0)
            }
        );
        sine.deactivate(processor);
    }
}

#[test]
fn notes_a_plugin_sends_come_out_in_order() {
    for format in [CLAP, VST2] {
        let host = Host::new();
        let mut sine = format.load(&host, format.sine);
        assert_eq!(sine.info().note_outputs.len(), 1);
        let mut processor = sine.activate(config(64)).unwrap();
        let (mut left, mut right) = (vec![0.0f32; 200], vec![0.0f32; 200]);
        let events = [
            Event::NoteOn {
                frame: 10,
                channel: 1,
                key: 60,
                velocity: 1.0,
            },
            Event::NoteOff {
                frame: 150,
                channel: 1,
                key: 60,
                velocity: 0.5,
            },
        ];
        // VST2 notes are MIDI: velocities in 127ths.
        let release = if format.format == PluginFormat::Vst2 {
            64.0 / 127.0
        } else {
            0.5
        };
        // Split into blocks of 64: the echoes keep their place in the whole.
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        assert_eq!(
            processor.output_events(),
            [
                Event::NoteOn {
                    frame: 10,
                    channel: 1,
                    key: 72,
                    velocity: 1.0
                },
                Event::NoteOff {
                    frame: 150,
                    channel: 1,
                    key: 72,
                    velocity: release
                },
            ],
            "{format:?}"
        );
        processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
        assert!(processor.output_events().is_empty(), "cleared each block");
        sine.deactivate(processor);
    }
}

#[test]
fn processors_can_go_to_another_thread_and_be_dropped_there() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let processor = gain.activate(config(256)).unwrap();
        // The "audio thread" processes and then drops it (which the engine
        // shouldn't, but it mustn't break anything).
        std::thread::spawn(move || {
            let mut processor = processor;
            let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
            processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
            assert!(close(left[0], 1.0));
        })
        .join()
        .unwrap();
        assert!(
            gain.is_active(),
            "{format:?}: deactivation waits for the main thread"
        );
        host.idle();
        assert!(!gain.is_active(), "{format:?}");
        let processor = gain.activate(config(256)).unwrap();
        gain.deactivate(processor);
    }
}

#[test]
fn clap_calls_happen_on_the_right_threads() {
    let host = Host::new();
    let mut gain = CLAP.load(&host, CLAP.gain);
    gain.set_param(GAIN, -3.0);
    let state = gain.save_state().unwrap();
    gain.load_state(&state).unwrap();
    let processor = gain.activate(config(256)).unwrap();
    let mut processor = std::thread::spawn(move || {
        let mut processor = processor;
        render(&mut processor, 256);
        processor.reset();
        processor.stop_processing();
        processor
    })
    .join()
    .unwrap();
    render(&mut processor, 256);
    processor.stop_processing();
    gain.deactivate(processor);
    host.idle();
    assert_eq!(gain.param_value(THREAD_ERRORS), Some(0.0));
}

#[test]
fn activating_twice_is_refused() {
    for format in FORMATS {
        let host = Host::new();
        let mut gain = format.load(&host, format.gain);
        let processor = gain.activate(config(256)).unwrap();
        assert!(gain.activate(config(256)).is_err());
        gain.deactivate(processor);
        assert!(!gain.is_active());
    }
}

#[test]
fn vst2_parameters_are_described() {
    let host = Host::new();
    let mut gain = VST2.load(&host, VST2.gain);
    let params = gain.params();
    let find = |id| params.iter().find(|p| p.id == id).unwrap();
    let g = find(GAIN);
    assert_eq!((g.name.as_str(), g.unit.as_str()), ("Gain", "dB"));
    assert_eq!(g.module, "Main", "the parameter's category");
    assert!(g.flags.automatable && !g.flags.modulatable && !g.flags.stepped);
    assert!((g.default - VST2.db(0.0)).abs() < VST2.eps());
    assert_eq!((g.min, g.max), (0.0, 1.0));
    let latency = find(LATENCY);
    assert!(latency.flags.stepped);
    assert_eq!(latency.step_count, 1024);
    assert_eq!(find(RESET).step_count, 1, "a switch");
    assert_eq!(params.len(), 12);

    // Only the current value has text.
    assert_eq!(gain.param_text(GAIN, VST2.db(-6.0)), None);
    gain.set_param(GAIN, VST2.db(-6.0));
    let value = gain.param_value(GAIN).unwrap();
    assert!((value - VST2.db(-6.0)).abs() < VST2.eps());
    assert_eq!(gain.param_text(GAIN, value).as_deref(), Some("-6.0 dB"));
    assert_eq!(gain.param_from_text(GAIN, "-6"), None);
    assert_eq!(gain.param_value(plugins::ParamId(99)), None);
}

#[test]
fn vst2_plugins_change_their_own_parameters_in_gestures() {
    let host = Host::new();
    let mut gain = VST2.load(&host, VST2.gain);
    let mut processor = gain.activate(config(256)).unwrap();
    let (mut left, mut right) = (vec![1.0f32; 256], vec![1.0f32; 256]);
    // CC 7 at full, on the audio thread: the plugin sets its own gain to
    // +12 dB, from that sample.
    let events = [Event::Midi {
        frame: 10,
        data: [0xB0, 7, 127],
    }];
    processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
    assert!(
        close(left[9], 1.0) && close(left[10], linear(12.0)),
        "{}",
        left[10]
    );
    host.idle();
    let gesture = |begin| PluginEvent::ParamGesture { id: GAIN, begin };
    assert_eq!(
        without_tempo(gain.take_events()),
        vec![
            gesture(true),
            PluginEvent::ParamValue {
                id: GAIN,
                value: 1.0
            },
            gesture(false)
        ]
    );
    gain.deactivate(processor);

    // The same from the main thread, as an editor's reset button would.
    gain.set_param(RESET, 1.0);
    host.idle();
    assert_eq!(
        gain.take_events(),
        vec![
            gesture(true),
            PluginEvent::ParamValue {
                id: GAIN,
                value: f64::from((60.0 / 72.0) as f32)
            },
            gesture(false)
        ]
    );
    assert!((gain.param_value(GAIN).unwrap() - VST2.db(0.0)).abs() < VST2.eps());
}

#[test]
fn vst2_plugin_edits_are_the_base_of_emulated_modulation() {
    let host = Host::new();
    let mut gain = VST2.load(&host, VST2.gain);
    let zero_db = gain.save_state().unwrap();
    let mut processor = gain.activate(config(256)).unwrap();
    let minus_six = |processor: &mut plugins::Processor| {
        let events = [Event::ParamMod {
            frame: 0,
            id: GAIN,
            amount: VST2.db(-6.0) - VST2.db(0.0),
        }];
        let (mut left, mut right) = (vec![1.0f32; 64], vec![1.0f32; 64]);
        processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
        left[0]
    };
    // The plugin moves its own Gain back to 0 dB on the audio thread (an
    // editor's reset button), then the host modulates by -6 dB: on top of
    // 0 dB, not of the -12 dB the host last set.
    gain.set_param(GAIN, VST2.db(-12.0));
    gain.set_param(RESET, 1.0);
    render(&mut processor, 256);
    let level = minus_six(&mut processor);
    assert!(close(level, linear(-6.0)), "{level}");

    // The same for values that change on the main thread (a state).
    gain.set_param(GAIN, VST2.db(-12.0));
    render(&mut processor, 256);
    gain.load_state(&zero_db).unwrap();
    let level = minus_six(&mut processor);
    assert!(close(level, linear(-6.0)), "{level}");
    gain.deactivate(processor);
}

#[test]
fn vst2_state_without_chunks_keeps_parameters_and_the_program() {
    let host = Host::new();
    let mut first = VST2.load(&host, VST2.sine);
    first.set_param(GAIN, VST2.db(-12.0));
    // A program change on the audio thread picks the second program.
    let mut processor = first.activate(config(64)).unwrap();
    let events = [Event::Midi {
        frame: 0,
        data: [0xC0, 1, 0],
    }];
    let (mut left, mut right) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    processor.process_stereo(&mut left, &mut right, None, &events, &Transport::STOPPED);
    first.deactivate(processor);
    assert_eq!(first.param_value(PROGRAM), Some(1.0));
    let state = first.save_state().unwrap();

    let mut second = VST2.load(&host, VST2.sine);
    assert_eq!(second.param_value(PROGRAM), Some(0.0));
    second.load_state(&state).unwrap();
    assert_eq!(second.param_value(PROGRAM), Some(1.0));
    assert!((second.param_value(GAIN).unwrap() - VST2.db(-12.0)).abs() < VST2.eps());

    // Another plugin's state is refused.
    let mut gain = VST2.load(&host, VST2.gain);
    let error = gain.load_state(&state).unwrap_err();
    assert!(error.to_string().contains("another plugin"), "{error}");
}

#[test]
fn vst2_reset_silences_voices_at_the_next_block() {
    let host = Host::new();
    let mut sine = VST2.load(&host, VST2.sine);
    let mut processor = sine.activate(config(128)).unwrap();
    let on = Event::NoteOn {
        frame: 0,
        channel: 3,
        key: 69,
        velocity: 1.0,
    };
    let (mut left, mut right) = (vec![0.0f32; 128], vec![0.0f32; 128]);
    processor.process_stereo(&mut left, &mut right, None, &[on], &Transport::STOPPED);
    assert!(left.iter().any(|s| *s != 0.0));
    processor.reset();
    processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
    assert!(
        left.iter().all(|s| *s == 0.0),
        "silent from the first sample"
    );
    sine.deactivate(processor);
}

#[test]
fn vst2_calls_happen_on_the_right_threads() {
    let host = Host::new();
    let mut gain = VST2.load(&host, VST2.gain);
    gain.set_param(GAIN, VST2.db(-3.0));
    let state = gain.save_state().unwrap();
    gain.load_state(&state).unwrap();
    let processor = gain.activate(config(256)).unwrap();
    let mut processor = std::thread::spawn(move || {
        let mut processor = processor;
        render(&mut processor, 256);
        processor.reset();
        render(&mut processor, 256);
        processor.stop_processing();
        processor
    })
    .join()
    .unwrap();
    // Whichever thread is inside a processor call is the audio thread.
    render(&mut processor, 256);
    processor.stop_processing();
    gain.deactivate(processor);
    host.idle();
    assert_eq!(gain.param_value(THREAD_ERRORS), Some(0.0));
}

#[test]
fn vst2_libraries_are_unloaded_when_their_last_plugin_closes() {
    // A copy of its own, which no other test's plugins keep loaded.
    let dir = std::env::temp_dir().join(format!("canopy-vst2-unload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir
        .join("canopy-test-gain")
        .with_extension(PluginFormat::Vst2.extension());
    std::fs::copy(&fixtures().vst2_gain, &path).unwrap();
    let info = scan::read_plugin_file(PluginFormat::Vst2, &path)
        .unwrap()
        .remove(0);
    let host = Host::new();
    // Reading the file made a plugin, closed again with its library.
    let first = host.load(&info).unwrap();
    assert_eq!(first.param_value(MADE), Some(1.0));
    let second = host.load(&info).unwrap();
    assert_eq!(second.param_value(MADE), Some(2.0), "one library for both");
    drop(first);
    drop(second);
    let third = host.load(&info).unwrap();
    assert_eq!(third.param_value(MADE), Some(1.0), "loaded afresh");
    drop(third);
    std::fs::remove_dir_all(&dir).unwrap();
}
