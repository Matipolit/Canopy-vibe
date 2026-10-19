//! Synthetic FL Studio files, written with the crate's test writer, read
//! back and imported.

use std::path::{Path, PathBuf};

use flp::write::{ChannelType, Item, SlotPlugin, Writer, note};
use flp::{AudioInfo, Choice, Files, ItemTarget, Options, Role, ids};
use midi::Velocity;
use project::{AutomationTarget, ClipRef, MidiClip, Track, TrackKind};

fn import(bytes: &[u8]) -> (flp::Project, flp::Import) {
    let fl = flp::parse(bytes).expect("parses");
    let plan = flp::Plan::new(&fl);
    let result = flp::import(&fl, &plan.choices(), &Files::default(), &Options::default());
    (fl, result)
}

/// Every track as `"-"` per level of nesting, then its name.
fn tree(project: &project::Project) -> Vec<String> {
    let mut names = Vec::new();
    project.visit(|t, depth| names.push(format!("{}{}", "-".repeat(depth), t.name)));
    names
}

fn track<'a>(project: &'a project::Project, name: &str) -> &'a Track {
    let mut found = None;
    project.visit(|t, _| {
        if found.is_none() && t.name == name {
            found = Some(t);
        }
    });
    found.unwrap_or_else(|| panic!("no track {name}: {:?}", tree(project)))
}

fn midi_clips(track: &Track) -> Vec<&MidiClip> {
    match &track.kind {
        TrackKind::Midi { clips, .. } => clips.iter().collect(),
        _ => panic!("{} isn't a MIDI track", track.name),
    }
}

fn instrument(track: &Track) -> &str {
    match &track.kind {
        TrackKind::Midi { instrument, .. } => &instrument.id,
        _ => panic!("{} isn't a MIDI track", track.name),
    }
}

fn devices(track: &Track) -> Vec<&str> {
    track.devices.iter().map(|d| d.name()).collect()
}

fn has_line(result: &flp::Import, start: &str) -> bool {
    result.report.iter().any(|l| l.starts_with(start))
}

fn slot(index: u8, name: &str) -> SlotPlugin {
    SlotPlugin {
        index,
        internal_name: name.to_owned(),
        name: None,
        state: vec![0; 8],
    }
}

