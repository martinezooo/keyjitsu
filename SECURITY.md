# Security

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private
reporting instead: open the repository's **Security** tab and choose
**Report a vulnerability**. You will get a reply there.

## What keyjitsu touches

- The keyboard, over USB raw HID (read events, set layers and LEDs, flash firmware when you ask).
- The Mac's built-in keyboard, only with the guard on, through the system `hidutil` tool (no root, no kernel extension).
- Two network endpoints, both read-only: `oryx.zsa.io` for your layout, and `api.github.com` for the latest release tag. There is no telemetry.
- Files under `~/Library/Application Support/keyjitsu/` and, with start-at-login on, one LaunchAgent plist in `~/Library/LaunchAgents/`.

## Supported versions

Only the latest release gets fixes.
