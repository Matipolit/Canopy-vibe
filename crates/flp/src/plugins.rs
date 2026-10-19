//! Which canopy built-in stands in for an FL plugin, by the plugin's name.
//! Exact names first, then words in the name; anything else gets a guess
//! and is marked unrecognized so the user looks at it.

/// A built-in the user can pick, the plugin itself, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Choice {
    /// A built-in instrument or effect, by id (`"canopy.reverb"`).
    Builtin(&'static str),
    /// The third-party plugin FL used, hosted, with its saved state, where
    /// it's installed (`Options::installed`); elsewhere the built-in the
    /// name suggests stands in.
    Plugin,
    /// Leave it out.
    Skip,
}

impl Choice {
    /// What the menu shows.
    pub fn label(self) -> &'static str {
        match self {
            Choice::Builtin(id) => instruments::find_builtin(id)
                .map(|d| d.name)
                .or_else(|| effects::find_builtin(id).map(|d| d.name))
                .unwrap_or(id),
            Choice::Plugin => "The plugin itself",
            Choice::Skip => "Skip",
        }
    }
}

pub const POLY_SYNTH: &str = "canopy.poly_synth";
pub const WAVETABLE: &str = "canopy.wavetable";
pub const DRUMS: &str = "canopy.drums";
pub const SAMPLER: &str = "canopy.sampler";
pub const GUITAR: &str = "canopy.guitar";

pub const EQ: &str = "canopy.eq";
pub const COMPRESSOR: &str = "canopy.compressor";
pub const LIMITER: &str = "canopy.limiter";
pub const CLIPPER: &str = "canopy.clipper";
pub const MULTIBAND: &str = "canopy.multiband";
pub const DUCKER: &str = "canopy.ducker";
pub const WAVESHAPER: &str = "canopy.waveshaper";
pub const SHAPER: &str = "canopy.shaper";
pub const VOCODER: &str = "canopy.vocoder";
pub const REVERB: &str = "canopy.reverb";
pub const DELAY: &str = "canopy.delay";
pub const CHORUS: &str = "canopy.chorus";
pub const UTILITY: &str = "canopy.utility";
pub const TRANSIENT: &str = "canopy.transient";
pub const ANALYZER: &str = "canopy.analyzer";
pub const FREQUENCY_SHIFTER: &str = "canopy.frequency_shifter";

/// The name FL's Sampler goes by in the plan.
pub const FL_SAMPLER: &str = "Sampler";

/// Every instrument the menu offers, in the instrument picker's order.
pub fn instrument_choices() -> Vec<Choice> {
    instruments::builtins()
        .iter()
        .map(|d| Choice::Builtin(d.id))
        .chain([Choice::Skip])
        .collect()
}

/// Every effect the menu offers.
pub fn effect_choices() -> Vec<Choice> {
    effects::builtins()
        .iter()
        .map(|d| Choice::Builtin(d.id))
        .chain([Choice::Skip])
        .collect()
}

/// A suggestion, and whether it came from a rule rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suggestion {
    pub choice: Choice,
    pub recognized: bool,
}

const fn rule(choice: Choice) -> Suggestion {
    Suggestion {
        choice,
        recognized: true,
    }
}

/// Generators that make no sound of their own.
const SILENT_GENERATORS: &[&str] = &[
    "midi out",
    "fruity envelope controller",
    "fruity keyboard controller",
    "fruity dance",
    "control surface",
    "fruity formula controller",
];

