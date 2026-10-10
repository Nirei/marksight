//! Model → widget tree: a thin GTK layer over the pure core.
//!
//! Every text-bearing label is registered in a [`LabelGroup`] keyed by its
//! [`crate::find::TextIndex`] entry: 0, so the find layer can rebuild exactly the
//! labels whose markup changes — nothing else is touched. Heading widgets are
//! registered per TOC entry; footnote widgets are registered by anchor.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::pango;
use gtk::prelude::*;
use gtk::{self as gtk};

use crate::doc::{Alignment, Block, BlockQuoteKind, Document, Inline};
use crate::markup::{self, Highlight};

pub type LinkHandler = Rc<dyn Fn(&str) -> bool>;

pub struct RenderCtx<'a> {
    pub doc: &'a Document,
    pub base_dir: Option<&'a Path>,
    pub link_handler: LinkHandler,
    pub anchors: Rc<RefCell<HashMap<String, gtk::Widget>>>,
    pub toc_targets: Rc<RefCell<Vec<gtk::Widget>>>,
}

pub enum Source {
    Inlines(Rc<Vec<Inline>>),
    Code(Rc<str>),
}

impl LabelGroup {
    pub fn text(&self, doc: &Document) -> String {
        match &self.source {
            Source::Inlines(inlines) => markup::walk_inlines(inlines, doc, None).text,
            Source::Code(code) => code.to_string(),
        }
    }
}

struct LabelSlot {
    label: gtk::Label,
    segment: usize,
    base_markup: String,
    current: RefCell<String>,
}

pub struct LabelGroup {
    entry: usize,
    source: Source,
    labels: Vec<LabelSlot>,
}

pub struct RenderedDoc {
    pub root: gtk::Widget,
    pub groups: Vec<LabelGroup>,
}

impl RenderedDoc {
    pub fn first_label(&self, entry: usize) -> Option<&gtk::Label> {
        self.groups
            .iter()
            .find(|g| g.entry == entry)
            .and_then(|g| g.labels.first())
            .map(|s| &s.label)
    }

    pub fn debug_highlighted(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter_map(|g| {
                let markup = g.labels.first().map(|l| l.current.borrow().clone())?;
                if markup.contains("<span background") {
                    Some(format!(
                        "entry {}: ...{}...",
                        g.entry,
                        markup.chars().take(160).collect::<String>()
                    ))
                } else {
                    None
                }
            })
            .collect()
    }
}

pub fn apply_highlight(
    doc: &Document,
    rendered: &RenderedDoc,
    ranges_of: impl Fn(usize) -> (Vec<std::ops::Range<usize>>, Option<std::ops::Range<usize>>),
    hit: &str,
    active: &str,
) {
    for group in &rendered.groups {
        let (ranges, active_range) = ranges_of(group.entry);
        if ranges.is_empty() && active_range.is_none() {
            for slot in &group.labels {
                update_slot(slot, slot.base_markup.clone());
            }
            continue;
        }
        let owned_active = active_range;
        let hl = Highlight {
            ranges: &ranges,
            active: owned_active.as_ref(),
            hit_color: hit,
            active_color: active,
        };
        match &group.source {
            Source::Code(code) => {
                let markup = markup::code_markup(code, Some(hl));
                for slot in &group.labels {
                    update_slot(slot, markup.clone());
                }
            }
            Source::Inlines(inlines) => {
                let walk = markup::walk_inlines(inlines, doc, Some(hl));
                for slot in &group.labels {
                    let markup = walk
                        .segments
                        .get(slot.segment)
                        .map(|s| s.markup.clone())
                        .unwrap_or_default();
                    update_slot(slot, markup);
                }
            }
        }
    }
}

fn update_slot(slot: &LabelSlot, markup: String) {
    let mut current = slot.current.borrow_mut();
    if *current != markup {
        slot.label.set_markup(&markup);
        *current = markup;
    }
}

pub fn render(ctx: &RenderCtx<'_>) -> RenderedDoc {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_css_classes(&["mv-content"]);
    let mut groups = Vec::new();
    render_blocks(&ctx.doc.blocks, &column, ctx, &mut groups, 1);
    render_footnotes(ctx, &column, &mut groups);
    for (entry, group) in groups.iter_mut().enumerate() {
        group.entry = entry;
    }
    RenderedDoc {
        root: column.upcast(),
        groups,
    }
}

