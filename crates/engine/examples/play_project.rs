//! Plays a saved project from its start to its end as a sound card would,
//! and says how long each callback took: the spread, how many missed their
//! deadline, and where in the song the slowest ones were.
//!
//! `cargo run --release -p engine --example play_project -- song.canopy
//! [--rate 44100] [--block 256] [--unpaced | --busy] [--from <bar>] [--seconds <s>]
//! [--cache | --background]`
//!
//! `--cache` first renders every track that can be cached (`engine::cache`)
//! and plays from those renders, saying what they cost to make and hold.
//! `--background` plays live while a thread at idle priority renders the
//! caches over and over, as the app does while tracks sit unchanged, to see
//! whether that disturbs playback.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use engine::{AudioLookup, Command, Compiler, MAX_BLOCK, TrackCache, cache};
use media::AudioBuffer;
use project::{Project, SourceId, Track, TrackId};

/// The project's decoded sources, and the caches to play.
#[derive(Clone, Default)]
struct Lookup {
    sources: Arc<HashMap<SourceId, Arc<AudioBuffer>>>,
    caches: HashMap<TrackId, TrackCache>,
}

impl AudioLookup for Lookup {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.sources.get(&id).cloned()
    }

    fn track_cache(&self, track: &Track) -> Option<TrackCache> {
        self.caches.get(&track.id).cloned()
    }
}

/// Makes the calling thread run only when nothing else wants the CPU.
fn idle_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain system calls on the calling thread.
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
    }
}

/// Renders every track that can be cached: the renders, and the CPU time
/// each took.
fn render_all(
    project: &Project,
    rate: u32,
    lookup: &Lookup,
    cancel: &AtomicBool,
) -> Vec<(TrackId, Arc<AudioBuffer>, Duration)> {
    let mut renders = Vec::new();
    for track in project.track_ids() {
        if cache::cacheable(project, track).is_err() {
            continue;
        }
        let started = Instant::now();
        let Some(audio) = cache::render(project, track, rate, lookup.clone(), cancel, |_| {})
        else {
            break;
        };
        renders.push((track, Arc::new(audio), started.elapsed()));
    }
    renders
}

