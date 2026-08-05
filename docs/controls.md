# Controls

Spaceman registers global hotkeys while automatic tiling is enabled. Character keys follow the
current macOS input source; Spaceman refreshes registrations after an input-source change. A mapping
collision or unsupported layout leaves tiling paused and reports `Hotkeys unavailable`.

| Keys | Action |
| --- | --- |
| `Option+1` ... `Option+9`, `Option+0` | Switch workspace 1 ... 10 |
| Add `Shift` to a workspace key | Move the focused window there without switching |
| `Option+Tab` | Toggle the focused display's current/previous workspace |
| `Option+H/J/K/L` | Focus the nearest tile left/down/up/right, including across displays |
| `Option+Shift+H/J/K/L` | Move the focused tile in that direction |
| `Option+/` | Toggle the active workspace layout's root horizontal/vertical axis |
| `Option+-` / `Option+=` | Shrink/grow the focused tile by 50 pixels at its immediate split |
| `Option+Shift+F` | Toggle maximize without entering native fullscreen |

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

`ctl debug` excludes window titles. It reports tiled geometry plus floating-window workspace, parked
state, and original recovery frame so workspace leaks can be diagnosed safely. Its `ignored` list
names quarantined windows that un-parked themselves repeatedly and are left unmanaged.

The agent listens at `~/Library/Application Support/Spaceman/control.sock`; both socket and lock file
are mode `0600`. Requests are newline-delimited, limited to 4 KiB, and served locally over a Unix
socket. The lock prevents two agents from running at once.
