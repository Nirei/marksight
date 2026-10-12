#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use marksight::find;
use marksight::markup;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parsed = marksight::doc::parse(&text);
    let index = find::TextIndex::build(&parsed);

    let needle_len = data.len() % 16;
    let needle =
        String::from_utf8_lossy(&data[data.len().saturating_sub(needle_len)..]).to_string();
    let matches = find::find(&index, &needle);

    for m in &matches {
        let entry_len = index
            .entries
            .get(m.entry)
            .map(|e| e.chars().count())
            .unwrap_or(0);
        assert!(m.start < m.end, "empty match");
        assert!(
            m.end <= entry_len,
            "match {m:?} out of bounds for length {entry_len}"
        );
        // The entry a match points at must actually contain the needle
        // (folded the same way find folds), or the index's decomposition
        // and the highlight offsets have drifted apart.
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let needle_folded: String = needle.chars().map(fold).collect();
        let entry_folded: String = index
            .entries
            .get(m.entry)
            .map(|e| e.chars().map(fold).collect())
            .unwrap_or_default();
        assert!(
            entry_folded.contains(&needle_folded),
            "match {m:?} points at entry without the needle: {entry_folded:?}"
        );
    }

    let active = matches.first().map(|m| m.start..m.end);
    let by_entry = find::by_entry(&matches);
    for (entry, block) in parsed.blocks.iter().enumerate() {
        let ranges = by_entry.get(entry).map(Vec::as_slice).unwrap_or(&[]);
        let Some(inlines) = inline_content(block) else {
            continue;
        };
        let hl = markup::Highlight {
            ranges: &ranges,
            active: active.as_ref(),
            hit_color: "#11223344",
            active_color: "#55667788",
        };
        let walk = markup::walk_inlines(inlines, &parsed, Some(hl));
        for segment in &walk.segments {
            assert!(
                markup::is_well_formed(&segment.markup),
                "highlight broke markup: {segment:?}"
            );
        }
    }
});

fn inline_content(block: &marksight::doc::Block) -> Option<&Vec<marksight::doc::Inline>> {
    match block {
        marksight::doc::Block::Heading { inlines, .. }
        | marksight::doc::Block::Paragraph(inlines) => Some(inlines),
        _ => None,
    }
}
