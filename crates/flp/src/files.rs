//! Finding the files a project names. FL saves full paths from the machine
//! it ran on (`C:\Users\…`, `%FLStudioFactoryData%\…`), so besides the path
//! itself this looks for the file by name next to the `.flp`, where
//! "consolidated" and zipped projects keep their samples.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{ChannelKind, Project};

/// An audio file's format, as the probe reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub frames: u64,
    pub channels: u16,
}

/// A file that was found, and for audio, what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub audio: Option<AudioInfo>,
}

/// The files a project names, by the path as written in the file. `None`
/// for those that couldn't be found (or read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Files {
    found: HashMap<String, Option<Found>>,
}

/// How deep to look under the project's folder.
const SEARCH_DEPTH: usize = 2;
/// Folders to look in at most, so a project in a huge folder can't stall.
const SEARCH_LIMIT: usize = 400;

impl Files {
    /// Looks up every sample and SFZ file `project` names. `probe` reads an
    /// audio file's format, or fails for files it can't read.
    pub fn find(
        project: &Project,
        flp_path: Option<&Path>,
        probe: &dyn Fn(&Path) -> Option<AudioInfo>,
    ) -> Files {
        let folders = flp_path
            .and_then(Path::parent)
            .map(search_folders)
            .unwrap_or_default();
        let mut files = Files::default();
        for channel in &project.channels {
            if channel.kind == ChannelKind::Sampler
                && let Some(raw) = &channel.sample_path
                && !files.found.contains_key(raw)
            {
                let found = locate(raw, &folders).and_then(|path| {
                    let audio = probe(&path)?;
                    Some(Found {
                        path,
                        audio: Some(audio),
                    })
                });
                files.found.insert(raw.clone(), found);
            }
            if let Some(raw) = channel.plugin.as_ref().and_then(|p| sfz_path_in(&p.state))
                && !files.found.contains_key(&raw)
            {
                let found = locate(&raw, &folders).map(|path| Found { path, audio: None });
                files.found.insert(raw, found);
            }
        }
        files
    }

    /// Records a lookup's result (for tests, or a caller that looks itself).
    pub fn insert(&mut self, raw: &str, found: Option<Found>) {
        self.found.insert(raw.to_owned(), found);
    }

    pub fn get(&self, raw: &str) -> Option<&Found> {
        self.found.get(raw)?.as_ref()
    }
}

/// The file `raw` names, if it's there: as written, with Windows
/// separators turned round, or by name in one of `folders`.
pub fn locate(raw: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    let raw = raw.trim().trim_end_matches('\0');
    if raw.is_empty() {
        return None;
    }
    let slashed = raw.replace('\\', "/");
    for candidate in [raw, slashed.as_str()] {
        let path = Path::new(candidate);
        if path.is_absolute() && path.is_file() {
            return Some(path.to_owned());
        }
    }
    let name = slashed.rsplit('/').next().filter(|n| !n.is_empty())?;
    folders
        .iter()
        .map(|folder| folder.join(name))
        .find(|path| path.is_file())
}

/// `dir` and the folders under it, breadth first, a couple of levels deep.
fn search_folders(dir: &Path) -> Vec<PathBuf> {
    let mut folders = vec![dir.to_owned()];
    let mut level = vec![dir.to_owned()];
    for _ in 0..SEARCH_DEPTH {
        let mut next = Vec::new();
        for folder in &level {
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            let mut subfolders: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            subfolders.sort();
            next.extend(subfolders);
        }
        next.truncate(SEARCH_LIMIT.saturating_sub(folders.len()));
        folders.extend(next.iter().cloned());
        level = next;
    }
    folders
}

/// An `.sfz` path inside a plugin's saved state (sforzando and the like
/// keep the file they play there), as ASCII or UTF-16 text.
pub fn sfz_path_in(state: &[u8]) -> Option<String> {
    let ascii = |b: u8| (0x20..0x7F).contains(&b) && b != b'"';
    // Single-byte text.
    let lower: Vec<u8> = state.iter().map(u8::to_ascii_lowercase).collect();
    if let Some(end) = find(&lower, b".sfz") {
        let start = state[..end]
            .iter()
            .rposition(|b| !ascii(*b))
            .map_or(0, |i| i + 1);
        let path = String::from_utf8_lossy(&state[start..end + 4]).into_owned();
        if looks_like_path(&path) {
            return Some(path);
        }
    }
    // UTF-16LE text: every other byte zero.
    if let Some(end) = find(&lower, b".\0s\0f\0z\0") {
        let mut start = end;
        while start >= 2 && state[start - 1] == 0 && ascii(state[start - 2]) {
            start -= 2;
        }
        let units: Vec<u16> = state[start..end + 8]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let path = String::from_utf16_lossy(&units);
        if looks_like_path(&path) {
            return Some(path);
        }
    }
    None
}

fn looks_like_path(path: &str) -> bool {
    path.len() > 4 && (path.contains('/') || path.contains('\\'))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfz_paths_in_plugin_state() {
        let mut state = vec![1, 2, 0, 0];
        state.extend_from_slice(b"C:\\Libraries\\Piano\\Grand.SFZ");
        state.extend_from_slice(&[0, 9, 9]);
        assert_eq!(
            sfz_path_in(&state).as_deref(),
            Some("C:\\Libraries\\Piano\\Grand.SFZ")
        );
        let utf16: Vec<u8> = "/home/me/strings.sfz"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut state = vec![0xFF, 0x01];
        state.extend(utf16);
        assert_eq!(sfz_path_in(&state).as_deref(), Some("/home/me/strings.sfz"));
        assert_eq!(sfz_path_in(b"nothing here .sfz"), None, "not a path");
        assert_eq!(sfz_path_in(&[]), None);
    }

    #[test]
    fn finds_samples_by_name_next_to_the_project() {
        let dir = std::env::temp_dir().join(format!("flp-files-{}", std::process::id()));
        let samples = dir.join("Samples");
        std::fs::create_dir_all(&samples).unwrap();
        std::fs::write(samples.join("Kick 1.wav"), b"x").unwrap();
        let folders = search_folders(&dir);
        assert_eq!(
            locate("C:\\Users\\me\\Kick 1.wav", &folders),
            Some(samples.join("Kick 1.wav"))
        );
        assert_eq!(
            locate("%FLStudioFactoryData%/Data/Kick 1.wav", &folders),
            Some(samples.join("Kick 1.wav"))
        );
        let absolute = samples.join("Kick 1.wav");
        assert_eq!(
            locate(absolute.to_str().unwrap(), &[]),
            Some(absolute.clone())
        );
        assert_eq!(locate("C:\\gone.wav", &folders), None);
        assert_eq!(locate("", &folders), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
