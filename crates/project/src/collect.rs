//! The files a project plays from, and rewriting where they are: what
//! File → Collect and Save gathers, and what makes a collected project
//! open from wherever it's unpacked.
//!
//! A collected project names its files relative to its own folder, with
//! `/` between folders whatever system wrote it, so it opens the same on
//! Linux and Windows. In memory paths are always whole: the session makes
//! them so on loading ([`Project::resolve_relative_paths`]) and writes a
//! collected project's back as relative ones when saving it
//! ([`Project::relative_to`]). Ordinary projects keep whole paths and
//! re-save byte for byte as before.

use std::path::{Component, Path, PathBuf};

use crate::{Project, SourceId, TrackId, TrackKind, walk_devices};

/// One place the project names a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileRef {
    /// An audio file clips play.
    Source(SourceId),
    /// A MIDI track's instrument file: a Sampler's `.sfz` or a Slicer's loop.
    Instrument(TrackId),
    /// A Wavetable oscillator's user table (0 = A, 1 = B, 2 = C).
    Wavetable(TrackId, usize),
}

/// A plugin the project uses, as it names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NeededPlugin {
    pub name: String,
    pub vendor: String,
}

impl Project {
    /// Every file the project names, in a stable order: sources first,
    /// then tracks top to bottom.
    pub fn files(&self) -> Vec<(FileRef, &Path)> {
        let mut files: Vec<(FileRef, &Path)> = self
            .sources
            .iter()
            .map(|s| (FileRef::Source(s.id), s.path.as_path()))
            .collect();
        self.visit(|track, _| {
            if let Some(file) = track.instrument_file() {
                files.push((FileRef::Instrument(track.id), file));
            }
            for (osc, table) in track.wavetable_files().iter().enumerate() {
                if let Some(table) = table {
                    files.push((FileRef::Wavetable(track.id, osc), table));
                }
            }
        });
        files
    }

    /// Calls `f` with every file path the project names, to change it.
    pub fn map_files(&mut self, mut f: impl FnMut(FileRef, &mut PathBuf)) {
        for source in &mut self.sources {
            f(FileRef::Source(source.id), &mut source.path);
        }
        self.visit_mut(|track| {
            let id = track.id;
            if let TrackKind::Midi { file, tables, .. } = &mut track.kind {
                if let Some(file) = file {
                    f(FileRef::Instrument(id), file);
                }
                for (osc, table) in tables.iter_mut().enumerate() {
                    if let Some(table) = table {
                        f(FileRef::Wavetable(id, osc), table);
                    }
                }
            }
        });
    }

    /// Makes every relative path whole, relative to `dir` (the project
    /// file's folder). Returns whether there were any: whether this is a
    /// collected project, to be saved with relative paths again.
    pub fn resolve_relative_paths(&mut self, dir: &Path) -> bool {
        let mut any = false;
        self.map_files(|_, path| {
            if is_relative(path) {
                *path = dir.join(from_portable(path));
                any = true;
            }
        });
        any
    }

    /// A copy whose paths inside `dir` are relative to it, written with `/`,
    /// for saving a collected project to `dir`. Paths elsewhere stay whole.
    pub fn relative_to(&self, dir: &Path) -> Project {
        let mut project = self.clone();
        project.map_files(|_, path| {
            if let Ok(inside) = path.strip_prefix(dir) {
                *path = to_portable(inside);
            }
        });
        project
    }

    /// The plugins the project uses, each once, by name.
    pub fn plugins_needed(&self) -> Vec<NeededPlugin> {
        fn named(name: &str, vendor: &str) -> NeededPlugin {
            NeededPlugin {
                name: name.to_owned(),
                vendor: vendor.to_owned(),
            }
        }
        fn effects(list: &[crate::Device], needed: &mut Vec<NeededPlugin>) {
            walk_devices(list, &mut |device| {
                if !device.effect.format.is_builtin() {
                    needed.push(match &device.plugin {
                        Some(plugin) => named(&plugin.name, &plugin.vendor),
                        None => named(&device.effect.id, ""),
                    });
                }
            });
        }
        let mut needed = Vec::new();
        effects(&self.master_devices, &mut needed);
        self.visit(|track, _| {
            effects(&track.devices, &mut needed);
            if let TrackKind::Midi {
                instrument, plugin, ..
            } = &track.kind
                && !instrument.format.is_builtin()
            {
                needed.push(match plugin {
                    Some(plugin) => named(&plugin.name, &plugin.vendor),
                    None => named(&instrument.id, ""),
                });
            }
        });
        needed.sort();
        needed.dedup();
        needed
    }
}

/// Relative on any system: `C:\…` and `\\server\…` are whole paths even
/// when a project saved on Windows is read on Linux, where `Path` would
/// call them relative.
fn is_relative(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return path.is_relative();
    };
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    path.is_relative() && !drive && !text.starts_with('\\') && !text.starts_with('/')
}

