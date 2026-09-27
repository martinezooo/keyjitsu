# Keyjitsu state model and architecture

Keyjitsu has three different kinds of information that must never be conflated:

1. **Oryx baseline**: the layout/revision identified by the keyboard serial and fetched from Oryx.
2. **Verified firmware state**: Keyjitsu-authored edits/custom layers identified by the `~kj<state-id>` marker embedded in firmware and reconstructed from the matching local firmware-state file.
3. **Pending editing state**: changes staged in the UI but not yet confirmed by a reconnecting keyboard.

Profiles, cached layouts, glow settings and UI selection are not device truth.

## Device-state confidence

The runtime derives one of four states. It does not store a separate "synced" boolean.

| State | Meaning | UI/build rule |
| --- | --- | --- |
| `VerifiedFirmware` | Connected firmware has a Keyjitsu state marker and its exact state exists locally. | May be called synced. Pending edits overlay this exact state. |
| `OryxBaseline` | Connected serial identifies an Oryx revision but has no Keyjitsu state marker. | Show as an unverified Oryx baseline. Building is a bootstrap operation and must warn that unknown custom firmware cannot be reconstructed. |
| `MissingFirmwareState` | Firmware contains a Keyjitsu marker but the matching local state is unavailable. | Never substitute Oryx. Render unknown state and block editing/building that could destroy unknown working behavior. |
| `UnknownDeviceIdentity` | Connected firmware identity cannot be parsed as an Oryx layout id. | Render unknown state and block state-dependent editing/building. |
| `OfflineSnapshot` | No device is connected. | Cached data is a snapshot only and must not be presented as current device truth. |

## Key resolution

For a physical `(layer, key)`:

1. If device state is missing/unknown, return an explicit unknown key (`?`), never an Oryx guess.
2. If a verified firmware state exists, apply its dance/edit/custom-layer entry.
3. Otherwise use the Oryx revision as an explicitly unverified baseline.
4. Pending edits/dances are applied only by the editing projection.

A truly empty key is rendered explicitly as `∅`, not as an accidental blank.

## Canonical action pipeline

Raw formats are normalized once:

```
Oryx GraphQL JSON
       |
       v
src/oryx_api.rs
  - null/list/object compatibility
  - singular modifier + modifier masks
  - combo trigger wire-format variants
       |
       v
KeyAction / OryxKey
       |
       +----> src/legend.rs       display labels
       |
       +----> src/key_action.rs   QMK string composition/parsing
       |
       v
src/gui/state.rs
  device/baseline/editing projections
       |
       v
UI modules
```

Rules:

- UI code must not parse raw Oryx JSON.
- UI code must not grow another parser for `LT(...)`, mod-tap wrappers or modified keycodes.
- `src/oryx_api.rs` owns Oryx wire compatibility.
- `src/key_action.rs` owns QMK action-string translation.
- `src/gui/state.rs` owns source precedence and state projections.
- `src/legend.rs` owns human labels.

## GUI modules

The previous single `src/gui/mod.rs` mixed state, device events, build/flash logic and every page. The split is intentional:

- `gui/state.rs`: runtime state projections and persistence-facing state operations
- `gui/live.rs`: Live/Layers editor and picker
- `gui/runtime.rs`: device/background event reduction
- `gui/firmware.rs`: build/flash workflow
- `gui/heatmap_page.rs`: heatmap UI/export
- `gui/fx.rs`: FX Studio
- `gui/peek.rs`: Peek settings/preview
- `gui/settings.rs`: settings/tools/performance/guard/autolayer
- `gui/worker.rs`: device I/O worker
- `gui/widget.rs`: keyboard drawing primitives
- `gui/mod.rs`: application shell, shared data and top-level routing

New feature logic should go into the narrowest module instead of expanding `gui/mod.rs`.

## Firmware identity invariant

A green **firmware synced** status is valid only when:

1. a keyboard is connected,
2. its serial contains a complete Keyjitsu state marker,
3. the matching local firmware-state loads successfully,
4. that state matches the connected layout/revision.

"No pending changes" alone is never proof of synchronization.

## Persistence invariant

- Oryx caches are baseline/reference data.
- `firmware-states/<state-id>.json` is Keyjitsu-authored firmware identity.
- `config.json` staged edits are pending changes.
- profiles are user snapshots and must not overwrite device identity.
- corrupt persistent data must be preserved/reported rather than silently replaced with defaults where doing so could lose state.

## Regression gates

Every change to state/action handling should cover at least:

- Oryx wire-format variants,
- QMK -> internal -> label round-trip,
- verified vs unverified vs missing firmware state,
- pending edits overlaying the correct base,
- reconnect confirmation after flash,
- blank/unknown states being explicit,
- `cargo fmt --check`,
- `cargo clippy --all-targets -- -D warnings`,
- `cargo test`.

The architecture is designed so a UI screenshot should not be required to determine where a key assignment came from.
