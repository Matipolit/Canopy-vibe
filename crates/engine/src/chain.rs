//! Effect chains in the engine: built on the UI thread, carried across
//! snapshot swaps by device id (so a reverb's tail rings on through edits),
//! and run on the audio thread after a track's sound and before its fader.
//!
//! A snapshot holds a slot for every device of a chain, in the project's
//! order, whatever its routing: [`run`] plays them one after another (a
//! linear chain), and a graph (`graph.rs`) plays them in its own order
//! through [`run_slot`]. Plugins come from the [`AudioLookup`] rather than
//! their descriptors, and get a fresh instance when the lookup says their
//! generation changed (a plugin that restarted).
//!
//! Other state that must outlive a snapshot (a graph's filters and delay
//! lines, modulators) is kept in [`StateSlot`]s, adopted by key the same way
//! instances are adopted by device id.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use effects::{Context, Effect, EffectRef, MAX_METERS};
use project::{Device, DeviceId, TrackId};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::MAX_BLOCK;
use crate::arrangement::{Arrangement, AudioLookup};
use crate::pool::Sends;

/// One frame of a device's tap: the sound going in (left, right) and coming
/// out (left, right), so a display can compare them (an EQ's spectrum
/// before and after).
pub type TapFrame = [f32; 4];

/// How much tapped audio waits for the UI: about a third of a second at
/// 48 kHz, many frames' worth. When the UI falls behind, the newest audio
/// is dropped until it catches up.
const TAP_FRAMES: usize = 1 << 14;

/// How many blocks a device's meters and tap keep running after the UI
/// last read them: about a second and a half at 48 kHz.
const WATCH_BLOCKS: u32 = 1024;

/// Values the audio thread publishes for the UI: an effect's or instrument's
/// [`effects::Effect::meters`].
///
/// Only a device whose panel is showing has its meters read, so they (and
/// its tap) are only worked out while the UI keeps reading them: each read
/// keeps them going for [`WATCH_BLOCKS`] more blocks. A new device starts
/// watched.
pub struct Meters {
    values: [AtomicU32; MAX_METERS],
    /// Blocks left before the audio thread stops publishing.
    watched: AtomicU32,
}

impl Default for Meters {
    fn default() -> Self {
        Meters {
            values: std::array::from_fn(|_| AtomicU32::new(0)),
            watched: AtomicU32::new(WATCH_BLOCKS),
        }
    }
}

impl Meters {
    pub(crate) fn store(&self, values: &[f32; MAX_METERS]) {
        for (slot, value) in self.values.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// The latest values; keeps them coming.
    pub fn read(&self) -> [f32; MAX_METERS] {
        self.watch();
        std::array::from_fn(|i| f32::from_bits(self.values[i].load(Ordering::Relaxed)))
    }

    /// Keeps the meters and tap running. True if they had stopped, so what
    /// they hold is old.
    pub(crate) fn watch(&self) -> bool {
        self.watched.swap(WATCH_BLOCKS, Ordering::Relaxed) == 0
    }

    /// Audio thread, once a block: whether to publish, counting the block.
    /// A read that lands between the load and the store may be lost, which
    /// the next one makes up for.
    pub(crate) fn watched(&self) -> bool {
        let left = self.watched.load(Ordering::Relaxed);
        if left == 0 {
            return false;
        }
        self.watched.store(left - 1, Ordering::Relaxed);
        true
    }
}

/// One effect in a chain, as a snapshot has it.
pub(crate) struct EffectSlot {
    pub id: DeviceId,
    pub bypassed: bool,
    /// The effect isn't one this build has; the sound passes through.
    pub missing: bool,
    /// A plugin that has no instance for now (it's restarting): silent until
    /// a snapshot brings one.
    pub silent: bool,
    /// Parameter values from the project, by index.
    pub params: Box<[f32]>,
    /// The effect's meters, for the UI.
    pub meters: Arc<Meters>,
    /// The send bus ([`Arrangement::sends`]) the effect's sidechain reads.
    pub sidechain: Option<usize>,
    /// The sidechain reads the block before this one: its source couldn't
    /// render first without a loop (see `arrangement::schedule`).
    pub late: bool,
    /// `None` means "keep the instance this device already has": the engine
    /// moves it over from the previous snapshot when this one arrives.
    pub processor: Option<Box<EffectProcessor>>,
}

/// A running effect: the state that must survive snapshot swaps.
pub(crate) struct EffectProcessor {
    pub effect: Box<dyn Effect>,
    /// What each parameter is actually set to, which automation and live
    /// knob moves change without a new snapshot.
    pub values: Box<[f32]>,
    /// Whether it was bypassed last block, to reset it when it comes back
    /// rather than replay a stale tail.
    pub bypassed: bool,
    /// Where the effect's sound goes for the UI, if it wants it.
    tap: Option<Producer<TapFrame>>,
    /// What the effect was last told by [`Effect::set_metering`].
    metering: bool,
}

impl EffectProcessor {
    pub fn set_param(&mut self, index: usize, value: f32) {
        self.effect.set_param(index, value);
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value;
        }
    }
}

