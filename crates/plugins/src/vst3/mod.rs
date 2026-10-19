//! VST3 hosting on the COM bindings of the `vst3` crate (generated from the
//! VST 3.8 SDK headers, which Steinberg publishes under the MIT license).
//!
//! - [`Module`]: a loaded `.vst3` (bundle or single file), its entry call
//!   and its plugin factory.
//! - `com`: the objects the host hands plugins (host application, component
//!   handler, plug frame with the Linux run loop, streams, messages).
//! - `instance`: one plugin on the main thread: component and controller,
//!   kept in sync; parameters, state, activation, idle work, editor.
//! - `processor`: the same plugin on the audio thread, with the parameter
//!   and event lists `process` takes, built once at activation.
//!
//! A VST3 plugin is two objects: the component (`IComponent` +
//! `IAudioProcessor`), which makes sound, and the edit controller, which
//! owns parameters and the editor. The host keeps them in step: edits in
//! the editor reach the host as `performEdit` and the host sends them on to
//! the processor; changes the processor reports go back to the controller.

mod com;
mod instance;
mod processor;

pub(crate) use instance::Vst3Instance;
pub(crate) use processor::Vst3Processor;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use crate::library::{Library, Registry};
use crate::thread::MainThread;
use crate::{Error, PluginFormat, PluginInfo, info};

static MODULES: Registry<Module> = Registry::new();

/// A loaded VST3 module whose entry has run.
pub(crate) struct Module {
    library: Library,
    factory: Option<ComPtr<IPluginFactory>>,
    /// The context given to `IPluginFactory3::setHostContext`.
    _context: Option<ComWrapper<com::HostApplication>>,
    /// That context's run loop: plugins may run timers on it while no
    /// editor is open. Its instances' idles run it, on the thread that
    /// loaded the module.
    pub run_loop: Arc<com::RunLoop>,
}

// SAFETY: the factory is a process-wide object whose methods the SDK
// declares thread-safe; entry and exit calls run under the library lock.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

/// The binary inside a bundle, or the file itself.
pub(crate) fn binary_path(path: &Path) -> PathBuf {
    if !path.is_dir() {
        return path.to_owned();
    }
    let arch = match std::env::consts::ARCH {
        "x86" if cfg!(windows) => "x86",
        "x86" => "i386",
        other => other,
    };
    let platform = if cfg!(windows) {
        format!("{arch}-win")
    } else {
        format!("{arch}-linux")
    };
    let dir = path.join("Contents").join(platform);
    let stem = path.file_stem().unwrap_or_default();
    let extension = if cfg!(windows) { "vst3" } else { "so" };
    let named = dir.join(stem).with_extension(extension);
    if named.exists() {
        return named;
    }
    // Some bundles name the binary differently from the bundle.
    std::fs::read_dir(&dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == extension))
        })
        .unwrap_or(named)
}

