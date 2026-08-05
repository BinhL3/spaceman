# Spaceman Contributor Guide

This file is the current engineering contract. README and `docs/` describe current user behavior;
`PLAN.md` and `docs/RESEARCH.md` are historical records.

## Architecture Map

- `src/bin/spaceman-agent.rs`, `src/app.rs`: GPUI/AppKit foreground lifecycle, action queue, gestures,
  status, restore-before-exit coordination, and the worker boundary.
- `src/status_item.rs`, `src/settings.rs`, `src/sparkle.rs`, `src/termination.rs`: main-thread menu,
  settings, signed updates, and asynchronous AppKit termination coordination.
- `src/hotkeys.rs`, `src/keyboard_layout.rs`: Carbon global shortcut adapters.
- `src/macos.rs`: Accessibility/Core Graphics observation and writes, display discovery, native event
  monitor, and native handle cache.
- `src/observation.rs`, `src/model.rs`, `src/reconcile.rs`: immutable observations, stable logical slots,
  and conservative physical-window reconciliation.
- `src/workspace.rs`, `src/runtime.rs`, `src/layout_tree.rs`, `src/navigation.rs`: ten workspaces,
  ownership, nested weighted layouts, directional operations, and persistence representation.
- `src/config.rs`: strict version-1 TOML parsing and first-match window disposition.
- `src/control.rs`, `src/main.rs`: mode-`0600` local Unix control protocol, CLI, dry-run/live probes,
  fixture replay, and HUD.
- `src/geometry.rs`, `src/live.rs`: frame math, parking, and legacy live reconciliation helpers.
- `scripts/`: signed bundle assembly/install and destructive live acceptance harnesses.

## Product Invariants

- A logical slot is product identity; a physical `AXWindow` binding is replaceable evidence.
- Native-tab replacement preserves slot, order, weight, nested layout, workspace, and maximize intent.
- Incomplete, timed-out, or ambiguous observations never prove absence. Preserve state and retry.
- One accepted observation produces one coherent model revision; native writes converge toward it.
- Rules are ordered and first-match. Do not silently introduce planned named app spaces or an
  orthogonal rule schema into current config.
- Automatic tiling starts when hotkeys are available. Documentation must not claim a paused startup.

## Recovery And Persistence Invariants

- Persist a window's original frame before moving it off-screen.
- Remove parked metadata only after restoration succeeds and the removal is durably persisted. Roll
  metadata back in memory if persistence fails.
- Pause, quit, restart-for-install, hotkey failure, and worker failure must favor restoration. Never
  recommend deleting `parked-windows.json` as the first recovery step.
- `workspace-state.json` must validate before restoration. Invalid config/state may be ignored so
  parked recovery can proceed; malformed parked state must not be silently discarded.
- Persist workspace/layout mutations atomically. Keep backward migration only for actual persisted
  formats already emitted.
- Never change state paths or schemas without migration/recovery tests and synchronized docs.

## Privacy And Security Invariants

- Accessibility is used only for window observation, focus, movement, resizing, and required input/
  display context. Never require SIP to be disabled.
- Window titles may be used for matching in memory but must never enter persistence, serialized
  snapshots, debug output, HUD output, fixtures, logs, or control responses.
- No telemetry, network access, or update checks without explicit product and security review.
- Keep `control.sock` and `control.lock` at mode `0600`, bound under the user's Application Support
  directory, with bounded requests and single-agent locking.
- Treat `_AXUIElementGetWindow` as private, optional, dynamically resolved compatibility risk. Do not
  expand private API usage casually or claim Mac App Store support.

## Thread And FFI Invariants

- AppKit/status UI and APIs requiring it stay on the main thread.
- Accessibility observer run-loop objects stay on the dedicated AX event thread.
- Window-management work and its thread-local AX handle cache stay on the worker thread.
- Native handles (`AXUIElement`, `AXObserver`, Core Foundation run-loop values, Objective-C objects)
  never enter logical models, persisted state, or cross-thread messages.
- Native callbacks only invalidate/request observation; they do not mutate the logical model.
- Keep unsafe FFI narrow, typed, dynamically checked where applicable, and covered by safe Rust
  boundaries. Do not relax crate-wide unsafe policy to simplify an adapter.

## Live-Test Safety

- Disable AeroSpace and every other window manager before directly launching a process that applies
  frames. The repository live harnesses instead require AeroSpace initially, capture its baseline, and
  own the disable/restore transition.
- Prefer fixture replay and dry-run commands. Use `watch-window` or disposable Ghostty processes for
  scoped live tests.
- Read live harnesses before running them. Confirm expected displays, preserve baseline workspace and
  focus, and verify cleanup. Never experiment on unrelated user windows.
- Preserve `~/Library/Application Support/Spaceman/parked-windows.json` until pause/quit restoration is
  confirmed. If a test crashes, reopen the same app and recover before cleaning state.

## Verification

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo run --locked -- replay fixtures/ghostty-tab-replacement.json
cargo run --locked -- config-check config.example.toml
git diff --check
```

Run `./scripts/live-ghostty-tabs.sh` and `./scripts/live-workspaces.sh` only when the change reaches
their live behavior, AeroSpace is initially enabled, and the other safety conditions above are
satisfied.

## Documentation Synchronization

Update README and linked docs in the same change when menu labels, shortcuts, CLI commands, config
schema, environment variables, state paths/schema, privacy output, supported windows/displays,
packaging, startup, restoration, or uninstall behavior changes. Verify every command and label against
source. Keep historical documents visibly marked and never rewrite them as current truth.

## Release Rules

- A Developer ID-signed source build is not a public release artifact.
- Do not claim release readiness without notarization/stapling, a downloadable checksummed artifact,
  architecture and minimum-OS validation, and clean-machine install/upgrade/recovery/uninstall tests.
- Build once from a clean reviewed revision, notarize and distribute that exact artifact, and verify
  signature, ticket, architecture, plist, archive contents, and checksum after packaging.
- Add a real app icon and demo before public presentation if they remain absent.
- Never tell artifact users they need a Developer ID identity; that is a source-build requirement.
- Never ship instructions to disable SIP, erase recovery metadata before restoration, or imply Mac App
  Store support.
