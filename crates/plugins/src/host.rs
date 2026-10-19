//! [`Host`]: where plugins are loaded, and the main-thread work they need
//! done.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clap::ClapInstance;
use crate::instance::{Backend, Graveyard, Inner};
use crate::thread::MainThread;
use crate::vst2::Vst2Instance;
use crate::vst3::Vst3Instance;
use crate::wake::Wake;
use crate::{Error, PluginFormat, PluginInfo, PluginInstance};

/// Loads plugins and runs their main-thread work.
///
/// Make it on the main (UI) thread and keep it there (it isn't `Send`):
/// that thread is the plugins' main thread from then on, which CLAP's
/// thread checks and the formats' rules refer to. Several hosts may live
/// on different threads (tests do that); each instance belongs to the host
/// that loaded it.
///
/// Call [`Self::idle`] often: every UI frame, and whenever the waker set
/// with [`Self::set_waker`] fires.
pub struct Host {
    inner: Rc<HostInner>,
}

struct HostInner {
    main: MainThread,
    wake: Arc<Wake>,
    graveyard: Arc<Graveyard>,
    instances: RefCell<Vec<Weak<Inner>>>,
    next_serial: Cell<u64>,
    watcher: RefCell<Option<JoinHandle<()>>>,
}

impl Default for Host {
    fn default() -> Self {
        Host::new()
    }
}

impl Host {
    pub fn new() -> Host {
        Host {
            inner: Rc::new(HostInner {
                main: MainThread::current(),
                wake: Arc::new(Wake::default()),
                graveyard: Arc::new(Graveyard::default()),
                instances: RefCell::new(Vec::new()),
                next_serial: Cell::new(0),
                watcher: RefCell::new(None),
            }),
        }
    }

    /// Loads the plugin `info` describes (from a scan, or from
    /// [`crate::scan::read_plugin_file`]) and initializes it.
    ///
    /// This runs the plugin's code in this process: a plugin that crashes
    /// while loading takes the app with it. Scanning out of process first
    /// weeds out the ones that crash on sight.
    pub fn load(&self, info: &PluginInfo) -> Result<PluginInstance, Error> {
        let host = &self.inner;
        let backend = match info.format {
            PluginFormat::Clap => {
                let module = crate::clap::Module::load(&info.path)?;
                Backend::Clap(Box::new(ClapInstance::new(
                    module,
                    info,
                    host.main,
                    host.wake.clone(),
                )?))
            }
            PluginFormat::Vst3 => {
                let module = crate::vst3::Module::load(&info.path)?;
                Backend::Vst3(Box::new(Vst3Instance::new(
                    module,
                    info,
                    host.main,
                    host.wake.clone(),
                )?))
            }
            PluginFormat::Vst2 => {
                let module = crate::vst2::Module::load(&info.path)?;
                Backend::Vst2(Box::new(Vst2Instance::new(
                    module,
                    info,
                    host.main,
                    host.wake.clone(),
                )?))
            }
        };
        let serial = host.next_serial.get();
        host.next_serial.set(serial + 1);
        let inner = Rc::new(Inner::new(info.clone(), backend, serial));
        host.instances.borrow_mut().push(Rc::downgrade(&inner));
        Ok(PluginInstance {
            inner,
            graveyard: host.graveyard.clone(),
            main: host.main,
        })
    }

    /// Does what the loaded plugins need done on the main thread: their
    /// callbacks, timers and file-descriptor handlers (which is how their
    /// editors run on Linux), VST2 editors' idle calls, editor window
    /// events, and collecting what
    /// they did for [`PluginInstance::take_events`]. Also drops processors
    /// that were dropped on another thread.
    ///
    /// Returns how soon to call it again at the latest (the next plugin
    /// timer), if any is pending. Without a waker, also call it regularly
    /// (every frame) while plugins are loaded: requests from the audio
    /// thread and readable descriptors are only seen here.
    pub fn idle(&self) -> Option<Duration> {
        let host = &self.inner;
        host.graveyard.clear();
        let instances: Vec<Rc<Inner>> = {
            let mut list = host.instances.borrow_mut();
            list.retain(|i| i.strong_count() > 0);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        let mut next: Option<Instant> = None;
        let mut fds = Vec::new();
        for instance in &instances {
            if let Some(due) = instance.idle() {
                next = Some(next.map_or(due, |n| n.min(due)));
            }
            instance.watched_fds(&mut fds);
        }
        host.wake.idled(fds, next);
        next.map(|due| due.saturating_duration_since(Instant::now()))
    }

    /// Calls `wake` from another thread whenever plugins need
    /// [`Self::idle`] to run soon: they asked for a main-thread callback or
    /// a restart, changed parameters from the audio thread, a timer is due,
    /// or a descriptor they watch (their editor's X11 connection) is
    /// readable. In an egui app: `move || ctx.request_repaint()`.
    ///
    /// Starts a watcher thread (the first time) that waits on the
    /// descriptors and timers [`Self::idle`] last saw.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        if let Some(watcher) = self.inner.wake.set_waker(Arc::new(wake)) {
            *self.inner.watcher.borrow_mut() = Some(watcher);
        }
    }
}

impl Drop for HostInner {
    fn drop(&mut self) {
        self.wake.stop();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        self.graveyard.clear();
    }
}
