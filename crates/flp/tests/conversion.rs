//! What the importer carries over from FL's plugins and mixer beyond the
//! layout: effect settings and their automation, sidechain routes, sampler
//! channels' pitch, and Vital presets on the Wavetable synth. Synthetic
//! files, written with the crate's test writer.

use flp::write::{ChannelType, Item, SlotPlugin, Writer, note};
use flp::{AutomationPoint, Files, Options};
use project::{AutomationTarget, Device, Track, TrackKind};

fn import(bytes: &[u8]) -> (flp::Project, flp::Import) {
    import_with(bytes, &Options::default())
}

fn import_with(bytes: &[u8], options: &Options) -> (flp::Project, flp::Import) {
    let fl = flp::parse(bytes).expect("parses");
    let plan = flp::Plan::new(&fl);
    let result = flp::import(&fl, &plan.choices(), &Files::default(), options);
    (fl, result)
}

fn track<'a>(project: &'a project::Project, name: &str) -> &'a Track {
    let mut found = None;
    project.visit(|t, _| {
        if found.is_none() && t.name == name {
            found = Some(t);
        }
    });
    found.unwrap_or_else(|| panic!("no track {name}"))
}

fn param(device: &Device, id: &str) -> f32 {
    let descriptor = device.effect.descriptor().unwrap();
    let p = descriptor.params.iter().find(|p| p.id == id).unwrap();
    device.params.get(id).copied().unwrap_or(p.default_value())
}

fn has_line(result: &flp::Import, start: &str) -> bool {
    result.report.iter().any(|l| l.starts_with(start))
}

fn numbers(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// An EQ 2 as FL 26 saved it: a low cut, a cut at 2.3 kHz, flat bells,
/// and a high cut at 24 dB/octave.
fn eq2_state() -> Vec<u8> {
    let mut values = vec![8];
    values.extend([0, 0, -600, 0, 0, 0, 0]); // levels, hundredths of a dB
    values.extend([10923, 18204, 40050, 32768, 40050, 47332, 60000]); // frequencies
    values.extend([25412, 25412, 25412, 25412, 25412, 25412, 25412]); // bandwidths
    values.extend([3, 6, 6, 6, 6, 6, 1]); // types: high pass, bells, low pass
    values.extend([0, 0, 0, 0, 0, 0, 1]); // orders
    values.push(150); // main level
    values.resize(88, 0);
    numbers(&values)
}

fn slot(index: u8, name: &str, state: Vec<u8>) -> SlotPlugin {
    SlotPlugin {
        index,
        internal_name: name.to_owned(),
        name: None,
        state,
    }
}

fn clip_points() -> Vec<AutomationPoint> {
    vec![
        AutomationPoint {
            position: 0.0,
            value: 0.0,
            tension: 0.0,
        },
        AutomationPoint {
            position: 4.0,
            value: 1.0,
            tension: 0.0,
        },
    ]
}

#[test]
fn eq2_settings_and_band_automation_carry_over() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
        .route_to(1);
    w.channel(
        1,
        ChannelType::Automation,
        "",
        "Fruity Parametric EQ 2 - Lead - Band 3 freq",
    )
    .automation(&clip_points());
    w.arrangement(
        0,
        "Arrangement",
        &[Item::pattern(1, 0, 384), Item::clip(1, 0, 384)],
    );
    w.insert("", &[], &[]);
    w.insert(
        "Lead",
        &[slot(0, "Fruity Parametric EQ 2", eq2_state())],
        &[0],
    );
    let eq = w.location(1, 0);
    w.link(1, 0x8000 | 9, eq);
    let (_, result) = import(&w.finish());
    let lead = track(&result.project, "Lead");
    let device = &lead.devices[0];
    assert_eq!(device.effect.id, "canopy.eq");
    // Band 1: FL's high pass is a low cut, at FL's default 63 Hz.
    assert_eq!(param(device, "b1_on"), 1.0);
    assert_eq!(param(device, "b1_type"), 0.0);
    assert!((param(device, "b1_freq") - 63.2).abs() < 0.5);
    // Band 3: -6 dB at 1.36 kHz, a two-octave bell; automated, so on.
    assert_eq!(param(device, "b3_type"), 2.0);
    assert!((param(device, "b3_gain") + 6.0).abs() < 1e-3);
    assert!((param(device, "b3_q") - 0.667).abs() < 0.01);
    // Flat bells are left off.
    assert_eq!(param(device, "b4_on"), 0.0);
    // Band 7: a high cut, one step steeper.
    assert_eq!(param(device, "b7_type"), 5.0);
    assert_eq!(param(device, "b7_slope"), 1.0);
    assert!((param(device, "output") - 1.5).abs() < 1e-3);
    assert!(has_line(
        &result,
        "Effect settings carried over for 1 effects"
    ));

    // The clip sweeps band 3 from 20 Hz to 20 kHz.
    let lane = lead
        .automation_lane(&AutomationTarget::DeviceParam {
            device: device.id,
            param: "b3_freq".into(),
        })
        .expect("a lane for band 3's frequency");
    let first = lane.points.first().unwrap().value;
    let last = lane.points.last().unwrap().value;
    assert!(first < 0.01 && last > 0.99, "{first} to {last}");
}

