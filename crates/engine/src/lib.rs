//! The audio engine: everything that runs on the audio thread, plus the handle
//! the UI uses to talk to it and the compiler that builds what it plays.
//!
//! Threading model:
//! - UI → engine: [`Command`]s through a lock-free ring buffer, including
//!   whole [`Arrangement`] snapshots.
//! - engine → UI: replaced snapshots through a second ring buffer, to be
//!   dropped on the UI thread; meters and playhead through atomics in [`Shared`].
//!
//! Neither thread ever waits on the other. The engine itself has no idea a
//! sound card exists, so it can be driven by tests or an offline bounce just as
//! well as by [`device`].

mod arrangement;
mod audio;
mod bounce;
mod chain;
mod command;
mod compensation;
#[cfg(feature = "cpal")]
pub mod device;
mod fader;
mod graph;
mod handle;
mod live;
mod modulation;
mod pool;
mod shared;

use std::sync::Arc;

pub use arrangement::{Arrangement, AudioLookup, Compiler, RenderedClip, clip_process};
pub use audio::{Engine, MAX_BLOCK};
pub use bounce::Bounce;
pub use chain::TapFrame;
pub use command::Command;
pub use handle::EngineHandle;
pub use live::{LiveEvent, LiveSender, Played};
pub use pool::default_workers;
pub use shared::{PeakMeter, Shared, TrackMeters};

/// How many commands can be waiting before `EngineHandle::send` starts failing.
/// The audio thread drains the queue every block, so this only fills up if
/// audio has stalled.
const COMMAND_QUEUE_CAPACITY: usize = 1024;

/// Replaced snapshots waiting for the UI to drop them. The UI empties this
/// every frame; if it ever fills, the engine holds off on swapping until it
/// has room rather than free anything itself.
const GARBAGE_QUEUE_CAPACITY: usize = 64;

/// Create an engine and the handle that controls it. Move the [`Engine`] into
/// the audio callback; keep the [`EngineHandle`] on the UI thread. Tracks
/// render on [`default_workers`] threads besides the audio thread.
pub fn create(sample_rate: f32) -> (EngineHandle, Engine) {
    create_with_workers(sample_rate, default_workers())
}

/// [`create`] with `workers` threads rendering tracks alongside the audio
/// thread; 0 renders everything on the audio thread.
pub fn create_with_workers(sample_rate: f32, workers: usize) -> (EngineHandle, Engine) {
    let (commands_tx, commands_rx) = rtrb::RingBuffer::new(COMMAND_QUEUE_CAPACITY);
    let (garbage_tx, garbage_rx) = rtrb::RingBuffer::new(GARBAGE_QUEUE_CAPACITY);
    let (live_tx, live_rx) = rtrb::RingBuffer::new(live::LIVE_QUEUE_CAPACITY);
    let (played_tx, played_rx) = rtrb::RingBuffer::new(live::LIVE_QUEUE_CAPACITY);
    let shared = Arc::new(Shared::default());
    let live = LiveSender(Arc::new(std::sync::Mutex::new(live_tx)));
    let handle = EngineHandle::new(
        commands_tx,
        garbage_rx,
        Arc::clone(&shared),
        sample_rate,
        live,
        played_rx,
    );
    let engine = Engine::new(
        commands_rx,
        garbage_tx,
        shared,
        sample_rate,
        live_rx,
        played_tx,
        workers,
    );
    (handle, engine)
}
