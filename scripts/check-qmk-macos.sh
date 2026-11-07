#!/usr/bin/env bash
# Read-only macOS QMK diagnostics. Does not install packages or change firmware.
set -uo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This check is for macOS." >&2
  exit 2
fi

missing=0
find_tool() {
  local name="$1" dir
  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return 0
  fi
  for dir in "$HOME/.local/bin" /opt/homebrew/bin /usr/local/bin; do
    if [[ -x "$dir/$name" ]]; then
      printf '%s\n' "$dir/$name"
      return 0
    fi
  done
  return 1
}

printf 'Keyjitsu / QMK prerequisites on %s (%s)\n\n' "$(sw_vers -productVersion)" "$(uname -m)"

if qmk="$(find_tool qmk)"; then
  printf 'QMK CLI: %s\n' "$qmk"
  "$qmk" --version | head -1
else
  echo 'QMK CLI: missing (install with brew install pipx && pipx install qmk)'
  missing=1
fi

if arm="$(find_tool arm-none-eabi-gcc)"; then
  printf 'ARM compiler: %s\n' "$arm"
  "$arm" --version | head -1
else
  echo 'ARM compiler: missing (see docs/QMK_MACOS.md)'
  missing=1
fi

firmware="${QMK_HOME:-$HOME/qmk_firmware}"
if [[ -d "$firmware/keyboards/zsa/voyager" && -f "$firmware/Makefile" ]]; then
  printf 'Firmware checkout: %s\n' "$firmware"
  if command -v git >/dev/null 2>&1; then
    printf 'Firmware branch: '
    git -C "$firmware" branch --show-current 2>/dev/null || true
    printf 'Firmware origin: '
    git -C "$firmware" remote get-url origin 2>/dev/null || true
  fi
else
  printf 'Firmware checkout: missing ZSA Voyager tree in %s\n' "$firmware"
  missing=1
fi

if (( missing == 0 )); then
  echo
  echo 'Core prerequisites found. Run qmk doctor and a firmware build to verify the full toolchain.'
else
  echo
  echo 'Some prerequisites are missing. Follow docs/QMK_MACOS.md.'
fi
exit "$missing"
