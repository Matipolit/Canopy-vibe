//! Writes FL Studio projects that play one sound through one FL effect, to
//! render in FL and in canopy and compare: how FL's own effects respond to
//! each knob, measured instead of guessed.
//!
//! ```sh
//! cargo run -p flp --example probe -- list song.flp [more.flp ...]
//! cargo run -p flp --example probe -- write out.flp --signal 'C:\probe\signal.wav' \
//!     --seconds 8 [--from song.flp --effect "Fruity Reeverb 2" [--nth 0]] [--set 13=64 ...]
//! cargo run -p flp --example probe -- batch out-dir --signal 'C:\probe\signal.wav' \
//!     --seconds 12 --states states.tsv
//! cargo run -p flp --example probe -- settings probe.flp [more.flp ...]
//! ```
//!
//! `list` prints every effect in the files' mixer slots, its state as the
//! 32-bit numbers most of FL's own effects save (a version, then their
//! knobs in automation's order) and as hex. `write` makes a project in
//! FL 26's layout at 120 BPM: one audio clip of `--signal` (the path as FL
//! on the rendering machine will find it), `--seconds` long, on playlist
//! track 1, played through insert 1. With `--from` and `--effect`, that
//! insert's first slot holds the effect, its state copied from the `--nth`
//! instance in `--from`; each `--set i=v` then overwrites 32-bit number `i`
//! of the state (0 is the version).
//!
//! `batch` writes one such project per line of `--states` (an effect's
//! internal name, a tab and its state in hex; an empty name for none), as
//! `<dir>/<line number>.flp`. FL loads only the first playlist item of a
//! project written this way, and plays nothing from Sampler channels
//! written with only the fields canopy reads, so each probe is a project
//! of its own.
//!
//! `settings` imports each file as canopy would and prints every built-in
//! effect it became, as its id and `param=value` pairs (only those the
//! import set), to run the same effect over the same signal with
//! `effects`' `apply_effect` example.
//!
//! Every project's song is `--seconds` long, at most 60: FL renders
//! whatever length it reads, and a malformed project once asked it for
//! 1.46 million bars.

use flp::write::{ChannelType, Item, SlotPlugin, Writer};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("write") => write(&args[1..]),
        Some("batch") => batch(&args[1..]),
        Some("settings") => settings(&args[1..]),
        _ => {
            eprintln!("usage: probe list <file.flp>... | probe write <out.flp> --signal <path> ...");
            std::process::exit(2);
        }
    }
}

