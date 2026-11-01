//! How the bridge finds the running app and proves it may talk to it.
//!
//! The app listens on a random port of `127.0.0.1` and writes an
//! [`Advert`] (the port, a fresh random token, its process id) to a file
//! only the user can read ([`file_path`]). The bridge reads the file,
//! connects, and sends [`hello`] as its first line; the app answers
//! [`WELCOME`] if the token matches and closes the connection otherwise.
//! After that both sides send MCP's JSON-RPC messages, one per line.
//!
//! An advert left behind by a canopy that died needs no special care: its
//! port refuses the connection, which reads as "canopy isn't running",
//! and the next canopy to start overwrites it.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What the app writes for the bridge to find it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advert {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

/// How long a new connection has to send its token before it is closed.
pub const TOKEN_WAIT: Duration = Duration::from_secs(2);

/// The app's answer to a good [`hello`].
pub const WELCOME: &str = r#"{"ok":true}"#;

/// Where the advert is kept: `CANOPY_MCP_FILE` if it is set (tests), else
/// `%LOCALAPPDATA%\canopy\mcp.json` on Windows and
/// `$XDG_CACHE_HOME/canopy/mcp.json` or `~/.cache/canopy/mcp.json`
/// elsewhere. `None` when none of those can be found.
pub fn file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CANOPY_MCP_FILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let non_empty = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let cache = if cfg!(windows) {
        PathBuf::from(non_empty("LOCALAPPDATA")?)
    } else if let Some(cache) = non_empty("XDG_CACHE_HOME") {
        PathBuf::from(cache)
    } else {
        PathBuf::from(non_empty("HOME")?).join(".cache")
    };
    Some(cache.join("canopy").join("mcp.json"))
}

/// A new token: 32 random bytes from the operating system, as 64
/// lowercase hexadecimal digits.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system has random numbers");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `advert` to `path`, replacing any advert there, so that a reader
/// sees either the old file or the new one, never half of one: it is
/// written beside `path` and renamed over it. On Unix only the user may
/// read it.
pub fn write_advert(path: &Path, advert: &Advert) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let text = serde_json::to_string(advert).map_err(io::Error::other)?;
    let write = || -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// The advert at `path`, if there is a readable one.
pub fn read_advert(path: &Path) -> Option<Advert> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Whether two tokens are the same, taking as long whichever byte differs,
/// so the time an answer takes says nothing about how much of a guess was
/// right.
pub fn tokens_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The bridge's first line: the token, and the client's name (from
/// `initialize`'s `clientInfo`) for the app to tell whose edits are whose.
pub fn hello(token: &str, client: Option<&str>) -> String {
    json!({ "token": token, "client": client }).to_string()
}

/// The token and client name from a [`hello`] line; `None` if it isn't one.
pub fn parse_hello(line: &str) -> Option<(String, Option<String>)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let token = value.get("token")?.as_str()?.to_owned();
    let client = value
        .get("client")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((token, client))
}

/// One line read by [`read_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A line, without its line ending.
    Text(String),
    /// A line longer than the limit. It was read to its end and dropped,
    /// so the next line reads normally.
    TooLong,
    /// The other side closed the connection.
    End,
}

/// Reads one line of at most `max` bytes. A line that isn't UTF-8 reads
/// with its bad bytes replaced, for the JSON parser to refuse.
pub fn read_line(reader: &mut impl BufRead, max: usize) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            // The end, perhaps of a last line without a newline.
            return Ok(if too_long {
                Line::TooLong
            } else if line.is_empty() {
                Line::End
            } else {
                Line::Text(text(line))
            });
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let part = &available[..newline.unwrap_or(available.len())];
        let (used, done) = match newline {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        if !too_long {
            if line.len() + part.len() > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(part);
            }
        }
        reader.consume(used);
        if done {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Text(text(line))
            });
        }
    }
}

fn text(mut line: Vec<u8>) -> String {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("canopy-mcp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn adverts_round_trip() {
        let dir = temp_dir("advert");
        let path = dir.join("deeper").join("mcp.json");
        assert_eq!(read_advert(&path), None);
        let advert = Advert {
            port: 41234,
            token: new_token(),
            pid: 77,
        };
        write_advert(&path, &advert).unwrap();
        assert_eq!(read_advert(&path), Some(advert.clone()));
        // A second write replaces the first, and leaves nothing beside it.
        let newer = Advert { port: 5, ..advert };
        write_advert(&path, &newer).unwrap();
        assert_eq!(read_advert(&path), Some(newer));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only the user may read the token");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tokens_compare_by_content() {
        let token = new_token();
        assert!(tokens_match(&token, &token.clone()));
        let mut other = token.clone().into_bytes();
        other[63] = if other[63] == b'0' { b'1' } else { b'0' };
        assert!(!tokens_match(&token, std::str::from_utf8(&other).unwrap()));
        assert!(!tokens_match(&token, &token[..63]));
        assert!(!tokens_match(&token, ""));
    }

    #[test]
    fn tokens_are_64_hex_digits() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(token, new_token(), "each one is new");
    }

    #[test]
    fn hello_carries_the_token_and_the_client() {
        assert_eq!(
            parse_hello(&hello("abc", Some("Claude Code"))),
            Some(("abc".to_owned(), Some("Claude Code".to_owned())))
        );
        assert_eq!(
            parse_hello(&hello("abc", None)),
            Some(("abc".to_owned(), None))
        );
        assert_eq!(parse_hello(r#"{"jsonrpc":"2.0","method":"ping"}"#), None);
        assert_eq!(parse_hello("not json"), None);
    }

    #[test]
    fn long_lines_are_dropped_and_reading_goes_on() {
        let input = format!("short\r\n{}\nafter\nlast", "x".repeat(100));
        // A small buffer, so lines span several reads.
        let mut reader = io::BufReader::with_capacity(7, input.as_bytes());
        let mut read = || read_line(&mut reader, 50).unwrap();
        assert_eq!(read(), Line::Text("short".into()));
        assert_eq!(read(), Line::TooLong);
        assert_eq!(read(), Line::Text("after".into()));
        assert_eq!(read(), Line::Text("last".into()));
        assert_eq!(read(), Line::End);
    }
}
