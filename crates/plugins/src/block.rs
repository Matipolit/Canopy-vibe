//! Plumbing the formats' processors share: the audio buffers a processor
//! owns, walking one block's events, the notes a plugin is holding, the
//! modulation the host emulates, and the queues between the audio and main
//! threads. Nothing here allocates after it's built.

use crate::{Event, ParamId};

/// How many parameter changes the main thread can queue for the audio
/// thread between blocks (host edits, a VST3 editor's knob moves).
pub(crate) const TO_AUDIO_CAPACITY: usize = 4096;
/// How many parameter changes and gestures the audio thread can queue
/// for the main thread between idles.
pub(crate) const FROM_AUDIO_CAPACITY: usize = 4096;
/// How many events one block can carry to a plugin. More are dropped.
pub(crate) const EVENT_CAPACITY: usize = 4096;
/// How many events one block can carry out of a plugin.
pub(crate) const OUT_EVENT_CAPACITY: usize = 1024;

/// A parameter change the main thread sends the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Change {
    pub id: ParamId,
    pub value: f64,
}

/// What the audio thread tells the main thread a plugin did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutEvent {
    Param(ParamId, f64),
    Gesture(ParamId, bool),
}

/// The main-thread ends of the queues between an active plugin's halves.
pub(crate) struct Queues {
    pub to_audio: rtrb::Producer<Change>,
    pub from_audio: rtrb::Consumer<OutEvent>,
}

/// The audio-thread ends.
pub(crate) struct AudioQueues {
    pub from_main: rtrb::Consumer<Change>,
    pub to_main: rtrb::Producer<OutEvent>,
}

pub(crate) fn queues() -> (Queues, AudioQueues) {
    let (to_audio, from_main) = rtrb::RingBuffer::new(TO_AUDIO_CAPACITY);
    let (to_main, from_audio) = rtrb::RingBuffer::new(FROM_AUDIO_CAPACITY);
    (
        Queues {
            to_audio,
            from_audio,
        },
        AudioQueues { from_main, to_main },
    )
}

/// Audio buffers for a plugin's ports in one direction: `max_block`
/// samples per channel, and the channel pointer arrays the formats want.
pub(crate) struct PortBuffers {
    samples: Vec<Vec<f32>>,
    /// Per port: its first channel's index in `samples`, and how many.
    ports: Vec<(usize, usize)>,
    /// One per channel, pointing into `samples`, which never reallocate.
    pointers: Vec<*mut f32>,
}

// SAFETY: the pointers only point into `samples`, which moves with them.
unsafe impl Send for PortBuffers {}

impl PortBuffers {
    pub fn new(channel_counts: &[u32], max_block: usize) -> PortBuffers {
        let mut samples = Vec::new();
        let mut ports = Vec::new();
        for &count in channel_counts {
            ports.push((samples.len(), count as usize));
            for _ in 0..count {
                samples.push(vec![0.0f32; max_block]);
            }
        }
        let pointers = samples.iter_mut().map(|c| c.as_mut_ptr()).collect();
        PortBuffers {
            samples,
            ports,
            pointers,
        }
    }

    pub fn port_count(&self) -> usize {
        self.ports.len()
    }

    pub fn channel_count(&self, port: usize) -> usize {
        self.ports.get(port).map_or(0, |p| p.1)
    }

    pub fn channel(&self, port: usize, channel: usize) -> Option<&[f32]> {
        let (first, count) = *self.ports.get(port)?;
        (channel < count).then(|| self.samples[first + channel].as_slice())
    }

    pub fn channel_mut(&mut self, port: usize, channel: usize) -> Option<&mut [f32]> {
        let (first, count) = *self.ports.get(port)?;
        (channel < count).then(|| self.samples[first + channel].as_mut_slice())
    }

    /// The channel pointer array of `port`, as the formats take it.
    pub fn pointers(&mut self, port: usize) -> *mut *mut f32 {
        match self.ports.get(port) {
            Some(&(first, count)) if count > 0 => self.pointers[first..].as_mut_ptr(),
            _ => std::ptr::null_mut(),
        }
    }

    /// Zeroes the first `frames` samples of every channel.
    pub fn clear(&mut self, frames: usize) {
        for channel in &mut self.samples {
            let n = frames.min(channel.len());
            channel[..n].fill(0.0);
        }
    }

