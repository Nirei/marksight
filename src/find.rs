//! Find layer: a flat per-block text index with case-insensitive search.
//!
//! The index is built from [`crate::markup::block_text`], so its character
//! offsets are exactly the offsets the markup walker understands. Entries are
//! the document's top-level blocks followed by each footnote's blocks.

use std::ops::Range;

use crate::doc::Document;
use crate::markup::block_text;

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
        let mut entries: Vec<String> = doc.blocks.iter().map(|b| block_text(b, doc)).collect();
        for footnote in &doc.footnotes {
            entries.extend(footnote.blocks.iter().map(|b| block_text(b, doc)));
        }
        TextIndex { entries }
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
    let needle: Vec<char> = needle.to_lowercase().chars().collect();
    let mut matches = Vec::new();
    for (entry, text) in index.entries.iter().enumerate() {
        let haystack: Vec<char> = text.to_lowercase().chars().collect();
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

pub fn entry_ranges(matches: &[Match], entry: usize) -> Vec<Range<usize>> {
    matches
        .iter()
        .filter(|m| m.entry == entry)
        .map(|m| m.start..m.end)
        .collect()
}

pub fn active_range(
    matches: &[Match],
    active: Option<usize>,
    entry: usize,
) -> Option<Range<usize>> {
    let active = active?;
    let m = matches.get(active)?;
    (m.entry == entry).then_some(m.start..m.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn index(markdown: &str) -> TextIndex {
        TextIndex::build(&parse(markdown))
    }

    #[test]
    fn index_covers_blocks_then_footnotes() {
        let idx = index("# head\n\npara\n\n> quote\n\ntext[^n]\n\n[^n]: note");
        assert_eq!(idx.len(), 5);
        assert_eq!(idx.entries[0], "head");
        assert_eq!(idx.entries[1], "para");
        assert_eq!(idx.entries[2], "quote");
        assert_eq!(idx.entries[3], "text1");
        assert_eq!(idx.entries[4], "note");
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
    fn entry_ranges_filter_by_entry() {
        let matches = find(&index("ab ab\n\ncd ab"), "ab");
        assert_eq!(entry_ranges(&matches, 0), vec![0..2, 3..5]);
        assert_eq!(entry_ranges(&matches, 1), vec![3..5]);
        assert_eq!(entry_ranges(&matches, 7), vec![]);
    }

    #[test]
    fn active_range_only_for_active_entry() {
        let matches = find(&index("ab\n\ncd ab"), "ab");
        assert_eq!(active_range(&matches, Some(1), 0), None);
        assert_eq!(active_range(&matches, Some(1), 1), Some(3..5));
        assert_eq!(active_range(&matches, None, 1), None);
        assert_eq!(active_range(&matches, Some(9), 1), None);
    }
}
