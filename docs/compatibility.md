# Compatibility

Spaceman is macOS-only. The app plist declares macOS 14.0 as its minimum, but the declared minimum,
newer macOS releases, and supported CPU architectures have not yet been validated on clean machines.
Treat compatibility as experimental rather than guaranteed.

The source build requires:

- Xcode command-line tools with `codesign` and `security`;
- Rust 1.95.0 from `rust-toolchain.toml`;
- a `Developer ID Application` signing identity;
- an ANSI number-row keyboard for the documented physical workspace keys.

Spaceman manages Accessibility elements with role `AXWindow` and subrole `AXStandardWindow` that are
on screen, not minimized, not in native fullscreen, centered on an attached display, and expose
settable position and size. Utility panels, dialogs with other subroles, minimized/fullscreen windows,
and applications with incomplete or unusual Accessibility implementations may float or be ignored.

Ghostty native tabs are the primary stable-logical-window target. Replacement is conservative: when
the evidence is ambiguous, Spaceman preserves existing logical state rather than guessing. Other apps
with physical-window identity churn are not implicitly guaranteed to behave like Ghostty.

Multiple displays are supported dynamically. `secondary` means the first non-main display reported by
macOS. Exact duplicate display names are ambiguous; prefer UUIDs. Missing assigned displays fall back
to main until they reconnect.

Spaceman uses the private `_AXUIElementGetWindow` symbol. It requires no SIP change, but an OS update
could remove or alter that symbol. Mac App Store distribution is not supported.
