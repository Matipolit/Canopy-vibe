//! Common factory metadata and an effect adapter for the existing audio bridge.

use effects::{Context, Effect, EffectDescriptor};
use instruments::{BuiltInInstrumentDescriptor, Instrument, MAX_METERS, ParamDescriptor};
use midi::NoteEvent;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

#[derive(Clone, Copy)]
pub enum Device {
    Instrument(&'static BuiltInInstrumentDescriptor),
    Effect(&'static EffectDescriptor),
}

impl Device {
    pub fn id(self) -> &'static str {
        match self {
            Self::Instrument(d) => d.id,
            Self::Effect(d) => d.id,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Instrument(d) => d.name,
            Self::Effect(d) => d.name,
        }
    }
    pub fn version(self) -> &'static str {
        match self {
            Self::Instrument(d) => d.version,
            Self::Effect(d) => d.version,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Instrument(d) => d.description,
            Self::Effect(d) => d.description,
        }
    }
    pub fn vendor(self) -> &'static str {
        match self {
            Self::Instrument(d) => d.author,
            Self::Effect(_) => "canopy",
        }
    }
    pub fn params(self) -> &'static [ParamDescriptor] {
        match self {
            Self::Instrument(d) => d.params,
            Self::Effect(d) => d.params,
        }
    }
    pub fn is_effect(self) -> bool {
        matches!(self, Self::Effect(_))
    }
    pub fn groups(self, index: usize) -> [&'static str; 2] {
        match self {
            Self::Instrument(d) => (d.groups)(index),
            Self::Effect(d) => [
                match d
                    .params
                    .get(index)
                    .map(|p| p.id.split('_').next().unwrap_or(""))
                {
                    Some("vol") => "Volume",
                    Some("flt") => "Filter",
                    Some("pan") => "Pan",
                    Some("tim") => "Time",
                    _ => "Output",
                },
                "",
            ],
        }
    }
    pub fn create(self, display: Arc<Display>) -> Box<dyn Instrument> {
        match self {
            Self::Instrument(d) => (d.create)(),
            Self::Effect(d) => Box::new(EffectAdapter {
                effect: (d.create)(),
                display,
                bpm: 120.0,
                beat: 0.0,
                playing: false,
            }),
        }
    }
}

/// Audio-thread meters read by the editor without touching the processor.
pub struct Display {
    pub visible: AtomicBool,
    pub playing: AtomicBool,
    pub meters: [AtomicU32; MAX_METERS],
}
impl Default for Display {
    fn default() -> Self {
        Self {
            visible: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            meters: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

struct EffectAdapter {
    effect: Box<dyn Effect>,
    display: Arc<Display>,
    bpm: f64,
    beat: f64,
    playing: bool,
}
impl Instrument for EffectAdapter {
    fn prepare(&mut self, rate: f32, block: usize) {
        self.effect.prepare(rate, block);
    }
    fn reset(&mut self) {
        self.effect.reset();
    }
    fn params(&self) -> &'static [ParamDescriptor] {
        self.effect.params()
    }
    fn set_param(&mut self, index: usize, value: f32) {
        self.effect.set_param(index, value);
    }
    fn modulate(&mut self, index: usize, value: f32) {
        self.effect.modulate(index, value);
    }
    fn set_tempo(&mut self, bpm: f64) {
        self.bpm = bpm;
    }
    fn set_position(&mut self, beat: f64, playing: bool) {
        self.beat = beat;
        self.playing = playing;
    }
    fn process(&mut self, _notes: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        let metering = self.display.visible.load(Ordering::Relaxed);
        self.effect.process(
            left,
            right,
            &Context {
                bpm: self.bpm,
                beat: self.beat,
                playing: self.playing,
                sidechain: None,
                metering,
            },
        );
        self.display.playing.store(self.playing, Ordering::Relaxed);
        if metering {
            let mut meters = [0.0; MAX_METERS];
            self.effect.meters(&mut meters);
            for (slot, value) in self.display.meters.iter().zip(meters) {
                slot.store(value.to_bits(), Ordering::Relaxed);
            }
        }
    }
}