/// Resumable document renderer: builds the same tree [`render`] would, but
/// one unit at a time (a top-level block, then a footnote row) under a time
/// budget, so the window can map with the first slice of content and the
/// rest streams in while the loop stays responsive.
pub struct ChunkedRenderer {
    doc: Document,
    base_dir: Option<PathBuf>,
    link_handler: LinkHandler,
    anchors: Rc<RefCell<HashMap<String, gtk::Widget>>>,
    toc_targets: Rc<RefCell<Vec<gtk::Widget>>>,
    column: gtk::Box,
    groups: Vec<LabelGroup>,
    next_block: usize,
    next_footnote: usize,
    footnotes_started: bool,
    /// A table whose rows are still streaming in: a table is a sequence of
    /// row units so its layout cost lands incrementally like any other
    /// content.
    pending_table: Option<PendingTable>,
    /// Characters of document text in the unit last attached; the streaming
    /// budget counts these (see [`ChunkedRenderer::render_units`]).
    last_chars: usize,
}

struct PendingTable {
    grid: gtk::Grid,
    block: usize,
    row: usize,
}

impl ChunkedRenderer {
    pub fn new(
        doc: Document,
        base_dir: Option<PathBuf>,
        link_handler: LinkHandler,
        anchors: Rc<RefCell<HashMap<String, gtk::Widget>>>,
        toc_targets: Rc<RefCell<Vec<gtk::Widget>>>,
    ) -> Self {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.set_css_classes(&["mv-content"]);
        ChunkedRenderer {
            doc,
            base_dir,
            link_handler,
            anchors,
            toc_targets,
            column,
            groups: Vec::new(),
            next_block: 0,
            next_footnote: 0,
            footnotes_started: false,
            pending_table: None,
            last_chars: 0,
        }
    }

    /// The partially built tree. Widgets already appended stay alive through
    /// the refcount the parent column holds.
    pub fn root(&self) -> &gtk::Widget {
        self.column.upcast_ref()
    }

    /// Build units until `budget` is spent. Returns true once the whole
    /// document, footnotes included, is in the tree; at least one unit is
    /// built per call, so progress is guaranteed even with a zero budget.
    /// Attach content until roughly `char_budget` characters of document
    /// text have landed. The budget counts text because layout, not
    /// construction, is the expensive part, and layout cost scales with the
    /// amount of wrapped text: paragraphs, table rows, and code all account
    /// alike, with no special cases per structure. At least one unit is
    /// always attached, so progress never stalls.
    pub fn render_units(&mut self, char_budget: usize) -> bool {
        let mut spent = 0usize;
        loop {
            let done = self.render_unit();
            spent += self.last_chars;
            if done {
                return true;
            }
            if spent >= char_budget {
                return false;
            }
        }
    }

    /// Assign find entries in creation order and hand back the document and
    /// the finished tree. Drains the renderer through `&mut self` so it can
    /// be called from inside a repeating main-loop closure.
    pub fn finish(&mut self) -> (Document, RenderedDoc) {
        for (entry, group) in self.groups.iter_mut().enumerate() {
            group.entry = entry;
        }
        (
            std::mem::take(&mut self.doc),
            RenderedDoc {
                root: self.column.clone().upcast(),
                groups: std::mem::take(&mut self.groups),
            },
        )
    }

    fn render_unit(&mut self) -> bool {
        let Self {
            doc,
            base_dir,
            link_handler,
            anchors,
            toc_targets,
            column,
            groups,
            next_block,
            next_footnote,
            footnotes_started,
            pending_table,
            last_chars,
            ..
        } = self;
        let ctx = RenderCtx {
            doc,
            base_dir: base_dir.as_deref(),
            link_handler: link_handler.clone(),
            anchors: anchors.clone(),
            toc_targets: toc_targets.clone(),
        };
        if let Some(pending) = pending_table {
            let Block::Table { aligns, head, rows } = &doc.blocks[pending.block] else {
                *pending_table = None;
                return false;
            };
            let (cells, is_head) = if pending.row < head.len() {
                (&head[pending.row], true)
            } else {
                (&rows[pending.row - head.len()], false)
            };
            *last_chars = cells
                .iter()
                .map(|cell| markup::walk_inlines(cell, doc, None).text.len())
                .sum();
            attach_table_row(
                &pending.grid,
                cells,
                pending.row,
                is_head,
                aligns,
                &ctx,
                groups,
            );
            pending.row += 1;
            if pending.row >= head.len() + rows.len() {
                *pending_table = None;
            }
            return false;
        }
        if *next_block < doc.blocks.len() {
            if matches!(doc.blocks[*next_block], Block::Table { .. }) {
                let grid = new_table();
                column.append(&grid);
                *pending_table = Some(PendingTable {
                    grid,
                    block: *next_block,
                    row: 0,
                });
                *last_chars = 1;
            } else {
                *last_chars = markup::block_text(&doc.blocks[*next_block], doc).len();
                render_block(&doc.blocks[*next_block], column, &ctx, groups, 1);
            }
            *next_block += 1;
            return false;
        }
        if *next_footnote < doc.footnotes.len() {
            if !*footnotes_started {
                *footnotes_started = true;
                start_footnotes(column);
            }
            let footnote = &doc.footnotes[*next_footnote];
            *last_chars = footnote
                .blocks
                .iter()
                .map(|block| markup::block_text(block, doc).len())
                .sum::<usize>()
                + footnote.label.len();
            render_footnote_row(*next_footnote, footnote, column, &ctx, groups);
            *next_footnote += 1;
            return false;
        }
        *last_chars = 0;
        true
    }
}

