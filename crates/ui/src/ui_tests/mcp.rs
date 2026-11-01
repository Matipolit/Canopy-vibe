//! LLM clients over a real socket: the token, reads, and edits that wait
//! for the user's gestures and dialogs, and the switch that refuses them.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use project::{NewTrack, TrackPosition};
use serde_json::{Value, json};

use super::Rig;
use crate::mcp::Server;

/// Starts the app's listener with its advert in a folder of the test's
/// own (never the user's), and returns the advert's path.
pub(super) fn listen(rig: &mut Rig, test: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("canopy-ui-mcp-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let advert = dir.join("mcp.json");
    let server = Server::start_at(advert.clone(), Arc::new(|| {})).expect("listens");
    rig.app_mut().mcp = Some(server);
    Scratch(advert)
}

/// A test's own folder for its token file, gone when the test ends.
pub(super) struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A client as the bridge is one: it has read the advert and given the
/// token.
pub(super) struct Client {
    stream: BufReader<TcpStream>,
    /// What has arrived of a line not yet finished.
    partial: Vec<u8>,
    next_id: u64,
}

impl Client {
    pub(super) fn connect(advert: &Path) -> Client {
        let advert = ::mcp::connection::read_advert(advert).expect("an advert");
        let mut client = Client::raw(advert.port);
        let hello = ::mcp::connection::hello(&advert.token, Some("Test Client"));
        client.write(&hello);
        // The connection's thread answers the token by itself.
        let welcome = client.line_within(Duration::from_secs(5));
        assert_eq!(welcome.as_deref(), Some(::mcp::connection::WELCOME));
        client
    }

    fn raw(port: u16) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        Client {
            stream: BufReader::new(stream),
            partial: Vec::new(),
            next_id: 1,
        }
    }

    fn write(&mut self, line: &str) {
        let stream = self.stream.get_mut();
        writeln!(stream, "{line}").unwrap();
        stream.flush().unwrap();
    }

    /// A line if one arrives within `wait`, without running frames.
    fn line_within(&mut self, wait: Duration) -> Option<String> {
        let start = Instant::now();
        loop {
            if let Some(line) = self.poll() {
                return Some(line);
            }
            if start.elapsed() > wait {
                return None;
            }
        }
    }

    /// A finished line, if one has arrived; panics if the app closed the
    /// connection.
    fn poll(&mut self) -> Option<String> {
        match self.stream.read_until(b'\n', &mut self.partial) {
            Ok(0) => panic!("the app closed the connection"),
            Ok(_) if self.partial.ends_with(b"\n") => {
                let line = String::from_utf8(std::mem::take(&mut self.partial)).unwrap();
                Some(line.trim_end().to_owned())
            }
            Ok(_) => None,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
            Err(e) => panic!("{e}"),
        }
    }

    /// Whether the app has closed the connection, within `wait`.
    fn closed_within(&mut self, wait: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < wait {
            match self.stream.read_until(b'\n', &mut self.partial) {
                Ok(0) => return true,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return true,
                Ok(_) => {}
            }
        }
        false
    }

    /// Sends `tools/call`; its id.
    fn send(&mut self, tool: &str, arguments: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        self.write(&line.to_string());
        id
    }

    /// The next reply, running frames until it comes (within `frames`).
    fn reply_within(&mut self, rig: &mut Rig, frames: usize) -> Option<Value> {
        for _ in 0..frames {
            rig.frames(1);
            if let Some(line) = self.line_within(Duration::from_millis(20)) {
                return Some(serde_json::from_str(&line).expect("JSON"));
            }
        }
        None
    }

    fn reply(&mut self, rig: &mut Rig) -> Value {
        self.reply_within(rig, 200).expect("a reply")
    }

    /// Calls `tool` and returns its answer (the JSON a tool returns, or
    /// its error's text as a string) and whether it is an error.
    pub(super) fn call(&mut self, rig: &mut Rig, tool: &str, arguments: Value) -> (Value, bool) {
        let id = self.send(tool, arguments);
        let reply = self.reply(rig);
        assert_eq!(reply["id"], json!(id), "{reply}");
        answer(&reply)
    }
}

