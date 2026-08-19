# Security And Privacy

## Accessibility Scope

Spaceman needs macOS Accessibility permission to enumerate eligible application windows, read their
role, state, title, position, and size, determine focus, and focus, move, or resize them. It also reads
the on-screen Core Graphics window list, display topology, keyboard input source, pointer position,
and mouse-button state needed for tiling and gestures.

The permission is broad because macOS grants Accessibility at application scope. Spaceman limits its
own writes to eligible standard, non-minimized, non-fullscreen windows whose position is settable.
Resizable windows may be moved and resized; movable non-resizable windows may be parked off-screen
and restored without changing their size. It does not require disabling System Integrity Protection.

## Data Handling

Spaceman has no telemetry or analytics. Packaged release builds use Sparkle to request the signed
`appcast.xml` update feed and, when an update is selected, its signed archive from Spaceman's public
Cloudflare R2 bucket. Automatic checks are enabled; automatic installation is not. Source CLI and
test binaries have no adjacent Sparkle framework and do not perform update checks. Other runtime data
stays on the Mac unless the user shares CLI output or files.

The optional `Launch at Login` menu action registers or unregisters the signed main app through
macOS Service Management. Registration is stored and enforced by macOS; Spaceman does not install a
separate helper, daemon, or launch-agent plist.

Spaceman can read a window title for exact config-rule matching. Titles remain in memory and are
explicitly excluded from serialized observations, persisted state, `spaceman ctl debug`, and
`spaceman hud` output.

Local files live under `~/Library/Application Support/Spaceman/`:

- `config.toml`: user-authored configuration;
- `workspace-state.json`: active workspace selectors, previous-workspace history, logical slots,
  last-focused and maximized slot intent, floating-window ownership, and layout trees;
- `parked-windows.json`: physical process/window identifiers and original frames needed for recovery;
- `agent.log`: timestamped diagnostic events (switches, parking, restoration, ownership changes) with
  process/window identifiers and frames, never window titles; mode `0600`, discarded past 5 MB at the
  next launch. Terminal launches keep diagnostics on stderr instead;
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
