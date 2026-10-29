//! Instruments' files, loaded on worker threads and kept by path, so tracks
//! that use the same file share it and the engine gets it the moment it's
//! ready: a sampler's .sfz and its samples, the Wavetable's user tables
//! (a WAV file per oscillator, band-limited on the worker too), and the
//! Slicer's loops (see [`Slicers`], which this holds).
//!
//! What a track needs is its [`DataKey`]; [`Banks::data`] gives what the
//! engine hands its instrument for that key, once everything in it has
//! loaded. A Wavetable track's oscillators' files load one by one and come
//! together as one `UserTables`, which is kept (as the same `Arc`, so the
//! engine doesn't rebuild the instrument) for as long as the set is used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use instruments::builtin::sampler::{self, Decoded, SfzBank};
use instruments::builtin::wavetable::{self as wt, UserTable, UserTables};
use instruments::{DataKind, InstrumentData};
use project::{Track, TrackId, TrackKind};

use crate::slicers::Slicers;

pub enum BankState {
    Loading,
    Ready(Arc<SfzBank>),
    Failed(String),
}

/// One user wavetable file, loading or loaded.
pub enum TableState {
    Loading,
    Ready(Arc<UserTable>),
    Failed(String),
}

/// What a track's instrument plays from, and what loads of it are kept by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataKey {
    /// An .sfz and its samples.
    Sfz(PathBuf),
    /// A Wavetable's user tables, by oscillator (as the track lists them).
    Wavetables(Vec<Option<PathBuf>>),
    /// A Slicer's loop, which depends on more of the track (and on the
    /// project's tempo) than its file: kept per track by [`Slicers`].
    Slices(TrackId),
}

impl DataKey {
    /// What `track`'s instrument needs loaded, if anything.
    pub fn of(track: &Track) -> Option<DataKey> {
        let TrackKind::Midi { instrument, .. } = &track.kind else {
            return None;
        };
        match instrument.descriptor()?.data {
            DataKind::None => None,
            DataKind::Sfz => Some(DataKey::Sfz(track.instrument_file()?.to_owned())),
            DataKind::Wavetables => {
                let files = track.wavetable_files();
                files
                    .iter()
                    .any(Option::is_some)
                    .then(|| DataKey::Wavetables(files.to_vec()))
            }
            DataKind::Slices => Some(DataKey::Slices(track.id)),
        }
    }
}

/// A load finished on a worker thread.
enum Done {
    Sfz(PathBuf, Result<Arc<SfzBank>, String>),
    Table(PathBuf, Result<Arc<UserTable>, String>),
}

pub struct Banks {
    states: HashMap<PathBuf, BankState>,
    tables: HashMap<PathBuf, TableState>,
    /// Sets of user tables, by the files they're made of, once every file
    /// in the set has loaded (or failed).
    sets: HashMap<Vec<Option<PathBuf>>, Arc<UserTables>>,
    /// Sets asked for whose files are still loading.
    pending: Vec<Vec<Option<PathBuf>>>,
    done: Receiver<Done>,
    send: Sender<Done>,
    wake: Arc<dyn Fn() + Send + Sync>,
    pub slicers: Slicers,
}

impl Banks {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Banks {
        let (send, done) = channel();
        Banks {
            states: HashMap::new(),
            tables: HashMap::new(),
            sets: HashMap::new(),
            pending: Vec::new(),
            done,
            send,
            slicers: Slicers::new(wake.clone()),
            wake,
        }
    }

    /// Starts loading what `key` needs unless it's loaded or loading already.
    pub fn request(&mut self, key: &DataKey) {
        match key {
            DataKey::Sfz(path) => self.request_sfz(path),
            DataKey::Wavetables(files) => {
                for path in files.iter().flatten() {
                    self.request_table(path);
                }
                // Put together by `receive`, which says it's there.
                if !self.sets.contains_key(files) && !self.pending.contains(files) {
                    self.pending.push(files.clone());
                }
            }
            // Kept in step with the project by `Slicers::update`.
            DataKey::Slices(_) => {}
        }
    }

    fn request_sfz(&mut self, path: &Path) {
        if self.states.contains_key(path) {
            return;
        }
        self.states.insert(path.to_owned(), BankState::Loading);
        let (path, send, wake) = (path.to_owned(), self.send.clone(), self.wake.clone());
        std::thread::Builder::new()
            .name("sfz loader".into())
            .spawn(move || {
                let result = load(&path).map(Arc::new);
                let _ = send.send(Done::Sfz(path, result));
                wake();
            })
            .ok();
    }

