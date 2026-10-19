//! The container: the `FLhd` header, the `FLdt` chunk, and the stream of
//! events inside it.
//!
//! An event is an id byte followed by its value, whose size the id decides:
//! ids below 64 carry a byte, below 128 a 16-bit word, below 192 a 32-bit
//! dword, and the rest a varint length and that many bytes (text, or a
//! struct). Everything is little-endian.

use std::fmt;

/// The first id of each size class.
pub const WORD: u8 = 64;
pub const DWORD: u8 = 128;
pub const TEXT: u8 = 192;
/// Variable-length ids from here on hold binary data, except a few later
/// additions that hold text (see [`is_text`]).
pub const DATA: u8 = 208;

/// File types FL writes in this container. Projects are 0; the others are
/// presets and scores, which hold pieces of a project.
const KNOWN_FORMATS: [i16; 10] = [-1, 0, 0x10, 0x18, 0x20, 0x30, 0x31, 0x32, 0x40, 0x50];

/// Why a file couldn't be read at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// It doesn't start with `FLhd`: not an FL Studio file.
    NotFlp,
    /// The header is cut short or holds impossible values.
    BadHeader(&'static str),
    /// There's no `FLdt` chunk after the header.
    NoData,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFlp => write!(f, "not an FL Studio project"),
            Self::BadHeader(what) => write!(f, "damaged FL Studio project: {what}"),
            Self::NoData => write!(f, "damaged FL Studio project: no data chunk"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// 0 for a project; presets and scores use other values.
    pub format: i16,
    /// How many channels the channel rack has.
    pub channels: u16,
    /// Ticks per quarter note.
    pub ppq: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Byte(u8),
    Word(u16),
    Dword(u32),
    /// Text or binary data, still encoded.
    Data(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event<'a> {
    pub id: u8,
    pub value: Value<'a>,
}

impl<'a> Event<'a> {
    /// The value as an unsigned number, for the fixed-size kinds.
    pub fn number(&self) -> Option<u32> {
        match self.value {
            Value::Byte(v) => Some(v.into()),
            Value::Word(v) => Some(v.into()),
            Value::Dword(v) => Some(v),
            Value::Data(_) => None,
        }
    }

    pub fn data(&self) -> Option<&'a [u8]> {
        match self.value {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }
}

/// Whether a variable-length event holds text rather than a struct.
pub fn is_text(id: u8) -> bool {
    // Arrangement, display group and playlist track names came later and
    // took ids from the data range.
    (TEXT..DATA).contains(&id) || matches!(id, 231 | 239 | 241)
}

/// The events in a file, and whether it ended in the middle of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Events<'a> {
    pub header: Header,
    pub events: Vec<Event<'a>>,
    /// The file stopped short: the declared data size was more than there
    /// is, or the last event was cut off. What came before is still good.
    pub truncated: bool,
}

/// Reads the header and splits the data chunk into events. Fails only when
/// the header is unusable; a file cut short gives the events before the cut.
pub fn read(bytes: &[u8]) -> Result<Events<'_>, ParseError> {
    let mut cursor = Cursor::new(bytes);
    if cursor.take(4) != Some(b"FLhd".as_slice()) {
        return Err(ParseError::NotFlp);
    }
    let size = cursor
        .u32()
        .ok_or(ParseError::BadHeader("header cut short"))?;
    if size != 6 {
        return Err(ParseError::BadHeader("unexpected header size"));
    }
    let format = cursor
        .u16()
        .ok_or(ParseError::BadHeader("header cut short"))? as i16;
    let channels = cursor
        .u16()
        .ok_or(ParseError::BadHeader("header cut short"))?;
    let ppq = cursor
        .u16()
        .ok_or(ParseError::BadHeader("header cut short"))?;
    if !KNOWN_FORMATS.contains(&format) {
        return Err(ParseError::BadHeader("unknown file type"));
    }
    if ppq == 0 {
        return Err(ParseError::BadHeader("no timebase"));
    }
    let header = Header {
        format,
        channels,
        ppq,
    };
    if cursor.take(4) != Some(b"FLdt".as_slice()) {
        return Err(ParseError::NoData);
    }
    let declared = cursor.u32().ok_or(ParseError::NoData)? as usize;
    let rest = cursor.rest();
    let mut truncated = declared > rest.len();
    let data = &rest[..declared.min(rest.len())];

    let mut events = Vec::new();
    let mut cursor = Cursor::new(data);
    // The version comes first; one event's size depends on it.
    let mut major = None;
    while !cursor.is_empty() {
        match read_event(&mut cursor, major) {
            Some(event) => {
                if event.id == VERSION
                    && let Some(text) = event.data()
                {
                    major = version_major(text);
                }
                events.push(event);
            }
            None => {
                truncated = true;
                break;
            }
        }
    }
    Ok(Events {
        header,
        events,
        truncated,
    })
}

