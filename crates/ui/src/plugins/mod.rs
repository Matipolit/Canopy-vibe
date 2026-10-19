//! Plugins (CLAP, VST3 and VST2) in the UI: the host the session keeps,
//! the plugin menus, the instrument picker, plugin panels and title-bar
//! controls, editor windows and the Plugin Manager.
//!
//! The plugin instances themselves, their states and what they do by
//! themselves are `hosting::PluginManager`'s; this module shows them and
//! turns the user's choices into project edits.

mod menus;
mod panel;
mod window;

use std::collections::BTreeMap;

use ::plugins::PluginInfo;
use ::plugins::scan::{ScanCommand, ScanOptions};
use eframe::egui::Context;
use hosting::{Loaded, Owner, PluginManager, Update};
use project::{DeviceId, PluginData, Project, TrackId, TrackKind};

use crate::session::Session;
use crate::timeline::TimelineView;

pub use menus::{effect_menu, instrument_picker};
pub(crate) use panel::{instrument_bar_extras, panel};
pub use panel::{missing, title_bar_extras};

/// The plugin host as the session keeps it ([`Session::plugins`]):
/// instances, their editors, what scanning found, and the Plugin Manager
/// window.
#[derive(Default)]
pub struct Plugins {
    pub manager: PluginManager,
    /// The Plugin Manager window is open.
    pub window: bool,
}

impl Plugins {
    /// Starts looking for installed plugins, in the background, and has
    /// the plugins wake the UI when they need the main thread. The app
    /// does this; tests set their own catalog.
    pub fn start(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        let waker = wake.clone();
        self.manager.set_waker(move || waker());
        self.rescan(wake, false);
    }

    /// Scans the plugin folders again; `retry` also reads the files that
    /// failed last time.
    pub fn rescan(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>, retry: bool) {
        let Ok(scanner) = ScanCommand::current_exe() else {
            return;
        };
        let mut options = ScanOptions::new(scanner);
        options.retry_failures = retry;
        self.manager.catalog_mut().scan(options, wake);
    }
}

/// Every frame, before the session updates: lets plugins do what they must
/// on the main thread (their callbacks, timers, editors' events), keeps
/// their instances in line with the project, and puts what they changed
/// by themselves into it.
pub fn pump(_ctx: &Context, session: &mut Session) {
    let (updates, rebuild) = session.plugins.manager.pump(&session.project);
    for update in updates {
        apply(session, update);
    }
    if rebuild {
        session.request_rebuild();
    }
}

/// Puts a change a plugin made by itself into the project.
fn apply(session: &mut Session, update: Update) {
    match update {
        Update::Param {
            owner,
            param,
            value,
        } => {
            // Already heard: the plugin changed itself. The project only
            // catches up, as part of a gesture.
            session.begin_live();
            let changed = match owner {
                Owner::Effect(device) => session.project.set_device_param(device, param, value),
                Owner::Instrument(track) => {
                    session.project.set_instrument_param(track, param, value)
                }
            };
            if changed.is_ok() {
                session.live_changed();
            }
        }
        Update::Commit { label } => session.end_live(&label),
        Update::Fill { owner, values } => {
            // What the plugin already is, not a change: no undo step.
            for (param, value) in values {
                let _ = match owner {
                    Owner::Effect(device) => {
                        session.project.set_device_param(device, &param, value)
                    }
                    Owner::Instrument(track) => {
                        session.project.set_instrument_param(track, &param, value)
                    }
                };
            }
            session.live_changed();
        }
        Update::Edit {
            owner,
            label,
            values,
            state,
        } => {
            session.edit(&label, |p| {
                if let Some(values) = values {
                    match owner {
                        Owner::Effect(device) => p.set_device_params(device, values)?,
                        Owner::Instrument(track) => p.set_instrument_params(track, values)?,
                    }
                }
                if let Some(state) = state {
                    set_state(p, owner, state)?;
                }
                Ok(())
            });
        }
    }
}

/// Replaces the saved state of `owner`'s plugin.
fn set_state(
    p: &mut Project,
    owner: Owner,
    state: project::Blob,
) -> Result<(), project::EditError> {
    let Some(data) = plugin_data(p, owner) else {
        return Ok(());
    };
    let data = PluginData {
        state,
        ..data.clone()
    };
    match owner {
        Owner::Effect(device) => p.set_device_plugin(device, Some(data)),
        Owner::Instrument(track) => p.set_instrument_plugin(track, Some(data)),
    }
}

/// What the project keeps about `owner`'s plugin.
pub(crate) fn plugin_data(project: &Project, owner: Owner) -> Option<&PluginData> {
    match owner {
        Owner::Effect(device) => project.device(device)?.plugin.as_ref(),
        Owner::Instrument(track) => match &project.track(track)?.kind {
            TrackKind::Midi { plugin, .. } => plugin.as_ref(),
            _ => None,
        },
    }
}

/// Every frame, after the panels: the Plugin Manager.
pub fn windows(ctx: &Context, session: &mut Session, _timeline: &mut TimelineView) {
    window::show(ctx, session);
}

/// Puts the state of every plugin into the project ([`project::PluginData`])
/// before it's saved or copied, so what's saved is what's heard. Not an
/// undo step.
pub(crate) fn refresh_states(session: &mut Session) {
    session.plugins.manager.refresh_states(&mut session.project);
}

/// Adds plugin effect `info` to `owner`'s chain (`None`: the master's), as
/// one undo step. Returns the device, or `None` if the plugin wouldn't
/// load (the status bar says why).
pub(crate) fn add_effect(
    session: &mut Session,
    owner: Option<TrackId>,
    info: &PluginInfo,
) -> Option<DeviceId> {
    let loaded = load(session, info)?;
    let (reference, data, values) = (
        loaded.effect_ref(),
        loaded.data.clone(),
        loaded.values.clone(),
    );
    let id = session.edit(&format!("Add {}", info.name), |p| {
        let id = p.add_device(owner, reference, None)?;
        p.set_device_plugin(id, Some(data))?;
        p.set_device_params(id, values)?;
        Ok(id)
    })?;
    session.plugins.manager.attach(Owner::Effect(id), loaded);
    Some(id)
}

/// Makes plugin instrument `info` what `track` plays, as one undo step.
pub(crate) fn set_instrument(session: &mut Session, track: TrackId, info: &PluginInfo) -> bool {
    let Some(loaded) = load(session, info) else {
        return false;
    };
    let reference = loaded.instrument_ref();
    let data = loaded.data.clone();
    let values: BTreeMap<String, f32> = loaded.values.clone();
    let done = session.edit("Change Instrument", |p| {
        p.set_instrument(track, reference)?;
        p.set_instrument_plugin(track, Some(data))?;
        p.set_instrument_params(track, values)
    });
    if done.is_some() {
        session
            .plugins
            .manager
            .attach(Owner::Instrument(track), loaded);
    }
    done.is_some()
}

fn load(session: &mut Session, info: &PluginInfo) -> Option<Loaded> {
    match session.plugins.manager.load(info) {
        Ok(loaded) => Some(loaded),
        Err(err) => {
            session.error(format!("Couldn't load {}: {err}", info.name));
            None
        }
    }
}

/// The plugin a panel's owner plays, in the manager's terms.
pub(crate) fn owner_of(owner: crate::devices::Owner) -> Owner {
    match owner {
        crate::devices::Owner::Device(device) => Owner::Effect(device),
        crate::devices::Owner::Instrument(track) => Owner::Instrument(track),
    }
}
