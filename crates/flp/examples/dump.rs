//! Prints what an FL Studio project holds, as the importer reads it: for
//! looking at real files while working on the importer.
//!
//! `cargo run -p flp --example dump -- path/to/project.flp [--events]`

use std::collections::BTreeMap;
use std::path::Path;

use flp::reader::{self, Value};
use flp::{Files, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let events = args.iter().any(|a| a == "--events");
    let devices = args.iter().any(|a| a == "--devices");
    // `--state=NAME`: the saved state of every slot whose plugin's name
    // holds NAME, as little-endian 32-bit words.
    let state = args
        .iter()
        .find_map(|a| a.strip_prefix("--state="))
        .map(str::to_lowercase);
    // `--save=DIR`: with `--state`, also writes each of those states to a
    // file in DIR.
    let save = args.iter().find_map(|a| a.strip_prefix("--save="));
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        dump(
            Path::new(path),
            events,
            devices,
            state.as_deref(),
            save.map(Path::new),
        );
    }
}

fn dump(
    path: &Path,
    show_events: bool,
    show_devices: bool,
    show_state: Option<&str>,
    save: Option<&Path>,
) {
    println!("==== {}", path.display());
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => return println!("can't read: {err}"),
    };
    let fl = match flp::parse(&bytes) {
        Ok(fl) => fl,
        Err(err) => return println!("can't parse: {err}"),
    };
    println!(
        "FL {} ppq {} tempo {:?} time {:?} title {:?} truncated {}",
        fl.version.as_deref().unwrap_or("?"),
        fl.ppq,
        fl.tempo,
        fl.time_signature,
        fl.title,
        fl.truncated
    );

    println!("-- channels ({})", fl.channels.len());
    for c in &fl.channels {
        let plugin = c.plugin.as_ref().map(|p| {
            format!(
                "{} [{}{}] state {}B",
                p.plugin_name(),
                p.internal_name,
                p.vst
                    .as_ref()
                    .map(|v| format!(", vst {} by {}", v.name, v.vendor))
                    .unwrap_or_default(),
                p.state.len()
            )
        });
        println!(
            "  #{:<3} {:?} {:?} insert {:?} vol {:?} pan {:?} on {} {} {}{}",
            c.iid,
            c.kind,
            c.display_name(),
            c.insert,
            c.volume,
            c.pan,
            c.enabled,
            plugin.unwrap_or_default(),
            c.sample_path.as_deref().unwrap_or(""),
            if c.children.is_empty() {
                String::new()
            } else {
                format!(" children {:?}", c.children)
            }
        );
        if let Some(a) = &c.automation {
            println!("        automation: {} points", a.points.len());
        }
        if let Some(preset) = c
            .plugin
            .as_ref()
            .and_then(|p| flp::vital::Preset::from_state(&p.state))
        {
            let converted = flp::vital::convert(&preset);
            let tables: Vec<String> = converted
                .tables
                .iter()
                .map(|t| {
                    t.as_ref().map_or("-".to_owned(), |t| {
                        format!("{} ({} frames)", t.name, t.frames.len())
                    })
                })
                .collect();
            let effects: Vec<&str> = converted.effects.iter().map(|(e, _)| *e).collect();
            println!(
                "        vital {:?}: {} params, tables {:?}, effects {:?}, dropped {:?}",
                preset.name,
                converted.params.len(),
                tables,
                effects,
                converted.dropped
            );
        }
    }

    println!("-- patterns ({})", fl.patterns.len());
    for p in &fl.patterns {
        let fine = p.notes.iter().filter(|n| n.fine_pitch != 120).count();
        let pan = p.notes.iter().filter(|n| n.pan != 64).count();
        let slides = p.notes.iter().filter(|n| n.is_slide()).count();
        let porta = p.notes.iter().filter(|n| n.flags != 0).count();
        println!(
            "  #{:<3} {:?} notes {} len {:?} controllers {} fine {} pan {} slides {} flagged {}",
            p.iid,
            p.display_name(),
            p.notes.len(),
            p.length,
            p.controller_events,
            fine,
            pan,
            slides,
            porta
        );
    }

    println!("-- arrangements ({})", fl.arrangements.len());
    for a in &fl.arrangements {
        println!(
            "  #{} {:?} items {} markers {} tracks {}",
            a.iid,
            a.name,
            a.items.len(),
            a.markers.len(),
            a.tracks.len()
        );
        for m in &a.markers {
            println!(
                "     marker @{} action {} {:?} {:?}/{:?}",
                m.position, m.action, m.name, m.numerator, m.denominator
            );
        }
    }

    println!("-- inserts");
    for i in &fl.inserts {
        if i.slots.is_empty() && i.name.is_none() && i.index > 0 {
            continue;
        }
        println!(
            "  {:<3} {:?} vol {:?} pan {:?} routes {:?} levels {:?} flags {:?}",
            i.index,
            i.display_name(),
            i.volume,
            i.pan,
            i.routes,
            i.route_levels,
            i.flags
        );
        for s in &i.slots {
            let p = &s.plugin;
            println!(
                "      slot {} {} [{}] on {:?} mix {:?} state {}B",
                s.index,
                p.display_name(),
                p.internal_name,
                s.enabled,
                s.mix,
                p.state.len()
            );
            if show_state.is_some_and(|name| p.display_name().to_lowercase().contains(name)) {
                let words: Vec<i32> = p
                    .state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|w| i32::from_le_bytes(*w))
                    .collect();
                println!("        words {words:?}");
                println!("        bytes {:02x?}", &p.state[..p.state.len().min(64)]);
                if let Some(dir) = save {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                    let file = dir.join(format!(
                        "{stem} {} {} {}.bin",
                        i.index,
                        s.index,
                        p.display_name()
                    ));
                    if let Err(err) = std::fs::write(&file, &p.state) {
                        println!("        can't save {}: {err}", file.display());
                    }
                }
            }
        }
    }

    println!("-- automation links ({})", fl.automation_links.len());
    for l in &fl.automation_links {
        let name = fl.channel(l.channel).map_or_else(
            || format!("controller {:#x}", l.channel),
            |c| c.display_name(),
        );
        println!("  {name}: {:?}", l.target);
    }

    let files = Files::find(&fl, Some(path), &|p| {
        Some(flp::AudioInfo {
            sample_rate: 44_100,
            frames: std::fs::metadata(p).map_or(0, |m| m.len() / 4),
            channels: 2,
        })
    });
    let plan = flp::Plan::new(&fl);
    println!("-- plan");
    for item in &plan.items {
        println!(
            "  {:?} {:?} -> {} {} (used by {})",
            item.role,
            item.name,
            item.suggestion.label(),
            if item.recognized { "" } else { "(guess)" },
            item.used_by.join(", ")
        );
    }
    let import = flp::import(&fl, &plan.choices(), &files, &Options::default());
    println!("-- import: {:?}", import.summary);
    for line in &import.report {
        println!("  {line}");
    }
    if show_devices {
        import.project.visit(|track, depth| {
            for device in &track.devices {
                let Some(descriptor) = device.effect.descriptor() else {
                    continue;
                };
                // Only what differs from the defaults.
                let changed: Vec<String> = descriptor
                    .params
                    .iter()
                    .filter_map(|p| {
                        let value = *device.params.get(p.id)?;
                        (value != p.default_value())
                            .then(|| format!("{}={}", p.id, p.format(value)))
                    })
                    .collect();
                println!(
                    "  {}{} / {}{}: {}",
                    "  ".repeat(depth),
                    track.name,
                    device.name(),
                    if device.bypassed { " (bypassed)" } else { "" },
                    changed.join(" ")
                );
            }
            for lane in &track.automation {
                println!(
                    "  {}{} lane {:?}: {} points",
                    "  ".repeat(depth),
                    track.name,
                    lane.target,
                    lane.points.len()
                );
            }
        });
    }

    let events = reader::read(&bytes).expect("parsed once");
    let mut counts: BTreeMap<u8, (usize, usize)> = BTreeMap::new();
    for e in &events.events {
        let entry = counts.entry(e.id).or_default();
        entry.0 += 1;
        if let Value::Data(d) = e.value {
            entry.1 += d.len();
        }
    }
    println!("-- event ids (id: count, bytes)");
    let line: Vec<String> = counts
        .iter()
        .map(|(id, (n, b))| format!("{id}:{n}/{b}"))
        .collect();
    println!("  {}", line.join(" "));
    if show_events {
        for e in &events.events {
            match e.value {
                Value::Data(d) => {
                    println!("  {} [{}] {:02x?}", e.id, d.len(), &d[..d.len().min(48)])
                }
                other => println!("  {} {:?}", e.id, other),
            }
        }
    }
}
