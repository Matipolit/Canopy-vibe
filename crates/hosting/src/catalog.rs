//! The plugins installed on this computer, as the last scan found them.
//!
//! Scanning reads new and changed plugin files in scanner processes
//! (`plugins::scan`), which can take a while the first time (a yabridge
//! plugin starts Wine), so it runs on a thread of its own and the catalog
//! fills in when it's done. Files that haven't changed come from the scan
//! cache at once.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use plugins::scan::{ScanFailure, ScanOptions, ScanReport};
use plugins::{PluginFormat, PluginInfo, PluginKind};

/// What scanning found, and whether it's still looking.
#[derive(Default)]
pub struct Catalog {
    plugins: Vec<PluginInfo>,
    failures: Vec<ScanFailure>,
    scanning: Option<Receiver<ScanReport>>,
    /// Bumped whenever the list changes, so plugins that weren't found
    /// are looked for again.
    version: u64,
    scanned: bool,
}

impl Catalog {
    /// Scans in the background (after any scan still running), calling
    /// `wake` when it's done; [`Self::poll`] picks the result up.
    pub fn scan(&mut self, options: ScanOptions, wake: Arc<dyn Fn() + Send + Sync>) {
        let (sender, receiver) = mpsc::channel();
        let previous = self.scanning.replace(receiver);
        let spawned = std::thread::Builder::new()
            .name("plugin scan".into())
            .spawn(move || {
                // One scan at a time: they share the cache file.
                if let Some(previous) = previous {
                    let _ = previous.recv();
                }
                let _ = sender.send(plugins::scan::scan(&options));
                wake();
            });
        if spawned.is_err() {
            self.scanning = None;
        }
    }

    /// Takes a finished scan's result; true if the list changed.
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.scanning else {
            return false;
        };
        match receiver.try_recv() {
            Ok(report) => {
                self.scanning = None;
                self.set(report.plugins);
                self.failures = report.failures;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                // The scan died: what it would have found stays unknown,
                // but nothing is "still being looked for" any more.
                self.scanning = None;
                self.scanned = true;
                self.version += 1;
                true
            }
        }
    }

    /// Replaces the list (with a scan's, or a test's own).
    pub fn set(&mut self, mut plugins: Vec<PluginInfo>) {
        // A plugin installed twice (a native build and a bridged one, or
        // in two folders) is listed once: native first, then by path.
        plugins.sort_by(|a, b| {
            (a.format, &a.id, a.bridged, &a.path).cmp(&(b.format, &b.id, b.bridged, &b.path))
        });
        plugins.dedup_by(|b, a| a.format == b.format && a.id == b.id);
        plugins.sort_by(|a, b| {
            (a.vendor.to_lowercase(), a.name.to_lowercase(), a.format).cmp(&(
                b.vendor.to_lowercase(),
                b.name.to_lowercase(),
                b.format,
            ))
        });
        self.plugins = plugins;
        self.version += 1;
        self.scanned = true;
    }

    /// Every plugin found, by vendor and then name.
    pub fn plugins(&self) -> &[PluginInfo] {
        &self.plugins
    }

    /// The plugins of `kind`.
    pub fn of_kind(&self, kind: PluginKind) -> impl Iterator<Item = &PluginInfo> {
        self.plugins.iter().filter(move |p| p.kind == kind)
    }

    /// Files the last scan couldn't read, and why.
    pub fn failures(&self) -> &[ScanFailure] {
        &self.failures
    }

    pub fn find(&self, format: PluginFormat, id: &str) -> Option<&PluginInfo> {
        self.plugins
            .iter()
            .find(|p| p.format == format && p.id == id)
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.is_some()
    }

    /// A scan has finished (or a list was set) since startup.
    pub fn is_scanned(&self) -> bool {
        self.scanned
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn info(id: &str, name: &str, bridged: bool) -> PluginInfo {
        PluginInfo {
            format: PluginFormat::Clap,
            id: id.into(),
            name: name.into(),
            vendor: "Vendor".into(),
            version: "1".into(),
            kind: PluginKind::Effect,
            categories: Vec::new(),
            path: PathBuf::from(format!("/{name}{bridged}")),
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: Vec::new(),
            note_outputs: Vec::new(),
            bridged,
        }
    }

    #[test]
    fn a_plugin_installed_twice_is_listed_once_preferring_the_native_one() {
        let mut catalog = Catalog::default();
        catalog.set(vec![
            info("b", "Beta", true),
            info("a", "Alpha", false),
            info("b", "Beta", false),
        ]);
        let names: Vec<_> = catalog
            .plugins()
            .iter()
            .map(|p| (&*p.name, p.bridged))
            .collect();
        assert_eq!(names, [("Alpha", false), ("Beta", false)]);
        assert!(
            catalog
                .find(PluginFormat::Clap, "b")
                .is_some_and(|p| !p.bridged)
        );
        assert_eq!(catalog.version(), 1);
    }
}
