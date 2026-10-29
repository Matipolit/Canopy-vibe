use dsp::{Adsr, AdsrDesc, Gain, Oscillator, Processor, Smoothed, Svf, Waveform};
use midi::{NoteEvent, NoteEventKind, Pitch};

use crate::envelope::{self, EnvelopeParams};
use crate::{BuiltInInstrumentDescriptor, Instrument, ParamDescriptor};
use crate::{choice, float};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.poly_synth",
    name: "Poly Synth",
    description: "Two detuned oscillators through a resonant lowpass filter",
    version: "1.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[ENVELOPE],
    create: || Box::new(PolySynth::new()),
    data: crate::DataKind::None,
    groups: crate::no_groups,
};

const VOICES: usize = 16;

/// Samples between filter updates. The cutoff follows the envelope, and
/// recomputing it costs an `exp2` and a `tan` per voice; at 48 kHz this still
/// updates 6000 times a second, too fast to hear steps.
const CONTROL_INTERVAL: usize = 8;

/// Scales a voice so one full-velocity note at default settings peaks around
/// 0.4 (when its two oscillators line up), leaving headroom for chords.
const VOICE_LEVEL: f32 = 0.8;

/// Share of each oscillator sent to its own side (first left, second right);
/// the rest goes to the other side. Constant sum, so a mono fold-down is flat.
const SPREAD_NEAR: f32 = 0.65;
const SPREAD_FAR: f32 = 1.0 - SPREAD_NEAR;

/// Ramp for cutoff, resonance and envelope amount changes.
const PARAM_RAMP_MS: f32 = 20.0;

/// Ramp for a sounding voice's velocity when it is retriggered or stolen, so
/// its level doesn't jump.
const VELOCITY_RAMP_MS: f32 = 5.0;

/// Where the start-phase sequence begins after `reset`, so a render from the
/// top always sounds the same.
const PHASE_SEED: u32 = 0x9E37_79B9;

const WAVEFORM: usize = 0;
const DETUNE: usize = 1;
const CUTOFF: usize = 2;
const RESONANCE: usize = 3;
const ENV_AMOUNT: usize = 4;
const GAIN_DB: usize = 5;
/// Attack, decay, sustain, release and their curves, in that order.
const ENVELOPE: EnvelopeParams = EnvelopeParams::starting_at("Envelope", 6);

const WAVEFORMS: [Waveform; 3] = [Waveform::Sine, Waveform::Saw, Waveform::Square];

/// The engine addresses parameters by index, so once shipped this list only
/// grows at the end.
const PARAMS: &[ParamDescriptor] = &[
    choice("waveform", "Waveform", &["Sine", "Saw", "Square"], 1),
    float("detune", "Detune", 0.0, 50.0, 7.0, "cents", false),
    float("cutoff", "Cutoff", 20.0, 20_000.0, 3000.0, "Hz", true),
    float("resonance", "Resonance", 0.0, 1.0, 0.2, "", false),
    float("env_amount", "Env Amount", 0.0, 6.0, 2.0, "oct", false),
    float("gain", "Gain", -60.0, 6.0, -6.0, "dB", false),
    envelope::attack("attack", 5.0),
    envelope::decay("decay", 300.0),
    envelope::sustain("sustain", 0.6),
    envelope::release("release", 300.0),
    envelope::curve("attack_curve", "Attack Curve", -0.5),
    envelope::curve("decay_curve", "Decay Curve", -0.7),
    envelope::curve("release_curve", "Release Curve", -0.7),
];

/// Filter settings shared by every voice, refreshed once per control interval.
#[derive(Debug, Clone, Copy, Default)]
struct FilterControls {
    /// Smoothed in octaves rather than Hz, so a sweep sounds even across the range.
    log2_cutoff: f32,
    resonance: f32,
    env_octaves: f32,
}

/// One note: two detuned oscillators panned apart, through a lowpass whose
/// cutoff opens with the amplitude envelope.
struct Voice {
    oscs: [Oscillator; 2],
    /// Left and right. The oscillators are panned before filtering, so each
    /// side needs its own filter state.
    filters: [Svf; 2],
    env: Adsr,
    velocity: Smoothed,
    pitch: Pitch,
    /// Key still down. Note-off clears it, so a voice is released only once.
    held: bool,
    /// The note counter at the last note-on; the lowest is the oldest note.
    started: u64,
    /// The note's own bend, as a frequency ratio, on top of the wheel's.
    bend: f32,
    /// Started since it last rendered: a bend arriving now is where the
    /// note starts, not a move to glide through.
    fresh: bool,
}

