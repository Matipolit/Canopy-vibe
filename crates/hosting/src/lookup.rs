//! The engine's view of the plugins: [`WithPlugins`] adds plugin effects
//! and instruments to any [`AudioLookup`], from the live instances
//! ([`PluginManager`]) or from ones made for an offline render
//! ([`Offline`]).

use std::sync::Arc;

use effects::Effect;
use engine::AudioLookup;
use instruments::{Instrument, InstrumentData};
use media::AudioBuffer;
use project::{AudioClip, Device, DeviceId, SourceId, Track, TrackId};

use crate::{Offline, Owner, PluginManager};

/// Where the compiler's plugins come from.
pub trait Plugins {
    fn effect(
        &self,
        device: &Device,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Effect>>;
    fn instrument(
        &self,
        track: &Track,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>>;
    fn latency(&self, owner: Owner) -> Option<usize>;
    fn generation_of(&self, owner: Owner) -> u64;
}

impl Plugins for PluginManager {
    fn effect(&self, device: &Device, rate: u32, max_block: usize) -> Option<Box<dyn Effect>> {
        PluginManager::effect(self, device, rate, max_block)
    }
    fn instrument(
        &self,
        track: &Track,
        rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        PluginManager::instrument(self, track, rate, max_block)
    }
    fn latency(&self, owner: Owner) -> Option<usize> {
        PluginManager::latency(self, owner)
    }
    fn generation_of(&self, owner: Owner) -> u64 {
        PluginManager::generation_of(self, owner)
    }
}

impl Plugins for Offline {
    fn effect(&self, device: &Device, rate: u32, max_block: usize) -> Option<Box<dyn Effect>> {
        Offline::effect(self, device, rate, max_block)
    }
    fn instrument(
        &self,
        track: &Track,
        rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        Offline::instrument(self, track, rate, max_block)
    }
    fn latency(&self, owner: Owner) -> Option<usize> {
        Offline::latency(self, owner)
    }
    fn generation_of(&self, owner: Owner) -> u64 {
        Offline::generation_of(self, owner)
    }
}

/// `audio`'s sources, renders and instrument data, and `plugins`' plugins.
pub struct WithPlugins<'a, L, P: ?Sized> {
    pub audio: L,
    pub plugins: &'a P,
}

impl<L: AudioLookup, P: Plugins + ?Sized> AudioLookup for WithPlugins<'_, L, P> {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.audio.source(id)
    }

    fn rendered(&self, clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        self.audio.rendered(clip)
    }

    fn instrument_data(&self, track: &Track) -> Option<InstrumentData> {
        self.audio.instrument_data(track)
    }

    fn plugin_effect(
        &self,
        device: &Device,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Effect>> {
        self.plugins.effect(device, sample_rate, max_block)
    }

    fn plugin_instrument(
        &self,
        track: &Track,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        self.plugins.instrument(track, sample_rate, max_block)
    }

    fn plugin_latency(&self, device: DeviceId) -> Option<usize> {
        self.plugins.latency(Owner::Effect(device))
    }

    fn plugin_instrument_latency(&self, track: TrackId) -> Option<usize> {
        self.plugins.latency(Owner::Instrument(track))
    }

    fn plugin_generation(&self, device: DeviceId) -> u64 {
        self.plugins.generation_of(Owner::Effect(device))
    }

    fn plugin_instrument_generation(&self, track: TrackId) -> u64 {
        self.plugins.generation_of(Owner::Instrument(track))
    }
}
