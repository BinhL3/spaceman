# Scrolling Strip (Labs)

Keep windows wide instead of shrinking tiles: a workspace's tiles become one horizontal ribbon of
columns (each 33%, 50%, 66%, 80% or 100% of the display, 50% by default) seen through a viewport
camera that glides on a spring, as in niri. Columns out of view peek 16 points past a free display
edge and are recorded like an inactive workspace's windows. An experiment: off by default, behind
`[labs] scrolling_strip`.

## Sub-features

```ts
Lab switch (Settings › Labs card / [labs] scrolling_strip / config set labs.scrolling_strip)
├── Off (default) -> Option+Shift+S not registered; ctl strip answers
│                    "Scrolling Strip is off (enable it in Settings › Labs)"; every workspace tiles as usual
└── On  -> hot-reloads: worker gate opens, Option+Shift+S registers           // strip.lab

Toggle (focused workspace) Option+Shift+S / ctl strip                         // strip.toggle
├── On : columns at their widths from the left edge, no glide; the rest peek or park durably
├── Off: every peeking or parked column restored, unchanged split layout re-applied
└── Fullscreen Layout on, or Option+/ -> strip off (the two modes exclude each other)

Ribbon and camera (pure math: src/strip.rs)                                   // strip.viewport
├── ribbon(): columns end to end at their widths
├── minimal_scroll(): the camera moves only when the focused column leaves the view, by the least distance
├── rest_layout(): in view -> its frame (a partly visible column runs off a free edge)
│                  out of view past a free edge -> 16 pt sliver (PEEK_PX), recorded as parked
│                  out of view past an edge shared with another display -> parked
└── Nothing ever crosses a shared edge (ScrollingDisplayBounds clamps it inside)

Glide (src/app/strip/motion.rs -> src/app/app_loop/glide.rs)                   // strip.glide
├── Worker: aims CameraSpring (critically damped, stiffness 800) from its current position and
│           velocity; a moved column gets its own offset spring; records every column the glide
│           takes out of view in parked-windows.json first; sends a StripMotion to the main thread
├── Main thread: CADisplayLink runs only while a glide does; each vsync samples the springs and
│                writes positions through InteractivePresenter (per-app lanes, newest frame wins,
│                no readback, enhanced UI held off for the whole glide)
├── Rapid taps -> the running spring is retargeted, one continuous glide
├── Settled -> link stops; worker lays the strip out once, peeks/parks out-of-view columns,
│              retires recovery frames of columns back in view
└── Switch, move, toggle, pause or quit mid-glide -> glide halted before windows are touched

Keys on a strip                                                               // strip.keys
├── Option+H/L        -> previous/next column, no wrap; past an end -> adjacent display
├── Option+Shift+H/L  -> move the focused column (neighbors glide to their new places)
├── Option+-/=        -> width 33% · 50% · 66% · 80% · 100%; unsized columns follow the last pick
├── Option-backtick   -> next column, wrapping
└── Option+J/K        -> adjacent display (no vertical axis on a strip)

Following focus                                                               // strip.follow
├── Click / Dock / Command-Tab onto any column, even one out of view -> camera glides to it
├── New tiled window -> becomes the focused column
└── Option+Shift+Space -> the column floats out and the strip closes the gap; tiling it again
                          brings it back as the focused column

Persistence and recovery                                                      // strip.persist
├── workspace-state.json "strip": enabled, widths, default width, anchor (all optional)
├── Switch away -> every column parked; back -> only the viewport revealed, then peeks
├── Pause / quit / restart -> every peeking or parked column restored (parked-windows.json)
├── An app drags an out-of-view column back -> put back at its sliver or parking spot
├── Display added or removed -> parked columns re-parked; peeks past a new seam parked
└── Ghostty native-tab replacement -> keeps the slot, its position and its width
```

Unit (FakeBackend), `src/app/tests/strip.rs`:
`lab_off_refuses_the_strip_and_tiles_as_usual_until_it_is_turned_on`,
`stepping_along_the_strip_peeks_parks_and_reveals_columns_durably`,
`focus_from_outside_spaceman_scrolls_to_a_parked_column`,
`pause_and_quit_restore_every_peeking_and_parked_column`,
`switching_away_parks_every_column_and_back_reveals_only_the_viewport`,
`reordering_and_width_presets_follow_the_focused_column`,
`turning_the_strip_off_reveals_every_column_in_the_split_layout`,
`ghostty_native_tab_replacement_keeps_the_strip_column_and_width`,
`a_strip_column_that_drifts_back_on_screen_is_put_back`,
`docking_a_display_parks_columns_that_peeked_past_the_new_seam`,
`a_focus_step_glides_and_settles_into_the_rest_layout`, `rapid_taps_retarget_one_glide`, and
`switching_workspaces_halts_a_running_glide`. Ribbon, camera and glide math: `src/strip/tests.rs`
(minimal scroll, peeks, seams, 120 Hz intermediate frames, retarget continuity). Enhanced UI hand-off:
`a_restore_owed_during_a_hold_is_paid_when_the_last_sink_closes` (`src/macos/presenter/holds.rs`).
Persistence: `persisted_strip_round_trips_and_excludes_fullscreen_layout`. Config:
`parses_labs_with_every_experiment_off_by_default`, `labs_switches_round_trip_and_survive_other_edits`
(CLI), `labs_switch_round_trips_and_leaves_an_all_off_table_out` (Settings). Shortcut gating:
`scrolling_strip_shortcut_registers_only_while_its_lab_is_on`.

## How To Get To It (User POV)

```ts
Settings › Labs -> Scrolling Strip switch on
Focus a window on the workspace
└── Option+Shift+S / spaceman ctl strip -> toggle
```

## Existing Checks

- Unit tests above. No live harness drives the strip against real apps yet; the glide was recorded
  by hand in a macOS VM.

## Gotchas

- **Glides write positions only.** A width change resizes the column at once, then the camera and
  its neighbors glide. `agent.log` logs each glide (`strip glide … over Nms`) and how many frames the
  main thread presented.
- **Shared edges.** A column partly in view on the side of another display is kept inside its own
  display, so it sits under its neighbor instead of running off the edge.
- **A closed column's neighbor appears on the next cycle.** Reconciliation retires the closed window
  first; the column that comes into view is laid out by the following observation cycle.
