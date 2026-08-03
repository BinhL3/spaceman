# Spaceman

Spaceman is an experimental macOS tiling window manager that treats native tabs as one logical
window. Its first target is the Ghostty failure where opening a native tab causes AeroSpace to
allocate another tile and halve the visible terminal.

The project uses Rust, will use GPUI for its foreground shell, requires no SIP changes, and keeps native Accessibility observation
separate from layout state. A physical `AXWindow` may disappear or change identity while its logical
tile remains in place. GPUI owns the foreground runtime and optional diagnostics UI; direct `objc2`
adapters own the menu-bar item and macOS window-management seams.

The first executable vertical slice implements fixture-driven reconciliation and layout:

```sh
cargo run -- replay fixtures/ghostty-tab-replacement.json
```

The JSON output shows every model revision, decision, and proposed placement. This path is also the
contract that live macOS observation and frame application will use.

On macOS with Accessibility permission granted, inspect the live observation and proposed layout:

```sh
cargo run -- snapshot
cargo run -- retile
```

Apply the proposed layout only when the dry run contains the expected windows:

```sh
cargo run -- retile --apply
```

Exercise reconciliation across repeated live observations, optionally applying each accepted layout:

```sh
cargo run -- watch 20
cargo run -- watch 20 --apply
```

The bounded watch is intended for hands-on Ghostty tests: create, select, and close tabs while it is
running, then inspect the per-cycle decisions in the JSON output.

For isolated integration tests, scope the watcher and writes to one initial physical window ID. New
physical IDs at the same logical frame are treated as candidates for that window's slot:

```sh
cargo run -- watch-window 12345 40 --apply
```

Run the complete autonomous Ghostty acceptance flow:

```sh
./scripts/live-ghostty-tabs.sh
```

The harness launches a disposable Ghostty process on an isolated AeroSpace workspace on the
non-focused display, snapshots all existing window/workspace assignments, pauses AeroSpace while Spaceman owns layout, creates and
switches native tabs through PID-targeted menu actions, closes the disposable group, restores every
baseline workspace, and verifies that no test windows or workspace changes leaked.

The repository also contains the implementation plan and supporting research:

- [PLAN.md](PLAN.md) defines the MVP, architecture, milestones, and acceptance tests.
- [docs/RESEARCH.md](docs/RESEARCH.md) records the source-backed findings behind the plan.

## Status

Active development. Fixture replay, live main-display/current-Space observation, bounded continuous
reconciliation, dry-run layout, and explicit frame application are executable and tested. A persistent
event-driven runtime and menu-bar packaging follow after the polling engine is stable.

## License

MIT
