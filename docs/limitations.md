# Known Limitations

Spaceman is an experimental public preview, not a broadly qualified release.

- The downloadable preview is currently arm64-only; x86_64 and universal packaging are not validated.
- The declared macOS 14 minimum lacks a documented validation matrix across supported macOS releases.
- Clean-machine install, permission, upgrade, recovery, and uninstall have not been release-qualified.
- The app depends on private `_AXUIElementGetWindow` behavior and cannot target the Mac App Store.
- Accessibility behavior varies by application; only standard, movable, on-screen windows can be
  workspace-owned. Nonstandard or immovable panels remain global.
- Native-tab identity has no perfect public identifier. Ambiguous transitions intentionally defer
  replacement and may need another cycle.
- Configuration supports ordered exact-match `float`/`route`/`ignore` rules and display assignment. It
  does not support named app spaces, regex title matching, or the planned orthogonal rule schema.
- Configuration is loaded only at process startup. `ctl restart` exits safely but does not relaunch.
- `secondary` selects the first non-main display; display ordering and duplicate names may surprise.
- Global shortcuts may collide with other applications or fail on keyboard layouts that cannot
  produce required characters.
- Settings currently covers Accessibility, Launch at Login, and signed updates rather than every
  configuration option. There is no telemetry or analytics.
- The repository still lacks a concise product demo.

Use the isolated live harnesses in [Development](development.md) before changing window-management,
workspace, display, or recovery behavior.
