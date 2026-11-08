# Performance ledger: the render path, November 2026

A data-oriented pass over what the audio thread does, done before reaching
for more caching (section 22 of `docs/progress.md` caches idle tracks as
renders; this pass makes the live render itself cheaper).

## Inputs

- **target**: the audio render: `engine` (`Engine::render`, `render_node`),
  the built-in effects and instruments it runs.
- **workload**: real projects played start to end as a sound card would ask:
  `~/Documents/canopy/hiphop-beat.canopy` (the project that underran on
  battery: 5 Wavetables, 6 drum kits, 2 guitars, ~60 effects),
  `~/Documents/canopy/test-140.canopy`, and the synthetic 33-track bench
  song (`bench -- --save target/bench`, then `target/bench/song.canopy`).
- **primary metric**: CPU per second of audio, single-threaded, unpaced
  (`play_project`'s "average % of one core"); lower is better. Paced and
  multi-threaded numbers are checked at the end.
- **out of bounds**: the project file format, parameter ids, the `Effect`
  and `Instrument` traits' contracts, the sound (bit-identical unless a
  change says otherwise and the golden diff is limited to that device).
- **budget**: until the remaining hotspots are each under ~5% or need an
  out-of-bounds change.

## Baseline

- command: `CANOPY_AUDIO_THREADS=0 taskset -c 2 target/release/examples/play_project <project> --unpaced`
  (A/B: `scratchpad/ab.sh <project> <rounds> <binaries…>`, runs interleaved)
- machine: i7-1365U (2 P-cores, 8 E-cores), pinned to P-core cpu2, AC power,
  `performance` platform profile, Linux 7.0, Rust 1.98, `--release` with
  line tables.
- metric (min of 3–4 interleaved runs, spread ±1–2%):
  - hiphop-beat: 22.7% of a core (median callback 1.30 ms at 256 frames)
  - test-140: 9.6%
  - bench song: 15.6%
- counters (hiphop-beat, `perf stat`): IPC 3.5; Top-down: retiring 58%,
  back-end bound 31%, front-end 7%, bad speculation 1.6%. Compute-bound,
  not memory-bound: the levers are doing less and doing it wider.

## Data inventory (hiphop-beat)

- 33 tracks (4 groups), about 60 effects: 13 EQ, 11 compressors, 6 reverbs,
  6 delays, 4 clippers (all 4× oversampling; safety off, off, sample peak,
  true peak), 4 choruses, 3 distortions, 2 hyper, 2 convolvers, a shaper,
  a limiter, a ducker, an analyzer. Instruments: 5 Wavetables, 6 drum kits,
  2 guitars.
- Engine blocks are 64 frames; effects are handed one block per call.

## Napkin math

- Whole project: 150 G cycles for 153.6 s of audio = ~22,000 cycles per
  stereo frame.
- Clipper at 4×: up 72 + 2·24 taps, down 2·24 + 2·136 (4·136 with the
  true-peak halves) per channel: ~900–1,400 multiply-adds per stereo
  sample, ~250–400 cycles at 4-wide SSE. Measured: ~1,300 cycles per
  clipper per sample. Gap 3–5×: per-sample calls, horizontal reductions,
  the ring writes and the meters.

## Hotspots (ranked, share of the render's P-core cycles, hiphop-beat)

1. Clipper (4): FIR dot 12.2%, body 7.3%, true-peak meter 3.6%: ~23%.
2. Reverb (6): ~11% plus its share of `Ring::get_between`.
3. Convolve (2): ~11%.
4. Wavetable (5): ~9%.
5. Distortion (3), with its IIR halfbands: ~7%.
6. Guitar (2): ~6.6%.
7. EQ (13): ~5.6%.
8. Chorus (4) 3.8%, compressor (11) 3.5%, hyper (2) 2.2%; engine plumbing 2.1%.

## Changes

### 1. The clipper's meters only while someone reads them  [KEPT]

- hypothesis: the output meter's true-peak detector (six 24-tap
  interpolations a sample) and two RMS meters run every sample though the
  engine already says when no one watches (`Effect::set_metering`); only
  the compressor and EQ listened.