fn parse(path: &str) -> flp::Project {
    let bytes = std::fs::read(path).unwrap_or_else(|err| fail(&format!("{path}: {err}")));
    flp::parse(&bytes).unwrap_or_else(|err| fail(&format!("{path}: {err}")))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn numbers(state: &[u8]) -> Vec<i32> {
    state
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn list(paths: &[String]) {
    for path in paths {
        let fl = parse(path);
        for insert in &fl.inserts {
            for slot in &insert.slots {
                let plugin = &slot.plugin;
                if plugin.internal_name.is_empty() {
                    continue;
                }
                let hex: String = plugin.state.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "{path}\tinsert {}\tslot {}\t{}\t{:?}\t{} bytes\t{:?}\t{}",
                    insert.index,
                    slot.index,
                    plugin.internal_name,
                    plugin.name.as_deref().unwrap_or(""),
                    plugin.state.len(),
                    numbers(&plugin.state),
                    if hex.len() > 400 { &hex[..400] } else { &hex },
                );
            }
        }
    }
}

fn settings(paths: &[String]) {
    for path in paths {
        let fl = parse(path);
        let plan = flp::Plan::new(&fl);
        let result = flp::import(&fl, &plan.choices(), &flp::Files::default(), &flp::Options::default());
        result.project.visit(|track, _| {
            for device in &track.devices {
                let params: Vec<String> =
                    device.params.iter().map(|(id, v)| format!("{id}={v}")).collect();
                println!("{path}\t{}\t{}\t{}", track.name, device.effect.id, params.join(" "));
            }
        });
    }
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn write(args: &[String]) {
    let out = args.first().unwrap_or_else(|| fail("write needs an output file"));
    let signal = value(args, "--signal").unwrap_or_else(|| fail("write needs --signal"));
    let seconds = seconds(args, 8.0);
    let nth: usize = value(args, "--nth").map_or(0, |s| s.parse().unwrap_or(0));

    let mut slots = Vec::new();
    if let (Some(from), Some(effect)) = (value(args, "--from"), value(args, "--effect")) {
        let fl = parse(from);
        let found = fl
            .inserts
            .iter()
            .flat_map(|insert| &insert.slots)
            .filter(|slot| slot.plugin.internal_name.eq_ignore_ascii_case(effect))
            .nth(nth)
            .unwrap_or_else(|| fail(&format!("no {effect} number {nth} in {from}")));
        let mut state = found.plugin.state.clone();
        for (i, arg) in args.iter().enumerate() {
            if arg != "--set" {
                continue;
            }
            let pair = args.get(i + 1).unwrap_or_else(|| fail("--set takes i=v"));
            let (index, number) = pair
                .split_once('=')
                .and_then(|(i, v)| Some((i.parse::<usize>().ok()?, v.parse::<i32>().ok()?)))
                .unwrap_or_else(|| fail(&format!("--set {pair}: expected i=v")));
            let at = index * 4;
            if at + 4 > state.len() {
                fail(&format!("--set {pair}: the state has {} numbers", state.len() / 4));
            }
            state[at..at + 4].copy_from_slice(&number.to_le_bytes());
        }
        slots.push(SlotPlugin {
            index: 0,
            internal_name: found.plugin.internal_name.clone(),
            name: found.plugin.name.clone(),
            state,
        });
    }

    save(out, signal, seconds, &slots);
}

fn seconds(args: &[String], default: f64) -> f64 {
    let seconds = value(args, "--seconds")
        .map(|s| s.parse().unwrap_or_else(|_| fail("--seconds takes a number")))
        .unwrap_or(default);
    if !(0.5..=60.0).contains(&seconds) {
        fail("--seconds must be between 0.5 and 60");
    }
    seconds
}

fn batch(args: &[String]) {
    let dir = args.first().unwrap_or_else(|| fail("batch needs an output folder"));
    let signal = value(args, "--signal").unwrap_or_else(|| fail("batch needs --signal"));
    let seconds = seconds(args, 12.0);
    let list = value(args, "--states").unwrap_or_else(|| fail("batch needs --states"));
    let text = std::fs::read_to_string(list).unwrap_or_else(|err| fail(&format!("{list}: {err}")));
    std::fs::create_dir_all(dir).unwrap_or_else(|err| fail(&format!("{dir}: {err}")));
    for (i, line) in text.lines().filter(|line| !line.is_empty()).enumerate() {
        let (name, hex) = line.split_once('\t').unwrap_or((line, ""));
        let slots = if name.is_empty() {
            Vec::new()
        } else {
            let state = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .unwrap_or_else(|_| fail(&format!("not hex: {hex}")));
            vec![SlotPlugin {
                index: 0,
                internal_name: name.to_owned(),
                name: None,
                state,
            }]
        };
        save(&format!("{dir}/{i}.flp"), signal, seconds, &slots);
    }
}

/// A project at 120 BPM playing `signal` once, as an audio clip on
/// playlist track 1, through insert 1 holding `slots`.
fn save(out: &str, signal: &str, seconds: f64, slots: &[SlotPlugin]) {
    let ppq = 96u16;
    let bpm = 120.0;
    let ticks = (seconds * bpm / 60.0 * f64::from(ppq)).round() as u32;
    let mut w = Writer::new(ppq, "26.1.0.5530");
    w.tempo(bpm).time_signature(4, 4);
    w.channel(0, ChannelType::AudioClip, "", "Signal")
        .sample(signal)
        .route_to(1);
    w.arrangement(0, "Arrangement", &[Item::clip(0, 0, ticks)]);
    w.current_arrangement(0);
    w.insert("", &[], &[]);
    w.insert("Probe", slots, &[0]);
    std::fs::write(out, w.finish()).unwrap_or_else(|err| fail(&format!("{out}: {err}")));
}
