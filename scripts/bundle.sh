#!/bin/sh
# Build Keyjitsu.app - a self-contained macOS bundle around the release binary.
# Usage: scripts/bundle.sh [--install]   (--install copies to /Applications)
set -eu

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# Apple's CFBundleShortVersionString must be a three-component numeric version.
SHORT_VERSION=${VERSION%%-*}
case "$VERSION" in
    *-beta.*) BUILD_VERSION=$((10000 + ${VERSION##*.})) ;;
    *-rc.*)   BUILD_VERSION=$((20000 + ${VERSION##*.})) ;;
    *)        BUILD_VERSION=30000 ;;
esac
APP=target/release/Keyjitsu.app

echo "▸ cargo build --release --locked (v$VERSION)"
cargo build --release --locked

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
    <key>CFBundleShortVersionString</key><string>$SHORT_VERSION</string>
    <key>CFBundleVersion</key><string>$BUILD_VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT - keyjitsu contributors</string>
</dict>
</plist>
PLIST

# Release signing is opt-in, because distributing a Developer ID signed app
# without notarization is not a valid public release. CI's legacy build remains
# ad hoc until the dedicated notarization release job is configured.
if [ -n "${KEYJITSU_CODESIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$KEYJITSU_CODESIGN_IDENTITY" "$APP"
else
    codesign --force --deep -s - "$APP"
fi

echo "✓ built $APP"
if [ "${1:-}" = "--install" ]; then
    rm -rf /Applications/Keyjitsu.app
    cp -R "$APP" /Applications/Keyjitsu.app
    echo "✓ installed /Applications/Keyjitsu.app"
fi