impl Voice {
    fn new() -> Self {
        Self {
            oscs: [
                Oscillator::new(Waveform::Saw, 440.0),
                Oscillator::new(Waveform::Saw, 440.0),
            ],
            filters: [Svf::new(1000.0, 0.0), Svf::new(1000.0, 0.0)],
            env: Adsr::new(AdsrDesc::default(), 44_100.0),
            velocity: Smoothed::new(0.0, VELOCITY_RAMP_MS),
            pitch: Pitch::A4,
            held: false,
            started: 0,
            bend: 1.0,
            fresh: false,
        }
    }

    fn prepare(&mut self, sample_rate: f32, env: AdsrDesc) {
        for osc in &mut self.oscs {
            osc.prepare(sample_rate);
        }
        for filter in &mut self.filters {
            filter.prepare(sample_rate);
        }
        self.env = Adsr::new(env, sample_rate);
        self.velocity.prepare(sample_rate);
        self.silence();
    }

    fn is_active(&self) -> bool {
        self.env.is_active()
    }

    fn silence(&mut self) {
        self.env.reset();
        self.held = false;
        for osc in &mut self.oscs {
            osc.reset();
        }
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    /// `phases` are where the oscillators start if the voice was silent.
    fn start(
        &mut self,
        pitch: Pitch,
        velocity: f32,
        phases: [f32; 2],
        detune_ratio: f32,
        controls: &FilterControls,
    ) {
        if self.is_active() {
            // Retriggered or stolen: keep phases, filter state and level going
            // (the envelope attacks from where it is), so the output doesn't jump.
            self.velocity.set_target(velocity);
        } else {
            self.silence();
            for (osc, phase) in self.oscs.iter_mut().zip(phases) {
                osc.set_phase(phase);
            }
            self.velocity.set_immediate(velocity);
        }
        self.pitch = pitch;
        self.held = true;
        self.bend = 1.0;
        self.fresh = true;
        self.tune(detune_ratio, false);
        self.env.start_attack();
        self.update_filter(controls);
    }

    fn release(&mut self) {
        self.held = false;
        self.env.start_release();
    }

    /// Puts the oscillators `detune_ratio` below and above the note. Gliding
    /// is for detune changes on a sounding note; a new note jumps.
    fn tune(&mut self, detune_ratio: f32, glide: bool) {
        let hz = self.pitch.frequency();
        for (osc, hz) in self
            .oscs
            .iter_mut()
            .zip([hz / detune_ratio, hz * detune_ratio])
        {
            if glide {
                osc.set_frequency(hz);
            } else {
                osc.set_frequency_immediate(hz);
            }
        }
    }

    fn update_filter(&mut self, controls: &FilterControls) {
        // The filter clamps this below Nyquist.
        let hz = (controls.log2_cutoff + controls.env_octaves * self.env.level()).exp2();
        for filter in &mut self.filters {
            filter.set_cutoff(hz);
            filter.set_resonance(controls.resonance);
        }
    }

    /// Adds this voice to `left` and `right`.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.fresh = false;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let a = self.oscs[0].next_sample();
            let b = self.oscs[1].next_sample();
            let amp = self.env.next_sample() * self.velocity.tick() * VOICE_LEVEL;
            let [filter_l, filter_r] = &mut self.filters;
            *l += filter_l
                .process_sample(a * SPREAD_NEAR + b * SPREAD_FAR)
                .lowpass
                * amp;
            *r += filter_r
                .process_sample(a * SPREAD_FAR + b * SPREAD_NEAR)
                .lowpass
                * amp;
        }
    }
}

