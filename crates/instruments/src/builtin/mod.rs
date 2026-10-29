use crate::BuiltInInstrumentDescriptor;

pub mod drums;
pub mod guitar;
mod oscillator;
mod poly_synth;
pub mod sampler;
pub mod slicer;
pub mod wavetable;

/// The instrument new MIDI tracks get.
pub const DEFAULT_INSTRUMENT: &str = poly_synth::DESCRIPTOR.id;

static BUILTINS: &[BuiltInInstrumentDescriptor] = &[
    poly_synth::DESCRIPTOR,
    wavetable::DESCRIPTOR,
    drums::DESCRIPTOR,
    sampler::DESCRIPTOR,
    guitar::DESCRIPTOR,
    oscillator::DESCRIPTOR,
    slicer::DESCRIPTOR,
];

/// Every built-in instrument, in the order the instrument picker lists them.
pub fn builtins() -> &'static [BuiltInInstrumentDescriptor] {
    BUILTINS
}

pub fn find_builtin(id: &str) -> Option<&'static BuiltInInstrumentDescriptor> {
    BUILTINS.iter().find(|d| d.id == id)
}

/// The modulators built-in instrument `id` exports (see
/// [`BuiltInInstrumentDescriptor::mod_outputs`]): one arm per instrument
/// that has any.
pub fn mod_outputs(id: &str) -> &'static [crate::ModOutput] {
    match id {
        "canopy.wavetable" => wavetable::MOD_OUTPUTS,
        _ => &[],
    }
}

/// Which voice a new note takes when every one is busy: the quietest of
/// those whose key is up (`released`), else the one started longest ago.
/// Takes `(index, voice)` pairs; `None` only when there are none.
pub(crate) fn voice_to_steal<'a, V: 'a>(
    voices: impl Iterator<Item = (usize, &'a V)> + Clone,
    released: impl Fn(&V) -> bool,
    level: impl Fn(&V) -> f32,
    started: impl Fn(&V) -> u64,
) -> Option<usize> {
    voices
        .clone()
        .filter(|(_, v)| released(v))
        .min_by(|(_, a), (_, b)| level(a).total_cmp(&level(b)))
        .or_else(|| voices.min_by_key(|(_, v)| started(v)))
        .map(|(index, _)| index)
}
