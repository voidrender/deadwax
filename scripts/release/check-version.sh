#!/usr/bin/env bash
# Checks that a release tag matches the version in Cargo.toml and prints the
# version without the leading "v". With no tag, prints the version in
# Cargo.toml.
#
# Usage: scripts/release/check-version.sh [vX.Y.Z]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
crate="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -1)"
tag="${1:-v$crate}"

if [[ "$tag" != "v$crate" ]]; then
  echo "error: tag $tag doesn't match version $crate in Cargo.toml" >&2
  exit 1
fi
echo "$crate"