const INSTRUMENTS: &[(&str, &str)] = &[
    // FL's own subtractive and FM synths.
    ("3x osc", POLY_SYNTH),
    ("3xosc", POLY_SYNTH),
    ("boobass", POLY_SYNTH),
    ("simsynth", POLY_SYNTH),
    ("simsynth live", POLY_SYNTH),
    ("ts404", POLY_SYNTH),
    ("poizone", POLY_SYNTH),
    ("toxic biohazard", POLY_SYNTH),
    ("wasp", POLY_SYNTH),
    ("wasp xt", POLY_SYNTH),
    ("autogun", POLY_SYNTH),
    ("fruity dx10", POLY_SYNTH),
    ("transistor bass", POLY_SYNTH),
    ("minisynth", POLY_SYNTH),
    ("synth1", POLY_SYNTH),
    ("synth1 vst", POLY_SYNTH),
    ("sylenth1", POLY_SYNTH),
    ("spire", POLY_SYNTH),
    ("diva", POLY_SYNTH),
    ("repro-1", POLY_SYNTH),
    ("repro-5", POLY_SYNTH),
    ("hive", POLY_SYNTH),
    ("hive 2", POLY_SYNTH),
    ("dune", POLY_SYNTH),
    ("dune 3", POLY_SYNTH),
    ("obxd", POLY_SYNTH),
    ("ob-xd", POLY_SYNTH),
    ("tal-u-no-lx", POLY_SYNTH),
    ("nexus", POLY_SYNTH),
    ("helm", POLY_SYNTH),
    ("odin 2", POLY_SYNTH),
    ("dexed", POLY_SYNTH),
    ("fl keys", POLY_SYNTH),
    ("dsk choirz", POLY_SYNTH),
    ("bbc symphony orchestra", POLY_SYNTH),
    ("bbc symphony orchestra (64 bit)", POLY_SYNTH),
    ("labs", POLY_SYNTH),
    ("splice instrument", POLY_SYNTH),
    ("keyscape", POLY_SYNTH),
    ("pianoteq", POLY_SYNTH),
    ("pianoteq 8", POLY_SYNTH),
    ("analog lab", POLY_SYNTH),
    ("analog lab v", POLY_SYNTH),
    ("mini v", POLY_SYNTH),
    ("u-he diva", POLY_SYNTH),
    ("tyrell n6", POLY_SYNTH),
    ("podolski", POLY_SYNTH),
    ("4front piano", POLY_SYNTH),
    ("4front piano module", POLY_SYNTH),
    // Wavetable, additive and modular synths.
    ("sytrus", WAVETABLE),
    ("flex", WAVETABLE),
    ("harmor", WAVETABLE),
    ("harmless", WAVETABLE),
    ("serum", WAVETABLE),
    ("serum 2", WAVETABLE),
    ("serumfx", WAVETABLE),
    ("vital", WAVETABLE),
    ("massive", WAVETABLE),
    ("massive x", WAVETABLE),
    ("pigments", WAVETABLE),
    ("phase plant", WAVETABLE),
    ("surge", WAVETABLE),
    ("surge xt", WAVETABLE),
    ("ana 2", WAVETABLE),
    ("morphine", WAVETABLE),
    ("ogun", WAVETABLE),
    ("omnisphere", WAVETABLE),
    ("zebra2", WAVETABLE),
    ("zebra", WAVETABLE),
    ("kepler", WAVETABLE),
    ("transient", WAVETABLE),
    // Drums.
    ("fpc", DRUMS),
    ("fruit kick", DRUMS),
    ("fruity kick", DRUMS),
    ("drumaxx", DRUMS),
    ("drumpad", DRUMS),
    ("fruity drumsynth live", DRUMS),
    ("drumsynth live", DRUMS),
    ("kick", DRUMS),
    ("kick 2", DRUMS),
    ("kick 3", DRUMS),
    ("battery", DRUMS),
    ("battery 4", DRUMS),
    ("addictive drums 2", DRUMS),
    ("superior drummer 3", DRUMS),
    ("ezdrummer", DRUMS),
    ("ezdrummer 3", DRUMS),
    ("microtonic", DRUMS),
    // Samplers.
    ("sampler", SAMPLER),
    ("directwave", SAMPLER),
    ("sforzando", SAMPLER),
    ("kontakt", SAMPLER),
    ("kontakt 7", SAMPLER),
    ("kontakt 8", SAMPLER),
    ("slicex", SAMPLER),
    ("fruity slicer", SAMPLER),
    ("fruity granulizer", SAMPLER),
    ("decentsampler", SAMPLER),
    ("tx16wx", SAMPLER),
    ("wave traveller", SAMPLER),
    // Strings that are plucked.
    ("plucked!", GUITAR),
    ("sakura", GUITAR),
    ("electri6ity", GUITAR),
    ("fl slayer", GUITAR),
    ("slayer", GUITAR),
    ("slayer 2", GUITAR),
];

/// Words that give an instrument away when its name isn't listed.
const INSTRUMENT_WORDS: &[(&str, &str)] = &[
    ("guitar", GUITAR),
    ("strum", GUITAR),
    ("pluck", GUITAR),
    ("shreddage", GUITAR),
    ("ample ", GUITAR),
    ("drum", DRUMS),
    ("kick", DRUMS),
    ("808", DRUMS),
    ("sfz", SAMPLER),
    ("sampler", SAMPLER),
    ("kontakt", SAMPLER),
    ("wavetable", WAVETABLE),
    ("serum", WAVETABLE),
];

