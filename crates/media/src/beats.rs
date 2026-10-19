//! Beats mode: the material cut at its transients, each slice moved to its
//! new time and played at its original speed. Hits keep their exact sound,
//! which no spectral stretch manages on drums.

/// Fade over the end of a slice cut short by the next one, in seconds. Long
/// enough not to click, short enough to keep the slice's character.
const FADE_OUT: f64 = 0.004;

/// Fade over the start of each slice. Slices start just before their attack,
/// so a millisecond takes the edge off a cut without dulling the hit.
const FADE_IN: f64 = 0.001;

/// A tail quieter than this (-60 dB) ends without a fade when silence
/// follows: the step down to zero is inaudible, and fading it would only eat
/// into the slice.
const QUIET: f32 = 0.001;

/// Moves each slice of the material to `start / speed`, for `frames` output
/// frames. `cuts` are the slice starts: sorted, starting at 0, all within the
/// material.
///
/// Speeding up, a slice is cut where the next one now starts, with a short
/// fade. Slowing down, it plays out whole and silence fills the gap.
pub(crate) fn rearrange(
    rate: u32,
    left: &[f32],
    right: &[f32],
    cuts: &[usize],
    speed: f64,
    frames: usize,
) -> (Vec<f32>, Vec<f32>) {
    let fade_out = (FADE_OUT * rate as f64).round() as usize;
    let fade_in = (FADE_IN * rate as f64).round() as usize;
    let place = |position: usize| (position as f64 / speed).round() as usize;

    let mut out_left = vec![0.0; frames];
    let mut out_right = vec![0.0; frames];
    for (k, &start) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).copied().unwrap_or(left.len());
        let at = place(start);
        if at >= frames {
            break;
        }
        let slot = place(end) - at;
        let cut_short = end - start > slot;
        let len = (end - start).min(slot).min(frames - at);

        let source = [&left[start..][..len], &right[start..][..len]];
        for (channel, out) in source.iter().zip([&mut out_left, &mut out_right]) {
            let out = &mut out[at..][..len];
            out.copy_from_slice(channel);
            // The clip's own start needs no fade: it begins as the material does.
            if k > 0 {
                fade(&mut out[..fade_in.min(len / 2)], true);
            }
        }
        // The clip's end is cut like any other clip; fade only between slices.
        let gap_follows = !cut_short && end - start < slot;
        let tail = fade_out.min(len / 2);
        let needs_fade = cut_short || gap_follows && peak(&source, len - tail) >= QUIET;
        if needs_fade && at + len < frames {
            fade(&mut out_left[at + len - tail..at + len], false);
            fade(&mut out_right[at + len - tail..at + len], false);
        }
    }
    (out_left, out_right)
}

/// The loudest sample from `from` to the end, in either channel.
fn peak(channels: &[&[f32]; 2], from: usize) -> f32 {
    channels
        .iter()
        .flat_map(|c| &c[from..])
        .fold(0.0, |m, s| m.max(s.abs()))
}

/// A raised-cosine fade over the whole slice, in or out.
fn fade(samples: &mut [f32], fade_in: bool) {
    let n = samples.len() as f32;
    for (i, s) in samples.iter_mut().enumerate() {
        let x = (i as f32 + 0.5) / n;
        let gain = 0.5 - 0.5 * (std::f32::consts::PI * x).cos();
        *s *= if fade_in { gain } else { 1.0 - gain };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn run(material: &[f32], cuts: &[usize], speed: f64, frames: usize) -> Vec<f32> {
        let (left, right) = rearrange(RATE, material, material, cuts, speed, frames);
        assert_eq!(left, right);
        assert_eq!(left.len(), frames);
        left
    }

    #[test]
    fn slowing_down_leaves_gaps_with_faded_edges() {
        let output = run(&[0.5; 3000], &[0, 1000, 2000], 0.5, 6000);
        for start in [0, 2000, 4000] {
            let slice = &output[start..start + 1000];
            // A 1 ms fade-in, except at the very start.
            if start > 0 {
                assert!(slice[0] < 0.01 && slice[47] > 0.49);
            }
            assert!(slice[48..800].iter().all(|&s| s == 0.5));
            // A 4 ms fade-out into the gap.
            assert!(slice[999] < 0.01 && slice[808] > 0.49);
            assert!(output[start + 1000..start + 2000].iter().all(|&s| s == 0.0));
        }
    }

    #[test]
    fn speeding_up_cuts_each_slice_at_the_next() {
        let output = run(&[0.5; 3000], &[0, 1000, 2000], 2.0, 1500);
        for start in [0, 500, 1000] {
            let slice = &output[start..start + 500];
            assert!(slice[48..300].iter().all(|&s| s == 0.5));
            if start < 1000 {
                // Faded out to meet the next slice.
                assert!(slice[499] < 0.01 && slice[308] > 0.49);
            } else {
                // The last runs into the end of the clip, which cuts it.
                assert_eq!(slice[499], 0.5);
            }
        }
    }

    #[test]
    fn a_tail_that_has_died_away_is_left_alone() {
        let mut material = vec![0.0; 2000];
        material[..500].fill(1.0);
        material[1000..1500].fill(1.0);
        let output = run(&material, &[0, 1000], 0.5, 4000);
        assert_eq!(output[..1000], material[..1000]);
        assert!(output[1000..2000].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn slices_past_the_end_are_dropped() {
        let output = run(&[0.5; 3000], &[0, 1000, 2000], 0.5, 2500);
        assert!(output[2048..2500].iter().all(|&s| s == 0.5));
        assert!(run(&[0.5; 100], &[0], 0.5, 0).is_empty());
        assert_eq!(run(&[], &[0], 2.0, 10), vec![0.0; 10]);
    }
}