/// `inside` written with `/` between folders.
fn to_portable(inside: &Path) -> PathBuf {
    let parts: Vec<_> = inside
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect();
    PathBuf::from(parts.join("/"))
}

/// A path written with `/` (or `\`), as this system spells it.
fn from_portable(path: &Path) -> PathBuf {
    path.to_string_lossy()
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectRef, Format, NewTrack, PluginData, TrackPosition};
    use instruments::InstrumentRef;

    fn project() -> (Project, TrackId, TrackId) {
        let mut p = Project::default();
        p.add_source(PathBuf::from("/songs/demo/loop.wav"), 44_100, 1, 2);
        let sampler = p.add_track(NewTrack::Midi, TrackPosition::END).unwrap();
        p.set_instrument_file(sampler, Some(PathBuf::from("/libs/piano/piano.sfz")))
            .unwrap();
        let synth = p.add_track(NewTrack::Midi, TrackPosition::END).unwrap();
        p.set_wavetable_file(synth, 2, Some(PathBuf::from("/songs/demo/Tables/saw.wav")))
            .unwrap();
        (p, sampler, synth)
    }

    #[test]
    fn files_lists_sources_instrument_files_and_tables() {
        let (p, sampler, synth) = project();
        let files: Vec<_> = p
            .files()
            .into_iter()
            .map(|(r, path)| (r, path.to_owned()))
            .collect();
        assert_eq!(
            files,
            vec![
                (
                    FileRef::Source(p.sources[0].id),
                    PathBuf::from("/songs/demo/loop.wav")
                ),
                (
                    FileRef::Instrument(sampler),
                    PathBuf::from("/libs/piano/piano.sfz")
                ),
                (
                    FileRef::Wavetable(synth, 2),
                    PathBuf::from("/songs/demo/Tables/saw.wav")
                ),
            ]
        );
    }

    #[test]
    fn paths_inside_the_folder_save_relative_and_load_whole() {
        let (p, _, _) = project();
        let saved = p.relative_to(Path::new("/songs/demo"));
        let paths: Vec<_> = saved
            .files()
            .into_iter()
            .map(|(_, p)| p.to_owned())
            .collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("loop.wav"),
                PathBuf::from("/libs/piano/piano.sfz"),
                PathBuf::from("Tables/saw.wav"),
            ]
        );

        let text = saved.to_file_string().unwrap();
        assert!(text.contains("\"Tables/saw.wav\""), "{text}");
        let mut loaded = Project::from_file_string(&text).unwrap();
        assert!(loaded.resolve_relative_paths(Path::new("/elsewhere/demo")));
        let paths: Vec<_> = loaded
            .files()
            .into_iter()
            .map(|(_, p)| p.to_owned())
            .collect();
        assert_eq!(
            paths,
            vec![
                Path::new("/elsewhere/demo").join("loop.wav"),
                PathBuf::from("/libs/piano/piano.sfz"),
                Path::new("/elsewhere/demo").join("Tables").join("saw.wav"),
            ]
        );
    }

    #[test]
    fn a_project_of_whole_paths_is_not_collected() {
        let (mut p, _, _) = project();
        let before = p.clone();
        assert!(!p.resolve_relative_paths(Path::new("/x")));
        assert_eq!(p, before);
    }

    #[test]
    fn windows_paths_are_whole_on_any_system() {
        for whole in [
            r"C:\Samples\kick.wav",
            r"\\server\share\kick.wav",
            "/a/b.wav",
        ] {
            assert!(!is_relative(Path::new(whole)), "{whole}");
        }
        assert!(is_relative(Path::new("Samples/kick.wav")));
        assert_eq!(
            from_portable(Path::new(r"Samples\Drums/kick.wav")),
            Path::new("Samples").join("Drums").join("kick.wav")
        );
    }

    #[test]
    fn plugins_needed_names_each_plugin_once() {
        let (mut p, sampler, _) = project();
        let data = |name: &str| PluginData {
            name: name.to_owned(),
            vendor: "Acme".to_owned(),
            path: None,
            state: Default::default(),
        };
        let plugin_effect = EffectRef {
            format: Format::Clap,
            id: "com.acme.verb".to_owned(),
            version: String::new(),
        };
        for track in [None, Some(sampler)] {
            let device = p.add_device(track, plugin_effect.clone(), None).unwrap();
            p.set_device_plugin(device, Some(data("Verb"))).unwrap();
        }
        p.add_device(None, EffectRef::builtin("canopy.reverb"), None)
            .unwrap();
        p.set_instrument(
            sampler,
            InstrumentRef {
                format: Format::Clap,
                id: "com.acme.synth".to_owned(),
                version: String::new(),
            },
        )
        .unwrap();
        p.set_instrument_plugin(sampler, Some(data("Synth")))
            .unwrap();

        let names: Vec<_> = p.plugins_needed().into_iter().map(|n| n.name).collect();
        assert_eq!(names, ["Synth", "Verb"]);
    }
}
