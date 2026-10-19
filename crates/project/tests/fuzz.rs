//! Random editing sessions, checking the project's invariants after every
//! edit: unique ids, sorted and non-overlapping clips, audio clips inside
//! their files, effect graphs that can play (and are only there when not
//! linear), modulation that refers only to what's there, and save/load
//! round trips.

use std::collections::HashSet;

use midi::{Pitch, Tick, Velocity};
use project::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo) as u64)) as i64
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        if v.is_empty() {
            None
        } else {
            Some(v[(self.next() % v.len() as u64) as usize])
        }
    }
    fn chance(&mut self, n: u64) -> bool {
        self.next().is_multiple_of(n)
    }
}

fn all_clips(p: &Project) -> Vec<(TrackId, ClipId)> {
    let mut v = Vec::new();
    p.visit(|t, _| {
        for c in t.clips() {
            v.push((t.id, c.id()));
        }
    });
    v
}

fn tracks_of(p: &Project, f: impl Fn(&Track) -> bool) -> Vec<TrackId> {
    let mut v = Vec::new();
    p.visit(|t, _| {
        if f(t) {
            v.push(t.id)
        }
    });
    v
}

/// Audio clips may overlap only where one of them is fading: crossfades
/// overlap the two clips' edges, and splitting inside one leaves pieces
/// that lie within the other's fade.
fn crossfaded(p: &Project, a: ClipRef, b: ClipRef) -> bool {
    let (ClipRef::Audio(x), ClipRef::Audio(y)) = (a, b) else {
        return false;
    };
    let overlap = (y.start.max(x.start), p.clip_end(a).min(p.clip_end(b)));
    let slack = Tick::new(2);
    [(x, a), (y, b)].iter().any(|&(clip, r)| {
        let end = p.clip_end(r);
        let fade_in = (
            clip.start,
            clip.start + p.audio_duration(clip, clip.fade_in),
        );
        let fade_out = (end - p.audio_duration(clip, clip.fade_out), end);
        [fade_in, fade_out]
            .iter()
            .any(|&(s, e)| e > s && overlap.0 + slack >= s && overlap.1 <= e + slack)
    })
}

pub fn check(p: &Project, check_overlap: bool, ctx: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let mut ids = HashSet::new();
    let mut note_ids = HashSet::new();
    p.visit(|t, _| {
        if !ids.insert(t.id.0) {
            errs.push(format!("{ctx}: dup id track {}", t.id));
        }
        let clips = t.clips();
        for w in clips.windows(2) {
            if w[0].start() > w[1].start() {
                errs.push(format!("{ctx}: unsorted clips on {}", t.id));
            }
        }
        let pairs = clips
            .iter()
            .enumerate()
            .flat_map(|(i, a)| clips[i + 1..].iter().map(move |b| [*a, *b]));
        for w in pairs {
            if check_overlap && p.clip_end(w[0]) > w[1].start() && !crossfaded(p, w[0], w[1]) {
                errs.push(format!(
                    "{ctx}: overlap on track {}: {:?}..{:?} and {:?}..{:?}",
                    t.id,
                    w[0].start(),
                    p.clip_end(w[0]),
                    w[1].start(),
                    p.clip_end(w[1])
                ));
            }
        }
        for c in clips {
            if !ids.insert(c.id().0) {
                errs.push(format!("{ctx}: dup id clip {}", c.id()));
            }
            match c {
                ClipRef::Midi(m) => {
                    if m.length <= Tick::ZERO {
                        errs.push(format!("{ctx}: midi clip {} len {:?}", m.id, m.length));
                    }
                    for n in &m.notes {
                        if !note_ids.insert(n.id.0) {
                            errs.push(format!("{ctx}: dup note id {}", n.id));
                        }
                    }
                }
                ClipRef::Audio(a) => {
                    let s = p.source(a.source).unwrap();
                    if a.fade_in + a.fade_out > a.played() {
                        errs.push(format!(
                            "{ctx}: audio clip {} fades {}+{} > {}",
                            a.id,
                            a.fade_in,
                            a.fade_out,
                            a.played()
                        ));
                    }
                    if a.looped && (a.play_len == 0 || a.loop_phase >= a.source_len) {
                        errs.push(format!(
                            "{ctx}: audio clip {} loop {} of {} phase {}",
                            a.id, a.play_len, a.source_len, a.loop_phase
                        ));
                    }
                    let speed = a.stretch.effective_speed(&p.tempo);
                    if !(speed.is_finite() && speed > 0.0) {
                        errs.push(format!("{ctx}: audio clip {} speed {speed}", a.id));
                    }
                    if a.source_len == 0 {
                        errs.push(format!("{ctx}: audio clip {} empty", a.id));
                    }
                    if a.source_offset + a.source_len > s.frames {
                        errs.push(format!(
                            "{ctx}: audio clip {} past file: {}+{} > {}",
                            a.id, a.source_offset, a.source_len, s.frames
                        ));
                    }
                }
            }
        }
    });
    errs.extend(check_chains(p, ctx, &mut ids));
    errs
}

