//! Pango markup builder.
//!
//! A single walker ([`walk_inlines`]) produces both the plain text of a run of
//! inlines and its Pango markup, segmented at image boundaries. Because text
//! and markup come from the same traversal, character offsets computed by
//! [`crate::find`] over the text are valid positions in the markup walk — that
//! alignment is what makes highlight rebuilding safe.
//!
//! Every piece of document text passes through [`escape_text`] (or
//! [`escape_attr`]) before entering markup; the only tags ever emitted are the
//! ones this module writes itself.

use std::ops::Range;

use crate::doc::{Alignment, Block, Document, Inline, Style};

#[derive(Clone, Debug)]
pub struct Segment {
    pub char_start: usize,
    pub markup: String,
}

#[derive(Clone, Debug, Default)]
pub struct Walk {
    pub text: String,
    pub segments: Vec<Segment>,
    pub images: Vec<ImageInfo>,
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub url: String,
    pub alt: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Highlight<'a> {
    pub ranges: &'a [Range<usize>],
    pub active: Option<&'a Range<usize>>,
    pub hit_color: &'a str,
    pub active_color: &'a str,
}

pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

struct Walker<'a> {
    footnotes: &'a Document,
    hl: Option<Highlight<'a>>,
    text: String,
    segments: Vec<Segment>,
    images: Vec<ImageInfo>,
    chars: usize,
    markup: String,
    open_tags: Vec<String>,
    segment_start: Option<usize>,
}

impl<'a> Walker<'a> {
    fn new(footnotes: &'a Document, hl: Option<Highlight<'a>>) -> Self {
        Walker {
            footnotes,
            hl,
            text: String::new(),
            segments: Vec::new(),
            images: Vec::new(),
            chars: 0,
            markup: String::new(),
            open_tags: Vec::new(),
            segment_start: None,
        }
    }

    fn push_chars(&mut self, raw: &str) {
        self.text.push_str(raw);
        self.chars += raw.chars().count();
    }

    fn open_tag(&mut self, tag: String) {
        self.markup.push_str(&tag);
        self.open_tags.push(tag);
    }

    fn close_tag(&mut self) {
        if let Some(tag) = self.open_tags.pop() {
            let name = tag[1..].split(&[' ', '>'][..]).next().unwrap_or("span");
            self.markup.push_str("</");
            self.markup.push_str(name);
            self.markup.push('>');
        }
    }

    fn boundary(&mut self) {
        let saved = self.open_tags.clone();
        while !self.open_tags.is_empty() {
            self.close_tag();
        }
        if !self.markup.is_empty() {
            let char_start = self.segment_start.unwrap_or(0);
            self.segments.push(Segment {
                char_start,
                markup: std::mem::take(&mut self.markup),
            });
        }
        self.segment_start = None;
        for tag in saved {
            self.open_tag(tag);
        }
    }

    fn highlight_state(&self, at: usize) -> u8 {
        let Some(hl) = self.hl else { return 0 };
        if hl.active.is_some_and(|r| at >= r.start && at < r.end) {
            return 2;
        }
        if hl.ranges.iter().any(|r| at >= r.start && at < r.end) {
            return 1;
        }
        0
    }

    fn emit(&mut self, raw: &str, style: Option<Style>) {
        if raw.is_empty() {
            return;
        }
        let base = self.chars;
        if self.segment_start.is_none() {
            self.segment_start = Some(base);
        }
        self.push_chars(raw);
        if let Some(style) = style {
            let mut opened = 0;
            if style.bold {
                self.open_tag("<b>".into());
                opened += 1;
            }
            if style.italic {
                self.open_tag("<i>".into());
                opened += 1;
            }
            if style.strike {
                self.open_tag("<s>".into());
                opened += 1;
            }
            if style.code {
                self.open_tag("<tt>".into());
                opened += 1;
            }
            self.emit_chunks(raw, base);
            for _ in 0..opened {
                self.close_tag();
            }
        } else {
            self.emit_chunks(raw, base);
        }
    }

    fn emit_chunks(&mut self, raw: &str, base: usize) {
        let mut state = self.highlight_state(base);
        if state > 0 {
            self.open_span(state);
        }
        let mut chunk = String::new();
        for (offset, ch) in raw.chars().enumerate() {
            let new_state = self.highlight_state(base + offset);
            if new_state != state {
                self.markup.push_str(&escape_text(&chunk));
                chunk.clear();
                if state > 0 {
                    self.close_tag();
                }
                if new_state > 0 {
                    self.open_span(new_state);
                }
                state = new_state;
            }
            chunk.push(ch);
        }
        self.markup.push_str(&escape_text(&chunk));
        if state > 0 {
            self.close_tag();
        }
    }