#[test]
fn tempo_notes_and_pattern_clips() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.tempo(128.0).time_signature(3, 4);
    let mut loud = note(0, 96, 48, 64);
    loud.velocity = 128;
    loud.release = 0;
    w.pattern_notes(1, &[note(0, 0, 96, 60), loud, note(1, 192, 0, 36)]);
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
        .levels(10_000, 6400)
        .color(200, 40, 40);
    w.channel(1, ChannelType::Sampler, "", "Kick")
        .sample("C:\\Samples\\kick.wav");
    w.pattern_name(1, "Intro");
    let mut trimmed = Item::pattern(1, 768, 288);
    trimmed.start_offset = Some(96.0);
    trimmed.end_offset = Some(384.0);
    trimmed.muted = true;
    trimmed.track = 1;
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384), trimmed]);
    let (fl, result) = import(&w.finish());

    // What the file says.
    assert_eq!(fl.tempo, Some(128.0));
    assert_eq!(fl.time_signature, Some((3, 4)));
    assert_eq!(fl.ppq, 96);
    assert_eq!(fl.channels.len(), 2);
    assert_eq!(fl.channels[0].kind, flp::ChannelKind::Instrument);
    assert_eq!(fl.channels[1].kind, flp::ChannelKind::Sampler);
    let pattern = fl.pattern(1).unwrap();
    assert_eq!(pattern.name.as_deref(), Some("Intro"));
    assert_eq!(pattern.notes.len(), 3);
    assert_eq!(
        (
            pattern.notes[1].position,
            pattern.notes[1].length,
            pattern.notes[1].key
        ),
        (96, 48, 64)
    );
    assert_eq!(pattern.notes[1].velocity, 128);
    assert_eq!((pattern.notes[2].channel, pattern.notes[2].length), (1, 0));
    let items = &fl.main_arrangement().unwrap().items;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].target, Some(ItemTarget::Pattern(1)));
    assert_eq!((items[0].position, items[0].length), (0, 384));
    assert_eq!((items[0].start_offset, items[0].muted), (None, false));
    assert_eq!((items[1].position, items[1].track), (768, 1));
    assert!(items[1].muted);
    assert_eq!(
        (items[1].start_offset, items[1].end_offset),
        (Some(96.0), Some(384.0))
    );

    // What canopy makes of it: 96 ticks a quarter become 960.
    let p = &result.project;
    assert!((p.tempo.tempo.to_bpm() - 128.0).abs() < 1e-3);
    assert_eq!(p.tempo.time_signature.numerator, 3);
    assert_eq!(tree(p), ["Lead", "Kick"]);
    let lead = track(p, "Lead");
    assert_eq!(instrument(lead), "canopy.poly_synth");
    assert_eq!(lead.color, project::Color::rgb(200, 40, 40));
    let clips = midi_clips(lead);
    assert_eq!(clips.len(), 2);
    assert_eq!(
        (
            clips[0].start.get(),
            clips[0].length.get(),
            clips[0].offset.get()
        ),
        (0, 3840, 0)
    );
    assert_eq!(clips[0].name, "Intro");
    assert!(!clips[0].muted);
    assert_eq!(
        (
            clips[1].start.get(),
            clips[1].length.get(),
            clips[1].offset.get()
        ),
        (7680, 2880, 960)
    );
    assert!(clips[1].muted);
    let notes: Vec<(i64, i64, u8)> = clips[0]
        .notes
        .iter()
        .map(|n| (n.start.get(), n.len.get(), n.pitch.get()))
        .collect();
    assert_eq!(notes, [(0, 960, 60), (960, 480, 64)]);
    assert_eq!(clips[0].notes[0].velocity, flp::velocity(100));
    assert_eq!(clips[0].notes[1].velocity, Velocity::MAX);
    assert_eq!(clips[0].notes[1].release_velocity, Velocity::MIN);

    // The step-sequencer note gets a sixteenth.
    let kick = track(p, "Kick");
    assert_eq!(instrument(kick), "canopy.sampler");
    let kick_notes: Vec<(i64, i64, u8)> = midi_clips(kick)[0]
        .notes
        .iter()
        .map(|n| (n.start.get(), n.len.get(), n.pitch.get()))
        .collect();
    assert_eq!(kick_notes, [(1920, 240, 36)]);
    assert!(has_line(&result, "Missing samples (1): kick.wav"));
    assert_eq!(result.summary.tracks, 2);
    assert_eq!(result.summary.midi_clips, 4);
    assert_eq!(result.summary.notes, 6);
}

/// Kick and the Tops group feed Drums; Bass goes straight to the master.
fn mixer_song() -> Vec<u8> {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.channel(0, ChannelType::Generator, "FPC", "Kick")
        .route_to(1);
    w.channel(1, ChannelType::Generator, "FPC", "Snare")
        .route_to(2);
    w.channel(2, ChannelType::Generator, "3x Osc", "Bass")
        .route_to(3);
    w.channel(3, ChannelType::Generator, "FPC", "Hat")
        .route_to(2);
    w.arrangement(0, "Arrangement", &[]);
    w.insert("", &[slot(0, "Fruity Limiter")], &[]);
    w.insert("Kick", &[slot(0, "Fruity Parametric EQ 2")], &[4]);
    w.insert("Tops", &[], &[4]);
    w.insert("Bass", &[], &[0]);
    w.insert(
        "Drums",
        &[slot(0, "Fruity Reeverb 2"), slot(1, "Fruity NoteBook 2")],
        &[0],
    );
    w.insert_with_flags("Spare", 0x40, &[], &[0]);
    let mut params = Vec::new();
    params.extend(Writer::insert_fader(3, 6400, -6400));
    params.extend(Writer::insert_fader(4, 16_000, 0));
    w.mixer_params(&params);
    w.finish()
}

