# Guitar model against recordings

Recordings (sources and licences in `reference-recordings.tsv`) and the model's
renders (`demos/guitar/compare/`, made with
`cargo run --release -p instruments --example guitar_demos -- demos/guitar/compare --compare`)
measured with the same scripts (`scripts/gtr.py`, `scripts/batch.py`):

- **B**: inharmonicity, `(f_n/n)² = f0²(1 + B n²)` fitted to every partial up to
  3 kHz (n ≤ 30). The guitar's test fits partials 2 to 12 instead, where the
  model is within 10% of the recordings (see below).
- **T60 early / late**: seconds to fall 60 dB, from straight lines fitted to each
  partial's envelope 0.04–0.4 s and 0.8–3.5 s after the onset; medians over
  partials 1–4 and 5–12 (only those within 25 dB of the strongest).
- **Wobble**: RMS deviation (dB) of the envelopes of partials 5–12 from a cubic,
  0.1–3 s: beating.
- **RMS dB/s**: the whole note's fall over its first 1.8 s.
- **Attack centroid**: spectral centroid of the first 20 ms.

Each cell is reference / model. "–" is a partial range with nothing within
25 dB of the strongest partial (the classical's basses are almost all
fundamental, in the recordings and the model alike).

### Nylon, fingers (SpeedY, Freesound 469, CC0)

| note | B | T60 p1-4 early | T60 p1-4 late | T60 p5-12 early | T60 p5-12 late | wobble p5-12 dB | RMS dB/s | attack centroid Hz |
|---|---|---|---|---|---|---|---|---|
| 40 | 1.61e-05 / 8.59e-06 | 8.3 / 13.0 | 26.3 / 11.6 | – / 2.3 | – / 9.5 | – / 0.9 | -5.2 / -6.6 | 154.0 / 136.0 |
| 45 | 1.16e-05 / 9.51e-06 | 12.2 / 7.9 | 18.9 / 8.3 | – / – | – / – | – / – | -3.4 / -7.6 | 131.0 / 165.0 |
| 50 | 0.00e+00 / 9.45e-06 | 7.0 / 5.3 | 9.7 / 9.7 | – / – | – / – | – / – | -2.1 / -10.2 | 158.0 / 219.0 |
| 55 | 4.38e-05 / 7.61e-05 | 3.7 / 3.7 | 4.0 / 3.8 | 1.7 / – | 2.5 / – | 1.1 / – | -13.7 / -13.1 | 214.0 / 274.0 |
| 59 | 7.16e-05 / 6.78e-05 | 3.2 / 3.3 | 3.9 / 4.7 | 1.6 / – | 3.5 / – | 4.5 / – | -12.1 / -14.3 | 578.0 / 342.0 |
| 64 | 6.00e-05 / 7.33e-05 | 3.4 / 1.4 | 3.9 / 4.4 | 1.4 / 0.7 | 3.6 / 4.1 | 3.7 / 2.8 | -14.7 / -16.4 | 358.0 / 602.0 |

### Electric, neck pickup DI (IDMT-SMT-Guitar Strat)

| note | B | T60 p1-4 early | T60 p1-4 late | T60 p5-12 early | T60 p5-12 late | wobble p5-12 dB | RMS dB/s | attack centroid Hz |
|---|---|---|---|---|---|---|---|---|
| 40 | 1.57e-04 / 8.68e-05 | 9.1 / 14.5 | 17.4 / 8.6 | 4.4 / 4.7 | 4.1 / 7.8 | 0.9 / 0.6 | -3.5 / -4.7 | 348.0 / 203.0 |
| 45 | 9.19e-05 / 6.72e-05 | 7.9 / 10.0 | 9.8 / 9.2 | 3.9 / 3.9 | 3.9 / 8.1 | 0.8 / 2.3 | -2.6 / -7.2 | 169.0 / 276.0 |
| 50 | 4.40e-05 / 5.16e-05 | 6.6 / 7.2 | 10.1 / 10.2 | 4.0 / 1.9 | 4.1 / 5.3 | 0.5 / 2.2 | -4.1 / -8.8 | 506.0 / 336.0 |
| 55 | 9.82e-05 / 7.70e-05 | 5.4 / 8.5 | 7.3 / 7.2 | 7.8 / 4.6 | 5.7 / 6.4 | 0.7 / 0.3 | -4.2 / -7.7 | 592.0 / 614.0 |
| 59 | 2.44e-05 / 2.58e-05 | 4.7 / 6.1 | 10.2 / 9.4 | 3.5 / 2.4 | 4.6 / 5.5 | 0.9 / 2.5 | -7.2 / -9.8 | 652.0 / 682.0 |
| 64 | 0.00e+00 / 1.88e-05 | 6.2 / 5.0 | 6.3 / 7.0 | 4.0 / 3.4 | 3.8 / 5.2 | 0.7 / 0.5 | -7.5 / -12.6 | 811.0 / 887.0 |

