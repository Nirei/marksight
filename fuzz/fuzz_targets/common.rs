//! Shared fuzzing helper: every block of a document, in tree order.

use marksight::doc::{Block, Document};

pub fn all_blocks(doc: &Document) -> Vec<&Block> {
    let mut out = Vec::new();
    walk(&doc.blocks, &mut out);
    for footnote in &doc.footnotes {
        walk(&footnote.blocks, &mut out);
    }
    out
}

fn walk<'a>(blocks: &'a [Block], out: &mut Vec<&'a Block>) {
    for block in blocks {
        out.push(block);
        match block {
            Block::Quote { blocks, .. } => walk(blocks, out),
            Block::List { items, .. } => {
                for item in items {
                    walk(&item.blocks, out);
                }
            }
            _ => {}
        }
    }
}