#[test]
fn inserts_become_groups_and_faders() {
    let (fl, result) = import(&mixer_song());
    assert_eq!(fl.inserts.len(), 6);
    assert_eq!(fl.insert(4).unwrap().volume, Some(16_000));
    assert_eq!(fl.insert(1).unwrap().outputs(), [4]);
    assert!(!fl.insert(5).unwrap().enabled());

    let p = &result.project;
    assert_eq!(
        tree(p),
        ["Drums", "-Kick", "-Tops", "--Snare", "--Hat", "Bass"]
    );
    let drums = track(p, "Drums");
    assert!(drums.is_group());
    assert!((drums.mixer.volume_db - flp::insert_volume_db(16_000.0)).abs() < 1e-4);
    assert!(drums.mixer.volume_db > 5.5);
    assert_eq!(devices(drums), ["Reverb"], "the notebook is skipped");
    // One channel on an insert: the insert folds into its track.
    let kick = track(p, "Kick");
    assert_eq!(instrument(kick), "canopy.drums");
    assert_eq!(devices(kick), ["EQ"]);
    assert!(track(p, "Tops").is_group());
    let bass = track(p, "Bass");
    assert!((bass.mixer.volume_db - flp::insert_volume_db(6400.0)).abs() < 1e-4);
    assert_eq!(bass.mixer.pan, -1.0);
    let master: Vec<&str> = p.master_devices.iter().map(|d| d.name()).collect();
    assert_eq!(master, ["Limiter"]);
    assert!(has_line(&result, "Effect settings not carried over: 3"));

    // The user's picks: no bass, a delay for the reverb.
    let plan = flp::Plan::new(&fl);
    let reverb = plan
        .items
        .iter()
        .find(|i| i.name == "Fruity Reeverb 2")
        .unwrap();
    assert_eq!(reverb.role, Role::Effect);
    assert_eq!(reverb.used_by, ["Drums"]);
    assert_eq!(reverb.suggestion, Choice::Builtin("canopy.reverb"));
    let mut choices = plan.choices();
    choices.set(Role::Instrument, "3x Osc", Choice::Skip);
    choices.set(
        Role::Effect,
        "Fruity Reeverb 2",
        Choice::Builtin("canopy.delay"),
    );
    let picked = flp::import(&fl, &choices, &Files::default(), &Options::default());
    assert_eq!(
        tree(&picked.project),
        ["Drums", "-Kick", "-Tops", "--Snare", "--Hat"]
    );
    assert_eq!(devices(track(&picked.project, "Drums")), ["Delay"]);
    assert!(has_line(&picked, "Skipped channels: Bass"));
}

#[test]
fn overlapping_clips_get_their_own_tracks_and_layers_pass_notes_on() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.pattern_notes(2, &[note(2, 0, 96, 48)]);
    w.channel(0, ChannelType::Generator, "Sytrus", "Lead");
    w.channel(1, ChannelType::Generator, "3x Osc", "Pad");
    w.channel(2, ChannelType::Layer, "", "Both")
        .word(ids::LAYER_CHILD, 0)
        .word(ids::LAYER_CHILD, 1);
    w.arrangement(
        0,
        "Arrangement",
        &[
            Item::pattern(1, 0, 768),
            Item::pattern(1, 384, 384),
            Item::pattern(2, 768, 384),
        ],
    );
    let (_, result) = import(&w.finish());
    let p = &result.project;
    assert_eq!(tree(p), ["Lead", "Lead 2", "Pad"]);
    let starts = |name: &str| -> Vec<i64> {
        midi_clips(track(p, name))
            .iter()
            .map(|c| c.start.get())
            .collect()
    };
    assert_eq!(starts("Lead"), [0, 7680], "the layer's clip lands after");
    assert_eq!(starts("Lead 2"), [3840]);
    assert_eq!(starts("Pad"), [7680]);
    assert_eq!(
        midi_clips(track(p, "Pad"))[0].notes[0].pitch.get(),
        48,
        "the layer's note"
    );
    assert_eq!(instrument(track(p, "Lead 2")), "canopy.wavetable");
    assert!(has_line(
        &result,
        "Layers play through their channels: Both"
    ));
}

