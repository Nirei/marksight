#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use marksight::doc::{Block, Inline};
use marksight::markup;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parsed = marksight::doc::parse(&text);

    for block in common::all_blocks(&parsed) {
        if let Some(inlines) = inline_content(block) {
            let walk = markup::walk_inlines(inlines, &parsed, None);
            for segment in &walk.segments {
                assert!(
                    markup::is_well_formed(&segment.markup),
                    "markup not well formed: {segment:?}"
                );
                assert!(
                    markup::is_pango_safe(&segment.markup),
                    "pango-unsafe characters reached the markup: {segment:?}"
                );
                assert!(segment.char_start <= walk.text.chars().count());
            }
        }
        if let Block::Code { text, .. } = block {
            let markup = markup::code_markup(text, None);
            assert!(markup::is_well_formed(&markup));
            assert!(markup::is_pango_safe(&markup));
        }
    }
});

fn inline_content(block: &Block) -> Option<&Vec<Inline>> {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => Some(inlines),
        _ => None,
    }
}
