# Translations

UI strings are marked with `gettext()` / `ngettext()` re-exported from
`src/i18n.rs`. No catalogs are shipped yet; until one is compiled and
installed, every lookup falls back to the English source strings.

Regenerate the template after changing marked strings:

    xgettext --package-name=marksight \
        --package-version=$(cargo metadata --format-version 1 \
            --no-deps | sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -1) \
        --keyword=gettext --keyword=ngettext:1,2 \
        --from-code=UTF-8 -o po/marksight.pot -f po/POTFILES.in

Start a new language with:

    msginit -i po/marksight.pot -l ll_CC -o po/ll_CC.po

and list its code in a `po/LINGUAS` file.

When the first translation lands, packaging must compile each catalog to
`<LOCALEDIR>/<ll_CC>/LC_MESSAGES/marksight.mo`. `LOCALEDIR` is set at
build time and defaults to `/usr/share/locale` (the deb prefix).