#[test]
fn automation_clips_become_lanes() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
        .route_to(1);
    let points = |pairs: &[(f64, f64)]| -> Vec<flp::AutomationPoint> {
        pairs
            .iter()
            .map(|&(position, value)| flp::AutomationPoint {
                position,
                value,
                tension: 0.0,
            })
            .collect()
    };
    w.channel(5, ChannelType::Automation, "", "Lead volume")
        .automation(&points(&[(0.0, 0.0), (4.0, 1.0)]));
    w.channel(6, ChannelType::Automation, "", "Lead pan")
        .automation(&points(&[(0.0, 0.5), (2.0, 1.0)]));
    w.channel(7, ChannelType::Automation, "", "Tempo")
        .automation(&points(&[(0.0, 0.5)]));
    w.channel(8, ChannelType::Automation, "", "Orphan")
        .automation(&points(&[(0.0, 0.5)]));
    w.channel(9, ChannelType::Automation, "", "Cutoff")
        .automation(&points(&[(0.0, 0.5)]));
    let mut again = Item::clip(5, 1152, 384);
    again.start_offset = Some(2.0);
    w.arrangement(
        0,
        "Arrangement",
        &[
            Item::clip(5, 384, 384),
            again,
            Item::clip(6, 0, 192),
            Item::clip(7, 0, 384),
            Item::clip(8, 0, 384),
            Item::clip(9, 0, 384),
        ],
    );
    w.marker(384, 0, "Verse").marker(768, 8, "3/4");
    w.insert("", &[], &[]);
    w.insert("Lead", &[], &[0]);
    // Channel volume, the insert's pan, tempo, and a plugin knob.
    let pan = w.location(1, 0);
    w.link(5, 0, 0)
        .link(6, 0x1FC1, pan)
        .link(7, 5, 0x4000)
        .link(9, 0x8003, 0);
    let (fl, result) = import(&w.finish());
    assert_eq!(fl.automation_links.len(), 4);
    assert_eq!(
        fl.channel(5).unwrap().automation.as_ref().unwrap().points[1].position,
        4.0
    );

    let p = &result.project;
    assert_eq!(tree(p), ["Lead"], "automation clips aren't tracks");
    let lead = track(p, "Lead");
    let lane = |target: &AutomationTarget| -> Vec<(i64, f32)> {
        lead.automation_lane(target)
            .unwrap()
            .points
            .iter()
            .map(|p| (p.tick.get(), (p.value * 1000.0).round() / 1000.0))
            .collect()
    };
    let top = project::volume_to_normalized(flp::channel_volume_db(12_800.0));
    let half = project::volume_to_normalized(flp::channel_volume_db(6_400.0));
    let r = |v: f32| (v * 1000.0).round() / 1000.0;
    // Bar 2 rises from silence; bar 4 starts halfway through the clip,
    // holding the last value until then, and reaches the top midway.
    assert_eq!(
        lane(&AutomationTarget::Volume),
        [
            (3840, 0.0),
            (7680, r(top)),
            (11_520, r(top)),
            (11_520, r(half)),
            (13_440, r(top)),
            (15_360, r(top))
        ]
    );
    assert_eq!(lane(&AutomationTarget::Pan), [(0, 0.5), (1920, 1.0)]);
    assert!(has_line(&result, "Tempo automation not imported"));
    assert!(has_line(
        &result,
        "Plugin parameter automation not imported (1 clips)"
    ));
    assert!(has_line(&result, "1 automation clips aren't linked"));
    assert!(has_line(&result, "1 markers skipped"));
    assert!(has_line(&result, "1 time signature changes skipped"));
    assert_eq!(result.summary.automation_lanes, 2);
}

