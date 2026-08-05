# Known Limitations

Spaceman is private, experimental software, not a proven public release.

- No downloadable, notarized, or stapled artifact exists.
- The declared macOS 14 minimum and CPU architectures lack a documented validation matrix.
- Clean-machine install, permission, upgrade, recovery, and uninstall have not been release-qualified.
- The app depends on private `_AXUIElementGetWindow` behavior and cannot target the Mac App Store.
- Accessibility behavior varies by application; only standard, movable, on-screen windows can be
  workspace-owned. Nonstandard or immovable panels remain global.
- Native-tab identity has no perfect public identifier. Ambiguous transitions intentionally defer
  replacement and may need another cycle.
- Configuration supports ordered exact-match `float`/`route` rules and display assignment only. It
  does not support named app spaces, regex title matching, or the planned orthogonal rule schema.
- Configuration is loaded only at process startup. `ctl restart` exits safely but does not relaunch.
- `secondary` selects the first non-main display; display ordering and duplicate names may surprise.
- Global shortcuts may collide with other applications or fail on keyboard layouts that cannot
  produce required characters.
- There is no preferences UI, automatic updater, telemetry, or network service.
- The repository still lacks a concise product demo.

Use the isolated live harnesses in [Development](development.md) before changing window-management,
workspace, display, or recovery behavior.