    /// Fills `frames` samples of `port` from `sources[..][from..]`: one
    /// source goes to every channel, two or more to as many channels as
    /// match (the rest silent); a one-channel port gets the first two mixed.
    pub fn write_from(&mut self, port: usize, sources: &[&[f32]], from: usize, frames: usize) {
        let channels = self.channel_count(port);
        let source = |c: usize| -> &[f32] { sources[c].get(from..from + frames).unwrap_or(&[]) };
        for ch in 0..channels {
            let dest = &mut self.channel_mut(port, ch).unwrap()[..frames];
            match sources.len() {
                0 => dest.fill(0.0),
                1 => copy(dest, source(0)),
                _ if channels == 1 => {
                    for (i, d) in dest.iter_mut().enumerate() {
                        let a = source(0).get(i).copied().unwrap_or(0.0);
                        let b = source(1).get(i).copied().unwrap_or(0.0);
                        *d = 0.5 * (a + b);
                    }
                }
                n if ch < n => copy(dest, source(ch)),
                _ => dest.fill(0.0),
            }
        }
    }

    /// Copies `frames` samples of `port` into `dests[..][to..]`: a
    /// one-channel port to every destination, otherwise channel by channel
    /// (extra destinations silent); one destination gets the first two
    /// channels mixed. No such port: silence.
    pub fn read_into(&self, port: usize, dests: &mut [&mut [f32]], to: usize, frames: usize) {
        let channels = self.channel_count(port);
        let count = dests.len();
        for (d, dest) in dests.iter_mut().enumerate() {
            let Some(dest) = dest.get_mut(to..to + frames) else {
                continue;
            };
            match channels {
                0 => dest.fill(0.0),
                1 => copy(dest, self.channel(port, 0).unwrap()),
                _ if count == 1 => {
                    let (a, b) = (
                        self.channel(port, 0).unwrap(),
                        self.channel(port, 1).unwrap(),
                    );
                    for (i, s) in dest.iter_mut().enumerate() {
                        *s = 0.5 * (a[i] + b[i]);
                    }
                }
                n if d < n => copy(dest, self.channel(port, d).unwrap()),
                _ => dest.fill(0.0),
            }
        }
    }
}

/// Copies what fits, silencing the rest of `dest`.
fn copy(dest: &mut [f32], source: &[f32]) {
    let n = dest.len().min(source.len());
    dest[..n].copy_from_slice(&source[..n]);
    dest[n..].fill(0.0);
}

/// Calls `push` with every event for frames `offset..offset + frames` of a
/// block, rebased so `offset` is 0: first the changes the main thread
/// queued (as parameter events at 0), then `events` in order. Queued
/// changes stay queued once `push` refuses one.
pub(crate) fn for_each_event(
    queued: &mut rtrb::Consumer<Change>,
    events: &[Event],
    offset: u32,
    frames: u32,
    mut push: impl FnMut(Event) -> bool,
) {
    while let Ok(change) = queued.peek().copied() {
        let event = Event::Param {
            frame: 0,
            id: change.id,
            value: change.value,
        };
        if !push(event) {
            break;
        }
        let _ = queued.pop();
    }
    let end = offset.saturating_add(frames);
    let from = events.partition_point(|e| e.frame() < offset);
    let to = events.partition_point(|e| e.frame() < end).max(from);
    let last = frames.saturating_sub(1);
    for event in &events[from..to] {
        push(event.at((event.frame() - offset).min(last)));
    }
}

/// Which notes a plugin has been told are down, per channel, so
/// all-notes-off can end exactly those.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HeldNotes([[u64; 2]; 16]);

impl HeldNotes {
    pub fn press(&mut self, channel: u8, key: u8) {
        self.0[usize::from(channel & 15)][usize::from(key >> 6 & 1)] |= 1 << (key & 63);
    }

    pub fn release(&mut self, channel: u8, key: u8) {
        self.0[usize::from(channel & 15)][usize::from(key >> 6 & 1)] &= !(1 << (key & 63));
    }

    /// Forgets every held note, calling `release(channel, key)` for each.
    pub fn release_all(&mut self, mut release: impl FnMut(u8, u8)) {
        for (channel, words) in self.0.iter_mut().enumerate() {
            for (word, bits) in words.iter_mut().enumerate() {
                while *bits != 0 {
                    let bit = bits.trailing_zeros();
                    *bits &= *bits - 1;
                    release(channel as u8, (word as u32 * 64 + bit) as u8);
                }
            }
        }
    }
}

