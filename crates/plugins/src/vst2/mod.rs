//! VST2 hosting on a small clean-room description of the VST 2.4 ABI
//! (`abi`), since Steinberg's VST2 SDK was never freely licensed.
//!
//! - [`Module`]: a loaded VST2 library and its entry point. A VST2 file
//!   holds one plugin, made by calling the entry once per instance.
//! - `host`: the callback every plugin gets (`audioMaster`) and the state
//!   behind it.
//! - `instance`: one plugin on the main thread: parameters, state,
//!   activation, idle work, editor.
//! - `processor`: the same plugin on the audio thread.
//!
//! VST2 writes down no thread rules; this host keeps to what hosts do:
//! `processReplacing`, `effProcessEvents` and the block's `setParameter`
//! calls on the audio thread, every other dispatcher call on the main
//! thread, `getParameter` from the main thread. Resuming and starting
//! (`effMainsChanged`, `effStartProcess`) go with activation, on the main
//! thread, as JUCE's and Carla's hosts do.
//!
//! Shell plugins (one file holding many, as Waves ships them) aren't
//! supported: reading one fails with an error saying so.

// The ABI's conventional names (`effOpen`, `kPlugCategSynth`), matched on
// throughout.
#![allow(non_upper_case_globals)]

pub(crate) mod abi;
mod host;
mod instance;
mod processor;

pub(crate) use instance::Vst2Instance;
pub(crate) use processor::Vst2Processor;

use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::library::{Library, Registry};
use crate::{AudioPortInfo, Error, PluginInfo};

static MODULES: Registry<Module> = Registry::new();

/// A loaded VST2 library, unloaded again when its last instance is gone
/// (see `crate::library` for why).
pub(crate) struct Module {
    library: Option<Library>,
    /// `VSTPluginMain`, or `main` in older plugins; none if the library
    /// isn't a VST2 plugin.
    entry: Option<abi::MainFn>,
    path: PathBuf,
}

impl Module {
    /// The library at `path`, loading it if needed.
    pub fn load(path: &Path) -> Result<Arc<Module>, Error> {
        check_architecture(path)?;
        MODULES.get_or_load(path, |path| {
            // SAFETY: loading a plugin runs its code; that's the point.
            let library = unsafe { Library::open(path)? };
            let entry = library
                .symbol("VSTPluginMain")
                .or_else(|| library.symbol("main"))
                // SAFETY: both entries are `AEffect* (audioMasterCallback)`
                // by the format's ABI.
                .map(|entry| unsafe { std::mem::transmute::<*mut c_void, abi::MainFn>(entry) });
            Ok(Module {
                library: Some(library),
                entry,
                path: path.to_owned(),
            })
        })
    }

    /// The folder the plugin file is in.
    pub fn directory(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new(""))
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // Plugins let go of their module under the lock already, so that a
        // load of the same file can't come between the last one going and
        // the library being unloaded (and find it still loaded, its globals
        // torn down).
        let _guard = (!crate::library::holding()).then(crate::library::lock);
        if let Some(library) = self.library.take() {
            // SAFETY: every plugin made from the library holds this module,
            // so they're all closed; the lock is held.
            unsafe { library.close() };
        }
    }
}

/// Refuses 32-bit plugins up front, with a clearer error than the loader's.
fn check_architecture(path: &Path) -> Result<(), Error> {
    let mut head = Vec::with_capacity(4096);
    if std::fs::File::open(path)
        .and_then(|f| f.take(4096).read_to_end(&mut head))
        .is_err()
    {
        // Loading reports it.
        return Ok(());
    }
    let thirty_two = if head.starts_with(b"\x7fELF") {
        head.get(4) == Some(&1)
    } else if head.starts_with(b"MZ") {
        // The PE header's offset is at 0x3C; the machine follows "PE\0\0".
        let at = head
            .get(0x3C..0x40)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        head.get(at..at + 6)
            .is_some_and(|pe| pe[..4] == *b"PE\0\0" && pe[4..6] == 0x014Cu16.to_le_bytes())
    } else {
        false
    };
    if thirty_two {
        return Err(Error::Load(format!(
            "{} is a 32-bit plugin; only 64-bit plugins can be loaded",
            path.display()
        )));
    }
    Ok(())
}

/// A unique id as [`PluginInfo::id`] holds it: eight uppercase hex digits.
pub(crate) fn id_to_string(id: i32) -> String {
    crate::state::vst2_id(id)
}

/// The inverse of [`id_to_string`].
pub(crate) fn string_to_id(text: &str) -> Option<i32> {
    if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok().map(|id| id as i32)
}

/// A string a plugin wrote into `buf`: up to the first NUL, as lossy
/// UTF-8, trimmed.
pub(crate) fn text(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_owned()
}

/// The most channels a side is taken to have, whatever a plugin claims.
const MAX_CHANNELS: i32 = 128;