    fn request_table(&mut self, path: &Path) {
        if self.tables.contains_key(path) {
            return;
        }
        self.tables.insert(path.to_owned(), TableState::Loading);
        let (path, send, wake) = (path.to_owned(), self.send.clone(), self.wake.clone());
        std::thread::Builder::new()
            .name("wavetable loader".into())
            .spawn(move || {
                let result = load_table(&path).map(Arc::new);
                let _ = send.send(Done::Table(path, result));
                wake();
            })
            .ok();
    }

    /// Puts the set of user tables `files` together if every file in it has
    /// loaded or failed. False if some are still loading.
    fn assemble(&mut self, files: &[Option<PathBuf>]) -> bool {
        let mut set = UserTables::default();
        for (osc, path) in files.iter().enumerate() {
            let Some(path) = path else {
                continue;
            };
            match self.tables.get(path) {
                Some(TableState::Ready(table)) => {
                    if let Some(slot) = set.tables.get_mut(osc) {
                        *slot = Some(table.clone());
                    }
                }
                Some(TableState::Failed(_)) => {}
                Some(TableState::Loading) | None => return false,
            }
        }
        self.sets.insert(files.to_vec(), Arc::new(set));
        true
    }

    /// Loads it again, picking up changes to the file.
    pub fn reload(&mut self, path: &Path) {
        self.states.remove(path);
        self.request_sfz(path);
    }

    /// Loads a user wavetable file again, picking up changes to it. The
    /// sets it's in come together again once it has.
    pub fn reload_table(&mut self, path: &Path) {
        self.tables.remove(path);
        let path = Some(path.to_owned());
        self.sets.retain(|files, _| !files.contains(&path));
    }

    /// Takes in finished loads; true if any arrived, or a set of user
    /// tables came together.
    pub fn receive(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Sfz(path, result) => {
                    // Ignore loads of files forgotten in the meantime.
                    if let Some(state) = self.states.get_mut(&path) {
                        *state = match result {
                            Ok(bank) => BankState::Ready(bank),
                            Err(err) => BankState::Failed(err),
                        };
                        any = true;
                    }
                }
                Done::Table(path, result) => {
                    if let Some(state) = self.tables.get_mut(&path)
                        && matches!(state, TableState::Loading)
                    {
                        *state = match result {
                            Ok(table) => TableState::Ready(table),
                            Err(err) => TableState::Failed(err),
                        };
                        any = true;
                    }
                }
            }
        }
        for files in std::mem::take(&mut self.pending) {
            if self.sets.contains_key(&files) {
                continue;
            }
            if self.assemble(&files) {
                any = true;
            } else {
                self.pending.push(files);
            }
        }
        any
    }

    pub fn get(&self, path: &Path) -> Option<&BankState> {
        self.states.get(path)
    }

    /// How a user wavetable file is doing.
    pub fn table(&self, path: &Path) -> Option<&TableState> {
        self.tables.get(path)
    }

    /// What the engine hands the instrument, once it's loaded.
    pub fn data(&self, key: &DataKey) -> Option<InstrumentData> {
        match key {
            DataKey::Sfz(path) => match self.states.get(path)? {
                BankState::Ready(bank) => Some(bank.clone()),
                _ => None,
            },
            DataKey::Wavetables(files) => {
                let set = self.sets.get(files)?;
                Some(set.clone() as InstrumentData)
            }
            DataKey::Slices(track) => Some(self.slicers.data(*track)? as InstrumentData),
        }
    }

    /// Forgets files no track uses any more, freeing their samples.
    pub fn retain(&mut self, used: &[DataKey]) {
        let (mut sfz, mut files, mut sets) = (Vec::new(), Vec::new(), Vec::new());
        for key in used {
            match key {
                DataKey::Sfz(path) => sfz.push(path),
                DataKey::Wavetables(set) => {
                    files.extend(set.iter().flatten());
                    sets.push(set);
                }
                DataKey::Slices(_) => {}
            }
        }
        self.states.retain(|path, _| sfz.contains(&path));
        self.tables.retain(|path, _| files.contains(&path));
        self.sets.retain(|set, _| sets.contains(&set));
        self.pending.retain(|set| sets.contains(&set));
    }
}

