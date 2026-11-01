//! Listens on loopback for the bridge: checks each connection's token,
//! reads its messages, and sends the tool calls to the UI thread.
//!
//! Each connection has a thread that reads and a thread that writes, so a
//! reply can go out from wherever the call is answered (the pump, on the
//! UI thread) without waiting on the reader. `initialize`, `ping` and
//! `tools/list` are answered by the reader itself; `tools/call` becomes a
//! [`Call`], and `notifications/cancelled` sets its flag.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use ::mcp::connection::{self, Advert, Line};
use ::mcp::protocol::{self, Incoming};
use ::mcp::tools;
use serde_json::{Value, json};

use super::pump::Waiting;
use super::renders::Renders;

/// The longest first line accepted: a token and a client's name.
const MAX_HELLO: usize = 4096;

/// A tool call from a client, waiting for the UI thread.
pub struct Call {
    /// The request's id, echoed in the reply.
    pub id: Value,
    pub name: String,
    pub args: Value,
    /// The client's `_meta.progressToken`, for progress notifications.
    pub progress: Option<Value>,
    /// The client's name, as the bridge gave it.
    pub client: Option<Arc<str>>,
    /// Where the reply line goes (the connection's writer).
    pub reply: Sender<String>,
    /// Set when the client cancels the call or goes away.
    pub cancelled: Arc<AtomicBool>,
}

impl Call {
    /// Sends the result of `tools/call`. A client that has gone away is
    /// no one's concern.
    pub fn answer(&self, result: Value) {
        let _ = self.reply.send(protocol::result(&self.id, result));
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// The app's end of the connection: listening, and the calls on their way
/// through the pump.
pub struct Server {
    port: u16,
    /// Where the advert was written.
    advert: PathBuf,
    calls: Receiver<Call>,
    clients: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    /// Live connections by number, to close when the server goes.
    streams: Arc<Mutex<HashMap<u64, TcpStream>>>,
    /// Calls received and not yet run, in order (see `pump`).
    pub(super) queue: VecDeque<Call>,
    /// Calls whose answers come on a later frame.
    pub(super) waiting: Vec<Waiting>,
    /// What the last call that changed something did, for the status bar.
    pub(super) last_action: Option<String>,
    /// The name of the client that made it.
    pub(super) last_client: Option<Arc<str>>,
    /// Renders for the analysis tools.
    pub(super) renders: Renders,
}

impl Server {
    /// Listens on a random port of `127.0.0.1` and advertises it at
    /// `connection::file_path()`, replacing any older canopy's advert.
    /// `wake` asks the UI for a frame when a call arrives.
    pub fn start(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Server> {
        let path = connection::file_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no folder for the token file")
        })?;
        Server::start_at(path, wake)
    }

    /// As [`Server::start`], with the advert at `advert` (tests).
    pub fn start_at(advert: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Server> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let port = listener.local_addr()?.port();
        let token: Arc<str> = connection::new_token().into();
        connection::write_advert(
            &advert,
            &Advert {
                port,
                token: token.to_string(),
                pid: std::process::id(),
            },
        )?;
        let (send, calls) = mpsc::channel();
        let clients = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(Mutex::new(HashMap::new()));
        let renders = Renders::new(wake.clone());
        let shared = Shared {
            token,
            calls: send,
            clients: clients.clone(),
            wake,
            streams: streams.clone(),
        };
        let stopped = stop.clone();
        thread::Builder::new()
            .name("mcp listener".into())
            .spawn(move || accept(listener, shared, stopped))?;
        Ok(Server {
            port,
            advert,
            calls,
            clients,
            stop,
            streams,
            queue: VecDeque::new(),
            waiting: Vec::new(),
            last_action: None,
            last_client: None,
            renders,
        })
    }

    /// The port it listens on; clients find it in the advert.
    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// How many clients are connected (with the right token).
    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::SeqCst)
    }

    /// What the last call that changed something did.
    pub fn last_action(&self) -> Option<&str> {
        self.last_action.as_deref()
    }

    /// The name of the client that last changed something, if it gave one.
    pub fn last_client(&self) -> Option<&str> {
        self.last_client.as_deref()
    }

    /// The renders for the analysis tools, running and kept.
    #[cfg(test)]
    pub fn renders(&self) -> &Renders {
        &self.renders
    }

    /// Moves the calls that have arrived onto the queue.
    pub(super) fn receive(&mut self) {
        self.queue.extend(self.calls.try_iter());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accepting thread, which then sees `stop`.
        let _ = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.port)));
        let streams: Vec<TcpStream> = self.streams.lock().map_or_else(
            |_| Vec::new(),
            |s| s.values().filter_map(|s| s.try_clone().ok()).collect(),
        );
        for stream in streams {
            let _ = stream.shutdown(Shutdown::Both);
        }
        // Only this canopy's advert: a newer one may have replaced it.
        if connection::read_advert(&self.advert)
            .is_some_and(|a| a.port == self.port && a.pid == std::process::id())
        {
            let _ = std::fs::remove_file(&self.advert);
        }
    }
}

/// What every connection's thread needs.
#[derive(Clone)]
struct Shared {
    token: Arc<str>,
    calls: Sender<Call>,
    clients: Arc<AtomicUsize>,
    wake: Arc<dyn Fn() + Send + Sync>,
    streams: Arc<Mutex<HashMap<u64, TcpStream>>>,
}

