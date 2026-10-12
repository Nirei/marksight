//! Property tests: structured markdown generation → model invariants.
//!
//! The generator composes real markdown shapes (nesting, tables, task lists,
//! alerts, footnotes, malformed fragments) rather than feeding raw bytes, so
//! every generated document exercises deep parser paths. Invariants cover
//! TOC path resolution, markup well-formedness, text/markup offset
//! agreement, find bounds, and output size proportional to input size.

use proptest::{collection, prelude::*};

use marksight::doc::{self, Block, Document, Inline};
use marksight::find;
use marksight::markup;

fn plain() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 äöüß✓&<>\"'`.~*_\\[\\]()#>|-]{1,24}"
}

fn arb_inline(depth: u32) -> BoxedStrategy<String> {
    let leaf = plain().prop_map(|t| t);
    if depth == 0 {
        leaf.boxed()
    } else {
        prop_oneof![
            6 => leaf,
            1 => (arb_inline(depth - 1)).prop_map(|t| format!("*{t}*")),
            1 => (arb_inline(depth - 1)).prop_map(|t| format!("**{t}**")),
            1 => (arb_inline(depth - 1)).prop_map(|t| format!("~~{t}~~")),
            1 => (plain()).prop_map(|t| format!("`{t}`")),
            1 => (plain(), plain()).prop_map(|(label, href)| format!("[{label}]({href})")),
            1 => (plain(), plain()).prop_map(|(alt, src)| format!("![{alt}]({src})")),
            1 => plain().prop_map(|label| format!("[^{label}]")),
        ]
        .boxed()
    }
}

fn arb_inlines() -> impl Strategy<Value = String> {
    collection::vec(arb_inline(2), 1..8).prop_map(|parts| parts.join(" "))
}

fn arb_code_body() -> impl Strategy<Value = String> {
    collection::vec("[^`\\n]{0,20}", 0..4).prop_map(|lines| lines.join("\n"))
}

fn arb_table() -> impl Strategy<Value = String> {
    (
        collection::vec(plain(), 1..4),
        collection::vec(plain(), 0..3),
    )
        .prop_map(|(head, rows)| {
            let width = head.len().max(1);
            let mut out = String::from("|");
            for cell in head.iter().take(width) {
                out.push_str(&format!(" {cell} |"));
            }
            out.push_str("\\n|");
            for _ in 0..width {
                out.push_str("---|");
            }
            for row in rows.iter() {
                out.push_str("\\n|");
                out.push_str(&format!(" {row} |"));
                for _ in 1..width {
                    out.push_str(" x |");
                }
            }
            out
        })
}

fn arb_block() -> BoxedStrategy<String> {
    prop_oneof![
        3 => (1u8..7, arb_inlines()).prop_map(|(level, text)| format!("{} {}", "#".repeat(level as usize), text)),
        4 => arb_inlines(),
        2 => (plain(), arb_code_body()).prop_map(|(info, body)| format!("```{info}\n{body}\n```")),
        1 => arb_code_body().prop_map(|body| body.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")),
        1 => plain().prop_map(|t| format!("> {t}")),
        1 => (arb_inlines(), proptest::bool::ANY).prop_map(|(t, task)| {
            if task { format!("- [x] {t}") } else { format!("- {t}") }
        }),
        1 => arb_inlines().prop_map(|t| format!("1. {t}")),
        1 => arb_table(),
        1 => Just("---".to_owned()),
        1 => plain().prop_map(|t| format!("<div>{t}</div>")),
        1 => plain().prop_map(|t| format!("> [!NOTE]\n> {t}")),
    ]
    .boxed()
}

fn arb_markdown() -> impl Strategy<Value = String> {
    collection::vec(arb_block(), 0..15).prop_map(|blocks| blocks.join("\n\n"))
}

fn inlines_of(block: &Block) -> Option<&Vec<Inline>> {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => Some(inlines),
        _ => None,
    }
}