/// A second of a 440 Hz sine, 16-bit mono at 44.1 kHz.
fn write_wav(path: &Path) {
    let rate = 44_100u32;
    let samples: Vec<i16> = (0..rate)
        .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 16_000.0) as i16)
        .collect();
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + samples.len() as u32 * 2).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((samples.len() as u32 * 2).to_le_bytes());
    for s in samples {
        bytes.extend(s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn probe(path: &Path) -> Option<AudioInfo> {
    let (buffer, channels) = media::decode(path).ok()?;
    Some(AudioInfo {
        sample_rate: buffer.sample_rate(),
        frames: buffer.frames() as u64,
        channels,
    })
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flp-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn audio_clips_and_sampler_channels_use_their_files() {
    let dir = temp_dir("audio");
    let tone = dir.join("tone.wav");
    write_wav(&tone);
    let kits = dir.join("Kits");
    std::fs::create_dir_all(&kits).unwrap();
    let kick_wav = kits.join("kick.wav");
    write_wav(&kick_wav);

    let mut w = Writer::new(96, "20.8.4.2576");
    w.tempo(120.0);
    w.pattern_notes(1, &[note(1, 0, 0, 60)]);
    w.channel(0, ChannelType::AudioClip, "", "Vox")
        .sample(tone.to_str().unwrap());
    w.channel(1, ChannelType::Sampler, "", "Kick")
        .sample("C:\\Users\\someone\\Kits\\kick.wav");
    w.channel(2, ChannelType::AudioClip, "", "Gone")
        .sample("D:\\gone.wav");
    let mut vox = Item::clip(0, 192, 96);
    vox.start_offset = Some(0.5);
    w.arrangement(
        0,
        "Arrangement",
        &[vox, Item::clip(2, 0, 96), Item::pattern(1, 0, 384)],
    );
    let flp_path = dir.join("song.flp");
    std::fs::write(&flp_path, w.finish()).unwrap();

    let fl = flp::parse(&std::fs::read(&flp_path).unwrap()).unwrap();
    let files = Files::find(&fl, Some(&flp_path), &probe);
    assert_eq!(
        files
            .get("C:\\Users\\someone\\Kits\\kick.wav")
            .unwrap()
            .path,
        kick_wav
    );
    assert!(files.get("D:\\gone.wav").is_none());
    let sfz_dir = dir.join("sfz");
    let options = Options {
        sfz_dir: Some(sfz_dir.clone()),
        ..Options::default()
    };
    let result = flp::import(&fl, &flp::Plan::new(&fl).choices(), &files, &options);
    let p = &result.project;
    assert_eq!(tree(p), ["Vox", "Kick", "Gone"]);

    // Two beats in at 120 BPM, a beat long, half a beat into the file.
    let vox = track(p, "Vox");
    let TrackKind::Audio { clips } = &vox.kind else {
        panic!("Vox should be an audio track");
    };
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].start.get(), 1920);
    assert_eq!(
        (clips[0].source_offset, clips[0].source_len),
        (11_025, 22_050)
    );
    let source = p.source(clips[0].source).unwrap();
    assert_eq!(
        (source.path.as_path(), source.sample_rate, source.frames),
        (tone.as_path(), 44_100, 44_100)
    );
    assert_eq!(p.clip_end(ClipRef::Audio(&clips[0])).get(), 1920 + 960);

    // The sampler plays its sample through a generated SFZ.
    let kick = track(p, "Kick");
    assert_eq!(instrument(kick), "canopy.sampler");
    let sfz = kick.instrument_file().unwrap().to_owned();
    assert_eq!(sfz.parent(), Some(sfz_dir.as_path()));
    let generated = result.files.iter().find(|f| f.path == sfz).unwrap();
    let text = String::from_utf8_lossy(&generated.contents);
    assert!(text.contains(&format!("sample={}", kick_wav.display())));
    assert!(text.contains("loop_mode=one_shot"));

    let gone = track(p, "Gone");
    assert!(gone.clips().is_empty());
    assert!(has_line(&result, "Missing samples (1): gone.wav"));
    assert_eq!(result.summary.audio_clips, 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// The same small song in each playlist record size.
fn layouts_song(version: &str) -> Vec<u8> {
    let mut w = Writer::new(96, version);
    if version.starts_with("25") || version.starts_with("26") {
        // FL 25's three-byte event, right after the version.
        w.raw(&[172, 1, 1, 0]);
    }
    w.tempo(140.0);
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.channel(0, ChannelType::Generator, "Fruity Wrapper", "Lead")
        .vst("Serum");
    let mut muted = Item::pattern(1, 384, 384);
    muted.muted = true;
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384), muted]);
    w.finish()
}

#[test]
fn fl_21_25_and_26_playlists() {
    for version in ["20.8.4.2576", "21.0.3.3517", "25.2.4.4960", "26.1.0.5530"] {
        let (fl, result) = import(&layouts_song(version));
        assert!(!fl.truncated, "{version}");
        assert_eq!(fl.tempo, Some(140.0), "{version}");
        let items = &fl.main_arrangement().unwrap().items;
        assert_eq!(items.len(), 2, "{version}");
        assert!(items[1].muted, "{version}");
        let vst = fl.channels[0]
            .plugin
            .as_ref()
            .unwrap()
            .vst
            .as_ref()
            .unwrap();
        assert_eq!(vst.name, "Serum");
        let lead = track(&result.project, "Lead");
        assert_eq!(instrument(lead), "canopy.wavetable", "{version}");
        let clips = midi_clips(lead);
        assert_eq!(
            clips
                .iter()
                .map(|c| (c.start.get(), c.muted))
                .collect::<Vec<_>>(),
            [(0, false), (3840, true)],
            "{version}"
        );
    }
}