/// What an FL generator becomes. `name` is the plugin's own name: a VST's,
/// or FL's for its native ones ("Sampler" for the Sampler).
pub fn suggest_instrument(name: &str) -> Suggestion {
    let key = normalize(name);
    if SILENT_GENERATORS.contains(&key.as_str()) {
        return rule(Choice::Skip);
    }
    if let Some((_, id)) = INSTRUMENTS.iter().find(|(n, _)| *n == key) {
        return rule(Choice::Builtin(id));
    }
    // At the start of a word: "strum" is in "instrument".
    if let Some((_, id)) = INSTRUMENT_WORDS
        .iter()
        .find(|(w, _)| contains_word(&key, w))
    {
        return rule(Choice::Builtin(id));
    }
    Suggestion {
        choice: Choice::Builtin(POLY_SYNTH),
        recognized: false,
    }
}

/// Effects with nothing to do in canopy (notes, sends, controllers,
/// visuals) or nothing like them there.
const SKIPPED_EFFECTS: &[&str] = &[
    "fruity notebook",
    "fruity notebook 2",
    "fruity send",
    "control surface",
    "zgameeditor visualizer",
    "fruity big clock",
    "fruity html notebook",
    "fruity formula controller",
    "fruity x-y controller",
    "fruity mute 2",
    "edison",
    "patcher",
    // Pitch tools: nothing in canopy does that.
    "newtone",
    "pitcher",
    "mautopitch",
    "graillon 2",
    "fruity pitch shifter",
    "khs ring mod",
];

