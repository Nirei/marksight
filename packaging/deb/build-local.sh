#!/usr/bin/env bash
# Build a marksight .deb from the working tree.
#
# One-time setup on a Debian/Ubuntu/Mint machine:
#   sudo apt install devscripts debhelper libgtk-4-dev
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
  echo "dpkg-buildpackage not found — sudo apt install devscripts debhelper" >&2
  exit 1
fi

cargo_bin=""
for candidate in "$(command -v cargo 2>/dev/null || true)" \
  "$HOME/.cargo/bin/cargo" \
  "$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo" \
  /usr/bin/cargo; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    cargo_bin="$candidate"
    break
  fi
done
[ -n "$cargo_bin" ] || {
  echo "cargo not found (PATH, ~/.cargo/bin, rustup, /usr/bin)" >&2
  exit 1
}

base="$(mktemp -d)"
work="$base/src"
mkdir "$work"
trap 'rm -rf "$base"' EXIT

tar \
  --exclude=./target \
  --exclude=./.git \
  --exclude=./.pi \
  --exclude=./fuzz/target \
  --exclude=./fuzz/corpus \
  --exclude='./packaging/deb/*.deb' \
  -cf - . | tar -xf - -C "$work"
cp -r packaging/deb/debian "$work/debian"
printf '%s\n' "$(cd "$(dirname "$cargo_bin")" && pwd)/$(basename "$cargo_bin")" >"$work/debian/cargo.path"

echo "building marksight_${version}_${arch}.deb in $work"
(cd "$work" && dpkg-buildpackage -us -uc -b)

cp "$base/marksight_${version}_${arch}.deb" packaging/deb/
echo "wrote packaging/deb/marksight_${version}_${arch}.deb"
echo "install with: sudo apt install ./packaging/deb/marksight_${version}_${arch}.deb"
