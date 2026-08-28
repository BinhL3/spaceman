# Controls

Spaceman registers global hotkeys while automatic tiling is enabled. Character keys follow the
current macOS input source; Spaceman refreshes registrations after an input-source change. An invalid
layout prevents startup registration and leaves tiling paused. During a layout change, Spaceman keeps
the existing registrations if it cannot construct a replacement plan; if replacement registration
fails after the old registrations are released, it pauses and reports `Hotkeys unavailable`.

| Keys | Action |
| --- | --- |
| `Option+1` ... `Option+9`, `Option+0` | Switch workspace 1 ... 10 |
| Add `Shift` to a workspace key | Move the focused window there without switching |
| `Option+A` ... `Option+Z` | Switch to that configured named workspace |
| Add `Shift` to a named workspace key | Move the focused window there without switching |
| `Option+Tab` | Toggle the focused display's current/previous workspace |
| Option-backtick | Focus the next tiled window, wrapping in stable workspace order |
| `Option+H/J/K/L` | Focus the nearest tile left/down/up/right, including across displays |
| `Option+Shift+H/J/K/L` | Move the focused tile in that direction |
| `Option+/` | Toggle the active workspace layout's root horizontal/vertical axis |
| `Option+-` / `Option+=` | Shrink/grow the focused tile by 50 pixels at its immediate split |
| `Option+Shift+F` | Toggle the workspace-wide Fullscreen Layout without entering native fullscreen |

Named workspace shortcuts are registered only when referenced by config. If a configured letter
overlaps a built-in shortcut, the workspace shortcut takes precedence.

Spaceman remembers the last-focused logical tile in each workspace and restores that focus when the
workspace becomes active again. Pressing an already-active workspace shortcut preserves the current
focus when one of that workspace's windows is already focused; otherwise it focuses the remembered
tile.

Fullscreen Layout is a workspace mode: every tiled window receives the full display frame, and the
focused window is raised above the others. Its menu-bar workspace glyph is filled while the mode is
active. Option-backtick focuses the next tiled window and wraps
through the workspace's stable tile order; it provides the same ordering in split layouts.
Command-backtick continues to use macOS's same-application window cycle, and Spaceman remembers the
resulting focus. Press `Option+Shift+F` again to restore the unchanged split layout. Fullscreen Layout
and its active tile survive workspace switches, native-tab physical-window replacement, and Spaceman
restarts. `Option+/` exits Fullscreen Layout, toggles the preserved split axis, and immediately applies
the resulting normal layout. Keyboard resize, balance, and tile reordering require leaving Fullscreen
Layout first. Pointer dragging and resizing are ignored while the mode is active. Floating windows are
not expanded. Removing or moving away the workspace's final tiled window exits Fullscreen Layout.

Dragging a managed tile over a sibling swaps it. Dropping near a tile edge can create a nested split;
dropping onto another display can move it to that display's active workspace. Resizing a managed
window with the pointer updates the relevant split weights. Spaceman previews the rest of the layout
during the gesture and applies the final result on mouse-up.

## Menu

The menu-bar control shows active workspace/layout glyphs and runtime errors. Its actions are:

- `Disable Tiling`, or `Enable Tiling` while paused;
- `Launch at Login`, shown checked when macOS has enabled the signed app bundle;
- `Settings...`, with Accessibility, launch-at-login, and software-update controls;
- `Quit Spaceman`.

If macOS reports that the login item requires approval, the menu label says
`Launch at Login (Approval Required...)`; choosing it opens Login Items settings. Registration does
not bypass macOS consent and does not affect the currently running process.

Pausing, quitting, and external macOS termination requests restore parked windows before completing.
Restoration retries are visible in the menu status. Installing a Sparkle update uses the same
restoration gate: Spaceman does not allow the installer or application termination to continue until
parked windows are restored and their recovery metadata is durably removed.

## CLI

The installed CLI is `~/.local/bin/spaceman`. Add `~/.local/bin` to `PATH`, or invoke that absolute
path. Commands that alter layouts require automatic tiling to be enabled.

```sh
spaceman ctl status
spaceman ctl debug
spaceman ctl switch 2
spaceman ctl back-and-forth
spaceman ctl move 7
spaceman ctl move-app com.tinyspeck.slackmacgap 6
spaceman ctl focus left
spaceman ctl move-direction right
spaceman ctl layout
spaceman ctl resize shrink
spaceman ctl resize grow
spaceman ctl balance
spaceman ctl retile
spaceman ctl pause
spaceman ctl resume
spaceman ctl restart
spaceman ctl quit
spaceman hud
```

Directions are `left`, `down`, `up`, and `right`. `balance` equalizes the focused tile's immediate
split while respecting nested minimum sizes. `hud` repeatedly renders sanitized runtime geometry in
the terminal until interrupted.

`ctl debug` excludes window titles. It reports each display's Fullscreen Layout state and tiled
geometry plus floating-window workspace, parked state, and original recovery frame so workspace leaks
can be diagnosed safely. Its `ignored` list names quarantined windows that un-parked themselves
repeatedly and are left unmanaged.

The agent listens at `~/Library/Application Support/Spaceman/control.sock`; both socket and lock file
are mode `0600`. Requests are newline-delimited, limited to 4 KiB, and served locally over a Unix
socket. The lock prevents two agents from running at once.
