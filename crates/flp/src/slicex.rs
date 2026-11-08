//! Slicex's saved state: the loop it plays, embedded whole, and where it
//! cuts it.
//!
//! The state holds a header, the path the loop came from, then the loop as
//! a complete WAV file (32-bit float as FL saves it), its slices as the
//! `cue ` chunk's points (frames from the start of the audio) and its
//! tempo, when it has one, in the `acid` chunk. Per-slice settings follow
//! the WAV, 68 bytes a slice, and then the second deck, empty unless the
//! user loaded one; neither is read.
//!
//! Slice `k` plays on key `first + k`. Nothing in the state says which key
//! comes first: projects show slices from C5 (60) up when they all fit
//! below 128, and from key 0 for a vocal cut into 564 slices. In between,
//! the first is taken to be where the last lands on 127.

/// Keys on the keyboard.
const KEYS: usize = 128;
/// The first slice's key when every slice fits above it: FL's C5.
const ROOT: usize = 60;

/// A Slicex state's loop and slices.
#[derive(Debug, Clone, PartialEq)]
pub struct Slicex<'a> {
    pub rate: u32,
    pub channels: u16,
    /// The WAV format tag: 1 integer PCM, 3 float.
    pub format: u16,
    pub bits: u16,
    /// The `data` chunk's bytes.
    data: &'a [u8],
    /// Each slice's first frame, ascending; empty when it isn't cut.
    pub starts: Vec<u64>,
    /// The loop's tempo, when it has one.
    pub tempo: Option<f32>,
}

/// The slices a canopy Slicer keeps of a Slicex: `count` of them from
/// slice `first`, the first of those on key `root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub first: usize,
    pub count: usize,
    pub root: u8,
}

impl<'a> Slicex<'a> {
    pub fn parse(state: &'a [u8]) -> Option<Slicex<'a>> {
        let at = (0..state.len().saturating_sub(12))
            .find(|&i| &state[i..i + 4] == b"RIFF" && &state[i + 8..i + 12] == b"WAVE")?;
        let size = u32_at(state, at + 4)? as usize;
        let riff = state.get(at + 12..(at + 8 + size).min(state.len()))?;
        let mut format = None;
        let mut data = None;
        let mut starts = Vec::new();
        let mut tempo = None;
        let mut i = 0;
        while i + 8 <= riff.len() {
            let id = &riff[i..i + 4];
            let len = u32_at(riff, i + 4)? as usize;
            let body = riff.get(i + 8..i + 8 + len)?;
            match id {
                b"fmt " if len >= 16 => {
                    format = Some((
                        u16_at(body, 0)?,
                        u16_at(body, 2)?,
                        u32_at(body, 4)?,
                        u16_at(body, 14)?,
                    ));
                }
                b"data" => data = Some(body),
                b"cue " => {
                    let count = u32_at(body, 0)? as usize;
                    starts = (0..count)
                        .filter_map(|k| u32_at(body, 4 + 24 * k + 4))
                        .map(u64::from)
                        .collect();
                }
                b"acid" if len >= 24 => {
                    tempo = f32_at(body, 20).filter(|t| t.is_finite() && *t > 0.0);
                }
                _ => {}
            }
            i += 8 + len + (len & 1);
        }
        let (format, channels, rate, bits) = format?;
        if channels == 0 || bits == 0 || bits % 8 != 0 || rate == 0 {
            return None;
        }
        let mut slicex = Slicex {
            rate,
            channels,
            format,
            bits,
            data: data?,
            starts,
            tempo,
        };
        let frames = slicex.frames();
        slicex.starts.retain(|&s| s < frames);
        slicex.starts.sort_unstable();
        slicex.starts.dedup();
        Some(slicex)
    }

    fn block(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits / 8)
    }

    /// The loop's length in frames.
    pub fn frames(&self) -> u64 {
        (self.data.len() / self.block()) as u64
    }

    /// Slices, counting an uncut loop as one.
    pub fn slices(&self) -> usize {
        self.starts.len().max(1)
    }

