//! Link activation and in-document navigation: the link handler handed to
//! the renderer, anchor scrolling, and the external-URI allowlist.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use super::App;
use crate::render;

/// The handler every rendered label routes `activate-link` through.
///
/// In-document targets: footnote rows ("#fn-N") and heading anchors from
/// explicit {#id} attributes or generated slugs share one map. A `#` URI is
/// always handled, also for unknown anchors: falling through would hand
/// "#whatever" to GTK's default launcher, which has no meaningful way to
/// open it. External links always stop propagation too, also for refused
/// schemes: letting GTK fall through would hand the URI to the default
/// launcher and bypass the allowlist.
pub(super) fn link_handler(
    app: &Rc<App>,
    anchors: &Rc<RefCell<HashMap<String, gtk::Widget>>>,
) -> render::LinkHandler {
    let anchors = anchors.clone();
    let app_for_links = app.clone();
    Rc::new(move |uri: &str| {
        if let Some(anchor) = uri.strip_prefix('#') {
            let target = anchors.borrow().get(anchor).cloned();
            if let Some(target) = target {
                scroll_to(&app_for_links, &target);
            }
            return true;
        }
        launch_uri(uri);
        true
    })
}

pub(super) fn scroll_to(app: &Rc<App>, target: &impl IsA<gtk::Widget>) {
    let target = target.upcast_ref::<gtk::Widget>().clone();
    let generation = {
        let mut state = app.state.borrow_mut();
        state.scroll_generation += 1;
        state.scroll_generation
    };
    apply_scroll(&app.ui.scrolled, &target);
    for delay in [80, 240] {
        let app = app.clone();
        let target = target.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            if app.state.borrow().scroll_generation == generation {
                apply_scroll(&app.ui.scrolled, &target);
            }
        });
    }
}

fn apply_scroll(scrolled: &gtk::ScrolledWindow, target: &gtk::Widget) {
    let Some(viewport) = scrolled.child().and_downcast::<gtk::Viewport>() else {
        return;
    };
    let Some(content) = viewport.child() else {
        return;
    };
    let Some(point) = target.compute_point(&content, &gtk::graphene::Point::new(0.0, 0.0)) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let desired = point.y() as f64 - 12.0;
    let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(desired.clamp(adjustment.lower(), max));
}

fn launch_uri(uri: &str) {
    if !is_launchable_uri(uri) {
        let shown: String = uri.chars().take(120).collect();
        eprintln!("marksight: refusing to open link (scheme not allowed): {shown}");
        return;
    }
    gtk::UriLauncher::new(uri).launch(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, |_| {});
}

/// Documents arrive from untrusted sources, so external links go through
/// a short allowlist. Only these schemes are handed to the system launcher;
/// everything else (javascript:, data:, file:, custom handlers, relative
/// paths) is refused, because a viewer that renders no HTML and runs no
/// scripts should not become a trampoline for them either.
fn is_launchable_uri(uri: &str) -> bool {
    let Some((scheme, _rest)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

#[cfg(test)]
mod tests {
    use super::is_launchable_uri;

    #[test]
    fn uri_allowlist_accepts_only_web_and_mail() {
        assert!(is_launchable_uri("https://example.com/a?b=c"));
        assert!(is_launchable_uri("http://example.com"));
        assert!(is_launchable_uri("HTTP://EXAMPLE.COM"));
        assert!(is_launchable_uri("mailto:someone@example.com"));
        assert!(!is_launchable_uri("javascript:alert(1)"));
        assert!(!is_launchable_uri("JaVaScRiPt:alert(1)"));
        assert!(!is_launchable_uri("data:text/html;base64,PHNjcmlwdD4="));
        assert!(!is_launchable_uri("file:///etc/passwd"));
        assert!(!is_launchable_uri("gopher://example.com"));
        assert!(!is_launchable_uri("example.com/relative"));
        assert!(!is_launchable_uri(""));
        assert!(!is_launchable_uri("#fragment"));
    }
}
