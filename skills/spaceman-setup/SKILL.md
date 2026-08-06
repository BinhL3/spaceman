---
name: spaceman-setup
description: Install, configure, verify, troubleshoot, or uninstall the Spaceman tiling window manager on a user's Mac. Use when a user asks to set up Spaceman, grant its Accessibility permission, configure workspaces or hotkeys, migrate from AeroSpace/yabai/Amethyst, recover off-screen windows, or remove Spaceman safely.
---

# Spaceman Setup

Spaceman is a native macOS tiling window manager (Apple silicon, macOS 14+). This skill walks an
agent through installing and operating it safely on a user's machine.

## Safety Rules (read first)

- **Never run two window managers at once.** Disable AeroSpace, yabai, Amethyst, and any other
  tiling manager *before* Spaceman first opens, and re-enable them only after Spaceman exits.
- **Never delete `~/Library/Application Support/Spaceman/parked-windows.json` as a troubleshooting
  step.** It records where off-screen parked windows must return. Deleting it strands windows.
- **Always let restoration finish before removing anything.** Pause or quit Spaceman and confirm
  windows returned before uninstalling or clearing state.
- Spaceman never requires disabling SIP. If any instructions suggest that, they are wrong.
- Ask before killing processes or closing the user's windows.

## Preflight

```sh
uname -m          # must print arm64 (Apple silicon only)
sw_vers -productVersion   # must be 14.0 or newer
```

Check for conflicting window managers and disable them:

```sh
pgrep -lx AeroSpace yabai Amethyst
aerospace enable off   # if AeroSpace is running
yabai --stop-service   # if yabai is running
```

For others, quit them from their menu bar item. Confirm with the user before disabling anything.

## Install

1. Download the notarized build and its checksum:

   ```sh
   cd "$(mktemp -d)"
   curl -fLO "https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip"
   curl -fLO "https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip.sha256"
   shasum -a 256 -c Spaceman-v0.1.1-macos-arm64.zip.sha256
   ```

   Stop if the checksum does not verify.

2. Install and open:

   ```sh
   ditto -x -k Spaceman-v0.1.1-macos-arm64.zip .
   mkdir -p "$HOME/Applications"
   mv Spaceman.app "$HOME/Applications/"
   open "$HOME/Applications/Spaceman.app"
   ```

3. Spaceman opens Settings when Accessibility access is missing. Have the user choose
   `Grant Access`, then enable Spaceman under
   **System Settings > Privacy & Security > Accessibility**. The agent cannot grant this; the user
   must click it themselves.

4. Optionally enable `Launch at Login` in Spaceman Settings. If macOS requires approval, the
   control opens **System Settings > General > Login Items & Extensions**.

Tiling starts automatically once hotkeys register; there is no paused first launch.

## Verify

The CLI ships inside the bundle. Link it once, then check status:

```sh
mkdir -p "$HOME/.local/bin"
ln -sf "$HOME/Applications/Spaceman.app/Contents/MacOS/spaceman" "$HOME/.local/bin/spaceman"
"$HOME/.local/bin/spaceman" ctl status
```

Healthy output reports `"mode":"running"` and `"busy":false`. Ensure `~/.local/bin` is on `PATH`.

## Everyday Controls

| Keys | Action |
| --- | --- |
| `Option+1` … `Option+9`, `Option+0` | Switch to workspace 1 … 10 |
| `Option+Shift+1` … `Option+Shift+0` | Move focused window to workspace 1 … 10 |
| `Option+Tab` | Back to the previous workspace on the focused display |
| `Option+H/J/K/L` | Focus left/down/up/right |
| `Option+Shift+H/J/K/L` | Move the focused tile |
| `Option+/` | Toggle the root split axis |
| `Option+-`, `Option+=` | Shrink/grow the focused tile |
| `Option+Shift+F` | Toggle maximize (not native fullscreen) |

CLI equivalents: `spaceman ctl switch 2`, `ctl move 7`, `ctl layout`, `ctl balance`, `ctl pause`,
`ctl resume`, `ctl back-and-forth`.

## Configure

Spaceman reads `~/Library/Application Support/Spaceman/config.toml` at process startup
(`SPACEMAN_CONFIG` overrides the path). Workspaces 1–5 prefer the main display and 6–10 the
secondary by default; assignments accept `main`, `secondary`, a display UUID, or an exact display
name. Always validate before restarting:

```sh
spaceman config-check "$HOME/Library/Application Support/Spaceman/config.toml"
spaceman displays
```

Consult `config.example.toml` and `docs/configuration.md` in the repository for the exact schema.
Rules are ordered and first-match; do not invent schema fields.

## Troubleshoot

- **Hotkeys do nothing**: another app may own `Option` shortcuts; check the menu bar item for a
  hotkey error. Accessibility permission missing also blocks management — the menu reports it.
- **Windows are off-screen after a crash**: reopen the same app, then relaunch Spaceman and let it
  restore. Do not delete state files. Follow `docs/recovery.md`.
- **Two managers fighting** (windows repeatedly moving): quit the other manager, then
  `spaceman ctl resume`.
- **Updates**: delivered through Sparkle from Spaceman Settings; they are signed and notarized.

## Uninstall (safe order)

```sh
spaceman ctl pause      # wait for "mode":"paused" and "busy":false
spaceman ctl quit       # wait for spaceman-agent to exit
```

Visually confirm parked windows returned, re-enable the previous window manager, then:

```sh
rm -rf "$HOME/Applications/Spaceman.app"
rm -f "$HOME/.local/bin/spaceman"
rm -rf "$HOME/Library/Application Support/Spaceman"   # only after restoration is confirmed
```

Optionally remove Spaceman from **System Settings > Privacy & Security > Accessibility**.