const EFFECTS: &[(&str, &str)] = &[
    ("fruity parametric eq 2", EQ),
    ("fruity parametric eq", EQ),
    ("fruity 7 band equalizer", EQ),
    ("fruity filter", EQ),
    ("fruity free filter", EQ),
    ("fruity love philter", EQ),
    ("fruity bass boost", EQ),
    ("fruity compressor", COMPRESSOR),
    ("fruity limiter", LIMITER),
    // FL's loudness tools: a limiter into a soft or hard clipper.
    ("emphasis", LIMITER),
    ("emphasizer", LIMITER),
    ("maximus", MULTIBAND),
    ("fruity multiband compressor", MULTIBAND),
    ("soundgoodizer", MULTIBAND),
    ("ott", MULTIBAND),
    ("xfer ott", MULTIBAND),
    ("fruity reeverb", REVERB),
    ("fruity reeverb 2", REVERB),
    ("fruity convolver", REVERB),
    ("fruity delay", DELAY),
    ("fruity delay 2", DELAY),
    ("fruity delay 3", DELAY),
    ("fruity delay bank", DELAY),
    ("fruity peak controller", DUCKER),
    ("kickstart", DUCKER),
    ("kickstart 2", DUCKER),
    ("lfotool", DUCKER),
    ("volumeshaper", DUCKER),
    ("duck", DUCKER),
    ("fruity waveshaper", WAVESHAPER),
    ("fruity blood overdrive", WAVESHAPER),
    ("fruity fast dist", WAVESHAPER),
    ("fruity squeeze", WAVESHAPER),
    ("distructor", WAVESHAPER),
    ("hardcore", WAVESHAPER),
    ("decapitator", WAVESHAPER),
    ("saturn", WAVESHAPER),
    ("saturn 2", WAVESHAPER),
    ("msaturator", WAVESHAPER),
    ("mwaveshaper", WAVESHAPER),
    ("mwavefolder", WAVESHAPER),
    ("mdistortion", WAVESHAPER),
    ("decimort", WAVESHAPER),
    ("decimort 2", WAVESHAPER),
    ("thermal", WAVESHAPER),
    ("fruity soft clipper", CLIPPER),
    ("freeclip", CLIPPER),
    ("standardclip", CLIPPER),
    ("kclip", CLIPPER),
    ("kclip 3", CLIPPER),
    ("gclip", CLIPPER),
    ("clipshifter", CLIPPER),
    ("tdr kotelnikov", COMPRESSOR),
    ("tdr kotelnikov ge", COMPRESSOR),
    ("supercharger", COMPRESSOR),
    ("supercharger gt", COMPRESSOR),
    ("khs compactor", COMPRESSOR),
    // AdHd's tube leveling amplifier, an LA-2A-style vocal compressor.
    ("leveling tool", COMPRESSOR),
    ("levelingtool", COMPRESSOR),
    ("levelingtool_64", COMPRESSOR),
    ("leveling tool_64", COMPRESSOR),
    ("cla-2a", COMPRESSOR),
    ("la-2a", COMPRESSOR),
    ("mcompressor", COMPRESSOR),
    ("tdr feedback compressor ii", COMPRESSOR),
    ("molotok", COMPRESSOR),
    ("tdr nova", EQ),
    ("tdr nova ge", EQ),
    ("mequalizer", EQ),
    ("slick eq", EQ),
    ("tdr slickeq", EQ),
    // Acustica's Baxandall EQ.
    ("coralbaxterzl", EQ),
    ("coral baxter", EQ),
    ("khs filter", EQ),
    ("khs ladder filter", EQ),
    ("epicverb", REVERB),
    ("valhallavintageverb", REVERB),
    ("valhallasupermassive", REVERB),
    ("valhallaroom", REVERB),
    ("valhallashimmer", REVERB),
    ("valhallaplate", REVERB),
    ("khs reverb", REVERB),
    ("raum", REVERB),
    ("mreverb", REVERB),
    ("valhalladelay", DELAY),
    ("valhallafreqecho", DELAY),
    ("khs delay", DELAY),
    ("replika", DELAY),
    ("multiband delay", DELAY),
    ("gross beat", SHAPER),
    ("shaperbox", SHAPER),
    ("shaperbox 2", SHAPER),
    ("shaperbox 3", SHAPER),
    ("fruity panomatic", SHAPER),
    ("mautopan", SHAPER),
    ("mtremolo", SHAPER),
    ("khs trance gate", SHAPER),
    ("khs gate", SHAPER),
    ("fruity chorus", CHORUS),
    ("fruity flangus", CHORUS),
    ("fruity flanger", CHORUS),
    ("fruity phaser", CHORUS),
    ("khs chorus", CHORUS),
    ("khs flanger", CHORUS),
    ("khs phaser", CHORUS),
    ("fruity balance", UTILITY),
    ("fruity stereo enhancer", UTILITY),
    ("fruity stereo shaper", UTILITY),
    // FL's DC offset remover.
    ("fruity center", UTILITY),
    ("fruity phase inverter", UTILITY),
    ("mstereoexpander", UTILITY),
    ("mstereoprocessor", UTILITY),
    ("khs stereo", UTILITY),
    ("ozone imager", UTILITY),
    ("izotope ozone imager", UTILITY),
    ("transient processor", TRANSIENT),
    ("khs transient shaper", TRANSIENT),
    ("transient master", TRANSIENT),
    ("vocodex", VOCODER),
    ("fruity vocoder", VOCODER),
    ("wave candy", ANALYZER),
    ("fruity spectroman", ANALYZER),
    ("fruity db meter", ANALYZER),
    ("fruity scope", ANALYZER),
    ("span", ANALYZER),
    ("span plus", ANALYZER),
    ("youlean loudness meter 2", ANALYZER),
    ("insight 2", ANALYZER),
    ("mloudnessanalyzer", ANALYZER),
    ("manalyzer", ANALYZER),
    // Image-Line's own (FL 20.8.4 on) and MeldaProduction's frequency
    // shifters.
    ("frequency shifter", FREQUENCY_SHIFTER),
    ("mfreqshifter", FREQUENCY_SHIFTER),
    ("mfreqshiftermb", FREQUENCY_SHIFTER),
];

/// Pieces of well-known plugin names, anywhere in the name.
const EFFECT_PARTS: &[(&str, &str)] = &[
    ("pro-q", EQ),
    ("pro-mb", MULTIBAND),
    ("pro-c", COMPRESSOR),
    ("pro-l", LIMITER),
    ("pro-r", REVERB),
    ("saturn", WAVESHAPER),
    ("trash", WAVESHAPER),
];