#[test]
fn compressor_and_reverb_settings_and_slot_mix() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
        .route_to(1);
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    w.insert("", &[], &[]);
    let compressor = numbers(&[2, -136, 56, 67, 150, 307, 2, 1]);
    let reverb = numbers(&[
        10001, 1319, 40, 12, 70, 100, 25, 40, 100, 500, 0, 128, 64, 128, 684, 0,
    ]);
    w.insert(
        "Lead",
        &[
            slot(0, "Fruity Compressor", compressor),
            slot(1, "Fruity Reeverb 2", reverb),
        ],
        &[0],
    );
    // The reverb's slot at half dry/wet.
    w.mixer_params(&[(1, 1, flp::ids::mixer::SLOT_MIX, 6400)]);
    let (_, result) = import(&w.finish());
    let lead = track(&result.project, "Lead");
    let compressor = &lead.devices[0];
    assert_eq!(compressor.effect.id, "canopy.compressor");
    assert!((param(compressor, "threshold") + 13.6).abs() < 1e-3);
    assert!((param(compressor, "ratio") - 5.6).abs() < 1e-3);
    assert!((param(compressor, "makeup") - 6.7).abs() < 1e-3);
    assert_eq!(param(compressor, "knee"), 7.0, "vintage");
    let reverb = &lead.devices[1];
    assert!((param(reverb, "decay") - 2.5).abs() < 1e-3);
    assert_eq!(param(reverb, "size"), 70.0);
    assert_eq!(param(reverb, "predelay"), 12.0);
    // Equal dry and wet is a 50% mix, halved again by the slot.
    assert!(
        (param(reverb, "mix") - 25.0).abs() < 0.01,
        "{}",
        param(reverb, "mix")
    );
}

/// A kick and a bass, the kick sidechained into the bass's compressor.
fn sidechain_song(version: &str) -> Vec<u8> {
    let mut w = Writer::new(96, version);
    w.pattern_notes(1, &[note(0, 0, 96, 60), note(1, 0, 96, 40)]);
    w.channel(0, ChannelType::Generator, "FPC", "Kick")
        .route_to(1);
    w.channel(1, ChannelType::Generator, "3x Osc", "Bass")
        .route_to(2);
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    w.insert("", &[], &[]);
    w.insert("Kick", &[], &[0, 2]);
    w.insert(
        "Bass",
        &[slot(
            0,
            "Fruity Compressor",
            numbers(&[2, 0, 40, 0, 0, 100, 0, 0]),
        )],
        &[0],
    );
    w.route_level(1, 2, 0);
    w.finish()
}

#[test]
fn routes_at_level_zero_become_sidechains() {
    for version in ["20.8.4.2576", "26.1.0.5530"] {
        let (fl, result) = import(&sidechain_song(version));
        assert_eq!(fl.insert(1).unwrap().sidechains(), [2], "{version}");
        assert_eq!(fl.insert(1).unwrap().outputs(), [0], "{version}");
        let p = &result.project;
        let kick = track(p, "Kick").id;
        let compressor = &track(p, "Bass").devices[0];
        assert_eq!(compressor.sidechain, Some(kick), "{version}");
        assert!(has_line(
            &result,
            "Sidechains carried over (1): Kick into Bass"
        ));
        assert!(!has_line(&result, "Sends not imported"), "{version}");
    }
}

