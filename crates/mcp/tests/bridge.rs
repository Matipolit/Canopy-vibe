//! The bridge, `canopy-mcp`, run as MCP clients run it: a process spoken
//! to on stdin and stdout. A listener in the test plays the app, written
//! to the advert file the bridge is pointed at with `CANOPY_MCP_FILE`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use mcp::connection::{self, Advert};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(10);

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<Value>,
    next_id: u64,
}

impl Bridge {
    fn start(advert: &Path) -> Bridge {
        let mut child = Command::new(env!("CARGO_BIN_EXE_canopy-mcp"))
            .env("CANOPY_MCP_FILE", advert)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the bridge starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (send, stdout_lines) = mpsc::channel();
        thread::spawn(move || {
            for line in stdout.lines() {
                let Ok(line) = line else { return };
                let value = serde_json::from_str(&line).expect("the bridge writes JSON");
                if send.send(value).is_err() {
                    return;
                }
            }
        });
        Bridge {
            child,
            stdin,
            stdout: stdout_lines,
            next_id: 1,
        }
    }

    /// Sends a request; the reply.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let reply = self.stdout.recv_timeout(WAIT).expect("the bridge replies");
        assert_eq!(reply["id"], json!(id), "{reply}");
        reply
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of a tool result, and whether it is an error.
fn text(reply: &Value) -> (String, bool) {
    let result = &reply["result"];
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    /// Answers every call with its tool's name.
    Answer,
    /// Answers one call, then closes and stops listening, as a canopy
    /// that quit.
    AnswerOnceThenQuit,
    /// Closes the connection on the first call, without answering.
    DropOnCall,
    /// Expects another token than the advert's.
    WrongToken,
}

/// The tokens and client names connections began with.
type Hellos = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A stand-in for the app: listens, writes the advert, checks tokens and
/// keeps every message it is sent.
struct FakeApp {
    port: u16,
    seen: Arc<Mutex<Vec<Value>>>,
    hellos: Hellos,
    stop: Arc<AtomicBool>,
}

impl FakeApp {
    fn start(advert: &Path, behaviour: Behaviour) -> FakeApp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = connection::new_token();
        let expected = match behaviour {
            Behaviour::WrongToken => connection::new_token(),
            _ => token.clone(),
        };
        connection::write_advert(
            advert,
            &Advert {
                port,
                token,
                pid: std::process::id(),
            },
        )
        .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let hellos = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen_by, hellos_by, stopped) = (seen.clone(), hellos.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let quit = serve(stream, &expected, behaviour, &seen_by, &hellos_by);
                if quit {
                    return;
                }
            }
        });
        FakeApp {
            port,
            seen,
            hellos,
            stop,
        }
    }

    fn calls(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m["method"] == json!("tools/call"))
            .cloned()
            .collect()
    }
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// One connection; true if the app quits after it.
fn serve(
    stream: TcpStream,
    expected: &str,
    behaviour: Behaviour,
    seen: &Mutex<Vec<Value>>,
    hellos: &Mutex<Vec<(String, Option<String>)>>,
) -> bool {
    let mut writer = stream.try_clone().unwrap();
    let mut lines = BufReader::new(stream).lines();
    let Some(Ok(hello)) = lines.next() else {
        return false;
    };
    let Some((token, client)) = connection::parse_hello(&hello) else {
        return false;
    };
    hellos.lock().unwrap().push((token.clone(), client));
    if !connection::tokens_match(&token, expected) {
        return false;
    }
    writeln!(writer, "{}", connection::WELCOME).unwrap();
    for line in lines {
        let Ok(line) = line else { return false };
        let message: Value = serde_json::from_str(&line).unwrap();
        seen.lock().unwrap().push(message.clone());
        if message["method"] != json!("tools/call") {
            continue;
        }
        if behaviour == Behaviour::DropOnCall {
            return false;
        }
        let result = mcp::protocol::tool_result(
            &[mcp::protocol::Content::Text(
                message["params"]["name"].as_str().unwrap().to_owned(),
            )],
            false,
        );
        writeln!(writer, "{}", mcp::protocol::result(&message["id"], result)).unwrap();
        if behaviour == Behaviour::AnswerOnceThenQuit {
            let _ = writer.shutdown(std::net::Shutdown::Both);
            return true;
        }
    }
    false
}