/// Words that give an effect away when its name isn't listed.
const EFFECT_WORDS: &[(&str, &str)] = &[
    ("ott", MULTIBAND),
    ("valhalla", REVERB),
    ("reverb", REVERB),
    ("verb", REVERB),
    ("delay", DELAY),
    ("echo", DELAY),
    ("timeless", DELAY),
    ("eq", EQ),
    ("filter", EQ),
    ("limit", LIMITER),
    ("maximizer", LIMITER),
    ("multiband", MULTIBAND),
    ("compress", COMPRESSOR),
    ("comp", COMPRESSOR),
    ("kickstart", DUCKER),
    ("sidechain", DUCKER),
    ("duck", DUCKER),
    ("distort", WAVESHAPER),
    ("overdrive", WAVESHAPER),
    ("saturat", WAVESHAPER),
    ("clip", WAVESHAPER),
    // Before "shaper", which would pick the volume shaper.
    ("transient", TRANSIENT),
    ("shaper", SHAPER),
    ("chorus", CHORUS),
    ("flang", CHORUS),
    ("phaser", CHORUS),
    ("vocoder", VOCODER),
    ("analy", ANALYZER),
    ("meter", ANALYZER),
    ("scope", ANALYZER),
    ("spectrum", ANALYZER),
    // Last, so "Stereo Delay" stays a delay.
    ("imager", UTILITY),
    ("widen", UTILITY),
    ("stereo", UTILITY),
];

/// What an FL effect becomes. Effects nothing fits are skipped, rather
/// than colouring the sound with the wrong thing.
pub fn suggest_effect(name: &str) -> Suggestion {
    let key = normalize(name);
    if SKIPPED_EFFECTS.contains(&key.as_str()) {
        return rule(Choice::Skip);
    }
    if let Some((_, id)) = EFFECTS.iter().find(|(n, _)| *n == key) {
        return rule(Choice::Builtin(id));
    }
    if let Some((_, id)) = EFFECT_PARTS.iter().find(|(p, _)| key.contains(p)) {
        return rule(Choice::Builtin(id));
    }
    if let Some((_, id)) = EFFECT_WORDS.iter().find(|(w, _)| contains_word(&key, w)) {
        return rule(Choice::Builtin(id));
    }
    Suggestion {
        choice: Choice::Skip,
        recognized: false,
    }
}

/// Plugins that pump to the beat on their own (no sidechain needed).
pub fn is_tempo_ducker(name: &str) -> bool {
    let key = normalize(name);
    ["kickstart", "lfotool", "volumeshaper", "duck"]
        .iter()
        .any(|w| key.starts_with(w))
}

pub fn is_peak_controller(name: &str) -> bool {
    normalize(name) == "fruity peak controller"
}