/// What the compiler remembers about each device the engine has an
/// instance of.
pub(crate) struct LiveDevice {
    effect: EffectRef,
    /// For a plugin, the lookup's generation of the instance the engine
    /// has ([`AudioLookup::plugin_generation`]); 0 for built-ins.
    generation: u64,
    /// The same meters every snapshot gets, so the UI can read them any time.
    pub meters: Arc<Meters>,
    /// The UI's end of the instance's tap.
    pub tap: Option<Consumer<TapFrame>>,
}

/// Turns a chain of devices into slots, creating (and preparing) instances
/// only for devices the engine doesn't have yet: built-ins from their
/// descriptors, plugins from `lookup`. `sends` says which send bus carries
/// each sidechain source.
pub(crate) fn build_chain<L: AudioLookup + ?Sized>(
    devices: &[Device],
    live: &mut HashMap<DeviceId, LiveDevice>,
    seen: &mut Vec<DeviceId>,
    sends: &HashMap<TrackId, usize>,
    sample_rate: u32,
    lookup: &L,
) -> Box<[EffectSlot]> {
    devices
        .iter()
        .map(|device| {
            seen.push(device.id);
            let params: Box<[f32]> = device.values().into();
            let descriptor = device.effect.descriptor();
            let plugin = !device.effect.format.is_builtin();
            let generation = if plugin {
                lookup.plugin_generation(device.id)
            } else {
                0
            };
            let existing = live
                .get(&device.id)
                .filter(|l| l.effect == device.effect && l.generation == generation);
            let meters = existing.map_or_else(Arc::default, |l| l.meters.clone());
            let mut silent = false;
            let processor = if existing.is_some() {
                None
            } else {
                let effect = match descriptor {
                    Some(_) if plugin => lookup.plugin_effect(device, sample_rate, MAX_BLOCK),
                    Some(d) => {
                        let mut effect = (d.create)();
                        effect.prepare(sample_rate as f32, MAX_BLOCK);
                        Some(effect)
                    }
                    None => None,
                };
                if descriptor.is_some() && effect.is_none() {
                    // A plugin that isn't ready: nothing remembered, so the
                    // next snapshot asks again.
                    silent = true;
                    live.remove(&device.id);
                }
                let (producer, consumer) = match descriptor {
                    Some(d) if d.tap && effect.is_some() => {
                        let (producer, consumer) = RingBuffer::new(TAP_FRAMES);
                        (Some(producer), Some(consumer))
                    }
                    _ => (None, None),
                };
                if !silent {
                    live.insert(
                        device.id,
                        LiveDevice {
                            effect: device.effect.clone(),
                            generation,
                            meters: meters.clone(),
                            tap: consumer,
                        },
                    );
                }
                effect.map(|mut effect| {
                    for (index, value) in params.iter().enumerate() {
                        effect.set_param(index, *value);
                    }
                    // What it reads in silence (levels at their floor, not
                    // at 0 dB) until the first block.
                    let mut idle = [0.0; MAX_METERS];
                    effect.meters(&mut idle);
                    meters.store(&idle);
                    Box::new(EffectProcessor {
                        effect,
                        values: params.clone(),
                        bypassed: device.bypassed,
                        tap: producer,
                        metering: true,
                    })
                })
            };
            let listens = descriptor.is_some_and(|d| d.sidechain);
            EffectSlot {
                id: device.id,
                bypassed: device.bypassed,
                missing: descriptor.is_none(),
                silent,
                params,
                meters,
                sidechain: device
                    .sidechain
                    .filter(|_| listens)
                    .and_then(|t| sends.get(&t).copied()),
                late: false,
                processor,
            }
        })
        .collect()
}

