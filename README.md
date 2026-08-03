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

The repository also contains the implementation plan and supporting research:

- [PLAN.md](PLAN.md) defines the MVP, architecture, milestones, and acceptance tests.
- [docs/RESEARCH.md](docs/RESEARCH.md) records the source-backed findings behind the plan.

## Status

Active development. Fixture replay, live main-display/current-Space observation, dry-run layout, and
explicit frame application are executable and tested. The next vertical slice is a continuous runtime
that preserves its model across observations; menu-bar packaging follows after that engine is stable.

## License

MIT
