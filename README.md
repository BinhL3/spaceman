# Spaceman

Spaceman is an experimental macOS tiling window manager built around stable logical windows. A
native window, including a selected Ghostty native tab, can change physical Accessibility identity
without creating a new tile. Spaceman keeps the logical slot and replaces its physical binding when
the evidence is unambiguous.

> [!WARNING]
> Disable AeroSpace, yabai, Amethyst, and every other window manager before first opening Spaceman
> or installing it from source. Two managers can repeatedly move the same windows. Keep the parked
> window state until Spaceman has successfully paused or quit and restored every window.

Spaceman currently:

- starts automatic tiling when its global hotkeys are available;
- provides ten logical workspaces, with 1-5 on the main display and 6-10 on the secondary display by
  default;
- assigns workspaces to `main`, `secondary`, a display UUID, or an exact display name, falling back
  to the main display when a preferred display disconnects;
- persists active workspaces, nested split layouts, tile order and weights, and parked-window recovery
  metadata across restarts;
- can register its signed app bundle with macOS to launch automatically at login;
- supports directional focus and movement across displays, drag-to-reorder/nest, mouse and keyboard
  resizing, balancing, and non-fullscreen maximize;
- keeps movable non-resizable and rule-floated windows with their logical workspace without tiling
  or resizing them;
- exposes a local CLI over a user-only Unix socket.

Spaceman uses Accessibility APIs to observe, focus, move, and resize eligible windows. It requires no
SIP change. Read [Security and privacy](docs/security-and-privacy.md),
[Compatibility](docs/compatibility.md), and [Limitations](docs/limitations.md) before trying it.

## Install Preview

The current preview is a Developer ID-signed, notarized arm64 build for Apple silicon Macs. It remains
experimental; read [Known limitations](docs/limitations.md) before installing it.

1. Disable every other window manager. For AeroSpace: `aerospace enable off`.
2. Download [Spaceman 0.1.1 for Apple silicon](https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip).
3. Unzip it, move `Spaceman.app` to `~/Applications`, and open it.
4. Complete the Accessibility setup shown by Spaceman.

The matching [SHA-256 checksum](https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip.sha256)
is published alongside the archive. Later preview updates are signed and delivered through Sparkle.

## Install From Source

Source builds are distinct from the notarized preview artifact and require your own `Developer ID
Application` signing identity.

Requirements: macOS, Xcode command-line tools, Rust 1.95.0, and a Developer ID Application identity.

From an existing repository checkout, first disable every other window manager, then run:

```sh
./scripts/install-app.sh
```

Set `SPACEMAN_CODESIGN_IDENTITY` if auto-detection does not choose the desired identity. The script
builds and signs `target/Spaceman.app`, installs it at `~/Applications/Spaceman.app`, links the CLI to
`~/.local/bin/spaceman`, and opens the app.

Ensure `~/.local/bin` is on `PATH`. For zsh:

```sh
printf '\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$HOME/.zshrc"
exec zsh
```

Artifact users do not need a Developer ID identity. See [Releasing](docs/releasing.md) for the
maintainer workflow.

## First Open

1. Disable every other window manager. For AeroSpace: `aerospace enable off`.
2. Open Spaceman: `open "$HOME/Applications/Spaceman.app"`.
3. Spaceman opens Settings when Accessibility access is missing. Choose `Grant Access`, then enable
   Spaceman under **System Settings > Privacy & Security > Accessibility**.
4. Optionally enable `Launch at Login` in Spaceman Settings.
5. Confirm operation with `spaceman ctl status`.

When hotkeys register successfully, Spaceman starts automatic tiling; it does not intentionally start
paused. Without Accessibility permission, the menu reports the permission error and tiling cannot
manage windows. The menu actions are `Disable Tiling` (or `Enable Tiling` while paused),
`Launch at Login`, `Settings...`, and `Quit Spaceman`. Settings shows Accessibility, launch-at-login,
and signed software-update controls. If macOS requires approval, the login item action opens **System
Settings > General > Login Items & Extensions**.

## Everyday Use

Core controls:

| Keys | Action |
| --- | --- |
| `Option+1` ... `Option+9`, `Option+0` | Switch to workspace 1 ... 10 |
| `Option+Shift+1` ... `Option+Shift+0` | Move the focused window to workspace 1 ... 10 |
| `Option+Tab` | Return to the previous workspace on the focused display |
| `Option+H/J/K/L` | Focus left/down/up/right |
| `Option+Shift+H/J/K/L` | Move the focused tile left/down/up/right |
| `Option+/` | Toggle the active layout's root split axis |
| `Option+-`, `Option+=` | Shrink/grow the focused tile by 50 pixels |
| `Option+Shift+F` | Toggle maximize without native fullscreen |

Character shortcuts follow the active macOS keyboard layout. Workspace digits remain the physical
ANSI number row, and Tab remains physical Tab. See [Controls](docs/controls.md) for drag, resize,
balance, CLI, and socket behavior.

```sh
spaceman ctl status
spaceman ctl switch 2
spaceman ctl move 7
spaceman ctl layout
spaceman ctl balance
spaceman ctl pause
spaceman ctl resume
```

## Configure

Spaceman reads `~/Library/Application Support/Spaceman/config.toml` at process startup. See the exact
current schema and [config.example.toml](config.example.toml) in
[Configuration](docs/configuration.md).

```sh
spaceman config-check "$HOME/Library/Application Support/Spaceman/config.toml"
spaceman displays
```

## Recover Or Remove

Use `spaceman ctl pause` or `spaceman ctl quit` and wait for `ctl status`/process exit to confirm that
restoration completed **before** removing the app or state. Do not delete `parked-windows.json` as an
early troubleshooting step: it records where off-screen parked windows must return.

Follow [Recovery](docs/recovery.md) for stuck or crashed processes and safe uninstall instructions.

## Documentation

- [Configuration](docs/configuration.md)
- [Controls](docs/controls.md)
- [Recovery and uninstall](docs/recovery.md)
- [Security and privacy](docs/security-and-privacy.md)
- [Compatibility](docs/compatibility.md)
- [Known limitations](docs/limitations.md)
- [Development](docs/development.md)
- [Releasing](docs/releasing.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Historical implementation plan](PLAN.md) and [historical research](docs/RESEARCH.md)

## Status

Private and experimental. Core behavior and automated tests exist, but release readiness is not
proven. Notarization/stapling, downloadable artifacts, architecture and minimum-OS validation, a
clean-machine test, and release presentation remain blockers.

## License

MIT
