//! Try the plugin host on your own plugins.
//!
//! ```sh
//! cargo run -p plugins --example plugin_cli -- scan                 # list what's installed
//! cargo run -p plugins --example plugin_cli -- params Vital         # a plugin's parameters
//! cargo run -p plugins --example plugin_cli -- render Vital out.wav # a chord through an instrument
//! cargo run -p plugins --example plugin_cli -- render OTT out.wav   # a test signal through an effect
//! cargo run -p plugins --example plugin_cli -- edit Vital           # open its editor until closed
//! ```
//!
//! A plugin is named by (part of) its name, its id, or the path of its
//! file. `scan` searches the standard folders (yabridge's included) or the
//! folders given after `--path` (every command takes it), scanning out of process (this program runs
//! itself as the scanner) with the cache in `~/.cache/canopy`. `render`
//! takes `--seconds N`, `--rate HZ`, `--block N`, `--note KEY` and
//! `--set ID=VALUE` (repeatable; CLAP values are plain, VST3 and VST2 ones
//! 0 to 1), and writes a 32-bit float stereo WAV.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use plugins::scan::{self, ScanCommand, ScanOptions};
use plugins::{
    AudioConfig, EditorOptions, Event, Host, ParamId, PluginFormat, PluginInfo, PluginKind,
    Transport,
};