/// The effect chains, their graphs and the modulation: ids unique with the
/// rest, every graph valid, non-linear and with its pool in sound order,
/// and nothing pointing at what isn't there.
fn check_chains(p: &Project, ctx: &str, ids: &mut HashSet<u32>) -> Vec<String> {
    let mut errs = Vec::new();
    let mut tracks = HashSet::new();
    p.visit(|t, _| {
        tracks.insert(t.id);
    });
    let mut chain = |owner: String, devices: &[Device], routing: Option<&Routing>| {
        for d in devices {
            if !ids.insert(d.id.0) {
                errs.push(format!("{ctx}: dup id device {}", d.id));
            }
        }
        let Some(routing) = routing else {
            return;
        };
        for n in &routing.nodes {
            if !ids.insert(n.id.0) {
                errs.push(format!("{ctx}: dup id node {} on {owner}", n.id));
            }
        }
        if let Err(err) = routing.validate(devices) {
            errs.push(format!("{ctx}: graph on {owner}: {err}"));
        }
        if routing.linear_chain().is_some() {
            errs.push(format!("{ctx}: linear graph kept on {owner}"));
        }
        let order: Vec<DeviceId> = routing
            .topological_order()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| match routing.node(id)?.kind {
                NodeKind::Device(d) => Some(d),
                _ => None,
            })
            .collect();
        let pool: Vec<DeviceId> = devices.iter().map(|d| d.id).collect();
        if order != pool {
            errs.push(format!("{ctx}: pool on {owner} not in sound order"));
        }
    };
    chain(
        "master".into(),
        &p.master_devices,
        p.master_routing.as_ref(),
    );
    p.visit(|t, _| chain(format!("track {}", t.id), &t.devices, t.routing.as_ref()));
    p.visit(|t, _| {
        for m in &t.modulators {
            if !ids.insert(m.id.0) {
                errs.push(format!("{ctx}: dup id modulator {}", m.id));
            }
            let listening = match m.listen {
                Some(Listen::Track(track)) => tracks.contains(&track),
                Some(Listen::Node(port)) => t
                    .routing
                    .as_ref()
                    .is_some_and(|r| r.node(port.node).is_some()),
                Some(Listen::Input) | None => true,
            };
            if !listening {
                errs.push(format!("{ctx}: modulator {} listens to nothing", m.id));
            }
        }
        for r in &t.modulations {
            if !ids.insert(r.id.0) {
                errs.push(format!("{ctx}: dup id route {}", r.id));
            }
            let scale = r.scale_by.as_ref().is_none_or(|s| t.has_source(s));
            if !t.has_source(&r.source) || !t.has_target(&r.target) || !scale {
                errs.push(format!("{ctx}: route {} on {} dangles", r.id, t.id));
            }
        }
    });
    errs
}

/// The chains there are: the master's (`None`) and every track's.
fn chains(p: &Project) -> Vec<Option<TrackId>> {
    std::iter::once(None)
        .chain(p.track_ids().into_iter().map(Some))
        .collect()
}

fn all_devices(p: &Project) -> Vec<DeviceId> {
    let mut v: Vec<DeviceId> = p.master_devices.iter().map(|d| d.id).collect();
    p.visit(|t, _| v.extend(t.devices.iter().map(|d| d.id)));
    v
}

const EFFECTS: [&str; 4] = [
    "canopy.eq",
    "canopy.compressor",
    "canopy.reverb",
    "canopy.utility",
];

