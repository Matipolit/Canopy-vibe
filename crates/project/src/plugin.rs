//! What the project keeps about a plugin (a CLAP, VST3 or VST2 effect or
//! instrument) besides its parameters: who made it, where it was last
//! found, and its own saved state.
//!
//! The plugin itself is named by its `EffectRef`/`InstrumentRef` (format,
//! id, version). Its parameters live where a built-in's do, in the
//! device's (or track's) `params`, keyed by the plugin's parameter id as a
//! decimal string, so automation, controller links and modulation address
//! them like any other. CLAP stores plain values there, VST3 and VST2
//! normalized ones, except that a parameter the plugin lists choices for
//! stores the index of its choice (see `hosting::describe`).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A plugin device's (or instrument's) identity and state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginData {
    pub name: String,
    pub vendor: String,
    /// Where the plugin was last found. Only a hint: the id is what's
    /// looked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The plugin's own saved state: a CLAP plugin's, or a VST3 plugin's
    /// component and controller states.
    #[serde(default, skip_serializing_if = "Blob::is_empty")]
    pub state: Blob,
}

/// Opaque bytes, shared rather than copied, so undo (which clones the whole
/// project) stays cheap however big a plugin's state is. Saved as a base64
/// string. Equal if they're the same bytes.
#[derive(Clone, Default)]
pub struct Blob(Arc<[u8]>);

impl Blob {
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Blob {
        Blob(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The same allocation, not just the same bytes: a cheap "unchanged"
    /// test for whoever loads the state into a plugin.
    pub fn ptr_eq(&self, other: &Blob) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl From<Vec<u8>> for Blob {
    fn from(bytes: Vec<u8>) -> Blob {
        Blob(bytes.into())
    }
}

impl From<&[u8]> for Blob {
    fn from(bytes: &[u8]) -> Blob {
        Blob(bytes.into())
    }
}

impl PartialEq for Blob {
    fn eq(&self, other: &Blob) -> bool {
        self.ptr_eq(other) || self.0 == other.0
    }
}

impl Eq for Blob {}

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({} bytes)", self.0.len())
    }
}

impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Blob {
    /// A string that isn't base64 (a damaged file) reads as no state, so
    /// the rest of the project still opens: the plugin starts from its
    /// parameters alone.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Blob, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(decode(&text).map_or_else(Blob::default, Blob::from))
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, with padding.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The inverse of [`encode`]. Whitespace is skipped and padding optional;
/// anything else outside the alphabet fails.
fn decode(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| ALPHABET.iter().position(|a| *a == c).map(|d| d as u32);
    let digits: Vec<u32> = text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace())
        .take_while(|c| *c != b'=')
        .map(digit)
        .collect::<Option<_>>()?;
    if digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0, |n, (i, d)| n | (d << (18 - 6 * i)));
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        for (bytes, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(bytes), text);
            assert_eq!(decode(text).unwrap(), bytes);
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
        assert_eq!(decode("Zm9v\n YmFy").unwrap(), b"foobar", "whitespace");
        assert_eq!(decode("Zg").unwrap(), b"f", "no padding");
        assert!(decode("Zm9v!").is_none());
        assert!(decode("Z").is_none());
    }

    #[test]
    fn blobs_are_shared_and_compare_by_bytes() {
        let a = Blob::from(vec![1, 2, 3]);
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        let c = Blob::from(&[1u8, 2, 3][..]);
        assert!(!a.ptr_eq(&c));
        assert_eq!(a, c);
        assert_ne!(a, Blob::default());
        assert!(Blob::default().is_empty());
        assert_eq!(format!("{a:?}"), "Blob(3 bytes)");
    }

    #[test]
    fn blobs_round_trip_as_text_and_damage_reads_as_empty() {
        let data = PluginData {
            name: "Diva".into(),
            vendor: "u-he".into(),
            path: Some("/usr/lib/clap/Diva.clap".into()),
            state: Blob::from((0..200).map(|i| (i * 7) as u8).collect::<Vec<_>>()),
        };
        let text = ron::to_string(&data).unwrap();
        assert_eq!(ron::from_str::<PluginData>(&text).unwrap(), data);
        let empty = PluginData {
            path: None,
            state: Blob::default(),
            ..data.clone()
        };
        let text = ron::to_string(&empty).unwrap();
        assert!(!text.contains("state") && !text.contains("path"), "{text}");
        let damaged: PluginData =
            ron::from_str(r#"(name: "Diva", vendor: "u-he", state: "not base64!")"#).unwrap();
        assert!(damaged.state.is_empty());
    }
}