/// Sixteen-voice subtractive synth: two detuned oscillators per voice through
/// a resonant lowpass, with one ADSR driving both the amplitude and the cutoff.
///
/// A new pitch takes an idle voice, or steals the quietest released voice,
/// or failing that the oldest held one. Idle voices cost nothing.
pub struct PolySynth {
    voices: [Voice; VOICES],
    /// `2^(detune / 2400)`: half the detune each way, so the pair stays
    /// centered on the note.
    detune_ratio: f32,
    env: AdsrDesc,
    log2_cutoff: Smoothed,
    resonance: Smoothed,
    env_octaves: Smoothed,
    controls: FilterControls,
    /// Samples until the next filter update. Carries across blocks and event
    /// splits so updates stay evenly spaced.
    control_countdown: usize,
    gain: Gain,
    notes_started: u64,
    /// Xorshift state for oscillator start phases.
    phase_seed: u32,
    /// The pitch wheel's frequency ratio, applied to every voice.
    bend: f32,
}

impl PolySynth {
    pub fn new() -> Self {
        let mut synth = Self {
            voices: std::array::from_fn(|_| Voice::new()),
            detune_ratio: 1.0,
            env: AdsrDesc::default(),
            log2_cutoff: Smoothed::new(0.0, PARAM_RAMP_MS),
            resonance: Smoothed::new(0.0, PARAM_RAMP_MS),
            env_octaves: Smoothed::new(0.0, PARAM_RAMP_MS),
            controls: FilterControls::default(),
            control_countdown: 0,
            gain: Gain::new(0.0),
            notes_started: 0,
            phase_seed: PHASE_SEED,
            bend: 1.0,
        };
        for (index, param) in PARAMS.iter().enumerate() {
            synth.set_param(index, param.default_value());
        }
        synth.reset();
        synth
    }

    fn handle(&mut self, kind: NoteEventKind) {
        match kind {
            NoteEventKind::On { pitch, velocity } => self.note_on(pitch, velocity.as_f32()),
            NoteEventKind::Off { pitch, .. } => {
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.held && v.pitch == pitch)
                {
                    voice.release();
                }
            }
            NoteEventKind::AllNotesOff => {
                for voice in self.voices.iter_mut().filter(|v| v.held) {
                    voice.release();
                }
            }
            NoteEventKind::PitchBend(value) => {
                self.bend = NoteEventKind::bend_ratio(value);
                for voice in &mut self.voices {
                    for osc in &mut voice.oscs {
                        osc.set_pitch_ratio(self.bend * voice.bend);
                    }
                }
            }
            NoteEventKind::NoteBend { pitch, cents } => {
                let wheel = self.bend;
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.held && v.pitch == pitch)
                {
                    voice.bend = NoteEventKind::note_bend_ratio(cents);
                    for osc in &mut voice.oscs {
                        if voice.fresh {
                            osc.set_pitch_ratio_immediate(wheel * voice.bend);
                        } else {
                            osc.modulate_pitch_ratio(wheel * voice.bend);
                        }
                    }
                }
            }
            NoteEventKind::Control { .. } => {}
        }
    }

    fn note_on(&mut self, pitch: Pitch, velocity: f32) {
        let index = self
            .voices
            .iter()
            .position(|v| v.is_active() && v.pitch == pitch)
            .or_else(|| self.voices.iter().position(|v| !v.is_active()))
            .unwrap_or_else(|| self.voice_to_steal());
        let phases = [self.random_phase(), self.random_phase()];
        self.notes_started += 1;
        let voice = &mut self.voices[index];
        voice.started = self.notes_started;
        voice.start(pitch, velocity, phases, self.detune_ratio, &self.controls);
        for osc in &mut voice.oscs {
            osc.set_pitch_ratio_immediate(self.bend);
        }
    }

    /// Free-running analog oscillators are never in step. Starting every note
    /// at the same phase would also line up a chord's waveforms into a peak
    /// about 3 dB louder.
    fn random_phase(&mut self) -> f32 {
        dsp::noise::unit(dsp::xorshift32(&mut self.phase_seed))
    }

    /// The quietest released voice, else the oldest. Only called when every
    /// voice is busy.
    fn voice_to_steal(&self) -> usize {
        let voices = self.voices.iter().enumerate();
        super::voice_to_steal(voices, |v| !v.held, |v| v.env.level(), |v| v.started).unwrap_or(0)
    }

    fn update_envelopes(&mut self) {
        for voice in &mut self.voices {
            voice.env.set_desc(self.env);
        }
    }

    fn read_controls(&mut self) {
        self.controls = FilterControls {
            log2_cutoff: self.log2_cutoff.current(),
            resonance: self.resonance.current(),
            env_octaves: self.env_octaves.current(),
        };
    }

    /// Jump the filter settings to their targets.
    fn settle_controls(&mut self) {
        for smoothed in [
            &mut self.log2_cutoff,
            &mut self.resonance,
            &mut self.env_octaves,
        ] {
            smoothed.settle();
        }
        self.read_controls();
    }

    /// Adds every sounding voice into `left` and `right`. Returns whether any was sounding.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        if !self.voices.iter().any(Voice::is_active) {
            // Nothing to ramp for, so the next note starts at the settings as they are now.
            self.settle_controls();
            return false;
        }
        let mut start = 0;
        while start < left.len() {
            let update = self.control_countdown == 0;
            if update {
                self.log2_cutoff.tick();
                self.resonance.tick();
                self.env_octaves.tick();
                self.read_controls();
                self.control_countdown = CONTROL_INTERVAL;
            }
            let end = start + self.control_countdown.min(left.len() - start);
            for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
                if update {
                    voice.update_filter(&self.controls);
                }
                voice.render(&mut left[start..end], &mut right[start..end]);
            }
            self.control_countdown -= end - start;
            start = end;
        }
        true
    }
}

