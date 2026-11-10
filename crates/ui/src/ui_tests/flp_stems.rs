//! An FL project imported and rendered as FL renders its stems, to hold
//! against the stems FL exported.
//!
//! FL's "split playlist tracks" export writes `<song> - Track <n>.wav` per
//! playlist track: the whole mix with only that track's clips playing.
//! `CANOPY_FLP_STEMS=<song.flp> CANOPY_FLP_STEMS_OUT=<dir>
//! [CANOPY_REAL_PLUGINS=<plugin files>] [CANOPY_FLP_STEMS_ONLY=3,14] cargo
//! test --release -p ui flp_stems -- --ignored --nocapture` writes
//! `Track <n>.wav` the same way for the imported project, `tracks.txt`
//! saying what plays on each, `inserts.txt` which track each mixer insert
//! became, and the project itself. `tools/flp_compare.py` compares them.
//! With `CANOPY_FLP_STEMS_TRACKS=1` it also writes each canopy track's own
//! output in those renders, after its fader, as `tracks/Track <n> -
//! <name>.wav`, for finding which one makes a sound. With
//! `CANOPY_FLP_STEMS_MIX=1` it renders the whole song once instead, as
//! `Mix.wav` (and with the tracks, `tracks/Mix - <name>.wav`), to hold
//! against FL's plain render. `CANOPY_FLP_STEMS_RAW=1` takes every effect,
//! fader and automation lane off first, for checking instruments' own
//! levels against FL's bounces of single channels.
//! `CANOPY_FLP_STEMS_KEEP=<track names, comma separated>` mutes every
//! other track that isn't a group.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::Bounce;
use project::TrackKind;

use super::Rig;
use crate::export;
use crate::flp_import::Dialog;
use crate::session::SourceState;

/// The rate canopy usually plays at (FL's renders are at 44.1 kHz; the
/// comparison reads each at its own). Some settings are fitted to it: a
/// bell near the top of the EQ is one shape at 48 kHz and another at 44.1.
const RATE: u32 = 48_000;

/// The lane that stands for the whole song.
const WHOLE: u16 = u16::MAX;

/// A render's file name: FL numbers its playlist tracks from 1.
fn stem_name(lane: u16) -> String {
    if lane == WHOLE {
        "Mix".to_owned()
    } else {
        format!("Track {}", lane + 1)
    }
}

