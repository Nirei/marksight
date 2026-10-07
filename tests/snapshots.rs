//! Golden-file snapshot tests for the pure markup layer.
//!
//! Each `tests/fixtures/*.md` is parsed and dumped via
//! [`markview::markup::debug_dump`]; the dump must match
//! `tests/snapshots/<name>.snap`. Regenerate with
//! `UPDATE_SNAPSHOTS=1 cargo test`.

use std::fs;
use std::path::PathBuf;

fn run() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = root.join("tests/fixtures");
    let snapshots = root.join("tests/snapshots");
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();

    let mut checked = 0;
    let mut entries: Vec<_> = fs::read_dir(&fixtures)
        .expect("fixtures directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort();

    assert!(!entries.is_empty(), "no fixtures found");

    for fixture in entries {
        let name = fixture.file_stem().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&fixture).unwrap();
        let dump = markview::markup::debug_dump(&markview::doc::parse(&source));
        let snap = snapshots.join(format!("{name}.snap"));
        if update || !snap.exists() {
            fs::create_dir_all(&snapshots).unwrap();
            fs::write(&snap, &dump).unwrap();
            eprintln!("updated {}", snap.display());
        } else {
            let expected = fs::read_to_string(&snap).unwrap_or_default();
            assert_eq!(
                expected, dump,
                "snapshot mismatch for {name}: run UPDATE_SNAPSHOTS=1 cargo test and review the diff"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 9,
        "expected at least 9 fixtures, checked {checked}"
    );
}

#[test]
fn snapshots_match_fixtures() {
    run();
}
