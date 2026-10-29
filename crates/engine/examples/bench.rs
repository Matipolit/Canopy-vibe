//! Where the time goes on a big project: the audio thread (rendering a
//! snapshot), the UI thread's per-edit work (cloning and comparing the
//! project for undo, compiling the snapshot), and saving and loading.
//!
//! `cargo run --release -p engine --example bench [-- seconds]`. Nothing is
//! asserted: it prints numbers to compare before and after a change, and is
//! what a profiler samples. `-- --save <dir>` also writes the song there as
//! `song.canopy` with its audio files, for the UI's frame-time test
//! (`ui_tests/perf.rs`). `-- --poly <n>` renders a different song instead:
//! `n` Poly Synth tracks holding chords and nothing else, which is pure
//! arithmetic on little state, so how it scales with `CANOPY_AUDIO_THREADS`
//! is the render pool's own scaling, apart from memory traffic and power
//! limits. `-- --paced` spaces the callbacks out as a sound card would
//! (every 256 frames' worth of time), so the render workers park between
//! them and the time includes waking them up, as it does when playing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use engine::{Command, Compiler, MAX_BLOCK};
use instruments::InstrumentRef;
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::{
    AutomationPoint, AutomationTarget, EffectRef, NewTrack, Note, Project, SourceId, TrackId,
    TrackPosition,
};

const RATE: u32 = 48_000;
const BARS: i64 = 64;

