//! Damaged and hostile input: cut short, scrambled, random, or full of
//! events nobody knows. Reading and importing must never panic; they
//! return an error or whatever could be read.

use flp::write::{ChannelType, Item, SlotPlugin, Writer, note};
use flp::{AutomationPoint, Files, Options, ids};

/// Most of what the importer handles, in one file. `noise` puts events no
/// reader knows between the real ones.
fn song(noise: bool) -> Vec<u8> {
    let mut w = Writer::new(96, "20.8.4.2576");
    let maybe_noise = |w: &mut Writer| {
        if noise {
            w.raw(&[77, 1, 2])
                .raw(&[60, 9])
                .raw(&[190, 1, 2, 3, 4])
                .raw(&[250, 3, 1, 2, 3]);
        }
    };
    w.tempo(133.0).time_signature(4, 4);
    maybe_noise(&mut w);
    w.pattern_notes(
        1,
        &[note(0, 0, 96, 60), note(1, 96, 0, 36), note(3, 0, 48, 72)],
    );
    maybe_noise(&mut w);
    w.channel(0, ChannelType::Generator, "Sytrus", "Lead")
        .levels(9000, 5000)
        .route_to(1);
    maybe_noise(&mut w);
    w.channel(1, ChannelType::Sampler, "", "Kick")
        .sample("C:\\kick.wav")
        .route_to(2);
    w.channel(2, ChannelType::AudioClip, "", "Vox")
        .sample("/nowhere/vox.wav")
        .route_to(2);
    w.channel(3, ChannelType::Layer, "", "Layer")
        .word(ids::LAYER_CHILD, 0)
        .word(ids::LAYER_CHILD, 1);
    w.channel(4, ChannelType::Automation, "", "Fade")
        .automation(&[
            AutomationPoint {
                position: 0.0,
                value: 0.2,
                tension: 0.3,
            },
            AutomationPoint {
                position: 8.0,
                value: 0.9,
                tension: -0.5,
            },
        ]);
    maybe_noise(&mut w);
    w.pattern_name(1, "Main");
    let mut trimmed = Item::pattern(1, 384, 192);
    trimmed.start_offset = Some(48.0);
    let mut clip = Item::clip(2, 96, 96);
    clip.start_offset = Some(0.25);
    w.arrangement(
        0,
        "Song",
        &[
            Item::pattern(1, 0, 384),
            trimmed,
            Item::pattern(1, 192, 384),
            clip,
            Item::clip(4, 0, 768),
        ],
    );
    w.marker(0, 0, "Start").marker(384, 8, "3/4");
    maybe_noise(&mut w);
    let eq = SlotPlugin {
        index: 0,
        internal_name: "Fruity Parametric EQ 2".to_owned(),
        name: None,
        state: vec![1; 32],
    };
    w.insert("", std::slice::from_ref(&eq), &[]);
    w.insert("Lead", &[eq], &[2]);
    w.insert("Drums", &[], &[0, 3]);
    w.insert("Send", &[], &[0]);
    let mut params = Vec::new();
    params.extend(Writer::insert_fader(1, 10_000, 1000));
    params.extend(Writer::insert_fader(2, 14_000, -3000));
    w.mixer_params(&params);
    w.link(4, 0x1FC0, 0x2000 | (2 << 6)).link(4, 0, 0);
    w.finish()
}

/// Reads, plans and imports, as the dialog does.
fn exercise(bytes: &[u8]) -> Option<flp::Import> {
    let fl = flp::parse(bytes).ok()?;
    let plan = flp::Plan::new(&fl);
    let files = Files::find(&fl, None, &|_| None);
    let options = Options {
        sfz_dir: Some(std::env::temp_dir().join("flp-robustness-never-written")),
        ..Options::default()
    };
    Some(flp::import(&fl, &plan.choices(), &files, &options))
}

/// A small fixed-seed generator, so failures repeat.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn the_song_imports_whole() {
    let result = exercise(&song(false)).expect("imports");
    assert!(result.summary.notes > 0);
    assert!(result.summary.tracks >= 3);
    // The Drums group's fader, and the lead's on both its tracks (its
    // clips overlap).
    assert_eq!(result.summary.automation_lanes, 3);
}

#[test]
fn files_cut_anywhere_never_panic() {
    let bytes = song(false);
    for len in 0..=bytes.len() {
        let cut = &bytes[..len];
        match flp::parse(cut) {
            Ok(fl) => {
                assert!(len >= 22, "a header needs 22 bytes, got {len}");
                assert_eq!(fl.truncated, len < bytes.len(), "at {len}");
            }
            Err(_) => assert!(len < 22, "only a cut header is an error ({len})"),
        }
        exercise(cut);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let header = &song(false)[..22];
    for round in 0..3000 {
        let len = rng.below(700);
        let mut bytes = if round % 2 == 0 {
            Vec::new()
        } else {
            header.to_vec()
        };
        bytes.extend(rng.bytes(len));
        if round % 2 == 1 {
            // A data size that fits, sometimes.
            let size = if round % 4 == 1 { len } else { rng.below(1000) };
            bytes[18..22].copy_from_slice(&(size as u32).to_le_bytes());
        }
        exercise(&bytes);
    }
}

#[test]
fn scrambled_songs_never_panic() {
    let original = song(false);
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..2500 {
        let mut bytes = original.clone();
        for _ in 0..1 + rng.below(8) {
            let at = 22 + rng.below(bytes.len() - 22);
            bytes[at] = rng.next() as u8;
        }
        // Now and then, cut it too.
        if rng.below(4) == 0 {
            let keep = 22 + rng.below(bytes.len() - 22);
            bytes.truncate(keep);
        }
        exercise(&bytes);
    }
}

#[test]
fn unknown_events_are_skipped() {
    let clean = flp::parse(&song(false)).unwrap();
    let noisy = flp::parse(&song(true)).unwrap();
    assert!(!noisy.truncated);
    assert_eq!(noisy.tempo, clean.tempo);
    assert_eq!(noisy.channels, clean.channels);
    assert_eq!(noisy.patterns, clean.patterns);
    assert_eq!(noisy.arrangements, clean.arrangements);
    assert_eq!(noisy.inserts, clean.inserts);
    assert_eq!(noisy.automation_links, clean.automation_links);
    let clean_import = exercise(&song(false)).unwrap();
    let noisy_import = exercise(&song(true)).unwrap();
    assert_eq!(noisy_import.project, clean_import.project);
}

#[test]
fn hostile_values_stay_in_range() {
    // Absurd tempo, a huge item, notes off the keyboard, a NaN offset.
    let mut w = Writer::new(1, "20.8.4.2576");
    w.dword(ids::TEMPO, u32::MAX);
    w.pattern_notes(1, &[note(0, u32::MAX, u32::MAX, 9999), note(0, 0, 1, 127)]);
    w.channel(0, ChannelType::Generator, "3x Osc", "Lead");
    let mut item = Item::pattern(1, u32::MAX, u32::MAX);
    item.start_offset = Some(f64::from(i32::MAX));
    let mut clip = Item::clip(0, 0, 10);
    clip.start_offset = Some(f64::NAN);
    w.arrangement(0, "A", &[item, clip]);
    let result = exercise(&w.finish()).expect("imports");
    let p = &result.project;
    assert_eq!(p.tempo.tempo.to_bpm().round(), project::Tempo::MAX_BPM);
    assert!(
        result
            .report
            .iter()
            .any(|l| l.contains("above MIDI's range")),
        "{:?}",
        result.report
    );
}
