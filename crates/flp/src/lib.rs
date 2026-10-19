//! Reads FL Studio projects (`.flp`) and turns them into canopy projects.
//!
//! [`parse`] reads a file into [`Project`], a plain description in FL's own
//! terms. [`Plan`] lists every FL instrument and effect with the built-in it
//! would become, for the user to change; [`Files`] finds the samples it
//! names; [`import`] then builds a `project::Project` and reports what
//! couldn't be carried over.

mod convert;
pub mod files;
pub mod ids;
mod import;
pub mod model;
pub mod native;
mod parse;
pub mod plugins;
pub mod reader;
pub mod vital;
pub mod vst;
pub mod write;

pub use files::{AudioInfo, Files, Found};
pub use import::{
    Choices, GeneratedFile, Import, Options, Plan, PlanItem, Role, Summary, channel_volume_db,
    import, insert_volume_db, velocity,
};
pub use model::*;
pub use parse::parse;
pub use plugins::Choice;
pub use reader::ParseError;