fn render_footnotes(ctx: &RenderCtx<'_>, column: &gtk::Box, groups: &mut Vec<LabelGroup>) {
    if ctx.doc.footnotes.is_empty() {
        return;
    }
    start_footnotes(column);
    for (index, footnote) in ctx.doc.footnotes.iter().enumerate() {
        render_footnote_row(index, footnote, column, ctx, groups);
    }
}

fn start_footnotes(column: &gtk::Box) {
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.set_css_classes(&["mv-rule"]);
    column.append(&separator);
    column.append(&plain_label("Footnotes", &["mv-h2"]));
}

fn render_footnote_row(
    index: usize,
    footnote: &crate::doc::Footnote,
    column: &gtk::Box,
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_css_classes(&["mv-footnote"]);
    let marker = gtk::Label::new(None);
    marker.set_markup(&format!(
        "<a href=\"#fn-{}\"><sup>[{}]</sup></a>",
        index + 1,
        index + 1
    ));
    marker.set_valign(gtk::Align::Start);
    marker.connect_activate_link({
        let handler = ctx.link_handler.clone();
        move |_, uri| link_activate(&handler, uri)
    });
    row.append(&marker);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_hexpand(true);
    render_blocks(&footnote.blocks, &body, ctx, groups, 1);
    row.append(&body);
    column.append(&row);
    ctx.anchors
        .borrow_mut()
        .insert(format!("fn-{}", index + 1), row.clone().upcast());
}

fn new_table() -> gtk::Grid {
    let table = gtk::Grid::new();
    table.set_css_classes(&["mv-table"]);
    table.set_hexpand(true);
    table.set_column_spacing(16);
    table.set_row_spacing(6);
    table
}

/// Attaches one table row (header or body). Shared by the synchronous
/// [`render`] path and the row-streaming [`ChunkedRenderer`].
fn attach_table_row(
    table: &gtk::Grid,
    cells: &[Vec<Inline>],
    row_index: usize,
    is_head: bool,
    aligns: &[Alignment],
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
) {
    for (column, cell) in cells.iter().enumerate() {
        let mut classes = vec!["mv-cell"];
        if is_head {
            classes.push("mv-cell-head");
        }
        let label = gtk::Label::new(None);
        label.set_css_classes(&classes);
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label.set_valign(gtk::Align::Start);
        let xalign = aligns
            .get(column)
            .map(|a| alignment_xalign(*a))
            .unwrap_or(0.0);
        label.set_xalign(xalign);
        label.set_hexpand(true);
        label.connect_activate_link({
            let handler = ctx.link_handler.clone();
            move |_, uri| link_activate(&handler, uri)
        });
        let walk = markup::walk_inlines(cell, ctx.doc, None);
        let base = walk
            .segments
            .first()
            .map(|s| s.markup.clone())
            .unwrap_or_default();
        label.set_markup(&base);
        groups.push(LabelGroup {
            entry: 0,
            source: Source::Inlines(Rc::new(cell.clone())),
            labels: vec![LabelSlot {
                label: label.clone(),
                segment: 0,
                base_markup: base.clone(),
                current: RefCell::new(base),
            }],
        });
        table.attach(&label, column as i32, row_index as i32, 1, 1);
    }
}

fn render_blocks(
    blocks: &[Block],
    into: &gtk::Box,
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
    depth: usize,
) {
    for block in blocks {
        render_block(block, into, ctx, groups, depth);
    }
}