/// A cable between two nodes of a chain's graph (making one if it has
/// none), from earlier in sound order to later, so it never loops.
fn add_cable(p: &mut Project, rng: &mut Rng, owner: Option<TrackId>) -> Result<(), EditError> {
    let devices = p.devices(owner).ok_or(EditError::NothingSelected)?.to_vec();
    let gain = [0.0, -6.0, 3.0][rng.range(0, 3) as usize];
    let muted = rng.chance(5);
    let (a, b) = (rng.next() as usize, rng.next() as usize);
    let sidechain = rng.chance(3);
    let routing = p.ensure_routing(owner)?;
    let order = routing.topological_order().unwrap_or_default();
    if order.len() < 2 {
        return Ok(());
    }
    let i = a % (order.len() - 1);
    let j = i + 1 + b % (order.len() - 1 - i);
    let (from, to) = (
        routing.node(order[i]).unwrap(),
        routing.node(order[j]).unwrap(),
    );
    let inputs = to.kind.inputs(&devices);
    if from.kind.outputs() == 0 || inputs == 0 {
        return Ok(());
    }
    let cable = Cable {
        from: PortRef {
            node: from.id,
            port: 0,
        },
        to: PortRef {
            node: to.id,
            port: u8::from(sidechain && inputs > 1),
        },
        gain_db: gain,
        muted,
    };
    if routing.cable(cable.key()).is_none() {
        routing.cables.push(cable);
    }
    Ok(())
}

/// A modulator on `track`, and a route from it (or the instrument) to
/// something on the track.
fn add_modulation(p: &mut Project, rng: &mut Rng, track: TrackId) -> Result<(), EditError> {
    let id = p.new_modulator_id();
    let route_id = p.new_mod_route_id();
    let tracks = p.track_ids();
    let t = p.track(track).ok_or(EditError::NoSuchTrack(track))?;
    let midi = matches!(t.kind, TrackKind::Midi { .. });
    let listen = match rng.range(0, 4) {
        0 => Some(Listen::Input),
        1 => rng.pick(&tracks).map(Listen::Track),
        2 => t.routing.as_ref().and_then(|r| r.nodes.first()).map(|n| {
            Listen::Node(PortRef {
                node: n.id,
                port: 0,
            })
        }),
        _ => None,
    };
    let mut targets = vec![
        AutomationTarget::Volume,
        AutomationTarget::Pan,
        AutomationTarget::ModulatorParam {
            modulator: id,
            param: "rate".into(),
        },
    ];
    for d in &t.devices {
        targets.push(AutomationTarget::DeviceParam {
            device: d.id,
            param: "mix".into(),
        });
    }
    if midi {
        targets.push(AutomationTarget::InstrumentParam("cutoff".into()));
    }
    let target = targets[rng.range(0, targets.len() as i64) as usize].clone();
    let source = if midi && rng.chance(2) {
        ModSource::Instrument("lfo1".into())
    } else {
        ModSource::Modulator { id, output: 0 }
    };
    let scale_by = rng
        .chance(3)
        .then_some(ModSource::Modulator { id, output: 0 });
    let t = p.track_mut(track).ok_or(EditError::NoSuchTrack(track))?;
    t.modulators.push(Modulator {
        id,
        kind: ModulatorRef {
            id: "canopy.lfo".into(),
            version: "1".into(),
        },
        params: Default::default(),
        name: None,
        listen,
        folded: false,
    });
    t.modulations.push(ModRoute {
        id: route_id,
        source,
        target,
        amount: 0.5,
        polarity: Polarity::Bipolar,
        curve: 0.0,
        scale_by,
        bypassed: false,
    });
    Ok(())
}

/// Every clip as `id:start..end`, audio clips with their frames, for failure reports.
fn describe(p: &Project) -> String {
    let mut out = Vec::new();
    p.visit(|t, _| {
        for c in t.clips() {
            let extra = match c {
                ClipRef::Audio(a) => format!(
                    "[{}+{}~{}/{}~of~{}@{}]",
                    a.source_offset,
                    a.source_len,
                    a.fade_in,
                    a.fade_out,
                    p.source(a.source).unwrap().frames,
                    p.source(a.source).unwrap().sample_rate
                ),
                ClipRef::Midi(_) => String::new(),
            };
            out.push(format!(
                "t{} c{}:{}..{}{extra}",
                t.id,
                c.id(),
                c.start().get(),
                p.clip_end(c).get()
            ));
        }
    });
    out.join(" ")
}

fn note(p: &mut Project, start: i64) -> Note {
    Note {
        id: p.new_note_id(),
        start: Tick::new(start),
        len: Tick::new(100),
        pitch: Pitch::new(60).unwrap(),
        velocity: Velocity::MAX,
        release_velocity: Velocity::CENTER,
        muted: false,
    }
}

