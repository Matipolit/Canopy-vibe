use std::fmt;
use std::io;

use symphonia::core::errors::Error as SymphoniaError;

/// Why a file couldn't be loaded. Holds plain strings rather than the
/// underlying errors so it can be cloned and kept around for the UI to show.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaError {
    /// The file couldn't be opened or read.
    Io(String),
    /// Not a container or codec we can decode.
    Unsupported(String),
    /// Recognized, but the data is malformed.
    Decode(String),
    /// The container holds no audio we can find (video only, say).
    NoAudioTrack,
    /// Decoded fine but contains zero frames.
    Empty,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "couldn't read file: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported format: {msg}"),
            Self::Decode(msg) => write!(f, "couldn't decode: {msg}"),
            Self::NoAudioTrack => f.write_str("file has no audio track"),
            Self::Empty => f.write_str("file contains no audio"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<io::Error> for MediaError {
    fn from(err: io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Not a `From` impl, so symphonia's types stay out of our public API.
pub(crate) fn from_symphonia(err: SymphoniaError) -> MediaError {
    match err {
        // Symphonia reads through the file we already opened, so an unexpected
        // end of file means the data was cut short, not that the disk failed.
        SymphoniaError::IoError(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            MediaError::Decode("unexpected end of file".into())
        }
        SymphoniaError::IoError(e) => MediaError::Io(e.to_string()),
        SymphoniaError::Unsupported(msg) => MediaError::Unsupported(msg.into()),
        other => MediaError::Decode(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_readable() {
        assert_eq!(
            MediaError::Io("No such file".into()).to_string(),
            "couldn't read file: No such file"
        );
        assert_eq!(MediaError::Empty.to_string(), "file contains no audio");
    }

    #[test]
    fn truncated_data_is_a_decode_error() {
        let eof = SymphoniaError::IoError(io::Error::from(io::ErrorKind::UnexpectedEof));
        assert!(matches!(from_symphonia(eof), MediaError::Decode(_)));
        let unsupported = SymphoniaError::Unsupported("codec");
        assert_eq!(
            from_symphonia(unsupported),
            MediaError::Unsupported("codec".into())
        );
    }
}
