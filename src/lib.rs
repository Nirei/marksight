//! marksight — a minimal, theme-native markdown viewer.
//!
//! The crate is split into a GTK-free core ([`doc`], [`markup`], [`find`]) and
//! a UI layer (`render`, `watch`, `app`) that is only compiled with the default
//! `ui` feature. The core is plain data over plain functions so it can be
//! unit-tested, property-tested, and fuzzed without a display server.

pub mod doc;
pub mod find;
pub mod markup;

#[cfg(feature = "ui")]
pub mod app;

#[cfg(feature = "ui")]
pub mod render;

#[cfg(feature = "ui")]
pub mod watch;
