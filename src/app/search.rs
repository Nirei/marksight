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

pub(super) fn run_find(app: &Rc<App>, needle: &str) {
    let (hit, active_color) = probe_colors(app);
    let state = app.state.borrow();
    let Some(rendered) = &state.rendered else {
        return;
    };
    let matches = find::find(&state.index, needle);
    let active = if matches.is_empty() {
        None
    } else if needle == state.find_needle && state.find_active.is_some_and(|a| a < matches.len()) {
        state.find_active
    } else {
        find::cycle(&matches, None, true)
    };
    render::apply_highlight(
        &state.doc,
        rendered,
        |entry| {
            (
                find::entry_ranges(&matches, entry),
                find::active_range(&matches, active, entry),
            )
        },
        &hit,
        &active_color,
    );
    let counter = if needle.trim().is_empty() {
        String::new()
    } else {
        format!("{}/{}", active.map(|a| a + 1).unwrap_or(0), matches.len())
    };
    app.ui.match_counter.set_text(&counter);
    let scroll_target = active
        .and_then(|a| matches.get(a))
        .and_then(|m| rendered.first_label(m.entry).cloned());
    drop(state);
    if let Some(target) = scroll_target {
        scroll_to(app, &target);
    }
    let mut state = app.state.borrow_mut();
    state.matches = matches;
    state.active = active;
    state.find_needle = needle.to_string();
    state.find_active = active;
}

pub(super) fn cycle_find(app: &Rc<App>, forward: bool) {
    let (hit, active_color) = probe_colors(app);
    let state = app.state.borrow();
    let active = find::cycle(&state.matches, state.active, forward);
    let Some(rendered) = &state.rendered else {
        return;
    };
    render::apply_highlight(
        &state.doc,
        rendered,
        |entry| {
            (
                find::entry_ranges(&state.matches, entry),
                find::active_range(&state.matches, active, entry),
            )
        },
        &hit,
        &active_color,
    );
    if !state.matches.is_empty() {
        app.ui.match_counter.set_text(&format!(
            "{}/{}",
            active.map(|a| a + 1).unwrap_or(0),
            state.matches.len()
        ));
    }
    let scroll_target = active
        .and_then(|a| state.matches.get(a))
        .and_then(|m| rendered.first_label(m.entry).cloned());
    drop(state);
    if let Some(target) = scroll_target {
        scroll_to(app, &target);
    }
    let mut state = app.state.borrow_mut();
    state.active = active;
    state.find_active = active;
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
