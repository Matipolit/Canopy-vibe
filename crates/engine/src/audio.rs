use std::sync::Arc;

use dsp::Smoothed;
use midi::{NoteEvent, NoteEventKind};
use project::ChannelMode;
use rtrb::{Consumer, Producer};

use crate::arrangement::{
    Automation, AutomationKind, Node, NodeSource, Processors, Region, ScheduledEvent,
};
use crate::chain::{self, EffectSlot};
use crate::fader::{Fader, FaderValues};
use crate::live::LiveState;
use crate::modulation::{ModContext, ModProgram, Sink};
use crate::pool::{Others, Pool, Sends};
use crate::{Arrangement, Command, LiveEvent, Played, Shared};

/// Largest block the engine renders in one go. Bigger device buffers are
/// split into several blocks. Automation is applied once per block, so this is
/// also its time resolution: 1.3 ms at 48 kHz. Defined in `dsp`, which
/// times modulated parameters' glides by it.
pub const MAX_BLOCK: usize = dsp::MAX_BLOCK;

/// Fade for audio clips when the transport starts, stops or jumps, so it
/// doesn't click.
const TRANSPORT_FADE_MS: f32 = 5.0;

/// All audio-thread state. Built on the UI thread, then moved into the audio
/// callback; from then on only the audio thread touches it.
pub struct Engine {
    commands: Consumer<Command>,
    garbage: Producer<Box<Arrangement>>,
    shared: Arc<Shared>,
    arrangement: Box<Arrangement>,
    playing: bool,
    playhead: u64,
    /// Where Stop returns to: the last place playback started or was moved to.
    return_position: u64,
    /// A seek waiting for the audio to fade out, so the jump doesn't click.
    pending_seek: Option<u64>,
    /// Fades audio clips in and out around transport changes. Instruments
    /// aren't gated, so their release tails ring out after Stop.
    transport_gate: Smoothed,
    gate: [f32; MAX_BLOCK],
    master: Fader,
    live_events: Consumer<LiveEvent>,
    played: Producer<Played>,
    live: LiveState,
    metronome: bool,
    sample_rate: f32,
    /// Renders the tracks of each block, on this thread and its workers.
    pool: Pool,
}

/// What every track's rendering needs to know about the block, the same
/// for all of them.
#[derive(Clone, Copy)]
pub(crate) struct Block {
    pub frames: usize,
    pub playing: bool,
    /// Playing, or still fading out after a stop or before a seek.
    pub rolling: bool,
    pub playhead: u64,
    pub bpm: f64,
    /// Where the song is at the block's start, in quarter notes.
    pub beat: f64,
    /// The transport fade, per sample.
    pub gate: [f32; MAX_BLOCK],
    /// Which of each send's buffers this block writes.
    pub parity: usize,
}

impl Default for Block {
    fn default() -> Self {
        Block {
            frames: 0,
            playing: false,
            rolling: false,
            playhead: 0,
            bpm: 120.0,
            beat: 0.0,
            gate: [0.0; MAX_BLOCK],
            parity: 0,
        }
    }
}

impl Engine {
    pub(crate) fn new(
        commands: Consumer<Command>,
        garbage: Producer<Box<Arrangement>>,
        shared: Arc<Shared>,
        sample_rate: f32,
        live_events: Consumer<LiveEvent>,
        played: Producer<Played>,
        workers: usize,
    ) -> Self {
        let mut transport_gate = Smoothed::new(0.0, TRANSPORT_FADE_MS);
        transport_gate.prepare(sample_rate);
        Self {
            commands,
            garbage,
            shared,
            arrangement: Arrangement::empty(),
            playing: false,
            playhead: 0,
            return_position: 0,
            pending_seek: None,
            transport_gate,
            gate: [0.0; MAX_BLOCK],
            master: Fader::new(FaderValues::UNITY, sample_rate),
            live_events,
            played,
            live: LiveState::new(),
            metronome: false,
            sample_rate,
            pool: Pool::new(workers),
        }
    }

    /// How many threads render tracks besides this one.
    pub fn workers(&self) -> usize {
        self.pool.workers()
    }

    /// Plays `arrangement` from the very start at once, with no fade-in and
    /// the master already at its level. For rendering offline: this drops
    /// the old snapshot here, which the audio thread must never do.
    pub(crate) fn start_offline(
        &mut self,
        arrangement: Box<Arrangement>,
        sample_rate: f32,
        start: u64,
    ) {
        self.master = Fader::new(arrangement.master, sample_rate);
        self.arrangement = arrangement;
        self.playing = true;
        self.playhead = start;
        self.return_position = start;
        self.transport_gate.set_immediate(1.0);
    }

    /// Retires everything the engine runs, for an offline render that's
    /// done: plugins stop on the thread that rendered them. Leaves nothing
    /// to play. Never call it on the audio thread of a live engine (it
    /// drops the snapshot here).
    pub(crate) fn retire_offline(&mut self) {
        self.arrangement.retire();
        self.arrangement = Arrangement::empty();
    }