- predicted: the detector's 3.6% of the render, plus a little.
- result: hiphop-beat 22.7 → 21.7% (−4%); test-140 9.6 → 8.9% (−7%).
- tests: new `effects/tests/metering.rs` (every built-in, and the clipper
  in each safety and oversampling, with meters switched off and on part way:
  the sound identical to the bit).
- files: `effects/src/clipper.rs`.

### 2. The clipper's oversampling a block at a time  [KEPT]

- hypothesis: every input sample called the FIR `dot` once per filter, out
  of line, ending in a horizontal sum, through rings written twice per
  sample; at 4× that is a dozen calls a sample per channel. Vectorizing
  along the outputs instead (a tap's coefficient times sixteen inputs in a
  row) needs no horizontal sums; keeping each output's sixteen partial sums
  and adding them in `dot`'s order keeps it bit-identical.
- change: `halfband::fir::convolve` (sixteen outputs at a time, same sums in
  the same order), `History` (a stream's last samples then the block, one
  slice), `UpBlock`/`DownBlock`; the clipper's `process` in three passes
  per 64 samples: the per-sample settings, the paths stage by stage over the
  block, then the safety and output per sample in their original order.
- predicted: the FIR's 12% of the render to a third.
- result: hiphop-beat 22.2 → 20.4% (−8%); test-140 9.2 → 7.7% (−16%). The
  clipper went from ~23% of the render to ~11%.
- tests: new `a_block_at_a_time_is_a_sample_at_a_time_to_the_bit` (block
  sizes 1, 7, 16, 64, 100; up, down, the steep lowpass with its half-sample
  values); the clipper's 11 tests; bit-identical by construction.
- files: `effects/src/halfband/fir.rs`, `effects/src/clipper.rs`.

### 3. Reverb: the lines' reads put together in registers  [REVERTED]

- hypothesis: `Network::tick`'s hottest instructions were vector loads of
  stack arrays just written one float at a time (a store-forwarding stall).
- result: hiphop-beat 20.4 → 21.0% (no better; within noise, slightly worse).
- lesson: the "stall" was sampling skid. Counted alone, one reverb runs at
  IPC 4.2 and retires 2,270 instructions a sample: it isn't waiting, it is
  doing that much. The gather loop is about 44 instructions per line per
  sample; a block form (still lines read as plain copies, which is exact
  because lines never hold -0.0) is the way to cut it.

### 4. Convolve: direct FIR, FFT, partitioning  [KEPT]

One convolver at defaults (3 s Hall, 48 kHz, `profile_effect`): 1.13% of a
core, ~4,970 instructions a sample, IPC 4, memory-bound 12%. Instruction
count, so: do fewer.

- 4a, the direct part's 64-tap sum through the sixteen-sum `dot` instead of
  one serial `.sum()`: 1.13 → 1.05% (−7%; predicted −20%: each sample's
  chain was independent of the next, so the core overlapped them already).
- 4b, the FFT's twiddles per stage, contiguous, the stages run four
  butterflies at a time (`F32x4`), the first two stages without multiplies:
  1.05 → 0.85% (−19%), bit-identical. It had been ~34 instructions a
  butterfly, scalar, the twiddles read with a stride.
- 4c, the gather's sums kept in registers across partitions: no change
  (0.853 → 0.851%), REVERTED: not load/store-bound as modelled.
- 4d, partitioning: head blocks of 64 to 1024 (was 4096), then tails of 512
  to 8192 and 4096 after (was one tail of 2048): 266 → 126 complex
  multiply-adds a sample for the hall, more transforms. 0.851 → 0.67% (−21%,
  predicted −19%). A level the impulse doesn't reach skips its inverse FFT
  (zeros): cabinet 0.486 → 0.46%.
- Altogether, per instance: Hall 1.13 → 0.67%, Room 0.86 → 0.56%, Plate
  0.99 → 0.62%, Spring 0.95 → 0.60%, Cabinet 0.67 → 0.43%.