#[test]
#[ignore = "needs CANOPY_FLP_STEMS and CANOPY_FLP_STEMS_OUT"]
fn flp_stems() {
    let flp = PathBuf::from(std::env::var_os("CANOPY_FLP_STEMS").expect("CANOPY_FLP_STEMS"));
    let out = PathBuf::from(std::env::var_os("CANOPY_FLP_STEMS_OUT").expect("CANOPY_FLP_STEMS_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let mut rig = Rig::with_rate("flp stems", RATE as f32);
    if std::env::var_os("CANOPY_REAL_PLUGINS").is_some() {
        let plugins = super::flp_import::real_plugins();
        rig.app_mut()
            .session
            .plugins
            .manager
            .catalog_mut()
            .set(plugins);
    }
    let manager = &mut rig.app_mut().session.plugins.manager;
    let dialog = Dialog::open_with_plugins(&flp, Some(out.join("generated")), manager).unwrap();
    for line in &dialog.preview().report {
        println!("report: {line}");
    }
    let inserts = dialog.preview().inserts.clone();
    let playlist_tracks = dialog.preview().playlist_tracks.clone();
    dialog.import_into(&mut rig.app_mut().session);
    rig.frames(3);

    // Everything loaded: audio files, sampler banks, wavetables, plugins.
    let start = Instant::now();
    loop {
        rig.frames(1);
        let session = &rig.app().session;
        let mut waiting = Vec::new();
        for source in &session.project.sources {
            if matches!(session.source_state(source.id), None | Some(SourceState::Loading)) {
                waiting.push(format!("source {}", source.path.display()));
            }
        }
        session.project.visit(|track, _| {
            if matches!(session.bank(track.id), Some(crate::banks::BankState::Loading)) {
                waiting.push(format!("bank of {}", track.name));
            }
            if crate::slicers::is_slicer(track)
                && track.instrument_file().is_some()
                && (matches!(
                    session.slicer_file(track.id),
                    None | Some(crate::slicers::FileState::Loading)
                ) || session.slicer_view(track.id).is_none())
            {
                waiting.push(format!("loop of {}", track.name));
            }
            for osc in 0..3 {
                if matches!(
                    session.wavetable(track.id, osc),
                    Some(crate::banks::TableState::Loading)
                ) {
                    waiting.push(format!("wavetable of {}", track.name));
                }
            }
        });
        if waiting.is_empty() && session.offline_audio().is_some() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(600),
            "still loading: {waiting:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("loaded in {:.1} s", start.elapsed().as_secs_f32());

    let session = &mut rig.app_mut().session;
    let mut project = session.project.clone();
    // Only the named tracks sounding (groups stay as they are), as FL
    // renders with every other channel switched off.
    if let Ok(keep) = std::env::var("CANOPY_FLP_STEMS_KEEP") {
        let keep: BTreeSet<&str> = keep.split(',').map(str::trim).collect();
        for id in project.track_ids() {
            if let Some(track) = project.track_mut(id)
                && !matches!(track.kind, TrackKind::Group { .. })
                && !keep.contains(track.name.as_str())
            {
                track.mixer.mute = true;
            }
        }
    }
    // Every track as its instrument or clips make it: no effects, faders
    // at unity, nothing automated, nothing muted.
    if std::env::var_os("CANOPY_FLP_STEMS_RAW").is_some() {
        for id in project.track_ids() {
            if let Some(track) = project.track_mut(id) {
                track.devices.clear();
                track.routing = None;
                track.automation.clear();
                track.mixer = project::Mixer::default();
            }
        }
    }
    let audio = session.offline_audio().unwrap();
    std::fs::write(
        out.join("project.canopy"),
        project.to_file_string().unwrap(),
    )
    .unwrap();

    // FL's "split playlist tracks" plays the whole mix once per playlist
    // track, the others muted: here, a version of the song per track with
    // only that track's clips, less every track left with nothing to play.
    let only: Option<BTreeSet<u16>> = std::env::var("CANOPY_FLP_STEMS_ONLY")
        .ok()
        .filter(|list| !list.trim().is_empty())
        .map(|list| list.split(',').filter_map(|n| n.trim().parse().ok()).collect());
    let whole = std::env::var_os("CANOPY_FLP_STEMS_MIX").is_some();
    let lanes: BTreeSet<u16> = playlist_tracks
        .values()
        .copied()
        .filter(|lane| only.as_ref().is_none_or(|o| o.contains(&(lane + 1))))
        .filter(|_| !whole)
        .collect();
    let mut jobs = Vec::new();
    let mut keep = Vec::new();
    for &lane in &lanes {
        let version = only_lane(&project, &playlist_tracks, lane);
        let Some((from, to)) = span(&version) else {
            continue;
        };
        let plugins = session.plugins.manager.offline(&version);
        let renders = export::render_missing(&version, &audio, RATE);
        let (arrangement, latency) =
            export::compile(&version, &audio, &renders, RATE, Some(&plugins));
        keep.push(plugins);
        let sample = |tick| u64::try_from(version.tempo.tick_to_sample(tick, RATE)).unwrap_or(0);
        jobs.push((lane, arrangement, latency, sample(from)..sample(to)));
    }
    if whole && let Some((_, to)) = span(&project) {
        let plugins = session.plugins.manager.offline(&project);
        let renders = export::render_missing(&project, &audio, RATE);
        let (arrangement, latency) =
            export::compile(&project, &audio, &renders, RATE, Some(&plugins));
        keep.push(plugins);
        let end = u64::try_from(project.tempo.tick_to_sample(to, RATE)).unwrap_or(0);
        jobs.push((WHOLE, arrangement, latency, 0..end));
    }
    let per_track = std::env::var_os("CANOPY_FLP_STEMS_TRACKS").is_some();
    let mut names = BTreeMap::new();
    project.visit(|track, _| {
        names.insert(track.id, track.name.clone());
    });
    let tracks: Vec<project::TrackId> = names.keys().copied().collect();
    if per_track {
        std::fs::create_dir_all(out.join("tracks")).unwrap();
    }
    let started = Instant::now();
    let queue = std::sync::Mutex::new(jobs);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(6));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some((lane, arrangement, latency, range)) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let mut bounce = Bounce::range(arrangement, RATE, range.clone(), true, latency);
                    if per_track {
                        bounce = bounce.with_stems(&tracks);
                    }
                    while !bounce.step(RATE as usize / 10) {}
                    let (rendered, stems) = bounce.finish_with_stems();
                    for (track, stem) in stems {
                        let name = names.get(&track).map_or("?", String::as_str).replace('/', "_");
                        let path = out.join("tracks").join(format!("{} - {name}.wav", stem_name(lane)));
                        media::write_wav(&path, &stem).unwrap();
                    }
                    // From the song's start, as FL writes its stems.
                    let lead = range.start as usize;
                    let mut left = vec![0.0; lead];
                    let mut right = vec![0.0; lead];
                    left.extend_from_slice(rendered.left());
                    right.extend_from_slice(rendered.right());
                    let stem = media::AudioBuffer::new(RATE, left, right);
                    // FL numbers its playlist tracks from 1.
                    let path = out.join(format!("{}.wav", stem_name(lane)));
                    media::write_wav(&path, &stem).unwrap();
                    println!(
                        "{}: {:.1} s from {:.1} s",
                        stem_name(lane),
                        rendered.frames() as f64 / f64::from(RATE),
                        lead as f64 / f64::from(RATE)
                    );
                }
            });
        }
    });
    println!("rendered in {:.1} s", started.elapsed().as_secs_f32());
    drop(keep);

    let mut listing = String::new();
    for &lane in &lanes {
        let mut names = BTreeSet::new();
        project.visit(|track, _| {
            if track
                .clips()
                .iter()
                .any(|c| playlist_tracks.get(&c.id()) == Some(&lane))
            {
                names.insert(track.name.clone());
            }
        });
        listing.push_str(&format!("{}\t{names:?}\n", lane + 1));
    }
    std::fs::write(out.join("tracks.txt"), listing).unwrap();
    let mut inserts_listing = String::new();
    for (insert, track) in &inserts {
        let track = project.track(*track).unwrap();
        let kind = match &track.kind {
            TrackKind::Group { .. } => "group".to_owned(),
            TrackKind::Audio { clips } => format!("audio, {} clips", clips.len()),
            TrackKind::Midi {
                instrument, clips, ..
            } => format!("{}, {} clips", instrument.id, clips.len()),
        };
        let devices: Vec<&str> = track.devices.iter().map(|d| d.name()).collect();
        inserts_listing.push_str(&format!(
            "{insert}\t{}\t{kind}\t{:.1} dB\t{devices:?}\n",
            track.name, track.mixer.volume_db
        ));
    }
    std::fs::write(out.join("inserts.txt"), inserts_listing).unwrap();
}

