# Compact, more visual instrument panels — design

Date: 29 October 2026. Approved by the user in conversation ("All three, slimmer";
"Whole Wavetable, then the rest").

## Goal

Every built-in instrument's panel fits the device view of a 1280×800 window (about
1040 px across and 250 px down at the detail panel's default height) without scrolling
sideways; controls sit in labelled, coloured sections; displays take the height left over
so a taller panel shows bigger visuals rather than empty space. The Wavetable first, then
the Drum Synth, Guitar, Poly Synth and Sampler.

Not changing: sound, parameters, descriptors, file format, presets. Everything a knob does
today (modulation rings, drag-to-modulate, right-click menus, automation, keyboard focus,
value entry) still works on every knob.

## Shared pieces

- **Section card** (`widgets::chrome::section`, or `devices::section`): a titled group on a
  card with a thin strip of its color across the top. The header row holds the title and
  optionally the section's on switch and menus; the body is laid out by the caller. Colors
  follow function, matching the mod-source groups: oscillators blue (A, B, C each their own
  shade), filters amber, envelopes green, LFOs sky, macros orange, the rest neutral.
- **Compact knob** (`Knob::compact()` and `Panel` support): diameter 24, width 46. The name
  sits under the circle; while hovered, dragged or keyboard-focused the name's place shows
  the value instead (and a tooltip shows it too). Height ≈ 38 px instead of 62. Rings and
  every interaction as the full knob. The accessible label stays the name, so tests that
  find knobs by name keep working.
- **Number field** for whole-number settings (octave, unison voices, polyphony): a small
  drag field with its name, automation menu as a knob's.
- **Displays fill the height left** after the controls in their section, with a minimum.

## Wavetable

- Page tabs move into the instrument's title bar (beside the picker, Plugins, Presets).
- The mod sources get a row of their own as small colored chips, each group named once
  (`ENV A 2 3 4 5 6 · LFO 1…8 · NOTE … · CTRL …`), about 600 px. Drag and click behave as now.
- **Oscillators page**, left to right: Osc A, Osc B, Osc C, Sub/Noise, Filter 1|2, Macros.
  - Oscillator card (~200 px): header with on switch, table menu and a `⋯` menu (Load,
    Edit, options); warp modes and morph; the 3D table filling the height; a row of fields
    (Octave, Unison voices, Stack); 15 compact knobs four to a row:
    Table (Position, Morph, Warp, Warp 2), Output (Level, Pan, Semi, Fine),
    Unison (Detune, Blend, Width, Random), Spread (Warp Spr, Pos Spr, Phase).
  - Sub and Noise stacked in one narrow card; the filter card with its curve, six compact
    knobs and routing.
  - Macros and output: a ~110 px column of compact macro knobs, the output volume and a
    small scope (it was 260 px and on every page).
- **Envelopes & LFOs**: an envelope card and an LFO card, each with chips to pick which
  (a tiny drawing of its shape on each chip), the editor filling the card, compact knobs
  under it, Sync/Retrig in the card's header.
- **Matrix**: only routes in use, then a `+ Route` row; each row a colored source chip,
  the destination, a bipolar amount bar and a curve thumbnail.
- **Voicing**: cards for Voices (mode, polyphony, quality), Glide (with its curve),
  Expression (bend, velocity), and a small per-oscillator table.

## Other instruments

- **Drum Synth**: pad grid stays; the pad's six layers as chips with a mini waveform each;
  the layer's controls in cards (Source, Pitch envelope, Amp envelope with its curve,
  Filter, Output) in two compact rows.
- **Guitar**: fretboard stays; knobs in String, Playing, Noises and Amp cards; body,
  technique and pickup choices as segmented controls in a header.
- **Poly Synth**: oscillator card drawing the chosen wave, filter card with a response
  curve, the envelope card as now.
- **Sampler**: a key-map display of the loaded regions' key ranges; Amp, Pitch and
  Envelope cards (with a small attack/release curve).

## Testing

- A UI test per instrument at 1280×800: the panel is no wider than the space it has.
- Existing UI tests keep passing (knobs found by name).
- Tour screenshots before and after; `ui_tests/perf.rs` device view no slower.
