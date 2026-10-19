use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use crate::{MediaError, Source, load};

/// At most this many files decode at once. Each one is single-threaded, so two
/// keeps a batch import moving without starving the audio or UI threads.
const MAX_WORKERS: usize = 2;

/// A finished load, successful or not.
#[derive(Debug)]
pub struct Loaded {
    /// Whatever the caller passed to `request`.
    pub key: u64,
    pub path: PathBuf,
    pub target_rate: u32,
    pub result: Result<Arc<Source>, MediaError>,
}

struct Request {
    key: u64,
    path: PathBuf,
    target_rate: u32,
}

type LoadFn = fn(&Path, u32) -> Result<Source, MediaError>;

/// Loads files on background threads so the UI never waits on the disk or a
/// decoder. Results come back through `try_recv` in the order they finish.
///
/// Dropping it lets the workers exit. They aren't joined: one may still be
/// busy with a long file, and the UI shouldn't wait for it. It finishes that
/// file, finds nobody listening, and exits.
pub struct Loader {
    requests: Sender<Request>,
    results: Receiver<Loaded>,
}

impl Loader {
    /// `wake` runs on a worker thread after each result is ready, so the UI
    /// can repaint and pick it up.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_load_fn(wake, load)
    }

    /// Lets tests swap in a load function that misbehaves.
    fn with_load_fn(wake: impl Fn() + Send + Sync + 'static, load: LoadFn) -> Self {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (result_tx, results) = mpsc::channel();
        let request_rx = Arc::new(Mutex::new(request_rx));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);

        let workers = thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .clamp(1, MAX_WORKERS);
        for i in 0..workers {
            let request_rx = Arc::clone(&request_rx);
            let result_tx = result_tx.clone();
            let wake = Arc::clone(&wake);
            thread::Builder::new()
                .name(format!("media-loader-{i}"))
                .spawn(move || worker(&request_rx, &result_tx, &*wake, load))
                .expect("failed to spawn media loader thread");
        }
        Self { requests, results }
    }

    /// Queue a load. `key` is yours to choose and comes back in `Loaded`.
    pub fn request(&self, key: u64, path: PathBuf, target_rate: u32) {
        // Only fails once every worker has exited, and they survive panics in
        // `load`, so in practice this always queues.
        let _ = self.requests.send(Request {
            key,
            path,
            target_rate,
        });
    }

    /// The next finished load, if any. Never blocks.
    pub fn try_recv(&self) -> Option<Loaded> {
        self.results.try_recv().ok()
    }
}

fn worker(
    requests: &Mutex<Receiver<Request>>,
    results: &Sender<Loaded>,
    wake: &dyn Fn(),
    load: LoadFn,
) {
    loop {
        // Holding the lock while blocked in recv is fine: the other workers
        // would only be waiting for the next request anyway.
        let request = requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv();
        // The Loader was dropped.
        let Ok(Request {
            key,
            path,
            target_rate,
        }) = request
        else {
            return;
        };

        // A decoder bug on one odd file shouldn't take the loader down with it.
        let result = catch_unwind(AssertUnwindSafe(|| load(&path, target_rate)))
            .unwrap_or_else(|panic| Err(MediaError::Decode(panic_message(&*panic))))
            .map(Arc::new);

        let loaded = Loaded {
            key,
            path,
            target_rate,
            result,
        };
        if results.send(loaded).is_err() {
            return;
        }
        wake();
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    let detail = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown cause");
    format!("decoder crashed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{TempDir, WavFormat, write_wav};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn wait_for(loader: &Loader, count: usize) -> Vec<Loaded> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut done = Vec::new();
        while done.len() < count {
            assert!(Instant::now() < deadline, "timed out waiting for loads");
            match loader.try_recv() {
                Some(loaded) => done.push(loaded),
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
        done.sort_by_key(|loaded| loaded.key);
        done
    }

    #[test]
    fn loads_in_the_background() {
        let dir = TempDir::new("loader");
        let path = dir.join("tone.wav");
        write_wav(&path, 44100, 2, WavFormat::Pcm16, &vec![0.25; 2 * 4410]);
        let missing = dir.join("missing.wav");

        let wakes = Arc::new(AtomicUsize::new(0));
        let loader = Loader::new({
            let wakes = Arc::clone(&wakes);
            move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert!(loader.try_recv().is_none());
        loader.request(7, path.clone(), 48000);
        loader.request(8, missing.clone(), 48000);

        let done = wait_for(&loader, 2);
        assert_eq!(
            (done[0].key, &done[0].path, done[0].target_rate),
            (7, &path, 48000)
        );
        let source = done[0].result.as_ref().unwrap();
        assert_eq!(source.native_rate, 44100);
        assert_eq!(source.native_frames, 4410);
        assert_eq!(source.channels, 2);
        assert_eq!(source.audio.sample_rate(), 48000);
        assert_eq!(source.audio.frames(), 4800);

        assert_eq!(done[1].path, missing);
        assert!(matches!(done[1].result, Err(MediaError::Io(_))));

        // wake runs just after the send, so give the second one a moment.
        let deadline = Instant::now() + Duration::from_secs(5);
        while wakes.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert!(loader.try_recv().is_none());
    }

    #[test]
    fn a_panicking_decoder_is_reported_and_the_worker_survives() {
        fn explode(path: &Path, rate: u32) -> Result<Source, MediaError> {
            if path.ends_with("bomb.wav") {
                panic!("boom");
            }
            load(path, rate)
        }
        let dir = TempDir::new("loader-panic");
        let good = dir.join("good.wav");
        write_wav(&good, 48000, 1, WavFormat::Float32, &[0.5; 100]);

        let loader = Loader::with_load_fn(|| {}, explode);
        // More bombs than workers, so a surviving worker has to handle the last request.
        for key in 0..MAX_WORKERS as u64 + 1 {
            loader.request(key, dir.join("bomb.wav"), 48000);
        }
        loader.request(100, good, 48000);

        let done = wait_for(&loader, MAX_WORKERS + 2);
        for loaded in &done[..MAX_WORKERS + 1] {
            let Err(MediaError::Decode(msg)) = &loaded.result else {
                panic!("expected a decode error, got {:?}", loaded.result);
            };
            assert!(msg.contains("boom"), "{msg}");
        }
        assert_eq!(done[MAX_WORKERS + 1].key, 100);
        assert!(done[MAX_WORKERS + 1].result.is_ok());
    }

    #[test]
    fn dropping_does_not_block() {
        let loader = Loader::new(|| {});
        loader.request(1, PathBuf::from("/nonexistent/file.wav"), 48000);
        drop(loader);
    }
}