fn main() {
    // When `scan` starts this program as a scanner, it scans and exits here.
    scan::run_scanner_if_requested();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("scan") => list(&args[1..]),
        Some("params") if args.len() > 1 => params(&args[1], &args[2..]),
        Some("render") if args.len() > 2 => render(&args[1], Path::new(&args[2]), &args[3..]),
        Some("edit") if args.len() > 1 => edit(&args[1], &args[2..]),
        _ => Err("usage: plugin_cli scan [--path DIR]... | params PLUGIN | render PLUGIN OUT.wav [options] | edit PLUGIN".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn options(args: &[String]) -> Result<ScanOptions, String> {
    let mut options = ScanOptions::new(ScanCommand::current_exe().map_err(|e| e.to_string())?);
    let paths: Vec<PathBuf> = args
        .windows(2)
        .filter(|w| w[0] == "--path")
        .map(|w| PathBuf::from(&w[1]))
        .collect();
    if !paths.is_empty() {
        options.paths = PluginFormat::ALL
            .into_iter()
            .flat_map(|f| paths.iter().map(move |p| (f, p.clone())))
            .collect();
    }
    Ok(options)
}

fn list(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    println!("Searching:");
    for (format, path) in &options.paths {
        println!("  {format:4} {}", path.display());
    }
    let started = Instant::now();
    let report = scan::scan(&options);
    println!(
        "\n{} plugins ({} files read, {} from the cache) in {:.1} s\n",
        report.plugins.len(),
        report.read,
        report.cached,
        started.elapsed().as_secs_f32()
    );
    for p in &report.plugins {
        let ports = |ports: &[plugins::AudioPortInfo]| {
            ports
                .iter()
                .map(|p| p.channels.to_string())
                .collect::<Vec<_>>()
                .join("+")
        };
        println!(
            "{:4} {:11} {} — {} {}{}\n     id {}  audio in {} out {}  notes in {}\n     {}",
            p.format,
            p.kind.name(),
            p.name,
            p.vendor,
            p.version,
            if p.bridged {
                "  (Windows, via yabridge)"
            } else {
                ""
            },
            p.id,
            ports(&p.audio_inputs),
            ports(&p.audio_outputs),
            p.note_inputs.len(),
            p.path.display()
        );
    }
    if !report.failures.is_empty() {
        println!("\nCouldn't read:");
        for f in &report.failures {
            println!("  {} {}: {}", f.format, f.path.display(), f.error);
        }
    }
    Ok(())
}

/// The plugin `name` picks: a path, an id, or part of a name.
fn find(name: &str, args: &[String]) -> Result<PluginInfo, String> {
    let options = options(args)?;
    let path = Path::new(name);
    let plugins = if path.exists() {
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let format = match extension.as_str() {
            "vst3" => PluginFormat::Vst3,
            "so" | "dll" => PluginFormat::Vst2,
            _ => PluginFormat::Clap,
        };
        scan::scan_file(&options, format, path)?
    } else {
        scan::scan(&options).plugins
    };
    let lower = name.to_lowercase();
    plugins
        .iter()
        .find(|p| p.id == name)
        .or_else(|| plugins.iter().find(|p| p.name.to_lowercase() == lower))
        .or_else(|| {
            plugins
                .iter()
                .find(|p| p.name.to_lowercase().contains(&lower))
        })
        .or(if path.exists() { plugins.first() } else { None })
        .cloned()
        .ok_or_else(|| format!("no plugin called {name}; try `scan`"))
}

fn params(name: &str, args: &[String]) -> Result<(), String> {
    let info = find(name, args)?;
    let host = Host::new();
    let instance = host.load(&info).map_err(|e| e.to_string())?;
    println!(
        "{} ({}, {}): {} parameters",
        info.name,
        info.vendor,
        info.format,
        instance.params().len()
    );
    for p in instance.params() {
        let value = instance.param_value(p.id).unwrap_or(p.default);
        let text = instance.param_text(p.id, value).unwrap_or_default();
        let mut flags = Vec::new();
        for (on, name) in [
            (p.flags.stepped, "stepped"),
            (p.flags.automatable, "automatable"),
            (p.flags.modulatable, "modulatable"),
            (p.flags.read_only, "read-only"),
            (p.flags.hidden, "hidden"),
        ] {
            if on {
                flags.push(name);
            }
        }
        let module = if p.module.is_empty() {
            String::new()
        } else {
            format!("{}/", p.module)
        };
        println!(
            "  {:>6} {module}{}: {value:.4} ({text}) range {}..{} default {} {}",
            p.id.0,
            p.name,
            p.min,
            p.max,
            p.default,
            flags.join(" ")
        );
    }
    println!(
        "latency {} samples, tail {:?}, editor: {}",
        instance.latency(),
        instance.tail(),
        instance.has_editor()
    );
    Ok(())
}

fn render(name: &str, out: &Path, args: &[String]) -> Result<(), String> {
    let value = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let number =
        |flag: &str, default: f64| value(flag).and_then(|v| v.parse().ok()).unwrap_or(default);
    let seconds = number("--seconds", 3.0);
    let rate = number("--rate", 48_000.0);
    let block = number("--block", 512.0) as usize;
    let key = number("--note", 60.0) as u8;

    let info = find(name, args)?;
    let host = Host::new();
    let mut instance = host.load(&info).map_err(|e| e.to_string())?;
    for set in args.windows(2).filter(|w| w[0] == "--set").map(|w| &w[1]) {
        let (id, v) = set.split_once('=').ok_or("--set takes ID=VALUE")?;
        let id = ParamId(id.parse().map_err(|_| "bad parameter id")?);
        instance.set_param(id, v.parse().map_err(|_| "bad parameter value")?);
    }
    let mut config = AudioConfig::new(rate, block as u32);
    config.offline = true;
    let mut processor = instance.activate(config).map_err(|e| e.to_string())?;
    println!(
        "{} ({}): latency {} samples",
        info.name,
        info.format,
        instance.latency()
    );

    let frames = (seconds * rate) as usize;
    let instrument = info.kind == PluginKind::Instrument;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    if !instrument {
        // A test signal: a decaying 110 Hz saw plucked every half second.
        for (i, (l, r)) in left.iter_mut().zip(&mut right).enumerate() {
            let t = i as f64 / rate;
            let since = t % 0.5;
            let saw = 2.0 * ((t * 110.0) % 1.0) - 1.0;
            let s = (saw * (-since * 6.0).exp() * 0.5) as f32;
            *l = s;
            *r = s;
        }
    }
    let chord = [key, key + 4, key + 7];
    let mut transport = Transport {
        playing: true,
        ..Transport::STOPPED
    };
    let mut events = Vec::new();
    for start in (0..frames).step_by(block) {
        let n = block.min(frames - start);
        events.clear();
        let off_at = frames * 2 / 3;
        for &k in &chord {
            if start == 0 {
                events.push(Event::NoteOn {
                    frame: 0,
                    channel: 0,
                    key: k,
                    velocity: 0.8,
                });
            }
            if (start..start + n).contains(&off_at) {
                let frame = (off_at - start) as u32;
                events.push(Event::NoteOff {
                    frame,
                    channel: 0,
                    key: k,
                    velocity: 0.5,
                });
            }
        }
        transport.sample_position = start as i64;
        transport.beat = start as f64 / rate * transport.tempo / 60.0;
        processor.process_stereo(
            &mut left[start..start + n],
            &mut right[start..start + n],
            None,
            &events,
            &transport,
        );
        host.idle();
    }
    processor.stop_processing();
    instance.deactivate(processor);
    write_wav(out, rate as u32, &left, &right).map_err(|e| e.to_string())?;
    let peak = left
        .iter()
        .chain(&right)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    println!("wrote {} ({frames} frames, peak {peak:.3})", out.display());
    Ok(())
}

fn edit(name: &str, args: &[String]) -> Result<(), String> {
    let info = find(name, args)?;
    let host = Host::new();
    let mut instance = host.load(&info).map_err(|e| e.to_string())?;
    if !instance.has_editor() {
        return Err(format!("{} has no editor this host can show", info.name));
    }
    // Plugins may need to be active to show everything; process silence.
    let mut processor = instance
        .activate(AudioConfig::new(48_000.0, 512))
        .map_err(|e| e.to_string())?;
    instance
        .open_editor(&EditorOptions::titled(format!(
            "{} — plugin_cli",
            info.name
        )))
        .map_err(|e| e.to_string())?;
    println!("Editor open; close its window to quit.");
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    loop {
        plugins::editor::pump_messages();
        let next = host.idle();
        for event in instance.take_events() {
            println!("{event:?}");
            if event == plugins::PluginEvent::EditorClosed {
                instance.deactivate(processor);
                return Ok(());
            }
        }
        left.fill(0.0);
        right.fill(0.0);
        processor.process_stereo(&mut left, &mut right, None, &[], &Transport::STOPPED);
        std::thread::sleep(
            next.unwrap_or(Duration::from_millis(10))
                .min(Duration::from_millis(10)),
        );
    }
}

/// A 32-bit float stereo WAV.
fn write_wav(path: &Path, rate: u32, left: &[f32], right: &[f32]) -> std::io::Result<()> {
    let data = (left.len() * 8) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 8).to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for (l, r) in left.iter().zip(right) {
        bytes.extend_from_slice(&l.to_le_bytes());
        bytes.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, bytes)
}