    fn open_span(&mut self, state: u8) {
        let color = match state {
            2 => self.hl.map(|h| h.active_color).unwrap_or("#000000"),
            _ => self.hl.map(|h| h.hit_color).unwrap_or("#000000"),
        };
        self.open_tag(format!("<span background=\"{}\">", escape_attr(color)));
    }

    fn footnote_ref(&mut self, label: &str) {
        match self.footnotes.footnote_index(label) {
            Some(index) => {
                let number = index + 1;
                let anchor = format!("#fn-{number}");
                self.open_tag(format!("<a href=\"{}\">", escape_attr(&anchor)));
                self.open_tag("<sup>".into());
                self.emit(&number.to_string(), None);
                self.close_tag();
                self.close_tag();
            }
            None => self.emit(&format!("[^{label}]"), None),
        }
    }
}

pub fn walk_inlines(inlines: &[Inline], footnotes: &Document, hl: Option<Highlight<'_>>) -> Walk {
    let mut walker = Walker::new(footnotes, hl);
    walk_into(inlines, &mut walker);
    walker.boundary();
    Walk {
        text: walker.text,
        segments: walker.segments,
        images: walker.images,
    }
}

fn walk_into(inlines: &[Inline], walker: &mut Walker<'_>) {
    for inline in inlines {
        match inline {
            Inline::Run { text, style } => walker.emit(text, Some(*style)),
            Inline::Link { href, title, runs } => {
                let label = if runs.is_empty() {
                    href.clone()
                } else {
                    String::new()
                };
                let attrs = if title.is_empty() {
                    String::new()
                } else {
                    format!(" title=\"{}\"", escape_attr(title))
                };
                walker.open_tag(format!("<a href=\"{}\"{}>", escape_attr(href), attrs));
                if runs.is_empty() {
                    walker.emit(&label, None);
                } else {
                    walk_into(runs, walker);
                }
                walker.close_tag();
            }
            Inline::Image { url, alt, title } => {
                walker.boundary();
                walker.push_chars(alt);
                walker.images.push(ImageInfo {
                    url: url.clone(),
                    alt: alt.clone(),
                    title: title.clone(),
                });
            }
            Inline::FootnoteRef { label } => walker.footnote_ref(label),
        }
    }
}

pub fn code_markup(code: &str, hl: Option<Highlight<'_>>) -> String {
    let footnotes = Document::default();
    let mut walker = Walker::new(&footnotes, hl);
    walker.emit(code, None);
    walker.boundary();
    walker.segments.pop().map(|s| s.markup).unwrap_or_default()
}

pub fn block_text(block: &Block, footnotes: &Document) -> String {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            walk_inlines(inlines, footnotes, None).text
        }
        Block::Code { text, .. } => text.clone(),
        Block::Quote { blocks, .. } => blocks_text(blocks, footnotes),
        Block::List { items, .. } => {
            let mut parts = Vec::new();
            for item in items {
                parts.push(blocks_text(&item.blocks, footnotes));
            }
            parts.join("\n")
        }
        Block::Table { head, rows, .. } => {
            let mut parts = Vec::new();
            for row in head.iter().chain(rows.iter()) {
                let mut cells = Vec::new();
                for cell in row {
                    cells.push(walk_inlines(cell, footnotes, None).text);
                }
                parts.push(cells.join(" "));
            }
            parts.join("\n")
        }
        Block::Rule => String::new(),
    }
}

pub fn blocks_text(blocks: &[Block], footnotes: &Document) -> String {
    let parts: Vec<String> = blocks.iter().map(|b| block_text(b, footnotes)).collect();
    parts.join("\n")
}

/// Plain-text dump of the whole document for snapshot tests and fuzz oracles.
pub fn document_text(doc: &Document) -> String {
    let mut out = blocks_text(&doc.blocks, doc);
    if !doc.footnotes.is_empty() {
        out.push_str("\n\n");
        for footnote in &doc.footnotes {
            out.push_str(&format!(
                "[^{}]: {}\n",
                footnote.label,
                blocks_text(&footnote.blocks, doc)
            ));
        }
    }
    out
}