    /// The key slice 0 plays on (see the module's notes).
    pub fn first_key(&self) -> usize {
        let n = self.slices();
        if ROOT + n <= KEYS {
            ROOT
        } else {
            KEYS.saturating_sub(n)
        }
    }

    /// The (at most) `most` slices to keep so that the keys in `used` are
    /// all played, or as many of them as fit, from the lowest: the first
    /// slices when those reach every key used (on the same keys as FL),
    /// otherwise from the lowest key used.
    pub fn window(&self, used: impl IntoIterator<Item = u8>, most: usize) -> Window {
        let n = self.slices();
        let first_key = self.first_key();
        let count = n.min(most.max(1));
        let played: Vec<usize> = used
            .into_iter()
            .filter_map(|key| usize::from(key).checked_sub(first_key))
            .filter(|&slice| slice < n)
            .collect();
        let lowest = played.iter().copied().min().unwrap_or(0);
        let highest = played.iter().copied().max().unwrap_or(0);
        let first = if highest < count {
            0
        } else {
            lowest.min(n - count)
        };
        Window {
            first,
            count,
            root: u8::try_from(first_key + first).unwrap_or(127).min(127),
        }
    }

    /// Where the window's slices start in [`Slicex::wav`]'s audio.
    pub fn window_starts(&self, window: Window) -> Vec<u64> {
        let (from, _) = self.window_frames(window);
        if self.starts.is_empty() {
            return vec![0];
        }
        self.starts[window.first..window.first + window.count]
            .iter()
            .map(|s| s - from)
            .collect()
    }

    /// The window's audio: from its first slice's start to the slice after
    /// its last (or the end).
    fn window_frames(&self, window: Window) -> (u64, u64) {
        let start = |k: usize| self.starts.get(k).copied();
        let from = start(window.first).unwrap_or(0);
        let to = start(window.first + window.count).unwrap_or(self.frames());
        (from, to.max(from))
    }

