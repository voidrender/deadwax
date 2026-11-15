#!/usr/bin/env bash
# Installs, tests and audits the Homebrew formula for a release from a
# throwaway local tap, without publishing anything. With --tarball, the
# formula builds from that file instead of the GitHub release tarball, so
# the packaging can be tested before the tag exists.
#
# This is meant for CI runners. On your own Mac it replaces any installed
# deadwax.
#
# Usage: scripts/release/test-formula.sh VERSION SHA256 [--tarball PATH]
set -euo pipefail

if [[ $# -ne 2 && ! ($# -eq 4 && "$3" == "--tarball") ]]; then
  echo "usage: $0 VERSION SHA256 [--tarball PATH]" >&2
  exit 2
fi
version="$1"
sha="$2"
tarball="${4:-}"

root="$(cd "$(dirname "$0")/../.." && pwd)"
formula="$("$root/scripts/release/render-formula.sh" "$version" "$sha")"

tap="deadwax/release-test"
brew untap --force "$tap" >/dev/null 2>&1 || true
brew tap-new --no-git "$tap" >/dev/null
# Homebrew only loads formulae from taps you have trusted.
brew trust --tap "$tap" >/dev/null
file="$(brew --repository "$tap")/Formula/deadwax.rb"

if [[ -n "$tarball" ]]; then
  [[ -f "$tarball" ]] || { echo "error: no tarball at $tarball" >&2; exit 1; }
  abs="$(cd "$(dirname "$tarball")" && pwd)/$(basename "$tarball")"
  # shellcheck disable=SC2001 # Anchored to the start of one line of many.
  sed "s|^  url \".*\"|  url \"file://$abs\"|" <<<"$formula" > "$file"
  grep -q "^  url \"file://$abs\"" "$file" || { echo "error: url not replaced" >&2; exit 1; }
else
  printf '%s\n' "$formula" > "$file"
fi

brew install --build-from-source "$tap/deadwax"
brew test "$tap/deadwax"

# The audit reads the formula as it will be published, with the release url.
printf '%s\n' "$formula" > "$file"
brew audit --strict "$tap/deadwax"
