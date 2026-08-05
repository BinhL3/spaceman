# Security And Privacy

## Accessibility Scope

Spaceman needs macOS Accessibility permission to enumerate eligible application windows, read their
role, state, title, position, and size, determine focus, and focus, move, or resize them. It also reads
the on-screen Core Graphics window list, display topology, keyboard input source, pointer position,
and mouse-button state needed for tiling and gestures.

The permission is broad because macOS grants Accessibility at application scope. Spaceman limits its
own writes to eligible standard, on-screen, non-minimized, non-fullscreen windows whose position and
size are settable. It does not require disabling System Integrity Protection.

## Data Handling

Spaceman currently has no telemetry, analytics, network requests, or update checks. Runtime data stays
on the Mac unless the user shares CLI output or files.

Spaceman can read a window title for exact config-rule matching. Titles remain in memory and are
explicitly excluded from serialized observations, persisted state, `spaceman ctl debug`, and
`spaceman hud` output.

Local files live under `~/Library/Application Support/Spaceman/`:

- `config.toml`: user-authored configuration;
- `workspace-state.json`: active workspace selectors, previous-workspace history, logical slots, and
  layout trees;
- `parked-windows.json`: physical process/window identifiers and original frames needed for recovery;
- `control.sock`: local Unix control socket, mode `0600`;
- `control.lock`: single-agent lock, mode `0600`.

State files contain application process/window identity and geometry, but no window titles. The code
uses atomic replacement for both JSON state files. The directory itself follows the user's normal
filesystem permissions; do not sync or publish recovery state without reviewing it.

## Native Interfaces

Most integration uses macOS Accessibility, AppKit, Carbon hotkeys, Core Foundation, and Core Graphics.
Spaceman dynamically resolves the private `_AXUIElementGetWindow` function to associate an
Accessibility element with a Core Graphics window ID. If that symbol disappears or changes, window
discovery can fail. This private API prevents Mac App Store distribution and is a compatibility and
review risk for any public release.

See [SECURITY.md](../SECURITY.md) for vulnerability reporting.