/// A `tools/call` reply's answer, and whether it is an error.
fn answer(reply: &Value) -> (Value, bool) {
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().expect("text content");
    let error = result["isError"].as_bool().unwrap();
    let json = if error {
        json!(text)
    } else {
        serde_json::from_str(text).expect("a tool answers JSON")
    };
    (json, error)
}

fn add_track(rig: &mut Rig) {
    rig.app_mut().session.edit("Add Track", |p| {
        p.add_track(NewTrack::Midi, TrackPosition::END)
    });
    rig.frames(1);
}

#[test]
fn an_llm_reads_the_project_through_a_socket() {
    let mut rig = Rig::new("mcp_read");
    let advert = listen(&mut rig, "read");
    let mut client = Client::connect(&advert);
    let (project, error) = client.call(&mut rig, "get_project", json!({}));
    assert!(!error, "{project}");
    let names: Vec<&str> = project["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = rig
        .app()
        .session
        .project
        .tracks
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(!names.is_empty());
    assert_eq!(names, expected);
    // The first track is selected when the app starts.
    assert_eq!(
        project["selection"]["tracks"],
        json!([rig.app().session.project.tracks[0].id.0])
    );

    // The status bar says a client is connected, and stops once it's gone.
    rig.frames(2);
    rig.harness.get_by_label("LLM");
    assert_eq!(rig.app().mcp.as_ref().unwrap().clients(), 1);
    drop(client);
    rig.wait_for("the client to go", |app| {
        app.mcp.as_ref().unwrap().clients() == 0
    });
    rig.frames(2);
    assert!(rig.harness.query_by_label("LLM").is_none());

    // The advert goes with the app.
    assert!(advert.exists());
    rig.app_mut().mcp = None;
    assert!(!advert.exists());
}

#[test]
fn a_connection_without_the_token_is_closed() {
    let mut rig = Rig::new("mcp_token");
    let advert = listen(&mut rig, "token");
    let port = rig.app().mcp.as_ref().unwrap().port();
    assert_eq!(::mcp::connection::read_advert(&advert).unwrap().port, port);

    let mut wrong = Client::raw(port);
    wrong.write(&::mcp::connection::hello(
        &::mcp::connection::new_token(),
        None,
    ));
    assert!(wrong.closed_within(Duration::from_secs(5)));

    // A request in place of the token is no better.
    let mut eager = Client::raw(port);
    eager.write(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    assert!(eager.closed_within(Duration::from_secs(5)));

    // Saying nothing is closed after two seconds.
    let mut silent = Client::raw(port);
    assert!(silent.closed_within(Duration::from_secs(5)));

    rig.frames(2);
    assert_eq!(rig.app().mcp.as_ref().unwrap().clients(), 0);
}

#[test]
fn calls_wait_while_a_knob_is_dragged() {
    let mut rig = Rig::new("mcp_drag");
    let advert = listen(&mut rig, "drag");
    let mut client = Client::connect(&advert);
    add_track(&mut rig);
    let steps = rig.app().session.history().position();
    let track = rig.app().session.project.tracks[0].id;

    // A fader drag under way.
    rig.app_mut().session.begin_live();
    rig.app_mut().session.project.tracks[0].mixer.volume_db = -12.0;
    let undo = client.send("history", json!({ "op": "undo" }));
    let rename = client.send("edit_track", json!({ "track": track.0, "name": "Renamed" }));
    let read = client.send("get_project", json!({}));
    // The read answers; the undo and the edit wait for the gesture.
    let reply = client.reply(&mut rig);
    assert_eq!(reply["id"], json!(read));
    let (project, _) = answer(&reply);
    assert_eq!(
        project["tracks"][0]["mixer"]["volume_db"].as_f64(),
        Some(-12.0)
    );
    assert!(client.reply_within(&mut rig, 10).is_none());
    assert_eq!(rig.app().session.history().position(), steps);
    assert_ne!(rig.app().session.project.tracks[0].name, "Renamed");

    // The drag ends as its own step; then the undo undoes exactly it, and
    // the edit lands as a step of its own.
    rig.app_mut().session.end_live("Volume");
    assert_eq!(rig.app().session.history().position(), steps + 1);
    let reply = client.reply(&mut rig);
    assert_eq!(reply["id"], json!(undo));
    let (undone, error) = answer(&reply);
    assert!(!error, "{undone}");
    assert_eq!(undone["undid"], json!("Volume"));
    // The edit runs in the same frame, after the undo.
    let reply = client.reply(&mut rig);
    assert_eq!(reply["id"], json!(rename));
    let (renamed, error) = answer(&reply);
    assert!(!error, "{renamed}");
    let session = &rig.app().session;
    assert_eq!(session.history().position(), steps + 1);
    assert!(
        session.undo_label().unwrap().starts_with("LLM: Edit"),
        "{:?}",
        session.undo_label()
    );
    let track = session.project.track(track).unwrap();
    assert_eq!(
        (track.name.as_str(), track.mixer.volume_db),
        ("Renamed", 0.0)
    );
}

#[test]
fn edits_wait_while_a_dialog_is_open() {
    let mut rig = Rig::new("mcp_dialog");
    let advert = listen(&mut rig, "dialog");
    let mut client = Client::connect(&advert);
    add_track(&mut rig);
    let tracks = rig.app().session.project.tracks.len();

    rig.key(Modifiers::COMMAND, Key::R);
    assert!(rig.app().export_dialog().is_some());
    let undo = client.send("history", json!({ "op": "undo" }));
    let (_, error) = client.call(&mut rig, "get_history", json!({}));
    assert!(!error, "reads answer while the dialog is open");
    assert!(client.reply_within(&mut rig, 10).is_none());
    assert_eq!(rig.app().session.project.tracks.len(), tracks);

    rig.harness.get_by_label("Cancel").click();
    let reply = client.reply(&mut rig);
    assert!(rig.app().export_dialog().is_none());
    assert_eq!(reply["id"], json!(undo));
    let (undone, error) = answer(&reply);
    assert!(!error, "{undone}");
    assert_eq!(undone["undid"], json!("Add Track"));
    assert_eq!(rig.app().session.project.tracks.len(), tracks - 1);
}

#[test]
fn the_switch_refuses_calls() {
    let mut rig = Rig::new("mcp_switch");
    let advert = listen(&mut rig, "switch");
    let mut client = Client::connect(&advert);
    add_track(&mut rig);
    let (_, error) = client.call(&mut rig, "get_project", json!({}));
    assert!(!error);

    // Switched off from the View menu, mid-session.
    rig.harness.get_by_label("View").click();
    rig.frames(2);
    rig.harness.get_by_label("Allow LLM connections").click();
    rig.frames(2);
    assert!(!rig.app().mcp_allowed);
    rig.key(Modifiers::NONE, Key::Escape);

    let before = rig.app().session.project.clone();
    let steps = rig.app().session.history().position();
    let (message, error) = client.call(&mut rig, "history", json!({ "op": "undo" }));
    assert!(error);
    assert!(
        message.as_str().unwrap().contains("turned off"),
        "{message}"
    );
    let (_, error) = client.call(&mut rig, "get_project", json!({}));
    assert!(error, "reads are refused too");
    assert_eq!(rig.app().session.history().position(), steps);
    assert!(rig.app().session.project == before);

    rig.app_mut().mcp_allowed = true;
    let (_, error) = client.call(&mut rig, "history", json!({ "op": "undo" }));
    assert!(!error);
    assert_eq!(rig.app().session.history().position(), steps - 1);
}

#[test]
fn history_and_transport_through_the_socket() {
    let mut rig = Rig::new("mcp_control");
    let advert = listen(&mut rig, "control");
    let mut client = Client::connect(&advert);
    let tracks = rig.app().session.project.tracks.len();
    add_track(&mut rig);
    add_track(&mut rig);

    let (undone, _) = client.call(&mut rig, "history", json!({ "op": "undo" }));
    assert_eq!(undone["undid"], json!("Add Track"));
    assert_eq!(rig.app().session.project.tracks.len(), tracks + 1);
    let (_, error) = client.call(&mut rig, "history", json!({ "op": "jump", "to": 0 }));
    assert!(!error);
    assert_eq!(rig.app().session.project.tracks.len(), tracks);
    let (redone, _) = client.call(&mut rig, "history", json!({ "op": "redo" }));
    assert_eq!(redone["position"], json!(1));
    assert_eq!(rig.app().session.project.tracks.len(), tracks + 1);
    // The status bar names what was done last.
    assert_eq!(
        rig.app().mcp.as_ref().unwrap().last_action(),
        Some("history")
    );

    let (moved, _) = client.call(&mut rig, "transport", json!({ "op": "seek", "at": 8 }));
    assert_eq!(moved["playhead"].as_f64(), Some(8.0));
    assert_eq!(moved["playhead_bar"], json!("3.1"));
    let (playing, _) = client.call(&mut rig, "transport", json!({ "op": "play" }));
    assert_eq!(playing["playing"], json!(true));
    assert!(rig.app().session.is_playing());
    client.call(&mut rig, "transport", json!({ "op": "stop" }));
    assert!(!rig.app().session.is_playing());
}

#[test]
fn a_cancelled_call_never_runs() {
    let mut rig = Rig::new("mcp_cancel");
    let advert = listen(&mut rig, "cancel");
    let mut client = Client::connect(&advert);
    add_track(&mut rig);
    let steps = rig.app().session.history().position();

    rig.app_mut().session.begin_live();
    let undo = client.send("history", json!({ "op": "undo" }));
    rig.frames(2);
    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": undo, "reason": "changed my mind" },
    });
    client.write(&cancel.to_string());
    // Let the connection's thread see it before the gesture ends.
    std::thread::sleep(Duration::from_millis(100));
    rig.app_mut().session.end_live("Nothing");
    assert!(client.reply_within(&mut rig, 10).is_none());
    assert_eq!(rig.app().session.history().position(), steps);
    // The connection carries on.
    let (_, error) = client.call(&mut rig, "get_history", json!({}));
    assert!(!error);
}

