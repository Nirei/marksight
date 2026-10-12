//! Gettext wiring for the UI layer.
//!
//! [`init`] is called once from `main` before the application is built and
//! before any thread is spawned, which is the one safe moment to switch the
//! process locale. Until a compiled catalog is installed under
//! [`LOCALEDIR`], every lookup falls back to the English source strings, so
//! untranslated builds behave exactly as before this module existed.
//!
//! The [`gettext`] and [`ngettext`] re-exports are the markers extraction
//! tools look for (see `po/README.md`). Placeholders inside marked strings
//! are named (`{heading}`, not `{}`) because `format!` needs a literal:
//! runtime substitution happens through `str::replace` at each call site.

pub use gettextrs::{gettext, ngettext};

/// The message domain: catalogs install as
/// `<localedir>/<locale>/LC_MESSAGES/marksight.mo`.
pub const GETTEXT_PACKAGE: &str = "marksight";

/// Where compiled catalogs are looked up. Packaging can override it at
/// build time with `LOCALEDIR=<dir>`; the default is the `/usr` prefix
/// the deb package installs under.
pub const LOCALEDIR: &str = match option_env!("LOCALEDIR") {
    Some(dir) => dir,
    None => "/usr/share/locale",
};

/// Switches to the user's locale and selects this crate's message domain.
pub fn init() {
    setlocale(LocaleCategory::LcAll, "");
    // These record the domain and its search path; they only fail on
    // allocation errors, so failing loudly is right here. A missing or
    // untranslated catalog is not an error: lookups then return the source
    // strings.
    bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR)
        .and_then(|_| bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8"))
        .and_then(|_| textdomain(GETTEXT_PACKAGE))
        .expect("marksight: cannot set up the gettext domain");
}

use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
