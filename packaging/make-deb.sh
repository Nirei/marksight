#!/usr/bin/env bash
# Build a marksight .deb via cargo-deb when available, falling back to
# dpkg-buildpackage (packaging/deb/build-local.sh). Catalogs are compiled
# by the make po-mo dependency before this runs; both build paths package
# them (cargo-deb via the asset list in Cargo.toml, dpkg via debian/rules).
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v cargo-deb >/dev/null 2>&1; then
  cargo deb && cp target/debian/marksight_*.deb packaging/deb/
else
  packaging/deb/build-local.sh
fi
