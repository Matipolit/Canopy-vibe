//! Playing from a MIDI keyboard: events from the MIDI driver's threads to
//! the audio thread, and what was played back to the UI, for recording.

use std::sync::{Arc, Mutex};

use midi::NoteEventKind;
use rtrb::Producer;

/// How many live events can wait for the audio thread.
pub(crate) const LIVE_QUEUE_CAPACITY: usize = 1024;

/// What a keyboard sends, on its way to the track it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEvent {
    Note(NoteEventKind),
    /// The sustain pedal: down or up. While it's down, released keys keep
    /// sounding until it comes up.
    Sustain(bool),
}

/// Sends live events to the audio thread from any thread. MIDI drivers
/// call back on threads of their own, one per device, so senders share one
/// queue behind a lock; only senders ever take it, never the audio thread.
#[derive(Clone)]
pub struct LiveSender(pub(crate) Arc<Mutex<Producer<LiveEvent>>>);

impl LiveSender {
    /// False if the event was dropped: the queue is full (audio has
    /// stalled) or poisoned.
    pub fn send(&self, event: LiveEvent) -> bool {
        self.0
            .lock()
            .is_ok_and(|mut queue| queue.push(event).is_ok())
    }
}

/// A note event the engine played from live input, after the sustain pedal
/// had its say. `sample` is where the playhead was, if it was rolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Played {
    pub sample: Option<u64>,
    pub kind: NoteEventKind,
}

/// The engine's side of live input, for the track it goes to.
pub(crate) struct LiveState {
    pub target: Option<project::TrackId>,
    /// Notes started on the target and not yet ended.
    pub sounding: [bool; 128],
    pub sustain: bool,
    /// Keys let go while the pedal is down, still sounding.
    pub sustained: [bool; 128],
    pub bent: bool,
}

impl LiveState {
    pub fn new() -> LiveState {
        LiveState {
            target: None,
            sounding: [false; 128],
            sustain: false,
            sustained: [false; 128],
            bent: false,
        }
    }
}