impl Default for PolySynth {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for PolySynth {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        for voice in &mut self.voices {
            voice.prepare(sample_rate, self.env);
        }
        // Filter settings are ticked once per control interval, not per sample.
        let control_rate = sample_rate / CONTROL_INTERVAL as f32;
        self.log2_cutoff.prepare(control_rate);
        self.resonance.prepare(control_rate);
        self.env_octaves.prepare(control_rate);
        self.gain.prepare(sample_rate);
        self.reset();
    }

    fn reset(&mut self) {
        self.bend = 1.0;
        for voice in &mut self.voices {
            voice.silence();
            voice.bend = 1.0;
            for osc in &mut voice.oscs {
                osc.set_pitch_ratio_immediate(1.0);
            }
        }
        self.settle_controls();
        self.gain.reset();
        self.control_countdown = 0;
        self.phase_seed = PHASE_SEED;
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let mut sounded = false;
        for (span, event) in crate::segments(events, left.len()) {
            sounded |= self.render(&mut left[span.clone()], &mut right[span]);
            if let Some(kind) = event {
                self.handle(kind);
            }
        }
        if sounded {
            self.gain.process(left, right);
        } else {
            // Silence needs no scaling; finish any gain ramp instantly instead.
            self.gain.reset();
        }
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        match index {
            WAVEFORM => {
                let waveform = WAVEFORMS[value as usize];
                for osc in self.voices.iter_mut().flat_map(|v| &mut v.oscs) {
                    osc.set_waveform(waveform);
                }
            }
            DETUNE => {
                self.detune_ratio = (value / 2400.0).exp2();
                for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
                    voice.tune(self.detune_ratio, true);
                }
            }
            CUTOFF => self.log2_cutoff.set_target(value.log2()),
            RESONANCE => self.resonance.set_target(value),
            ENV_AMOUNT => self.env_octaves.set_target(value),
            GAIN_DB => self.gain.set_db(value),
            _ => {
                if ENVELOPE.apply(index, value, &mut self.env) {
                    self.update_envelopes();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use midi::Velocity;

    use super::*;
    use crate::ParameterKind;

    const SAMPLE_RATE: f32 = 48_000.0;
    const BLOCK: usize = 512;

    fn synth() -> PolySynth {
        let mut synth = PolySynth::new();
        synth.prepare(SAMPLE_RATE, BLOCK);
        synth
    }

    fn pitch(note: u8) -> Pitch {
        Pitch::new(note).unwrap()
    }

    fn on(frame: u32, note: u8) -> NoteEvent {
        NoteEvent {
            frame,
            kind: NoteEventKind::On {
                pitch: pitch(note),
                velocity: Velocity::MAX,
            },
        }
    }

    fn off(frame: u32, note: u8) -> NoteEvent {
        NoteEvent {
            frame,
            kind: NoteEventKind::Off {
                pitch: pitch(note),
                velocity: Velocity::CENTER,
            },
        }
    }

    const ALL_NOTES_OFF: NoteEvent = NoteEvent {
        frame: 0,
        kind: NoteEventKind::AllNotesOff,
    };

    fn peak(buffer: &[f32]) -> f32 {
        buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    /// Renders `blocks` blocks, with `events` in the first, and returns the
    /// peak over both channels.
    fn run(synth: &mut PolySynth, events: &[NoteEvent], blocks: usize) -> f32 {
        let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
        let mut max = 0.0f32;
        for block in 0..blocks {
            synth.process(if block == 0 { events } else { &[] }, &mut left, &mut right);
            assert!(left.iter().chain(&right).all(|s| s.is_finite()));
            max = max.max(peak(&left)).max(peak(&right));
        }
        max
    }

    /// Enough blocks to cover `ms`.
    fn blocks(ms: f32) -> usize {
        (ms / 1000.0 * SAMPLE_RATE / BLOCK as f32).ceil() as usize
    }

    fn active_voices(synth: &PolySynth) -> usize {
        synth.voices.iter().filter(|v| v.is_active()).count()
    }

    fn sounding(synth: &PolySynth, note: u8) -> bool {
        synth
            .voices
            .iter()
            .any(|v| v.is_active() && v.pitch == pitch(note))
    }

    #[test]
    fn silent_without_notes() {
        let mut synth = synth();
        assert_eq!(run(&mut synth, &[], 10), 0.0);
    }

    #[test]
    fn a_note_starts_at_its_event_frame() {
        let mut synth = synth();
        let (mut left, mut right) = ([1.0; BLOCK], [1.0; BLOCK]);
        synth.process(&[on(100, 60)], &mut left, &mut right);
        assert_eq!(peak(&left[..100]), 0.0, "silent before the note");
        assert_eq!(peak(&right[..100]), 0.0, "silent before the note");
        assert!(peak(&left[100..]) > 0.1);
        assert!(peak(&right[100..]) > 0.1);
        assert_ne!(
            left, right,
            "the oscillators are spread across the stereo field"
        );
    }

    #[test]
    fn one_voice_peaks_with_headroom_for_chords() {
        let mut synth = synth();
        for note in [36, 60, 84] {
            // The peak comes during the attack, highest when the oscillators
            // happen to start in step. Each note from silence gets new phases.
            let (mut lowest, mut highest) = (f32::MAX, 0.0f32);
            for _ in 0..10 {
                let level = run(&mut synth, &[on(0, note)], blocks(200.0));
                run(&mut synth, &[off(0, note)], blocks(310.0));
                lowest = lowest.min(level);
                highest = highest.max(level);
            }
            assert!(
                lowest > 0.2 && (0.3..=0.5).contains(&highest),
                "note {note} peaks between {lowest} and {highest}"
            );
        }
    }

    #[test]
    fn simultaneous_notes_take_one_voice_each() {
        let mut synth = synth();
        run(
            &mut synth,
            &[on(0, 60), on(0, 64), on(0, 67), on(10, 72)],
            1,
        );
        assert_eq!(active_voices(&synth), 4);
    }

    #[test]
    fn retriggering_a_pitch_reuses_its_voice() {
        let mut synth = synth();
        run(&mut synth, &[on(0, 60)], 4);
        run(&mut synth, &[on(0, 60)], 4);
        assert_eq!(active_voices(&synth), 1);

        // Also while it is releasing, and it is then held again.
        run(&mut synth, &[off(0, 60)], 2);
        run(&mut synth, &[on(0, 60)], 2);
        assert_eq!(active_voices(&synth), 1);
        run(&mut synth, &[], blocks(1000.0));
        assert_eq!(active_voices(&synth), 1, "still held");
    }

    #[test]
    fn the_seventeenth_note_steals_the_oldest() {
        let mut synth = synth();
        let notes: Vec<NoteEvent> = (0..16).map(|i| on(i, 40 + i as u8)).collect();
        run(&mut synth, &notes, 1);
        assert_eq!(active_voices(&synth), 16);

        run(&mut synth, &[on(0, 80)], 1);
        assert_eq!(active_voices(&synth), 16);
        assert!(sounding(&synth, 80), "the new note plays");
        assert!(!sounding(&synth, 40), "the oldest note made way");
        assert!((41..56).all(|note| sounding(&synth, note)));
    }

    #[test]
    fn stealing_prefers_the_quietest_released_voice() {
        let mut synth = synth();
        let notes: Vec<NoteEvent> = (0..16).map(|i| on(i, 40 + i as u8)).collect();
        run(&mut synth, &notes, blocks(500.0));
        run(&mut synth, &[off(0, 45)], blocks(100.0));
        // 50 has only just started its release, so it's louder than 45.
        run(&mut synth, &[off(0, 50), on(10, 80)], 1);
        assert!(sounding(&synth, 80));
        assert!(
            !sounding(&synth, 45),
            "the quietest released voice made way"
        );
        assert!(sounding(&synth, 40), "the oldest held note keeps playing");
        assert!(sounding(&synth, 50));
    }

    #[test]
    fn note_off_goes_silent_after_the_release_time() {
        let mut synth = synth();
        run(&mut synth, &[on(0, 60)], blocks(500.0));
        // The default release is 300 ms.
        assert!(run(&mut synth, &[off(0, 60)], blocks(250.0)) > 0.1);
        assert_eq!(active_voices(&synth), 1, "still in its release");
        run(&mut synth, &[], blocks(60.0));
        assert_eq!(active_voices(&synth), 0);
        assert_eq!(run(&mut synth, &[], 4), 0.0);
    }

    #[test]
    fn all_notes_off_releases_every_voice_with_a_tail() {
        let mut synth = synth();
        run(
            &mut synth,
            &[on(0, 60), on(0, 64), on(0, 67)],
            blocks(500.0),
        );
        assert!(run(&mut synth, &[ALL_NOTES_OFF], 1) > 0.1, "not cut off");
        assert_eq!(active_voices(&synth), 3);
        assert!(synth.voices.iter().all(|v| !v.held));
        run(&mut synth, &[], blocks(310.0));
        assert_eq!(active_voices(&synth), 0);
        assert_eq!(run(&mut synth, &[], 4), 0.0);
    }

    #[test]
    fn reset_silences_immediately() {
        let mut synth = synth();
        run(&mut synth, &[on(0, 60), on(0, 64)], 4);
        synth.reset();
        assert_eq!(active_voices(&synth), 0);
        assert_eq!(run(&mut synth, &[], 1), 0.0);
    }

    #[test]
    fn filter_changes_ramp_instead_of_jumping() {
        let mut synth = synth();
        run(&mut synth, &[on(0, 60)], 1);
        synth.set_param(CUTOFF, 20.0);
        synth.set_param(RESONANCE, 1.0);
        run(&mut synth, &[], 1);
        let FilterControls {
            log2_cutoff,
            resonance,
            ..
        } = synth.controls;
        assert!(log2_cutoff > 20f32.log2() && log2_cutoff < 3000f32.log2());
        assert!(resonance > 0.2 && resonance < 1.0);
        run(&mut synth, &[], blocks(20.0));
        assert_eq!(synth.controls.log2_cutoff, 20f32.log2());
        assert_eq!(synth.controls.resonance, 1.0);
    }

    #[test]
    fn extreme_parameters_stay_finite_and_bounded() {
        let range = |param: &ParamDescriptor| match param.kind {
            ParameterKind::Float { min, max, .. } => [min, max],
            ParameterKind::Enum { labels, .. } => [0.0, (labels.len() - 1) as f32],
        };
        // More notes than voices, spread over the keyboard, so some get stolen
        // and some harmonic sits near any cutoff.
        let chord: Vec<NoteEvent> = (0..20).map(|i| on(i, 24 + 4 * i as u8)).collect();
        let mut synth = synth();
        run(&mut synth, &chord, 1);

        let mut loudest = 0.0f32;
        for (index, param) in PARAMS.iter().enumerate() {
            for value in range(param) {
                synth.set_param(index, value);
                loudest = loudest.max(run(&mut synth, &chord, 8));
            }
            synth.set_param(index, param.default_value());
        }
        for extreme in [0, 1] {
            for (index, param) in PARAMS.iter().enumerate() {
                synth.set_param(index, range(param)[extreme]);
            }
            loudest = loudest.max(run(&mut synth, &chord, blocks(500.0)));
            loudest = loudest.max(run(&mut synth, &[ALL_NOTES_OFF], 8));
        }
        assert!(loudest < 20.0, "peaked at {loudest}");
    }

    #[test]
    fn nan_and_unknown_parameters_are_ignored() {
        let mut synth = synth();
        for index in 0..PARAMS.len() + 2 {
            synth.set_param(index, f32::NAN);
        }
        assert!(run(&mut synth, &[on(0, 60)], 8) > 0.1);
    }
}
