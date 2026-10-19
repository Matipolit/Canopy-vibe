//! Prints the events in an FL Studio file, one a line, for working out what
//! a file holds: `cargo run -p flp --example flp_dump -- song.flp`. With
//! `--model`, prints what the parser made of it instead; with `--import`,
//! the canopy project an import with the suggested replacements makes.

use flp::reader::{self, TextEncoding, Value};

fn main() {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let model = args.iter().any(|a| a == "--model");
    let import = args.iter().any(|a| a == "--import");
    args.retain(|a| a != "--model" && a != "--import");
    let Some(path) = args.first().cloned() else {
        eprintln!("usage: flp_dump [--model | --import] <file.flp>");
        std::process::exit(2);
    };
    if import {
        let bytes = std::fs::read(&path).unwrap_or_else(|err| {
            eprintln!("{}: {err}", path.to_string_lossy());
            std::process::exit(1);
        });
        match flp::parse(&bytes) {
            Ok(project) => show_import(&project, std::path::Path::new(&path)),
            Err(err) => eprintln!("{err}"),
        }
        return;
    }
    if model {
        let bytes = std::fs::read(&path).unwrap_or_else(|err| {
            eprintln!("{}: {err}", path.to_string_lossy());
            std::process::exit(1);
        });
        match flp::parse(&bytes) {
            Ok(project) => summary(&project),
            Err(err) => eprintln!("{err}"),
        }
        return;
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{}: {err}", path.to_string_lossy());
            std::process::exit(1);
        }
    };
    let read = match reader::read(&bytes) {
        Ok(read) => read,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    println!("{:?}", read.header);
    let mut encoding = TextEncoding::Unknown;
    for (index, event) in read.events.iter().enumerate() {
        let shown = match event.value {
            Value::Byte(v) => format!("byte {v}"),
            Value::Word(v) => format!("word {v}"),
            Value::Dword(v) => format!("dword {v} ({:#x}, {} as i32)", v, v as i32),
            Value::Data(data) if event.id == 199 => {
                let text = String::from_utf8_lossy(data)
                    .trim_end_matches('\0')
                    .to_owned();
                encoding = TextEncoding::for_version(&text);
                format!("version {text:?}")
            }
            Value::Data(data) if reader::is_text(event.id) => {
                format!("text {:?}", encoding.decode(data))
            }
            Value::Data(data) => {
                let hex: Vec<String> = data.iter().take(64).map(|b| format!("{b:02x}")).collect();
                let more = if data.len() > 64 { " …" } else { "" };
                format!("data[{}] {}{more}", data.len(), hex.join(" "))
            }
        };
        println!("{index:6} {:3} {shown}", event.id);
    }
    if read.truncated {
        println!("(truncated)");
    }
}

/// What an import with the suggested replacements makes. Samples aren't
/// read (no decoder here), so they all count as missing.
fn show_import(fl: &flp::Project, path: &std::path::Path) {
    let plan = flp::Plan::new(fl);
    for item in &plan.items {
        println!(
            "{:?} {:?} -> {} {} (used by {})",
            item.role,
            item.name,
            item.suggestion.label(),
            if item.recognized { "" } else { "(guess)" },
            item.used_by.join(", ")
        );
    }
    let files = flp::Files::find(fl, Some(path), &|_| None);
    let result = flp::import(fl, &plan.choices(), &files, &flp::Options::default());
    println!("{:?}", result.summary);
    for line in &result.report {
        println!("report: {line}");
    }
    let p = &result.project;
    println!(
        "master {:?} devices {:?}",
        p.master,
        p.master_devices
            .iter()
            .map(|d| d.name())
            .collect::<Vec<_>>()
    );
    p.visit(|track, depth| {
        let kind = match &track.kind {
            project::TrackKind::Midi {
                instrument, file, ..
            } => format!("midi {} {:?}", instrument.id, file),
            project::TrackKind::Audio { .. } => "audio".to_owned(),
            project::TrackKind::Group { .. } => "group".to_owned(),
        };
        println!(
            "{}{} [{kind}] vol {:.1} pan {:.2}{} clips {} devices {:?} lanes {:?}",
            "  ".repeat(depth + 1),
            track.name,
            track.mixer.volume_db,
            track.mixer.pan,
            if track.mixer.mute { " muted" } else { "" },
            track.clips().len(),
            track.devices.iter().map(|d| d.name()).collect::<Vec<_>>(),
            track
                .automation
                .iter()
                .map(|l| (&l.target, l.points.len()))
                .collect::<Vec<_>>()
        );
    });
}

fn summary(p: &flp::Project) {
    println!(
        "version {:?}, format {}, ppq {}, tempo {:?}, time {:?}, {} channels (header {}), truncated {}",
        p.version,
        p.format,
        p.ppq,
        p.tempo,
        p.time_signature,
        p.channels.len(),
        p.channel_count,
        p.truncated
    );
    for c in &p.channels {
        let plugin = c.plugin.as_ref().map(|p| {
            format!(
                "{:?} vst={:?} state={}B",
                p.internal_name,
                p.vst.as_ref().map(|v| &v.name),
                p.state.len()
            )
        });
        println!(
            "  ch {:3} {:?} {:?} plugin={:?} sample={:?} vol={:?} pan={:?} insert={:?} enabled={} auto={:?} children={:?}",
            c.iid,
            c.kind,
            c.name,
            plugin,
            c.sample_path,
            c.volume,
            c.pan,
            c.insert,
            c.enabled,
            c.automation.as_ref().map(|a| a
                .points
                .iter()
                .map(|p| (p.position, p.value, p.tension))
                .collect::<Vec<_>>()),
            c.children
        );
    }
    for pat in &p.patterns {
        let channels: std::collections::BTreeSet<u16> =
            pat.notes.iter().map(|n| n.channel).collect();
        println!(
            "  pat {:3} {:?} len={:?} notes={} channels={:?} ctrl={} first={:?}",
            pat.iid,
            pat.name,
            pat.length,
            pat.notes.len(),
            channels,
            pat.controller_events,
            pat.notes.first()
        );
    }
    for a in &p.arrangements {
        println!(
            "  arrangement {} {:?}: {} items, {} markers, {} tracks (current {:?})",
            a.iid,
            a.name,
            a.items.len(),
            a.markers.len(),
            a.tracks.len(),
            p.current_arrangement
        );
        for item in &a.items {
            println!("    {item:?}");
        }
        for m in &a.markers {
            println!("    marker {m:?}");
        }
    }
    for i in &p.inserts {
        let default = i.name.is_none()
            && i.slots.is_empty()
            && i.volume.is_none_or(|v| v == 12800)
            && i.pan.is_none_or(|v| v == 0)
            && i.outputs() == [0];
        if default {
            continue;
        }
        println!(
            "  insert {:3} {:?} vol={:?} pan={:?} flags={:?} outputs={:?}",
            i.index,
            i.name,
            i.volume,
            i.pan,
            i.flags,
            i.outputs()
        );
        for s in &i.slots {
            println!(
                "    slot {} {:?} name={:?} vst={:?} enabled={:?} mix={:?} state={}B",
                s.index,
                s.plugin.internal_name,
                s.plugin.name,
                s.plugin.vst.as_ref().map(|v| &v.name),
                s.enabled,
                s.mix,
                s.plugin.state.len()
            );
        }
    }
    for link in &p.automation_links {
        println!("  link {link:?}");
    }
}
