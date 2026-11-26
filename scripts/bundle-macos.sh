#!/usr/bin/env bash
# Wrap the release binary in "Better Basic TTS.app".
#
# A real .app (rather than a bare binary started from a terminal) matters on
# macOS: the Accessibility permission needed for "speak selection" is granted
# per application, and the bundle gives it a stable identity.
#
# Usage: cargo build --release -p better-basic-tts && scripts/bundle-macos.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
binary="$root/target/release/better-basic-tts"
app="$root/target/release/Better Basic TTS.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

[ -x "$binary" ] || { echo "build first: cargo build --release -p better-basic-tts" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/better-basic-tts"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Better Basic TTS</string>
  <key>CFBundleDisplayName</key><string>Better Basic TTS</string>
  <key>CFBundleIdentifier</key><string>io.github.better-basic-tts</string>
  <key>CFBundleExecutable</key><string>better-basic-tts</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppleEventsUsageDescription</key><string>Better Basic TTS reads the text you select in other applications.</string>
</dict>
</plist>
PLIST

# Ad-hoc signature so macOS keeps the Accessibility grant between launches.
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || echo "warning: codesign failed; the app is unsigned" >&2

(cd "$root/target/release" && rm -f Better-Basic-TTS-macos.zip && ditto -c -k --keepParent "Better Basic TTS.app" Better-Basic-TTS-macos.zip)
echo "Created $app"