/// FL 26 routes channels with a word rather than a byte, and numbers mixer
/// locations from `0x7000` rather than `0x2000`; the same song reads the
/// same either way.
#[test]
fn fl_26_routing_faders_and_links() {
    for version in ["20.8.4.2576", "26.1.0.5530"] {
        let mut w = Writer::new(96, version);
        w.pattern_notes(1, &[note(0, 0, 96, 60)]);
        w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
            .route_to(2);
        w.channel(1, ChannelType::Automation, "", "Lead fader")
            .automation(&[flp::AutomationPoint {
                position: 0.0,
                value: 0.5,
                tension: 0.0,
            }]);
        w.channel(2, ChannelType::Automation, "", "EQ band 7 freq")
            .automation(&[flp::AutomationPoint {
                position: 0.0,
                value: 0.5,
                tension: 0.0,
            }]);
        w.arrangement(
            0,
            "Arrangement",
            &[
                Item::pattern(1, 0, 384),
                Item::clip(1, 0, 384),
                Item::clip(2, 0, 384),
            ],
        );
        w.insert("", &[], &[]);
        w.insert("Pad", &[], &[0]);
        w.insert("Lead", &[slot(3, "Fruity Parametric EQ 2")], &[1]);
        let mut params = Vec::new();
        params.extend(Writer::insert_fader(0, 12_800, 0));
        params.extend(Writer::insert_fader(2, 6400, 3200));
        params.push((2, 3, ids::mixer::SLOT_ENABLED, 0));
        w.mixer_params(&params);
        let fader = w.location(2, 0);
        let eq = w.location(2, 3);
        w.link(1, 0x1FC0, fader).link(2, 0x800D, eq);
        let (fl, result) = import(&w.finish());
        assert_eq!(fl.channels[0].insert, Some(2), "{version}");
        let lead = fl.insert(2).unwrap();
        assert_eq!(
            (lead.volume, lead.pan),
            (Some(6400), Some(3200)),
            "{version}"
        );
        assert_eq!(lead.slots[0].enabled, Some(false), "{version}");
        let targets: Vec<_> = fl.automation_links.iter().map(|l| l.target).collect();
        assert_eq!(
            targets,
            [
                flp::AutomationTarget::Insert {
                    insert: 2,
                    slot: Some(0),
                    param: 0x1FC0
                },
                flp::AutomationTarget::Insert {
                    insert: 2,
                    slot: Some(3),
                    param: 0x800D
                },
            ],
            "{version}"
        );
        let p = &result.project;
        assert_eq!(tree(p), ["Pad", "-Lead"], "{version}");
        let track = track(p, "Lead");
        assert!(track.devices[0].bypassed, "{version}");
        assert!(
            track.automation_lane(&AutomationTarget::Volume).is_some(),
            "{version}"
        );
    }
}

#[test]
fn fl_9_text_and_pattern_blocks() {
    let mut w = Writer::new(96, "9.0.3");
    w.word(ids::TEMPO_COARSE, 140);
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    w.word(ids::CHANNEL_NEW, 0)
        .byte(ids::CHANNEL_TYPE, 0)
        .ansi(ids::SAMPLE_PATH, "\\Projects\\kick.wav")
        .ansi(ids::CHANNEL_NAME_OLD, "Kick");
    w.ansi(ids::PATTERN_NAME, "Beat");
    // A pattern block: no pattern base, its number counted down from 999.
    let mut block = Vec::new();
    block.extend(0u32.to_le_bytes());
    block.extend(0u16.to_le_bytes());
    block.extend(0x6000u16.to_le_bytes());
    block.extend(384u32.to_le_bytes());
    block.extend(998u16.to_le_bytes());
    block.extend([0, 0, 120, 0, 64, 0, 64, 100, 128, 128]);
    block.extend((-1.0f32).to_le_bytes());
    block.extend((-1.0f32).to_le_bytes());
    w.data(ids::PLAYLIST, &block);
    let (fl, result) = import(&w.finish());
    assert_eq!(fl.tempo, Some(140.0));
    assert_eq!(fl.channels[0].name.as_deref(), Some("Kick"));
    assert_eq!(
        fl.channels[0].sample_path.as_deref(),
        Some("\\Projects\\kick.wav")
    );
    assert_eq!(fl.pattern(1).unwrap().name.as_deref(), Some("Beat"));
    let kick = track(&result.project, "Kick");
    let clips = midi_clips(kick);
    assert_eq!(clips.len(), 1);
    assert_eq!((clips[0].start.get(), clips[0].length.get()), (0, 3840));
}

