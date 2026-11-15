#!/usr/bin/env bash
# Prints the Homebrew formula for a release, built from
# packaging/homebrew/deadwax.rb with the release's url and checksum.
#
# Usage: scripts/release/render-formula.sh VERSION SHA256
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
  -e "s|/archive/refs/tags/v[^\"]*\.tar\.gz\"|/archive/refs/tags/v$version.tar.gz\"|" \
  -e "s|^  sha256 \"[0-9a-f]*\"|  sha256 \"$sha\"|" \
  -e '/release workflow fills in/d' \
  "$root/packaging/homebrew/deadwax.rb")"

# Fail loudly if the template changed shape and a substitution missed.
grep -q "/archive/refs/tags/v$version.tar.gz\"" <<<"$out" || { echo "error: url not set" >&2; exit 1; }
grep -q "^  sha256 \"$sha\"" <<<"$out" || { echo "error: sha256 not set" >&2; exit 1; }
printf '%s\n' "$out"