fn advert_path(test: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("canopy-bridge-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir.join("mcp.json"))
}

/// A test's own folder for its token file, gone when the test ends.
struct Scratch(PathBuf);

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

#[test]
fn it_initializes_and_lists_tools_without_canopy() {
    let advert = advert_path("init");
    let mut bridge = Bridge::start(&advert);
    let init = bridge.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test client", "version": "1" },
        }),
    );
    assert_eq!(init["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("canopy"));
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("Ids")
    );
    let listed = bridge.request("tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"get_project"), "{names:?}");
    assert_eq!(names.len(), mcp::tools::all().len());
    assert_eq!(bridge.request("ping", json!({}))["result"], json!({}));
    let unknown = bridge.request("resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));
}

#[test]
fn a_call_without_canopy_is_a_tool_error() {
    let advert = advert_path("absent");
    let mut bridge = Bridge::start(&advert);
    let (message, error) = text(&bridge.call("get_project", json!({})));
    assert!(error);
    assert!(message.contains("canopy isn't running"), "{message}");
}

#[test]
fn calls_reach_canopy_once_it_appears_and_carry_the_token() {
    let advert = advert_path("appears");
    let mut bridge = Bridge::start(&advert);
    bridge.request(
        "initialize",
        json!({ "clientInfo": { "name": "Claude Code", "version": "2" } }),
    );
    let (_, error) = text(&bridge.call("get_project", json!({})));
    assert!(error, "nothing is listening yet");

    let app = FakeApp::start(&advert, Behaviour::Answer);
    let (answer, error) = text(&bridge.call("get_project", json!({})));
    assert!(!error);
    assert_eq!(answer, "get_project");
    let token = connection::read_advert(&advert).unwrap().token;
    assert_eq!(
        app.hellos.lock().unwrap().as_slice(),
        [(token, Some("Claude Code".to_owned()))]
    );
    // The call arrives as the client sent it; later ones use the same
    // connection.
    let (answer, _) = text(&bridge.call("get_track", json!({ "track": 3 })));
    assert_eq!(answer, "get_track");
    let calls = app.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["params"]["arguments"], json!({ "track": 3 }));
    assert_eq!(app.hellos.lock().unwrap().len(), 1);
}

#[test]
fn a_stale_advert_reads_as_not_running() {
    let advert = advert_path("stale");
    // A port nothing listens on any more.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    connection::write_advert(
        &advert,
        &Advert {
            port,
            token: connection::new_token(),
            pid: 1,
        },
    )
    .unwrap();
    let mut bridge = Bridge::start(&advert);
    let (message, error) = text(&bridge.call("get_project", json!({})));
    assert!(error);
    assert!(message.contains("canopy isn't running"), "{message}");
}

#[test]
fn a_restarted_canopy_is_found_again() {
    let advert = advert_path("restart");
    let mut bridge = Bridge::start(&advert);
    let first = FakeApp::start(&advert, Behaviour::AnswerOnceThenQuit);
    let (answer, error) = text(&bridge.call("get_project", json!({})));
    assert!(!error);
    assert_eq!(answer, "get_project");
    drop(first);

    let second = FakeApp::start(&advert, Behaviour::Answer);
    let (answer, error) = text(&bridge.call("get_history", json!({})));
    assert!(!error, "{answer}");
    assert_eq!(answer, "get_history");
    assert_eq!(second.calls().len(), 1);
}

#[test]
fn a_wrong_token_is_refused() {
    let advert = advert_path("token");
    let mut bridge = Bridge::start(&advert);
    let app = FakeApp::start(&advert, Behaviour::WrongToken);
    let (message, error) = text(&bridge.call("get_project", json!({})));
    assert!(error);
    assert!(message.contains("refused"), "{message}");
    assert!(app.calls().is_empty());
}

#[test]
fn a_dropped_edit_is_not_retried() {
    let advert = advert_path("dropped");
    let mut bridge = Bridge::start(&advert);
    let app = FakeApp::start(&advert, Behaviour::DropOnCall);
    let (message, error) = text(&bridge.call("edit_track", json!({ "track": 1, "mute": true })));
    assert!(error);
    assert!(message.contains("may or may not"), "{message}");
    // A read in its place is asked again, once, on a new connection.
    let (message, error) = text(&bridge.call("get_project", json!({})));
    assert!(error);
    assert!(message.contains("try again"), "{message}");
    let names: Vec<Value> = app
        .calls()
        .iter()
        .map(|c| c["params"]["name"].clone())
        .collect();
    assert_eq!(
        names,
        [
            json!("edit_track"),
            json!("get_project"),
            json!("get_project")
        ]
    );
}