fn render_block(
    block: &Block,
    into: &gtk::Box,
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
    depth: usize,
) {
    match block {
        Block::Heading { level, inlines, .. } => {
            let css = format!("mv-h{level}");
            let widget = render_inlines(inlines, &["mv-para", &css], ctx, groups);
            widget.update_property(&[gtk::accessible::Property::Label(
                &markup::walk_inlines(inlines, ctx.doc, None).text,
            )]);
            ctx.toc_targets.borrow_mut().push(widget.clone());
            into.append(&widget);
        }
        Block::Paragraph(inlines) => {
            let widget = render_inlines(inlines, &["mv-para"], ctx, groups);
            into.append(&widget);
        }
        Block::Code { text, .. } => {
            let label = gtk::Label::new(None);
            label.set_css_classes(&["mv-code"]);
            label.set_xalign(0.0);
            label.set_valign(gtk::Align::Start);
            label.set_wrap(true);
            label.set_wrap_mode(pango::WrapMode::WordChar);
            label.connect_activate_link({
                let handler = ctx.link_handler.clone();
                move |_, uri| link_activate(&handler, uri)
            });
            let base = markup::code_markup(text, None);
            label.set_markup(&base);
            groups.push(LabelGroup {
                entry: 0,
                source: Source::Code(Rc::from(text.as_str())),
                labels: vec![LabelSlot {
                    label: label.clone(),
                    segment: 0,
                    base_markup: base.clone(),
                    current: RefCell::new(base),
                }],
            });
            into.append(&label);
        }
        Block::Quote { kind: None, blocks } => {
            let quote = gtk::Box::new(gtk::Orientation::Vertical, 8);
            quote.set_css_classes(&["mv-quote"]);
            quote.set_hexpand(true);
            render_blocks(blocks, &quote, ctx, groups, depth + 1);
            into.append(&quote);
        }
        Block::Quote {
            kind: Some(alert),
            blocks,
        } => {
            let css = match alert {
                BlockQuoteKind::Note => "mv-alert mv-alert-note",
                BlockQuoteKind::Tip => "mv-alert mv-alert-tip",
                BlockQuoteKind::Important => "mv-alert mv-alert-important",
                BlockQuoteKind::Warning => "mv-alert mv-alert-warning",
                BlockQuoteKind::Caution => "mv-alert mv-alert-caution",
            };
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
            box_.set_css_classes(&[css]);
            box_.set_hexpand(true);
            box_.append(&plain_label(alert_title(*alert), &["mv-alert-title"]));
            render_blocks(blocks, &box_, ctx, groups, depth + 1);
            into.append(&box_);
        }
        Block::List {
            start,
            tight,
            items,
        } => {
            let list = gtk::Box::new(gtk::Orientation::Vertical, if *tight { 3 } else { 9 });
            list.set_css_classes(&["mv-list"]);
            list.set_hexpand(true);
            if depth > 1 {
                list.add_css_class("mv-list-nested");
            }
            for (position, item) in items.iter().enumerate() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_valign(gtk::Align::Start);
                let marker = match item.task {
                    Some(checked) => {
                        let check = gtk::CheckButton::new();
                        check.set_active(checked);
                        check.set_can_focus(false);
                        check.set_can_target(false);
                        check.clone().upcast::<gtk::Widget>()
                    }
                    None => match start {
                        Some(first) => {
                            let label =
                                gtk::Label::new(Some(&format!("{}.", first + position as u64)));
                            finish_marker(&label);
                            label.upcast::<gtk::Widget>()
                        }
                        None => {
                            let label = gtk::Label::new(Some("•"));
                            finish_marker(&label);
                            label.upcast::<gtk::Widget>()
                        }
                    },
                };
                row.append(&marker);
                let body = gtk::Box::new(gtk::Orientation::Vertical, if *tight { 3 } else { 9 });
                body.set_hexpand(true);
                render_blocks(&item.blocks, &body, ctx, groups, depth + 1);
                row.append(&body);
                list.append(&row);
            }
            into.append(&list);
        }
        Block::Table { aligns, head, rows } => {
            let table = new_table();
            for (row_index, row) in head.iter().chain(rows.iter()).enumerate() {
                attach_table_row(&table, row, row_index, row_index == 0, aligns, ctx, groups);
            }
            into.append(&table);
        }
        Block::Rule => {
            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            separator.set_css_classes(&["mv-rule"]);
            into.append(&separator);
        }
    }
}

fn finish_marker(label: &gtk::Label) {
    label.set_css_classes(&["mv-list-marker"]);
    label.set_valign(gtk::Align::Start);
    label.set_xalign(0.5);
}

fn alignment_xalign(align: Alignment) -> f32 {
    match align {
        Alignment::None | Alignment::Left => 0.0,
        Alignment::Center => 0.5,
        Alignment::Right => 1.0,
    }
}

fn alert_title(kind: BlockQuoteKind) -> &'static str {
    match kind {
        BlockQuoteKind::Note => "Note",
        BlockQuoteKind::Tip => "Tip",
        BlockQuoteKind::Important => "Important",
        BlockQuoteKind::Warning => "Warning",
        BlockQuoteKind::Caution => "Caution",
    }
}

