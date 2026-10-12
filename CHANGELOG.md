# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-10-12

First public release.

### Added

- CommonMark and GitHub-Flavored Markdown rendered to native GTK 4 widgets:
  tables with alignment, task lists, footnotes, strikethrough, alerts,
  images and autolinks. The desktop theme is followed automatically, light
  or dark, with no configuration.
- Live reload: the document re-renders shortly after every save.
- Table of contents sidebar (`F9`) with click-to-jump navigation.
- Find in document (`Ctrl+F`): case-insensitive search, highlighted
  matches, `Enter` / `Shift+Enter` to cycle, and a match counter.
- GitHub-style heading anchors and safe internal links.
- User interface translated into 100 locales besides English.
- Accessibility: screen reader support with announced match counts, image
  alt text, correct heading levels and a named menu button.
- File opening through the XDG portal, so Flatpak confinement works
  unchanged.
- Optional startup-phase timing markers behind `MARKSIGHT_TIMING=1`.

### Changed

- Renamed from markview to marksight, with the marksight logo as the
  application icon.

### Fixed

- Second launches open their file instantly, in a new window, without
  starting a second application; the startup notification completes.
- The window appears only after the first page has rendered, with the
  contents sidebar fully built.
- The advertised `Ctrl+F` shortcut now opens a working search bar, and
  re-running the same search preserves the current match.
- Case-insensitive find no longer breaks on non-ASCII text.
- Find no longer reports matches inside image alt text.
- Table of contents and anchor jumps are deterministic and independent of
  the scroll position.
- External links open only for allowlisted schemes; heading anchors scroll
  within the document and never launch anything.
- Live reload survives transient read failures, reloads only the displayed
  file, and applies a queue of saves in one pass.
- Images render in table cells and keep their space in documents taller
  than the window.
- Image loading stays inside the document's own directory: absolute paths,
  `..` escapes, `file:` URLs and symlinks pointing outside show a
  placeholder instead; remote images are never fetched.
- Control characters in documents are replaced with U+FFFD instead of
  crashing the viewer.
- Document reads are limited to regular files of at most 64 MiB.
- Container nesting is capped at 256 levels, so hostile documents cannot
  hang the viewer.
- Pinned note sides are mirrored correctly in right-to-left locales.
- The file dialog uses GTK's translated default title.
- Compiled translations ship in every package build path.
- The About dialog logo and the taskbar icon no longer depend on the icon
  theme.
- The desktop entry is registered under Office;Viewer, so marksight appears
  in application menus.
- `Ctrl+Q` closes the window when no application is attached.

### Performance

- Documents stream into the view instead of building fully before the
  first paint; streaming is budgeted by text size, so long files render
  progressively.
- Highlight lookups are binary searches over the find index.

[1.0.0]: https://github.com/cinzatech/marksight/commits/v1.0.0