/// `project` with only the clips from FL playlist track `lane`, and without
/// the tracks (and then groups) left with nothing to play.
fn only_lane(
    project: &project::Project,
    lanes: &BTreeMap<project::ClipId, u16>,
    lane: u16,
) -> project::Project {
    let mut version = project.clone();
    let mine = |id: project::ClipId| lanes.get(&id) == Some(&lane);
    fn strip(tracks: &mut [project::Track], mine: &dyn Fn(project::ClipId) -> bool) {
        for track in tracks {
            match &mut track.kind {
                TrackKind::Audio { clips } => clips.retain(|c| mine(c.id)),
                TrackKind::Midi { clips, .. } => clips.retain(|c| mine(c.id)),
                TrackKind::Group { children, .. } => strip(children, mine),
            }
        }
    }
    strip(&mut version.tracks, &mine);
    loop {
        let mut empty = Vec::new();
        version.visit(|track, _| {
            let silent = match &track.kind {
                TrackKind::Group { children, .. } => children.is_empty(),
                _ => track.clips().is_empty(),
            };
            if silent {
                empty.push(track.id);
            }
        });
        if empty.is_empty() {
            break;
        }
        version.delete_tracks(&empty).unwrap();
    }
    version
}

/// Where `project`'s clips start and end, if it has any.
fn span(project: &project::Project) -> Option<(midi::Tick, midi::Tick)> {
    let mut span: Option<(midi::Tick, midi::Tick)> = None;
    project.visit(|track, _| {
        for clip in track.clips() {
            let start = clip.start();
            let end = project.clip_end(clip);
            span = Some(span.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
        }
    });
    span
}