fn render_inlines(
    inlines: &[Inline],
    css: &[&str],
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
) -> gtk::Widget {
    let walk = markup::walk_inlines(inlines, ctx.doc, None);
    if walk.images.is_empty() {
        let label = gtk::Label::new(None);
        label.set_css_classes(css);
        label.set_xalign(0.0);
        label.set_valign(gtk::Align::Start);
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label.set_hexpand(true);
        label.connect_activate_link({
            let handler = ctx.link_handler.clone();
            move |_, uri| link_activate(&handler, uri)
        });
        let base = walk
            .segments
            .first()
            .map(|s| s.markup.clone())
            .unwrap_or_default();
        label.set_markup(&base);
        groups.push(LabelGroup {
            entry: 0,
            source: Source::Inlines(Rc::new(inlines.to_vec())),
            labels: vec![LabelSlot {
                label: label.clone(),
                segment: 0,
                base_markup: base.clone(),
                current: RefCell::new(base),
            }],
        });
        label.upcast::<gtk::Widget>()
    } else {
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
        box_.set_hexpand(true);
        for (index, image) in walk.images.iter().enumerate() {
            if let Some(segment) = walk.segments.get(index) {
                if !segment.markup.trim().is_empty() {
                    let label = labeled(
                        segment.markup.clone(),
                        css,
                        ctx,
                        groups,
                        Rc::new(inlines.to_vec()),
                        index,
                    );
                    box_.append(&label);
                }
            }
            box_.append(&picture_for(image, ctx));
        }
        if let Some(last) = walk.segments.get(walk.images.len()) {
            if !last.markup.trim().is_empty() {
                let label = labeled(
                    last.markup.clone(),
                    css,
                    ctx,
                    groups,
                    Rc::new(inlines.to_vec()),
                    walk.images.len(),
                );
                box_.append(&label);
            }
        }
        box_.upcast::<gtk::Widget>()
    }
}

fn labeled(
    markup: String,
    css: &[&str],
    ctx: &RenderCtx<'_>,
    groups: &mut Vec<LabelGroup>,
    inlines: Rc<Vec<Inline>>,
    segment: usize,
) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_css_classes(css);
    label.set_xalign(0.0);
    label.set_valign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.set_hexpand(true);
    label.connect_activate_link({
        let handler = ctx.link_handler.clone();
        move |_, uri| link_activate(&handler, uri)
    });
    label.set_markup(&markup);
    groups.push(LabelGroup {
        entry: 0,
        source: Source::Inlines(inlines),
        labels: vec![LabelSlot {
            label: label.clone(),
            segment,
            base_markup: markup.clone(),
            current: RefCell::new(markup),
        }],
    });
    label
}

fn picture_for(image: &markup::ImageInfo, ctx: &RenderCtx<'_>) -> gtk::Widget {
    if let Some(path) = local_image_path(&image.url, ctx.base_dir) {
        if let Ok(texture) = gdk::Texture::from_filename(&path) {
            let picture = gtk::Picture::for_paintable(&texture);
            picture.set_css_classes(&["mv-image"]);
            // Fill + ScaleDown: fill the column width so the allocated width
            // is meaningful, scale down to fit, never upscale.
            picture.set_content_fit(gtk::ContentFit::ScaleDown);
            picture.set_halign(gtk::Align::Fill);
            picture.set_can_shrink(true);
            if !image.alt.is_empty() {
                picture.set_tooltip_text(Some(&image.alt));
            }
            return picture.upcast::<gtk::Widget>();
        }
    }
    let fallback = gtk::Label::new(Some(&format!("🖼 {}: {}", image.alt, image.url)));
    fallback.set_css_classes(&["mv-image-fallback"]);
    fallback.set_xalign(0.0);
    fallback.set_wrap(true);
    fallback.set_wrap_mode(pango::WrapMode::WordChar);
    fallback.upcast::<gtk::Widget>()
}

fn local_image_path(url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    if url.contains("://") || url.starts_with("data:") {
        return None;
    }
    let path = PathBuf::from(url);
    if path.is_absolute() {
        return Some(path);
    }
    base_dir.map(|base| base.join(path))
}

fn plain_label(text: &str, css: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_css_classes(css);
    label.set_xalign(0.0);
    label.set_valign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label
}

fn link_activate(handler: &LinkHandler, uri: &str) -> glib::Propagation {
    if handler(uri) {
        glib::Propagation::Stop
    } else {
        glib::Propagation::Proceed
    }
}
