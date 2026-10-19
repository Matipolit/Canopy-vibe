use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::SourceId;

/// An audio file the project uses. The samples themselves live in the
/// `media` crate's cache, not in the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSource {
    pub id: SourceId,
    pub path: PathBuf,
    /// The file's own rate. Clip offsets and lengths count frames at this rate,
    /// so they stay valid whatever rate the session runs at.
    pub sample_rate: u32,
    pub frames: u64,
    pub channels: u16,
}

impl AudioSource {
    /// The file name without its extension, for clip labels.
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map_or_else(|| "audio".to_owned(), |s| s.to_string_lossy().into_owned())
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }
}