/// Takes the instance device `id` has in `old`, wherever it is.
fn take(old: &mut Arrangement, id: DeviceId) -> Option<Box<EffectProcessor>> {
    old.nodes
        .iter_mut()
        .flat_map(|n| n.effects.iter_mut())
        .chain(old.master_effects.iter_mut())
        .find(|s| s.id == id)
        .and_then(|s| s.processor.take())
}

/// Moves instances from `old` into the slots of `chain` that keep theirs,
/// bringing parameters up to date except where `automated(slot, param)`
/// (automation or modulation sets those).
pub(crate) fn adopt(
    chain: &mut [EffectSlot],
    old: &mut Arrangement,
    automated: impl Fn(usize, usize) -> bool,
) {
    for (index, slot) in chain.iter_mut().enumerate() {
        if slot.processor.is_some() || slot.missing || slot.silent {
            continue;
        }
        let Some(mut processor) = take(old, slot.id) else {
            continue;
        };
        for (param, value) in slot.params.iter().enumerate() {
            if !automated(index, param) && processor.values.get(param) != Some(value) {
                processor.set_param(param, *value);
            }
        }
        slot.processor = Some(processor);
    }
}

/// Retires every instance `chain` still holds, on the audio thread: the
/// snapshot that replaced this one didn't take them, so they're done.
pub(crate) fn retire(chain: &mut [EffectSlot]) {
    for slot in chain {
        if let Some(processor) = slot.processor.as_deref_mut() {
            processor.effect.retire();
        }
    }
}

/// The sidechain `slot` listens to, from the send buses, for `frames`.
pub(crate) fn sidechain<'a>(
    slot: &EffectSlot,
    sends: Sends<'a>,
    frames: usize,
) -> Option<(&'a [f32], &'a [f32])> {
    sends.read(slot.sidechain?, slot.late, frames)
}

/// Runs a chain over one block, in place, in order. `sends` holds the
/// sidechain sources' latest blocks.
pub(crate) fn run(
    chain: &mut [EffectSlot],
    left: &mut [f32],
    right: &mut [f32],
    context: &Context,
    sends: Sends<'_>,
) {
    let frames = left.len();
    for slot in chain {
        let context = Context {
            sidechain: sidechain(slot, sends, frames),
            ..*context
        };
        run_slot(slot, left, right, &context);
    }
}

