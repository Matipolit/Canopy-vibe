//! Clip rendering on a worker thread, so stretching a long clip never stalls
//! the UI.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::{AudioBuffer, ClipProcess, process_clip};

/// A finished render, with the slot and tag it was requested with.
#[derive(Debug)]
pub struct Rendered {
    pub slot: u64,
    pub tag: u64,
    pub audio: Arc<AudioBuffer>,
}

struct Job {
    slot: u64,
    tag: u64,
    input: Arc<AudioBuffer>,
    spec: ClipProcess,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

pub struct Renderer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    results: Receiver<Rendered>,
}

impl Renderer {
    /// `wake` is called after each render finishes, so the UI can repaint.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Renderer {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (sender, results) = mpsc::channel();
        let worker_queue = queue.clone();
        thread::Builder::new()
            .name("clip renderer".into())
            .spawn(move || work(&worker_queue, &sender, &wake))
            .expect("spawning the clip renderer");
        Renderer { queue, results }
    }

    /// Queues a render. A queued, not yet started request for the same
    /// `slot` (one clip, say) is replaced, so dragging a control renders only
    /// the latest setting. `tag` comes back with the result, to tell which
    /// request it answers.
    pub fn request(&self, slot: u64, tag: u64, input: Arc<AudioBuffer>, spec: ClipProcess) {
        let (lock, ready) = &*self.queue;
        let Ok(mut queue) = lock.lock() else {
            return;
        };
        queue.jobs.retain(|job| job.slot != slot);
        queue.jobs.push_back(Job {
            slot,
            tag,
            input,
            spec,
        });
        ready.notify_one();
    }

    pub fn try_recv(&self) -> Option<Rendered> {
        self.results.try_recv().ok()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.closed = true;
            queue.jobs.clear();
        }
        ready.notify_all();
    }
}

fn work(
    queue: &(Mutex<Queue>, Condvar),
    results: &Sender<Rendered>,
    wake: &(dyn Fn() + Send + Sync),
) {
    let (lock, ready) = queue;
    loop {
        let job = {
            let Ok(mut queue) = lock.lock() else {
                return;
            };
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = match ready.wait(queue) {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
            }
        };
        let audio = Arc::new(process_clip(&job.input, &job.spec));
        let rendered = Rendered {
            slot: job.slot,
            tag: job.tag,
            audio,
        };
        if results.send(rendered).is_err() {
            return;
        }
        wake();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::StretchMode;

    #[test]
    fn renders_in_the_background_and_keeps_only_the_latest_request() {
        let input = Arc::new(AudioBuffer::new(48_000, vec![0.5; 1000], vec![0.5; 1000]));
        let renderer = Renderer::new(|| {});
        let spec = |out_len| ClipProcess {
            offset: 0,
            len: 1000,
            reversed: false,
            looped: false,
            phase: 0,
            play_len: 1000,
            mode: StretchMode::Resample,
            speed: 1.0,
            transpose: 0.0,
            out_len,
        };
        for out_len in [100, 200, 300] {
            renderer.request(7, out_len, input.clone(), spec(out_len));
        }
        let start = Instant::now();
        let mut last = None;
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(rendered) = renderer.try_recv() {
                assert_eq!(rendered.slot, 7);
                assert_eq!(rendered.tag, rendered.audio.frames() as u64);
                last = Some(rendered.audio.frames());
                if last == Some(300) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(last, Some(300), "the latest request wins");
    }
}