- sound: identical to the bit through 4b; after 4d within 2.1e-7 of before
  (−133 dB); the test against a direct convolution: largest error 2.5e-7 at
  a level of 0.1; a new impulse still made and faded in within 147 ms.
- result: hiphop-beat 19.6 → 18.6% (−5%).
- later revised (change 14): the last tail went back to blocks of 2048.
- files: `effects/src/convolve.rs`, `dsp/src/simd.rs` (`F32x4::store`).

### 5. Subnormals flushed by the processor while rendering  [KEPT]

- found: the Radio impulse cost 60% more than the Cabinet with the same
  instruction count: 2 million FP microcode assists (subnormal partitions of
  its spectrum). The whole of hiphop-beat took 31 million (~3% of its
  cycles); `dsp::flush` by hand doesn't reach everything.
- change: `dsp::denormals::FlushToZero` (MXCSR FTZ|DAZ on x86-64, FPCR.FZ on
  aarch64), held through `Engine::process_interleaved` and the CLAP bridge's
  `process` (the thread's mode put back after), set for good on the
  engine's worker threads.
- result: assists 31 M → 8.9 K; hiphop-beat 19.1 → 18.6% (−3%); test-140
  7.6 → 7.4% (−3%).
- sound: only values below 1.2e-38 change (they become zero).
- files: `dsp/src/denormals.rs`, `engine/src/audio.rs`, `engine/src/pool.rs`,
  `canopy-clap/src/bridge.rs`.

### 6. Both channels' IIR halfbands side by side  [KEPT]

- hypothesis: the distortion's and waveshaper's oversampler steps each
  channel's halfband alone, each a pair of allpass chains whose every step
  waits on the last: four independent recurrences run as four scalar
  chains. As the four lanes of one vector, `[L even, L odd, R even,
  R odd]`, each allpass is one vector sub, mul and add.
- change: `halfband::iir::Halfbands` (lanes; the coefficients interleaved
  in `Design::lanes`; a design with an odd count leaves the odd lanes as
  they were through its last step), `Oversampler::tick` takes both channels.
- result (alone, `profile_effect`, 48 kHz): distortion as in hiphop-beat
  0.619 → 0.459% (−26%), at drive 33/mix 28 0.734 → 0.602% (−18%), at 8×
  1.161 → 0.995% (−14%); waveshaper 0.728 → 0.600% (−18%), 8× 1.26 → 1.08%.
  hiphop-beat 17.9 → 17.5% (−2%); test-140 7.0 → 6.8% (−4%).
- sound: identical to the bit (new test against the scalar halfband, every
  design width; `--compare` on six settings).
- files: `effects/src/halfband/iir.rs`, `effects/src/shaping.rs`,
  `effects/src/distortion.rs`, `effects/src/waveshaper.rs`.

### 7. The reverb's network a block at a time  [KEPT]

- hypothesis (from change 3's lesson): `tick` gathers 64 scattered floats
  a sample (four taps of sixteen lines), ~44 instructions a line. Every line
  is longer than a 32-sample chunk and its read position moves by a small
  fraction of a sample per chunk, so the block's reads never meet its
  writes and each line's four taps over the block are runs in a row.
- change: `Network::run`: per line, the block's delays accumulated as
  `tick` would, then (whole-sample delay constant, no wrap) the
  interpolation four outputs at a time from plain loads; otherwise the
  scalar reads. Then `mix` (the loss filters, Hadamard, feed; shared with
  `tick`) per sample, then the writes per line. Lines too short for a block
  (a small room at a low rate) go through `tick`. `Reverb::run` in passes
  (pre-delay and diffusers, networks, the rest).
- result (alone): Hall 0.589 → 0.320% (−46%), hiphop-beat's setting 0.459 →
  0.327%, Bass + Gentle 0.499 → 0.354%, Room 0.78 → 0.70%, fast deep
  modulation at size 0 0.461 → 0.419% (Plate unchanged). hiphop-beat 17.5 →
  16.6% (−5%); test-140 6.7 → 6.4% (−4.5%).
- sound: identical to the bit (eight settings, all three types).
- files: `effects/src/reverb.rs`.

Golden after change 7: 26 of 25,711 renders differ, all `canopy.convolve`
(change 4d); every other device identical.

### 8. One FFT for the convolver and the wavetable's morphs  [KEPT]

- found: the morphs' inverse FFT (`wavetable/morph.rs`) was the convolver's
  old one again: twiddles read with a stride, scalar.
- change: the convolver's transform moved to `dsp::fft::Fft`
  (`with_twiddles` lets the morph keep its own single-precision twiddle
  values); both use it.
- result: hiphop-beat 16.6 → 16.1% (−3%); test-140 unchanged (no morphs).
- sound: the convolver identical; the morph within rounding (the first two
  stages no longer multiply by `cos(π/2)` as an f32, -4.4e-8).
- tests: `dsp::fft` against the DFT, every size 1–512, both ways.
- files: `dsp/src/fft.rs`, `effects/src/convolve.rs`,
  `instruments/src/builtin/wavetable/morph.rs`.

### 9. The FIR kernel in AVX, chosen at run time  [KEPT]

- hypothesis: the clipper's FIR (`fir::convolve`, 8.8% of the render) is
  vertical multiply-adds; eight lanes halve its instructions; no fused
  multiply-add, so the same sums to the bit.
- first try, 16 outputs a group: kernel 7.1 → 6.0% of the render only, two
  accumulators per sum, the additions waiting on each other (hiphop-beat
  −1.3%). 32 outputs a group (four accumulators): hiphop-beat 16.0 → 15.5%
  (−3%); test-140 6.3 → 5.9% (−6%).
- sound: identical (the block-vs-sample test runs the AVX path here).
- files: `effects/src/halfband/fir.rs`.

### 10. A still pre-delay read a block at a time  [KEPT]

- found: the reverb's pre-delay was `Ring::get_between` twice a sample
  (2.7% of hiphop-beat's render), a cubic each at the same fraction.
- change: `Ring::push_and_read` (a block pushed in one copy, then a still
  tap's reads interpolated four at a time from the run they lie in; sample
  by sample when the block wraps or the ring hasn't filled), `Tap::still_at`;
  the pre-delay ring a block longer. `line::catmull_rom_x4` shared with the
  reverb's network.
- result: the reverb alone 0.320 → 0.293%; hiphop-beat 15.4 → 15.2%
  (−1.3%); test-140 5.9 → 5.8%.
- sound: identical (new test: every age, integral and not, blocks of 1 to
  33, against sample by sample; five reverb settings).

### 11. The distortion a block at a time, its curve settled per block  [KEPT]

- found: one distortion (hiphop-beat's setting) retired ~1,800
  instructions a sample against ~250 of work: the oversampler's per-sample
  loops over a run-time number of stages, two buffers zeroed a sample, and
  per sub-sample a `match` on the curve, the filter's places, the switch
  and Downsample, with `shaping::bend` out of line.
- change: `Oversampler::run` (each stage over the whole block, both
  channels in lanes); the distortion works out every sample's shaping
  first, then runs the paths over the block, then the switch's crossfade.
  A block with the curve alone (no filter, switch or Downsample) runs a
  loop per curve (`run_plain`, the mode a constant, `bend` inlined). While
  the filter's knobs glide (its tuning moves per sample) it goes sample by
  sample as before.
- result (alone): hiphop-beat's setting 0.459 → 0.292% (−36%; 0.619% at
  the start of the pass); Tube at drive 33 0.602 → 0.463%; 8× 0.995 →
  0.829%. hiphop-beat 15.2 → 14.7% (−3.3%).
- sound: identical (four settings; the per-sample path is unchanged).

### 12. The waveshaper on the same block path  [KEPT]

- change: as 11, in stretches between its tone and drawn-curve updates;
  `Bend` no longer borrows the points (they come alongside).
- result: alone 0.573 → 0.530%; test-140 6.0 → 5.9% (−2%).
- tried and REVERTED: stepping both channels together through the plain
  loop (to overlap their DC and tone recurrences): no faster (0.53 → 0.57%,
  distortion likewise); the sub-sample loop isn't waiting on one channel's
  chain.

### 13. The clipper's true-peak safety reads a block at a time  [KEPT]

- found: in test-140 (three clippers at the default true-peak safety) the
  safety's quarter-sample interpolator, `Up::push` four times a sample per
  clipper, was 8.2% of the render.
- change: `Quarters` on `UpBlock` (a sample's two pushes are a two-sample
  block); when a block is true-peak all through with values between the
  samples (no switch), its readings are worked out for the whole block
  (`Safety::read_block`) and the per-sample safety takes them
  (`tick_read`). The clipper's last pass split in two: the paths'
  crossfade into arrays, then the safety and output per sample as before.
- result: default clipper alone 1.18 → 1.11%; test-140 6.2 → 5.8% (−7%);
  hiphop-beat 15.8 → 15.7% (one of its clippers is true-peak).
- sound: identical (seven settings: every safety, 1× to 16×, smooth knee).

### 14. Convolve: the last tail back to 2048, the multiply-adds in AVX  [KEPT]

- found by `effects/tests/startup.rs` (a running convolver reset and given
  a new impulse against a fresh one): a new impulse starts gathering, and
  then fades in, at the last tail's block boundaries, so change 4d's 4096
  blocks made a knob's new impulse arrive up to 85 ms later (the test's
  166 ms pre-delay no longer hid the switch).
- change: tails of 512 to 4096, then 2048 to the end (180 complex
  multiply-adds a sample for the 3 s hall: still a third fewer than the
  original 266); the gather's complex multiply-add compiled a second time
  with AVX, chosen at run time (no fused multiply-add: identical sums).
- result: alone, the hall 0.705% (AVX against the same build without:
  0.753 → 0.70%); hiphop-beat unchanged against change 13 (15.8–16.2%,
  noise): the AVX pays for the shorter blocks. A new impulse is made and
  faded in within 104 ms (147 ms with 4096 blocks).
- sound: within 1.8e-7 of the original convolver; SSE and AVX identical.

## Final

Measured at the end against the untouched baseline binary, interleaved, the
same session (the machine drifts by a few percent over hours, so only
interleaved pairs count).

| project | baseline | final | change |
|---|---|---|---|
| hiphop-beat, one core, unpaced | 22.2% | 15.0% | −32% (1.48×) |
| test-140, one core, unpaced | 9.1% | 5.4% | −41% (1.69×) |
| bench song (synthetic), one core | 15.5% | 12.6% | −19% |

hiphop-beat as the app plays it (default one worker, paced at 256 frames,
44.1 kHz, 60 s):

| | baseline | final |
|---|---|---|
| performance profile: average | 13.3% | 10.2% |
| median / 99% / 99.9% callback | 0.76 / 1.19 / 1.6 ms | 0.59 / 0.82 / 1.16 ms |
| power-saver profile: average | 50.2% | 40.9% |
| callbacks past the 5.8 ms deadline | 11 | 2 |
| callbacks past half the deadline | 5,150 | 303 |
| median / 99.9% | 2.90 / 5.78 ms | 2.40 / 3.88 ms |

Memory: unchanged but for a few kilobytes (block scratch on the stack, the
reverb's pre-delay a 32-sample block longer, a `History` per halfband).

Sound: golden (25,711 renders of every device) differs only in
`canopy.convolve` (26 renders; the partitioning, within 1.8e-7 of before)
and `canopy.wavetable`'s morph settings (24; the shared FFT's first stages).
Everything else is identical to the bit. Tests: effects, dsp, instruments,
engine and canopy-clap all pass in debug (the real-time no-allocation checks
included).

**Kept**: 1 clipper meters on demand; 2 clipper oversampling in blocks; 4
convolver (direct FIR, FFT, partitioning); 5 FTZ/DAZ on render threads; 6
IIR halfbands in lanes; 7 reverb network in blocks; 8 one shared FFT; 9 AVX
FIR kernel; 10 still pre-delay reads in blocks; 11 distortion in blocks;
12 waveshaper in blocks; 13 true-peak safety readings in blocks; 14
convolver tails revised, AVX multiply-add.

**Reverted, and what they taught**: 3 (the reverb's "store-forwarding
stall" was skid; counted alone the reverb was retiring 2,270 instructions a
sample at IPC 4.2, not waiting); 4c (the convolver's gather wasn't
load/store-bound); 12's sub-step (stepping both channels together through
the curve loops: no overlap to gain). Across every device the lesson was
the same: IPC was already ~3.5–4.2, retiring 57–68%; nothing was waiting on
memory. The waste was instructions: per-sample calls and `match`es, scalar
gathers, horizontal sums, work for meters no one read, a third of a
convolver's multiplications in blocks too small.

**Remaining hotspots** (hiphop-beat, final): EQ 7.8% (13 instances,
already in registers, latency-bound recurrences), reverb network 7.2% (now
spread thinly: the loss filters, the Hadamard mix), chorus 5.2% + its tap
reads 2.3%, compressor 5.1% (11), the clipper's FIR 5.0% (AVX), the
convolver's multiply-adds 5.0%, hyper ~2.5% + its tap reads ~2.5%. Each
under ~8%, and no single one large.

**Next steps**, by payoff:
1. The Wavetable's unison oscillators (`render_warp`: 14% of the bench song,
   ~104 instructions per unison voice per sample at IPC 4.2): vectorize
   along time per voice (phases and gains as scalar prefix sums, the table
   reads gathered, the interpolation four at a time). Perhaps −7% on the
   bench song, −1–2% on hiphop-beat; changes rounding in the wavetable.
2. The chorus and hyper's modulated taps: read a block's taps at once
   (positions per sample first, then the four-sample cubic vectorized over
   the block). −2–3% on hiphop-beat.
3. Cheap vectorized `tanh`/`exp` for the curves the distortion and
   waveshaper run per sub-sample (`dsp::simd` already has `exp2`), over the
   block now that the curve loop is per mode. Changes the sound by ~1e-7.

## After the final measurement (second session)

### 15. Wavetable unison along time  [REVERTED]

- hypothesis: `render_warp`'s plain case (no warp, no second warp, no
  band-limit fade) spends ~104 instructions per unison voice per sample; run
  the recurrences (phase, increment, gains, position) as a scalar pass into
  arrays, then the reads, interpolation and mix four samples at a time.
