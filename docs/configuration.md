# Configuration

Spaceman loads configuration once, when `spaceman-agent` starts, from:

```text
~/Library/Application Support/Spaceman/config.toml
```

Set `SPACEMAN_CONFIG` in the agent's environment to replace that path. A missing file means default
configuration. Invalid configuration is reported to stderr during recovery; Spaceman falls back to
defaults and ignores workspace state so it can prioritize restoring parked windows.

Validate a file before replacing or restarting the app:

```sh
spaceman config-check "$HOME/Library/Application Support/Spaceman/config.toml"
```

The complete example is [config.example.toml](../config.example.toml).

## Current Schema

The parser rejects unknown fields. The root must contain `version = 1` and may contain repeated
`[[rules]]` and `[[workspaces]]` tables.

```toml
version = 1

[[rules]]
bundle_id = "com.example.Application" # optional exact match
title = "Preferences"                 # optional exact match
action = "float"                      # required: "float", "route", or "ignore"

[[rules]]
bundle_id = "com.tinyspeck.slackmacgap"
action = "route"
workspace = 2                         # required for route; forbidden for float and ignore

[[workspaces]]
workspace = 9                         # integer 1 through 10; unique in this list
monitor = "main"                      # non-empty selector
```

A rule needs at least one of `bundle_id` or `title`. If both are present, both must match exactly.
Rules run in file order and the first matching rule wins. An unmatched window is managed normally.

- `float` excludes the window from tiling and resizing, preserves its native frame, and keeps it with
  the logical workspace where it was first observed.
- `route` assigns a newly inserted logical window to workspace 1 through 10. It does not repeatedly
  route existing slots, including a Ghostty native-tab physical replacement.
- `ignore` leaves the window entirely alone: never tiled, never owned by a workspace, never moved or
  parked. Use it for applications that insist on positioning their own windows. A previously managed
  window is released the next time it is observed on screen.

Title matching occurs only in memory. Titles are not serialized into persisted state, snapshots,
`ctl debug`, or the HUD.

## Display Assignments

Defaults assign workspaces 1-5 to `main` and 6-10 to `secondary`. `monitor` accepts:

- `main`, the display macOS marks as main;
- `secondary`, the first attached non-main display;
- an exact display UUID from `spaceman displays`;
- an exact display name from `spaceman displays`.

```sh
spaceman displays
```

A missing `secondary`, UUID, or name falls back to the main display. Spaceman retains the preferred
assignment as its own active lane; reconnecting that display makes its assigned active workspace
visible again.

## Process Scope

`SPACEMAN_MANAGED_DISPLAY` is an internal process-scope override used by isolated tests. It accepts
`main`, `secondary`, or `both` (the default). It is not a TOML field, and the installed app does not
provide a UI for setting it. A scoped process registers only workspace hotkeys assigned to its managed
main/secondary selectors; normal users should leave it unset.

## Applying Changes

There is no live config reload. Pause or quit and confirm restoration, then open the app again:

```sh
spaceman ctl pause
spaceman ctl status
spaceman ctl quit
open "$HOME/Applications/Spaceman.app"
```

`spaceman ctl restart` safely restores parked windows and exits, but currently does not relaunch the
app by itself. The source installer uses it before replacing the bundle, then opens the new bundle.