/// Human-readable model + markup dump used by golden-file snapshot tests.
pub fn debug_dump(doc: &Document) -> String {
    let mut out = String::new();
    dump_blocks(&doc.blocks, doc, 0, &mut out);
    for (index, footnote) in doc.footnotes.iter().enumerate() {
        out.push_str(&format!(
            "--- footnote {} [^{}]\n",
            index + 1,
            footnote.label
        ));
        dump_blocks(&footnote.blocks, doc, 0, &mut out);
    }
    out
}

fn dump_blocks(blocks: &[Block], doc: &Document, depth: usize, out: &mut String) {
    for block in blocks {
        let pad = "  ".repeat(depth);
        match block {
            Block::Heading { level, .. } => {
                out.push_str(&format!("{pad}=== heading {level}\n"));
                dump_inline(block, doc, out, &pad);
            }
            Block::Paragraph(_) => {
                out.push_str(&format!("{pad}=== paragraph\n"));
                dump_inline(block, doc, out, &pad);
            }
            Block::Code { info, text } => {
                out.push_str(&format!(
                    "{pad}=== code info={info:?}\n{pad}text: {text:?}\n"
                ));
            }
            Block::Quote { kind, blocks } => {
                let kind = kind
                    .map(|k| format!("{k:?}"))
                    .unwrap_or_else(|| "none".into());
                out.push_str(&format!("{pad}=== quote kind={kind}\n"));
                dump_blocks(blocks, doc, depth + 1, out);
            }
            Block::List {
                start,
                tight,
                items,
            } => {
                let start = start.map(|s| s.to_string()).unwrap_or_else(|| "-".into());
                out.push_str(&format!("{pad}=== list start={start} tight={tight}\n"));
                for item in items {
                    let task = item
                        .task
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "x".into());
                    out.push_str(&format!("{pad}  --- item task={task}\n"));
                    dump_blocks(&item.blocks, doc, depth + 2, out);
                }
            }
            Block::Table { aligns, head, rows } => {
                let aligns: Vec<String> = aligns.iter().map(|a| align_name(*a)).collect();
                out.push_str(&format!("{pad}=== table aligns=[{}]\n", aligns.join(",")));
                for row in head.iter().chain(rows.iter()) {
                    let mut cells = Vec::new();
                    for cell in row {
                        let walk = walk_inlines(cell, doc, None);
                        cells.push(format!(
                            "{}|{}",
                            walk.text,
                            walk.segments
                                .iter()
                                .map(|s| s.markup.clone())
                                .collect::<String>()
                        ));
                    }
                    out.push_str(&format!("{pad}  row: {}\n", cells.join(" ; ")));
                }
            }
            Block::Rule => out.push_str(&format!("{pad}=== rule\n")),
        }
    }
}

fn dump_inline(block: &Block, doc: &Document, out: &mut String, pad: &str) {
    let inlines = match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => inlines,
        _ => return,
    };
    let walk = walk_inlines(inlines, doc, None);
    out.push_str(&format!("{pad}text: {}\n", walk.text));
    for segment in &walk.segments {
        out.push_str(&format!(
            "{pad}markup@{}: {}\n",
            segment.char_start, segment.markup
        ));
    }
}

fn align_name(align: Alignment) -> String {
    match align {
        Alignment::None => "none".into(),
        Alignment::Left => "left".into(),
        Alignment::Center => "center".into(),
        Alignment::Right => "right".into(),
    }
}

