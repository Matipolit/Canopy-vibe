//! canopy's built-in instruments as CLAP plugins, for other hosts: one
//! file (`canopy.clap`, built by `tools/package-clap.sh`) whose factory
//! offers every built-in that plays without loading files of its own
//! ([`packaged`]). Each keeps its built-in id ("canopy.guitar") as its
//! CLAP id and is named "Canopy" and its name.
//!
//! - [`values`]: parameters as the host sees them (floats normalized,
//!   enums stepped), shared by the main and audio threads.
//! - [`bridge`]: the instrument on the audio thread, run as canopy's
//!   engine runs it (blocks of `dsp::MAX_BLOCK`, parameters at block
//!   starts, the sustain pedal held back), so it sounds the same as in a
//!   canopy song.
//! - [`state`]: what a host's project keeps (JSON of the parameters by id).
//! - `plugin`: the C ABI over those (params, state, audio and note
//!   ports). There's no editor: hosts show their own controls.

pub mod bridge;
mod plugin;
pub mod state;
pub mod values;

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::null;
use std::sync::OnceLock;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::version::CLAP_VERSION;
use instruments::{BuiltInInstrumentDescriptor, DataKind};

/// The built-in instruments the file offers, in the instrument picker's
/// order: all but those that need files loaded to make a sound (the
/// Sampler's `.sfz`, the Slicer's loop). The Wavetable comes with its
/// factory tables.
pub fn packaged() -> impl Iterator<Item = &'static BuiltInInstrumentDescriptor> {
    instruments::builtins()
        .iter()
        .filter(|d| !matches!(d.data, DataKind::Sfz | DataKind::Slices))
}

/// The name a host lists `instrument` by.
pub fn plugin_name(instrument: &BuiltInInstrumentDescriptor) -> String {
    format!("Canopy {}", instrument.name)
}

/// CLAP's words for what kind of instrument it is.
fn features(instrument: &BuiltInInstrumentDescriptor) -> &'static [&'static CStr] {
    match instrument.id {
        "canopy.drums" => &[c"instrument", c"drum-machine", c"stereo"],
        "canopy.guitar" => &[c"instrument", c"stereo"],
        _ => &[c"instrument", c"synthesizer", c"stereo"],
    }
}

struct Entry {
    instrument: &'static BuiltInInstrumentDescriptor,
    descriptor: clap_plugin_descriptor,
}

// SAFETY: the descriptor only points at strings leaked for the life of
// the library, never written again.
unsafe impl Send for Entry {}
unsafe impl Sync for Entry {}

/// The packaged instruments with their CLAP descriptors, made the first
/// time a host asks.
fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let leak = |s: &str| -> *const c_char {
            let s = CString::new(s).unwrap_or_default();
            Box::leak(s.into_boxed_c_str()).as_ptr()
        };
        packaged()
            .map(|instrument| {
                let features: Vec<*const c_char> = features(instrument)
                    .iter()
                    .map(|f| f.as_ptr())
                    .chain([null()])
                    .collect();
                Entry {
                    instrument,
                    descriptor: clap_plugin_descriptor {
                        clap_version: CLAP_VERSION,
                        id: leak(instrument.id),
                        name: leak(&plugin_name(instrument)),
                        vendor: leak(instrument.author),
                        url: c"".as_ptr(),
                        manual_url: c"".as_ptr(),
                        support_url: c"".as_ptr(),
                        version: leak(instrument.version),
                        description: leak(instrument.description),
                        features: Box::leak(features.into_boxed_slice()).as_ptr(),
                    },
                }
            })
            .collect()
    })
}

unsafe extern "C" fn factory_count(_factory: *const clap_plugin_factory) -> u32 {
    entries().len() as u32
}

unsafe extern "C" fn factory_descriptor(
    _factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    entries()
        .get(index as usize)
        .map_or(null(), |e| &e.descriptor)
}

unsafe extern "C" fn factory_create(
    _factory: *const clap_plugin_factory,
    _host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if id.is_null() {
        return null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    match entries()
        .iter()
        .find(|e| id.to_bytes() == e.instrument.id.as_bytes())
    {
        Some(entry) => plugin::create(entry.instrument, &entry.descriptor),
        None => null(),
    }
}

struct Factory(clap_plugin_factory);
// SAFETY: only function pointers.
unsafe impl Sync for Factory {}

static FACTORY: Factory = Factory(clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
});

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if !id.is_null() && unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY.0 as *const _ as *const c_void
    } else {
        null()
    }
}

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