impl Module {
    /// The module at `path` (a bundle folder or a file), loading it if
    /// needed.
    pub fn load(path: &Path) -> Result<Arc<Module>, Error> {
        let binary = binary_path(path);
        MODULES.get_or_load(&binary, |binary| {
            // SAFETY: loading a plugin runs its code; that's the point.
            let library = unsafe { Library::open(binary)? };
            let missing = |name: &str| Error::Load(format!("{}: no {name}", binary.display()));
            #[cfg(target_os = "linux")]
            {
                // SAFETY: ModuleEntry takes the dlopen handle and must run
                // before anything else in the module (under the library lock).
                if let Some(entry) = library.symbol("ModuleEntry") {
                    let entry: unsafe extern "C" fn(*mut std::ffi::c_void) -> bool =
                        unsafe { std::mem::transmute(entry) };
                    if !unsafe { entry(library.raw_handle()) } {
                        return Err(Error::Load(format!(
                            "{}: ModuleEntry failed",
                            binary.display()
                        )));
                    }
                }
            }
            #[cfg(windows)]
            {
                // SAFETY: InitDll is optional and takes nothing.
                if let Some(init) = library.symbol("InitDll") {
                    let init: unsafe extern "system" fn() -> bool =
                        unsafe { std::mem::transmute(init) };
                    if !unsafe { init() } {
                        return Err(Error::Load(format!("{}: InitDll failed", binary.display())));
                    }
                }
            }
            let mut module = Module {
                library,
                factory: None,
                _context: None,
                run_loop: Arc::new(com::RunLoop::new(MainThread::current())),
            };
            let get = module
                .library
                .symbol("GetPluginFactory")
                .ok_or_else(|| missing("GetPluginFactory"))?;
            // SAFETY: GetPluginFactory is `IPluginFactory* PLUGIN_API ()`,
            // returning a reference we now own (or null).
            let factory = unsafe {
                let get: unsafe extern "system" fn() -> *mut IPluginFactory =
                    std::mem::transmute(get);
                ComPtr::from_raw(get())
            };
            let Some(factory) = factory else {
                // Dropping `module` runs the exit call, which needs the
                // lock we hold: run it here instead.
                module.exit();
                std::mem::forget(module);
                return Err(missing("plugin factory"));
            };
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let context = com::HostApplication::new(module.run_loop.clone());
                if let Some(unknown) = context.to_com_ptr::<FUnknown>() {
                    // SAFETY: a host context the module may keep; we keep it too.
                    unsafe { factory3.setHostContext(unknown.as_ptr()) };
                }
                module._context = Some(context);
            }
            module.factory = Some(factory);
            Ok(module)
        })
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory
            .as_ref()
            .expect("loaded modules have a factory")
    }

    /// The module's exit call; the caller holds the library lock.
    fn exit(&self) {
        #[cfg(target_os = "linux")]
        if let Some(exit) = self.library.symbol("ModuleExit") {
            // SAFETY: balances ModuleEntry, after the factory is released.
            unsafe {
                let exit: unsafe extern "C" fn() -> bool = std::mem::transmute(exit);
                exit();
            }
        }
        #[cfg(windows)]
        if let Some(exit) = self.library.symbol("ExitDll") {
            // SAFETY: balances InitDll.
            unsafe {
                let exit: unsafe extern "system" fn() -> bool = std::mem::transmute(exit);
                exit();
            }
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        let _guard = crate::library::lock();
        self.factory = None;
        self._context = None;
        // Handlers the plugin left on the context go before its exit call
        // (if the plugin still holds the context, they stay: better a leak
        // than calling into an exited module).
        if let Some(run_loop) = Arc::get_mut(&mut self.run_loop) {
            run_loop.clear_owned();
        } else {
            std::mem::forget(self.run_loop.clone());
        }
        self.exit();
    }
}

/// A class id as Steinberg prints FUIDs: 32 hex digits of the four 32-bit
/// words the class was declared with. COM byte order on Windows reorders
/// the first two words' bytes; this undoes that, so a plugin has the same
/// id on Windows and bridged to Linux.
pub(crate) fn tuid_to_string(tuid: &TUID) -> String {
    crate::state::vst3_id(&tuid.map(|c| c as u8), cfg!(windows))
}

/// The inverse of [`tuid_to_string`].
pub(crate) fn string_to_tuid(text: &str) -> Option<TUID> {
    let bytes = crate::state::vst3_bytes(text, cfg!(windows))?;
    Some(bytes.map(|b| b as c_char_t))
}

#[allow(non_camel_case_types)]
type c_char_t = std::ffi::c_char;

/// What `IPluginFactory` says about class `index`, in the richest form the
/// factory offers.
struct ClassInfo {
    cid: TUID,
    category: String,
    name: String,
    vendor: String,
    version: String,
    sub_categories: String,
}

