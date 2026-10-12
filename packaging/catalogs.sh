#!/usr/bin/env bash
# Compile and install gettext catalogs for marksight.
#
# The catalog list comes from po/LINGUAS (one locale code per line).
#   catalogs.sh stage                 compile po/*.po into po/.mo/
#   catalogs.sh install <datadir>     install under <datadir>/locale/
#   catalogs.sh uninstall <datadir>   remove from <datadir>/locale/
#   catalogs.sh clean                 delete the po/.mo staging tree
#
# 'stage' also warns when a locale is missing from the cargo-deb asset
# list in Cargo.toml, so the two lists cannot drift apart silently.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  echo "usage: $0 stage|install <datadir>|uninstall <datadir>|clean" >&2
  exit 2
}

case "${1:-}" in
stage)
  # Wipe any previous staging tree first: in shared checkouts it may
  # belong to another user, and stale catalogs must never survive.
  rm -rf po/.mo
  while read -r l; do
    [ -n "$l" ] || continue
    install -d -m 775 "po/.mo/$l/LC_MESSAGES"
    msgfmt -o "po/.mo/$l/LC_MESSAGES/marksight.mo" "po/$l.po"
    grep -q "usr/share/locale/$l/LC_MESSAGES" Cargo.toml ||
      echo "WARNING: $l is in po/LINGUAS but not in [package.metadata.deb] assets" >&2
  done <po/LINGUAS
  ;;
install)
  [ $# -eq 2 ] || usage
  while read -r l; do
    [ -n "$l" ] || continue
    install -Dm644 "po/.mo/$l/LC_MESSAGES/marksight.mo" \
      "$2/locale/$l/LC_MESSAGES/marksight.mo"
  done <po/LINGUAS
  ;;
uninstall)
  [ $# -eq 2 ] || usage
  while read -r l; do
    [ -n "$l" ] || continue
    rm -f "$2/locale/$l/LC_MESSAGES/marksight.mo"
  done <po/LINGUAS
  ;;
clean)
  rm -rf po/.mo
  ;;
*)
  usage
  ;;
esac