- result: bit-identical, but 12% slower (7-voice unison 4.3 → 4.85%,
  interleaved). Taking each lane's index out of a vector, gathering the
  four table values per lane and putting vectors back together cost more
  than the vector arithmetic saved: the scalar loop was already as good as
  the reads allow (the earlier round's "the table reads are gathers" again).

### 16. The compressor skips its passes while idle below the knee  [REVERTED]

- hypothesis: with nothing held or being let go and every level in a stretch
  below the knee, the dB levels, the attack/release recurrence and the gains
  all come out as nothing and 1; skip them.
- result: bit-identical, no change on the bench song (12.8%) or hiphop-beat
  (15.1%): in real songs the compressors are rarely idle for a whole
  stretch. What's left of a compressor is its attack/release recurrence, a
  serial chain of ~25 cycles a sample, within ~3× of its floor.

### 17. One design for meters: the engine measures the edges  [KEPT]

- found (changes 1 and 13, and an inventory of all 23 effects): each
  effect measured its own levels, per sample, in its own way: input and
  output peaks re-implemented in a dozen effects, half in linear units and
  half in dB, in two slot orders; some "input" meters after an input gain
  and one on a mono sum; four never read by any panel; and only three
  effects (EQ, compressor, clipper) listening to the opt-in `set_metering`.
- change:
  - `effects::meter`: `Levels` (what an effect asks for, in its
    descriptor), `EdgeMeters` (per channel peak and RMS in and out, and the
    output's true peak), `EdgeLevels` (what's published, in dB).
  - The engine measures them in `chain::run_slot` around `process`, only
    while the panel is read, and only what the descriptor asks; publishes
    them beside the device's own meters (`Meters::read_levels`,
    `Compiler::device_levels`, `Session::device_levels`, `Panel::levels`).
  - `Context::metering` replaces `Effect::set_metering`, so no effect can
    forget it; the internal meters left (the limiter's and clipper's input
    after gain, the convolver's wet peak, the utility's image, the
    compressor's detector) update only while it's on.
  - Moved to the engine: EQ, compressor, limiter, clipper (with true peak),
    transient, waveshaper, distortion, filter, utility. Deleted, unread:
    hyper's and the frequency shifter's output peaks, the reverb's wet peak.
    Left alone on purpose: the analyzer (measuring is its job), the amp,
    multiband, vocoder and ducker (their meters are state they keep anyway).
  - The reset contract: after `reset` every meter reads what a new instance
    reads (`extremes.rs`), instead of only the first slot reading 0.
- result: hiphop-beat 15.0 → 14.9%, test-140 5.47 → 5.43% (mean), bench
  12.6 → 12.5%: within noise, as expected (the expensive meters were
  already gated); the gain is the design.
- sound: unchanged (every migrated effect against the build before, at its
  defaults and 18 other settings: identical). Golden differs only in the 12
  migrated effects' renders, whose meter readings it hashes.
- tests: `meter::edge_meters_read_what_goes_in_and_comes_out`; engine
  `levels_at_an_effects_edges_are_measured_only_while_read`; `metering.rs`
  now drives `Context::metering`; meter tests of moved levels measure them
  as the engine does (`testing::run_measured`).
- also fixed: `effects/tests/performance.rs` holds every effect to the
  reverb, which changes 7 and 10 made 2.0× cheaper, so untouched effects
  (chorus, vocoder, hyper) failed their limits in release builds; the
  reference is now the reverb at what it cost before (`REVERB_SPEEDUP`), so
  each limit keeps the meaning it was written with.

## Workers: what they cost and what they save (7 Nov 2026)

Every number above is one thread. This measures the engine's worker
threads, baseline against final, unpinned as the app runs, three rounds
interleaved, AC, `performance` profile. Total CPU is `perf stat`'s
task-clock over every thread, less a run that only loads the project.

hiphop-beat paced like a sound card (256 frames at 44.1 kHz, 60 s;
callback time as a share of its period, then all threads' CPU as a share
of one core):

| workers | callback, baseline → final | median / 99.9% (final) | total CPU, baseline → final |
|---|---|---|---|
| 0 | 21.4 → 16.2% | 0.92 / 1.79 ms | 21.8 → 16.6% |
| 1 (the default here) | 13.5 → 10.1% | 0.58 / 1.07 ms | 32.9 → 26.0% |
| 2 | 14.8 → 9.7% | 0.55 / 0.91 ms | 54.7 → 40.2% |
| 3 | 14.9 → 9.1% | 0.51 / 1.35 ms | 74.5 → 52.1% |

- A worker costs ~0.55 ms of CPU a callback whatever the project: final,
  one worker against none, 5.67 s over 10,335 callbacks. That is the
  pool's spinning by design (300 µs after a callback's blocks, 250 µs
  before the next is due, `pool.rs`): 9.4% of a core at 256 frames, not
  the "few percent" its comment says. Further workers cost ~12% of a core
  each (final; ~17% for the baseline, whose heavier nodes landed on the
  E-cores).