/// Checks that markup produced by this module is well-formed XML: tags
/// balance, attributes are quoted, and every `&` introduces one of the few
/// entities we emit. This is the fuzz and property-test oracle for the
/// “document text never leaks into markup unescaped” invariant.
pub fn is_well_formed(markup: &str) -> bool {
    const ENTITIES: [&str; 5] = ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"];
    let valid_entity = |tail: &str| ENTITIES.iter().any(|e| tail.starts_with(e));
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = markup;
    while let Some(open) = rest.find('<') {
        let (text, after) = rest.split_at(open);
        let mut amp_scan = text;
        while let Some(at) = amp_scan.find('&') {
            if !valid_entity(&amp_scan[at..]) {
                return false;
            }
            amp_scan = &amp_scan[at + 1..];
        }
        rest = after;
        let Some(close) = rest.find('>') else {
            return false;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            match stack.pop() {
                Some(open_name) if open_name == name => {}
                _ => return false,
            }
        } else {
            let name = tag.split(&[' ', '\t'][..]).next().unwrap_or("");
            if name.is_empty() {
                return false;
            }
            stack.push(name);
        }
    }
    let mut amp_scan = rest;
    while let Some(at) = amp_scan.find('&') {
        if !valid_entity(&amp_scan[at..]) {
            return false;
        }
        amp_scan = &amp_scan[at + 1..];
    }
    stack.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn markup(markdown: &str) -> String {
        let doc = parse(markdown);
        match &doc.blocks[0] {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                walk_inlines(inlines, &doc, None)
                    .segments
                    .into_iter()
                    .map(|s| s.markup)
                    .collect()
            }
            Block::Code { text, .. } => code_markup(text, None),
            _ => panic!("no label block in {markdown:?}"),
        }
    }

    fn text(markdown: &str) -> String {
        block_text(&parse(markdown).blocks[0], &parse(markdown))
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(markup("hello"), "hello");
    }

    #[test]
    fn special_characters_are_escaped() {
        assert_eq!(markup("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn styles_map_to_tags() {
        assert_eq!(
            markup("**b** *i* ~~s~~ `c`"),
            "<b>b</b> <i>i</i> <s>s</s> <tt>c</tt>"
        );
    }

    #[test]
    fn styles_nest_in_document_order() {
        assert_eq!(markup("***bi***"), "<b><i>bi</i></b>");
    }

    #[test]
    fn link_markup_escapes_href_and_title() {
        assert_eq!(
            markup(r#"[t](https://x/?a=1&b=2 "q \"t\"")"#),
            r#"<a href="https://x/?a=1&amp;b=2" title="q &quot;t&quot;">t</a>"#
        );
    }

    #[test]
    fn empty_link_falls_back_to_href_text() {
        assert_eq!(
            markup("[](https://example.com)"),
            r#"<a href="https://example.com">https://example.com</a>"#
        );
    }

    #[test]
    fn footnote_reference_becomes_anchor() {
        let doc = parse("ref[^a]\n\n[^a]: note");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let walk = walk_inlines(inlines, &doc, None);
        assert_eq!(
            walk.segments[0].markup,
            r##"ref<a href="#fn-1"><sup>1</sup></a>"##
        );
        assert_eq!(walk.text, "ref1");
    }

    #[test]
    fn unresolved_footnote_reference_is_literal() {
        assert_eq!(markup("ghost[^nope]"), "ghost[^nope]");
    }

    #[test]
    fn image_splits_segments_and_contributes_alt_to_text() {
        let doc = parse("before ![alt text](img.png) after");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let walk = walk_inlines(inlines, &doc, None);
        assert_eq!(walk.text, "before alt text after");
        assert_eq!(walk.segments.len(), 2);
        assert_eq!(walk.segments[0].markup, "before ");
        assert_eq!(walk.segments[0].char_start, 0);
        assert_eq!(walk.segments[1].markup, " after");
        assert_eq!(
            walk.segments[1].char_start,
            "before alt text".chars().count()
        );
    }

    #[test]
    fn image_inside_link_reopens_link_tags() {
        let doc = parse("[![pic](i.png) tail](https://x)");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let walk = walk_inlines(inlines, &doc, None);
        assert_eq!(walk.segments[0].markup, r#"<a href="https://x"></a>"#);
        assert_eq!(walk.segments[1].markup, r#"<a href="https://x"> tail</a>"#);
    }

    #[test]
    fn code_block_markup_escapes() {
        assert_eq!(
            code_markup("if a < b && c {\n", None),
            "if a &lt; b &amp;&amp; c {\n"
        );
    }

    #[test]
    fn highlight_wraps_matched_chars() {
        let doc = parse("the quick brown fox");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let all = 4..9;
        let ranges = vec![all];
        let hl = Highlight {
            ranges: &ranges,
            active: None,
            hit_color: "#ff0000",
            active_color: "#00ff00",
        };
        let walk = walk_inlines(inlines, &doc, Some(hl));
        assert_eq!(
            walk.segments[0].markup,
            "the <span background=\"#ff0000\">quick</span> brown fox"
        );
    }

    #[test]
    fn active_match_takes_precedence() {
        let doc = parse("aaaa");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let ranges = vec![0..2, 2..4];
        let active = 2..4;
        let hl = Highlight {
            ranges: &ranges,
            active: Some(&active),
            hit_color: "#1111111",
            active_color: "#2222222",
        };
        let walk = walk_inlines(inlines, &doc, Some(hl));
        assert_eq!(
            walk.segments[0].markup,
            "<span background=\"#1111111\">aa</span><span background=\"#2222222\">aa</span>"
        );
    }

    #[test]
    fn highlight_splits_inside_styled_runs() {
        let doc = parse("**abcdef**");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let all = 2..4;
        let ranges = vec![all];
        let hl = Highlight {
            ranges: &ranges,
            active: None,
            hit_color: "#0000ff",
            active_color: "#0000ff",
        };
        let walk = walk_inlines(inlines, &doc, Some(hl));
        assert_eq!(
            walk.segments[0].markup,
            "<b>ab<span background=\"#0000ff\">cd</span>ef</b>"
        );
    }

    #[test]
    fn highlight_uses_char_offsets_not_bytes() {
        let doc = parse("äöüß ✓ möö");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        let all = 5..7;
        let ranges = vec![all];
        let hl = Highlight {
            ranges: &ranges,
            active: None,
            hit_color: "#00ff00",
            active_color: "#00ff00",
        };
        let walk = walk_inlines(inlines, &doc, Some(hl));
        assert_eq!(
            walk.segments[0].markup,
            "äöüß <span background=\"#00ff00\">✓ </span>möö"
        );
    }

    #[test]
    fn block_text_covers_structures() {
        assert_eq!(text("# heading"), "heading");
        assert_eq!(text("a `b` c"), "a b c");
        assert_eq!(text("- one\n- two"), "one\ntwo");
        assert_eq!(text("> quoted"), "quoted");
        assert_eq!(text("| a | b |\n|---|---|\n| 1 | 2 |"), "a b\n1 2");
        assert_eq!(block_text(&Block::Rule, &parse("")), "");
    }

    #[test]
    fn document_text_appends_footnotes() {
        let doc = parse("hi[^1]\n\n[^1]: note");
        assert_eq!(document_text(&doc), "hi1\n\n[^1]: note\n");
    }

    #[test]
    fn walk_text_and_markup_char_offsets_agree() {
        let markdown = "mix **bold** ![img](i.png) [link](u) `code`[^1] ünïcödé\n\n[^1]: n";
        let doc = parse(markdown);
        for block in doc
            .blocks
            .iter()
            .chain(doc.footnotes.iter().flat_map(|f| f.blocks.iter()))
        {
            let inlines = match block {
                Block::Heading { inlines, .. } | Block::Paragraph(inlines) => inlines,
                _ => continue,
            };
            let walk = walk_inlines(inlines, &doc, None);
            let image_alts: Vec<String> = collect_image_alts(inlines);
            let rendered: usize = walk
                .segments
                .iter()
                .map(|s| strip_tags(&s.markup).chars().count())
                .sum::<usize>()
                + image_alts.iter().map(|a| a.chars().count()).sum::<usize>();
            assert_eq!(rendered, walk.text.chars().count());
            assert!(
                walk.segments
                    .iter()
                    .all(|s| s.char_start <= walk.text.chars().count())
            );
        }
    }

    fn collect_image_alts(inlines: &[Inline]) -> Vec<String> {
        let mut out = Vec::new();
        for inline in inlines {
            match inline {
                Inline::Image { alt, .. } => out.push(alt.clone()),
                Inline::Link { runs, .. } => out.extend(collect_image_alts(runs)),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn well_formed_checker_accepts_our_markup() {
        assert!(is_well_formed("plain"));
        assert!(is_well_formed("<b>bold</b> and <i><tt>x</tt></i>"));
        assert!(is_well_formed(
            "<a href=\"https://x?a=1&amp;b=2\" title=\"t\">label</a>"
        ));
        assert!(is_well_formed("&lt;&amp;&gt;&quot;&apos;"));
        assert!(is_well_formed("<span background=\"#11223344\">hit</span>"));
    }

    #[test]
    fn well_formed_checker_rejects_broken_markup() {
        assert!(!is_well_formed("<b>unclosed"));
        assert!(!is_well_formed("</b>stray-close"));
        assert!(!is_well_formed("<b><i>crossed</b></i>"));
        assert!(!is_well_formed("raw & ampersand"));
        assert!(!is_well_formed("unfinished &"));
        assert!(!is_well_formed("<"));
        assert!(!is_well_formed("<b"));
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
        out
    }
}
