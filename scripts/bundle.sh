#!/bin/sh
# Build Keyjitsu.app — a self-contained macOS bundle around the release binary.
# Usage: scripts/bundle.sh [--install]   (--install copies to /Applications)
set -eu

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=target/release/Keyjitsu.app

echo "▸ cargo build --release (v$VERSION)"
cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/keyjitsu "$APP/Contents/MacOS/keyjitsu"
cp resources/Keyjitsu.icns "$APP/Contents/Resources/Keyjitsu.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.keyjitsu.gui</string>
    <key>CFBundleName</key><string>Keyjitsu</string>
    <key>CFBundleDisplayName</key><string>Keyjitsu</string>
    <key>CFBundleExecutable</key><string>keyjitsu</string>
    <key>CFBundleIconFile</key><string>Keyjitsu</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT — keyjitsu contributors</string>
</dict>
</plist>
PLIST

# Ad-hoc signature: keeps Gatekeeper happy on THIS machine (distribution
# to others needs a real Developer ID + notarization).
codesign --force --deep -s - "$APP"

echo "✓ built $APP"
if [ "${1:-}" = "--install" ]; then
    rm -rf /Applications/Keyjitsu.app
    cp -R "$APP" /Applications/Keyjitsu.app
    echo "✓ installed /Applications/Keyjitsu.app"
fi