/// The ASCII version string event, `"20.8.4.2576"`.
pub const VERSION: u8 = TEXT + 7;

/// FL 25 writes this id, from the dword range, with only three bytes.
const THREE_BYTE_ID: u8 = 172;

fn version_major(text: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(text).ok()?;
    text.trim_end_matches('\0').split('.').next()?.parse().ok()
}

fn read_event<'a>(cursor: &mut Cursor<'a>, major: Option<u32>) -> Option<Event<'a>> {
    let id = cursor.u8()?;
    let value = if id < WORD {
        Value::Byte(cursor.u8()?)
    } else if id < DWORD {
        Value::Word(cursor.u16()?)
    } else if id == THREE_BYTE_ID && major.is_some_and(|m| m >= 25) {
        Value::Data(cursor.take(3)?)
    } else if id < TEXT {
        Value::Dword(cursor.u32()?)
    } else {
        let len = cursor.varint()?;
        Value::Data(cursor.take(usize::try_from(len).ok()?)?)
    };
    Some(Event { id, value })
}

/// Reads little-endian values from a slice, returning `None` past its end.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos.min(self.bytes.len())..]
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.array()?))
    }

    /// An unsigned LEB128 number: 7 bits a byte, low bits first, the top
    /// bit set on every byte but the last. At most 32 bits.
    pub fn varint(&mut self) -> Option<u32> {
        let mut value: u32 = 0;
        for shift in (0..35).step_by(7) {
            let byte = self.u8()?;
            let bits = u32::from(byte & 0x7F);
            // The fifth byte may only hold the top four bits.
            if shift == 28 && bits > 0x0F {
                return None;
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

/// How the file stores text: FL 11.5 and later write UTF-16LE, older
/// versions single-byte ANSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf16,
    Ansi,
    /// No version seen yet: guess from each string.
    Unknown,
}

impl TextEncoding {
    /// From the version string FL writes first (`"20.8.4.2576"`).
    pub fn for_version(version: &str) -> TextEncoding {
        let mut parts = version.split('.').map(|p| p.trim().parse::<u32>());
        match (parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor))) if (major, minor) >= (11, 5) => TextEncoding::Utf16,
            (Some(Ok(_)), _) => TextEncoding::Ansi,
            _ => TextEncoding::Unknown,
        }
    }

    pub fn decode(self, bytes: &[u8]) -> String {
        let utf16 = match self {
            TextEncoding::Utf16 => true,
            TextEncoding::Ansi => false,
            TextEncoding::Unknown => looks_utf16(bytes),
        };
        let text = if utf16 {
            decode_utf16le(bytes)
        } else {
            decode_ansi(bytes)
        };
        text.trim_end_matches('\0').to_owned()
    }
}