fn walk_all_blocks<'a>(blocks: &'a [Block], out: &mut Vec<&'a Block>) {
    for block in blocks {
        out.push(block);
        match block {
            Block::Quote { blocks, .. } => walk_all_blocks(blocks, out),
            Block::List { items, .. } => {
                for item in items {
                    walk_all_blocks(&item.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// Independent count of the document's text units (one per rendered label:
/// paragraphs, headings, code, per table cell) used to cross-check
/// [`marksight::markup::text_units`].
fn count_units(blocks: &[Block]) -> usize {
    let mut count = 0;
    for block in blocks {
        match block {
            Block::Heading { .. } | Block::Paragraph(_) | Block::Code { .. } => count += 1,
            Block::Quote { blocks, .. } => count += count_units(blocks),
            Block::List { items, .. } => {
                for item in items {
                    count += count_units(&item.blocks);
                }
            }
            Block::Table { head, rows, .. } => {
                count += head.iter().chain(rows.iter()).map(Vec::len).sum::<usize>();
            }
            Block::Rule => {}
        }
    }
    count
}

fn assert_well_formed_block(
    block: &Block,
    doc: &Document,
) -> Result<(), proptest::test_runner::TestCaseError> {
    if let Some(inlines) = inlines_of(block) {
        let walk = markup::walk_inlines(inlines, doc, None);
        for segment in &walk.segments {
            prop_assert!(
                markup::is_well_formed(&segment.markup),
                "unbalanced markup {segment:?}"
            );
            prop_assert!(
                markup::is_pango_safe(&segment.markup),
                "pango-unsafe characters reached the markup: {segment:?}"
            );
            prop_assert!(segment.char_start <= walk.text.chars().count());
        }
        // The searchable text is exactly the characters the markup
        // renders: image alt (tooltip-only) and anything else invisible
        // must never enter the index, or find reports unshowable matches.
        let rendered: usize = walk
            .segments
            .iter()
            .map(|s| strip_tags(&s.markup).chars().count())
            .sum();
        prop_assert_eq!(
            rendered,
            walk.text.chars().count(),
            "walked text must be exactly the rendered characters"
        );
    }
    if let Block::Code { text, .. } = block {
        prop_assert!(markup::is_well_formed(&markup::code_markup(text, None)));
        prop_assert!(markup::is_pango_safe(&markup::code_markup(text, None)));
    }
    Ok(())
}

fn strip_tags(markup: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in markup.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // The only entities our markup ever contains are the five escape_text
    // and escape_attr emit (is_well_formed enforces that); unescape them so
    // character counts compare against the plain text.
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn toc_paths_resolve_to_headings(markdown in arb_markdown()) {
        let doc = doc::parse(&markdown);
        let mut paths = std::collections::HashSet::new();
        for entry in &doc.headings {
            prop_assert!(paths.insert(entry.path.clone()), "duplicate toc path {:?}", entry.path);
            match doc.resolve(&entry.path) {
                Some(Block::Heading { level, .. }) => prop_assert_eq!(*level, entry.level),
                other => return Err(proptest::test_runner::TestCaseError::fail(format!("path {:?} resolved to {other:?}", entry.path))),
            }
        }
    }

    #[test]
    fn markup_is_well_formed_everywhere(markdown in arb_markdown()) {
        let doc = doc::parse(&markdown);
        let mut blocks = Vec::new();
        walk_all_blocks(&doc.blocks, &mut blocks);
        for footnote in &doc.footnotes {
            walk_all_blocks(&footnote.blocks, &mut blocks);
        }
        for block in &blocks {
            assert_well_formed_block(block, &doc)?;
        }
    }

    #[test]
    fn highlight_ranges_stay_in_bounds(markdown in arb_markdown(), needle in plain()) {
        let doc = doc::parse(&markdown);
        let index = find::TextIndex::build(&doc);
        let matches = find::find(&index, &needle);
        for m in &matches {
            let entry_len = index.entries.get(m.entry).map(|e| e.chars().count()).unwrap_or(0);
            prop_assert!(m.start < m.end);
            prop_assert!(m.end <= entry_len, "match {m:?} exceeds entry length {entry_len}");
        }
    }

    #[test]
    fn highlight_rebuild_stays_well_formed(markdown in arb_markdown(), needle in plain()) {
        let doc = doc::parse(&markdown);
        let index = find::TextIndex::build(&doc);
        let matches = find::find(&index, &needle);
        let active_range = matches.first().map(|m| m.start..m.end);
        for (entry, block) in doc.blocks.iter().enumerate() {
            let Some(inlines) = inlines_of(block) else { continue };
            let ranges = find::entry_ranges(&matches, entry);
            let active_here = find::active_range(&matches, matches.iter().position(|_| false), entry);
            let _ = active_here;
            let hl = markup::Highlight {
                ranges: &ranges,
                active: active_range.as_ref(),
                hit_color: "#11223344",
                active_color: "#55667788",
            };
            let walk = markup::walk_inlines(inlines, &doc, Some(hl));
            for segment in &walk.segments {
                prop_assert!(markup::is_well_formed(&segment.markup), "highlight broke markup: {segment:?}");
            }
        }
    }

    #[test]
    fn output_size_is_proportional(markdown in arb_markdown()) {
        let doc = doc::parse(&markdown);
        let text = markup::document_text(&doc);
        let dump = markup::debug_dump(&doc);
        let input = markdown.chars().count().max(1);
        prop_assert!(text.chars().count() <= input * 3, "text exploded: {} -> {}", input, text.chars().count());
        prop_assert!(dump.len() <= input * 40, "dump exploded: {} -> {}", input, dump.len());
    }

    #[test]
    fn index_units_match_an_independent_leaf_walk(markdown in arb_markdown()) {
        let doc = doc::parse(&markdown);
        let index = find::TextIndex::build(&doc);
        let count = |blocks: &[Block]| count_units(blocks);
        let expected: usize = count(&doc.blocks)
            + doc.footnotes.iter().map(|f| count(&f.blocks)).sum::<usize>();
        prop_assert_eq!(index.entries.len(), expected);
    }

    #[test]
    fn matches_point_at_entries_containing_the_needle(markdown in arb_markdown(), needle in plain()) {
        let doc = doc::parse(&markdown);
        let index = find::TextIndex::build(&doc);
        let matches = find::find(&index, &needle);
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let needle: String = needle.chars().map(fold).collect();
        for m in &matches {
            let entry = index
                .entries
                .get(m.entry)
                .unwrap_or_else(|| panic!("match {m:?} past the index"));
            let folded: String = entry.chars().map(fold).collect();
            prop_assert!(
                folded.contains(&needle),
                "match {m:?} points at entry {entry:?} which lacks the needle"
            );
        }
    }

    #[test]
    fn cycle_is_total_over_matches(markdown in arb_markdown(), needle in plain()) {
        let doc = doc::parse(&markdown);
        let index = find::TextIndex::build(&doc);
        let matches = find::find(&index, &needle);
        if matches.is_empty() {
            prop_assert_eq!(find::cycle(&matches, None, true), None);
            prop_assert_eq!(find::cycle(&matches, Some(0), false), None);
        } else {
            let mut active = find::cycle(&matches, None, true);
            prop_assert_eq!(active, Some(0));
            for _ in 0..matches.len() + 2 {
                active = find::cycle(&matches, active, true);
                prop_assert!(active.is_some());
            }
            let mut back = find::cycle(&matches, None, false);
            prop_assert_eq!(back, Some(matches.len() - 1));
            for _ in 0..matches.len() + 2 {
                back = find::cycle(&matches, back, false);
                prop_assert!(back.is_some());
            }
        }
    }
}
