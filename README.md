# Keyjitsu ⌨️🥋

Voyager management, a local app. A GUI and CLI manager for the
[ZSA Voyager](https://www.zsa.io/voyager).

It aims for Keymapp feature parity and adds what Keymapp doesn't do: 100% local
firmware builds (with generated tap dances), per-key RGB and press effects, a
custom-effect sequencer, an app-aware autolayer, a Karabiner-style
built-in-keyboard guard, and a transparent layer minimap.

No daemon, no login, no cloud. Everything talks straight to the keyboard over
raw HID. The only network use is an anonymous read of your Oryx layout.

> Quit Keymapp before running keyjitsu. The raw HID channel is exclusive.

## Status

**Beta.** Only tested on **macOS** with the **ZSA Voyager**. Other operating
systems and other ZSA boards are unsupported for now: the code is portable
Rust, but nothing else has been verified, so treat it as "may or may not
work". The config format can still change before 1.0.

## Screenshots

The **Live** editor: pick a key, set its keycode, glow color, and on-press effect.

![Live key editor](docs/live.png)

The **Heatmap**: press counts across the board, with a ranking and CSV export.

![Heatmap](docs/heatmap.png)

The **Peek** overlay: a see-through minimap of the current layer that floats,
click-through, over whatever you are doing (here, a media and navigation layer).

![Peek layer overlay](docs/peek.png)

## What's in the app

Run `keyjitsu` with no arguments for the windowed app. The left sidebar is the
menu, and the active tab expands its sub-items below it. Profiles sit at the
top: snapshot the whole setup under a name and switch between them without
losing anything.

- **Live.** Your layout with real Oryx legends, keys lighting up as you type.
  Click a key to edit its four slots (tap, hold, double tap, tap+hold), pick
  keycodes from an Oryx-style picker, set a per-key glow color and an on-press
  effect, then hit Build & flash. Firmware is compiled locally, and double-tap
  or tap+hold become generated QMK tap dances. Staged edits survive restarts
  (shown as pending) until you build.
- **Heatmap.** Per-layer or summed press counts, a ranking, and CSV export.
- **Peek.** A small, see-through overlay that shows the current layer's keys,
  so you can glance at what a layer does without leaving what you are doing. It
  appears when you switch layers (and can stay up the whole time you are on a
  non-base layer), floats click-through over everything on any monitor, and can
  be summoned by holding a key or chord on the Voyager.
- **FX Studio** (experimental). Build and test RGB effects: built-in constant
  and press effects, plus a step sequencer for your own (paint keys, duplicate
  a step, nudge it around the board, loop). Apply to the whole board, or per
  key from the editor's on-press slot. Board RGB survives restarts.
- **Shortcuts.** A searchable cheatsheet of ~160 shortcuts (macOS, editors,
  terminals, pentest tools, QMK patterns), with categories, your own entries,
  and hideable built-ins.
- **Performance** (experimental). keyjitsu samples its own CPU, tagged by what
  it was doing.
- **Autolayer.** Switch layers by the frontmost app, matched on the bundle id.
- **Settings.** Local QMK toolchain status and firmware build, the keyboard
  guard, and start-at-login.

The connection auto-reconnects on unplug, replug, and after flashing.

## Compared to Keymapp

ZSA gives you two separate tools: **[Oryx](https://configure.zsa.io)**, a website
where you design your layout and it builds the firmware in the cloud, and
**[Keymapp](https://blog.zsa.io/keymapp/)**, a desktop app that flashes that
firmware and shows a live layout reference. Keymapp itself does not edit or build,
that is Oryx's job. keyjitsu is one desktop app that edits, builds, flashes, and
shows your layout, all locally on macOS. This table compares the two desktop apps.

| Feature | Keymapp | keyjitsu |
| --- | :---: | :---: |
| Remap keys in the app | ❌ (only in Oryx, the website) | ✅ locally |
| Build firmware | ❌ (Oryx builds it in the cloud) | ✅ 100% local, no login |
| Flash firmware | ✅ (Oryx-built firmware, fetched from the cloud) | ✅ (its own local build) |
| Tap dances (double-tap, tap+hold) | ❌ (in Oryx) | ✅ generated locally |
| Per-key RGB | ❌ (in Oryx) | ✅ in the app |
| Custom RGB effect sequencer | ❌ | ✅ FX Studio |
| Heatmap | ✅ per layer | ✅ + summed, ranking, CSV |
| Live layer minimap | ✅ a plain always-on-top window | ✅ **Peek** |
| Minimap: transparent + click-through | ❌ (cannot be made see-through) | ✅ opacity slider, ignores the mouse |
| Minimap: position and triggers | drag the window | ✅ per-monitor anchor, shown on a key or chord, auto-hide, monochrome, combo and timing readout |
| ⭐ **Rest the Voyager on top of the MacBook keyboard** (built-in ignored, no ghost presses) | ❌ | ✅ guard, cannot lock you out |
| Autolayer (switch layers by app) | ❌ | ✅ |
| CLI / scripting | Zapp CLI + API | ✅ built-in CLI |
| Platforms | ✅ Windows, macOS, Linux | macOS only (tested) |
| All ZSA boards | ✅ | Voyager only (tested) |
| Signed download + support | ✅ | ❌ beta, build from source |

## CLI

Every GUI feature also has a command. A few of them:

| Command | What it does |
| --- | --- |
| `keyjitsu list` | List connected ZSA keyboards |
| `keyjitsu status` | Connection, protocol/firmware version, active layer |
| `keyjitsu live` | Full-screen TUI live view (legends, heatmap overlay, layer browsing) |
| `keyjitsu layout` | Print every layer with legends (`--layer N`, `--json`, `--url`, `--refresh`) |
| `keyjitsu watch` | Stream key/layer events (`--json` for scripting) |
| `keyjitsu heatmap show/reset` | Render or clear collected stats |
| `keyjitsu layer set/unset N` | Switch layers from scripts |
| `keyjitsu rgb set/all/release` | Per-key or whole-board RGB |
| `keyjitsu build-local` | Compile firmware locally (`--set "L,POS=KC_X"`, `--dance "L,POS=TAP,HOLD,DOUBLE,TAPHOLD"`) |
| `keyjitsu flash <file\|url>` | Flash a `.bin` or an Oryx URL (`--latest` for the newest revision) |
| `keyjitsu guard` | Disable the built-in Mac keyboard while a ZSA board is connected (Ctrl+C restores) |

Add `--serial <substr>` to pick one of several keyboards.

## Local firmware builds

One-time setup (about 2 GB, plus ARM GCC via Homebrew):

```sh
pipx install qmk
qmk setup zsa/qmk_firmware -b firmware25
```

Settings shows a green "Ready" pill once the toolchain is in place. A build
fetches your layout's generated source from Oryx once (cached, offline
afterwards), patches the keymap with your staged edits, generates any tap
dances, compiles with `qmk`, and flashes over the Voyager's bootloader. No ZSA
account needed.

Your edits live in that local firmware, not in Oryx. keyjitsu only reads your
layout from Oryx (anonymously) for the legends. It never writes back to the
portal. So remapping here does not carry over to Oryx, and re-flashing from
Oryx later would overwrite your keyjitsu changes.

## How it works

- **Protocol.** ZSA's open Oryx raw-HID protocol v4 (32-byte reports, usage
  page `0xFF60`), as published in [zsa/qmk_modules](https://github.com/zsa/qmk_modules).
- **Legends.** Your layout is identified by the id in the keyboard's USB serial
  (`hash/revision`), fetched from the Oryx GraphQL API, then cached under
  `~/Library/Application Support/keyjitsu/`.
- **Flashing.** Uses ZSA's own open-source [zapp](https://github.com/zsa/zapp)
  (`zapp-core`, MIT + Commons Clause) for the DFU/Ignition bootloaders and dual
  STM32+GD32 images. You press the reset button, then keyjitsu waits and flashes.
- **Guard.** Remaps the built-in keyboard's keys to no-ops with `hidutil` (no
  special permission needed). It can't lock you out: the built-in comes back
  around the lock screen and after any reboot, and is restored on toggle-off,
  disconnect, and quit. Engaged only while a ZSA keyboard is present.
- **Storage.** Everything lives under `~/Library/Application Support/keyjitsu/`:
  `config.json` (all settings), `profiles/*.json` (named snapshots), heatmap
  stats, and cached layouts and sources.

## Install

There is no signed download yet, since it is a beta (a shippable `.app` would
need Apple notarization). You build it yourself. You need [Rust](https://rustup.rs).
The QMK toolchain is only needed later, for local firmware builds (see above).

Quick way, if Rust is on your PATH:

```sh
cargo install --git https://github.com/martinezooo/keyjitsu
```

That drops a `keyjitsu` binary in `~/.cargo/bin`. Run `keyjitsu` for the app, or
`keyjitsu list` for the CLI.

For the full macOS app bundle (dock icon, launch-at-login), build from a clone:

```sh
git clone https://github.com/martinezooo/keyjitsu
cd keyjitsu
scripts/bundle.sh --install   # builds release, installs Keyjitsu.app to /Applications
```

Plain `cargo build --release` also works (binary in `target/release/keyjitsu`),
and `cargo test` runs the suite.

macOS is the primary and only tested target. The guard, autolayer, and peek are
macOS-only. The rest is portable Rust (hidapi with egui/ratatui) but unverified
elsewhere.

## Acknowledgements

Keyjitsu stands on open work from ZSA and the QMK community, and on the Rust
ecosystem. It does not reimplement what those projects already do well.

- **ZSA.** The Voyager itself, the open Oryx raw-HID protocol
  ([zsa/qmk_modules](https://github.com/zsa/qmk_modules)) that keyjitsu speaks,
  the public read-only Oryx layout API it reads anonymously, and the open-source
  [zapp](https://github.com/zsa/zapp) flasher (`zapp-core`), which keyjitsu links
  for the actual firmware flashing rather than rolling its own.
- **QMK.** Local builds use the standard [QMK](https://qmk.fm) toolchain and
  ZSA's `qmk_firmware` fork. The generated tap dances follow QMK's own idiom.
- **Rust crates.** [eframe / egui](https://github.com/emilk/egui) for the GUI,
  [ratatui](https://github.com/ratatui/ratatui) and crossterm for the TUI,
  [hidapi](https://crates.io/crates/hidapi) for USB HID, plus serde, ureq,
  clap, anyhow, zip, ctrlc, directories, and objc2 / core-foundation on macOS.
  See `Cargo.toml` for the full list and versions.

The protocol is implemented from ZSA's public spec, and layout data comes
through the public Oryx API. Keyjitsu is an independent project, not affiliated
with or endorsed by ZSA.

## License

MIT (see [LICENSE](LICENSE)). Firmware flashing links ZSA's `zapp-core`, which
is MIT + Commons Clause (no reselling its functionality), so keep that in mind
if you redistribute. The bundled symbol font is Noto Sans Symbols 2
(SIL OFL 1.1).