/// A note event as MIDI 1.0 bytes.
pub(crate) fn note_to_midi(event: Event) -> Option<[u8; 3]> {
    let velocity = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
    match event {
        Event::NoteOn {
            channel,
            key,
            velocity: v,
            ..
        } => Some([0x90 | (channel & 15), key & 127, velocity(v).max(1)]),
        Event::NoteOff {
            channel,
            key,
            velocity: v,
            ..
        } => Some([0x80 | (channel & 15), key & 127, velocity(v)]),
        _ => None,
    }
}

/// A MIDI note on or off as a note event at `frame`.
pub(crate) fn midi_to_note(frame: u32, data: [u8; 3]) -> Option<Event> {
    let (channel, key) = (data[0] & 15, data[1] & 127);
    let velocity = f32::from(data[2] & 127) / 127.0;
    match data[0] & 0xF0 {
        0x90 if data[2] > 0 => Some(Event::NoteOn {
            frame,
            channel,
            key,
            velocity,
        }),
        0x80 | 0x90 => Some(Event::NoteOff {
            frame,
            channel,
            key,
            velocity,
        }),
        _ => None,
    }
}

/// Events a plugin sent out during one block, kept without allocating.
pub(crate) struct OutEvents {
    events: Vec<Event>,
}

impl OutEvents {
    pub fn new() -> OutEvents {
        OutEvents {
            events: Vec::with_capacity(OUT_EVENT_CAPACITY),
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Adds an event; false if the block is full.
    pub fn push(&mut self, event: Event) -> bool {
        if self.events.len() == self.events.capacity() {
            return false;
        }
        self.events.push(event);
        true
    }

    /// Sorts them by frame, since plugins may send them out of order.
    pub fn sort(&mut self) {
        // Insertion sort: stable, allocation-free, and the list is nearly
        // always sorted already.
        for i in 1..self.events.len() {
            let mut j = i;
            while j > 0 && self.events[j - 1].frame() > self.events[j].frame() {
                self.events.swap(j - 1, j);
                j -= 1;
            }
        }
    }

    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }
}

/// Each parameter's last value and modulation, for parameters the plugin
/// can't modulate itself: the processor sends their sum as a value.
pub(crate) struct Modulation {
    /// Sorted by id.
    params: Vec<Modulated>,
}

struct Modulated {
    id: u32,
    base: f64,
    amount: f64,
    min: f64,
    max: f64,
    /// The plugin modulates it itself (CLAP `PARAM_MOD`).
    native: bool,
}

/// What to send the plugin for a parameter event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Apply {
    Value(f64),
    Mod(f64),
}

impl Modulation {
    /// `params`: id, current value, range, and whether the plugin
    /// modulates it itself.
    pub fn new(params: impl IntoIterator<Item = (u32, f64, f64, f64, bool)>) -> Modulation {
        let mut params: Vec<Modulated> = params
            .into_iter()
            .map(|(id, base, min, max, native)| Modulated {
                id,
                base,
                amount: 0.0,
                min: min.min(max),
                max: max.max(min),
                native,
            })
            .collect();
        params.sort_unstable_by_key(|p| p.id);
        Modulation { params }
    }

    fn find(&mut self, id: ParamId) -> Option<&mut Modulated> {
        let index = self.params.binary_search_by_key(&id.0, |p| p.id).ok()?;
        Some(&mut self.params[index])
    }

    /// A new value: what to send (the value, plus any emulated modulation).
    pub fn set(&mut self, id: ParamId, value: f64) -> f64 {
        match self.find(id) {
            Some(p) => {
                p.base = value;
                if p.native || p.amount == 0.0 {
                    value
                } else {
                    (value + p.amount).clamp(p.min, p.max)
                }
            }
            None => value,
        }
    }

    /// A new modulation amount: what to send.
    pub fn modulate(&mut self, id: ParamId, amount: f64) -> Apply {
        match self.find(id) {
            Some(p) if !p.native => {
                p.amount = amount;
                Apply::Value((p.base + amount).clamp(p.min, p.max))
            }
            _ => Apply::Mod(amount),
        }
    }

