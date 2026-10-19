//! canopy-learn: learn to play keyboard with a MIDI keyboard, on the DAW's
//! instruments and audio engine.
//!
//! The logic is plain, tested code with no UI: music theory (`theory`),
//! where notation goes (`notation`), grading (`grading`), the exercises
//! (`reading`, `drill`), songs and MIDI files (`song`, `smf`) and saved
//! progress (`progress`). `ui` draws it and drives it from the keyboard,
//! `devices` and `audio` connect it to MIDI and sound.

pub mod audio;
pub mod clock;
pub mod devices;
pub mod drill;
pub mod grading;
pub mod notation;
pub mod progress;
pub mod reading;
pub mod rng;
pub mod smf;
pub mod song;
pub mod theory;
pub mod typing;
pub mod ui;

pub use ui::app::LearnApp;