fn class_info(
    factory: &ComPtr<IPluginFactory>,
    index: i32,
    factory_vendor: &str,
) -> Option<ClassInfo> {
    // SAFETY: factory queries with structs to fill; strings are fixed-size.
    unsafe {
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            let mut info: PClassInfoW = std::mem::zeroed();
            if factory3.getClassInfoUnicode(index, &mut info) == kResultOk {
                return Some(ClassInfo {
                    cid: info.cid,
                    category: com::narrow(&info.category),
                    name: com::wide(&info.name),
                    vendor: com::wide(&info.vendor),
                    version: com::wide(&info.version),
                    sub_categories: com::narrow(&info.subCategories),
                });
            }
        }
        if let Some(factory2) = factory.cast::<IPluginFactory2>() {
            let mut info: PClassInfo2 = std::mem::zeroed();
            if factory2.getClassInfo2(index, &mut info) == kResultOk {
                return Some(ClassInfo {
                    cid: info.cid,
                    category: com::narrow(&info.category),
                    name: com::narrow(&info.name),
                    vendor: com::narrow(&info.vendor),
                    version: com::narrow(&info.version),
                    sub_categories: com::narrow(&info.subCategories),
                });
            }
        }
        let mut info: PClassInfo = std::mem::zeroed();
        (factory.getClassInfo(index, &mut info) == kResultOk).then(|| ClassInfo {
            cid: info.cid,
            category: com::narrow(&info.category),
            name: com::narrow(&info.name),
            vendor: factory_vendor.to_owned(),
            version: String::new(),
            sub_categories: String::new(),
        })
    }
}

/// The audio-module classes in a module: their ids and class infos.
fn audio_classes(module: &Module) -> Vec<ClassInfo> {
    let factory = module.factory();
    // SAFETY: factory queries with structs to fill.
    let vendor = unsafe {
        let mut info: PFactoryInfo = std::mem::zeroed();
        if factory.getFactoryInfo(&mut info) == kResultOk {
            com::narrow(&info.vendor)
        } else {
            String::new()
        }
    };
    // SAFETY: as above.
    let count = unsafe { factory.countClasses() };
    (0..count)
        .filter_map(|i| class_info(factory, i, &vendor))
        .filter(|c| c.category == "Audio Module Class")
        .map(|mut c| {
            if c.vendor.is_empty() {
                c.vendor = vendor.clone();
            }
            c
        })
        .collect()
}

/// The class `id` names in this module, if it's there.
pub(crate) fn find_class(module: &Module, id: &str) -> Option<TUID> {
    let tuid = string_to_tuid(id)?;
    audio_classes(module)
        .iter()
        .find(|c| c.cid == tuid)
        .map(|c| c.cid)
}

/// What's in the VST3 module at `path`. Each plugin's component is created
/// and initialized to read its buses, so run this out of process unless
/// the module is trusted.
pub(crate) fn read_file(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    let module = Module::load(path)?;
    let mut infos = Vec::new();
    for class in audio_classes(&module) {
        let categories: Vec<String> = class
            .sub_categories
            .split('|')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let has = |c: &str| categories.iter().any(|x| x == c);
        let mut info = PluginInfo {
            format: PluginFormat::Vst3,
            id: tuid_to_string(&class.cid),
            name: class.name.clone(),
            vendor: class.vendor.clone(),
            version: class.version.clone(),
            kind: crate::PluginKind::Effect,
            categories: categories.clone(),
            path: path.to_owned(),
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: Vec::new(),
            note_outputs: Vec::new(),
            bridged: crate::scan::is_bridged(PluginFormat::Vst3, path),
        };
        if let Ok(buses) = instance::read_buses(&module, &class.cid) {
            (
                info.audio_inputs,
                info.audio_outputs,
                info.note_inputs,
                info.note_outputs,
            ) = buses;
        }
        info.kind = info::guess_kind(has("Instrument"), has("Fx"), false, &info);
        infos.push(info);
    }
    Ok(infos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_print_as_their_declared_words() {
        let tuid = vst3::uid(0x6E332252, 0x54224A00, 0xAA69301A, 0xF318797D);
        let text = tuid_to_string(&tuid);
        assert_eq!(text, "6E33225254224A00AA69301AF318797D");
        assert_eq!(string_to_tuid(&text), Some(tuid));
        assert_eq!(string_to_tuid("nope"), None);
    }
}