/// How a plugin's numbered channels are grouped into ports: the first two
/// (one, for a mono plugin) are the main port, the next two the sidechain
/// (or extra outputs), then the rest in pairs.
pub(crate) fn channel_groups(channels: i32) -> Vec<u32> {
    let mut left = channels.clamp(0, MAX_CHANNELS) as u32;
    let mut groups = Vec::new();
    while left > 0 {
        let n = left.min(2);
        groups.push(n);
        left -= n;
    }
    groups
}

/// The ports for `inputs` and `outputs` channels.
pub(crate) fn audio_ports(inputs: i32, outputs: i32) -> (Vec<AudioPortInfo>, Vec<AudioPortInfo>) {
    let ports = |channels: i32, name: fn(usize) -> String| {
        channel_groups(channels)
            .into_iter()
            .enumerate()
            .map(|(i, channels)| AudioPortInfo {
                name: name(i),
                channels,
                main: i == 0,
            })
            .collect()
    };
    (
        ports(inputs, |i| match i {
            0 => "Input".into(),
            1 => "Sidechain".into(),
            _ => format!("Input {}", i + 1),
        }),
        ports(outputs, |i| match i {
            0 => "Output".into(),
            _ => format!("Output {}", i + 1),
        }),
    )
}

/// A vendor version as text. JUCE and most current frameworks pack it as
/// `0xMMmmpp`; anything smaller is shown as the number it is.
pub(crate) fn version_text(version: isize) -> String {
    match version {
        v if (0x1_0000..=0xFF_FFFF).contains(&v) => {
            format!("{}.{}.{}", v >> 16, (v >> 8) & 0xFF, v & 0xFF)
        }
        v if v > 0 => v.to_string(),
        _ => String::new(),
    }
}

/// What's in the VST2 file at `path`: its one plugin, or nothing if the
/// library has no VST2 entry point (a helper library next to the plugins).
/// The plugin is made and opened to read it, so run this out of process
/// unless the file is trusted.
pub(crate) fn read_file(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    let module = Module::load(path)?;
    if module.entry.is_none() {
        return Ok(Vec::new());
    }
    let plugin = instance::Vst2Plugin::new(
        module,
        None,
        crate::thread::MainThread::current(),
        Arc::default(),
    )?;
    plugin.check_supported(path)?;
    Ok(vec![plugin.describe(path)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_ids_print_as_eight_hex_digits() {
        let id = i32::from_be_bytes(*b"Vtal");
        assert_eq!(id_to_string(id), "5674616C");
        assert_eq!(string_to_id("5674616C"), Some(id));
        assert_eq!(id_to_string(-1), "FFFFFFFF");
        assert_eq!(string_to_id("FFFFFFFF"), Some(-1));
        assert_eq!(string_to_id("+674616C"), None);
        assert_eq!(string_to_id("5674616"), None);
    }

    #[test]
    fn channels_group_into_main_sidechain_and_pairs() {
        assert_eq!(channel_groups(0), Vec::<u32>::new());
        assert_eq!(channel_groups(1), [1]);
        assert_eq!(channel_groups(4), [2, 2]);
        assert_eq!(channel_groups(5), [2, 2, 1]);
        assert_eq!(channel_groups(-3), Vec::<u32>::new());
        let (inputs, outputs) = audio_ports(4, 6);
        assert_eq!(
            inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Input", "Sidechain"]
        );
        assert!(inputs[0].main && !inputs[1].main);
        assert_eq!(outputs.len(), 3);
        assert_eq!(outputs[2].name, "Output 3");
    }

    #[test]
    fn versions_read_as_packed_or_plain_numbers() {
        assert_eq!(version_text(0x01_02_03), "1.2.3");
        assert_eq!(version_text(1200), "1200");
        assert_eq!(version_text(0), "");
    }

    #[test]
    fn plugin_text_stops_at_the_nul_and_survives_bad_bytes() {
        assert_eq!(text(b"Gain\0junk"), "Gain");
        assert_eq!(text(b" dB "), "dB");
        assert_eq!(text(b"\xFFx\0"), "\u{FFFD}x");
    }

    #[test]
    fn thirty_two_bit_libraries_are_refused() {
        let dir = std::env::temp_dir().join(format!("canopy-vst2-arch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let elf32 = dir.join("old.so");
        std::fs::write(&elf32, b"\x7fELF\x01\x01\x01\0").unwrap();
        let mut pe32 = vec![0u8; 0x80];
        pe32[..2].copy_from_slice(b"MZ");
        pe32[0x3C] = 0x40;
        pe32[0x40..0x46].copy_from_slice(b"PE\0\0\x4C\x01");
        let dll = dir.join("old.dll");
        std::fs::write(&dll, &pe32).unwrap();
        for path in [&elf32, &dll] {
            let error = check_architecture(path).unwrap_err();
            assert!(error.to_string().contains("32-bit"), "{error}");
        }
        pe32[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        std::fs::write(&dll, &pe32).unwrap();
        assert!(check_architecture(&dll).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
