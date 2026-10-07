# markview — a minimal, theme-native markdown viewer

> Status: planning · Target stack: Rust + GTK4 (no libadwaita, no WebKit)
> Scope: open a markdown file → render to native widgets → live reload, TOC, find
> Goal: publishable via crates.io, Flathub, PPA/AUR, and (eventually) Debian official

## 1. Decisions (locked)

| Decision | Choice | Why |
| --- | --- | --- |
| GUI toolkit | **gtk4-rs** (plain GTK4, no libadwaita) | Follows `gtk-theme-name` automatically → Mint-Y/Mint-Y-Dark fidelity on Cinnamon; zero code for theming. libadwaita would force Adwaita styling and ignore the system theme. |
| Rendering | **Native GTK widgets** (no WebKitGTK) | ~10× lighter than WebKit (~200 MB RSS), tiny attack surface (no JS engine, no HTML), and only native widgets follow the GTK theme. |
| Markdown standard | **CommonMark + GFM** via `pulldown-cmark` | Fully CommonMark-conformant plus GFM tables, task lists, strikethrough, footnotes, alerts. Fastest Rust parser (pull/streaming). Covers ~all real-world READMEs. |
| File watching | `notify` (inotify backend) | Widely used, pure-Rust API over Linux inotify. |
| Features | Viewer + live reload + TOC sidebar + Ctrl+F find | Per user decision. |
| License | MIT OR Apache-2.0 | Rust-ecosystem standard, DFSG-free (required for Debian path). |

Detected environment: Linux Mint 22.3, Cinnamon, GTK 4.14.5, Rust 1.98. GTK ≥ 4.10 is our floor (GtkFileDialog/UriLauncher); Mint 22 = 4.14 ✓, Debian trixie = 4.16 ✓, Ubuntu 24.04 = 4.14 ✓.

## 2. Non-goals

- No editing, no export/PDF, no plugin system, no network features.
- No raw-HTML passthrough — CommonMark allows inline HTML; we **drop** `Html`/`InlineHtml` events rather than render them (this is the classic viewer XSS vector). Nothing text-untrusted ever reaches anything that executes.
- No custom theming engine — the system GTK theme *is* the theme.

## 3. Code guidelines

> Default to zero comments. Let code express intent. Comments are only ever allowed if the code itself cannot adequately express something.

Scope of the rule: implementation comments (`//`). If code needs a `//` to be understood, restructure it first; commit messages and this plan absorb the *why*. API documentation is a different thing — `///`/`//!` doc-comments stating a module's or function's contract are good practice and expected, binary crate or not.

## 4. Architecture

```text
CLI arg / GtkFileDialog / file-manager activation
        │
        ▼
   read file ──► pulldown-cmark pull parser
        │              │
        │              ▼
        │      doc.rs: pure model      ← unit-testable, no GTK types
        │      (blocks, inline runs, headings, text index)
        │              │
        │              ▼
        │      render.rs: model → widget tree (thin GTK layer)
        ▼                     │
 notify + debounce ──► rebuild on change
```

**Key principle:** parsing/model-building is a pure, GTK-free module (fully unit-testable and fuzzable); GTK code is a thin mapper from model → widgets.

### 4.1 Block → widget mapping

| Markdown | Widget |
| --- | --- |
| Heading h1–h6 | `GtkLabel` + CSS size class; registered in TOC model |
| Paragraph | `GtkLabel`, wrap mode `word-char`, Pango markup for inline styles |
| Bold/italic/strike/code/link | Pango `<b> <i> <s> <tt> <a href>` — all text escaped, tags added by us (never by the document) |
| Link activation | `activate-link` signal → `GtkUriLauncher` (portal-respecting) |
| List (nested) | `GtkBox` rows with progressive left margin; bullets/numbers as labels |
| Task list item | read-only `GtkCheckButton` + text |
| Blockquote | `GtkBox` + `.blockquote` CSS (left border, subtle bg) |
| Fenced/indented code | monospace `GtkLabel` + `.code-block` CSS (relative bg via theme colors) |
| Table (GFM) | `GtkGrid` + CSS row separators, alignment honored |
| Thematic break | `GtkSeparator` |
| Image | `Gdk::Texture::from_file` (png/jpeg/tiff built in; SVG optional via `librsvg` feature) |
| Footnotes | auto-generated small-print section at document end |
| Raw HTML | **dropped** (security) |

### 4.2 Theming strategy

- Ship one small CSS file that references **theme-defined named colors only** (`@theme_fg_color`, `@theme_bg_color`, alpha blends) — so Mint-Y, Mint-Y-Dark, and any installed theme restyle the app with no per-theme work.
- Relative font scaling for headings; monospace via the theme's mono font. No hardcoded colors anywhere.

### 4.3 Live reload

`notify` watcher + ~150 ms debounce → re-read + re-parse on a worker thread → swap widget tree on the main thread via `glib::MainContext`. Typical README re-parses in single-digit ms; huge files stay off the UI thread.

### 4.4 TOC & find

- TOC: headings collected during parse → `GtkListBox` in a collapsible sidebar (`GtkPaned`/reveal, F9); click scrolls to block.
- Find: `GtkSearchBar` + entry (Ctrl+F); flat text index per block; matches highlighted by rebuilding only the matched labels' Pango markup with a `@theme_selected_bg_color`-alpha span; Enter/n+Enter cycles matches.

