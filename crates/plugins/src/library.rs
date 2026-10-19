//! Loading plugin libraries, once per process.
//!
//! CLAP and VST3 have an entry and an exit call per library (CLAP's
//! `init`/`deinit`, VST3's `ModuleEntry`/`ModuleExit` or `InitDll`/`ExitDll`)
//! that must not run at the same time as each other; VST2's entry makes one
//! plugin per call, and runs under the same lock. Each format keeps a
//! [`Registry`] of loaded modules, shared by every host and instance in the
//! process, and loads and unloads under one lock.
//!
//! When its last instance is gone a module's exit call runs, but the library
//! itself stays mapped: plugins often leave threads or thread-local
//! destructors behind that crash once their code is unmapped, which is why
//! most hosts keep plugin libraries loaded. Loading it again later finds
//! the same mapping and calls the entry again.
//!
//! VST2 is the exception ([`Library::close`]). It has no exit call, so a
//! plugin can only tear its globals down as its last instance closes and
//! set them up again from scratch at the next entry call, and frameworks
//! count on being unloaded in between instead: JUCE's VST2 plugins on Linux
//! hang closing an instance made after all earlier ones were closed, if
//! their library stayed loaded. VST2 hosts unload such libraries, and so
//! does this one.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::Error;

/// Serializes every entry and exit call, and the registries.
static LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// This thread holds [`LOCK`].
    static HOLDING: Cell<bool> = const { Cell::new(false) };
}

/// [`LOCK`], held until dropped.
pub(crate) struct LockGuard {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Before the field unlocks.
        HOLDING.set(false);
    }
}

pub(crate) fn lock() -> LockGuard {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    HOLDING.set(true);
    LockGuard { _guard: guard }
}

/// Whether this thread holds the lock already (taking it again would
/// deadlock).
pub(crate) fn holding() -> bool {
    HOLDING.get()
}

/// A dynamic library, never unloaded.
pub(crate) struct Library {
    #[cfg(unix)]
    inner: ManuallyDrop<libloading::os::unix::Library>,
    #[cfg(windows)]
    inner: ManuallyDrop<libloading::os::windows::Library>,
    /// What `dlopen` returned, which VST3's `ModuleEntry` wants.
    #[cfg(unix)]
    handle: *mut c_void,
}

// SAFETY: a loaded library is process-wide; the handle is only an
// identifier, and looking symbols up is thread-safe on both OSes.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Loads the library at `path`.
    ///
    /// # Safety
    ///
    /// Loading runs the library's initializers, which can do anything.
    pub unsafe fn open(path: &Path) -> Result<Library, Error> {
        let error = |e: libloading::Error| Error::Load(format!("{}: {e}", path.display()));
        #[cfg(unix)]
        {
            use libloading::os::unix::{Library as Unix, RTLD_LOCAL, RTLD_NOW};
            // RTLD_NOW: report missing symbols now rather than crash later.
            // SAFETY: the caller accepts running the library's initializers.
            let library =
                unsafe { Unix::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map_err(error)?;
            let handle = library.into_raw();
            // SAFETY: `handle` came from `into_raw` just above.
            let inner = unsafe { Unix::from_raw(handle) };
            Ok(Library {
                inner: ManuallyDrop::new(inner),
                handle,
            })
        }
        #[cfg(windows)]
        {
            // SAFETY: as above.
            let inner = unsafe { libloading::os::windows::Library::new(path) }.map_err(error)?;
            Ok(Library {
                inner: ManuallyDrop::new(inner),
            })
        }
    }

    /// The address of the exported symbol `name`, if it has one.
    pub fn symbol(&self, name: &str) -> Option<*mut c_void> {
        // SAFETY: we only read the symbol's address, as a data pointer; the
        // callers cast it to the type the format's headers give it.
        unsafe {
            self.inner
                .get::<*mut c_void>(name.as_bytes())
                .ok()
                .map(|s| *s)
                .filter(|p| !p.is_null())
        }
    }

    /// The handle `dlopen` returned.
    #[cfg(unix)]
    pub fn raw_handle(&self) -> *mut c_void {
        self.handle
    }

    /// Unloads the library (`dlclose`, `FreeLibrary`), for formats whose
    /// plugins expect it (see the module docs).
    ///
    /// # Safety
    ///
    /// Nothing may use the library's code or data afterwards: every plugin
    /// made from it must be gone, and the caller holds the entry/exit lock.
    pub unsafe fn close(self) {
        let inner = ManuallyDrop::into_inner(self.inner);
        // SAFETY: by the caller. A library with thread-local destructors
        // still pending stays mapped (the loader sees to that).
        let _ = inner.close();
    }
}

/// The modules of one format that are loaded, by canonical path.
pub(crate) struct Registry<T> {
    modules: Mutex<Vec<(PathBuf, Weak<T>)>>,
}

impl<T> Registry<T> {
    pub const fn new() -> Registry<T> {
        Registry {
            modules: Mutex::new(Vec::new()),
        }
    }

    /// The module loaded from `path`, loading it with `load` (under the
    /// entry/exit lock) if nothing holds it yet.
    pub fn get_or_load(
        &self,
        path: &Path,
        load: impl FnOnce(&Path) -> Result<T, Error>,
    ) -> Result<Arc<T>, Error> {
        let key = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        let _guard = lock();
        let mut modules = self.modules.lock().unwrap_or_else(|e| e.into_inner());
        modules.retain(|(_, module)| module.strong_count() > 0);
        if let Some(module) = modules
            .iter()
            .find(|(p, _)| *p == key)
            .and_then(|(_, m)| m.upgrade())
        {
            return Ok(module);
        }
        let module = Arc::new(load(&key)?);
        modules.push((key, Arc::downgrade(&module)));
        Ok(module)
    }
}
