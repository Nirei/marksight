//! Pure markdown document model.
//!
//! Translates a stream of [`pulldown_cmark`] events into a GTK-free tree of
//! [`Block`]s and [`Inline`]s. Everything in this module is plain data: it can
//! be unit-tested, property-tested, and fuzzed without a display server.
//!
//! Raw HTML is deliberately dropped (never modeled), so nothing downstream can
//! ever render it — the classic viewer XSS vector.

use pulldown_cmark::{CodeBlockKind, Options, Parser, Tag, TagEnd};

pub use pulldown_cmark::Alignment;
pub use pulldown_cmark::BlockQuoteKind;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

impl Style {
    fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn strike(mut self) -> Self {
        self.strike = true;
        self
    }

    fn code(self) -> Self {
        Style { code: true, ..self }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Inline {
    Run {
        text: String,
        style: Style,
    },
    Link {
        href: String,
        title: String,
        runs: Vec<Inline>,
    },
    Image {
        url: String,
        alt: String,
        title: String,
    },
    FootnoteRef {
        label: String,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub struct ListItem {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

pub type TableRow = Vec<Vec<Inline>>;

#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Heading {
        level: u8,
        id: Option<String>,
        inlines: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Code {
        info: String,
        text: String,
    },
    Quote {
        kind: Option<BlockQuoteKind>,
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        tight: bool,
        items: Vec<ListItem>,
    },
    Table {
        aligns: Vec<Alignment>,
        head: Vec<TableRow>,
        rows: Vec<TableRow>,
    },
    Rule,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Footnote {
    pub label: String,
    pub blocks: Vec<Block>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TocEntry {
    pub level: u8,
    pub path: Vec<usize>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub footnotes: Vec<Footnote>,
    pub headings: Vec<TocEntry>,
}

impl Document {
    /// Zero-based position of the footnote definition for `label`.
    pub fn footnote_index(&self, label: &str) -> Option<usize> {
        self.footnotes.iter().position(|f| f.label == label)
    }

    /// Follows a [`TocEntry::path`] to the block it points at.
    ///
    /// Each path element indexes one container level; a [`Block::List`]
    /// consumes two: the item, then the block inside the item.
    pub fn resolve(&self, path: &[usize]) -> Option<&Block> {
        let mut node = self.blocks.get(*path.first()?)?;
        let mut i = 1;
        while i < path.len() {
            node = match node {
                Block::Quote { blocks, .. } => blocks.get(*path.get(i)?)?,
                Block::List { items, .. } => {
                    let item = items.get(*path.get(i)?)?;
                    i += 1;
                    item.blocks.get(*path.get(i)?)?
                }
                _ => return None,
            };
            i += 1;
        }
        Some(node)
    }
}

pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES
}

pub fn parse(markdown: &str) -> Document {
    let mut builder = Builder::new();
    for event in Parser::new_ext(markdown, options()) {
        builder.event(event);
    }
    builder.finish()
}

/// GitHub-style heading slug: lowercase, punctuation and symbols dropped,
/// spaces to '-', with later duplicates suffixed -1, -2 (see
/// [`Builder::claim_slug`]). Letters of any script, digits, '_' and '-'
/// survive; a heading whose text yields nothing (punctuation only) gets
/// "section".
fn heading_slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for ch in text.trim().chars() {
        for lowered in ch.to_lowercase() {
            match lowered {
                ' ' => slug.push('-'),
                '-' | '_' => slug.push(lowered),
                c if c.is_alphanumeric() => slug.push(c),
                _ => {}
            }
        }
    }
    if slug.is_empty() {
        slug.push_str("section");
    }
    slug
}

/// Plain text of a heading's inlines for slug generation: runs and link
/// labels contribute their text, images their alt text. Footnote references
/// cannot be numbered yet (definitions may appear later in the document) and
/// contribute nothing.
fn heading_text(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Run { text, .. } => out.push_str(text),
            Inline::Link { href, runs, .. } => {
                if runs.is_empty() {
                    out.push_str(href);
                } else {
                    heading_text(runs, out);
                }
            }
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::FootnoteRef { .. } => {}
        }
    }
}

/// Deepest block-container nesting (nested blockquotes, lists, footnote
/// bodies) kept as real structure. Containers nested beyond this are
/// flattened into their parent: their blocks attach to the enclosing
/// container instead of creating another tree level. This bounds the
/// recursion depth of every downstream consumer (text extraction, TOC
/// resolution, widget rendering), none of which is written iteratively; a
/// document of `> ` repeated twenty thousand times must degrade to flat
/// text, not overflow the stack.
const MAX_CONTAINER_DEPTH: usize = 256;

enum Frame {
    Root,
    Quote {
        kind: Option<BlockQuoteKind>,
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        any_paragraph: bool,
        items: Vec<ListItem>,
    },
    Item {
        task: Option<bool>,
        explicit_paragraph: bool,
        blocks: Vec<Block>,
        pending_inlines: Vec<Inline>,
    },
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    Heading {
        level: u8,
        id: Option<String>,
        inlines: Vec<Inline>,
    },
    Code {
        info: String,
        text: String,
    },
    Cell {
        inlines: Vec<Inline>,
    },
    Link {
        href: String,
        title: String,
        runs: Vec<Inline>,
    },
    Image {
        url: String,
        title: String,
        alt: String,
    },
    Table {
        aligns: Vec<Alignment>,
        in_head: bool,
        head: Vec<TableRow>,
        rows: Vec<TableRow>,
    },
}

struct Builder {
    stack: Vec<Frame>,
    styles: Vec<Style>,
    path: Vec<usize>,
    blocks: Vec<Block>,
    footnotes: Vec<Footnote>,
    headings: Vec<TocEntry>,
    /// Anchor ids already claimed by earlier headings, explicit or
    /// generated: the first occurrence of an id wins, later ones dedupe.
    used_ids: std::collections::HashSet<String>,
    /// Container frames currently on the stack (quotes, lists, items,
    /// footnote bodies), counted for [`MAX_CONTAINER_DEPTH`].
    depth: usize,
    /// Container starts swallowed by the depth cap whose matching end
    /// events are still to come.
    suppressed: usize,
}

fn append_inline(inlines: &mut Vec<Inline>, inline: Inline) {
    if let Inline::Run { text, style } = inline {
        append_run(inlines, &text, style);
    } else {
        inlines.push(inline);
    }
}

fn append_run(inlines: &mut Vec<Inline>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(Inline::Run {
        text: last,
        style: last_style,
    }) = inlines.last_mut()
    {
        if *last_style == style {
            last.push_str(text);
            return;
        }
    }
    inlines.push(Inline::Run {
        text: text.to_owned(),
        style,
    });
}

impl Builder {
    fn new() -> Self {
        Builder {
            stack: vec![Frame::Root],
            styles: Vec::new(),
            path: Vec::new(),
            blocks: Vec::new(),
            footnotes: Vec::new(),
            headings: Vec::new(),
            used_ids: std::collections::HashSet::new(),
            depth: 0,
            suppressed: 0,
        }
    }

    /// Starts a container level. Returns false when the depth cap has been
    /// reached: the caller must not push a frame, and the matching end event
    /// will be swallowed by [`Builder::end_container`].
    fn begin_container(&mut self) -> bool {
        if self.depth >= MAX_CONTAINER_DEPTH {
            self.suppressed += 1;
            return false;
        }
        self.depth += 1;
        true
    }

    /// Ends a container level started by [`Builder::begin_container`].
    /// Returns false when this end belongs to a suppressed start.
    fn end_container(&mut self) -> bool {
        if self.suppressed > 0 {
            self.suppressed -= 1;
            return false;
        }
        self.depth -= 1;
        true
    }

    /// Claims `id` for a heading: the first occurrence of a duplicate
    /// explicit id wins, later ones get no anchor at all.
    fn claim_id(&mut self, id: String) -> Option<String> {
        if self.used_ids.insert(id.clone()) {
            Some(id)
        } else {
            None
        }
    }

    /// Claims the slug of `text`, appending GitHub-style -1, -2 suffixes
    /// until it is unique among all earlier anchors.
    fn claim_slug(&mut self, text: &str) -> String {
        let base = heading_slug(text);
        let mut candidate = base.clone();
        let mut suffix = 1;
        while !self.used_ids.insert(candidate.clone()) {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        candidate
    }

    fn current_style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_inline(&mut self, inline: Inline) {
        for frame in self.stack.iter_mut().rev() {
            match frame {
                Frame::Paragraph { inlines }
                | Frame::Heading { inlines, .. }
                | Frame::Cell { inlines }
                | Frame::Link { runs: inlines, .. } => {
                    append_inline(inlines, inline);
                    return;
                }
                Frame::Item {
                    pending_inlines, ..
                } => {
                    append_inline(pending_inlines, inline);
                    return;
                }
                _ => continue,
            }
        }
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let style = self.current_style();
        for frame in self.stack.iter_mut().rev() {
            match frame {
                Frame::Code { text: code, .. } => {
                    code.push_str(text);
                    return;
                }
                Frame::Image { alt, .. } => {
                    alt.push_str(text);
                    return;
                }
                Frame::Paragraph { inlines }
                | Frame::Heading { inlines, .. }
                | Frame::Cell { inlines }
                | Frame::Link { runs: inlines, .. } => {
                    append_run(inlines, text, style);
                    return;
                }
                Frame::Item {
                    pending_inlines, ..
                } => {
                    append_run(pending_inlines, text, style);
                    return;
                }
                _ => continue,
            }
        }
    }

    fn mark_parent_list_loose(&mut self) {
        for frame in self.stack.iter_mut().rev() {
            if let Frame::List { any_paragraph, .. } = frame {
                *any_paragraph = true;
                return;
            }
        }
    }

    fn set_item_task(&mut self, checked: bool) {
        for frame in self.stack.iter_mut().rev() {
            if let Frame::Item { task, .. } = frame {
                *task = Some(checked);
                return;
            }
        }
    }

    fn container_len(&self) -> usize {
        for frame in self.stack.iter().rev() {
            match frame {
                Frame::Root => return self.blocks.len(),
                Frame::Quote { blocks, .. } | Frame::Footnote { blocks, .. } => {
                    return blocks.len();
                }
                Frame::List { items, .. } => return items.len(),
                Frame::Item { blocks, .. } => return blocks.len(),
                _ => continue,
            }
        }
        0
    }

    fn flush_item_pending(&mut self) {
        for frame in self.stack.iter_mut().rev() {
            if let Frame::Item {
                blocks,
                pending_inlines,
                ..
            } = frame
            {
                if !pending_inlines.is_empty() {
                    let inlines = std::mem::take(pending_inlines);
                    blocks.push(Block::Paragraph(inlines));
                }
                return;
            }
        }
    }

    fn attach(&mut self, block: Block) {
        self.flush_item_pending();
        if let Block::Heading { level, .. } = &block {
            if !self
                .stack
                .iter()
                .any(|f| matches!(f, Frame::Footnote { .. }))
            {
                let mut path = self.path.clone();
                path.push(self.container_len());
                self.headings.push(TocEntry {
                    level: *level,
                    path,
                });
            }
        }
        for frame in self.stack.iter_mut().rev() {
            match frame {
                Frame::Root => {
                    self.blocks.push(block);
                    return;
                }
                Frame::Quote { blocks, .. } | Frame::Footnote { blocks, .. } => {
                    blocks.push(block);
                    return;
                }
                Frame::Item { blocks, .. } => {
                    blocks.push(block);
                    return;
                }
                _ => continue,
            }
        }
    }

    fn event(&mut self, event: pulldown_cmark::Event) {
        use pulldown_cmark::Event;

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => {
                for frame in self.stack.iter_mut().rev() {
                    match frame {
                        Frame::Image { alt, .. } => {
                            alt.push_str(&text);
                            return;
                        }
                        _ => continue,
                    }
                }
                let inline = Inline::Run {
                    text: text.into_string(),
                    style: self.current_style().code(),
                };
                self.push_inline(inline);
            }
            Event::FootnoteReference(label) => {
                self.push_inline(Inline::FootnoteRef {
                    label: label.into_string(),
                });
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::TaskListMarker(checked) => self.set_item_task(checked),
            Event::Rule => self.attach(Block::Rule),
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.stack.push(Frame::Paragraph {
                inlines: Vec::new(),
            }),
            Tag::Heading { level, id, .. } => self.stack.push(Frame::Heading {
                level: level as u8,
                id: id.map(|s| s.into_string()),
                inlines: Vec::new(),
            }),
            Tag::BlockQuote(kind) => {
                if !self.begin_container() {
                    return;
                }
                self.flush_item_pending();
                self.path.push(self.container_len());
                self.stack.push(Frame::Quote {
                    kind,
                    blocks: Vec::new(),
                });
            }
            Tag::CodeBlock(kind) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.stack.push(Frame::Code {
                    info,
                    text: String::new(),
                });
            }
            Tag::List(start) => {
                if !self.begin_container() {
                    return;
                }
                self.flush_item_pending();
                self.path.push(self.container_len());
                self.stack.push(Frame::List {
                    start,
                    any_paragraph: false,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                if !self.begin_container() {
                    return;
                }
                self.flush_item_pending();
                self.path.push(self.container_len());
                self.stack.push(Frame::Item {
                    task: None,
                    explicit_paragraph: false,
                    blocks: Vec::new(),
                    pending_inlines: Vec::new(),
                });
            }
            Tag::FootnoteDefinition(label) => {
                if !self.begin_container() {
                    return;
                }
                self.flush_item_pending();
                self.path.push(self.container_len());
                self.stack.push(Frame::Footnote {
                    label: label.into_string(),
                    blocks: Vec::new(),
                });
            }
            Tag::Table(aligns) => self.stack.push(Frame::Table {
                aligns,
                in_head: false,
                head: Vec::new(),
                rows: Vec::new(),
            }),
            Tag::TableHead => {
                if let Some(Frame::Table { in_head, head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                    head.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(Frame::Table {
                    in_head,
                    head,
                    rows,
                    ..
                }) = self.stack.last_mut()
                {
                    if *in_head {
                        head.push(Vec::new());
                    } else {
                        rows.push(Vec::new());
                    }
                }
            }
            Tag::TableCell => self.stack.push(Frame::Cell {
                inlines: Vec::new(),
            }),
            Tag::Link {
                dest_url, title, ..
            } => self.stack.push(Frame::Link {
                href: dest_url.into_string(),
                title: title.into_string(),
                runs: Vec::new(),
            }),
            Tag::Image {
                dest_url, title, ..
            } => self.stack.push(Frame::Image {
                url: dest_url.into_string(),
                title: title.into_string(),
                alt: String::new(),
            }),
            Tag::Emphasis => {
                let style = self.current_style().italic();
                self.styles.push(style);
            }
            Tag::Strong => {
                let style = self.current_style().bold();
                self.styles.push(style);
            }
            Tag::Strikethrough => {
                let style = self.current_style().strike();
                self.styles.push(style);
            }
            _ => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => {
                if let Some(Frame::Paragraph { inlines }) = self.stack.pop() {
                    if let Some(Frame::Item {
                        explicit_paragraph, ..
                    }) = self.stack.last_mut()
                    {
                        *explicit_paragraph = true;
                    }
                    self.attach(Block::Paragraph(inlines));
                }
            }
            TagEnd::Heading(_) => {
                if let Some(Frame::Heading { level, id, inlines }) = self.stack.pop() {
                    // Every heading gets an anchor: an explicit {#id} when
                    // its id is still free (the first duplicate wins, later
                    // ones drop to no anchor), otherwise a GitHub-style slug
                    // deduplicated with -1, -2 suffixes.
                    let id = match id {
                        Some(explicit) => self.claim_id(explicit),
                        None => {
                            let mut text = String::new();
                            heading_text(&inlines, &mut text);
                            Some(self.claim_slug(&text))
                        }
                    };
                    self.attach(Block::Heading { level, id, inlines });
                }
            }
            TagEnd::BlockQuote(_) => {
                if !self.end_container() {
                    return;
                }
                self.path.pop();
                if let Some(Frame::Quote { kind, blocks }) = self.stack.pop() {
                    self.attach(Block::Quote { kind, blocks });
                }
            }
            TagEnd::CodeBlock => {
                if let Some(Frame::Code { info, text }) = self.stack.pop() {
                    self.attach(Block::Code { info, text });
                }
            }
            TagEnd::List(ordered) => {
                if !self.end_container() {
                    return;
                }
                self.path.pop();
                if let Some(Frame::List {
                    start,
                    any_paragraph,
                    items,
                }) = self.stack.pop()
                {
                    self.attach(Block::List {
                        start: if ordered { start } else { None },
                        tight: !any_paragraph,
                        items,
                    });
                }
            }
            TagEnd::Item => {
                if !self.end_container() {
                    return;
                }
                self.path.pop();
                let frame = self.stack.pop();
                if let Some(Frame::Item {
                    task,
                    explicit_paragraph,
                    mut blocks,
                    pending_inlines,
                }) = frame
                {
                    if !pending_inlines.is_empty() {
                        blocks.push(Block::Paragraph(pending_inlines));
                    }
                    if explicit_paragraph {
                        self.mark_parent_list_loose();
                    }
                    for open in self.stack.iter_mut().rev() {
                        if let Frame::List { items, .. } = open {
                            items.push(ListItem { task, blocks });
                            return;
                        }
                    }
                }
            }
            TagEnd::FootnoteDefinition => {
                if !self.end_container() {
                    return;
                }
                self.path.pop();
                if let Some(Frame::Footnote { label, blocks }) = self.stack.pop() {
                    self.footnotes.push(Footnote { label, blocks });
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table {
                    aligns, head, rows, ..
                }) = self.stack.pop()
                {
                    self.attach(Block::Table { aligns, head, rows });
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = false;
                }
            }
            TagEnd::TableCell => {
                if let Some(Frame::Cell { inlines }) = self.stack.pop() {
                    for frame in self.stack.iter_mut().rev() {
                        if let Frame::Table {
                            in_head,
                            head,
                            rows,
                            ..
                        } = frame
                        {
                            let target = if *in_head { head } else { rows };
                            if let Some(row) = target.last_mut() {
                                row.push(inlines);
                            }
                            return;
                        }
                    }
                }
            }
            TagEnd::Link => {
                if let Some(Frame::Link { href, title, runs }) = self.stack.pop() {
                    self.push_inline(Inline::Link { href, title, runs });
                }
            }
            TagEnd::Image => {
                if let Some(Frame::Image { url, alt, title }) = self.stack.pop() {
                    self.push_inline(Inline::Image { url, alt, title });
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            _ => {}
        }
    }

    fn finish(self) -> Document {
        Document {
            blocks: self.blocks,
            footnotes: self.footnotes,
            headings: self.headings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::BlockQuoteKind;

    fn run(text: &str) -> Inline {
        Inline::Run {
            text: text.into(),
            style: Style::default(),
        }
    }

    fn styled(text: &str, style: Style) -> Inline {
        Inline::Run {
            text: text.into(),
            style,
        }
    }

    fn para(text: &str) -> Block {
        Block::Paragraph(vec![run(text)])
    }

    fn heading(level: u8, text: &str) -> Block {
        Block::Heading {
            level,
            id: Some(heading_slug(text)),
            inlines: vec![run(text)],
        }
    }

    fn code(text: &str) -> Block {
        Block::Code {
            info: String::new(),
            text: text.into(),
        }
    }

    fn quote(blocks: Vec<Block>) -> Block {
        Block::Quote { kind: None, blocks }
    }

    fn bullet(items: Vec<Vec<Block>>) -> Block {
        Block::List {
            start: None,
            tight: true,
            items: items
                .into_iter()
                .map(|blocks| ListItem { task: None, blocks })
                .collect(),
        }
    }

    fn assert_doc(markdown: &str, blocks: Vec<Block>) -> Document {
        let doc = parse(markdown);
        assert_eq!(doc.blocks, blocks, "markdown: {markdown:?}");
        doc
    }

    #[test]
    fn empty_document() {
        assert_doc("", vec![]);
        assert_doc("   \n\n  \t \n", vec![]);
    }

    #[test]
    fn simple_paragraph() {
        assert_doc("hello world", vec![para("hello world")]);
    }

    #[test]
    fn paragraphs_merge_source_lines() {
        assert_doc("one\ntwo\nthree", vec![para("one two three")]);
    }

    #[test]
    fn headings_levels_and_toc() {
        let doc = assert_doc(
            "# top\n\n## mid\n\n### deep\n\n# again",
            vec![
                heading(1, "top"),
                heading(2, "mid"),
                heading(3, "deep"),
                heading(1, "again"),
            ],
        );
        let levels: Vec<u8> = doc.headings.iter().map(|h| h.level).collect();
        assert_eq!(levels, vec![1, 2, 3, 1]);
        let paths: Vec<Vec<usize>> = doc.headings.iter().map(|h| h.path.clone()).collect();
        assert_eq!(paths, vec![vec![0], vec![1], vec![2], vec![3]]);
        for entry in &doc.headings {
            assert!(matches!(
                doc.resolve(&entry.path),
                Some(Block::Heading { .. })
            ));
        }
    }

    #[test]
    fn setext_headings() {
        assert_doc("title\n=====", vec![heading(1, "title")]);
        assert_doc("sub\n---", vec![heading(2, "sub")]);
    }

    #[test]
    fn heading_attributes_id() {
        let doc = parse("# custom { #my-id }");
        let Block::Heading { id, .. } = &doc.blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(id.as_deref(), Some("my-id"));
    }

    #[test]
    fn heading_slug_rules() {
        assert_eq!(heading_slug("Hello World"), "hello-world");
        assert_eq!(heading_slug("Hello, World!"), "hello-world");
        assert_eq!(heading_slug("C++ & Rust?"), "c--rust");
        assert_eq!(heading_slug("  padded  "), "padded");
        assert_eq!(
            heading_slug("keep_under-scored-dash"),
            "keep_under-scored-dash"
        );
        assert_eq!(heading_slug("Ünïcödé Tëst"), "ünïcödé-tëst");
        assert_eq!(heading_slug("中文标题"), "中文标题");
        assert_eq!(heading_slug("!!!"), "section");
        assert_eq!(heading_slug(""), "section");
    }

    #[test]
    fn headings_without_id_get_slug_anchors() {
        let doc = parse("# Hello, World!\n\ntext");
        let Block::Heading { id, .. } = &doc.blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(id.as_deref(), Some("hello-world"));
    }

    #[test]
    fn duplicate_headings_dedupe_with_suffixes() {
        let doc = parse("# a\n\n# a\n\n# a");
        let ids: Vec<&str> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::Heading { id, .. } => id.as_deref().unwrap(),
                _ => "",
            })
            .collect();
        assert_eq!(ids, vec!["a", "a-1", "a-2"]);
    }

    #[test]
    fn explicit_id_wins_over_slug() {
        // The explicit id is used verbatim; the slug "hello-world" is not
        // generated for a heading that carries its own id.
        let doc = parse("# Hello, World! {#explicit}");
        let Block::Heading { id, .. } = &doc.blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(id.as_deref(), Some("explicit"));
    }

    #[test]
    fn explicit_and_generated_ids_share_one_namespace() {
        // A slug colliding with an explicit id dedupes...
        let doc = parse("# Foo {#foo}\n\n# Foo");
        let ids: Vec<Option<&str>> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::Heading { id, .. } => id.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![Some("foo"), Some("foo-1")]);
        // ...and a duplicate explicit id keeps only its first occurrence.
        let doc = parse("# one {#dup}\n\n# two {#dup}");
        let ids: Vec<Option<&str>> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::Heading { id, .. } => id.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![Some("dup"), None]);
    }

    #[test]
    fn heading_slug_uses_inline_text_link_and_alt() {
        let doc = parse("# a [linked *word*](https://x) and ![alt text](i.png)");
        let Block::Heading { id, .. } = &doc.blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(id.as_deref(), Some("a-linked-word-and-alt-text"));
    }

    #[test]
    fn heading_in_quote_toc_path_resolves() {
        let doc = parse("> ## quoted\n\ntext");
        assert_eq!(doc.blocks.len(), 2);
        assert_eq!(doc.headings.len(), 1);
        let entry = &doc.headings[0];
        assert_eq!(entry.level, 2);
        assert_eq!(entry.path, vec![0, 0]);
        assert!(matches!(
            doc.resolve(&entry.path),
            Some(Block::Heading { .. })
        ));
    }

    #[test]
    fn heading_in_list_item_toc_path_resolves() {
        let doc = parse("- item one\n- item two\n  - nested with heading\n    #### deep\n");
        assert_eq!(doc.headings.len(), 1);
        let entry = &doc.headings[0];
        assert!(matches!(
            doc.resolve(&entry.path),
            Some(Block::Heading { level: 4, .. })
        ));
    }

    #[test]
    fn heading_in_footnote_not_in_toc() {
        let doc = parse("text[^1]\n\n[^1]: note\n\n#### hidden");
        assert_eq!(doc.headings.len(), 1);
        assert_eq!(doc.headings[0].path, vec![1]);
    }

    #[test]
    fn inline_styles_nest() {
        let doc = parse("**bold** and *italic* and ~~gone~~ and `mono`");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        let italic = Style {
            italic: true,
            ..Style::default()
        };
        let strike = Style {
            strike: true,
            ..Style::default()
        };
        let mono = Style {
            code: true,
            ..Style::default()
        };
        assert_eq!(
            inlines,
            &vec![
                styled("bold", bold),
                run(" and "),
                styled("italic", italic),
                run(" and "),
                styled("gone", strike),
                run(" and "),
                styled("mono", mono),
            ]
        );
    }

    #[test]
    fn inline_styles_combine() {
        let doc = parse("**bold with *nested italics* tail**");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        let both = Style {
            bold: true,
            italic: true,
            ..Style::default()
        };
        assert_eq!(
            inlines,
            &vec![
                styled("bold with ", bold),
                styled("nested italics", both),
                styled(" tail", bold)
            ]
        );
    }

    #[test]
    fn code_span_inside_emphasis_keeps_code_style() {
        let doc = parse("**bold `mono` tail**");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let bold_mono = Style {
            bold: true,
            code: true,
            ..Style::default()
        };
        assert!(inlines.iter().any(|i| *i == styled("mono", bold_mono)));
    }

    #[test]
    fn links_carry_title_and_runs() {
        let doc = parse("[label](https://example.com \"the title\")");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            inlines,
            &vec![Inline::Link {
                href: "https://example.com".into(),
                title: "the title".into(),
                runs: vec![run("label")],
            }]
        );
    }

    #[test]
    fn links_contain_styled_runs() {
        let doc = parse("[**bold** label](https://example.com)");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let Inline::Link { runs, .. } = &inlines[0] else {
            panic!("expected link");
        };
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        assert_eq!(runs, &vec![styled("bold", bold), run(" label")]);
    }

    #[test]
    fn reference_links_resolve() {
        let doc = parse("[label][ref]\n\n[ref]: https://example.com \"title\"");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            inlines,
            &vec![Inline::Link {
                href: "https://example.com".into(),
                title: "title".into(),
                runs: vec![run("label")],
            }]
        );
    }

    #[test]
    fn link_wrapping_emphasis() {
        let doc = parse("**[label](https://example.com)**");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        assert_eq!(
            inlines,
            &vec![Inline::Link {
                href: "https://example.com".into(),
                title: String::new(),
                runs: vec![styled("label", bold)],
            }]
        );
    }

    #[test]
    fn image_alt_flattens_to_plain_text() {
        let doc = parse("![alt *emphasis* `code`](img.png \"pic\")");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            inlines,
            &vec![Inline::Image {
                url: "img.png".into(),
                alt: "alt emphasis code".into(),
                title: "pic".into(),
            }]
        );
    }

    #[test]
    fn fenced_code_with_info() {
        let doc = parse("```rust\nfn main() {}\n```");
        assert_eq!(
            doc.blocks,
            vec![Block::Code {
                info: "rust".into(),
                text: "fn main() {}\n".into()
            }]
        );
    }

    #[test]
    fn indented_code() {
        let doc = parse("    indented code\n    second line");
        assert_eq!(doc.blocks, vec![code("indented code\nsecond line")]);
    }

    #[test]
    fn unclosed_fence_runs_to_eof() {
        let doc = parse("```\nnever closed");
        assert_eq!(doc.blocks, vec![code("never closed")]);
    }

    #[test]
    fn ordered_list_start_value() {
        let doc = parse("3. three\n4. four");
        let Block::List {
            start,
            tight,
            items,
        } = &doc.blocks[0]
        else {
            panic!("expected list");
        };
        assert_eq!(*start, Some(3));
        assert!(*tight);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks, vec![para("three")]);
        assert_eq!(items[1].blocks, vec![para("four")]);
    }

    #[test]
    fn tight_list_has_no_spacing_class() {
        let doc = parse("- a\n- b");
        let Block::List { tight, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        assert!(*tight);
    }

    #[test]
    fn loose_list_detected() {
        let doc = parse("- a\n\n- b");
        let Block::List { tight, items, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        assert!(!*tight);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks, vec![para("a")]);
    }

    #[test]
    fn task_list_markers() {
        let doc = parse("- [ ] todo\n- [x] done");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items[0].task, Some(false));
        assert_eq!(items[1].task, Some(true));
    }

    #[test]
    fn nested_lists() {
        let doc = parse("- outer\n  - inner\n- outer two");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].blocks,
            vec![para("outer"), bullet(vec![vec![para("inner")]])]
        );
        assert_eq!(items[1].blocks, vec![para("outer two")]);
    }