/// A busy song: wavetable pads with unison, drums, guitars, poly synths,
/// audio tracks, groups with reverbs, an EQ and compressor on everything,
/// a mastering chain, and automation on every fader. The audio files are
/// named into `dir`.
fn song(dir: &Path) -> (Project, Vec<(SourceId, Arc<AudioBuffer>)>) {
    let mut project = Project::new();
    project.set_tempo_bpm(124.0);
    let bar = project.tempo.bar_len();
    let mut sources = Vec::new();
    let mut groups = Vec::new();

    /// An instrument's id, how many tracks play it, and their settings.
    type Part = (&'static str, usize, &'static [(&'static str, f32)]);
    let instruments: [Part; 4] = [
        (
            "canopy.wavetable",
            8,
            &[
                ("osc_a_unison", 3.0),
                ("osc_b_on", 1.0),
                ("osc_b_unison", 3.0),
            ],
        ),
        ("canopy.drums", 4, &[]),
        ("canopy.guitar", 2, &[]),
        ("canopy.poly_synth", 6, &[]),
    ];
    for (id, count, settings) in instruments {
        let mut members = Vec::new();
        for n in 0..count {
            let track = project
                .add_track(NewTrack::Midi, TrackPosition::END)
                .unwrap();
            project
                .set_instrument(track, InstrumentRef::builtin(id))
                .unwrap();
            for &(param, value) in settings {
                project.set_instrument_param(track, param, value).unwrap();
            }
            fill_notes(&mut project, track, id, n);
            chain(
                &mut project,
                Some(track),
                &["canopy.eq", "canopy.compressor"],
            );
            fader_automation(&mut project, track, bar);
            members.push(track);
        }
        let group = project.group_tracks(&members).unwrap();
        chain(&mut project, Some(group), &["canopy.reverb"]);
        groups.push(group);
    }

    let mut members = Vec::new();
    for n in 0..8 {
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let frames = RATE as u64 * 8;
        let source = project.add_source(dir.join(format!("{n}.wav")), RATE, frames, 2);
        let audio = (0..frames)
            .map(|i| ((i as f32 * 0.01 * (n + 1) as f32).sin()) * 0.2)
            .collect::<Vec<_>>();
        sources.push((
            source,
            Arc::new(AudioBuffer::new(RATE, audio.clone(), audio)),
        ));
        for b in (0..BARS).step_by(4) {
            project
                .add_audio_clip(track, Tick::new(b * bar.get()), source)
                .unwrap();
        }
        chain(
            &mut project,
            Some(track),
            &["canopy.eq", "canopy.compressor"],
        );
        fader_automation(&mut project, track, bar);
        members.push(track);
    }
    let group = project.group_tracks(&members).unwrap();
    chain(&mut project, Some(group), &["canopy.delay"]);

    chain(
        &mut project,
        None,
        &["canopy.eq", "canopy.multiband", "canopy.limiter"],
    );
    (project, sources)
}

/// `tracks` Poly Synths each holding a six-note chord for the whole song,
/// with nothing else: compute-bound, and every track independent.
fn poly_song(tracks: usize) -> Project {
    let mut project = Project::new();
    project.set_tempo_bpm(124.0);
    let bar = project.tempo.bar_len();
    for n in 0..tracks {
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let clip = project
            .add_midi_clip(track, Tick::ZERO, bar * BARS)
            .unwrap();
        let notes: Vec<_> = [48u8, 52, 55, 59, 62, 67]
            .iter()
            .map(|p| Note {
                id: project.new_note_id(),
                start: Tick::ZERO,
                len: bar * BARS,
                pitch: Pitch::new(p + (n % 5) as u8).unwrap(),
                velocity: Velocity::from_midi1(100),
                release_velocity: Velocity::CENTER,
                muted: false,
            })
            .collect();
        project.set_notes(clip, notes).unwrap();
    }
    project
}

fn chain(project: &mut Project, track: Option<TrackId>, effects: &[&str]) {
    for id in effects {
        project
            .add_device(track, EffectRef::builtin(id), None)
            .unwrap();
    }
}

fn fader_automation(project: &mut Project, track: TrackId, bar: Tick) {
    let points: Vec<_> = (0..BARS * 4)
        .map(|beat| AutomationPoint {
            tick: Tick::new(beat * bar.get() / 4),
            value: 0.6 + 0.2 * ((beat % 8) as f32 / 8.0),
            curve: 0.0,
        })
        .collect();
    project
        .set_automation(track, AutomationTarget::Volume, points)
        .unwrap();
}

/// Sixteenth-note patterns, four bars per clip, over the whole song: about
/// 20,000 notes in all.
fn fill_notes(project: &mut Project, track: TrackId, instrument: &str, seed: usize) {
    let bar = project.tempo.bar_len();
    let step = bar / 16;
    for clip_start in (0..BARS).step_by(4) {
        let clip = project
            .add_midi_clip(track, Tick::new(clip_start * bar.get()), bar * 4)
            .unwrap();
        let mut notes = Vec::new();
        for s in 0..64i64 {
            let (pitch, len) = match instrument {
                "canopy.drums" => {
                    if s % 4 != 0 && (s + seed as i64) % 3 != 0 {
                        continue;
                    }
                    ([36u8, 38, 42, 46][(s as usize + seed) % 4], step)
                }
                "canopy.wavetable" => {
                    if s % 8 != 0 {
                        continue;
                    }
                    (48 + ((s / 8 + seed as i64) % 12) as u8, step * 8)
                }
                _ => {
                    if s % 2 != 0 {
                        continue;
                    }
                    (52 + ((s / 2 + seed as i64 * 3) % 19) as u8, step * 2)
                }
            };
            notes.push(Note {
                id: project.new_note_id(),
                start: step * s,
                len,
                pitch: Pitch::new(pitch).unwrap(),
                velocity: Velocity::from_midi1(90 + (s % 5) as u8 * 6),
                release_velocity: Velocity::CENTER,
                muted: false,
            });
        }
        project.set_notes(clip, notes).unwrap();
    }
}

/// Counts the bytes allocated and not yet freed, for [`history_memory`].
struct Counting;

static LIVE_BYTES: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

// SAFETY: passes every call on to the system allocator, only counting.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: the caller's layout, as given.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: as given.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What undo keeps after 200 edits of one clip each (a note's velocity),
/// and how long each edit's bookkeeping (copy, compare) takes.
fn history_memory(project: &Project) {
    let mut project = project.clone();
    let mut clips = Vec::new();
    project.visit(|track, _| {
        for clip in track.clips() {
            if let project::ClipRef::Midi(clip) = clip {
                clips.push(clip.id);
            }
        }
    });
    let mut history = project::History::default();
    let before = LIVE_BYTES.load(std::sync::atomic::Ordering::Relaxed);
    let start = Instant::now();
    for step in 0..200 {
        let clip = clips[step % clips.len()];
        history
            .edit(&mut project, "Velocity", |p| {
                let Some((_, project::ClipRef::Midi(midi))) = p.clip(clip) else {
                    return Ok(());
                };
                let mut notes: Vec<Note> = midi.notes.iter().cloned().collect();
                if let Some(note) = notes.first_mut() {
                    note.velocity = Velocity::from_midi1(1 + (step % 120) as u8);
                }
                p.set_notes(clip, notes)
            })
            .unwrap();
    }
    let each = start.elapsed().as_secs_f64() / 200.0;
    let held = LIVE_BYTES.load(std::sync::atomic::Ordering::Relaxed) - before;
    println!(
        "{:<44} {:>9.1} MB",
        "undo history after 200 one-clip edits",
        held as f64 / 1e6
    );
    println!(
        "{:<44} {:>9.3} ms",
        "  each edit (copy, edit, compare)",
        each * 1e3
    );
}

fn time<T>(what: &str, repeat: usize, mut f: impl FnMut() -> T) -> T {
    let start = Instant::now();
    let mut out = None;
    for _ in 0..repeat {
        out = Some(f());
    }
    let each = start.elapsed().as_secs_f64() / repeat as f64;
    println!("{what:<44} {:>9.3} ms", each * 1e3);
    out.unwrap()
}

fn main() {
    let mut seconds = 20.0;
    let mut save: Option<PathBuf> = None;
    let mut poly = None;
    let mut paced = false;
    let mut busy_wait = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--save" => save = args.next().map(PathBuf::from),
            "--poly" => poly = args.next().and_then(|n| n.parse::<usize>().ok()),
            "--paced" => paced = true,
            // Paced, but the audio thread spins to the next callback
            // instead of sleeping: the workers' wake-up cost alone, without
            // the audio thread's own core going cold.
            "--busy" => {
                paced = true;
                busy_wait = true;
            }
            s => {
                seconds = s
                    .parse()
                    .expect("seconds, --save <dir>, --poly <n> or --paced")
            }
        }
    }
    let dir = save.clone().unwrap_or_else(|| PathBuf::from("/bench"));
    let (project, sources) = match poly {
        Some(tracks) => (poly_song(tracks), Vec::new()),
        None => song(&dir),
    };
    if let Some(dir) = &save {
        std::fs::create_dir_all(dir).unwrap();
        for (n, (_, audio)) in sources.iter().enumerate() {
            media::write_wav(&dir.join(format!("{n}.wav")), audio).unwrap();
        }
        std::fs::write(dir.join("song.canopy"), project.to_file_string().unwrap()).unwrap();
        println!("saved {}", dir.join("song.canopy").display());
    }
    let mut notes = 0;
    project.visit(|track, _| {
        for clip in track.clips() {
            if let project::ClipRef::Midi(clip) = clip {
                notes += clip.notes.len();
            }
        }
    });
    let tracks = project.track_ids().len();
    println!("{tracks} tracks, {notes} notes, {BARS} bars at 124 BPM\n");

    let copy = time("Project::clone", 20, || project.clone());
    history_memory(&project);
    time("Project == Project (unchanged)", 20, || project == copy);
    let text = time("save (to_file_string)", 5, || {
        project.to_file_string().unwrap()
    });
    println!(
        "{:<44} {:>9.1} KB",
        "  file size",
        text.len() as f64 / 1024.0
    );
    let loaded = time("load (from_file_string)", 5, || {
        Project::from_file_string(&text).unwrap()
    });
    assert_eq!(loaded, project);

    let lookup = |id: SourceId| {
        sources
            .iter()
            .find(|(s, _)| *s == id)
            .map(|(_, a)| Arc::clone(a))
    };
    let mut compiler = Compiler::new(RATE);
    let first = time("compile (first: builds every processor)", 1, || {
        compiler.compile(&project, lookup)
    });
    let (handle, mut engine) = engine::create(RATE as f32);
    let mut handle = handle;
    println!(
        "{:<44} {:>9} (CANOPY_AUDIO_THREADS overrides)",
        "render worker threads",
        engine.workers()
    );
    handle.send(Command::Arrangement(first)).ok().unwrap();
    // Every compile after the first adopts the instances the engine has,
    // which is what a drag's end or an undo costs on the UI thread.
    let mut send = |arrangement| {
        handle.send(Command::Arrangement(arrangement)).ok().unwrap();
        let mut out = [0.0f32; 2 * MAX_BLOCK];
        engine.process_interleaved(&mut out, 2);
        handle.collect_garbage();
    };
    for _ in 0..3 {
        send(compiler.compile(&project, lookup));
    }
    let next = time("compile (again, instances kept)", 10, || {
        compiler.compile(&project, lookup)
    });
    send(next);

    // The wavetable bank finishes generating in the background (a fraction
    // of a second); let it, so it isn't timed as rendering.
    std::thread::sleep(std::time::Duration::from_millis(500));
    handle.send(Command::SetPlaying(true)).ok().unwrap();
    let device_block = 256;
    let mut out = vec![0.0f32; 2 * device_block];
    let blocks = (seconds * RATE as f64 / device_block as f64) as usize;
    // Let every voice start before timing.
    for _ in 0..200 {
        engine.process_interleaved(&mut out, 2);
    }
    let period = std::time::Duration::from_secs_f64(device_block as f64 / RATE as f64);
    let cpu_start = thread_cpu_seconds();
    let start = Instant::now();
    let mut busy = std::time::Duration::ZERO;
    let mut worst = std::time::Duration::ZERO;
    let mut sink = 0.0f32;
    for i in 0..blocks {
        if paced {
            // Sleep to the callback's time, as the sound card would wake
            // us; what's measured is the callback itself.
            let due = start + period * i as u32;
            while Instant::now() < due {
                if busy_wait {
                    std::hint::spin_loop();
                } else {
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                }
            }
        }
        let callback = Instant::now();
        engine.process_interleaved(&mut out, 2);
        let took = callback.elapsed();
        busy += took;
        worst = worst.max(took);
        sink += out[0];
    }
    let elapsed = busy.as_secs_f64();
    let cpu = thread_cpu_seconds()
        .zip(cpu_start)
        .map(|(end, start)| end - start);
    assert!(sink.is_finite());
    let audio_seconds = blocks as f64 * device_block as f64 / RATE as f64;
    println!(
        "\nrender {audio_seconds:.1} s of audio{}: {:.3} s, {:.1}% of one core ({:.1} µs per {device_block}-frame callback, worst {:.1} µs)",
        if paced {
            ", paced like a sound card"
        } else {
            ""
        },
        elapsed,
        100.0 * elapsed / audio_seconds,
        1e6 * elapsed / blocks as f64,
        1e6 * worst.as_secs_f64()
    );
    if let Some(cpu) = cpu {
        // Time the audio thread was on a core: unlike the wall clock, not
        // inflated by other programs taking turns with it.
        println!(
            "  audio thread's own CPU time: {cpu:.3} s, {:.1}% of one core",
            100.0 * cpu / audio_seconds
        );
    }
    drop(sources);
}

/// The calling thread's CPU time so far, where the system says (Linux).
fn thread_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let nanos: f64 = stat.split_whitespace().next()?.parse().ok()?;
    Some(nanos * 1e-9)
}