/// Lower case, with vendor suffixes and spacing evened out.
pub fn normalize(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let lower = lower
        .strip_suffix(" (x64)")
        .or_else(|| lower.strip_suffix(" x64"))
        .unwrap_or(&lower);
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `word` at the start of one of the name's words (so "eq" matches "Pro-Q
/// EQ" but not "frequency").
fn contains_word(name: &str, word: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric())
        .any(|part| part.starts_with(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument(name: &str) -> Choice {
        suggest_instrument(name).choice
    }

    fn effect(name: &str) -> Choice {
        suggest_effect(name).choice
    }

    #[test]
    fn instruments_by_name() {
        assert_eq!(instrument("3x Osc"), Choice::Builtin(POLY_SYNTH));
        assert_eq!(instrument("Sytrus"), Choice::Builtin(WAVETABLE));
        assert_eq!(instrument("FLEX"), Choice::Builtin(WAVETABLE));
        assert_eq!(instrument("Serum"), Choice::Builtin(WAVETABLE));
        assert_eq!(instrument("Vital"), Choice::Builtin(WAVETABLE));
        assert_eq!(instrument("FPC"), Choice::Builtin(DRUMS));
        assert_eq!(instrument("DirectWave"), Choice::Builtin(SAMPLER));
        assert_eq!(instrument("sforzando"), Choice::Builtin(SAMPLER));
        assert_eq!(instrument(FL_SAMPLER), Choice::Builtin(SAMPLER));
        assert_eq!(instrument("Ample Guitar M"), Choice::Builtin(GUITAR));
        assert_eq!(instrument("Plucked!"), Choice::Builtin(GUITAR));
        assert_eq!(instrument("MIDI Out"), Choice::Skip);
        let unknown = suggest_instrument("Mystery Box 3000");
        assert!(!unknown.recognized);
        assert_eq!(unknown.choice, Choice::Builtin(POLY_SYNTH));
        assert!(suggest_instrument("Harmor").recognized);
    }

    #[test]
    fn effects_by_name() {
        assert_eq!(effect("Fruity Parametric EQ 2"), Choice::Builtin(EQ));
        assert_eq!(effect("Fruity parametric EQ 2"), Choice::Builtin(EQ));
        assert_eq!(effect("Fruity Limiter"), Choice::Builtin(LIMITER));
        assert_eq!(effect("Fruity Compressor"), Choice::Builtin(COMPRESSOR));
        assert_eq!(effect("Fruity Reeverb 2"), Choice::Builtin(REVERB));
        assert_eq!(effect("ValhallaVintageVerb"), Choice::Builtin(REVERB));
        assert_eq!(effect("Fruity Delay 3"), Choice::Builtin(DELAY));
        assert_eq!(effect("Soundgoodizer"), Choice::Builtin(MULTIBAND));
        assert_eq!(effect("OTT"), Choice::Builtin(MULTIBAND));
        assert_eq!(effect("Fruity Peak Controller"), Choice::Builtin(DUCKER));
        assert_eq!(effect("Kickstart 2"), Choice::Builtin(DUCKER));
        assert_eq!(effect("Fruity WaveShaper"), Choice::Builtin(WAVESHAPER));
        assert_eq!(effect("Distructor"), Choice::Builtin(WAVESHAPER));
        assert_eq!(effect("Gross Beat"), Choice::Builtin(SHAPER));
        assert_eq!(effect("ShaperBox 3"), Choice::Builtin(SHAPER));
        assert_eq!(effect("Vocodex"), Choice::Builtin(VOCODER));
        assert_eq!(effect("Wave Candy"), Choice::Builtin(ANALYZER));
        assert_eq!(effect("FabFilter Pro-Q 3"), Choice::Builtin(EQ));
        assert_eq!(effect("FabFilter Pro-L 2"), Choice::Builtin(LIMITER));
        assert_eq!(effect("Fruity NoteBook 2"), Choice::Skip);
        assert_eq!(effect("Fruity Frequency Shifter"), Choice::Skip);
        assert_eq!(
            effect("Frequency Shifter"),
            Choice::Builtin(FREQUENCY_SHIFTER)
        );
        assert_eq!(effect("MFreqShifter"), Choice::Builtin(FREQUENCY_SHIFTER));
        assert_eq!(effect("Fruity Chorus"), Choice::Builtin(CHORUS));
        assert_eq!(effect("kHs Phaser"), Choice::Builtin(CHORUS));
        assert_eq!(effect("Fruity Stereo Enhancer"), Choice::Builtin(UTILITY));
        assert_eq!(effect("MStereoExpander"), Choice::Builtin(UTILITY));
        assert_eq!(effect("Stereo Delay"), Choice::Builtin(DELAY));
        assert_eq!(effect("Emphasizer"), Choice::Builtin(LIMITER));
        assert_eq!(effect("Transient Processor"), Choice::Builtin(TRANSIENT));
        assert_eq!(effect("kHs Transient Shaper"), Choice::Builtin(TRANSIENT));
        assert_eq!(effect("SPL Transient Designer"), Choice::Builtin(TRANSIENT));
        assert_eq!(effect("CORALBAXTERZL"), Choice::Builtin(EQ));
        assert!(!suggest_effect("Mystery Box 3000").recognized);
        assert!(is_tempo_ducker("Kickstart 2") && !is_tempo_ducker("Fruity Peak Controller"));
    }

    #[test]
    fn every_choice_is_a_real_builtin() {
        let all = INSTRUMENTS
            .iter()
            .chain(INSTRUMENT_WORDS)
            .map(|(_, id)| *id)
            .chain([POLY_SYNTH]);
        for id in all {
            assert!(instruments::find_builtin(id).is_some(), "{id}");
        }
        for (_, id) in EFFECTS.iter().chain(EFFECT_PARTS).chain(EFFECT_WORDS) {
            assert!(effects::find_builtin(id).is_some(), "{id}");
        }
        assert_eq!(Choice::Builtin(REVERB).label(), "Reverb");
        assert_eq!(Choice::Builtin(SAMPLER).label(), "SFZ Sampler");
        assert_eq!(Choice::Skip.label(), "Skip");
        assert!(instrument_choices().contains(&Choice::Skip));
        assert_eq!(effect_choices().len(), effects::builtins().len() + 1);
    }
}