#[test]
fn sampler_channels_keep_their_root_note_and_pitch() {
    let dir = std::env::temp_dir().join(format!("flp-sampler-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("bell.wav");
    std::fs::write(&wav, b"stand-in").unwrap();
    let mut w = Writer::new(96, "20.8.4.2576");
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.channel(0, ChannelType::Sampler, "", "Bell")
        .sample(wav.to_str().unwrap())
        .sampler_settings(57, 160);
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    let bytes = w.finish();
    let fl = flp::parse(&bytes).unwrap();
    assert_eq!(fl.channels[0].root_note, Some(57));
    assert_eq!(fl.channels[0].pitch_cents, Some(160));
    let mut files = Files::default();
    files.insert(
        wav.to_str().unwrap(),
        Some(flp::Found {
            path: wav.clone(),
            audio: None,
        }),
    );
    let options = Options {
        sfz_dir: Some(dir.clone()),
        ..Options::default()
    };
    let result = flp::import(&fl, &flp::Plan::new(&fl).choices(), &files, &options);
    let sfz = String::from_utf8_lossy(&result.files[0].contents).into_owned();
    assert!(sfz.contains("pitch_keycenter=57"), "{sfz}");
    assert!(sfz.contains("tune=160"), "{sfz}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A Vital state: the wrapper's records naming the plugin, then the preset.
fn vital_state(json: &str) -> Vec<u8> {
    let mut data = 8u32.to_le_bytes().to_vec();
    data.extend(54u32.to_le_bytes());
    data.extend(5u64.to_le_bytes());
    data.extend(b"Vital");
    data.extend(55u32.to_le_bytes());
    data.extend(0u64.to_le_bytes());
    data.extend(json.as_bytes());
    data
}

#[test]
fn vital_presets_rebuild_on_the_wavetable_synth() {
    let json = r#"{"preset_name":"Wobble","settings":{
        "osc_1_on":1.0,"osc_1_level":0.5,"osc_1_transpose":-12.0,"osc_1_unison_voices":3.0,
        "osc_1_unison_detune":3.0,"osc_2_on":0.0,
        "filter_1_on":1.0,"filter_1_cutoff":81.0,"filter_1_resonance":0.6,"filter_1_style":1.0,
        "env_1_attack":0.3,"env_1_sustain":0.5,
        "lfo_1_sync":1.0,"lfo_1_tempo":8.0,
        "modulation_1_amount":0.5,
        "modulations":[{"source":"lfo_1","destination":"filter_1_cutoff"}],
        "polyphony":1.0,"legato":1.0,
        "reverb_on":1.0,"reverb_dry_wet":0.4,"reverb_decay_time":1.0,
        "chorus_on":0.0}}"#;
    let mut w = Writer::new(96, "26.1.0.5530");
    w.pattern_notes(1, &[note(0, 0, 96, 48)]);
    w.channel(0, ChannelType::Generator, "Fruity Wrapper", "Wobble bass");
    w.data(flp::ids::PLUGIN_STATE, &vital_state(json));
    w.channel(
        1,
        ChannelType::Automation,
        "",
        "Wobble bass - Filter 1 Cutoff",
    )
    .automation(&clip_points());
    w.arrangement(
        0,
        "Arrangement",
        &[Item::pattern(1, 0, 384), Item::clip(1, 0, 384)],
    );
    w.link(1, 0x8000 | 20, 0);
    let (_, result) = import(&w.finish());
    let bass = track(&result.project, "Wobble bass");
    let TrackKind::Midi {
        instrument, params, ..
    } = &bass.kind
    else {
        panic!("a MIDI track");
    };
    assert_eq!(instrument.id, "canopy.wavetable");
    let get = |id: &str| params.get(id).copied();
    assert_eq!(get("osc_a_octave"), Some(3.0));
    assert_eq!(get("osc_a_unison"), Some(2.0));
    assert_eq!(get("osc_a_detune"), Some(9.0));
    assert_eq!(get("osc_b_on"), Some(0.0));
    assert_eq!(get("filter_type"), Some(1.0), "LP 24");
    assert!((get("filter_cutoff").unwrap() - 880.0).abs() < 0.5);
    assert!((get("env1_attack").unwrap() - 8.1).abs() < 0.01);
    assert_eq!(get("voice_mode"), Some(2.0), "legato");
    assert_eq!(get("lfo1_sync"), Some(1.0));
    // LFO 1 on the cutoff: half of 128 semitones, against 10 octaves.
    assert_eq!(get("mod1_dest"), Some(20.0));
    assert!((get("mod1_amount").unwrap() - 0.5333).abs() < 0.001);
    // Its reverb, as a device, and the cutoff automation as a lane.
    assert_eq!(bass.devices[0].effect.id, "canopy.reverb");
    assert!((param(&bass.devices[0], "decay") - 2.0).abs() < 1e-3);
    assert!(
        bass.automation_lane(&AutomationTarget::InstrumentParam("filter_cutoff".into()))
            .is_some()
    );
    assert!(has_line(
        &result,
        "Vital presets rebuilt on the Wavetable synth: 1"
    ));
}

#[test]
fn vital_wavetables_and_modulation_effects_come_along() {
    // A square drawn as a line, on oscillator 1; chorus and phaser on.
    let json = r#"{"preset_name":"Square","settings":{
        "osc_1_on":1.0,"osc_1_wave_frame":0.0,
        "wavetables":[{"name":"Drawn square","groups":[{"components":[{
            "type":"Line Source","interpolation_style":1,
            "keyframes":[{"position":0,"line":{"num_points":4,
                "points":[0.0,0.0,0.5,0.0,0.5,1.0,1.0,1.0],"powers":[0.0,0.0,0.0,0.0]}}]}]}]}],
        "chorus_on":1.0,"chorus_dry_wet":0.3,"chorus_voices":2.0,
        "chorus_delay_1":-9.0,"chorus_delay_2":-7.0,"chorus_sync":0.0,"chorus_frequency":-2.0,
        "phaser_on":1.0,"phaser_center":72.0,"phaser_mod_depth":36.0,"phaser_dry_wet":1.0}}"#;
    let mut w = Writer::new(96, "26.1.0.5530");
    w.pattern_notes(1, &[note(0, 0, 96, 48)]);
    w.channel(0, ChannelType::Generator, "Fruity Wrapper", "Lead");
    w.data(flp::ids::PLUGIN_STATE, &vital_state(json));
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    let dir = std::env::temp_dir().join("flp-vital-tables");
    let options = Options {
        sfz_dir: Some(dir.clone()),
        ..Options::default()
    };
    let (_, result) = import_with(&w.finish(), &options);
    let lead = track(&result.project, "Lead");
    let TrackKind::Midi { params, .. } = &lead.kind else {
        panic!("a MIDI track");
    };
    assert_eq!(
        params.get("osc_a_table").copied(),
        Some(instruments::builtin::wavetable::USER_TABLE as f32)
    );
    let file = lead.wavetable_file(0).expect("osc A's table");
    assert_eq!(file, dir.join("Lead 0 osc A.wav"));
    let wav = result.files.iter().find(|f| f.path == file).unwrap();
    assert!(wav.contents.starts_with(b"RIFF"));
    assert_eq!(lead.wavetable_file(1), None);

    let chorus: Vec<&Device> = lead
        .devices
        .iter()
        .filter(|d| d.effect.id == "canopy.chorus")
        .collect();
    assert_eq!(chorus.len(), 2);
    assert_eq!(param(chorus[0], "mode"), 0.0);
    assert!((param(chorus[0], "mix") - 30.0).abs() < 1e-3);
    assert!(
        (param(chorus[0], "rate") - 0.25).abs() < 1e-4,
        "free, 2^-2 Hz"
    );
    assert_eq!(param(chorus[0], "voices"), 1.0, "two voices");
    // Halfway between 2^-9 and 2^-7 seconds.
    assert!((param(chorus[0], "delay") - 4.883).abs() < 0.01);
    assert_eq!(param(chorus[1], "mode"), 2.0);
    assert!((param(chorus[1], "center") - 523.25).abs() < 0.1, "C5");
    assert_eq!(param(chorus[1], "depth"), 100.0, "three octaves");
    assert!(!has_line(&result, "Not carried over from Vital"));
}