fn accept(listener: TcpListener, shared: Shared, stop: Arc<AtomicBool>) {
    for (number, stream) in (0u64..).zip(listener.incoming()) {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let shared = shared.clone();
        let _ = thread::Builder::new()
            .name("mcp connection".into())
            .spawn(move || serve(stream, number, shared));
    }
}

/// One connection, from its token to its end.
fn serve(stream: TcpStream, number: u64, shared: Shared) {
    let (Ok(writer), Ok(closer)) = (stream.try_clone(), stream.try_clone()) else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(client) = handshake(&mut reader, &shared.token) else {
        let _ = closer.shutdown(Shutdown::Both);
        return;
    };
    let (out, lines) = mpsc::channel::<String>();
    let _ = thread::Builder::new()
        .name("mcp writer".into())
        .spawn(move || write_lines(writer, lines));
    let _ = out.send(connection::WELCOME.to_owned());
    if let Ok(mut streams) = shared.streams.lock() {
        streams.insert(number, closer);
    }
    shared.clients.fetch_add(1, Ordering::SeqCst);
    (shared.wake)();

    let mut connection = Connection {
        client: client.map(Into::into),
        out,
        flags: HashMap::new(),
        shared: &shared,
    };
    loop {
        match connection::read_line(&mut reader, protocol::MAX_LINE) {
            Ok(Line::Text(line)) => connection.receive(&line),
            Ok(Line::TooLong) => connection.send(protocol::error(
                &Value::Null,
                protocol::INVALID_REQUEST,
                "a message may be at most 16 MB",
            )),
            Ok(Line::End) | Err(_) => break,
        }
    }

    // Nobody is left to answer: calls still waiting are dropped.
    for flag in connection.flags.values() {
        flag.store(true, Ordering::SeqCst);
    }
    if let Ok(mut streams) = shared.streams.lock()
        && let Some(stream) = streams.remove(&number)
    {
        let _ = stream.shutdown(Shutdown::Both);
    }
    shared.clients.fetch_sub(1, Ordering::SeqCst);
    (shared.wake)();
}

/// Reads the first line within `TOKEN_WAIT`; the client's name if it
/// carried the token.
fn handshake(reader: &mut BufReader<TcpStream>, token: &str) -> Option<Option<String>> {
    let stream = reader.get_ref();
    stream.set_read_timeout(Some(connection::TOKEN_WAIT)).ok()?;
    let Ok(Line::Text(line)) = connection::read_line(reader, MAX_HELLO) else {
        return None;
    };
    let (given, client) = connection::parse_hello(&line)?;
    if !connection::tokens_match(&given, token) {
        return None;
    }
    reader.get_ref().set_read_timeout(None).ok()?;
    Some(client)
}

fn write_lines(mut stream: TcpStream, lines: Receiver<String>) {
    for line in lines {
        if writeln!(stream, "{line}")
            .and_then(|()| stream.flush())
            .is_err()
        {
            return;
        }
    }
}

/// A connection as its reader keeps it.
struct Connection<'a> {
    client: Option<Arc<str>>,
    out: Sender<String>,
    /// Cancellation flags of the calls sent on, by their ids' JSON text.
    flags: HashMap<String, Arc<AtomicBool>>,
    shared: &'a Shared,
}

impl Connection<'_> {
    fn send(&self, line: String) {
        let _ = self.out.send(line);
    }

    fn receive(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        match protocol::parse(line) {
            Err(reply) => self.send(reply),
            Ok(Incoming::Request { id, method, params }) => self.request(id, &method, params),
            Ok(Incoming::Notification { method, params }) => {
                if method == "notifications/cancelled"
                    && let Some(id) = params.get("requestId")
                    && let Some(flag) = self.flags.get(&id.to_string())
                {
                    flag.store(true, Ordering::SeqCst);
                    (self.shared.wake)();
                }
            }
        }
    }

    fn request(&mut self, id: Value, method: &str, mut params: Value) {
        match method {
            // The bridge answers these itself; a client connecting
            // directly gets the same answers here.
            "initialize" => {
                if self.client.is_none() {
                    self.client = params
                        .pointer("/clientInfo/name")
                        .and_then(Value::as_str)
                        .map(Into::into);
                }
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let result = protocol::initialize(requested, tools::INSTRUCTIONS);
                self.send(protocol::result(&id, result));
            }
            "ping" => self.send(protocol::result(&id, json!({}))),
            "tools/list" => self.send(protocol::result(&id, tools::list())),
            "tools/call" => {
                let Some(name) = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return self.send(protocol::error(
                        &id,
                        protocol::INVALID_PARAMS,
                        "tools/call needs the tool's `name`",
                    ));
                };
                let args = params
                    .get_mut("arguments")
                    .map(Value::take)
                    .unwrap_or_else(|| json!({}));
                let progress = params.pointer("/_meta/progressToken").cloned();
                // Flags only the connection still holds belong to calls
                // that have been answered.
                self.flags.retain(|_, flag| Arc::strong_count(flag) > 1);
                let cancelled = Arc::new(AtomicBool::new(false));
                self.flags.insert(id.to_string(), cancelled.clone());
                let call = Call {
                    id,
                    name,
                    args,
                    progress,
                    client: self.client.clone(),
                    reply: self.out.clone(),
                    cancelled,
                };
                if self.shared.calls.send(call).is_ok() {
                    (self.shared.wake)();
                }
            }
            _ => self.send(protocol::error(
                &id,
                protocol::METHOD_NOT_FOUND,
                &format!("canopy doesn't offer {method:?}"),
            )),
        }
    }
}
