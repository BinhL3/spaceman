# Research Behind The Spaceman MVP

## The Ghostty Failure Is A Native-Tab Identity Problem

Ghostty uses AppKit native tabs. macOS reports each native tab as a separate accessibility window,
which causes tiling managers to reserve a slot for tabs that are not visible.

Primary references:

- [Ghostty: macOS Tiling Window Managers](https://ghostty.org/docs/help/macos-tiling-wms)
- [AeroSpace issue #68: Native macOS tabs are considered windows](https://github.com/nikitabobko/AeroSpace/issues/68)
- [Ghostty issue #3170: Creating a new tab splits the screen](https://github.com/ghostty-org/ghostty/issues/3170)
- [AeroSpace discussion #2071: Ghostty 1.3.1 regression report](https://github.com/nikitabobko/AeroSpace/discussions/2071)

The reproduced environment was Ghostty 1.3.1, AeroSpace 0.20.3-Beta, and a tiled root layout. Creating
one tab caused the exact reported half-width result.

## AeroSpace's Proposed Fix Replaces A Physical Window In Place

[AeroSpace PR #2206](https://github.com/nikitabobko/AeroSpace/pull/2206) reconciles Ghostty and Fork
against each application's current `AXWindows` list. When the focused tab receives a new physical
window ID, the patch replaces the stale window in the same parent, index, and adaptive weight. It
also preserves cached geometry and fullscreen state.

Important implementation lessons:

- Apply the behavior only to known bundle IDs.
- Register the focused tab before pruning the stale tab.
- Prefer the application's previous focused ID as the replacement candidate.
- Replace only a single unambiguous candidate.
- Perform unbind and bind in one synchronous model transaction.
- Skip reconciliation while the left mouse button is down.

The PR was open and unmerged when this plan was written on July 29, 2026.

## AeroSpace Demonstrates A Sustainable API Floor

[AeroSpace](https://github.com/nikitabobko/AeroSpace) is MIT-licensed Swift. It avoids disabling SIP
and uses the Accessibility API plus `_AXUIElementGetWindow` for stable Core Graphics window IDs.

Relevant architecture:

- `NSWorkspace` discovers applications and emits lifecycle invalidations.
- One Accessibility observer follows each application.
- Notifications schedule a complete refresh rather than directly editing the tree.
- Logical layout mutation runs serially on the main actor.
- Frame writes run on per-app threads because AX calls can block.
- Layout calculation is separate from native frame application.

Spaceman adopts those proven constraints but starts with a smaller flat workspace and makes physical
binding replacement part of the initial model.

## Paneru Demonstrates Geometric Tab Grouping

[Paneru](https://github.com/karinushka/paneru) is an MIT-licensed Rust window manager. Its tab logic
groups same-application windows whose complete frames match and treats the group as one layout
column. Tests reject same-width windows whose origins or heights differ.

The geometric heuristic is useful corroborating evidence, but it cannot be Spaceman's only identity
rule. Separate top-level windows can overlap exactly, and native-tab exposure changes during drag and
focus transitions. Spaceman therefore starts with AeroSpace's conservative focused-ID replacement
for allowlisted apps. Full-frame matching may become a secondary signal after recorded fixtures show
where it is safe.

## The Accessibility API Is Sufficient For An MVP

Apple's Accessibility API provides application/window discovery, observation, focus, position, and
size control:

- [`AXUIElement`](https://developer.apple.com/documentation/applicationservices/axuielement_h)
- [`AXObserverCreate`](https://developer.apple.com/documentation/applicationservices/1462089-axobservercreate)
- [`AXUIElementSetAttributeValue`](https://developer.apple.com/documentation/applicationservices/1462085-axuielementsetattributevalue)
- [`AXIsProcessTrustedWithOptions`](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions)

The important attributes and notifications are:

- `kAXWindowsAttribute`
- `kAXFocusedWindowAttribute`
- `kAXPositionAttribute`
- `kAXSizeAttribute`
- `kAXWindowCreatedNotification`
- `kAXFocusedWindowChangedNotification`
- `kAXUIElementDestroyedNotification`
- `kAXWindowMovedNotification`
- `kAXWindowResizedNotification`

There is no public API to obtain a `CGWindowID` from an `AXUIElement`. Mature window tools use the
private `_AXUIElementGetWindow` function. Isolating that call in one module limits the compatibility
risk and rules out Mac App Store distribution.

## Frame Writes Need A Defensive Sequence

Applications constrain window sizes and Accessibility exposes position and size as separate writes.
Mature tools such as [Rectangle](https://github.com/rxhanson/Rectangle) and AeroSpace use a defensive
sequence:

1. Set size.
2. Set position.
3. Set size again.

Spaceman will compare the current and target frame first, apply only changes, and associate writes
with a generation so resulting move/resize events do not create a feedback loop.

## Native Spaces Are Outside The MVP

macOS has no supported public API for creating, enumerating, or moving windows between Mission
Control Spaces. Yabai gains deeper Spaces control through private APIs and a scripting addition that
requires reducing SIP protections.

Spaceman will not take that path. Its MVP has one workspace. A later workspace implementation can
emulate workspaces by moving inactive windows off-screen, following AeroSpace's no-SIP design.

References:

- [AeroSpace guide: virtual workspace emulation](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces)
- [yabai](https://github.com/koekeishiya/yabai)

## Comparable Projects And Licenses

| Project | Language | License | Relevant lesson |
| --- | --- | --- | --- |
| [AeroSpace](https://github.com/nikitabobko/AeroSpace) | Swift | MIT | Reconciliation, virtual workspaces, no-SIP operation |
| [yabai](https://github.com/koekeishiya/yabai) | C | MIT | Deep native control with much higher OS/SIP maintenance |
| [Amethyst](https://github.com/ianyh/Amethyst) | Swift/Objective-C | MIT | Mature automatic tiling and layout vocabulary |
| [Rectangle](https://github.com/rxhanson/Rectangle) | Swift | MIT | Focused Accessibility seam and reliable frame writes |
| [Paneru](https://github.com/karinushka/paneru) | Rust | MIT | Testable state separation and geometric tab grouping |
| [Loop](https://github.com/MrKai77/Loop) | Swift | GPL-3.0 | Polished manual window movement, but incompatible reuse license |

Spaceman uses MIT to match the reusable ecosystem and permit broad adoption.

## Research Conclusion

The smallest credible Spaceman is not an AeroSpace fork and not a complete workspace manager. It is
a Swift menu-bar app with three deep modules:

- `MacWindows` turns inconsistent Accessibility state into snapshots and commands.
- `Workspace` reconciles physical windows into stable logical slots.
- `Tiler` converts logical slots into target rectangles.

The first release succeeds when Ghostty tabs stop changing the logical slot count while ordinary
windows still tile correctly. Everything else can follow from that proven seam.
