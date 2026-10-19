//! Plugins (CLAP, VST3 and VST2) as canopy's devices: the glue between the
//! `plugins` host and the project, the engine and the UI.
//!
//! - [`PluginManager`] keeps an instance for every plugin effect and
//!   instrument the project names, loads their states, and turns what the
//!   plugins do by themselves (their editors) into [`Update`]s for the
//!   project. It lives on the UI thread, which is the plugins' main thread.
//! - [`PluginEffect`] and [`PluginInstrument`] are the engine's
//!   [`effects::Effect`] and [`instruments::Instrument`] around a plugin's
//!   processor; the session's `engine::AudioLookup` hands them out through
//!   the manager.
//! - [`describe`] registers each plugin's description (a leaked
//!   `&'static` descriptor, as built-ins have) so automation, presets,
//!   controller links and panels address its parameters like any other.
//! - [`Catalog`] is what scanning found.
//! - [`WithPlugins`] adds the plugins to any `engine::AudioLookup`, so the
//!   compiler gets them; the engine itself never names this crate.

mod adapter;
mod catalog;
pub mod describe;
mod lookup;
mod manager;

pub use adapter::{PluginEffect, PluginInstrument};
pub use catalog::Catalog;
pub use describe::Described;
pub use lookup::{Plugins, WithPlugins};
pub use manager::{Loaded, Offline, Owner, PluginManager, Status, Update, usable, uses_plugins};