    /// Render into an interleaved buffer with `channels` channels. The audio
    /// callback calls this, so it must stay real-time safe.
    pub fn process_interleaved(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        // Reading the clock doesn't allocate or lock (it's a vDSO call).
        let started = std::time::Instant::now();
        self.pool.callback_started();
        self.drain_commands();
        self.drain_live();

        let mut peaks = [0.0f32; 2];
        for chunk in out.chunks_mut(MAX_BLOCK * channels) {
            let frames = chunk.len() / channels;
            let mut done = 0;
            while done < frames {
                // A block stops at the loop's end, and the rest of it
                // carries on from the loop's start.
                let to_loop_end = self.frames_to_loop_end(frames - done);
                let n = to_loop_end.unwrap_or(frames - done);
                self.render(n);
                let master = &self.arrangement.master_bus;
                let part = &mut chunk[done * channels..(done + n) * channels];
                for (i, frame) in part.chunks_exact_mut(channels).enumerate() {
                    let (l, r) = (master.left[i], master.right[i]);
                    peaks[0] = peaks[0].max(l.abs());
                    peaks[1] = peaks[1].max(r.abs());
                    match frame {
                        [mono] => *mono = 0.5 * (l + r),
                        [left, right, rest @ ..] => {
                            *left = l;
                            *right = r;
                            rest.fill(0.0);
                        }
                        [] => unreachable!("channels > 0"),
                    }
                }
                done += n;
                if to_loop_end.is_some() {
                    self.loop_back();
                }
            }
        }

        self.shared.publish(peaks, self.playhead);
        let lasts = (out.len() / channels) as f32 / self.sample_rate;
        if lasts > 0.0 {
            self.shared
                .record_load(started.elapsed().as_secs_f32() / lasts);
        }
    }

    /// If playback reaches the loop's end within the next `frames`, how
    /// many frames until it does (never 0).
    fn frames_to_loop_end(&self, frames: usize) -> Option<usize> {
        let (_, end) = self.arrangement.loop_range?;
        if !self.playing || self.pending_seek.is_some() || self.playhead >= end {
            return None;
        }
        let left = end - self.playhead;
        (left <= frames as u64).then_some(left as usize)
    }

    /// Back to the loop's start, as seamlessly as a block boundary: notes
    /// still sounding end, like a jump, and what's under the start plays.
    fn loop_back(&mut self) {
        if let Some((start, _)) = self.arrangement.loop_range {
            self.playhead = start;
            self.release_all_notes();
        }
    }

    fn drain_commands(&mut self) {
        loop {
            // A snapshot swap hands the old one back through the garbage
            // queue. If the UI hasn't emptied it, wait rather than drop the
            // old snapshot here.
            if matches!(self.commands.peek(), Ok(Command::Arrangement(_)))
                && self.garbage.slots() == 0
            {
                break;
            }
            let Ok(command) = self.commands.pop() else {
                break;
            };
            match command {
                Command::SetPlaying(playing) => self.set_playing(playing),
                Command::SetLiveTarget(target) => {
                    if target != self.live.target {
                        self.release_live();
                        self.live.target = target;
                    }
                }
                Command::SetMetronome(on) => self.metronome = on,
                Command::Seek(sample) => self.seek(sample),
                Command::Arrangement(arrangement) => self.swap(arrangement),
                Command::SetTrackVolume { track, db } => {
                    if let Some((p, mods)) = self.track(track)
                        && !mods.set_base(Sink::Volume, db)
                    {
                        p.fader.set_gain(project::volume_gain(db));
                    }
                }
                Command::SetTrackPan { track, pan } => {
                    if let Some((p, mods)) = self.track(track)
                        && !mods.set_base(Sink::Pan, pan)
                    {
                        p.fader.set_pan(pan);
                    }
                }
                Command::SetTrackMute { track, mute } => {
                    if let Some(p) = self.processors(track) {
                        p.fader.set_mute(mute);
                    }
                }
                Command::SetMasterVolume(db) => self.master.set_gain(project::volume_gain(db)),
                Command::SetInstrumentParam {
                    track,
                    index,
                    value,
                } => {
                    if let Some((p, mods)) = self.track(track)
                        && !mods.set_base(Sink::instrument(index), value)
                    {
                        p.set_param(index, value);
                    }
                }
                Command::SetDeviceParam {
                    device,
                    index,
                    value,
                } => self.set_device_param(device, index, value),
                Command::SetModAmount {
                    owner,
                    route,
                    amount,
                } => {
                    if let Some(node) = owner.and_then(|t| self.arrangement.node_mut(t)) {
                        node.mods.set_amount(route, amount);
                    }
                }
                Command::SetModulatorParam {
                    modulator,
                    index,
                    value,
                } => {
                    for node in &mut self.arrangement.nodes {
                        if node.mods.set_modulator_param(modulator, index, value) {
                            break;
                        }
                    }
                }
                Command::SetCableGain {
                    owner,
                    cable,
                    gain_db,
                } => {
                    let program = match owner {
                        Some(track) => self.arrangement.node_mut(track).map(|n| &mut n.program),
                        None => Some(&mut self.arrangement.master_program),
                    };
                    if let Some(program) = program {
                        program.set_cable_gain(cable, gain_db);
                    }
                }
                Command::PreviewNote { track, event } => {
                    if let Some(p) = self.processors(track) {
                        p.push_preview(event);
                    }
                }
            }
        }
    }

