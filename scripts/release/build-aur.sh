#!/usr/bin/env bash
# Builds the AUR package for a release, running its check(), and leaves
# PKGBUILD and .SRCINFO in OUT_DIR. With --tarball, makepkg builds from that
# file instead of downloading the GitHub release tarball, so the packaging
# can be tested before the tag exists.
#
# This is meant for an Arch Linux container running as root. makepkg won't
# run as root, so the script creates a "builder" user for it.
#
# Usage: scripts/release/build-aur.sh VERSION SHA256 OUT_DIR [--tarball PATH]
set -euo pipefail

if [[ $# -ne 3 && ! ($# -eq 5 && "$4" == "--tarball") ]]; then
  echo "usage: $0 VERSION SHA256 OUT_DIR [--tarball PATH]" >&2
  exit 2
fi
version="$1"
sha="$2"
out="$3"
tarball="${5:-}"

root="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$out"
"$root/scripts/release/render-pkgbuild.sh" "$version" "$sha" > "$out/PKGBUILD"

# makepkg uses a source file that is already in the build directory under
# the name the PKGBUILD gives it, and checks it against sha256sums.
if [[ -n "$tarball" ]]; then
  cp "$tarball" "$out/deadwax-$version.tar.gz"
fi

id builder >/dev/null 2>&1 || useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
chown -R builder "$out"
cd "$out"
sudo -u builder makepkg --syncdeps --noconfirm --cleanbuild
sudo -u builder sh -c 'makepkg --printsrcinfo > .SRCINFO'
