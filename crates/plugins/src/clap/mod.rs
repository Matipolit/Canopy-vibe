//! CLAP hosting on the raw C ABI (`clap-sys`, CLAP 1.2).
//!
//! - [`Module`]: a loaded `.clap` file, its entry and its plugin factory.
//! - `host`: the `clap_host` we give each plugin, and every host extension
//!   it may ask for (thread-check, log, params, state, latency, tail, gui,
//!   timer-support, posix-fd-support, audio- and note-ports).
//! - `instance`: one plugin on the main thread: parameters, state,
//!   activation, idle work, editor.
//! - `processor`: the same plugin on the audio thread.
//!
//! CLAP sorts its functions into main-thread and audio-thread ones and
//! says which in its headers; each call here keeps to that, and the
//! comments say so where it isn't obvious.

mod host;
mod instance;
mod processor;

pub(crate) use instance::ClapInstance;
pub(crate) use processor::ClapProcessor;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::sync::Arc;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::plugin::clap_plugin_descriptor;
use clap_sys::version::clap_version_is_compatible;

use crate::library::{Library, Registry};
use crate::{Error, PluginFormat, PluginInfo, info};

static MODULES: Registry<Module> = Registry::new();

/// A loaded `.clap` file whose entry has been initialized.
pub(crate) struct Module {
    library: Library,
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
}

// SAFETY: the entry and factory are process-wide tables the plugin keeps
// for as long as it's loaded; CLAP makes `get_factory` and the factory's
// functions thread-safe, and init/deinit run under the library lock.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

impl Module {
    /// The module at `path`, loading and initializing it if needed.
    pub fn load(path: &Path) -> Result<Arc<Module>, Error> {
        MODULES.get_or_load(path, |path| {
            // SAFETY: loading a plugin runs its code; that's the point.
            let library = unsafe { Library::open(path)? };
            let entry = library
                .symbol("clap_entry")
                .ok_or_else(|| Error::Load(format!("{}: no clap_entry", path.display())))?
                as *const clap_plugin_entry;
            // SAFETY: `clap_entry` is a `clap_plugin_entry` by CLAP's ABI.
            let entry_ref = unsafe { &*entry };
            if !clap_version_is_compatible(entry_ref.clap_version) {
                return Err(Error::Load(format!(
                    "{}: unsupported CLAP version {}.{}",
                    path.display(),
                    entry_ref.clap_version.major,
                    entry_ref.clap_version.minor
                )));
            }
            let c_path = CString::new(path.to_string_lossy().as_bytes())
                .map_err(|_| Error::Load(format!("{}: bad path", path.display())))?;
            // SAFETY: init gets the plugin's own path, under the library lock
            // the registry holds, as CLAP requires.
            let initialized = entry_ref
                .init
                .is_some_and(|init| unsafe { init(c_path.as_ptr()) });
            if !initialized {
                return Err(Error::Load(format!("{}: init failed", path.display())));
            }
            let factory = entry_ref
                .get_factory
                // SAFETY: a thread-safe call with a valid factory id.
                .map(|get| unsafe { get(CLAP_PLUGIN_FACTORY_ID.as_ptr()) })
                .unwrap_or(std::ptr::null())
                as *const clap_plugin_factory;
            if factory.is_null() {
                // SAFETY: balances the init above, still under the lock.
                if let Some(deinit) = entry_ref.deinit {
                    unsafe { deinit() };
                }
                return Err(Error::Load(format!(
                    "{}: no plugin factory",
                    path.display()
                )));
            }
            Ok(Module {
                library,
                entry,
                factory,
            })
        })
    }

    fn factory(&self) -> &clap_plugin_factory {
        // SAFETY: checked non-null in `load`; valid while the module is.
        unsafe { &*self.factory }
    }

    /// The descriptors of the plugins in this file.
    pub fn descriptors(&self) -> Vec<&clap_plugin_descriptor> {
        let factory = self.factory();
        let count = factory
            .get_plugin_count
            // SAFETY: factory functions are thread-safe.
            .map_or(0, |count| unsafe { count(self.factory) });
        (0..count)
            .filter_map(|i| {
                let get = factory.get_plugin_descriptor?;
                // SAFETY: index in range; null or a descriptor that lives as
                // long as the module.
                unsafe { get(self.factory, i).as_ref() }
            })
            .filter(|d| !d.id.is_null())
            .collect()
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        let _guard = crate::library::lock();
        // SAFETY: init succeeded when the module was made; deinit runs once,
        // under the library lock. The library itself stays loaded.
        unsafe {
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
        let _ = &self.library;
    }
}

/// A C string from a plugin, or "" for null.
pub(crate) fn text(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: plugins hand out NUL-terminated strings.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// A fixed-size C string buffer from a plugin.
pub(crate) fn text_buf(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What's in the `.clap` file at `path`. Each plugin is created and
/// initialized to read its ports, so run this out of process unless the
/// file is trusted.
pub(crate) fn read_file(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    let module = Module::load(path)?;
    let mut infos = Vec::new();
    for descriptor in module.descriptors() {
        let features = features(descriptor);
        let has = |f: &str| features.iter().any(|x| x == f);
        let mut info = PluginInfo {
            format: PluginFormat::Clap,
            id: text(descriptor.id),
            name: text(descriptor.name),
            vendor: text(descriptor.vendor),
            version: text(descriptor.version),
            kind: crate::PluginKind::Effect,
            categories: features.clone(),
            path: path.to_owned(),
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: Vec::new(),
            note_outputs: Vec::new(),
            bridged: crate::scan::is_bridged(PluginFormat::Clap, path),
        };
        // Ports need a live instance; a plugin that won't make one is still
        // listed, with no ports.
        if let Ok(instance) = ClapInstance::new(
            module.clone(),
            &info,
            crate::thread::MainThread::current(),
            Default::default(),
        ) {
            let (inputs, outputs) = instance.audio_ports();
            info.audio_inputs = inputs;
            info.audio_outputs = outputs;
            let (inputs, outputs) = instance.note_ports();
            info.note_inputs = inputs;
            info.note_outputs = outputs;
        }
        info.kind = info::guess_kind(
            has("instrument"),
            has("audio-effect") || has("analyzer"),
            has("note-effect") || has("note-detector"),
            &info,
        );
        infos.push(info);
    }
    Ok(infos)
}

fn features(descriptor: &clap_plugin_descriptor) -> Vec<String> {
    let mut features = Vec::new();
    let mut ptr = descriptor.features;
    if ptr.is_null() {
        return features;
    }
    // SAFETY: a null-terminated array of C strings, per CLAP.
    unsafe {
        while !(*ptr).is_null() && features.len() < 256 {
            features.push(text(*ptr));
            ptr = ptr.add(1);
        }
    }
    features
}