/// UTF-16LE text, with any broken surrogates replaced.
pub fn decode_utf16le(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Single-byte text. FL wrote the Windows code page; Latin-1 gets the
/// common characters right and never fails.
pub fn decode_ansi(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Even length, ending in a two-byte terminator or with every high byte
/// zero (plain ASCII as UTF-16).
fn looks_utf16(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    bytes.ends_with(&[0, 0]) || bytes.iter().skip(1).step_by(2).all(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(events: &[u8]) -> Vec<u8> {
        let mut bytes = b"FLhd".to_vec();
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&96u16.to_le_bytes());
        bytes.extend_from_slice(b"FLdt");
        bytes.extend_from_slice(&(events.len() as u32).to_le_bytes());
        bytes.extend_from_slice(events);
        bytes
    }

    #[test]
    fn reads_every_size_class() {
        let bytes = file(&[
            9, 1, // byte
            66, 0x34, 0x12, // word
            156, 0x58, 0x0F, 0x02, 0x00, // dword: 135 000
            194, 3, b'a', b'b', b'c', // text
            224, 0, // empty data
        ]);
        let read = read(&bytes).unwrap();
        assert_eq!(
            read.header,
            Header {
                format: 0,
                channels: 3,
                ppq: 96
            }
        );
        assert!(!read.truncated);
        let values: Vec<Value> = read.events.iter().map(|e| e.value).collect();
        assert_eq!(
            values,
            [
                Value::Byte(1),
                Value::Word(0x1234),
                Value::Dword(135_000),
                Value::Data(b"abc"),
                Value::Data(b""),
            ]
        );
        assert_eq!(read.events[2].number(), Some(135_000));
        assert_eq!(read.events[3].data(), Some(&b"abc"[..]));
    }

    #[test]
    fn varints() {
        let parse = |bytes: &[u8]| Cursor::new(bytes).varint();
        assert_eq!(parse(&[0]), Some(0));
        assert_eq!(parse(&[0x7F]), Some(127));
        assert_eq!(parse(&[0x80, 0x01]), Some(128));
        assert_eq!(parse(&[0xE5, 0x8E, 0x26]), Some(624_485));
        assert_eq!(parse(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]), Some(u32::MAX));
        assert_eq!(parse(&[0xFF, 0xFF, 0xFF, 0xFF, 0x1F]), None, "too big");
        assert_eq!(parse(&[0x80, 0x80]), None, "cut short");
        assert_eq!(
            parse(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]),
            None,
            "too long"
        );
        // Long data lengths are read right.
        let mut events = vec![213, 0x90, 0x03];
        events.extend(std::iter::repeat_n(7u8, 400));
        let bytes = file(&events);
        let read = read(&bytes).unwrap();
        assert_eq!(read.events[0].data().unwrap().len(), 400);
    }

    #[test]
    fn text_encodings() {
        let utf16: Vec<u8> = "Kick ü\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(TextEncoding::Utf16.decode(&utf16), "Kick ü");
        assert_eq!(TextEncoding::Unknown.decode(&utf16), "Kick ü");
        assert_eq!(TextEncoding::Ansi.decode(b"Kick \xFC\0"), "Kick ü");
        assert_eq!(TextEncoding::Unknown.decode(b"Kick\0"), "Kick");
        // A lone surrogate doesn't fail.
        assert_eq!(
            TextEncoding::Utf16.decode(&[0x00, 0xD8, b'a', 0]),
            "\u{FFFD}a"
        );
        // An odd byte at the end is dropped.
        assert_eq!(TextEncoding::Utf16.decode(&[b'a', 0, b'b']), "a");

        assert_eq!(
            TextEncoding::for_version("20.8.4.2576"),
            TextEncoding::Utf16
        );
        assert_eq!(TextEncoding::for_version("11.5.0"), TextEncoding::Utf16);
        assert_eq!(TextEncoding::for_version("11.1.1"), TextEncoding::Ansi);
        assert_eq!(TextEncoding::for_version("9.0"), TextEncoding::Ansi);
        assert_eq!(TextEncoding::for_version("junk"), TextEncoding::Unknown);
        assert!(is_text(192) && is_text(203) && is_text(241));
        assert!(!is_text(213) && !is_text(224));
    }

    #[test]
    fn bad_headers_are_errors() {
        assert_eq!(read(b""), Err(ParseError::NotFlp));
        assert_eq!(read(b"RIFF\0\0\0\0WAVE"), Err(ParseError::NotFlp));
        let good = file(&[]);
        assert!(read(&good).is_ok());
        assert!(matches!(read(&good[..10]), Err(ParseError::BadHeader(_))));
        assert_eq!(read(&good[..14]), Err(ParseError::NoData));

        let mut size = good.clone();
        size[4] = 8;
        assert!(matches!(read(&size), Err(ParseError::BadHeader(_))));
        let mut format = good.clone();
        format[9] = 1;
        assert!(matches!(read(&format), Err(ParseError::BadHeader(_))));
        let mut ppq = good.clone();
        ppq[12] = 0;
        ppq[13] = 0;
        assert!(matches!(read(&ppq), Err(ParseError::BadHeader(_))));
        let mut magic = good;
        magic[14] = b'X';
        assert_eq!(read(&magic), Err(ParseError::NoData));
    }

    #[test]
    fn fl25_writes_one_dword_id_with_three_bytes() {
        let mut events = vec![VERSION, 12];
        events.extend_from_slice(b"25.2.4.4960\0");
        events.extend([172, 1, 1, 0]);
        events.extend([156, 0xC0, 0xD4, 0x01, 0x00]);
        let bytes_25 = file(&events);
        let read_25 = read(&bytes_25).unwrap();
        assert!(!read_25.truncated);
        assert_eq!(read_25.events[1].value, Value::Data(&[1, 1, 0]));
        assert_eq!(read_25.events[2].number(), Some(120_000));

        // Earlier versions give it the usual four.
        let mut events = vec![VERSION, 12];
        events.extend_from_slice(b"20.8.4.2576\0");
        events.extend([172, 1, 1, 0, 0]);
        let bytes_20 = file(&events);
        let read_20 = read(&bytes_20).unwrap();
        assert_eq!(read_20.events[1].number(), Some(0x0101));
    }

    #[test]
    fn truncation_keeps_what_came_before() {
        let mut bytes = file(&[9, 1, 66, 1, 0, 194, 10, b'a']);
        let read_cut = read(&bytes).unwrap();
        assert!(read_cut.truncated);
        assert_eq!(read_cut.events.len(), 2);

        // Declared longer than the file.
        bytes = file(&[9, 1]);
        bytes[18] = 50;
        let read_long = read(&bytes).unwrap();
        assert!(read_long.truncated);
        assert_eq!(read_long.events.len(), 1);
    }
}
