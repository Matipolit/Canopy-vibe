//! `canopy-mcp`: what an MCP client launches to talk to canopy.
//!
//! It speaks MCP on stdin and stdout and answers `initialize`, `ping` and
//! `tools/list` itself from the shared tool table, so it starts at once and
//! keeps running whether or not canopy is open. Each `tools/call` goes to
//! the running app over loopback TCP (see `mcp::connection`), connecting
//! the first time and again whenever canopy has restarted; without an app
//! the call fails with a message saying to open it.
//!
//! When the connection drops before a call is answered, a read is asked
//! again of a fresh connection, once; anything else fails, saying it may or
//! may not have happened, since asking twice could make an edit twice.
//!
//! Every message from stdin, from each connection's reader and the end of
//! each arrives on one channel, so a single loop owns the state and is the
//! only writer of stdout.

use std::collections::HashMap;
use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mcp::connection::{self, Line};
use mcp::protocol::{self, Content, Incoming};
use mcp::tools::{self, Kind};
use serde_json::{Value, json};

/// How long connecting to the app, and its answer to the token, may take.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

const NOT_RUNNING: &str = "canopy isn't running; open it and try again";

enum Event {
    /// A line from the client, or `None` when stdin closed.
    Client(Option<Line>),
    /// A line from the app.
    App(String),
    /// Connection `n` closed.
    Closed(u64),
}

/// A call sent to the app and not yet answered.
struct Waiting {
    /// The request as the client sent it, to send again.
    line: String,
    id: Value,
    /// A read, which may safely be asked twice.
    read: bool,
    retried: bool,
    /// The connection it went out on.
    connection: u64,
}

struct Connection {
    number: u64,
    stream: TcpStream,
}

struct Bridge {
    events: mpsc::Sender<Event>,
    out: io::Stdout,
    connection: Option<Connection>,
    connections: u64,
    /// The client's name, from `initialize`, for the app's history.
    client: Option<String>,
    /// By the request id's JSON text.
    waiting: HashMap<String, Waiting>,
}

fn main() {
    let (events, inbox) = mpsc::channel();
    let from_client = events.clone();
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let line = connection::read_line(&mut stdin, protocol::MAX_LINE).ok();
            let end = matches!(line, None | Some(Line::End));
            if from_client
                .send(Event::Client(line.filter(|l| *l != Line::End)))
                .is_err()
                || end
            {
                return;
            }
        }
    });
    let mut bridge = Bridge {
        events,
        out: io::stdout(),
        connection: None,
        connections: 0,
        client: None,
        waiting: HashMap::new(),
    };
    for event in inbox {
        match event {
            Event::Client(None) => break,
            Event::Client(Some(Line::Text(line))) => bridge.client_line(line),
            Event::Client(Some(_)) => bridge.send(&protocol::error(
                &Value::Null,
                protocol::INVALID_REQUEST,
                "a message may be at most 16 MB",
            )),
            Event::App(line) => bridge.app_line(line),
            Event::Closed(number) => bridge.closed(number),
        }
    }
}