#[test]
fn peak_controllers_and_kickstart_become_duckers() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.channel(0, ChannelType::Generator, "FPC", "Kick")
        .route_to(1);
    w.channel(1, ChannelType::Generator, "3x Osc", "Bass")
        .route_to(2);
    w.channel(2, ChannelType::Generator, "3x Osc", "Pad")
        .route_to(3);
    w.arrangement(0, "Arrangement", &[]);
    w.insert("", &[], &[]);
    w.insert("Kick", &[slot(0, "Fruity Peak Controller")], &[0]);
    w.insert("Bass", &[], &[0]);
    w.insert(
        "Pad",
        &[SlotPlugin {
            index: 0,
            internal_name: "Fruity Wrapper".to_owned(),
            name: Some("Kickstart 2".to_owned()),
            state: Vec::new(),
        }],
        &[0],
    );
    // The controller in insert 1, slot 0, drives the bass's volume.
    w.link(0x2000 | (1 << 6), 0, 1);
    let (fl, result) = import(&w.finish());
    assert_eq!(fl.automation_links.len(), 1);
    let p = &result.project;
    let kick = track(p, "Kick");
    assert!(kick.devices.is_empty(), "it doesn't duck itself");
    let bass = track(p, "Bass");
    assert_eq!(devices(bass), ["Ducker"]);
    assert_eq!(bass.devices[0].sidechain, Some(kick.id));
    let pad = track(p, "Pad");
    assert_eq!(devices(pad), ["Ducker"]);
    assert_eq!(pad.devices[0].param("mode"), Some(2.0), "to the beat");
}

#[test]
fn disabled_things_stay_off() {
    let mut w = Writer::new(96, "20.8.4.2576");
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead")
        .byte(ids::CHANNEL_ENABLED, 0)
        .route_to(1);
    w.channel(1, ChannelType::Generator, "3x Osc", "Pad")
        .route_to(1);
    w.arrangement(0, "Arrangement", &[]);
    w.insert("", &[], &[]);
    // Insert off, effects off.
    w.insert_with_flags("Synths", 0x40, &[slot(0, "Fruity Limiter")], &[0]);
    let (_, result) = import(&w.finish());
    let p = &result.project;
    assert!(track(p, "Lead").mixer.mute);
    assert!(!track(p, "Pad").mixer.mute);
    let synths = track(p, "Synths");
    assert!(synths.mixer.mute);
    assert!(synths.devices[0].bypassed);
}

/// One of FL's wrapper records, as the writer takes them.
fn record(id: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.extend((bytes.len() as u64).to_le_bytes());
    out.extend(bytes);
    out
}