/// Reads an SFZ file and decodes every sample it uses.
fn load(path: &Path) -> Result<SfzBank, String> {
    let decode = |file: &Path| {
        let (audio, _) = media::decode(file).map_err(|e| e.to_string())?;
        Ok(Decoded::stereo(
            audio.left().to_vec(),
            audio.right().to_vec(),
            audio.sample_rate(),
        ))
    };
    sampler::load(path, &decode).map_err(|e| e.to_string())
}

/// Reads a user wavetable: decoded at its own rate (frames are counted in
/// samples), its first channel, band-limited here on the worker thread.
fn load_table(path: &Path) -> Result<UserTable, String> {
    let decode = |file: &Path| {
        let (audio, _) = media::decode(file).map_err(|e| e.to_string())?;
        Ok(audio.left().to_vec())
    };
    wt::load_user_table(path, &decode)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// A 32-bit float mono WAV of `frames` frames of `size` samples, with a
    /// `clm ` chunk giving the size: the layout Serum writes, and the FL
    /// Studio importer for Vital's tables.
    fn float_wavetable(frames: usize, size: usize) -> Vec<u8> {
        let samples: Vec<f32> = (0..frames * size)
            .map(|i| {
                let (frame, p) = (i / size, (i % size) as f32 / size as f32);
                (std::f32::consts::TAU * p * (frame + 1) as f32).sin()
            })
            .collect();
        let mut clm = format!("<!>{size} 10000000 wavetable").into_bytes();
        if clm.len() % 2 == 1 {
            clm.push(0);
        }
        let data_len = samples.len() * 4;
        let mut out = b"RIFF".to_vec();
        out.extend(((4 + 24 + 8 + clm.len() + 8 + data_len) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(3u16.to_le_bytes()); // IEEE float
        out.extend(1u16.to_le_bytes()); // mono
        out.extend(44_100u32.to_le_bytes());
        out.extend((44_100u32 * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend(b"clm ");
        out.extend((clm.len() as u32).to_le_bytes());
        out.extend(&clm);
        out.extend(b"data");
        out.extend((data_len as u32).to_le_bytes());
        for s in samples {
            out.extend(s.to_le_bytes());
        }
        out
    }

    #[test]
    fn user_tables_load_per_file_and_come_together_as_one_set() {
        let dir = std::env::temp_dir().join(format!("canopy-banks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, c) = (dir.join("a.wav"), dir.join("c.wav"));
        std::fs::write(&a, float_wavetable(4, 256)).unwrap();
        std::fs::write(&c, float_wavetable(1, 2048)).unwrap();
        let broken = dir.join("broken.wav");
        std::fs::write(&broken, b"RIFF....WAVE nothing").unwrap();

        let mut banks = Banks::new(Arc::new(|| {}));
        let wait = |banks: &mut Banks, key: &DataKey| {
            let start = Instant::now();
            while banks.data(key).is_none() {
                assert!(start.elapsed() < Duration::from_secs(20), "{key:?}");
                std::thread::sleep(Duration::from_millis(5));
                banks.receive();
            }
            let data = banks.data(key).unwrap();
            data.downcast::<UserTables>().unwrap()
        };
        let key = DataKey::Wavetables(vec![Some(a.clone()), None, Some(c.clone())]);
        banks.request(&key);
        let set = wait(&mut banks, &key);
        let frames = |set: &UserTables, osc: usize| set.tables[osc].as_ref().map(|t| t.frames());
        assert_eq!(frames(&set, 0), Some(4), "the clm chunk's frame size");
        assert_eq!(frames(&set, 1), None);
        assert_eq!(frames(&set, 2), Some(1));
        // Asked again, it's the same set: the engine keeps its instrument.
        banks.request(&key);
        banks.receive();
        assert!(Arc::ptr_eq(&set, &wait(&mut banks, &key)));

        // A file that fails leaves its oscillator without, and says why.
        let with_broken = DataKey::Wavetables(vec![Some(a.clone()), Some(broken.clone())]);
        banks.request(&with_broken);
        let set = wait(&mut banks, &with_broken);
        assert_eq!(frames(&set, 0), Some(4));
        assert_eq!(frames(&set, 1), None);
        assert!(matches!(banks.table(&broken), Some(TableState::Failed(_))));

        // What no track uses any more is let go.
        banks.retain(std::slice::from_ref(&key));
        assert!(banks.table(&broken).is_none());
        assert!(banks.data(&with_broken).is_none());
        assert!(banks.data(&key).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
