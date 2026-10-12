//! Find-in-document: the debounced search poll, match highlighting and
//! cycling driven by the search bar.

use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use super::App;
use super::links::scroll_to;
use crate::find;
use crate::render;

const FIND_DEBOUNCE: Duration = Duration::from_millis(120);
const FIND_POLL: Duration = Duration::from_millis(40);

/// Installs the per-window find poll. Held weakly: a closed window stops
/// paying for its own timer instead of keeping the App alive forever.
pub(super) fn install_find_poll(app: &Rc<App>) {
    let find_weak = Rc::downgrade(app);
    glib::timeout_add_local(FIND_POLL, move || {
        let Some(find_app) = find_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if find_app.closed.get() {
            return glib::ControlFlow::Break;
        }
        let pending = find_app.state.borrow_mut().find_pending.take();
        if let Some(at) = pending {
            if at.elapsed() < FIND_DEBOUNCE {
                find_app.state.borrow_mut().find_pending = Some(at);
            } else {
                let needle = find_app.ui.search_entry.text().to_string();
                run_find(&find_app, &needle);
            }
        }
        glib::ControlFlow::Continue
    });
}

pub(super) fn schedule_find(app: &Rc<App>) {
    if let Ok(mut state) = app.state.try_borrow_mut() {
        state.find_pending = Some(std::time::Instant::now());
    }
}

pub(super) fn clear_find(app: &Rc<App>) {
    if !app.ui.search_entry.text().is_empty() {
        app.ui.search_entry.set_text("");
    } else {
        run_find(app, "");
    }
}

/// Applies the current match set to the labels, the counter and the
/// viewport, and scrolls the active match into view. The one owner of the
/// label-update flow, shared by the re-search ([`run_find`]) and cycle
/// ([`cycle_find`]) paths; both persist their state first, so this reads
/// a single consistent borrow.
fn present_matches(app: &Rc<App>, active: Option<usize>) {
    let (hit, active_color) = probe_colors(app);
    let state = app.state.borrow();
    let Some(rendered) = &state.rendered else {
        return;
    };
    // find returns matches sorted by entry: one pass buckets them, and
    // every label group then reads its slice by index.
    let by_entry = find::by_entry(&state.matches);
    let active_match = active.and_then(|a| state.matches.get(a)).copied();
    render::apply_highlight(
        &state.doc,
        rendered,
        &by_entry,
        active_match.as_ref(),
        &hit,
        &active_color,
    );
    let counter = if state.find_needle.trim().is_empty() {
        String::new()
    } else {
        format!(
            "{}/{}",
            active.map(|a| a + 1).unwrap_or(0),
            state.matches.len()
        )
    };
    app.ui.match_counter.set_text(&counter);
    let scroll_target = active
        .and_then(|a| state.matches.get(a))
        .and_then(|m| rendered.first_label(m.entry).cloned());
    drop(state);
    if let Some(target) = scroll_target {
        scroll_to(app, &target);
    }
}

pub(super) fn run_find(app: &Rc<App>, needle: &str) {
    let active = {
        let state = app.state.borrow();
        if state.rendered.is_none() {
            return;
        }
        let matches = find::find(&state.index, needle);
        // Same needle as before (a live reload re-runs the search) and the
        // old position still in range: continue where the reader was.
        // Anything else starts at the first match.
        let active = if matches.is_empty() {
            None
        } else if needle == state.find_needle && state.active.is_some_and(|a| a < matches.len()) {
            state.active
        } else {
            find::cycle(&matches, None, true)
        };
        drop(state);
        let mut state = app.state.borrow_mut();
        state.matches = matches;
        state.active = active;
        state.find_needle = needle.to_string();
        active
    };
    present_matches(app, active);
}

pub(super) fn cycle_find(app: &Rc<App>, forward: bool) {
    let active = {
        let state = app.state.borrow();
        if state.rendered.is_none() {
            return;
        }
        find::cycle(&state.matches, state.active, forward)
    };
    present_matches(app, active);
    app.state.borrow_mut().active = active;
}

fn probe_colors(app: &Rc<App>) -> (String, String) {
    (
        rgba_hex(app.ui.probe_hit.color()),
        rgba_hex(app.ui.probe_active.color()),
    )
}

fn rgba_hex(color: gdk::RGBA) -> String {
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        to_byte(color.red()),
        to_byte(color.green()),
        to_byte(color.blue()),
        to_byte(color.alpha())
    )
}
