## Wishlist (in no particular order)

- A fully static, fully working Windows build, with VST3 support
- CLAP support on Windows and Linux
- The Wavetable synth, closer to Serum 2: an effects rack inside it, editing
  wavetables (and spectral morph modes), more warp modes and filter types,
  freeform LFO points and drawable envelopes, more LFOs and envelopes, unison
  detune modes, drag-and-drop modulation, aftertouch and MPE. (Done: a third
  oscillator, user wavetables from WAV files, `MAX_METERS` raised to 64.)
- The FL Studio importer, beyond what it does now: tempo automation and
  time-signature changes, markers, mixer sends, note fine pitch, pan and
  slides, and the step sequencer's automation. Effect settings come over for
  FL's EQ 2, Compressor, Reeverb 2, Delay 3, Balance, Stereo Enhancer/Shaper,
  Center, Chorus, Flangus and Transient Processor, and for OTT, TDR Kotelnikov
  and Nova, Valhalla Supermassive, Kilohearts' Compactor, Flanger, Phaser and
  Transient Shaper, and Vital. Still to
  decode: Fruity Limiter, Maximus, Vocodex, Melda's plugins, Leveling Tool,
  epicVerb. Knob curves still guessed: Fruity Chorus's delay (depth and LFO
  speeds left out), Stereo Enhancer's separation, Delay 3's feedback,
  which Transient Processor knobs are attack and release; not
  carried over at all: the Enhancer's phase offset and the Stereo Shaper's
  delay (canopy's Utility has a side delay for them once their units are known).
- PipeWire supported natively on Linux: canopy as its own PipeWire node by
  default rather than through PipeWire's ALSA layer, choosing its outputs and
  inputs, following the graph's rate and quantum, and showing up by name in
  patchbays. (Today the `pipewire` feature only switches cpal's backend, is off
  by default, and needs extra dev packages to build.)
- Instruments more compact and more visually interesting, the Wavetable synth
  most of all: it is very wide and not very intuitive. Fewer, better-grouped
  controls on screen at once, displays that show what each section does to the
  sound, and a layout that fits a normal window without scrolling sideways.


for the drum synth, i'd like to have more granular control, to be able to turn each part of a synthesized drum's off or on, and tweak it as much as I like, and to be able to route these drums into separate effects (this would require more general effect routing work)

- Done: every pad is six layers (any source, its own envelopes, filter, drive,
  FM from the layer above), with choke groups and a pad filter.
- Still to do: routing pads to separate effects (multi-output instruments in
  the engine, then a pad's output picker), a per-layer sample source, and
  per-layer velocity to cutoff.
