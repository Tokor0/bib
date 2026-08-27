//! `bib` — a bibliography manager with first-class Typst/hayagriva output.
//!
//! The library target exists so integration tests can drive the same code the
//! binary does, rather than only shelling out to it.

pub mod cli;
pub mod config;
pub mod formats;
pub mod identify;
pub mod index;
pub mod model;
pub mod providers;
pub mod store;
pub mod util;