fn main() {
    let mut path = None;
    let (mut rate, mut block, mut paced, mut busy) = (44_100u32, 256usize, true, false);
    let (mut cached, mut background) = (false, false);
    let (mut from_bar, mut seconds) = (None::<u32>, None::<f64>);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = || {
            args.next()
                .expect("a number")
                .parse::<f64>()
                .expect("a number")
        };
        match arg.as_str() {
            "--rate" => rate = number() as u32,
            "--block" => block = number() as usize,
            "--unpaced" => paced = false,
            // Paced, but spinning to each callback instead of sleeping, so
            // the core never idles long enough to clock down.
            "--busy" => busy = true,
            "--from" => from_bar = Some(number() as u32),
            "--seconds" => seconds = Some(number()),
            "--cache" => cached = true,
            "--background" => background = true,
            s => path = Some(s.to_owned()),
        }
    }
    let path = path.expect("a .canopy file");
    let text = std::fs::read_to_string(&path).expect("readable project");
    let project = Project::from_file_string(&text).expect("a project");

    let mut sources: Vec<(SourceId, Arc<AudioBuffer>)> = Vec::new();
    for source in &project.sources {
        match media::load(&source.path, rate) {
            Ok(loaded) => sources.push((source.id, loaded.audio)),
            Err(err) => eprintln!("couldn't load {}: {err}", source.path.display()),
        }
    }
    let mut lookup = Lookup {
        sources: Arc::new(sources.into_iter().collect()),
        caches: HashMap::new(),
    };
    let names: HashMap<TrackId, String> = {
        let mut names = HashMap::new();
        project.visit(|t, _| {
            names.insert(t.id, t.name.clone());
        });
        names
    };
    for track in project.track_ids() {
        if let Err(why) = cache::cacheable(&project, track) {
            println!("  not cacheable: {:<24} {why:?}", names[&track]);
        }
    }
    if cached {
        let started = Instant::now();
        let renders = render_all(&project, rate, &lookup, &AtomicBool::new(false));
        let mut bytes = 0;
        for (track, audio, took) in &renders {
            let seconds = audio.duration_seconds();
            println!(
                "  cached {:<24} {:>6.1} s of audio in {:>6.0} ms: {:>5.2}% of a core to render",
                names[track],
                seconds,
                took.as_secs_f64() * 1e3,
                100.0 * took.as_secs_f64() / seconds
            );
            bytes += audio.frames() * 8;
            lookup.caches.insert(*track, cache::active(audio.clone()));
        }
        println!(
            "rendered {} caches in {:.2} s, holding {:.0} MB",
            renders.len(),
            started.elapsed().as_secs_f64(),
            bytes as f64 / 1e6
        );
    }
    let stop = Arc::new(AtomicBool::new(false));
    let renderer = background.then(|| {
        let (project, lookup, stop) = (project.clone(), lookup.clone(), stop.clone());
        std::thread::spawn(move || {
            idle_priority();
            let mut rounds = 0;
            while !stop.load(Ordering::Relaxed) {
                render_all(&project, rate, &lookup, &stop);
                rounds += 1;
            }
            rounds
        })
    });

    let mut compiler = Compiler::new(rate);
    let started = Instant::now();
    let first = compiler.compile(&project, lookup.clone());
    println!(
        "first compile: {:.1} ms",
        started.elapsed().as_secs_f64() * 1e3
    );
    let (mut handle, mut engine) = engine::create(rate as f32);
    println!("render worker threads: {}", engine.workers());
    handle.send(Command::Arrangement(first)).ok().unwrap();
    let mut out = vec![0.0f32; 2 * block];
    engine.process_interleaved(&mut out[..2 * MAX_BLOCK], 2);
    handle.collect_garbage();
    // The Wavetable's bank fills in the background.
    std::thread::sleep(Duration::from_millis(500));

    let tempo = &project.tempo;
    let start_tick = from_bar.map_or(midi::Tick::ZERO, |bar| tempo.bar_start(i64::from(bar)));
    let start = tempo.tick_to_sample(start_tick, rate).max(0) as u64;
    let end = tempo.tick_to_sample(project.end(), rate).max(0) as u64;
    let end = seconds.map_or(end, |s| start + (s * f64::from(rate)) as u64);
    handle.send(Command::Seek(start)).ok().unwrap();
    handle.send(Command::SetPlaying(true)).ok().unwrap();

    let deadline = Duration::from_secs_f64(block as f64 / f64::from(rate));
    let blocks = ((end - start) as usize).div_ceil(block);
    let mut times = Vec::with_capacity(blocks);
    let clock = Instant::now();
    for i in 0..blocks {
        if paced {
            let due = clock + deadline * i as u32;
            let now = Instant::now();
            if busy {
                while Instant::now() < due {
                    std::hint::spin_loop();
                }
            } else if now < due {
                std::thread::sleep(due - now);
            }
        }
        let callback = Instant::now();
        engine.process_interleaved(&mut out, 2);
        times.push(callback.elapsed());
        handle.collect_garbage();
    }

    stop.store(true, Ordering::Relaxed);
    if let Some(renderer) = renderer {
        let rounds = renderer.join().unwrap();
        println!("background: {rounds} rounds of renders while playing");
    }
    let seconds_of_audio = (blocks * block) as f64 / f64::from(rate);
    let total: Duration = times.iter().sum();
    let mut sorted = times.clone();
    sorted.sort();
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    let over = |share: f64| {
        times
            .iter()
            .filter(|t| t.as_secs_f64() > deadline.as_secs_f64() * share)
            .count()
    };
    println!(
        "\n{seconds_of_audio:.1} s of audio in {}-frame callbacks at {rate} Hz{}: deadline {:.2} ms",
        block,
        if paced { ", paced" } else { "" },
        deadline.as_secs_f64() * 1e3
    );
    println!(
        "average {:.1}% of one core; median {:.2} ms, 99% {:.2} ms, 99.9% {:.2} ms, worst {:.2} ms",
        100.0 * total.as_secs_f64() / seconds_of_audio,
        at(0.5),
        at(0.99),
        at(0.999),
        at(1.0)
    );
    println!(
        "callbacks over the deadline: {} of {} (over half of it: {})",
        over(1.0),
        times.len(),
        over(0.5)
    );
    let mut worst: Vec<(usize, Duration)> = times.iter().copied().enumerate().collect();
    worst.sort_by_key(|w| std::cmp::Reverse(w.1));
    println!("slowest callbacks:");
    let quarter = tempo.tick_to_sample(midi::Tick::QUARTER, rate) as f64;
    for (i, took) in worst.iter().take(12) {
        let sample = start as f64 + (i * block) as f64;
        let beats = sample / quarter;
        println!(
            "  {:>7.2} ms at {:>6.2} s (bar {:.2})",
            took.as_secs_f64() * 1e3,
            sample / f64::from(rate),
            1.0 + beats / f64::from(tempo.time_signature.numerator)
        );
    }
}