    #[test]
    fn list_item_with_code_block() {
        let doc = parse("- item\n\n  ```\n  code\n  ```\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items[0].blocks, vec![para("item"), code("code\n")]);
    }

    #[test]
    fn blockquote_plain_and_nested() {
        let doc = parse("> outer\n> > inner");
        assert_eq!(
            doc.blocks,
            vec![quote(vec![para("outer"), quote(vec![para("inner")])])]
        );
    }

    #[test]
    fn blockquote_lazy_continuation_merges_lines() {
        assert_doc("> one\n> two", vec![quote(vec![para("one two")])]);
    }

    #[test]
    fn gfm_alerts() {
        for (syntax, kind) in [
            ("> [!NOTE]\n> text", BlockQuoteKind::Note),
            ("> [!TIP]\n> text", BlockQuoteKind::Tip),
            ("> [!IMPORTANT]\n> text", BlockQuoteKind::Important),
            ("> [!WARNING]\n> text", BlockQuoteKind::Warning),
            ("> [!CAUTION]\n> text", BlockQuoteKind::Caution),
        ] {
            let doc = parse(syntax);
            assert_eq!(
                doc.blocks,
                vec![Block::Quote {
                    kind: Some(kind),
                    blocks: vec![para("text")]
                }],
                "{syntax}"
            );
        }
    }

    #[test]
    fn table_alignments_and_rows() {
        let doc = parse(
            "| left | center | right | default |\n|:-----|:------:|------:|---------|\n| a | b | c | d |\n| e | f | g | h |",
        );
        let Block::Table { aligns, head, rows } = &doc.blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(
            aligns,
            &vec![
                Alignment::Left,
                Alignment::Center,
                Alignment::Right,
                Alignment::None
            ]
        );
        assert_eq!(head.len(), 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(head[0].len(), 4);
        assert_eq!(rows[1][2], vec![run("g")]);
    }

    #[test]
    fn table_head_only() {
        let doc = parse("| a | b |\n|---|---|");
        let Block::Table { head, rows, .. } = &doc.blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(head.len(), 1);
        assert!(rows.is_empty());
    }

    #[test]
    fn footnotes_reference_and_definition() {
        let doc = parse("before[^note] after\n\n[^note]: the note text");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            inlines,
            &vec![
                run("before"),
                Inline::FootnoteRef {
                    label: "note".into()
                },
                run(" after")
            ]
        );
        assert_eq!(doc.footnotes.len(), 1);
        assert_eq!(doc.footnotes[0].label, "note");
        assert_eq!(doc.footnotes[0].blocks, vec![para("the note text")]);
        assert_eq!(doc.footnote_index("note"), Some(0));
    }

    #[test]
    fn footnote_reference_without_definition_stays_literal() {
        let doc = parse("dangling[^ghost]");
        assert_eq!(doc.blocks, vec![para("dangling[^ghost]")]);
        assert_eq!(doc.footnote_index("ghost"), None);
    }

    #[test]
    fn multiple_footnotes_keep_source_order() {
        let doc = parse("a[^2] b[^1]\n\n[^2]: second\n\n[^1]: first");
        assert_eq!(doc.footnote_index("2"), Some(0));
        assert_eq!(doc.footnote_index("1"), Some(1));
    }

    #[test]
    fn raw_html_is_dropped() {
        let doc = parse("see <b>bold</b> and <img src=x> now");
        assert_eq!(doc.blocks, vec![para("see bold and  now")]);
    }

    #[test]
    fn html_block_entirely_removed() {
        let doc = parse("<div>\n  <p>contained</p>\n</div>\n\nreal text");
        assert_eq!(doc.blocks, vec![para("real text")]);
    }

    #[test]
    fn soft_break_becomes_space_hard_break_becomes_newline() {
        let doc = parse("soft\nbreak and hard\\\nbreak");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(inlines, &vec![run("soft break and hard\nbreak")]);
    }

    #[test]
    fn thematic_rule() {
        assert_doc("---\n\ntext", vec![Block::Rule, para("text")]);
        assert_doc(
            "before\n\n***\n\nafter",
            vec![para("before"), Block::Rule, para("after")],
        );
    }

    #[test]
    fn nesting_beyond_cap_flattens_without_panicking() {
        // A 40 KB file of repeated `> ` used to overflow the stack in the
        // recursive text/render walkers. The builder must cap the tree and
        // flatten the rest into the deepest kept container.
        let depth = 20_000;
        let markdown = "> ".repeat(depth) + "bottom";
        let doc = parse(&markdown);
        let mut deepest = 0;
        let mut node = doc.blocks.first();
        while let Some(Block::Quote { blocks, .. }) = node {
            deepest += 1;
            node = blocks.first();
        }
        assert_eq!(deepest, 256, "nesting must stop at the cap");
        assert!(
            matches!(node, Some(Block::Paragraph(inlines)) if inlines == &vec![run("bottom")]),
            "flattened content must survive, got {node:?}"
        );
    }

    #[test]
    fn nesting_beyond_cap_keeps_toc_consistent() {
        // Headings inside the flattened region still resolve: their TOC
        // paths are computed against the capped tree.
        let mut markdown = String::new();
        for _ in 0..300 {
            markdown.push_str("> ");
        }
        markdown.push_str("## deep heading\n\nvisible text");
        let doc = parse(&markdown);
        assert_eq!(doc.headings.len(), 1);
        let entry = &doc.headings[0];
        assert!(matches!(
            doc.resolve(&entry.path),
            Some(Block::Heading { level: 2, .. })
        ));
    }

    #[test]
    fn deep_nesting_does_not_panic() {
        let depth = 200;
        let markdown = "> ".repeat(depth) + "bottom";
        let doc = parse(&markdown);
        let mut node = doc.blocks.first();
        for _ in 0..depth - 1 {
            node = match node {
                Some(Block::Quote { blocks, .. }) => blocks.first(),
                _ => panic!("expected nested quotes"),
            };
        }
        assert!(
            matches!(node, Some(Block::Quote { blocks, .. }) if blocks == &vec![para("bottom")])
        );
    }

    #[test]
    fn entities_and_specials_survive_as_text() {
        let doc = parse("a < b & c > d \"quoted\"");
        assert_eq!(doc.blocks, vec![para("a < b & c > d \"quoted\"")]);
    }
}
