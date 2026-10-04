# Tiling

Arrange a workspace's windows as non-overlapping tiles in nested horizontal/vertical splits, and
navigate, reorder, nest, resize, and balance them from the keyboard or the pointer.

## Sub-features

```ts
Automatic tiling                                        // tile.auto
├── New managed window -> inserted into the workspace's split tree, everything reflows
├── Window closes -> its slot retires, siblings reflow
└── Ghostty native tab replaces its window -> same slot, order, weight, nesting

Focus                                                   // tile.focus
├── Option+H/J/K/L / ctl focus <dir> -> nearest tile in that direction, crossing displays
│   └── In Fullscreen Layout -> previous/next tile in stable order (see fullscreen-layout.md)
└── Option+` / ctl focus next        -> next tile in stable order, wrapping

Reorder                                                 // tile.reorder
├── Option+Shift+H/J/K/L / ctl move-direction <dir> -> swap/move within the tree
│   ├── At the workspace edge -> moves to the adjacent display's active workspace
│   └── Perpendicular move    -> re-orients or creates a mixed split
└── Pointer drag
    ├── Over a sibling     -> live-previewed swap, applied on mouse-up
    ├── Near a tile edge   -> nested split
    └── Onto another display -> moved to that display's active workspace

Size                                                    // tile.size
├── Option+- / Option+= / ctl resize shrink|grow -> 50 px at the immediate split
├── Pointer resize       -> split weights follow the new frame, preview during the gesture
└── ctl balance          -> equalize the focused tile's immediate split (no default hotkey)

Axis                                                    // tile.axis
└── Option+/ / ctl layout -> flip the root split axis (also exits Fullscreen Layout)
```

## How To Get To It (User POV)

```ts
Keyboard: Option+H/J/K/L, Option+Shift+H/J/K/L, Option+`, Option+-, Option+=, Option+/
Pointer:  drag a managed window (reorder/nest/move display), drag its edge (resize)
CLI:      spaceman ctl focus | move-direction | resize | balance | layout | retile
```

Input arriving while the worker is busy queues in order (up to 64); queued input is dropped on
pause, quit, or hotkey loss.

## Existing Checks

- Unit (FakeBackend), `src/app/tests/layout.rs`: `focus_navigation_follows_linear_tile_order`,
  `focus_navigation_crosses_to_the_adjacent_display`,
  `workspace_focus_cycle_wraps_through_tile_order_in_every_layout`,
  `keyboard_resize_applies_and_persists_the_focused_layout`,
  `balance_applies_and_persists_the_focused_parent_split`,
  `reorder_moves_to_the_adjacent_display_at_a_tile_boundary`,
  `perpendicular_reorder_creates_a_mixed_split_for_three_windows`,
  `layout_toggle_repartitions_and_persists_the_focused_workspace`.
- Unit (FakeBackend), `src/app/tests/gestures.rs`: drag swap preview, edge-nesting preview,
  cross-display drop; `startup.rs` covers gesture classification and `src/app/input.rs` covers
  queued final gestures.
- Fixture replay: `cargo run -- replay fixtures/ghostty-tab-replacement.json` (reconciliation of a
  native-tab replacement, without the runtime).
- Live harness (not run): `scripts/live-ghostty-tabs.sh` for native-tab slot preservation.

## Gotchas

- Tests check requested frames; apps with minimum sizes may refuse them. Spaceman records such
  windows as constrained instead of fighting every cycle, so a "misplaced" tile can be intentional.
- `balance` has no default hotkey; it is CLI-only.
- A window that enters macOS native fullscreen (for example browser video) loses its slot after two
  seconds and returns as a new tile at the end of the layout
  ([known defect](fullscreen-layout.md#gotchas)).
- Floating windows (rules or non-resizable) are owned by a workspace but never tiled or resized.
- An app whose windows span several workspaces sends its new windows to the display under the
  window rather than "its" workspace.