#[test]
fn stereo_tools_and_choruses_carry_over() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.channel(0, ChannelType::Generator, "3x Osc", "Pad")
        .route_to(1);
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    w.insert("", &[], &[]);
    w.insert(
        "Pad",
        &[
            // Left a little, wider, the right side flipped.
            slot(
                0,
                "Fruity Stereo Enhancer",
                numbers(&[-32, 256, -48, 0, 1, 2]),
            ),
            slot(1, "Fruity Balance", numbers(&[64, 0])),
            slot(
                2,
                "Fruity Chorus",
                numbers(&[1, 512, 2250, 341, 1500, 2500, 0, 23, 0, 0, 0, 2500, 0]),
            ),
            slot(3, "Fruity Center", numbers(&[0, 1])),
        ],
        &[0],
    );
    let (_, result) = import(&w.finish());
    let pad = track(&result.project, "Pad");
    let ids: Vec<&str> = pad.devices.iter().map(|d| d.effect.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "canopy.utility",
            "canopy.utility",
            "canopy.chorus",
            "canopy.utility"
        ]
    );
    let enhancer = &pad.devices[0];
    assert_eq!(param(enhancer, "balance"), -25.0);
    assert_eq!(param(enhancer, "width"), 150.0);
    assert!(param(enhancer, "gain").abs() < 1e-3, "256 is 0 dB");
    assert_eq!(param(enhancer, "invert_right"), 1.0);
    assert_eq!(param(enhancer, "invert_left"), 0.0);
    // A balance turned all the way down mutes.
    assert_eq!(param(&pad.devices[1], "mute"), 1.0);
    assert_eq!(param(&pad.devices[1], "balance"), 50.0);
    let chorus = &pad.devices[2];
    assert_eq!(param(chorus, "delay"), 15.0);
    assert!((param(chorus, "spread") - 119.9).abs() < 0.1);
    assert_eq!(param(chorus, "voices"), 1.0, "two LFOs running");
    assert_eq!(param(&pad.devices[3], "dc"), 1.0);
}