    fn drain_live(&mut self) {
        while let Ok(event) = self.live_events.pop() {
            match event {
                LiveEvent::Note(kind) => match kind {
                    NoteEventKind::On { pitch, .. } => {
                        let p = usize::from(pitch.get());
                        self.live.sustained[p] = false;
                        self.live.sounding[p] = true;
                        self.play_live(kind);
                    }
                    NoteEventKind::Off { pitch, .. } => {
                        let p = usize::from(pitch.get());
                        if !self.live.sounding[p] {
                            continue;
                        }
                        if self.live.sustain {
                            self.live.sustained[p] = true;
                        } else {
                            self.live.sounding[p] = false;
                            self.play_live(kind);
                        }
                    }
                    NoteEventKind::PitchBend(value) => {
                        self.live.bent = value != 0;
                        self.play_live(kind);
                    }
                    NoteEventKind::Control { .. } => self.play_live(kind),
                    NoteEventKind::AllNotesOff => self.release_live(),
                },
                LiveEvent::Sustain(down) => {
                    self.live.sustain = down;
                    if !down {
                        for p in 0..128 {
                            if std::mem::take(&mut self.live.sustained[p]) {
                                self.live.sounding[p] = false;
                                self.play_live(live_off(p));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Ends every live note, and centers the pitch wheel, on the target.
    fn release_live(&mut self) {
        for p in 0..128 {
            self.live.sustained[p] = false;
            if std::mem::take(&mut self.live.sounding[p]) {
                self.play_live(live_off(p));
            }
        }
        if std::mem::take(&mut self.live.bent) {
            self.play_live(NoteEventKind::PitchBend(0));
        }
    }

    fn play_live(&mut self, kind: NoteEventKind) {
        if let Some(target) = self.live.target
            && let Some(processors) = self.processors(target)
        {
            processors.push_preview(kind);
        }
        let sample = self.playing.then_some(self.playhead);
        // Nobody reading means nothing to record; dropping is fine.
        let _ = self.played.push(Played { sample, kind });
    }

    fn processors(&mut self, track: project::TrackId) -> Option<&mut Processors> {
        self.arrangement.node_mut(track)?.processors.as_deref_mut()
    }

    /// A track's processors, with its modulation.
    fn track(&mut self, track: project::TrackId) -> Option<(&mut Processors, &mut ModProgram)> {
        let Node {
            processors, mods, ..
        } = self.arrangement.node_mut(track)?;
        Some((processors.as_deref_mut()?, mods))
    }

    /// A live knob move on an effect, wherever it is, unless its track's
    /// modulation takes it as the parameter's base.
    fn set_device_param(&mut self, device: project::DeviceId, index: usize, value: f32) {
        let arrangement = &mut *self.arrangement;
        for node in &mut arrangement.nodes {
            if let Some(slot) = node.effects.iter().position(|s| s.id == device) {
                if !node.mods.set_base(Sink::effect(slot, index), value)
                    && let Some(processor) = node.effects[slot].processor.as_deref_mut()
                {
                    processor.set_param(index, value);
                }
                return;
            }
        }
        let slot = arrangement
            .master_effects
            .iter_mut()
            .find(|s| s.id == device);
        if let Some(processor) = slot.and_then(|s| s.processor.as_deref_mut()) {
            processor.set_param(index, value);
        }
    }

    fn set_playing(&mut self, playing: bool) {
        match (self.playing, playing) {
            (false, true) => {
                if let Some(sample) = self.pending_seek.take() {
                    self.playhead = sample;
                    self.transport_gate.set_immediate(0.0);
                }
                self.playing = true;
                self.return_position = self.playhead;
                self.transport_gate.set_target(1.0);
            }
            (true, false) => {
                // Keep rolling until the fade-out ends, then rewind.
                self.playing = false;
                self.transport_gate.set_target(0.0);
                self.release_all_notes();
            }
            (false, false) => {
                // Stop while stopped goes back to the very start.
                self.return_position = 0;
                if !self.transport_gate.is_smoothing() {
                    self.playhead = 0;
                }
            }
            (true, true) => {}
        }
    }

    /// While audio is sounding, fades out first and jumps once it's silent
    /// (see the end of `render`); otherwise jumps now.
    fn seek(&mut self, sample: u64) {
        if self.transport_gate.current() > 0.0 {
            self.pending_seek = Some(sample);
            self.transport_gate.set_target(0.0);
        } else {
            self.jump(sample);
        }
    }

    fn jump(&mut self, sample: u64) {
        self.playhead = sample;
        self.return_position = sample;
        self.release_all_notes();
        if self.playing {
            self.transport_gate.set_target(1.0);
        }
    }

    fn release_all_notes(&mut self) {
        for node in &mut self.arrangement.nodes {
            if let Some(p) = node.processors.as_deref_mut() {
                p.all_notes_off = true;
            }
        }
    }

    /// Moves every surviving track's processors from the current snapshot
    /// into `new` (pointer moves only), then hands the current one back.
    fn swap(&mut self, mut new: Box<Arrangement>) {
        let old = &mut self.arrangement;
        for node in &mut new.nodes {
            if node.processors.is_some() {
                continue;
            }
            let Some(previous) = old.nodes.iter_mut().find(|o| o.id == node.id) else {
                continue;
            };
            let Some(mut processors) = previous.processors.take() else {
                continue;
            };
            // Compared with what the processors actually hold rather than the
            // old snapshot: automation and live commands move them without a
            // snapshot. Automated values are left to the automation, and
            // modulated ones to the modulation.
            let mut fader = node.fader;
            if node.automates(AutomationKind::Volume) || node.mods.modulates(Sink::Volume) {
                fader.gain = processors.fader.gain_target();
            }
            if node.automates(AutomationKind::Pan) || node.mods.modulates(Sink::Pan) {
                fader.pan = processors.fader.pan_target();
            }
            processors.fader.set(fader);
            processors.compensation.set(node.compensation);
            for (index, value) in node.params.iter().enumerate() {
                let automated = node
                    .automation
                    .iter()
                    .any(|a| matches!(a.kind, AutomationKind::Param(i, _) if i == index))
                    || node.mods.modulates(Sink::instrument(index));
                if !automated && processors.params.get(index) != Some(value) {
                    processors.set_param(index, *value);
                }
            }
            if let (NodeSource::Midi(new_events), NodeSource::Midi(old_events)) =
                (&node.source, &previous.source)
                && new_events != old_events
            {
                // A Note Off that was due may be gone now.
                processors.all_notes_off = true;
            }
            node.processors = Some(processors);
        }
        // Effects keep their instances by device id, wherever they were;
        // graphs and modulators keep their state.
        for node in &mut new.nodes {
            let Node {
                id,
                automation,
                effects,
                program,
                mods,
                ..
            } = node;
            let automated = |slot: usize, param: usize| {
                automation
                    .iter()
                    .any(|a| matches!(a.kind, AutomationKind::Effect(s, p, _) if (s, p) == (slot, param)))
                    || mods.modulates(Sink::effect(slot, param))
            };
            chain::adopt(effects, old, automated);
            program.adopt(old);
            if let Some(previous) = old.node_mut(*id) {
                mods.adopt(&mut previous.mods);
            }
        }
        chain::adopt(&mut new.master_effects, old, |_, _| false);
        new.master_program.adopt(old);
        if new.master != old.master {
            self.master.set(new.master);
        }
        if new.bpm != old.bpm {
            for node in &mut new.nodes {
                if let Some(instrument) = node
                    .processors
                    .as_deref_mut()
                    .and_then(|p| p.instrument.as_deref_mut())
                {
                    instrument.set_tempo(new.bpm);
                }
            }
        }
        // What the new snapshot didn't take is done: it stops here, on the
        // audio thread, before going back to be dropped.
        old.retire();
        let old = std::mem::replace(&mut self.arrangement, new);
        if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
            // Can't happen: there was a free slot before popping. If it
            // somehow did, leaking beats freeing on the audio thread.
            std::mem::forget(old);
        }
    }

    /// Render `frames` (at most `MAX_BLOCK`) into the master bus.
    fn render(&mut self, frames: usize) {
        let rolling = self.playing || self.transport_gate.is_smoothing();
        for gate in &mut self.gate[..frames] {
            *gate = self.transport_gate.tick();
        }
        self.arrangement.parity ^= 1;
        let bpm = self.arrangement.bpm;
        let block = Block {
            frames,
            playing: self.playing,
            rolling,
            playhead: self.playhead,
            bpm,
            beat: self.playhead as f64 * bpm / (60.0 * f64::from(self.sample_rate)),
            gate: self.gate,
            parity: self.arrangement.parity,
        };
        // Every track, on this thread and the workers.
        self.pool.run(&mut self.arrangement, block);

        let Arrangement {
            nodes,
            master_inputs,
            master_bus,
            sends,
            parity,
            beat_len,
            beats_per_bar,
            master_effects,
            master_program,
            ..
        } = &mut *self.arrangement;
        let context = effects::Context {
            bpm,
            beat: block.beat,
            playing: self.playing,
            sidechain: None,
        };
        master_bus.clear(frames);
        for &input in master_inputs.iter() {
            master_bus.add(&nodes[input as usize].out, frames);
        }
        master_program.run(
            master_effects,
            &mut master_bus.left[..frames],
            &mut master_bus.right[..frames],
            &context,
            Sends::new(sends, *parity),
        );
        self.master.process(
            &mut master_bus.left[..frames],
            &mut master_bus.right[..frames],
        );
        if self.metronome && self.playing {
            let clicks = Clicks {
                beat_len: *beat_len,
                beats_per_bar: *beats_per_bar,
                sample_rate: self.sample_rate,
            };
            clicks.add(
                self.playhead,
                &mut master_bus.left[..frames],
                &mut master_bus.right[..frames],
            );
        }

        if rolling {
            self.playhead += frames as u64;
        }
        let silent = !self.transport_gate.is_smoothing();
        if silent && let Some(sample) = self.pending_seek.take() {
            self.jump(sample);
        }
        if rolling && silent && !self.playing {
            self.playhead = self.return_position;
        }
    }
}

/// Renders one track's block into `node.out`: its automation, its source
/// (notes to its instrument, its audio regions, or the sum of its inputs
/// for a group), its modulation, effects, send, fader and compensation,
/// and its meters. Runs on whichever thread took the node; `others` gives
/// it the nodes it waited for, the sends and the meters.
pub(crate) fn render_node(node: &mut Node, index: usize, others: &Others<'_>, block: &Block) {
    let frames = block.frames;
    let Node {
        source,
        inputs,
        out,
        effects,
        program,
        mods,
        send,
        automation,
        meters: node_meters,
        processors,
        ..
    } = node;
    let left = &mut out.left[..frames];
    let right = &mut out.right[..frames];
    let Some(processors) = processors.as_deref_mut() else {
        left.fill(0.0);
        right.fill(0.0);
        return;
    };
    if block.playing {
        apply_automation(automation, processors, effects, mods, block.playhead);
    }
    let sends = others.sends();
    let context = effects::Context {
        bpm: block.bpm,
        beat: block.beat,
        playing: block.playing,
        sidechain: None,
    };
    // The events move out while the modulation and the instrument borrow
    // them beside the processors; the capacity comes back with them.
    let mut events = std::mem::take(&mut processors.events);
    events.clear();
    if let NodeSource::Midi(scheduled) = source {
        if processors.all_notes_off {
            processors.all_notes_off = false;
            push_event(&mut events, 0, NoteEventKind::AllNotesOff);
        }
        for kind in processors.preview.drain(..) {
            push_event(&mut events, 0, kind);
        }
        if block.playing {
            collect_events(scheduled, block.playhead, frames, &mut events);
        }
    }
    let mod_context = ModContext {
        frames,
        bpm: block.bpm,
        beat: block.beat,
        playing: block.playing,
        events: &events,
        sends,
    };
    mods.pre(&mod_context, processors);
    match source {
        NodeSource::Midi(_) => match processors.instrument.as_deref_mut() {
            Some(instrument) => {
                instrument.set_position(block.beat, block.playing);
                instrument.process(&events, left, right);
                if node_meters.watched() {
                    let mut values = [0.0; effects::MAX_METERS];
                    instrument.meters(&mut values);
                    node_meters.store(&values);
                }
            }
            None => {
                left.fill(0.0);
                right.fill(0.0);
            }
        },
        NodeSource::Audio(regions) => {
            left.fill(0.0);
            right.fill(0.0);
            if block.rolling {
                render_regions(regions, block.playhead, &block.gate[..frames], left, right);
            }
        }
        NodeSource::Bus(_) => {
            left.fill(0.0);
            right.fill(0.0);
            for &input in inputs.iter() {
                let other = &others.output(input as usize).out;
                for (o, s) in left.iter_mut().zip(&other.left[..frames]) {
                    *o += s;
                }
                for (o, s) in right.iter_mut().zip(&other.right[..frames]) {
                    *o += s;
                }
            }
        }
    }
    mods.post(&mod_context, (left, right), processors, effects);
    processors.events = events;
    program.run(effects, left, right, &context, sends);
    if let Some(bus) = send.and_then(|i| sends.write(i)) {
        bus.left[..frames].copy_from_slice(left);
        bus.right[..frames].copy_from_slice(right);
    }
    processors.fader.process(left, right);
    processors.compensation.process(left, right);
    others.meters().record(index, [peak(left), peak(right)]);
}

/// The metronome: a short, quickly decaying tone at each beat, higher on
/// the first of a bar. Worked out from the playhead alone, so seeking and
/// looping need nothing reset.
struct Clicks {
    beat_len: f64,
    beats_per_bar: u32,
    sample_rate: f32,
}

const CLICK_SECONDS: f64 = 0.04;
const CLICK_LEVEL: f32 = 0.35;

impl Clicks {
    fn add(&self, playhead: u64, left: &mut [f32], right: &mut [f32]) {
        if self.beat_len < 1.0 {
            return;
        }
        let rate = f64::from(self.sample_rate);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let sample = (playhead + i as u64) as f64;
            let beat = (sample / self.beat_len).floor();
            let t = (sample - beat * self.beat_len) / rate;
            if t >= CLICK_SECONDS {
                continue;
            }
            let downbeat = (beat as u64).is_multiple_of(u64::from(self.beats_per_bar));
            let hz = if downbeat { 1760.0 } else { 1320.0 };
            let envelope = (-t / 0.008).exp() as f32;
            let click = (std::f64::consts::TAU * hz * t).sin() as f32 * envelope * CLICK_LEVEL;
            *l += click;
            *r += click;
        }
    }
}

fn live_off(pitch: usize) -> NoteEventKind {
    NoteEventKind::Off {
        pitch: midi::Pitch::new(pitch as u8).unwrap_or(midi::Pitch::MIDDLE_C),
        velocity: midi::Velocity::CENTER,
    }
}

fn push_event(events: &mut Vec<NoteEvent>, frame: u32, kind: NoteEventKind) {
    if events.len() < events.capacity() {
        events.push(NoteEvent { frame, kind });
    }
}

/// Adds the events that fall in `playhead..playhead + frames`.
fn collect_events(
    scheduled: &[ScheduledEvent],
    playhead: u64,
    frames: usize,
    events: &mut Vec<NoteEvent>,
) {
    let end = playhead + frames as u64;
    let first = scheduled.partition_point(|e| e.sample < playhead);
    for event in scheduled[first..].iter().take_while(|e| e.sample < end) {
        push_event(events, (event.sample - playhead) as u32, event.kind);
    }
}

/// Adds the audio of every region overlapping this block, with clip fades
/// and the transport gate applied.
pub(crate) fn render_regions(
    regions: &[Region],
    playhead: u64,
    gate: &[f32],
    left: &mut [f32],
    right: &mut [f32],
) {
    let end = playhead + left.len() as u64;
    let first = regions.partition_point(|r| r.reach <= playhead);
    for region in regions[first..].iter().take_while(|r| r.start < end) {
        let from = region.start.max(playhead);
        let to = (region.start + region.len).min(end);
        let (src_left, src_right) = (region.audio.left(), region.audio.right());
        for sample in from..to {
            let i = sample - region.start;
            let Some(src) = usize::try_from(region.offset + i)
                .ok()
                .filter(|s| *s < src_left.len())
            else {
                break;
            };
            let remaining = region.len - i;
            let mut gain = region.gain;
            if i < region.fade_in {
                let t = (i + 1) as f32 / region.fade_in as f32;
                gain *= dsp::stage_shape(t, region.fade_in_curve);
            }
            if remaining <= region.fade_out {
                let t = remaining as f32 / region.fade_out as f32;
                gain *= dsp::stage_shape(t, region.fade_out_curve);
            }
            let out = (sample - playhead) as usize;
            gain *= gate[out];
            let (l, r) = match region.channels {
                ChannelMode::Stereo => (src_left[src], src_right[src]),
                ChannelMode::Mono => {
                    let mid = 0.5 * (src_left[src] + src_right[src]);
                    (mid, mid)
                }
                ChannelMode::Swap => (src_right[src], src_left[src]),
            };
            left[out] += l * gain;
            right[out] += r * gain;
        }
    }
}

/// Moves what's automated to where its lanes are now. A modulated value
/// goes to the modulation instead, as the base it modulates around.
fn apply_automation(
    automation: &mut [Automation],
    processors: &mut Processors,
    effects: &mut [EffectSlot],
    mods: &mut ModProgram,
    sample: u64,
) {
    for lane in automation {
        let Some(value) = lane.value_following(sample) else {
            continue;
        };
        if value == lane.last {
            continue;
        }
        lane.last = value;
        match lane.kind {
            AutomationKind::Volume => {
                let db = project::normalized_to_volume(value);
                if !mods.set_base(Sink::Volume, db) {
                    processors.fader.set_gain(project::volume_gain(db));
                }
            }
            AutomationKind::Pan => {
                let pan = project::normalized_to_pan(value);
                if !mods.set_base(Sink::Pan, pan) {
                    processors.fader.set_pan(pan);
                }
            }
            AutomationKind::Param(index, descriptor) => {
                let value = descriptor.from_normalized(value);
                if !mods.set_base(Sink::instrument(index), value) {
                    processors.set_param(index, value);
                }
            }
            AutomationKind::Effect(slot, index, descriptor) => {
                let value = descriptor.from_normalized(value);
                if !mods.set_base(Sink::effect(slot, index), value)
                    && let Some(processor) = effects
                        .get_mut(slot)
                        .and_then(|s| s.processor.as_deref_mut())
                {
                    processor.set_param(index, value);
                }
            }
            AutomationKind::Modulator(slot, index, descriptor) => {
                mods.set_base(
                    Sink::modulator(slot, index),
                    descriptor.from_normalized(value),
                );
            }
        }
    }
}

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use media::AudioBuffer;
    use midi::{NoteEventKind, Pitch, Tick, Velocity};
    use project::{
        AutomationPoint, AutomationTarget, Mixer, NewTrack, Note, Project, SourceId, TrackId,
        TrackPosition,
    };

    use crate::{Command, Compiler, Engine, EngineHandle, create};

    const RATE: u32 = 48_000;
    /// At the default 120 BPM.
    const SAMPLES_PER_BEAT: usize = 24_000;

    struct Rig {
        handle: EngineHandle,
        engine: Engine,
        compiler: Compiler,
        audio: Option<(SourceId, Arc<AudioBuffer>)>,
    }

    impl Rig {
        fn new() -> Rig {
            let (handle, engine) = create(RATE as f32);
            Rig {
                handle,
                engine,
                compiler: Compiler::new(RATE),
                audio: None,
            }
        }

        fn load(&mut self, project: &Project) {
            let audio = self.audio.clone();
            let arrangement = self.compiler.compile(project, |id| {
                audio
                    .as_ref()
                    .filter(|(s, _)| *s == id)
                    .map(|(_, a)| a.clone())
            });
            self.handle.send(Command::Arrangement(arrangement)).unwrap();
        }

        fn send(&mut self, command: Command) {
            self.handle.send(command).unwrap();
        }

        /// Renders `frames` and returns the left channel.
        fn run(&mut self, frames: usize) -> Vec<f32> {
            let mut out = vec![0.0; frames * 2];
            self.engine.process_interleaved(&mut out, 2);
            out.chunks(2).map(|f| f[0]).collect()
        }
    }

    fn peak(buffer: &[f32]) -> f32 {
        buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    fn note(project: &mut Project, start: i64, len: i64) -> Note {
        Note {
            id: project.new_note_id(),
            start: Tick::new(start),
            len: Tick::new(len),
            pitch: Pitch::A4,
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
        }
    }

    /// A MIDI track with one clip at bar 2 holding one note of `len` ticks.
    fn midi_project(len: i64) -> (Project, TrackId) {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let bar = project.tempo.bar_len();
        let clip = project.add_midi_clip(track, bar, bar * 4).unwrap();
        let note = note(&mut project, 0, len);
        project.set_notes(clip, vec![note]).unwrap();
        (project, track)
    }

    /// An audio track (in a group) with a second of constant 0.5 at beat 2.
    fn audio_project(rig: &mut Rig) -> (Project, TrackId, TrackId) {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let group = project.group_tracks(&[track]).unwrap();
        let source = project.add_source("/tmp/dc.wav".into(), RATE, RATE as u64, 1);
        project
            .add_audio_clip(track, Tick::QUARTER, source)
            .unwrap();
        let samples = vec![0.5; RATE as usize];
        rig.audio = Some((
            source,
            Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
        ));
        (project, track, group)
    }

    #[test]
    fn silent_until_played() {
        let mut rig = Rig::new();
        assert_eq!(peak(&rig.run(512)), 0.0);
    }

    #[test]
    fn midi_notes_play_where_they_are() {
        let mut rig = Rig::new();
        let (project, _) = midi_project(960);
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        let before = rig.run(4 * SAMPLES_PER_BEAT);
        assert_eq!(peak(&before), 0.0, "nothing before bar 2");
        let note = rig.run(SAMPLES_PER_BEAT / 2);
        assert!(peak(&note) > 0.05, "the note plays at bar 2");
    }

    #[test]
    fn audio_clips_play_their_samples() {
        let mut rig = Rig::new();
        let (project, _, _) = audio_project(&mut rig);
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        let before = rig.run(SAMPLES_PER_BEAT);
        assert_eq!(peak(&before), 0.0);
        let clip = rig.run(SAMPLES_PER_BEAT);
        assert!(
            clip[0] > 0.0 && clip[0] < 0.01,
            "fades in instead of clicking"
        );
        assert_eq!(
            clip[1000], 0.5,
            "plays the samples unchanged after the fade"
        );
        let after = rig.run(2 * SAMPLES_PER_BEAT);
        assert_eq!(after[SAMPLES_PER_BEAT], 0.0, "silent once the clip ends");
    }

    #[test]
    fn audio_at_another_rate_lines_up() {
        let mut rig = Rig::new();
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        // The file is 24 kHz; the pool holds it converted to 48 kHz.
        let source = project.add_source("/tmp/ramp.wav".into(), 24_000, 24_000, 1);
        let clip = project.add_audio_clip(track, Tick::ZERO, source).unwrap();
        project
            .set_clip_bounds(clip, Tick::QUARTER, Tick::QUARTER * 2)
            .unwrap();
        let ramp: Vec<f32> = (0..RATE).map(|i| i as f32 / RATE as f32).collect();
        rig.audio = Some((source, Arc::new(AudioBuffer::new(RATE, ramp.clone(), ramp))));
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        let out = rig.run(2 * SAMPLES_PER_BEAT);
        let i = SAMPLES_PER_BEAT + 1000;
        // Half a second into the file at its own rate is 0.5 s into the ramp.
        assert!(
            (out[i] - (24_000.0 + 1000.0) / RATE as f32).abs() < 1e-6,
            "{}",
            out[i]
        );
    }

    #[test]
    fn group_mute_and_solo() {
        let mut rig = Rig::new();
        let (mut project, track, group) = audio_project(&mut rig);
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        rig.run(SAMPLES_PER_BEAT + 2000);
        assert!(peak(&rig.run(100)) > 0.4);

        rig.send(Command::SetTrackMute {
            track: group,
            mute: true,
        });
        rig.run(2000);
        assert_eq!(
            peak(&rig.run(100)),
            0.0,
            "muting the group mutes what's in it"
        );
        rig.send(Command::SetTrackMute {
            track: group,
            mute: false,
        });
        rig.run(2000);

        // Soloing another track silences this one...
        let other = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let solo = Mixer {
            solo: true,
            ..Mixer::default()
        };
        project.set_mixer(other, solo).unwrap();
        rig.load(&project);
        rig.run(2000);
        assert_eq!(peak(&rig.run(100)), 0.0);

        // ...and soloing the track inside the group brings it back.
        project.set_mixer(track, solo).unwrap();
        rig.load(&project);
        rig.run(2000);
        assert!(peak(&rig.run(100)) > 0.4);
    }

    #[test]
    fn snapshots_keep_instruments_playing_and_come_back_to_be_dropped() {
        let mut rig = Rig::new();
        let (mut project, track) = midi_project(960 * 16);
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        rig.run(4 * SAMPLES_PER_BEAT + 4800);

        // An unrelated edit: the held note keeps sounding.
        project.rename_track(track, "renamed").unwrap();
        rig.load(&project);
        rig.run(RATE as usize);
        assert!(peak(&rig.run(100)) > 0.05, "note survives the swap");
        assert!(
            rig.engine.arrangement.nodes[0].processors.is_some(),
            "processors moved over"
        );
        assert!(rig.engine.garbage.slots() < crate::GARBAGE_QUEUE_CAPACITY);
        rig.handle.collect_garbage();
        assert_eq!(rig.engine.garbage.slots(), crate::GARBAGE_QUEUE_CAPACITY);

        // Deleting the note releases it instead of leaving it hanging.
        let clip = project.track(track).unwrap().clips()[0].id();
        project.set_notes(clip, Vec::new()).unwrap();
        rig.load(&project);
        rig.run(RATE as usize);
        assert_eq!(peak(&rig.run(100)), 0.0);
    }

    #[test]
    fn stop_returns_to_where_play_started() {
        let mut rig = Rig::new();
        rig.send(Command::Seek(1000));
        rig.send(Command::SetPlaying(true));
        rig.run(4800);
        assert_eq!(rig.handle.playhead_samples(), 5800);
        rig.send(Command::SetPlaying(false));
        rig.run(480); // 10 ms, longer than the 5 ms fade
        assert_eq!(rig.handle.playhead_samples(), 1000);
        rig.send(Command::SetPlaying(false));
        rig.run(64);
        assert_eq!(
            rig.handle.playhead_samples(),
            0,
            "stopping again goes to the start"
        );
    }

    #[test]
    fn volume_automation_overrides_the_fader() {
        let mut rig = Rig::new();
        let (mut project, track, _) = audio_project(&mut rig);
        let silent = AutomationPoint {
            tick: Tick::ZERO,
            value: 0.0,
            curve: 0.0,
        };
        project
            .set_automation(track, AutomationTarget::Volume, vec![silent])
            .unwrap();
        rig.load(&project);
        rig.send(Command::SetPlaying(true));
        rig.run(SAMPLES_PER_BEAT + 2000);
        assert_eq!(peak(&rig.run(100)), 0.0);
    }

    #[test]
    fn previewed_notes_play_while_stopped() {
        let mut rig = Rig::new();
        let (project, track) = midi_project(960);
        rig.load(&project);
        let on = NoteEventKind::On {
            pitch: Pitch::MIDDLE_C,
            velocity: Velocity::MAX,
        };
        rig.send(Command::PreviewNote { track, event: on });
        assert!(peak(&rig.run(4800)) > 0.05);
        assert_eq!(
            rig.handle.playhead_samples(),
            0,
            "the transport didn't move"
        );
    }

    #[test]
    fn handles_mono_and_surround_devices() {
        let mut rig = Rig::new();
        let (project, track) = midi_project(960);
        rig.load(&project);
        let on = NoteEventKind::On {
            pitch: Pitch::A4,
            velocity: Velocity::MAX,
        };
        rig.send(Command::PreviewNote { track, event: on });

        let mut mono = vec![0.0; 512];
        rig.engine.process_interleaved(&mut mono, 1);
        assert!(peak(&mono) > 0.0);

        let mut surround = vec![0.0; 512 * 6];
        rig.engine.process_interleaved(&mut surround, 6);
        let extra_channels_silent = surround
            .as_chunks::<6>()
            .0
            .iter()
            .all(|f| f[2..].iter().all(|s| *s == 0.0));
        assert!(peak(&surround) > 0.0);
        assert!(extra_channels_silent);
    }
}
