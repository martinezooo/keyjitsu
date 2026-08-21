# Changelog

All notable changes to Keyjitsu are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Update check: keyjitsu asks GitHub for the latest release tag once at
  startup (a toggle in Settings turns this off) and on demand via **Check for
  updates**. A newer version shows as a small badge at the bottom of the
  sidebar and a link in Settings. Nothing is downloaded or installed by itself.
- The sidebar's bottom shows what is on right now: the keyboard guard and the
  autolayer, next to the optional CPU pill.

### Changed
- The **Shortcuts** tab is gone from the main menu. It was a reference list of
  common shortcuts, not the keys on your keyboard, and read as if it were. It
  now lives in **Settings** as a collapsible **Shortcut library** card.
- Sidebar polish: rows are painted with left-aligned labels (sub-items used to
  drift right because buttons centre their text), experimental tabs carry a
  small "exp" badge instead of it being part of the name, and the connection
  pill says "No keyboard" with the full hint on hover instead of truncating.

## [0.9.1] - 2026-08-21

### Added
- The keyboard guard can no longer lock you out: the built-in keyboard is
  automatically re-enabled around the **lock screen** (the login window is
  always usable) and re-disabled on unlock, via the system's screen
  lock/unlock notifications.
- Staged per-key remaps and tap dances now **persist across restarts** (saved
  per layout, shown as pending) instead of living only in memory until the
  next build.
- With no keyboard attached, Live shows the **last layout from the on-disk
  cache** (never touching the network), so you can view and plan your layout
  with the board unplugged. On a first run with nothing cached, a short
  plug-in hint replaces the old wall of blank keys.

### Fixed
- A transparent vertical strip could appear between the sidebar and the rest
  of the window while no keyboard was connected: the long connection hint
  inflated the sidebar panel beyond its fixed width. The pill now truncates
  (full text on hover).
- All tabs now share one panel colour, so the Live tab no longer shows a
  darker band next to the sidebar.

### Changed
- Two internal polish passes: de-duplicated helpers (hex parsing, board
  extent, glow conversion, the layer-switch family list), magic numbers turned
  into named constants (including a peek-sizing value that had drifted),
  dead-code removal, and guard-code cleanup, with no behaviour changes.

## [0.9.0] - 2026-07-09

First public beta. Feature-complete for the ZSA Voyager on macOS. The API and
config format may still change before 1.0.

### Added
- **GUI** (`keyjitsu` with no args): left-sidebar app with Live, Layers,
  Heatmap, Peek, FX Studio, Shortcuts, Performance, Autolayer and Settings.
- **Live editor**: per-key slot rows (tap / hold / double-tap / tap+hold),
  Oryx-style keycode picker (layer options shown by **name**), per-key glow
  color and on-press effect, then **Build & flash** with a progress modal
  (phase, progress bar, readable collapsible logs).
- **Local firmware builds**: 100% offline, no ZSA login. Fetches your Oryx
  source once (cached), patches the keymap, **generates QMK tap dances** for
  double-tap / tap+hold (with a comfortable 200 ms tapping term), compiles
  with `qmk`, flashes over the bootloader.
- **Author layers in-app** (no Oryx): add / rename / delete custom layers,
  fill their keys, reach them with a switch key, built into a fresh
  `LAYOUT` block locally.
- **Profiles**: snapshot the whole configuration and switch between setups
  losslessly (sidebar).
- **Heatmap** (per-layer / summed, ranking, CSV export), **Peek** layer
  minimap (transparent, click-through, monitor-positionable, key- or
  chord-bound, optional live combo readout with gesture + timing measurement),
  **FX Studio** RGB effects incl. a custom step sequencer, **Shortcuts**
  cheatsheet (~160 entries, categories, your own + hideable built-ins),
  **Autolayer** (switch by frontmost app), **Performance** self-CPU sampler.
- **Keyboard guard**: disable the Mac's built-in keyboard while the Voyager is
  connected, with a three-layer restore (Drop / signal handler / startup
  self-heal) so it can never be left disabled.
- **CLI**: `list`, `status`, `live`, `layout`, `watch`, `heatmap`, `layer`,
  `rgb`, `status-led`, `brightness`, `build-local` (`--set`, `--dance`,
  `--new-layer`), `flash`, `guard`, `overlay`, `autolayer`.
- **Keyjitsu.app** bundle (`scripts/bundle.sh`) with an icon.

### Hardened (pre-release review)
- Config is written atomically and an unreadable config is backed up rather
  than silently wiped.
- Guard restore only clears its safety marker on success. The CLI guard
  self-heals a stale marker on startup.
- Firmware feature detection (MOUSEKEY/EXTRAKEY) scans layers and tap dances,
  not just edits. Tap-dance layer actions (LT/TT/DF) emit real layer ops, and a
  corrupt/truncated source download can no longer poison the cache.
- RGB frames are coalesced at the single HID write point so a slow write
  can't back up the command queue. LEDs and the guard are released on quit.
- Deleting a custom layer renumbers every layer-switch reference (MO/TO/TG/
  TT/OSL/DF/LT) and per-key override that pointed above it, so no key is
  left pointing at the wrong (or a vanished) layer.
- The bootloader watcher spawned during a flash is given its own timeout so
  a canceled or timed-out flash can't leak a thread that waits forever.

### Known limitations
- macOS is the primary target (guard / autolayer / peek are macOS-only).
- The `.app` is ad-hoc signed. Distributing to other machines needs a
  Developer ID signature + notarization.
- Tap dances aren't yet supported on user-authored layers.
- A truly wedged HID write can't be timed out (hidapi limitation).
