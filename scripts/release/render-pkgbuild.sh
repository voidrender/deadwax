#!/usr/bin/env bash
# Prints the AUR PKGBUILD for a release, built from packaging/aur/PKGBUILD
# with the release's version and checksum.
#
# Usage: scripts/release/render-pkgbuild.sh VERSION SHA256
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION SHA256" >&2
  exit 2
fi
version="$1"
sha="$2"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: bad version '$version'" >&2; exit 2; }
[[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo "error: bad sha256 '$sha'" >&2; exit 2; }

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$(sed \
  -e "s|^pkgver=.*|pkgver=$version|" \
  -e "s|^pkgrel=.*|pkgrel=1|" \
  -e "s|^sha256sums=.*|sha256sums=('$sha')|" \
  -e '/release workflow fills in/d' \
  "$root/packaging/aur/PKGBUILD")"

grep -q "^pkgver=$version$" <<<"$out" || { echo "error: pkgver not set" >&2; exit 1; }
grep -q "^sha256sums=('$sha')$" <<<"$out" || { echo "error: sha256sums not set" >&2; exit 1; }
printf '%s\n' "$out"
