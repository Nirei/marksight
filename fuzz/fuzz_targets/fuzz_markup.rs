#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use markview::doc::{Block, Inline};
use markview::markup;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parsed = markview::doc::parse(&text);

    for block in common::all_blocks(&parsed) {
        if let Some(inlines) = inline_content(block) {
            let walk = markup::walk_inlines(inlines, &parsed, None);
            for segment in &walk.segments {
                assert!(
                    markup::is_well_formed(&segment.markup),
                    "markup not well formed: {segment:?}"
                );
                assert!(segment.char_start <= walk.text.chars().count());
            }
        }
        if let Block::Code { text, .. } = block {
            assert!(markup::is_well_formed(&markup::code_markup(text, None)));
        }
    }
});

fn inline_content(block: &Block) -> Option<&Vec<Inline>> {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => Some(inlines),
        _ => None,
    }
}
