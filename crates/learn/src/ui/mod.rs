//! The app: a view per kind of practice over a shared on-screen piano.

pub mod app;
mod drill_view;
mod free_view;
pub mod glyphs;
pub mod piano;
mod reading_view;
mod song_view;
pub mod staff;
mod stats_view;
pub mod theme;
#[cfg(test)]
mod ui_tests;
mod widgets;