/// Runs one effect over a block, in place: its bypass (resetting it when it
/// comes back, rather than replaying a stale tail), its meters and its tap.
/// `context.sidechain` is what it listens to.
pub(crate) fn run_slot(
    slot: &mut EffectSlot,
    left: &mut [f32],
    right: &mut [f32],
    context: &Context,
) {
    if slot.silent && !slot.bypassed {
        left.fill(0.0);
        right.fill(0.0);
        return;
    }
    let Some(processor) = slot.processor.as_deref_mut() else {
        return;
    };
    let frames = left.len();
    let watched = slot.meters.watched();
    if processor.metering != watched {
        processor.metering = watched;
        processor.effect.set_metering(watched);
    }
    let mut input = [[0.0f32; 2]; MAX_BLOCK];
    let mut meters = [0.0; MAX_METERS];
    let tapped = watched && processor.tap.is_some();
    if tapped {
        for (frame, (l, r)) in input.iter_mut().zip(left.iter().zip(right.iter())) {
            *frame = [*l, *r];
        }
    }
    if slot.bypassed {
        if !std::mem::replace(&mut processor.bypassed, true) {
            // Silenced, so its meters read what it reads in silence
            // (it's reset again when it comes back anyway).
            processor.effect.reset();
            processor.effect.meters(&mut meters);
            slot.meters.store(&meters);
        }
    } else {
        if std::mem::take(&mut processor.bypassed) {
            processor.effect.reset();
        }
        processor.effect.process(left, right, context);
        if watched {
            processor.effect.meters(&mut meters);
            slot.meters.store(&meters);
        }
    }
    if let Some(tap) = processor.tap.as_mut().filter(|_| tapped) {
        let n = frames.min(tap.slots());
        if let Ok(chunk) = tap.write_chunk_uninit(n) {
            let out = left.iter().zip(right.iter());
            chunk.fill_from_iter(
                input
                    .iter()
                    .zip(out)
                    .map(|(i, (l, r))| [i[0], i[1], *l, *r]),
            );
        }
    }
}

/// State that must outlive a snapshot, kept by key the way an effect's
/// instance is kept by device id: a graph's crossover filters by node, its
/// delay lines and gain ramps by cable, modulators by id. Built on the UI
/// thread for keys the engine hasn't got; `value: None` means "take the one
/// the previous snapshot has".
#[allow(dead_code)] // for graph and modulation state as they arrive
pub(crate) struct StateSlot<K, T> {
    pub key: K,
    pub value: Option<Box<T>>,
}

#[allow(dead_code)] // for graph and modulation state as they arrive
impl<K: PartialEq, T> StateSlot<K, T> {
    /// A slot with fresh state.
    pub fn new(key: K, value: T) -> StateSlot<K, T> {
        StateSlot {
            key,
            value: Some(Box::new(value)),
        }
    }

    /// A slot that keeps the state its key already has.
    pub fn kept(key: K) -> StateSlot<K, T> {
        StateSlot { key, value: None }
    }

    /// Takes the state of the slot with `key` out of `slots`, if it has any.
    pub fn take(slots: &mut [StateSlot<K, T>], key: &K) -> Option<Box<T>> {
        slots
            .iter_mut()
            .find(|s| s.key == *key)
            .and_then(|s| s.value.take())
    }

    /// Fills the slots of `slots` that keep their state with what `find`
    /// takes for their key (from the previous snapshot, by
    /// [`StateSlot::take`]). Pointer moves only: real-time safe.
    pub fn adopt(slots: &mut [StateSlot<K, T>], mut find: impl FnMut(&K) -> Option<Box<T>>) {
        for slot in slots {
            if slot.value.is_none() {
                slot.value = find(&slot.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_adopted_by_key_and_taken_once() {
        let mut old = vec![StateSlot::new(1, 10.0f32), StateSlot::new(2, 20.0)];
        let mut new = vec![
            StateSlot::kept(2),
            StateSlot::new(3, 30.0),
            StateSlot::kept(4),
        ];
        StateSlot::adopt(&mut new, |key| StateSlot::take(&mut old, key));
        assert_eq!(new[0].value.as_deref(), Some(&20.0), "moved over");
        assert_eq!(new[1].value.as_deref(), Some(&30.0), "its own");
        assert!(new[2].value.is_none(), "nothing to adopt");
        assert!(old[1].value.is_none(), "taken, not copied");
        assert_eq!(old[0].value.as_deref(), Some(&10.0), "left to be dropped");
    }
}