    /// The window's audio as a WAV file, in the loop's own format.
    pub fn wav(&self, window: Window) -> Vec<u8> {
        let (from, to) = self.window_frames(window);
        let block = self.block();
        let audio = &self.data[from as usize * block..to as usize * block];
        let mut out = Vec::with_capacity(audio.len() + 44);
        out.extend(b"RIFF");
        out.extend((4 + (8 + 16) + 8 + audio.len() as u32).to_le_bytes());
        out.extend(b"WAVE");
        out.extend(b"fmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(self.format.to_le_bytes());
        out.extend(self.channels.to_le_bytes());
        out.extend(self.rate.to_le_bytes());
        out.extend((self.rate * block as u32).to_le_bytes());
        out.extend((block as u16).to_le_bytes());
        out.extend(self.bits.to_le_bytes());
        out.extend(b"data");
        out.extend((audio.len() as u32).to_le_bytes());
        out.extend(audio);
        out
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    u32_at(b, at).map(f32::from_bits)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// A Slicex state as FL saves one: a header and path, the loop (stereo
    /// float, `frames` long, frame `f` holding `f` in both channels) with
    /// its slices and tempo, and the per-slice records after it.
    pub(crate) fn state(frames: u32, starts: &[u32], tempo: f32) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend(3u16.to_le_bytes());
        fmt.extend(2u16.to_le_bytes());
        fmt.extend(44_100u32.to_le_bytes());
        fmt.extend((44_100u32 * 8).to_le_bytes());
        fmt.extend(8u16.to_le_bytes());
        fmt.extend(32u16.to_le_bytes());
        let data: Vec<u8> = (0..frames)
            .flat_map(|f| [f as f32, f as f32])
            .flat_map(f32::to_le_bytes)
            .collect();
        let mut cue = (starts.len() as u32).to_le_bytes().to_vec();
        for (k, s) in starts.iter().enumerate() {
            cue.extend((k as u32 + 1).to_le_bytes());
            cue.extend(s.to_le_bytes());
            cue.extend(b"data");
            cue.extend([0; 8]);
            cue.extend(s.to_le_bytes());
        }
        let mut acid = vec![0u8; 20];
        acid[4] = 60;
        acid.extend(tempo.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        body.extend(chunk(b"JUNK", &[0; 28]));
        body.extend(chunk(b"fmt ", &fmt));
        body.extend(chunk(b"data", &data));
        body.extend(chunk(b"acid", &acid));
        body.extend(chunk(b"cue ", &cue));
        let mut out = vec![3, 0, 6, 0, 0, 0, 0, 0];
        out.extend(b"FC:\\loops\\break.wav");
        out.extend(b"RIFF");
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out.extend([0; 400]);
        out.extend((starts.len() as u32).to_le_bytes());
        out.extend(vec![0; 68 * starts.len()]);
        out
    }

    #[test]
    fn reads_the_loop_its_slices_and_its_tempo() {
        let s = state(1000, &[0, 250, 500, 750], 168.0);
        let x = Slicex::parse(&s).unwrap();
        assert_eq!((x.rate, x.channels, x.format, x.bits), (44_100, 2, 3, 32));
        assert_eq!(x.frames(), 1000);
        assert_eq!(x.starts, [0, 250, 500, 750]);
        assert_eq!(x.tempo, Some(168.0));
        assert_eq!(x.first_key(), 60);
    }

    #[test]
    fn the_audio_starts_at_the_first_slice_kept() {
        // A break whose first slice starts a little in.
        let s = state(1000, &[100, 400, 700], 0.0);
        let x = Slicex::parse(&s).unwrap();
        assert_eq!(x.tempo, None);
        let w = x.window([60, 62], 64);
        assert_eq!(
            w,
            Window {
                first: 0,
                count: 3,
                root: 60
            }
        );
        assert_eq!(x.window_starts(w), [0, 300, 600]);
        let wav = x.wav(w);
        let back = Slicex::parse(&wav).unwrap();
        assert_eq!(back.frames(), 900);
        // Its first frame is the loop's frame 100.
        assert_eq!(f32_at(back.data, 0), Some(100.0));
    }

    #[test]
    fn many_slices_start_low_and_keep_the_window_the_notes_play() {
        let starts: Vec<u32> = (0..564).map(|k| k * 10).collect();
        let s = state(5640, &starts, 0.0);
        let x = Slicex::parse(&s).unwrap();
        assert_eq!(x.first_key(), 0);
        // Notes from key 15 to 40: slices 15 to 40, all within the first
        // 64, which stay on their keys.
        let w = x.window([20, 15, 40], 64);
        assert_eq!(
            w,
            Window {
                first: 0,
                count: 64,
                root: 0
            }
        );
        assert_eq!(x.window_starts(w)[40], 400);
        // Notes reaching past them: kept from the lowest.
        let w = x.window([100, 70], 64);
        assert_eq!(
            w,
            Window {
                first: 70,
                count: 64,
                root: 70
            }
        );
        let kept = x.window_starts(w);
        assert_eq!(kept.len(), 64);
        assert_eq!(kept[0], 0);
        assert_eq!(kept[25], 250);
        assert_eq!(Slicex::parse(&x.wav(w)).unwrap().frames(), 640);
        // Ninety slices start where the last lands on 127, and a note on
        // the last keeps the window ending there.
        let starts: Vec<u32> = (0..90).map(|k| k * 10).collect();
        let s = state(900, &starts, 0.0);
        let x = Slicex::parse(&s).unwrap();
        assert_eq!(x.first_key(), 38);
        assert_eq!(
            x.window([127], 64),
            Window {
                first: 26,
                count: 64,
                root: 64
            }
        );
    }

    #[test]
    fn states_without_a_loop_are_none() {
        assert_eq!(Slicex::parse(&[0; 64]), None);
        assert_eq!(Slicex::parse(b"RIFF\x04\x00\x00\x00WAVE"), None);
    }
}