/// A song with third-party plugins as FL 20 and later save them: Vital (a
/// CLAP) on a channel, OTT (a VST2 without chunks) and TDR Nova (a VST3)
/// on an insert.
fn hosted_song() -> Vec<u8> {
    let mut w = Writer::new(96, "21.0.3.3517");
    w.tempo(120.0);
    w.pattern_notes(1, &[note(0, 0, 96, 60)]);
    let mut vital_state = vec![0u8; 4];
    vital_state.extend(b"{\"preset_name\":\"Lead\"}");
    w.channel(0, ChannelType::Generator, "Fruity Wrapper", "Lead")
        .wrapper(&[
            (58, b"audio.vital.synth"),
            (54, b"Vital"),
            (55, b"C:\\Program Files\\Common Files\\CLAP\\Vital.clap"),
            (56, b"Vital Audio"),
            (53, &vital_state),
        ])
        .route_to(1);
    w.arrangement(0, "Arrangement", &[Item::pattern(1, 0, 384)]);
    // OTT's VST2: FL's header, then its 19 values.
    let mut ott = (-9i32).to_le_bytes().to_vec();
    ott.push(0x05);
    ott.extend((-2i32).to_le_bytes());
    ott.extend([0; 12]);
    ott.extend(19u32.to_le_bytes());
    for k in 0..19 {
        ott.extend((k as f32 / 20.0).to_le_bytes());
    }
    let mut nova = 1u32.to_le_bytes().to_vec();
    nova.extend(record(1, &[0; 64]));
    nova.extend(record(3, b"component"));
    nova.extend(record(4, b"controller"));
    let ott_state = flp::write::wrapper_state(&[
        (51, &i32::from_be_bytes(*b"XfTT").to_le_bytes()),
        (54, b"OTT"),
        (55, b"C:\\Program Files\\VstPlugins\\OTT_x64.dll"),
        (56, b"Xfer Records"),
        (53, &ott),
    ]);
    let nova_id = hex("54545356356474616472206e6f766100");
    let nova_state = flp::write::wrapper_state(&[
        (52, &nova_id),
        (54, b"TDR Nova"),
        (55, b"C:\\Program Files\\Common Files\\VST3\\TDR Nova.vst3"),
        (56, b"Tokyo Dawn Labs"),
        (53, &nova),
    ]);
    let wrapped = |index: u8, state: Vec<u8>| SlotPlugin {
        index,
        internal_name: "Fruity Wrapper".into(),
        name: None,
        state,
    };
    w.insert("", &[], &[]);
    w.insert(
        "Lead",
        &[wrapped(0, ott_state), wrapped(1, nova_state)],
        &[0],
    );
    w.finish()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn third_party_plugins_come_in_as_themselves_where_installed() {
    let fl = flp::parse(&hosted_song()).expect("parses");
    let plan = flp::Plan::new(&fl);
    let ids: Vec<(String, Vec<flp::PluginId>)> = plan
        .items
        .iter()
        .map(|i| (i.name.clone(), i.plugins.clone()))
        .collect();
    let id = |format, id: &str| flp::PluginId {
        format,
        id: id.to_owned(),
    };
    let vital = id(flp::PluginFormat::Clap, "audio.vital.synth");
    let ott = id(flp::PluginFormat::Vst2, "58665454");
    // Saved on Windows, the class id's first words come byte-swapped; put
    // back, they spell what the VST3 SDK makes for a plugin that was a
    // VST2 first: "VST", its VST2 id ("Td5a"), its name ("tdr nova").
    let nova = id(flp::PluginFormat::Vst3, "56535454643561746472206E6F766100");
    assert_eq!(
        ids,
        [
            ("Vital".to_owned(), vec![vital.clone()]),
            ("OTT".to_owned(), vec![ott.clone()]),
            ("TDR Nova".to_owned(), vec![nova.clone()]),
        ]
    );
    let mut choices = plan.choices();
    for item in &plan.items {
        choices.set(item.role, &item.name, Choice::Plugin);
    }
    let options = Options {
        installed: [vital, ott].into_iter().collect(),
        ..Options::default()
    };
    let result = flp::import(&fl, &choices, &Files::default(), &options);
    let lead = track(&result.project, "Lead");
    let TrackKind::Midi {
        instrument, plugin, ..
    } = &lead.kind
    else {
        panic!("a MIDI track");
    };
    assert_eq!(instrument.format, project::Format::Clap);
    assert_eq!(instrument.id, "audio.vital.synth");
    let plugin = plugin.as_ref().expect("Vital's data");
    assert_eq!(plugin.state.as_bytes(), b"{\"preset_name\":\"Lead\"}");
    assert_eq!(plugin.vendor, "Vital Audio");

    // OTT as itself, its values in the host's VST2 state; Nova isn't
    // installed, so the built-in its name suggests stands in.
    let formats: Vec<(project::Format, &str)> = lead
        .devices
        .iter()
        .map(|d| (d.effect.format, d.effect.id.as_str()))
        .collect();
    assert_eq!(formats[0], (project::Format::Vst2, "58665454"));
    assert_eq!(formats[1].0, project::Format::BuiltIn, "{formats:?}");
    let values: Vec<f32> = (0..19).map(|k| k as f32 / 20.0).collect();
    let expected = plugins::state::vst2(
        i32::from_be_bytes(*b"XfTT"),
        0,
        &plugins::state::Vst2Data::Params(values),
    );
    let ott_device = &lead.devices[0];
    assert_eq!(
        ott_device.plugin.as_ref().unwrap().state.as_bytes(),
        expected.as_slice()
    );
    assert!(ott_device.params.is_empty(), "the host fills them in");
    assert!(has_line(&result, "Plugins brought in as themselves"));
    assert!(has_line(
        &result,
        "Not installed here, so built-ins stand in: TDR Nova"
    ));
}

#[test]
fn a_vst3s_state_keeps_its_component_and_controller() {
    let fl = flp::parse(&hosted_song()).expect("parses");
    let nova = fl
        .inserts
        .iter()
        .flat_map(|i| &i.slots)
        .find(|s| s.plugin.display_name() == "TDR Nova")
        .unwrap();
    assert_eq!(
        flp::vst::hosted_state(&nova.plugin),
        Some(plugins::state::vst3(b"component", b"controller"))
    );
}
