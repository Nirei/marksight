# Translations

UI strings are marked with `gettext()` / `ngettext()` re-exported from
`src/i18n.rs`. Catalogs are shipped for the locales listed in
`po/LINGUAS`; messages without a translation fall back to the English
source strings.

Regenerate the template after changing marked strings:

    xgettext --package-name=marksight \
        --package-version=$(cargo metadata --format-version 1 \
            --no-deps | sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -1) \
        --keyword=gettext --keyword=ngettext:1,2 \
        --from-code=UTF-8 -o po/marksight.pot -f po/POTFILES.in

Start a new language with:

    msginit -i po/marksight.pot -l ll_CC --no-translator -o po/ll_CC.po

and list its code in a `po/LINGUAS` file. If the locale is not installed
on the generating machine, `msginit` leaves `Plural-Forms` as
`nplurals=INTEGER; plural=EXPRESSION;` — fill in the locale's formula
by hand in that case.

Packaging compiles every catalog listed in `po/LINGUAS` with `msgfmt` to
`<LOCALEDIR>/<ll_CC>/LC_MESSAGES/marksight.mo`. `LOCALEDIR` is set at
build time and defaults to `/usr/share/locale` (the deb and AUR prefix);
the flatpak manifest passes `LOCALEDIR=/app/share/locale`.

## Translation conventions

Context for translators: marksight is a minimal, theme-native markdown
viewer that renders documents with native GTK 4 widgets. The translatable
strings cover the main window actions (open, menu, quit, empty state,
error dialog), the table of contents sidebar, the search bar, and rendered
document elements (GFM alert titles, footnotes, image alt-text fallback).

- The name `marksight` and the word `Markdown` stay in English.
- GFM alert titles (`Note`, `Tip`, `Important`, `Warning`, `Caution`)
  follow the wording GitHub uses for localized alerts.
- Named placeholders (`{size}`, `{heading}`, ...) must appear verbatim
  in every translation; the words around them may reorder freely.
- Keyboard tokens (`Ctrl+O`, `F9`, `Shift+Enter`) are never translated.
- Use the register standard for desktop UI in the target language;
  existing GNOME translations are a good reference.
- Generate new catalogs with `msginit --no-translator` so plural forms
  match the locale.