#[test]
fn the_app_host_loads_presets_and_refuses_what_it_cannot_do() {
    use ::mcp::host::{ClipProcess, Host, SlicerOp, Target};
    use project::{ClipId, EditError, EffectRef};

    use crate::mcp::AppHost;

    let mut rig = Rig::new("mcp_host");
    let track = rig.app().session.project.tracks[0].id;
    let reverb = rig
        .app_mut()
        .session
        .edit("Add Reverb", |p| {
            p.add_device(Some(track), EffectRef::builtin("canopy.reverb"), None)
        })
        .unwrap();
    let app = rig.app_mut();
    let selection = app.timeline.selection.clone();
    let mut host = AppHost::new(&mut app.session, &selection, &[], false);

    // Factory presets by name, in any case; a wrong name lists the right ones.
    let presets = host.presets(Target::Device(reverb));
    assert!(presets.iter().any(|p| p.name == "Small room" && p.factory));
    let steps = host.history().position();
    host.load_preset(Target::Device(reverb), "small ROOM")
        .unwrap();
    assert_eq!(host.history().position(), steps + 1);
    let size = host.project().device(reverb).unwrap().param("size");
    assert_eq!(size, Some(25.0));
    let missing = host.load_preset(Target::Device(reverb), "Cathedral of Sound");
    assert!(missing.unwrap_err().contains("Small room"));
    assert!(
        host.presets(Target::Device(project::DeviceId(9999)))
            .is_empty()
    );

    // A failed edit and a panicking one both leave the project as it was.
    let before = host.project().clone();
    let failed = host.edit(
        "Nothing",
        Box::new(|p| {
            p.tempo.tempo = project::Tempo::from_bpm(90.0);
            Err(EditError::Invalid("no".into()))
        }),
    );
    assert!(failed.is_err());
    let panicked = host.edit(
        "Panic",
        Box::new(|p| {
            p.tempo.tempo = project::Tempo::from_bpm(90.0);
            panic!("on purpose")
        }),
    );
    let Err(EditError::Invalid(message)) = panicked else {
        panic!("a panic is an error");
    };
    assert!(message.contains("on purpose"), "{message}");
    assert!(*host.project() == before);
    assert_eq!(host.history().position(), steps + 1);

    // What the track can't do says why.
    assert!(host.slicer(track).is_none());
    let slicing = host.slicer_op(track, SlicerOp::Grid { division: 4 });
    assert!(slicing.unwrap_err().contains("Slicer"));
    let chopped = host.process_clips(ClipProcess::Normalize, &[ClipId(9999)]);
    assert!(chopped.unwrap_err().contains("doesn't exist"));
    let file = host.set_instrument_file(track, std::env::temp_dir());
    assert!(file.is_err());
    assert!(
        host.render(::mcp::host::RenderRequest {
            targets: vec![],
            range: None,
            variant: Default::default(),
        })
        .is_err()
    );
}