### Electric, clean (ntnvsl, Freesound 34051, CC0)

| note | B | T60 p1-4 early | T60 p1-4 late | T60 p5-12 early | T60 p5-12 late | wobble p5-12 dB | RMS dB/s | attack centroid Hz |
|---|---|---|---|---|---|---|---|---|
| 40 | 1.69e-04 / 8.68e-05 | 11.6 / 14.5 | 12.8 / 8.6 | 2.4 / 4.7 | 6.0 / 7.8 | 2.1 / 0.6 | -5.8 / -4.7 | 428.0 / 203.0 |
| 45 | 8.81e-05 / 6.72e-05 | 27.4 / 10.0 | 11.0 / 9.2 | 3.7 / 3.9 | 4.7 / 8.1 | 2.2 / 2.3 | -4.2 / -7.2 | 626.0 / 276.0 |
| 50 | 1.95e-04 / 5.16e-05 | 6.5 / 7.2 | 9.2 / 10.2 | 1.7 / 1.9 | 6.2 / 5.3 | 2.2 / 2.2 | -6.8 / -8.8 | 635.0 / 336.0 |
| 55 | 6.42e-05 / 7.70e-05 | 9.7 / 8.5 | 10.6 / 7.2 | 1.6 / 4.6 | 5.6 / 6.4 | 2.3 / 0.3 | -6.7 / -7.7 | 943.0 / 614.0 |
| 64 | 5.34e-05 / 1.88e-05 | 4.0 / 5.0 | 7.3 / 7.0 | 2.6 / 3.4 | 5.2 / 5.2 | 1.6 / 0.5 | -10.0 / -12.6 | 1334.0 / 887.0 |

### Steel, low E (FenrirFangs, Freesound 234739, CC0)

| note | B | T60 p1-4 early | T60 p1-4 late | T60 p5-12 early | T60 p5-12 late | wobble p5-12 dB | RMS dB/s | attack centroid Hz |
|---|---|---|---|---|---|---|---|---|
| 40 | 5.60e-05 / 5.96e-05 | 8.7 / 13.4 | 16.6 / 11.8 | 3.4 / 3.9 | 48.4 / 8.1 | 1.6 / 0.4 | -4.0 / -5.8 | 188.0 / 187.0 |

## Inharmonicity over partials 2 to 12 (`guitar_sound.rs`)

| string | recordings | model |
|---|---|---|
| electric low E | 1.57e-4 (IDMT), 1.69e-4 (ntnvsl) | 1.74e-4 |
| electric A | 0.88e-4 – 0.92e-4 | 0.98e-4 |
| nylon low E | 1.2e-5 – 1.6e-5 | 2.2e-5 |
| nylon B | 7.2e-5 – 7.4e-5 | 6.9e-5 |
| steel low E | 0.56e-4 (FenrirFangs) – 2.1e-4 (NSynth) | 1.1e-4 |

The dispersion filter (four first-order allpasses) is matched at the 14th
partial: below it the stretch follows the string's `B`; above it, on the
low wound strings, it grows more slowly than a stiff string's, which is why
the fit over 30 partials above reads about half the recordings' value there.

## Strums (GuitarSet, 175 strums of 4 or more strings within 60 ms)

| | recordings | model (default 30 ms knob) |
|---|---|---|
| spread, first to last string | median 26 ms, quartiles 21–35 ms, at most 59 | 24 ms at velocity 0.8, 30 at 0.6, 39 at 0.3 |
| gap between strings | median 7.4 ms, quartiles 5.1–10.3 | about 6 ms, ±35% humanized |
| direction | 126 down, 22 up, 27 out of order | alternating on the beat grid, or set |

## Palm mute (EG-IPT, neck humbucker, direct)

| | recordings | model (electric, neck, key-switched) |
|---|---|---|
| fall 30 → 230 ms after the attack | 18 – 31 dB | 28 – 30 dB (A2, D3) |
| spectral centroid 20–120 ms | 126 – 216 Hz (open: 214 – 389) | 133 – 216 Hz (open: 161 – 324) |

## Octave-band balance, first 0.5 s (dB re the note's total, averaged over the six open strings)

| | <250 | 250–500 | 0.5–1k | 1–2k | 2–4k | 4–8k |
|---|---|---|---|---|---|---|
| nylon recordings (fingers) | -4.0 | -16.3 | -21.1 | -28.3 | -36.6 | -55.8 |
| nylon model | -6.3 | -9.5 | -18.5 | -35.0 | -51.3 | -74.2 |
| Strat neck DI recordings | -13.0 | -4.6 | -9.4 | -15.0 | -24.5 | -43.4 |
| electric neck model | -16.0 | -6.6 | -13.2 | -19.2 | -22.5 | -37.6 |
