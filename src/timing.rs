//! Optional startup-phase timing, printed to stderr.
//!
//! Enable with `MARKSIGHT_TIMING=1`: every [`mark`] then prints the
//! milliseconds elapsed since [`init`] next to its label, which traces the
//! phases between process start and the document being on screen. With the
//! variable unset (the shipped default), [`mark`] is a single atomic load
//! and no clock is ever read, so release builds carry the markers for free.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

/// Arm the clock when `MARKSIGHT_TIMING=1` is set; a no-op otherwise.
///
/// Called once from `main` (and from the startup bench) before any other
/// work, so elapsed times count from the earliest possible instant.
pub fn init() {
    if std::env::var_os("MARKSIGHT_TIMING").as_deref() == Some(std::ffi::OsStr::new("1")) {
        ENABLED.store(true, Ordering::Relaxed);
        START.get_or_init(Instant::now);
    }
}

/// Record a startup phase. Does nothing unless [`init`] armed the clock.
pub fn mark(label: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(start) = START.get() {
        eprintln!(
            "[timing] {label}: +{:.2} ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_is_a_noop_without_init() {
        // With the clock never armed there is no start instant; the mark
        // must neither panic nor read any clock.
        mark("never-initialized");
        assert!(!ENABLED.load(Ordering::Relaxed));
    }
}
