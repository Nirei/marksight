//! Find layer: a flat per-unit text index with case-insensitive search.
//!
//! The index is [`crate::markup::text_units`] — one entry per rendered
//! label, in render order (paragraphs, headings and code blocks anywhere in
//! the tree, one entry per table cell, footnote bodies last). The shipped
//! index and the index these tests exercise are built by the same function,
//! and its character offsets are exactly the offsets the markup walker
//! understands.

use std::ops::Range;

use crate::doc::Document;
use crate::markup::text_units;

#[derive(Clone, Debug, Default)]
pub struct TextIndex {
    pub entries: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub entry: usize,
    pub start: usize,
    pub end: usize,
}

impl TextIndex {
    pub fn build(doc: &Document) -> Self {
        TextIndex {
            entries: text_units(doc),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn find(index: &TextIndex, needle: &str) -> Vec<Match> {
    if needle.trim().is_empty() {
        return Vec::new();
    }
    // Lowercase one char at a time, keeping only the first char of any
    // expansion: 'İ' lowercases to two chars ("i" + combining dot above),
    // which makes the haystack longer than the entry and shifts every
    // later match off the original text. A 1:1 fold keeps Match offsets
    // valid indices into the original entry, which is what highlighting
    // consumes.
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let needle: Vec<char> = needle.chars().map(fold).collect();
    let mut matches = Vec::new();
    for (entry, text) in index.entries.iter().enumerate() {
        let haystack: Vec<char> = text.chars().map(fold).collect();
        let mut at = 0;
        while at + needle.len() <= haystack.len() {
            if haystack[at..at + needle.len()] == needle[..] {
                matches.push(Match {
                    entry,
                    start: at,
                    end: at + needle.len(),
                });
                at += needle.len();
            } else {
                at += 1;
            }
        }
    }
    matches
}

pub fn cycle(matches: &[Match], active: Option<usize>, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let last = matches.len() - 1;
    match active {
        None => Some(if forward { 0 } else { last }),
        Some(current) => Some(match (forward, current) {
            (true, at) if at >= last => 0,
            (true, at) => at + 1,
            (false, 0) => last,
            (false, at) => at - 1,
        }),
    }
}

/// Match ranges bucketed per entry, ready for
/// [`crate::render::apply_highlight`]. `find` returns matches sorted by
/// entry, so one pass buckets them all; each label group then reads its
/// slice by index instead of rescanning every match.
pub fn by_entry(matches: &[Match]) -> Vec<Vec<Range<usize>>> {
    let mut out: Vec<Vec<Range<usize>>> = Vec::new();
    for m in matches {
        if out.len() <= m.entry {
            out.resize_with(m.entry + 1, Vec::new);
        }
        out[m.entry].push(m.start..m.end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn index(markdown: &str) -> TextIndex {
        TextIndex::build(&parse(markdown))
    }

    #[test]
    fn index_covers_one_entry_per_rendered_label() {
        // One entry per rendered label, in render order: list items and
        // table cells are separate entries (they are separate labels),
        // while a quote's inner paragraph is its own entry too. Footnote
        // bodies follow the document blocks.
        let idx = index(
            "# head\n\n- one\n- two\n\n| a | needle |\n|---|---|\n| c | d |\n\n> quoted\n\ntext[^n]\n\n[^n]: note",
        );
        assert_eq!(
            idx.entries,
            vec![
                "head", "one", "two", "a", "needle", "c", "d", "quoted", "text1", "note"
            ]
        );
    }

    #[test]
    fn empty_document_yields_empty_index() {
        assert!(index("").is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let matches = find(&index("The THE the"), "the");
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn empty_and_whitespace_needles_match_nothing() {
        let idx = index("some text");
        assert!(find(&idx, "").is_empty());
        assert!(find(&idx, "   ").is_empty());
        assert!(find(&idx, "\t\n").is_empty());
    }

    #[test]
    fn matches_are_ordered_and_non_overlapping() {
        let matches = find(&index("aaaa"), "aa");
        assert_eq!(
            matches,
            vec![
                Match {
                    entry: 0,
                    start: 0,
                    end: 2
                },
                Match {
                    entry: 0,
                    start: 2,
                    end: 4
                }
            ]
        );
    }

    #[test]
    fn matches_span_entries_in_order() {
        let matches = find(&index("# Alpha\n\nbeta alpha\n\n> gamma"), "alpha");
        assert_eq!(matches.len(), 2);
        assert_eq!(
            matches[0],
            Match {
                entry: 0,
                start: 0,
                end: 5
            }
        );
        assert_eq!(
            matches[1],
            Match {
                entry: 1,
                start: 5,
                end: 10
            }
        );
    }

    #[test]
    fn unicode_offsets_are_chars() {
        let matches = find(&index("äöü ✓ möö"), "ö");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].start, 1);
        assert_eq!(matches[1].start, 7);
        assert_eq!(matches[2].start, 8);
    }

    #[test]
    fn unicode_case_folding() {
        let matches = find(&index("Straße STRASSE"), "straße");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, 0);
    }

    #[test]
    fn image_alt_text_is_not_searchable() {
        // Alt text is tooltip-only, never rendered: a needle inside it
        // must not be reported as a match no label can highlight.
        assert!(find(&index("![needle alt](img.png) tail"), "needle").is_empty());
        let matches = find(&index("visible needle ![needle alt](img.png)"), "needle");
        assert_eq!(matches.len(), 1, "only the visible occurrence counts");
        assert_eq!(matches[0].start, 8);
    }

    #[test]
    fn search_covers_table_and_list_text() {
        let idx = index("| needle |\n|---|\n| x |\n\n- needle\n- other");
        let matches = find(&idx, "needle");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn cycling_wraps_both_ways() {
        let matches = find(&index("x x x"), "x");
        assert_eq!(matches.len(), 3);
        assert_eq!(cycle(&matches, None, true), Some(0));
        assert_eq!(cycle(&matches, None, false), Some(2));
        assert_eq!(cycle(&matches, Some(0), true), Some(1));
        assert_eq!(cycle(&matches, Some(2), true), Some(0));
        assert_eq!(cycle(&matches, Some(0), false), Some(2));
        assert_eq!(cycle(&matches, Some(1), false), Some(0));
    }

    #[test]
    fn cycling_empty_matches_is_none() {
        assert_eq!(cycle(&[], None, true), None);
        assert_eq!(cycle(&[], Some(0), true), None);
    }

    #[test]
    fn cycling_single_match_stays() {
        let matches = find(&index("one hit"), "hit");
        assert_eq!(matches.len(), 1);
        assert_eq!(cycle(&matches, Some(0), true), Some(0));
        assert_eq!(cycle(&matches, Some(0), false), Some(0));
    }

    #[test]
    fn by_entry_buckets_matches_per_entry() {
        let matches = find(&index("ab ab\n\ncd ab"), "ab");
        assert_eq!(by_entry(&matches), vec![vec![0..2, 3..5], vec![3..5]]);
        // An entry with no matches, and no matches at all, read as empty.
        assert_eq!(by_entry(&matches).get(7), None);
        assert!(by_entry(&find(&index("x"), "zz")).is_empty());
    }

    #[test]
    fn fold_keeps_match_offsets_in_bounds() {
        // 'İ' lowercases to two chars ("i" + combining dot); the haystack
        // must stay index-aligned with the entry or matches after it run
        // out of bounds. Found by fuzz_find.
        let idx = index("İx");
        assert_eq!(idx.entries[0].chars().count(), 2);
        assert_eq!(
            find(&idx, "x"),
            vec![Match {
                entry: 0,
                start: 1,
                end: 2
            }]
        );
        assert_eq!(
            find(&idx, "i"),
            vec![Match {
                entry: 0,
                start: 0,
                end: 1
            }]
        );
        assert_eq!(
            find(&idx, "ix"),
            vec![Match {
                entry: 0,
                start: 0,
                end: 2
            }]
        );
    }

}