## 5. Security posture

- No network, no JavaScript, no HTML rendering, no `unsafe` in our crate.
- The only markup we ever hand Pango is text-escaped-by-us wrapped in tags emitted by us.
- Fuzz the model/markup layer per §7.3 — pulldown-cmark is fuzzed upstream; our event→model→Pango layer is the new attack surface.
- Flatpak: default sandbox; file access exclusively via portal (GtkFileDialog / document portal).

## 6. Performance budgets

| Metric | Budget |
| --- | --- |
| Cold start | < 150 ms |
| RSS (typical README) | < 80 MB (GTK4 baseline dominates) |
| Parse+render 10k-line doc | < 100 ms |
| Stripped binary | < 15 MB |

## 7. Testing strategy

Three layers, ordered by cost, plus a GUI smoke layer:

### 7.1 Unit (every commit, headless)

`doc.rs` and `find.rs` are GTK-free by design — plain `cargo test`. Markdown string in → assert model out: block sequence, heading levels/ids, text-index contents, escaping.

### 7.2 Property + snapshot (every commit, headless)

- `proptest` generates structured markdown (nesting depth, table shapes, mixed inline styles, malformed fragments) and asserts model invariants: no panics, unique heading ids, bounded memory per input size.
- The Pango-markup builder is a pure function (`model → String`); golden-file snapshots per block type — changes surface as reviewable diffs.

### 7.3 Fuzzing (scheduled CI, deep exploration)

`cargo-fuzz` targets over the layers upstream fuzzing does not cover:

| Target | Pipeline | Invariants |
| --- | --- | --- |
| `fuzz_model` | arbitrary bytes → parse → model | no panic/hang; model invariants hold; input size-capped |
| `fuzz_markup` | model → Pango markup string | no panic; output parses as valid Pango markup; no unescaped `<`/`&` originating from document text |
| `fuzz_find` | arbitrary text + needle | index build + highlight cycle terminates; match offsets in bounds |

Corpus persisted across CI runs and grows. Later: OSS-Fuzz integration (Rust supported) — strengthens the security story for Flathub/Debian review.

### 7.4 GUI smoke (CI, Xvfb)

`xvfb-run` boots the real app against fixtures (huge table, pathological nesting, 10 MB document) — no crash, timing budgets from §6 hold.

## 8. Repo layout

```text
markview/
├── Cargo.toml
├── src/
│   ├── main.rs      # CLI (clap-lite or hand-rolled), app bootstrap
│   ├── app.rs       # window, headerbar, paned, search bar
│   ├── doc.rs       # pure model: events → blocks/headings/text index
│   ├── render.rs    # model → GTK widget tree
│   ├── watch.rs     # notify + debounce
│   └── find.rs      # search + highlight
├── resources/markview.css  (+ .gresource.xml)
├── data/            # markview.desktop.in, metainfo/appdata, SVG icon
├── packaging/       # flathub/, deb/, aur/
└── .github/workflows/  # ci.yml, release.yml
```

Dependencies: `gtk4`, `pulldown-cmark`, `notify`, `glib` (transitively required). Nothing exotic — all present in Debian/Ubuntu/Arch repos.

## 9. Milestones

- **M0 — Vertical slice:** window + CLI arg + headings/paragraphs/code/lists render theme-correctly. Proves the widget-mapping approach.
- **M1 — Full coverage:** tables, task lists, footnotes, blockquotes, images, links, alerts — with unit/property tests per block type (§7.1–7.2).
- **M2 — Features:** live reload, TOC sidebar, find.
- **M3 — Polish:** open dialog, error/empty states, a11y labels, keybindings, CSS pass, full testing suite per §7 (fuzz targets landed, snapshots complete, smoke in CI).
- **M4 — Packaging wave 1:** CI + release binaries (cargo-dist), crates.io publish, Flathub submission, AUR PKGBUILDs, Launchpad PPA.
- **M5 — Debian:** ITP bug, Salsa packaging, sponsor, archive upload (flows to Ubuntu/Mint automatically).

## 10. Open verification checklist (before M0)

- [ ] `markview` name availability on crates.io and as Flathub app-ID domain (a `markview.nvim` plugin exists in a different ecosystem — no legal issue, but check for confusion; pick a domain/app-ID accordingly).
- [ ] Pin exact `pulldown-cmark` / `gtk4` crate versions; confirm `ENABLE_GFM` umbrella flag set (tables, strikethrough, task lists, alerts) + footnotes + heading attributes.
- [ ] Confirm Mint-Y GTK4 CSS has expected named colors for our custom CSS (spot-check `@theme_fg_color` etc.).
- [ ] Decide flatpak runtime version at submission time (latest stable `org.gnome.Platform`).

## 11. Risks

| Risk | Mitigation |
| --- | --- |
| Inline code / link styling fidelity across themes | Style via CSS classes + theme named colors only; accept graceful degradation on exotic themes. |
| CLI-passed file paths inside flatpak sandbox | Document `flatpak run ... --file=…` portal flow; primary UX is file dialog + file-manager activation, both portal-clean. |
| Debian inclusion timeline (sponsor-driven, months) | Treat as post-1.0; Flathub/AUR/PPA give full coverage meanwhile. |
| `markview` name collision at publication time | Decide name + domain in M0, before any submission. |
