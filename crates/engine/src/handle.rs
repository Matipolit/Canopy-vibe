use std::sync::Arc;

use rtrb::{Consumer, Producer};

use crate::{Arrangement, Command, LiveSender, Played, Shared};

/// The UI thread's end of the engine.
pub struct EngineHandle {
    commands: Producer<Command>,
    garbage: Consumer<Box<Arrangement>>,
    shared: Arc<Shared>,
    sample_rate: f32,
    live: LiveSender,
    played: Consumer<Played>,
}

impl EngineHandle {
    pub(crate) fn new(
        commands: Producer<Command>,
        garbage: Consumer<Box<Arrangement>>,
        shared: Arc<Shared>,
        sample_rate: f32,
        live: LiveSender,
        played: Consumer<Played>,
    ) -> Self {
        Self {
            commands,
            garbage,
            shared,
            sample_rate,
            live,
            played,
        }
    }

    /// For MIDI drivers to send what a keyboard plays straight to the audio
    /// thread. It goes to the track set with [`Command::SetLiveTarget`].
    pub fn live_sender(&self) -> LiveSender {
        self.live.clone()
    }

    /// The next note the engine played from live input, oldest first.
    pub fn pop_played(&mut self) -> Option<Played> {
        self.played.pop().ok()
    }

    /// Queue a command for the audio thread. Gives it back if the queue is
    /// full, which only happens when the audio thread has stalled.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands.push(command).map_err(|err| match err {
            rtrb::PushError::Full(command) => command,
        })
    }

    /// Drops snapshots the audio thread has replaced. Call it every frame:
    /// the engine won't swap in a new snapshot while this queue is full.
    pub fn collect_garbage(&mut self) {
        while let Ok(old) = self.garbage.pop() {
            drop(old);
        }
    }

    /// Peak level of each channel since the last call, as linear amplitude.
    pub fn take_peaks(&self) -> [f32; 2] {
        self.shared.take_peaks()
    }

    /// The busiest audio callback since the last call, as the share of its
    /// time it spent rendering.
    pub fn take_load(&self) -> f32 {
        self.shared.take_load()
    }

    pub fn playhead_samples(&self) -> u64 {
        self.shared.playhead()
    }

    pub fn playhead_seconds(&self) -> f64 {
        self.shared.playhead() as f64 / self.sample_rate as f64
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }
}