- One worker takes 38% off the callback for those 9.4 points; a second
  and third take 4% and 10% more for 14 and 26 points. One stays the
  right default on this laptop.
- The pass saved about the same at every worker count (21–30% of total
  CPU, 24–39% of the callback): the effects it made cheaper are on the
  render's critical path as much as off it.
- Unpaced, final: hiphop-beat 14.5 → 8.6 → 8.0 → 7.5% for 0–3 workers;
  the bench song 12.0 → 6.9 → 6.0 → 5.4%, where the baseline got worse
  past one worker (8.5 → 12.4 → 17.5%, 87 s of CPU for 124 s of audio at
  three; not investigated).
- Possible next step, not done: a worker only while the callback needs
  one (hiphop-beat's final single thread is 16% of the period), since its
  spinning is a fixed ~9% of a core on battery.

### Worker prototypes (7 Nov 2026)

Profiled first (one worker, hiphop-beat paced): the worker spent ~45% of
its CPU waiting, nearly all of it between callbacks (`pool::worker`'s spin
and its clock reads); waiting inside a block for other nodes was ~1%. And
unpinned, the worker ran about half its time on E-cores.

Prototypes, chosen at run time (`CANOPY_POOL`, `CANOPY_POOL_PIN`, in
`pool.rs`): `spin` (what shipped), `end` (park as soon as the callback
ends, keep the timed wake before the next), `demand` (park at the end, no
timed wake: the audio thread wakes the workers when a callback starts),
`adaptive` (`demand`, joining only while callbacks take over 40% of the
period), `+pin` (render threads kept to the P-cores). hiphop-beat, paced,
three rounds interleaved, medians; CPU is all threads as a share of a core:

| 256 frames | CPU | callback | 99.9% |
|---|---|---|---|
| no worker | 17.1% | 16.5% | 1.59 ms |
| spin | 26.2% | 10.2% | 0.97 ms |
| end | 21.7% | 10.4% | 0.96 ms |
| end+pin | 21.0% | 10.0% | 0.91 ms |
| demand | 20.4% | 10.9% | 1.00 ms |
| demand+pin | 20.2% | 10.8% | 0.89 ms |
| adaptive | 17.3% | 16.5% | 1.66 ms |
| spin, 2 workers | 39.3% | 9.7% | 0.81 ms |
| demand, 2 workers | 25.6% | 10.0% | 0.85 ms |
| demand+pin, 2 workers | 28.0% | 10.2% | 0.87 ms (a miss) |

| 64 frames (1.45 ms deadline) | CPU | callback | callbacks past half |
|---|---|---|---|
| no worker | 18.4% | 16.9% | 3 |
| spin | 49.2% | 10.3% | 29 (and a miss) |
| end | 28.4% | 9.8% | 0 |
| end+pin | 27.8% | 9.5% | 0 |
| demand | 22.2% | 14.9% | 0 |

- `end` takes the 300 µs spin after each callback away and loses nothing:
  −4.5 points at 256 frames, −21 at 64 (where `spin` spun every 1.45 ms),
  with the same or better callbacks.
- `demand` saves another point or two but pays the wake-up in the callback,
  which matters at small buffers (14.9% against 9.8% at 64 frames).
- Pinning helps a little with one worker; with two it puts three threads
  on two P-cores' hyper-threads and costs more.
- `adaptive` costs what one thread costs while a project is light.
- The output is the same in every mode: the engine's tests pass under
  each (`parallel.rs` compares 1, 3 and 7 workers against one thread).

### 18. Workers park when the callback ends, and join only when needed  [KEPT]

- change: `end` and `adaptive` together. The engine tells the pool when a
  callback ends (`Pool::callback_finished`); workers waiting for another
  block park then, keeping the timed wake before the next callback. The
  workers take part while callbacks need them (`TakingPart`): from the
  start, leaving after two seconds of callbacks under 15% of the period,
  back as soon as one takes over 40%. Pinning not kept.
- expected, from the prototypes: a project one thread plays easily costs
  one thread (hiphop-beat 26.2 → ~17% of a core at 256 frames); a heavy one
  gets the worker at `end`'s cost (−4.5 points at 256 frames, −21 at 64).
- check: a run of the final build on battery (not a valid comparison)
  showed the workers staying in where callbacks took over 40% of the period,
  as designed. The AC comparison of the final build against the shipped one
  is still to do.
- tests: `pool::tests` (when workers leave and come back); the engine's
  tests (`parallel.rs`: 1, 3 and 7 workers identical to one thread).

