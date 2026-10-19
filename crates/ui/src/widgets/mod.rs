//! Custom widgets drawn with egui's painter.

pub(crate) mod dahdsr;
pub(crate) mod envelope;
pub(crate) mod eq_curve;
pub(crate) mod keys;
pub(crate) mod knob;
mod meter;
pub(crate) mod node_editor;
pub(crate) mod scopes;
pub(crate) mod shape;
pub(crate) mod spectrum;
pub(crate) mod value_entry;

pub use envelope::EnvelopeEditor;
pub use eq_curve::EqCurve;
pub use knob::Knob;
pub use meter::StereoMeter;
pub(crate) mod chrome;
