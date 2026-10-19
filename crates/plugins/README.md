# plugins

Hosts third-party CLAP, VST3 and VST2 plugins (effects and instruments)
for canopy, on Linux and Windows: finding them, loading them, running their
audio, parameters and state, and showing their editors. The crate knows
nothing about canopy's projects or engine; a glue crate adapts it. The API
and its threading rules are documented in `src/lib.rs`
(`cargo doc -p plugins --open`).

## Trying it with your plugins

`examples/plugin_cli.rs` scans, inspects, renders and opens editors:

```sh
# Every plugin in the standard folders (yabridge's included), read out of
# process and cached in ~/.cache/canopy/plugins.json:
cargo run -p plugins --example plugin_cli -- scan

# Only some folders:
cargo run -p plugins --example plugin_cli -- scan --path ~/.vst3/yabridge

# A plugin's parameters, by (part of) its name, its id or its file:
cargo run -p plugins --example plugin_cli -- params Vital

# Render 3 s through it to a 32-bit float WAV: an instrument plays a C
# major chord (--note sets the root), an effect gets plucked saw notes.
cargo run -p plugins --example plugin_cli -- render Vital vital.wav
cargo run -p plugins --example plugin_cli -- render OTT ott.wav --seconds 5 --set 0=0.5

# Open its editor; close the window to quit. Prints what the plugin does
# (parameter gestures and values as you turn its knobs).
cargo run -p plugins --example plugin_cli -- edit Vital
```

`render` also takes `--rate`, `--block` and `--set ID=VALUE` (repeatable;
ids as `params` lists them; CLAP values are plain, VST3 and VST2 values 0
to 1). Every command takes `--path DIR`; a plugin can also be named by its
file (`params /usr/lib/vst/Vital.so`).

## Where plugins are looked for

