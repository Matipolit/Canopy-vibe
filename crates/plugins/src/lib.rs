//! Hosts third-party CLAP, VST3 and VST2 plugins, effects and instruments,
//! on Linux and Windows: finding them, loading them, running their audio,
//! parameters and state, and showing their editors. It knows nothing about
//! canopy's projects or engine; a glue crate adapts it.
//!
//! # Shape
//!
//! - [`scan`] finds plugin files in the standard folders and reads what's
//!   in them in separate processes, so a plugin that crashes or hangs while
//!   loading only takes its scanner down. Results are cached by file size
//!   and modification time.
//! - A [`Host`] lives on the main (UI) thread. [`Host::load`] makes a
//!   [`PluginInstance`] from a [`PluginInfo`]. [`Host::idle`], called every
//!   frame and whenever the waker from [`Host::set_waker`] fires, runs what
//!   plugins need run on the main thread: their callbacks, their editors'
//!   timers and file descriptors, and collects what they did as
//!   [`PluginEvent`]s ([`PluginInstance::take_events`]).
//! - [`PluginInstance::activate`] prepares the plugin for a sample rate and
//!   block size and returns its [`Processor`], which moves to the audio
//!   thread. Everything the host does there is real-time safe: no
//!   allocation, locks or I/O. To change the setup, or when the plugin asks
//!   for a restart, bring the processor back and call
//!   [`PluginInstance::deactivate`] or [`PluginInstance::restart`].
//! - Editors open in their own native windows ([`EditorOptions`]): X11 on
//!   Linux (XWayland on a Wayland desktop), Win32 on Windows.
//!
//! # Threads
//!
//! The thread a [`Host`] is made on is its plugins' main thread: loading,
//! activation, parameters' descriptions and text, state, editors and idle
//! work happen there, as CLAP and VST3 require and VST2 hosts do
//! ([`PluginInstance`] isn't `Send`). A thread is the audio thread while it's inside a [`Processor`]
//! call; CLAP's thread check sees it so, export threads included. Plugins
//! may call the host from any thread: requests set flags that the next idle
//! acts on, and main-thread-only requests from elsewhere are refused.
//!
//! # Values
//!
//! Parameters are identified by [`ParamId`] and set in their format's
//! host-side units: CLAP plain values, VST3 and VST2 normalized ones (see
//! [`ParamInfo`]). A block's automation, modulation, notes and MIDI go to
//! [`Processor::process`] as [`Event`]s with sample offsets. Changes a
//! plugin makes itself (its editor, CLAP output events, VST3 `performEdit`,
//! VST2 `audioMasterAutomate`) come back as [`PluginEvent::ParamValue`]
//! inside gestures; the host forwards VST3 editor edits to the processor
//! itself, as VST3 requires.
//!
//! # Formats
//!
//! CLAP through `clap-sys` (CLAP 1.2), VST3 through the `vst3` crate's
//! bindings to the VST 3.8 SDK (MIT-licensed since October 2025), VST2
//! through a small clean-room description of its ABI (`vst2::abi`; the
//! VST2 SDK was never freely licensed). The `unsafe` code that talks to
//! plugins lives in the `clap`, `vst3` and `vst2` modules, `library`
//! (loading) and `editor` (native windows), each call with a `SAFETY` note;
//! the public API is safe.
//!
//! # Windows plugins on Linux
//!
//! Through yabridge, which wraps Windows plugins as Linux ones: see
//! [`scan`] and the README next to this crate's manifest.
//!
//! # Tests
//!
//! [`fixtures`] builds tiny test plugins (`test-plugins/clap-gain`,
//! `test-plugins/vst3-gain` and `test-plugins/vst2-gain`: a gain effect and
//! a sine instrument each) for tests here and in other crates.

mod block;
mod clap;
pub mod editor;
pub mod fixtures;
mod host;
mod info;
mod instance;
mod library;
mod params;
mod process;
mod runloop;
pub mod scan;
pub mod state;
mod thread;
mod vst2;
mod vst3;
mod wake;

use std::fmt;

pub use editor::EditorOptions;
pub use host::Host;
pub use info::{AudioPortInfo, NotePortInfo, PluginFormat, PluginInfo, PluginKind};
pub use instance::{PluginInstance, Processor};
pub use params::{ParamFlags, ParamId, ParamInfo};
pub use process::{AudioConfig, Event, PluginEvent, ProcessStatus, Tail, Transport};
pub use wake::WATCH_INTERVAL;

/// Why something didn't work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file couldn't be loaded as a plugin of its format.
    Load(String),
    /// The file has no plugin with the id asked for.
    NotFound(String),
    /// The plugin failed or refused a call (or was in the wrong state for
    /// it, like activating twice).
    Plugin(String),
    /// Saving or restoring the plugin's state failed.
    State(String),
    /// The plugin has no editor we can show, or opening it failed.
    Editor(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Load(e) => write!(f, "couldn't load the plugin: {e}"),
            Error::NotFound(id) => write!(f, "no plugin {id} in that file"),
            Error::Plugin(e) | Error::State(e) | Error::Editor(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}
