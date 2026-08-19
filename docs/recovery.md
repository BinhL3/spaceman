# Recovery And Uninstall

Spaceman moves inactive-workspace windows beyond the visible display union. Before doing so, it
atomically records each original frame in:

```text
~/Library/Application Support/Spaceman/parked-windows.json
```

**Preserve that file until pause or quit restoration is confirmed.** Deleting it first can discard
the only record of where an off-screen window belongs.

## Normal Recovery

1. Ask Spaceman to pause: `spaceman ctl pause`.
2. Poll `spaceman ctl status` until mode is `paused`, not `restoring`.
3. If needed, ask it to quit: `spaceman ctl quit`.
4. Wait for `pgrep -x spaceman-agent` to return no process.
5. Check that formerly hidden windows are visible before changing state files.

Pause and quit retry failed restorations. Do not kill the process while the menu says `Restoring
windows` unless it is irrecoverably stuck.

## After A Crash

Reopen the same installed app without deleting state:

```sh
open "/Applications/Spaceman.app"
```

If workspace state is absent or invalid, startup first attempts to restore every window recorded in
`parked-windows.json`. If valid workspace state exists, the app resumes that persisted workspace
model and restores windows as their workspaces become visible. Then use `spaceman ctl pause` if the
goal is to return all windows and stop tiling. If Accessibility permission or startup hotkey
registration is unavailable, startup remains paused and attempts to restore all parked windows.

If the app will not stay open, preserve a copy of the whole state directory before manual diagnosis:

```sh
cp -R "$HOME/Library/Application Support/Spaceman" \
  "$HOME/Desktop/Spaceman-recovery-state"
```

Do not edit physical window IDs or frames casually. They are recovery metadata, not a supported
configuration surface. A stale entry for a window that no longer exists is harmless compared with
losing metadata for a live parked window.

## Safe Uninstall

1. Turn off `Launch at Login` in Spaceman Settings and confirm it is disabled.
2. Disable automatic tiling with `spaceman ctl pause`.
3. Wait for `spaceman ctl status` to report `"mode":"paused"` and `"busy":false`.
4. Run `spaceman ctl quit` and wait for `spaceman-agent` to exit.
5. Visually confirm that parked windows returned.
6. Re-enable the previous window manager only after Spaceman exits.
7. Remove the app and CLI link.

```sh
rm -rf "/Applications/Spaceman.app"
rm -f "$HOME/.local/bin/spaceman"
```

Only after restoration is confirmed may you remove configuration and state:

```sh
rm -rf "$HOME/Library/Application Support/Spaceman"
```

Remove Spaceman from **System Settings > Privacy & Security > Accessibility** if desired. Spaceman
does not install a daemon, kernel extension, system extension, or network service.
