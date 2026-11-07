#!/usr/bin/env bash
# Build, Developer-ID sign, notarize, staple, and optionally publish the macOS
# Apple Silicon release. Run from the logged-in Mac session with an unlocked
# Keychain. No credentials are stored in this repository.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
SHA="$(git rev-parse --short=7 HEAD)"
TAG="signed-macos-v${VERSION}-${SHA}"
NAME="Keyjitsu-v${VERSION}-${SHA}-macos-arm64"
DIST="$ROOT/target/signed"
APP="$ROOT/target/release/Keyjitsu.app"
PROFILE="${KEYJITSU_NOTARY_PROFILE:-keyjitsu-notary}"
IDENTITY="${KEYJITSU_CODESIGN_IDENTITY:?set KEYJITSU_CODESIGN_IDENTITY to the Developer ID fingerprint}"
PUBLISH=0
if [[ "${1:-}" == "--publish" ]]; then
  PUBLISH=1
elif [[ -n "${1:-}" && "${1:-}" != "--no-publish" ]]; then
  echo "usage: $0 [--publish | --no-publish]" >&2
  exit 2
fi

[[ "$(uname -m)" == "arm64" ]] || { echo "an arm64 Mac is required" >&2; exit 2; }
[[ -z "$(git status --porcelain)" ]] || { echo "git worktree must be clean" >&2; exit 2; }
[[ "$(git rev-parse HEAD)" == "$(git ls-remote origin refs/heads/master | cut -f1)" ]] || {
  echo "local HEAD differs from GitHub master; refusing release" >&2; exit 2;
}
security find-identity -v -p codesigning | grep -Fq "$IDENTITY" || {
  echo "selected Developer ID private key is not available" >&2; exit 2;
}
xcrun notarytool history --keychain-profile "$PROFILE" --output-format json >/dev/null

echo "==> build Keyjitsu $VERSION ($SHA)"
KEYJITSU_CODESIGN_IDENTITY="$IDENTITY" scripts/bundle.sh
codesign --verify --deep --strict "$APP"
DETAILS="$(codesign -dvvv "$APP" 2>&1)"
grep -Fq "Authority=Developer ID Application: Marcin Bortkiewicz (TVQDVS8HK4)" <<<"$DETAILS"
grep -Fq "TeamIdentifier=TVQDVS8HK4" <<<"$DETAILS"
grep -Fq "flags=0x10000(runtime)" <<<"$DETAILS"
grep -Fq "Timestamp=" <<<"$DETAILS"

mkdir -p "$DIST"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/keyjitsu-release.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

echo "==> Apple notarization: app"
ditto -c -k --keepParent "$APP" "$WORK/Keyjitsu-submit.zip"
xcrun notarytool submit "$WORK/Keyjitsu-submit.zip" --keychain-profile "$PROFILE"   --wait --timeout 30m --output-format json > "$DIST/$NAME-app-notary.json"
python3 - "$DIST/$NAME-app-notary.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
assert data.get("status") == "Accepted", data
print("Apple accepted app submission", data["id"])
PY
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
codesign --verify --deep --strict "$APP"
spctl --assess --type execute --verbose=3 "$APP"
ditto -c -k --keepParent "$APP" "$DIST/$NAME.zip"

echo "==> Apple notarization: installer DMG"
mkdir -p "$WORK/dmg"
ditto "$APP" "$WORK/dmg/Keyjitsu.app"
ln -s /Applications "$WORK/dmg/Applications"
hdiutil create -volname Keyjitsu -srcfolder "$WORK/dmg" -format UDZO   -ov "$DIST/$NAME.dmg"
codesign --force --timestamp --sign "$IDENTITY" "$DIST/$NAME.dmg"
xcrun notarytool submit "$DIST/$NAME.dmg" --keychain-profile "$PROFILE"   --wait --timeout 30m --output-format json > "$DIST/$NAME-dmg-notary.json"
python3 - "$DIST/$NAME-dmg-notary.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
assert data.get("status") == "Accepted", data
print("Apple accepted DMG submission", data["id"])
PY
xcrun stapler staple "$DIST/$NAME.dmg"
xcrun stapler validate "$DIST/$NAME.dmg"
codesign --verify "$DIST/$NAME.dmg"
spctl --assess --type open --context context:primary-signature --verbose=3 "$DIST/$NAME.dmg"

echo "==> verify downloaded update ZIP contents and checksums"
mkdir -p "$WORK/verify"
ditto -x -k "$DIST/$NAME.zip" "$WORK/verify"
xcrun stapler validate "$WORK/verify/Keyjitsu.app"
codesign --verify --deep --strict "$WORK/verify/Keyjitsu.app"
( cd "$DIST" && shasum -a 256 "$NAME.zip" "$NAME.dmg" > "$NAME-SHA256SUMS.txt" )
echo "==> signed, notarized, stapled: $DIST/$NAME"

if [[ "$PUBLISH" == 1 ]]; then
  echo "==> publish $TAG"
  if git ls-remote --tags origin "refs/tags/$TAG" | grep -q .; then
    echo "tag already exists; refusing to overwrite an existing release" >&2
    exit 2
  fi
  git tag -a "$TAG" -m "Signed notarized Keyjitsu $VERSION ($SHA)"
  git push origin "refs/tags/$TAG"
  cat > "$WORK/release-notes.md" <<EOF
## Keyjitsu $VERSION — signed macOS release

Apple Silicon (arm64). Developer ID Application (Team TVQDVS8HK4),
Apple-notarized and stapled app and installer DMG.

Includes the in-app update checker, SHA-256 and Developer ID verification,
and an explicitly confirmed update and restart flow. macOS only.

Source commit: `$(git rev-parse HEAD)`.
Checksums: `$NAME-SHA256SUMS.txt`.
EOF
  gh release create "$TAG" --repo martinezooo/keyjitsu --verify-tag --prerelease     --title "Keyjitsu $VERSION — Signed macOS"     --notes-file "$WORK/release-notes.md"     "$DIST/$NAME.zip" "$DIST/$NAME.dmg" "$DIST/$NAME-SHA256SUMS.txt"
  gh release view "$TAG" --repo martinezooo/keyjitsu --json tagName,assets
fi
