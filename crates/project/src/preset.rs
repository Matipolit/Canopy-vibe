//! The file format of presets: an effect's or instrument's settings saved
//! under a name, to load into any copy of it. Where they're kept is the
//! app's business.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Blob;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetFile {
    /// The effect's or instrument's id, so a preset is only ever loaded
    /// into the kind of device it was saved from.
    pub device: String,
    /// Settings by id, as a device stores them.
    pub params: BTreeMap<String, f32>,
    /// A plugin's own state (see `PluginData::state`), for plugins whose
    /// sound isn't all in their parameters. Empty for built-ins.
    #[serde(default, skip_serializing_if = "Blob::is_empty")]
    pub state: Blob,
}

impl PresetFile {
    pub fn to_text(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())
    }

    pub fn from_text(text: &str) -> Result<PresetFile, String> {
        ron::from_str(text).map_err(|e| e.to_string())
    }
}
