# Spaceman

Spaceman is an experimental macOS tiling window manager that treats native tabs as one logical
window. Its first target is the Ghostty failure where opening a native tab causes AeroSpace to
allocate another tile and halve the visible terminal.

The project will use the macOS Accessibility API, require no SIP changes, and keep window discovery
separate from layout state. A physical `AXWindow` may disappear or change identity while its logical
tile remains in place.

This repository currently contains the implementation plan and supporting research:

- [PLAN.md](PLAN.md) defines the MVP, architecture, milestones, and acceptance tests.
- [docs/RESEARCH.md](docs/RESEARCH.md) records the source-backed findings behind the plan.

## Status

Planning. No production code exists yet.

## License

MIT