| | Linux | Windows |
|---|---|---|
| CLAP | `$CLAP_PATH`, `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` | `$CLAP_PATH`, `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` |
| VST3 | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` (bundles with `Contents/x86_64-linux/*.so`) | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` (bundles or single `.vst3` files) |
| VST2 | `$VST_PATH`, `$LXVST_PATH`, `~/.vst`, `/usr/lib/vst`, `/usr/local/lib/vst`, `/usr/lib/lxvst`, `/usr/local/lib/lxvst`, `~/.lxvst` (`.so` files) | `$VST_PATH`, `%PROGRAMFILES%\VSTPlugins`, `%PROGRAMFILES%\Steinberg\VSTPlugins`, `%COMMONPROGRAMFILES%\VST2`, `%COMMONPROGRAMFILES%\Steinberg\VST2` (`.dll` files) |

Folders are searched recursively. VST2 has no one standard folder on
Windows: if your plugins are elsewhere, add the folder to `VST_PATH`
(`;`-separated; `:` on Linux). Libraries in a VST2 folder that aren't
plugins (helper libraries some vendors install next to them) are skipped
quietly. Plugins with native Linux builds install
into these (Vital ships Linux VST3 and CLAP builds, for one); check each
vendor. Windows-only plugins need yabridge.

## Windows plugins on Linux: yabridge

[yabridge](https://github.com/robbert-vdh/yabridge) runs Windows VST2, VST3
and (since 5.0) CLAP plugins under Wine and presents each as a native Linux
plugin, editor included. canopy needs nothing special for it: yabridge's
output lands inside the folders above.

1. Install Wine Staging (yabridge recommends it; distribution packages can
   be too old, so on Debian and Ubuntu use WineHQ's repository). Check the
   yabridge README for the Wine versions known to work: the latest
   release, 5.1.1, has trouble with some recent Wine versions.
2. Install your Windows plugins with their installers under Wine, e.g.
   `wine Serum_Installer.exe`. They end up in the Wine prefix, normally
   `~/.wine/drive_c/Program Files/Common Files/VST3` and `.../CLAP`; VST2
   plugins wherever their installer puts them, often
   `~/.wine/drive_c/Program Files/VSTPlugins` or
   `.../Program Files/Steinberg/VstPlugins`.
3. Install yabridge: from your distribution (Arch, Fedora COPR, ...) or
   the release archive:
   ```sh
   tar -C ~/.local/share -xavf yabridge-5.1.1.tar.gz
   export PATH="$PATH:$HOME/.local/share/yabridge"   # add to ~/.bashrc
   ```
4. Tell yabridgectl where the Windows plugins are, and set them up:
   ```sh
   yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/VST3"
   yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/CLAP"
   yabridgectl add "$HOME/.wine/drive_c/Program Files/VSTPlugins"   # VST2
   yabridgectl sync      # again after installing or updating plugins, yabridge or Wine
   yabridgectl status    # what's set up
   ```
5. Scan in canopy (or `plugin_cli scan`). The bridged plugins are:
   - VST3: `~/.vst3/yabridge/<Plugin>.vst3`, a bundle whose
     `Contents/x86_64-linux/<Plugin>.so` is yabridge's loader, with the
     Windows module linked in as `Contents/x86_64-win/<Plugin>.vst3`;
   - CLAP: `~/.clap/yabridge/<Plugin>.clap` (yabridge's loader) next to
     `<Plugin>.clap-win`, a link to the Windows plugin;
   - VST2: `~/.vst/yabridge/<folders>/<Plugin>.so` (yabridge's loader)
     next to `<Plugin>.dll`, a link to the Windows plugin, the folders
     mirroring the Windows ones. With yabridge's older "inline" setup
     (`yabridgectl set --vst2-location=inline`) the `.so` sits next to the
     Windows `.dll` in the Wine prefix instead; add that folder to
     `VST_PATH` then.

   Scans mark them as bridged ("Windows, via yabridge" in `plugin_cli`),
   and rescan them when the Windows file changes.

Notes:

- The first scan of a bridged plugin starts Wine, which can take a while;
  the scanner allows 60 s per file.
- yabridge forces Wine onto X11, and canopy gives every editor its own X11
  window, so editors work in a Wayland session too (through XWayland).
- A Windows `.clap` or `.vst3` copied into a Linux plugin folder without
  yabridge, or a Windows VST2 `.dll` there without yabridge's `.so` next to
  it, is reported as "a Windows plugin; bridge it with yabridge".
- yabridge isn't installed on the machine this crate was written on, so the
  bridged path is untested here; the folders and layout above are what
  `yabridgectl sync` produces (see its `tools/yabridgectl` sources).

## VST2

VST2 is hosted through a small description of its binary interface written
for this crate (`src/vst2/abi.rs`), a clean-room one: Steinberg's VST2 SDK
was never freely licensed and is no longer distributed, and the GPL
reimplementations of it aren't used either. Only 64-bit plugins load; a
32-bit one is reported as such. What's different from the other formats:

- A plugin's id is its VST2 unique id as 8 hex digits ("56697461" is
  `'Vita'`). These ids are only unique per vendor, so the file counts too.
- Parameters are 0 to 1, kept by the plugin as 32-bit floats. All are
  automatable; none can be modulated without moving (modulation moves the
  value, as for VST3). VST2 has no read-only parameters.
- The plugin can only show its parameter's current value as text, so
  `param_text` is `None` for any other value, and it can't read a value
  from text without setting the parameter (`param_from_text` is `None`).
- State is the plugin's bank chunk if it keeps one, or else its program
  and every parameter's value, in a small container of canopy's.
- `reset` (after a seek) ends held notes and sends All Sound Off on every
  channel: voices stop, but an effect's tail rings on, since VST2's only
  reset (suspend and resume) isn't safe on the audio thread.
- Shell plugins (many plugins in one file, as Waves ships them) aren't
  supported; a scan reports them.
- Editors size themselves; the user can't resize their windows. On Linux
  they run from `effEditIdle`, about 30 times a second. Plugins that need
  the host's Xlib `Display` (a few very old ones) won't show.
- A VST2 library is unloaded when its last instance closes, unlike the
  other formats' (see `src/library.rs`): VST2 has no exit call, and JUCE's
  VST2 plugins hang on Linux if a new instance is closed in a library that
  stayed loaded after all earlier instances were.

## Tests

```sh
cargo test -p plugins                               # builds the test plugins first
cargo test -p plugins --test editor -- --ignored    # opens editor windows (needs an X display)
```

`test-plugins/clap-gain`, `test-plugins/vst3-gain` and
`test-plugins/vst2-gain` are tiny plugins (a gain effect and a sine
instrument each, with small X11 editors) that the tests build with
`plugins::fixtures::build()`; other crates' tests can use it too. The VST2
one is a single library installed under two names, since a VST2 file holds
one plugin; it becomes the plugin its file name says.
