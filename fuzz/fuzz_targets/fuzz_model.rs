#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use markview::doc::{self, Block};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parsed = doc::parse(&text);

    for entry in &parsed.headings {
        assert!(
            matches!(parsed.resolve(&entry.path), Some(Block::Heading { level, .. }) if *level == entry.level),
            "toc path {:?} does not resolve to its heading",
            entry.path
        );
    }

    let _ = markview::markup::document_text(&parsed);
    let _ = markview::find::TextIndex::build(&parsed);
});
