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

iconset="$out/AppIcon.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
swiftc -O "$root/scripts/make-icon.swift" -o "$out/make-icon"
"$out/make-icon" "$out/icon-1024.png"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$out/icon-1024.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z "$((s * 2))" "$((s * 2))" "$out/icon-1024.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

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
