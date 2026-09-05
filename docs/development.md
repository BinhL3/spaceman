# Development

Read the root [AGENTS.md](../AGENTS.md) before changing implementation. It defines architecture and
safety invariants. [PLAN.md](../PLAN.md) and [RESEARCH.md](RESEARCH.md) are historical design records,
not current product documentation.

## Verify

```sh
cargo fmt --check
scripts/validate-release.sh --metadata-only
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo audit
cargo run --locked -- replay fixtures/ghostty-tab-replacement.json
cargo run --locked -- config-check config.example.toml
(cd website && bun install --frozen-lockfile && bun audit && bun run build)
git diff --check
```

### Focused Reliability Checks

The reliability regressions in `src/app/tests/reliability.rs` exercise ambiguous multi-display tabs,
cross-file crash recovery, delayed window-server confirmation, and display-gap parking through the
fake backend. `src/app/input.rs` tests ordered input, queue limits, and complete gestures without
preview ticks. Layout and geometry tests cover resize-then-drag and multi-display parking. Run the
focused checks without touching desktop windows:

```sh
cargo test --locked --lib app::tests::reliability
cargo test --locked --lib app::input::tests
cargo test --locked --lib layout_tree::tests
cargo test --locked --lib geometry::tests
```

Read-only or dry-run macOS probes:

```sh
cargo run --locked -- config-check config.example.toml
cargo run --locked -- displays
cargo run --locked -- snapshot
cargo run --locked -- retile
cargo run --locked -- watch 20
```

Commands with `--apply` and the menu-bar app move real windows. Disable every other window manager and
use disposable windows before running them. The live harnesses below are the exception: they require
AeroSpace to be enabled initially so they can capture a baseline, then they disable and restore it.

## Live Acceptance

```sh
./scripts/live-ghostty-tabs.sh
./scripts/live-workspaces.sh
```

These harnesses require a current signed bundle at `target/Spaceman.app`; build it with
`./scripts/build-app.sh` first. `live-workspaces.sh` isolates control and recovery files with
`SPACEMAN_APPLICATION_SUPPORT`; `live-ghostty-tabs.sh` uses a scoped, non-persistent `watch-window`
process. Both restore their baseline but still exercise live Accessibility writes. Start them with
AeroSpace enabled, read each script, preserve unrelated windows, and confirm its expected display and
Ghostty assumptions. Never run a destructive live test on a user's normal workspace merely because
unit tests pass.

## Documentation Checks

When commands, menu labels, config fields, state paths, shortcuts, packaging, or recovery semantics
change, update README and the corresponding document in the same change. Inspect links, stale labels,
and whitespace:

```sh
rg -n '\[[^]]+\]\([^)]+' --glob '*.md'
rg -n 'Disable Tiling|Enable Tiling|Settings\.\.\.|Quit Spaceman' --glob '*.md'
rg -n 'Automatic Tiling|Retile Now|starts paused|paused startup' --glob '*.md' --glob '!docs/development.md'
git diff --check
```