fn run(seed: u64, tempo_changes: bool) -> Vec<String> {
    let mut rng = Rng(seed);
    let mut p = Project::with_default_tracks();
    let mut h = History::default();
    let mut errs = Vec::new();
    for step in 0..400 {
        let op = rng.range(0, 30);
        let clips = all_clips(&p);
        let midi_tracks = tracks_of(&p, |t| matches!(t.kind, TrackKind::Midi { .. }));
        let audio_tracks = tracks_of(&p, |t| matches!(t.kind, TrackKind::Audio { .. }));
        let all_tracks = p.track_ids();
        let ctx = format!("seed {seed} step {step} op {op}");
        let before = p.clone();
        match op {
            0 => {
                if let Some(t) = rng.pick(&midi_tracks) {
                    let start = Tick::new(rng.range(0, 40_000));
                    let len = Tick::new(rng.range(1, 8000));
                    let _ = h.edit(&mut p, "add", |p| {
                        let c = p.add_midi_clip(t, start, len)?;
                        let n = note(p, 50);
                        p.set_notes(c, vec![n])
                    });
                }
            }
            1 => {
                if let Some(t) = rng.pick(&audio_tracks) {
                    let rate = [8000u32, 22050, 44100, 48000, 96000][rng.range(0, 5) as usize];
                    let frames = rng.range(1, 400_000) as u64;
                    let start = Tick::new(rng.range(0, 40_000));
                    let _ = h.edit(&mut p, "add audio", |p| {
                        let s = p.add_source(
                            format!("/tmp/{}.wav", rng.next()).into(),
                            rate,
                            frames,
                            2,
                        );
                        p.add_audio_clip(t, start, s)
                    });
                }
            }
            2 | 3 => {
                // consistent move (same delta, same track)
                if !clips.is_empty() {
                    let n = rng.range(1, 4) as usize;
                    let mut sel = HashSet::new();
                    for _ in 0..n {
                        sel.insert(rng.pick(&clips).unwrap());
                    }
                    let delta = rng.range(-5000, 5000);
                    let earliest = sel
                        .iter()
                        .map(|(_, c)| p.clip(*c).unwrap().1.start().get())
                        .min()
                        .unwrap();
                    let delta = delta.max(-earliest);
                    let moves: Vec<ClipMove> = sel
                        .iter()
                        .map(|(t, c)| ClipMove {
                            clip: *c,
                            track: *t,
                            start: p.clip(*c).unwrap().1.start() + Tick::new(delta),
                        })
                        .collect();
                    let _ = h.edit(&mut p, "move", |p| p.move_clips(&moves));
                }
            }
            4 | 5 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let (_, clip) = p.clip(c).unwrap();
                    let (s, e) = (clip.start().get(), p.clip_end(clip).get());
                    let (mut ns, mut ne) = if rng.chance(2) {
                        (rng.range(s - 3000, e), e)
                    } else {
                        (s, rng.range(s + 1, e + 3000))
                    };
                    if let ClipRef::Audio(a) = clip {
                        let src = p.source(a.source).unwrap();
                        let to_ticks = |f: u64| {
                            Tick::new(
                                p.tempo.frames_to_ticks(f as f64, src.sample_rate).floor() as i64
                            )
                        };
                        let earliest = (a.start - to_ticks(a.source_offset)).max(Tick::ZERO).get();
                        let latest = (a.start + to_ticks(src.frames - a.source_offset)).get();
                        ns = ns.max(earliest);
                        ne = ne.min(latest);
                    }
                    ns = ns.max(0);
                    if ne > ns {
                        let _ = h.edit(&mut p, "bounds", |p| {
                            p.set_clip_bounds(c, Tick::new(ns), Tick::new(ne))
                        });
                    }
                }
            }
            6 => {
                if !clips.is_empty() {
                    let sel: Vec<ClipId> = (0..rng.range(1, 4))
                        .map(|_| rng.pick(&clips).unwrap().1)
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    let at = Tick::new(rng.range(0, 45_000));
                    let _ = h.edit(&mut p, "split", |p| p.split_clips(&sel, at));
                }
            }
            7 => {
                if !clips.is_empty() {
                    let sel: Vec<ClipId> = (0..rng.range(1, 4))
                        .map(|_| rng.pick(&clips).unwrap().1)
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    let _ = h.edit(&mut p, "dup", |p| p.duplicate_clips(&sel));
                }
            }
            8 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let _ = h.edit(&mut p, "del", |p| p.delete_clips(&[c]));
                }
            }
            9 => {
                if tempo_changes {
                    let bpm = rng.range(20, 999) as f64;
                    p.set_tempo_bpm(bpm);
                }
            }
            10 => {
                let kind =
                    [NewTrack::Midi, NewTrack::Audio, NewTrack::Group][rng.range(0, 3) as usize];
                let groups = tracks_of(&p, |t| t.is_group());
                let parent = if rng.chance(2) {
                    rng.pick(&groups)
                } else {
                    None
                };
                let _ = h.edit(&mut p, "add track", |p| {
                    p.add_track(
                        kind,
                        TrackPosition {
                            parent,
                            before: None,
                        },
                    )
                });
            }
            11 => {
                let n = rng.range(1, 4);
                let sel: Vec<TrackId> = (0..n).filter_map(|_| rng.pick(&all_tracks)).collect();
                let _ = h.edit(&mut p, "group", |p| p.group_tracks(&sel));
            }
            12 => {
                let groups = tracks_of(&p, |t| t.is_group());
                if let Some(g) = rng.pick(&groups) {
                    let _ = h.edit(&mut p, "ungroup", |p| p.ungroup(g));
                }
            }
            13 => {
                let n = rng.range(1, 3);
                let sel: Vec<TrackId> = (0..n).filter_map(|_| rng.pick(&all_tracks)).collect();
                let groups = tracks_of(&p, |t| t.is_group());
                let parent = if rng.chance(2) {
                    rng.pick(&groups)
                } else {
                    None
                };
                let siblings: Vec<TrackId> = match parent {
                    None => p.tracks.iter().map(|t| t.id).collect(),
                    Some(g) => p
                        .track(g)
                        .unwrap()
                        .children()
                        .iter()
                        .map(|t| t.id)
                        .collect(),
                };
                let before = if rng.chance(2) {
                    rng.pick(&siblings)
                } else {
                    None
                };
                let _ = h.edit(&mut p, "move tracks", |p| {
                    p.move_tracks(&sel, TrackPosition { parent, before })
                });
            }
            14 => {
                h.undo(&mut p);
            }
            15 => {
                h.redo(&mut p);
            }
            16 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let a = rng.range(-20_000, 60_000);
                    let b = rng.range(-20_000, 60_000);
                    let _ = h.edit(&mut p, "bounds2", |p| {
                        p.set_clip_bounds(c, Tick::new(a.min(b)), Tick::new(a.max(b)))
                    });
                }
            }
            17 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let what = rng.range(0, 6);
                    let speed =
                        [0.25, 0.5, 0.9, 1.0, 1.5, 2.0, 4.0, 100.0][rng.range(0, 8) as usize];
                    let mode = [
                        StretchMode::Resample,
                        StretchMode::Stretch,
                        StretchMode::Beats,
                    ][rng.range(0, 3) as usize];
                    let bpm = rng.range(-10, 300) as f64;
                    let flag = rng.chance(2);
                    let _ = h.edit(&mut p, "audio", |p| {
                        p.edit_audio_clip(c, |a| match what {
                            0 => a.reversed = !a.reversed,
                            1 => a.looped = !a.looped,
                            2 => a.stretch.speed = speed,
                            3 => a.stretch.mode = mode,
                            4 => {
                                a.stretch.original_bpm = Some(bpm);
                                a.stretch.sync = flag;
                            }
                            _ => a.stretch.transpose = bpm,
                        })
                    });
                }
            }
            18 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let frames = rng.range(-200_000, 200_000);
                    let _ = h.edit(&mut p, "slip", |p| p.slip_clip(c, frames));
                }
            }
            19 => {
                if let Some((_, c)) = rng.pick(&clips) {
                    let at: Vec<Tick> = (0..rng.range(0, 6))
                        .map(|_| Tick::new(rng.range(-1000, 60_000)))
                        .collect();
                    let _ = h.edit(&mut p, "chop", |p| p.chop_clip(c, &at));
                }
            }
            20 => {
                // Hand-set fades may leave an overlap unfaded; that's the
                // user's call, so only fade clips nothing overlaps.
                let alone = |p: &Project, c: ClipId| {
                    let (track, clip) = p.clip(c).unwrap();
                    let (s, e) = (clip.start(), p.clip_end(clip));
                    p.track(track)
                        .unwrap()
                        .clips()
                        .iter()
                        .all(|o| o.id() == c || p.clip_end(*o) <= s || o.start() >= e)
                };
                if let Some((_, c)) = rng.pick(&clips).filter(|&(_, c)| alone(&p, c)) {
                    let (fi, fo) = (rng.range(0, 300_000) as u64, rng.range(0, 300_000) as u64);
                    let _ = h.edit(&mut p, "fades", |p| p.set_clip_fades(c, fi, fo, 0.3, -2.0));
                }
            }
            22 => {
                let owner = rng.pick(&chains(&p)).flatten();
                let effect = EFFECTS[rng.range(0, EFFECTS.len() as i64) as usize];
                let at = rng.chance(2).then(|| rng.range(0, 6) as usize);
                let _ = h.edit(&mut p, "add effect", |p| {
                    p.add_device(owner, EffectRef::builtin(effect), at)
                });
            }
            23 => {
                if let Some(d) = rng.pick(&all_devices(&p)) {
                    let _ = h.edit(&mut p, "remove effect", |p| p.remove_devices(&[d]));
                }
            }
            24 | 25 => {
                let devices = all_devices(&p);
                let sel: Vec<DeviceId> = (0..rng.range(1, 3))
                    .filter_map(|_| rng.pick(&devices))
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let to = rng.pick(&chains(&p)).flatten();
                let at = rng.range(0, 6) as usize;
                if op == 24 {
                    let _ = h.edit(&mut p, "move effects", |p| p.move_devices(&sel, to, at));
                } else {
                    let copied = p.copy_devices(&sel);
                    let _ = h.edit(&mut p, "paste effects", |p| {
                        p.insert_devices(to, Some(at), &copied)
                    });
                }
            }
            26 => {
                let owner = rng.pick(&chains(&p)).flatten();
                let _ = h.edit(&mut p, "patch", |p| {
                    add_cable(p, &mut rng, owner)?;
                    p.normalize_routing(owner)
                });
            }
            27 => {
                if let Some(t) = rng.pick(&all_tracks) {
                    let _ = h.edit(&mut p, "modulate", |p| add_modulation(p, &mut rng, t));
                }
            }
            28 => {
                if let Some(t) = rng.pick(&midi_tracks) {
                    let instrument =
                        ["canopy.poly_synth", "canopy.drums"][rng.range(0, 2) as usize];
                    let _ = h.edit(&mut p, "instrument", |p| {
                        p.set_instrument(t, instruments::InstrumentRef::builtin(instrument))
                    });
                }
            }
            29 => {
                if all_tracks.len() > 2
                    && let Some(t) = rng.pick(&all_tracks)
                {
                    let _ = h.edit(&mut p, "delete track", |p| p.delete_tracks(&[t]));
                }
            }
            21 if !clips.is_empty() => {
                let sel: Vec<ClipId> = (0..rng.range(1, 4))
                    .map(|_| rng.pick(&clips).unwrap().1)
                    .collect();
                let copied = p.copy_clips(&sel);
                let at = Tick::new(rng.range(0, 60_000));
                let onto = if rng.chance(2) {
                    rng.pick(&all_tracks)
                } else {
                    None
                };
                let _ = h.edit(&mut p, "paste", |p| p.paste_clips(&copied, at, onto));
            }
            _ => {}
        }
        let e = check(&p, !tempo_changes, &ctx);
        if !e.is_empty() {
            errs.extend(e);
            errs.push(format!("  before: {}", describe(&before)));
            errs.push(format!("  after:  {}", describe(&p)));
            return errs;
        }
        // round trip
        if step % 50 == 0 {
            let text = p.to_file_string().unwrap();
            let loaded = Project::from_file_string(&text).unwrap();
            assert_eq!(loaded, p);
        }
    }
    errs
}

#[test]
fn fuzz_no_tempo() {
    let mut fails = Vec::new();
    for seed in 1..300u64 {
        let e = run(seed * 7919 + 13, false);
        if !e.is_empty() {
            fails.push(e.join("\n"));
        }
    }
    for f in fails.iter().take(8) {
        eprintln!("{f}\n---");
    }
    eprintln!("failures: {}", fails.len());
    assert!(fails.is_empty());
}

#[test]
fn fuzz_tempo() {
    let mut fails = Vec::new();
    for seed in 1..300u64 {
        let e = run(seed * 104729 + 7, true);
        if !e.is_empty() {
            fails.push(e.join("\n"));
        }
    }
    for f in fails.iter().take(8) {
        eprintln!("{f}\n---");
    }
    eprintln!("failures: {}", fails.len());
    assert!(fails.is_empty());
}
