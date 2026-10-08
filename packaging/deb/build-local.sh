#!/usr/bin/env bash
# Build a marksight .deb from the working tree.
#
# One-time setup on a Debian/Ubuntu/Mint machine:
#   sudo apt install devscripts debhelper dh-cargo libgtk-4-dev
# (cargo/rustc may come from rustup; dpkg-buildpackage runs as your user)
#
# Usage: packaging/deb/build-local.sh
# Result: packaging/deb/marksight_<version>_<arch>.deb

set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

version="$(dpkg-parsechangelog -l packaging/deb/debian/changelog -S Version)"
arch="$(dpkg --print-architecture)"

if ! [ -x "$(command -v dpkg-buildpackage)" ]; then
    echo "dpkg-buildpackage not found — sudo apt install devscripts debhelper dh-cargo" >&2
    exit 1
fi

work="$(mktemp -d)"
parent="$(dirname "$work")"
trap 'rm -rf "$work"' EXIT

tar \
    --exclude=./target \
    --exclude=./.git \
    --exclude=./.pi \
    --exclude=./fuzz/target \
    --exclude=./fuzz/corpus \
    --exclude='./packaging/deb/*.deb' \
    -cf - . | tar -xf - -C "$work"
cp -r packaging/deb/debian "$work/debian"

echo "building marksight_${version}_${arch}.deb in $work"
(cd "$work" && dpkg-buildpackage -us -uc -b)

cp "$parent/marksight_${version}_${arch}.deb" packaging/deb/
echo "wrote packaging/deb/marksight_${version}_${arch}.deb"
echo "install with: sudo apt install ./packaging/deb/marksight_${version}_${arch}.deb"
