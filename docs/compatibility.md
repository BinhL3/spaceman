# Compatibility

Spaceman is macOS-only. The app plist declares macOS 14.0 as its minimum, but the declared minimum,
newer macOS releases, and supported CPU architectures have not yet been validated on clean machines.
Treat compatibility as experimental rather than guaranteed.

The source build requires:

- Xcode command-line tools with `codesign` and `security`;
- Rust 1.95.0 from `rust-toolchain.toml`;
- a `Developer ID Application` signing identity;
- an ANSI number-row keyboard for the documented physical workspace keys.

`Launch at Login` uses macOS Service Management's `SMAppService` and therefore requires a properly
signed application bundle. macOS may require explicit approval under **System Settings > General >
Login Items & Extensions**.

Spaceman observes Accessibility elements with role `AXWindow` and subrole `AXStandardWindow` that are
on screen, not minimized, not in native fullscreen, centered on an attached display, and expose a
settable position. Windows with a settable size can tile. Movable non-resizable windows and windows
matched by a `float` rule retain their frame but park and reveal with their logical workspace. Utility
panels, dialogs with other subroles, and applications with incomplete or unusual Accessibility
implementations may remain global or be ignored.

Some applications reposition their own windows. If a parked window reappears on screen while its
application is frontmost, the user summoned it: the stale recovery frame is discarded and the window —
tiled or floating — is re-homed to the workspace it reappeared on. If it reappears in the background —
Electron applications re-assert saved bounds on screen-parameter changes — it is quietly pushed back
off screen with its recovery frame intact. If an application undoes several re-parks in a row, the
reappearance is honored after all and the window is re-homed. A window whose reappearance is honored
twice within five minutes is quarantined for the rest of the session: released from its workspace and
left entirely unmanaged. Quarantined windows appear in the `ignored` list of `spaceman ctl debug`, and an `ignore`
config rule opts such applications out permanently. Windows an application keeps off screen itself are
left alone rather than parked, and never recorded as recovery frames.

Ghostty native tabs are the primary stable-logical-window target. Replacement is conservative: when
the evidence is ambiguous, Spaceman preserves existing logical state rather than guessing. Other apps
with physical-window identity churn are not implicitly guaranteed to behave like Ghostty.

Multiple displays are supported dynamically. `secondary` means the first non-main display reported by
macOS. Exact duplicate display names are ambiguous; prefer UUIDs. Missing assigned displays fall back
to main until they reconnect.

Spaceman uses the private `_AXUIElementGetWindow` symbol. It requires no SIP change, but an OS update
could remove or alter that symbol. Mac App Store distribution is not supported.