#[test]
fn an_audio_file_becomes_a_clip() {
    use project::ClipRef;

    let mut rig = Rig::new("mcp_import");
    let advert = listen(&mut rig, "import");
    let mut client = Client::connect(&advert);
    let dir = super::temp_dir("mcp_import");
    let wav = dir.join("tone.wav");
    super::write_sine_wav(&wav, 1, 440.0);
    let tracks = rig.app().session.project.tracks.len();
    let steps = rig.app().session.history().position();

    // The answer waits for the file to load, then names the clip.
    let id = client.send(
        "add_clip",
        json!({ "start": 4, "kind": "audio", "file": wav }),
    );
    let reply = client.reply_within(&mut rig, 1000).expect("a reply");
    assert_eq!(reply["id"], json!(id));
    let (added, error) = answer(&reply);
    assert!(!error, "{added}");
    let clip = project::ClipId(added["id"].as_u64().unwrap() as u32);
    let session = &rig.app().session;
    let Some((track, ClipRef::Audio(audio))) = session.project.clip(clip) else {
        panic!("an audio clip: {added}");
    };
    assert_eq!(audio.start, midi::Tick::new(4 * midi::PPQ));
    assert_eq!(added["track"], json!(track.0));
    assert_eq!(session.project.tracks.len(), tracks + 1, "on a new track");
    assert_eq!(session.history().position(), steps + 1, "one undo step");
    assert_eq!(session.undo_label(), Some("LLM: Import Audio"));

    // A file that isn't there is an error at once.
    let (message, error) = client.call(
        &mut rig,
        "add_clip",
        json!({ "start": 0, "kind": "audio", "file": dir.join("missing.wav") }),
    );
    assert!(error);
    assert!(
        message.as_str().unwrap().contains("isn't a file"),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_factory_preset_loads_by_name() {
    use project::EffectRef;

    let mut rig = Rig::new("mcp_preset");
    let advert = listen(&mut rig, "preset");
    let mut client = Client::connect(&advert);
    let track = rig.app().session.project.tracks[0].id;
    let reverb = rig
        .app_mut()
        .session
        .edit("Add Reverb", |p| {
            p.add_device(Some(track), EffectRef::builtin("canopy.reverb"), None)
        })
        .unwrap();
    let steps = rig.app().session.history().position();

    let (presets, _) = client.call(&mut rig, "list_presets", json!({ "device": reverb.0 }));
    assert!(
        presets["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == json!("Small room")),
        "{presets}"
    );
    let (loaded, error) = client.call(
        &mut rig,
        "load_preset",
        json!({ "device": reverb.0, "preset": "small room" }),
    );
    assert!(!error, "{loaded}");
    let session = &rig.app().session;
    assert_eq!(
        session.project.device(reverb).unwrap().param("size"),
        Some(25.0)
    );
    assert_eq!(session.history().position(), steps + 1);
    assert!(session.undo_label().unwrap().starts_with("LLM: "));

    let (message, error) = client.call(
        &mut rig,
        "load_preset",
        json!({ "device": reverb.0, "preset": "Cathedral of Sound" }),
    );
    assert!(error);
    assert!(
        message.as_str().unwrap().contains("Small room"),
        "{message}"
    );
}

#[test]
fn the_slicer_slices_at_transients_over_the_socket() {
    let mut rig = Rig::new("mcp_slicer");
    let advert = listen(&mut rig, "slicer");
    let mut client = Client::connect(&advert);
    let track = rig.app().session.project.tracks[0].id;
    let dir = super::temp_dir("mcp_slicer");
    let wav = dir.join("break.wav");
    super::slicer::write_break(&wav);

    // The answer comes once the loop has loaded and been sliced.
    let id = client.send(
        "set_instrument",
        json!({ "track": track.0, "instrument": "canopy.slicer", "file": wav }),
    );
    let reply = client.reply_within(&mut rig, 1000).expect("a reply");
    assert_eq!(reply["id"], json!(id));
    let (set, error) = answer(&reply);
    assert!(!error, "{set}");
    let t = rig.app().session.project.track(track).unwrap();
    assert_eq!(t.instrument_file(), Some(wav.as_path()));
    assert!(set["slices"].as_u64().unwrap() > 1, "{set}");

    // Cut evenly, then back at the hits.
    let (grid, error) = client.call(
        &mut rig,
        "slicer",
        json!({ "track": track.0, "op": "grid", "division": 4 }),
    );
    assert!(!error, "{grid}");
    assert_eq!(grid["slices"].as_array().unwrap().len(), 4, "{grid}");
    let (cut, error) = client.call(
        &mut rig,
        "slicer",
        json!({ "track": track.0, "op": "transients", "sensitivity": 0.5 }),
    );
    assert!(!error, "{cut}");
    let slices = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .slices()
        .to_vec();
    assert_eq!(slices.len(), 8, "the break's eight hits: {cut}");
    assert_eq!(cut["slices"].as_array().unwrap().len(), 8);
    let fifth = cut["slices"][4]["start_seconds"].as_f64().unwrap();
    assert!(
        (fifth - 1.0).abs() < 0.01,
        "the fifth hit is a second in: {fifth}"
    );
    let undo = rig.app().session.undo_label().unwrap().to_owned();
    assert!(undo.starts_with("LLM: "), "{undo}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sine at `hz` on the rig's first audio track, from the song's start.
fn sine_track(rig: &mut Rig, dir: &Path, hz: f32) -> project::TrackId {
    let wav = dir.join("sine.wav");
    super::write_sine_wav(&wav, 2, hz);
    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![wav],
        Some(track),
        midi::Tick::ZERO,
        TrackPosition::END,
    );
    rig.wait_for("the import", |app| !app.session.project.sources.is_empty());
    rig.wait_for("the audio to load", |app| {
        app.session.offline_audio().is_some()
    });
    track
}

/// The rig's server's renders.
fn renders(rig: &Rig) -> &crate::mcp::renders::Renders {
    rig.app().mcp.as_ref().unwrap().renders()
}

#[test]
fn analyze_a_track_over_the_socket() {
    let mut rig = Rig::new("mcp_analyze");
    let advert = listen(&mut rig, "analyze");
    let mut client = Client::connect(&advert);
    let dir = super::temp_dir("mcp_analyze");
    let track = sine_track(&mut rig, &dir, 1000.0);

    let (out, error) = client.call(
        &mut rig,
        "analyze",
        json!({ "target": { "track": track.0 }, "measures": ["spectrum", "loudness"],
                "range": { "from": 0, "to": 2 } }),
    );
    assert!(!error, "{out}");
    let peak = out["measures"]["spectrum"]["peaks"][0]["hz"]
        .as_f64()
        .unwrap();
    assert!((peak - 1000.0).abs() < 20.0, "the sine's peak: {out}");
    assert_eq!(out["range"]["seconds"].as_f64(), Some(1.0));
    assert!(out.get("stale").is_none(), "{out}");

    // A plot answers with an image after its JSON.
    let id = client.send(
        "plot",
        json!({ "kind": "spectrogram", "targets": [{ "track": track.0 }],
                "range": { "from": 0, "to": 2 }, "width": 400, "height": 240 }),
    );
    let reply = client.reply(&mut rig);
    assert_eq!(reply["id"], json!(id));
    let content = &reply["result"]["content"];
    assert_eq!(content[1]["type"], json!("image"), "{reply}");
    assert_eq!(content[1]["mimeType"], json!("image/png"));
    let (axes, error) = answer(&reply);
    assert!(!error, "{axes}");
    assert_eq!(axes["axes"]["width"], json!(400));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_same_render_is_reused() {
    let mut rig = Rig::new("mcp_reuse");
    let advert = listen(&mut rig, "reuse");
    let mut client = Client::connect(&advert);
    let dir = super::temp_dir("mcp_reuse");
    let track = sine_track(&mut rig, &dir, 440.0);

    let ask = |measure: &str| {
        json!({ "target": { "track": track.0 }, "measures": [measure],
                "range": { "from": 0, "to": 2 } })
    };
    let (first, error) = client.call(&mut rig, "analyze", ask("spectrum"));
    assert!(!error, "{first}");
    assert_eq!(renders(&rig).started(), 1);
    let (second, error) = client.call(&mut rig, "analyze", ask("loudness"));
    assert!(!error, "{second}");
    assert_eq!(
        renders(&rig).started(),
        1,
        "the second answer is the first render's"
    );
    assert_eq!(first["range"], second["range"]);

    // Another variant, or the project changed, renders again.
    let (_, error) = client.call(
        &mut rig,
        "analyze",
        json!({ "target": { "track": track.0 }, "measures": ["loudness"],
                "range": { "from": 0, "to": 2 }, "variant": "dry" }),
    );
    assert!(!error);
    assert_eq!(renders(&rig).started(), 2);
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(track, "Sine"));
    let (_, error) = client.call(&mut rig, "analyze", ask("spectrum"));
    assert!(!error);
    assert_eq!(renders(&rig).started(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_edit_during_a_render_marks_it_stale() {
    let mut rig = Rig::new("mcp_stale");
    let advert = listen(&mut rig, "stale");
    let mut client = Client::connect(&advert);
    let dir = super::temp_dir("mcp_stale");
    let track = sine_track(&mut rig, &dir, 440.0);
    let ask = json!({ "target": { "track": track.0 }, "measures": ["loudness"],
                      "range": { "from": 0, "to": 2 } });

    let id = client.send("analyze", ask.clone());
    rig.wait_for("the render to start", |app| {
        app.mcp.as_ref().unwrap().renders().started() == 1
    });
    // The user edits before the answer is collected.
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(track, "Changed"));
    let reply = client.reply(&mut rig);
    assert_eq!(reply["id"], json!(id));
    let (out, error) = answer(&reply);
    assert!(!error, "{out}");
    assert_eq!(out["stale"], json!(true), "{out}");
    assert!(out["note"].as_str().unwrap().contains("changed"));

    // It wasn't kept: the same question renders the project as it is now.
    let (out, error) = client.call(&mut rig, "analyze", ask);
    assert!(!error, "{out}");
    assert_eq!(renders(&rig).started(), 2);
    assert!(out.get("stale").is_none(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_render_stops() {
    use midi::{Pitch, Tick, Velocity};
    use project::Note;

    let mut rig = Rig::new("mcp_render_cancel");
    let advert = listen(&mut rig, "render_cancel");
    let mut client = Client::connect(&advert);
    // A chord held for over nine minutes: far longer to render than the
    // test waits.
    let track = rig.app().session.project.track_ids()[0];
    let beats = 1100;
    rig.app_mut().session.edit("Add", |p| {
        let clip = p.add_midi_clip(track, Tick::ZERO, Tick::new(beats * midi::PPQ))?;
        let notes: Vec<Note> = (0..12)
            .map(|step| Note {
                id: p.new_note_id(),
                start: Tick::ZERO,
                len: Tick::new(beats * midi::PPQ),
                pitch: Pitch::new(48 + 3 * step).unwrap(),
                velocity: Velocity::MAX,
                release_velocity: Velocity::CENTER,
                muted: false,
                bend: Vec::new(),
            })
            .collect();
        p.set_notes(clip, notes)
    });

    let call = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "analyze",
            "arguments": { "target": { "track": track.0 }, "measures": ["loudness"] },
            "_meta": { "progressToken": "render-7" },
        },
    });
    client.write(&call.to_string());
    // Progress comes every half second while it renders.
    let progress = client.reply_within(&mut rig, 400).expect("progress");
    assert_eq!(
        progress["method"],
        json!("notifications/progress"),
        "{progress}"
    );
    assert_eq!(progress["params"]["progressToken"], json!("render-7"));
    let done = progress["params"]["progress"].as_f64().unwrap();
    assert!(done > 0.0 && done < 1.0, "{progress}");
    assert_eq!(renders(&rig).busy(), 1);

    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 7, "reason": "too slow" },
    });
    client.write(&cancel.to_string());
    rig.wait_for("the render to stop", |app| {
        app.mcp.as_ref().unwrap().renders().busy() == 0
    });
    assert_eq!(renders(&rig).stopped(), 1, "stopped, not finished");
    // No answer comes.
    rig.frames(3);
    while let Some(line) = client.line_within(Duration::from_millis(200)) {
        let message: Value = serde_json::from_str(&line).unwrap();
        assert!(
            message.get("id").is_none(),
            "a cancelled call answered: {line}"
        );
    }
    let (_, error) = client.call(&mut rig, "get_history", json!({}));
    assert!(!error, "the connection carries on");
}
