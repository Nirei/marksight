//! Startup bench: measure the phases marksight owns, without GApplication.
//!
//! Pairs with the MARKSIGHT_TIMING=1 markers in src/app.rs. Usage:
//!   DISPLAY=:9 MARKSIGHT_TIMING=1 cargo run --release --example startup_bench -- FILE

use std::path::Path;
use std::time::{Duration, Instant};

use gtk::glib;

use marksight::app;

fn main() {
    let path = std::env::args().nth(1).expect("usage: startup_bench FILE");
    let path = Path::new(&path);
    marksight::timing::init();

    let t = Instant::now();
    gtk::init().expect("gtk init");
    println!("[bench] gtk-init:            +{:7.2} ms", ms(t));

    let gtk_app = gtk::Application::new(None::<&str>, Default::default());
    let t = Instant::now();
    let mv = app::ensure_window(&gtk_app);
    println!("[bench] window-built:        +{:7.2} ms", ms(t));

    let t = Instant::now();
    mv.open_path(path);
    println!("[bench] open_path-returned:  +{:7.2} ms", ms(t));

    // Pump like the smoke tests do until the document page is showing.
    let t = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        while glib::MainContext::default().iteration(false) {}
        if mv.visible_page() == "doc" {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("[bench] doc-visible:         +{:7.2} ms (page={})", ms(t), mv.visible_page());
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
