# Security

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private
reporting instead: open the repository's **Security** tab and choose
**Report a vulnerability**. You will get a reply there.

## What keyjitsu touches

- The keyboard, over USB raw HID (read events, set layers and LEDs, flash firmware when you ask).
- The Mac's built-in keyboard, only with the guard on, through the system `hidutil` tool (no root, no kernel extension).
- Network: `oryx.zsa.io` for layouts; `api.github.com` for release metadata.
  With explicit confirmation, the macOS updater downloads signed releases
  from `github.com` / GitHub's asset CDN. It verifies the SHA-256 digest,
  expected Developer ID and Team ID, bundle ID and Apple's notarization
  before installing. There is no telemetry.
- Files under `~/Library/Application Support/keyjitsu/`, temporary update
  staging files, and (if enabled) a LaunchAgent in `~/Library/LaunchAgents/`.
  The updater replaces only the app bundle, not user data.

## Supported versions

Only the latest release gets fixes.