impl Bridge {
    /// Writes a line to the client.
    fn send(&mut self, line: &str) {
        let mut out = self.out.lock();
        // A client that has gone away will close stdin next.
        let _ = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    fn client_line(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let message = match protocol::parse(&line) {
            Ok(message) => message,
            Err(reply) => return self.send(&reply),
        };
        match message {
            Incoming::Request { id, method, params } => match method.as_str() {
                "initialize" => {
                    self.client = params
                        .pointer("/clientInfo/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let requested = params.get("protocolVersion").and_then(Value::as_str);
                    let result = protocol::initialize(requested, tools::INSTRUCTIONS);
                    self.send(&protocol::result(&id, result));
                }
                "ping" => self.send(&protocol::result(&id, json!({}))),
                "tools/list" => self.send(&protocol::result(&id, tools::list())),
                "tools/call" => self.call(id, &params, line),
                _ => self.send(&protocol::error(
                    &id,
                    protocol::METHOD_NOT_FOUND,
                    &format!("canopy doesn't offer {method:?}"),
                )),
            },
            // Cancellations and the like matter only to an app that is
            // working on something.
            Incoming::Notification { .. } => {
                if let Some(connection) = &mut self.connection {
                    let _ = writeln!(connection.stream, "{line}");
                }
            }
        }
    }

    fn call(&mut self, id: Value, params: &Value, line: String) {
        let read = params
            .get("name")
            .and_then(Value::as_str)
            .and_then(tools::find)
            .is_some_and(|tool| matches!(tool.kind, Kind::Read(_)));
        let mut waiting = Waiting {
            line,
            id,
            read,
            retried: false,
            connection: 0,
        };
        // A connection that broke unnoticed fails the write: nothing was
        // received, so one more try on a new connection is safe for any call.
        for _ in 0..2 {
            match self.forward(&waiting.line) {
                Ok(number) => {
                    waiting.connection = number;
                    self.waiting.insert(waiting.id.to_string(), waiting);
                    return;
                }
                Err(Unreachable::Refused(message)) => {
                    return self.fail(&waiting.id, &message);
                }
                Err(Unreachable::Broken) => {}
            }
        }
        self.fail(&waiting.id, NOT_RUNNING);
    }

    /// Sends a line to the app, connecting first if need be; the
    /// connection's number.
    fn forward(&mut self, line: &str) -> Result<u64, Unreachable> {
        if self.connection.is_none() {
            self.connection = Some(self.connect()?);
        }
        let connection = self.connection.as_mut().expect("connected");
        match writeln!(connection.stream, "{line}") {
            Ok(()) => Ok(connection.number),
            Err(_) => {
                self.drop_connection();
                Err(Unreachable::Broken)
            }
        }
    }

    fn drop_connection(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = connection.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Finds the app from its advert, connects and gives the token; a
    /// thread then passes on what the app sends.
    fn connect(&mut self) -> Result<Connection, Unreachable> {
        let advert = connection::file_path()
            .and_then(|path| connection::read_advert(&path))
            .ok_or_else(|| Unreachable::Refused(NOT_RUNNING.to_owned()))?;
        let address = SocketAddr::from(([127, 0, 0, 1], advert.port));
        let mut stream = TcpStream::connect_timeout(&address, CONNECT_WAIT)
            .map_err(|_| Unreachable::Refused(NOT_RUNNING.to_owned()))?;
        let refused = || {
            Unreachable::Refused(
                "canopy refused the connection: its token didn't match (is another canopy \
                 starting?); try again"
                    .to_owned(),
            )
        };
        let _ = stream.set_nodelay(true);
        writeln!(
            stream,
            "{}",
            connection::hello(&advert.token, self.client.as_deref())
        )
        .map_err(|_| refused())?;
        let reader_stream = stream.try_clone().map_err(|_| refused())?;
        reader_stream
            .set_read_timeout(Some(CONNECT_WAIT))
            .map_err(|_| refused())?;
        let mut reader = BufReader::new(reader_stream);
        match connection::read_line(&mut reader, protocol::MAX_LINE) {
            Ok(Line::Text(answer)) if answer == connection::WELCOME => {}
            _ => return Err(refused()),
        }
        let _ = reader.get_ref().set_read_timeout(None);
        self.connections += 1;
        let number = self.connections;
        let events = self.events.clone();
        thread::spawn(move || {
            loop {
                match connection::read_line(&mut reader, protocol::MAX_LINE) {
                    Ok(Line::Text(line)) => {
                        if events.send(Event::App(line)).is_err() {
                            return;
                        }
                    }
                    Ok(Line::TooLong) => {}
                    Ok(Line::End) | Err(_) => {
                        let _ = events.send(Event::Closed(number));
                        return;
                    }
                }
            }
        });
        Ok(Connection { number, stream })
    }

    fn app_line(&mut self, line: String) {
        if let Ok(message) = serde_json::from_str::<Value>(&line)
            && message.get("method").is_none()
            && let Some(id) = message.get("id")
        {
            // A reply: answered, whichever connection it came on.
            self.waiting.remove(&id.to_string());
        }
        self.send(&line);
    }

    /// Connection `number` closed: calls it hadn't answered are asked again
    /// (reads) or fail.
    fn closed(&mut self, number: u64) {
        if self.connection.as_ref().is_some_and(|c| c.number == number) {
            self.drop_connection();
        }
        let (lost, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(_, w)| w.connection == number);
        self.waiting = kept.into_iter().collect();
        for (_, mut waiting) in lost {
            if !waiting.read || waiting.retried {
                let message = if waiting.read {
                    "canopy closed the connection before answering; try again"
                } else {
                    "canopy closed the connection before answering, so this may or may not \
                     have happened: check (get_project, get_history) before trying again"
                };
                self.fail(&waiting.id, message);
                continue;
            }
            waiting.retried = true;
            match self.forward(&waiting.line) {
                Ok(number) => {
                    waiting.connection = number;
                    self.waiting.insert(waiting.id.to_string(), waiting);
                }
                Err(Unreachable::Refused(message)) => self.fail(&waiting.id, &message),
                Err(Unreachable::Broken) => self.fail(&waiting.id, NOT_RUNNING),
            }
        }
    }

    /// Answers call `id` with a tool error, which the model reads.
    fn fail(&mut self, id: &Value, message: &str) {
        let result = protocol::tool_result(&[Content::Text(message.to_owned())], true);
        self.send(&protocol::result(id, result));
    }
}

enum Unreachable {
    /// No app, or it wouldn't have us: says why.
    Refused(String),
    /// The connection broke as the line went out.
    Broken,
}
