#!/usr/bin/env bash
# Builds, signs, and installs the Senritsu notification helper app.
# Usage: scripts/install-macos-notifier.sh [--build-only]
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: the notifier helper is macOS only" >&2
  exit 1
fi

build_only=0
case "${1:-}" in
  "") ;;
  --build-only) build_only=1 ;;
  *) echo "usage: $0 [--build-only]" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/macos/SenritsuNotifier"
out="$root/target/notifier"
app="$out/SenritsuNotifier.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

swiftc -O "$src/main.swift" -o "$app/Contents/MacOS/SenritsuNotifier" \
  -framework AppKit -framework UserNotifications
cp "$src/Info.plist" "$app/Contents/Info.plist"

cp "$src/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"

codesign --force --deep -s - "$app"
echo "Built $app"

if [[ "$build_only" -eq 1 ]]; then
  exit 0
fi

dest="$HOME/Applications"
mkdir -p "$dest"
rm -rf "$dest/SenritsuNotifier.app"
cp -R "$app" "$dest/"
echo "Installed to $dest/SenritsuNotifier.app"

# First run makes macOS ask for notification permission.
open -n -a "$dest/SenritsuNotifier.app" --args --title Senritsu --body "Notifications enabled"