    /// The plugin changed a value itself (its editor): the new base.
    pub fn plugin_set(&mut self, id: ParamId, value: f64) {
        if let Some(p) = self.find(id) {
            p.base = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(frame: u32, key: u8) -> Event {
        Event::NoteOn {
            frame,
            channel: 0,
            key,
            velocity: 1.0,
        }
    }

    fn param(frame: u32, id: u32) -> Event {
        Event::Param {
            frame,
            id: ParamId(id),
            value: f64::from(id),
        }
    }

    #[test]
    fn events_in_range_are_rebased_after_queued_changes() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(4);
        producer
            .push(Change {
                id: ParamId(9),
                value: 9.0,
            })
            .unwrap();
        let events = [
            on(0, 60),
            param(10, 1),
            on(10, 62),
            on(20, 64),
            param(25, 2),
            on(30, 65),
        ];
        let mut seen = Vec::new();
        for_each_event(&mut consumer, &events, 10, 20, |event| {
            seen.push(match event {
                Event::NoteOn { frame, key, .. } => (frame, u32::from(key)),
                Event::Param { frame, id, .. } => (frame, id.0),
                _ => unreachable!(),
            });
            true
        });
        assert_eq!(seen, vec![(0, 9), (0, 1), (0, 62), (10, 64), (15, 2)]);
        assert!(consumer.is_empty());
    }

    #[test]
    fn refused_queued_changes_wait_for_the_next_block() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(4);
        for id in [1, 2] {
            producer
                .push(Change {
                    id: ParamId(id),
                    value: 0.0,
                })
                .unwrap();
        }
        let mut taken = 0;
        for_each_event(&mut consumer, &[], 0, 64, |_| {
            taken += 1;
            taken < 2
        });
        assert_eq!(consumer.slots(), 1, "the refused change stays queued");
    }

    #[test]
    fn held_notes_release_once_per_channel() {
        let mut held = HeldNotes::default();
        for (channel, key) in [(0, 0), (0, 60), (0, 63), (3, 64), (15, 127)] {
            held.press(channel, key);
        }
        held.release(0, 63);
        let mut released = Vec::new();
        held.release_all(|c, k| released.push((c, k)));
        assert_eq!(released, vec![(0, 0), (0, 60), (3, 64), (15, 127)]);
        held.release_all(|_, _| panic!("nothing left"));
    }

    #[test]
    fn midi_notes_round_trip() {
        let on = Event::NoteOn {
            frame: 3,
            channel: 2,
            key: 61,
            velocity: 100.0 / 127.0,
        };
        assert_eq!(midi_to_note(3, note_to_midi(on).unwrap()), Some(on));
        let zero = midi_to_note(0, [0x90, 60, 0]).unwrap();
        assert!(
            matches!(zero, Event::NoteOff { key: 60, .. }),
            "velocity 0 ends a note"
        );
        assert_eq!(midi_to_note(0, [0xB0, 1, 2]), None);
    }

    #[test]
    fn channels_map_between_layouts() {
        let mut buffers = PortBuffers::new(&[1, 2], 8);
        let (left, right) = ([1.0; 4], [3.0; 4]);
        buffers.write_from(0, &[&left, &right], 0, 4);
        buffers.write_from(1, &[&left], 0, 4);
        assert_eq!(
            buffers.channel(0, 0).unwrap()[..4],
            [2.0; 4],
            "mixed to mono"
        );
        assert_eq!(
            buffers.channel(1, 1).unwrap()[..4],
            [1.0; 4],
            "mono to both"
        );
        let (mut l, mut r) = ([0.0; 6], [0.0; 6]);
        buffers.read_into(0, &mut [&mut l, &mut r], 2, 4);
        assert_eq!(
            (l, r),
            (
                [0.0, 0.0, 2.0, 2.0, 2.0, 2.0],
                [0.0, 0.0, 2.0, 2.0, 2.0, 2.0]
            )
        );
        buffers.read_into(5, &mut [&mut l], 0, 6);
        assert_eq!(l, [0.0; 6]);
        assert!(buffers.pointers(2).is_null());
    }

    #[test]
    fn emulated_modulation_adds_to_the_last_value() {
        let mut modulation =
            Modulation::new([(1, 0.5, 0.0, 1.0, false), (2, 0.0, -1.0, 1.0, true)]);
        assert_eq!(modulation.modulate(ParamId(1), 0.25), Apply::Value(0.75));
        assert_eq!(modulation.set(ParamId(1), 0.9), 1.0, "clamped to the range");
        assert_eq!(modulation.modulate(ParamId(1), 0.0), Apply::Value(0.9));
        assert_eq!(modulation.modulate(ParamId(2), 0.5), Apply::Mod(0.5));
        assert_eq!(modulation.modulate(ParamId(7), 0.5), Apply::Mod(0.5));
    }
}
