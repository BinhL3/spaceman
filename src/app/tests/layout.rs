use super::*;

#[test]
fn ghostty_native_tab_replacement_preserves_workspace_and_fullscreen_layout() {
    let previous = window(10);
    let replacement = window(11);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation_for("com.mitchellh.ghostty", &previous, normal)),
        focused: Some(previous.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(previous.clone(), normal);
    let config = Config::parse(
        "version = 1\n[[rules]]\nbundle_id = 'com.mitchellh.ghostty'\naction = 'route'\nworkspace = 2",
    )
    .unwrap();
    let mut runtime = runtime_with_config(backend, config);
    bind(&mut runtime, WorkspaceId::ONE, &previous);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    runtime
        .toggle_fullscreen_layout(outputs(), &AtomicBool::new(false))
        .unwrap();
    let accepted = runtime.backend.frames[&previous];
    runtime.backend.observation = Some(observation_for(
        "com.mitchellh.ghostty",
        &previous,
        accepted,
    ));
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    runtime.backend.frames.remove(&previous);
    runtime.backend.frames.insert(replacement.clone(), accepted);
    runtime.backend.observation = Some(observation_for(
        "com.mitchellh.ghostty",
        &replacement,
        accepted,
    ));

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&replacement),
        Some(WorkspaceId::ONE)
    );
    assert!(runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert_eq!(
        runtime.workspaces.preferred_focus(WorkspaceId::ONE),
        Some(replacement.clone())
    );
    assert_eq!(runtime.backend.frames[&replacement], outputs().main().frame);
    assert!(!runtime.parked.contains_key(&replacement));
}

#[test]
fn fullscreen_layout_toggles_without_changing_tile_order() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(second.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);
    let outputs = outputs();

    let status = runtime
        .toggle_fullscreen_layout(outputs.clone(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.frames[&first], outputs.main().frame);
    assert_eq!(runtime.backend.frames[&second], outputs.main().frame);
    assert!(runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert!(matches!(
        status.layouts[0].mode,
        status_item::WorkspaceLayoutMode::Fullscreen
    ));
    assert_eq!(
        runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
        vec![first.clone(), second.clone()]
    );

    let status = runtime
        .toggle_fullscreen_layout(outputs, &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.frames[&first].width, 500);
    assert_eq!(runtime.backend.frames[&second].x, 500);
    assert!(!runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert!(matches!(
        status.layouts[0].mode,
        status_item::WorkspaceLayoutMode::Split(_)
    ));
    assert_eq!(
        runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
        vec![first, second]
    );
}

#[test]
fn removing_the_last_window_exits_fullscreen_layout() {
    let window = window(10);
    let mut backend = FakeBackend {
        focused: Some(window.clone()),
        observation: Some(observation(&window, frame(50))),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), frame(50));
    let mut runtime = runtime(backend);
    let outputs = outputs();
    let cancelled = AtomicBool::new(false);
    runtime.cycle(outputs.clone(), &cancelled).unwrap();
    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();
    runtime
        .pending_fullscreen_focus_restore
        .insert(WorkspaceId::ONE);

    runtime.backend.frames.remove(&window);
    runtime.backend.observation = Some(crate::NativeObservation {
        cycle: 0,
        mouse_down: false,
        applications: Vec::new(),
    });
    let status = runtime.cycle(outputs, &cancelled).unwrap();

    assert!(!runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert!(runtime.pending_fullscreen_focus_restore.is_empty());
    assert!(matches!(
        status.layouts[0].mode,
        status_item::WorkspaceLayoutMode::Split(_)
    ));
}

#[test]
fn focus_navigation_follows_linear_tile_order() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(second.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);

    runtime
        .focus_relative(Direction::Left, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.focused, Some(first.clone()));
    assert!(matches!(
        runtime.backend.calls.last(),
        Some(BackendCall::Focus(window)) if window == &first
    ));

    runtime
        .focus_relative(Direction::Right, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.focused, Some(second));
}

#[test]
fn workspace_focus_cycle_wraps_through_tile_order_in_every_layout() {
    let first = window(10);
    let second = window(20);
    let third = window(30);
    let mut backend = FakeBackend {
        focused: Some(second.clone()),
        ..FakeBackend::default()
    };
    for (window, initial) in [(&first, 50), (&second, 500), (&third, 750)] {
        backend.frames.insert(window.clone(), frame(initial));
    }
    let mut runtime = runtime(backend);
    for window in [&first, &second, &third] {
        bind(&mut runtime, WorkspaceId::ONE, window);
    }
    let outputs = outputs();
    let cancelled = AtomicBool::new(false);

    runtime.cycle_focus(outputs.clone(), &cancelled).unwrap();
    assert_eq!(runtime.backend.focused, Some(third.clone()));
    assert_eq!(runtime.workspaces.preferred_focus(WorkspaceId::ONE), None);
    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();

    runtime.cycle_focus(outputs.clone(), &cancelled).unwrap();
    assert_eq!(runtime.backend.focused, Some(first.clone()));
    runtime.cycle_focus(outputs, &cancelled).unwrap();
    assert_eq!(runtime.backend.focused, Some(second));
}

#[test]
fn fullscreen_layout_remembers_native_same_app_focus_changes() {
    let first = window(10);
    let second = window(20);
    let output = outputs().main().frame;
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        frontmost: Some(first.app.clone()),
        observation: Some(observation_for_windows(
            "com.example.Test",
            &first,
            &[(first.clone(), frame(50)), (second.clone(), frame(500))],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);
    let outputs = outputs();
    let cancelled = AtomicBool::new(false);
    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();

    runtime.backend.focused = Some(second.clone());
    runtime.backend.observation = Some(observation_for_windows(
        "com.example.Test",
        &second,
        &[(first.clone(), output), (second.clone(), output)],
    ));
    runtime.cycle(outputs, &cancelled).unwrap();

    assert_eq!(
        runtime.workspaces.preferred_focus(WorkspaceId::ONE),
        Some(second)
    );
}

#[test]
fn fullscreen_layout_activates_persisted_cross_app_focus_after_restart() {
    let first = window(10);
    let focused = crate::PhysicalWindowId {
        app: AppInstanceId {
            pid: 2,
            launched_at: 2,
        },
        window_id: 20,
    };
    let output = outputs().main().frame;
    let mut persisted = Workspaces::default();
    for (id, window) in [(10, &first), (20, &focused)] {
        persisted.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
            id: SlotId(id),
            weight: 1,
            binding: BindingState::Bound(window.clone()),
        });
    }
    persisted.refresh_ownership(WorkspaceId::ONE);
    persisted.remember_focus(WorkspaceId::ONE, &focused);
    persisted.set_fullscreen_layout(WorkspaceId::ONE, true);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        frontmost: Some(first.app.clone()),
        observation: Some(crate::NativeObservation {
            cycle: 0,
            mouse_down: false,
            applications: vec![
                crate::AppObservation::Complete {
                    app: crate::ObservedApplication {
                        id: first.app.clone(),
                        bundle_id: "com.example.First".into(),
                    },
                    focused: Some(first.clone()),
                    windows: vec![crate::ObservedWindow {
                        id: first.clone(),
                        frame: output,
                        resizable: true,
                        title: None,
                    }],
                },
                crate::AppObservation::Complete {
                    app: crate::ObservedApplication {
                        id: focused.app.clone(),
                        bundle_id: "com.example.Second".into(),
                    },
                    focused: Some(focused.clone()),
                    windows: vec![crate::ObservedWindow {
                        id: focused.clone(),
                        frame: output,
                        resizable: true,
                        title: None,
                    }],
                },
            ],
        }),
        ..FakeBackend::default()
    };
    backend.frames.insert(first, output);
    backend.frames.insert(focused.clone(), output);
    let mut runtime = WorkspaceRuntime::with_backend_config_state(
        ManagementScope::Both,
        backend,
        HashMap::new(),
        Config::default(),
        Some(persisted.state()),
    );

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert!(
        runtime
            .backend
            .calls
            .contains(&BackendCall::Focus(focused.clone()))
    );
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    assert_eq!(
        runtime.workspaces.preferred_focus(WorkspaceId::ONE),
        Some(focused)
    );
}

#[test]
fn failed_fullscreen_focus_restore_does_not_fight_later_focus() {
    let first = window(10);
    let persisted_focus = window(20);
    let output = outputs().main().frame;
    let mut persisted = Workspaces::default();
    for (id, window) in [(10, &first), (20, &persisted_focus)] {
        persisted.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
            id: SlotId(id),
            weight: 1,
            binding: BindingState::Bound(window.clone()),
        });
    }
    persisted.refresh_ownership(WorkspaceId::ONE);
    persisted.remember_focus(WorkspaceId::ONE, &persisted_focus);
    persisted.set_fullscreen_layout(WorkspaceId::ONE, true);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        frontmost: Some(first.app.clone()),
        focus_outcome: Some(false),
        observation: Some(observation_for_windows(
            "com.example.Test",
            &first,
            &[(first.clone(), output), (persisted_focus.clone(), output)],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), output);
    backend.frames.insert(persisted_focus.clone(), output);
    let mut runtime = WorkspaceRuntime::with_backend_config_state(
        ManagementScope::Both,
        backend,
        HashMap::new(),
        Config::default(),
        Some(persisted.state()),
    );

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime
            .backend
            .calls
            .iter()
            .filter(|call| **call == BackendCall::Focus(persisted_focus.clone()))
            .count(),
        1
    );
    assert_eq!(
        runtime.workspaces.preferred_focus(WorkspaceId::ONE),
        Some(first)
    );
}

#[test]
fn layout_toggle_exits_fullscreen_and_applies_the_next_split_axis() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);
    let outputs = outputs();
    let cancelled = AtomicBool::new(false);
    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();

    runtime.toggle_layout(outputs, &cancelled).unwrap();

    assert!(!runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert_eq!(
        runtime.workspaces.engine(WorkspaceId::ONE).root_axis(),
        crate::Axis::Vertical
    );
    assert_eq!(runtime.backend.frames[&first].height, 400);
    assert_eq!(runtime.backend.frames[&second].y, 400);
}

#[test]
fn focus_navigation_crosses_to_the_adjacent_display() {
    let main = window(10);
    let secondary = window(20);
    let mut backend = FakeBackend {
        focused: Some(main.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(main.clone(), frame(50));
    backend.frames.insert(secondary.clone(), frame(1050));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &main);
    bind(&mut runtime, WorkspaceId::SIX, &secondary);

    runtime
        .focus_relative(Direction::Right, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.focused, Some(secondary));

    runtime.backend.focused = Some(main.clone());
    runtime
        .focus_relative(Direction::Up, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.focused, Some(main));
}

#[test]
fn layout_toggle_repartitions_and_persists_the_focused_workspace() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);

    runtime
        .toggle_layout(outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .axis,
        crate::Axis::Vertical
    );
    assert_eq!(runtime.backend.frames[&first].height, 400);
    assert_eq!(runtime.backend.frames[&second].y, 400);
    assert!(runtime.backend.workspace_state.is_some());
}

#[test]
fn resizing_a_tile_updates_weights_and_live_previews_its_sibling() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);

    runtime
        .resize_window(
            first.clone(),
            crate::Rect {
                x: 0,
                y: 0,
                width: 600,
                height: 800,
            },
            true,
            outputs(),
            &AtomicBool::new(false),
        )
        .unwrap();

    let slots = &runtime
        .workspaces
        .engine(WorkspaceId::ONE)
        .model()
        .workspace
        .slots;
    assert_eq!((slots[0].weight, slots[1].weight), (600, 400));
    assert_eq!(runtime.backend.frames[&second].x, 600);
    assert_eq!(runtime.backend.frames[&first], frame(50));
    assert!(runtime.backend.workspace_state.is_none());

    runtime
        .resize_window(
            first,
            crate::Rect {
                x: 0,
                y: 0,
                width: 600,
                height: 800,
            },
            false,
            outputs(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert!(runtime.backend.workspace_state.is_some());
}

#[test]
fn keyboard_resize_applies_and_persists_the_focused_layout() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);

    runtime
        .resize_focused(ResizeDirection::Grow, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.frames[&first].width, 550);
    assert_eq!(runtime.backend.frames[&second].x, 550);
    assert!(runtime.backend.workspace_state.is_some());
}

#[test]
fn balance_applies_and_persists_the_focused_parent_split() {
    let first = window(10);
    let second = window(20);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(second.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &second);
    runtime
        .resize_focused(ResizeDirection::Grow, outputs(), &AtomicBool::new(false))
        .unwrap();
    runtime.backend.workspace_state = None;

    runtime
        .balance_focused(outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(runtime.backend.frames[&first].width, 500);
    assert_eq!(runtime.backend.frames[&second].x, 500);
    assert!(runtime.backend.workspace_state.is_some());
}

#[test]
fn reorder_moves_to_the_adjacent_display_at_a_tile_boundary() {
    let moving = window(10);
    let secondary = window(20);
    let mut backend = FakeBackend {
        focused: Some(moving.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(moving.clone(), frame(50));
    backend.frames.insert(secondary.clone(), frame(1050));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &moving);
    bind(&mut runtime, WorkspaceId::SIX, &secondary);

    runtime
        .reorder_focused(Direction::Right, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&moving),
        Some(WorkspaceId::SIX)
    );
    assert_eq!(runtime.backend.focused, Some(moving));
}

#[test]
fn perpendicular_reorder_reorients_a_two_window_workspace_before_crossing_displays() {
    let top = window(10);
    let bottom = window(20);
    let other_display = window(30);
    let mut backend = FakeBackend {
        focused: Some(bottom.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(top.clone(), frame(50));
    backend.frames.insert(bottom.clone(), frame(500));
    backend.frames.insert(other_display.clone(), frame(1050));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &top);
    bind(&mut runtime, WorkspaceId::ONE, &bottom);
    bind(&mut runtime, WorkspaceId::SIX, &other_display);
    runtime
        .workspaces
        .engine_mut(WorkspaceId::ONE)
        .toggle_axis();

    runtime
        .reorder_focused(Direction::Right, outputs(), &AtomicBool::new(false))
        .unwrap();

    assert_eq!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .axis,
        crate::Axis::Horizontal
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&bottom),
        Some(WorkspaceId::ONE)
    );
    assert_eq!(
        runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
        vec![top, bottom]
    );
}

#[test]
fn perpendicular_reorder_creates_a_mixed_split_for_three_windows() {
    let first = window(10);
    let moving = window(20);
    let third = window(30);
    let mut backend = FakeBackend {
        focused: Some(moving.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(moving.clone(), frame(400));
    backend.frames.insert(third.clone(), frame(750));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &moving);
    bind(&mut runtime, WorkspaceId::ONE, &third);

    runtime
        .reorder_focused(Direction::Up, outputs(), &AtomicBool::new(false))
        .unwrap();

    let placements = runtime
        .workspaces
        .engine(WorkspaceId::ONE)
        .placements(&Settings::for_output(outputs().main().frame));
    assert_eq!(placements[0].window.as_ref(), Some(&moving));
    assert_eq!(
        placements[0].frame,
        crate::Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400
        }
    );
    assert_eq!(
        placements[1].frame,
        crate::Rect {
            x: 0,
            y: 400,
            width: 500,
            height: 400
        }
    );
    assert_eq!(
        placements[2].frame,
        crate::Rect {
            x: 500,
            y: 400,
            width: 500,
            height: 400
        }
    );
    assert!(runtime.backend.workspace_state.is_some());
}

#[test]
fn switching_away_and_back_preserves_fullscreen_layout() {
    let first = window(10);
    let focused = window(20);
    let incoming = window(30);
    let outputs = outputs();
    let displays = outputs.frames().collect::<Vec<_>>();
    let incoming_normal = frame(100);
    let incoming_parked =
        crate::geometry::parked_frame(outputs.main().frame, &displays, incoming_normal);
    let mut backend = FakeBackend {
        focused: Some(focused.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(focused.clone(), frame(500));
    backend.frames.insert(incoming.clone(), incoming_parked);
    let mut parked = HashMap::new();
    parked.insert(incoming.clone(), incoming_normal);
    let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &focused);
    let target = WorkspaceId::new(2).unwrap();
    bind(&mut runtime, target, &incoming);
    let cancelled = AtomicBool::new(false);

    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();
    runtime.switch(target, outputs.clone(), &cancelled).unwrap();
    runtime
        .switch(WorkspaceId::ONE, outputs.clone(), &cancelled)
        .unwrap();

    assert!(runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert_eq!(runtime.backend.frames[&first], outputs.main().frame);
    assert_eq!(runtime.backend.frames[&focused], outputs.main().frame);
    assert_eq!(runtime.backend.focused, Some(focused));
}

#[test]
fn switching_away_and_back_restores_fullscreen_layout_focus() {
    let first = window(10);
    let focused = window(20);
    let incoming = window(30);
    let outputs = outputs();
    let displays = outputs.frames().collect::<Vec<_>>();
    let incoming_normal = frame(100);
    let incoming_parked =
        crate::geometry::parked_frame(outputs.main().frame, &displays, incoming_normal);
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(focused.clone(), frame(500));
    backend.frames.insert(incoming.clone(), incoming_parked);
    let mut parked = HashMap::new();
    parked.insert(incoming.clone(), incoming_normal);
    let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &focused);
    let target = WorkspaceId::new(2).unwrap();
    bind(&mut runtime, target, &incoming);
    let cancelled = AtomicBool::new(false);

    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();
    runtime.backend.focused = Some(focused.clone());
    runtime.switch(target, outputs.clone(), &cancelled).unwrap();
    runtime
        .switch(WorkspaceId::ONE, outputs.clone(), &cancelled)
        .unwrap();

    assert!(runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
    assert_eq!(runtime.backend.frames[&first], outputs.main().frame);
    assert_eq!(runtime.backend.frames[&focused], outputs.main().frame);
    assert_eq!(runtime.backend.focused, Some(focused));
}

#[test]
fn selecting_an_active_fullscreen_workspace_preserves_its_current_focus() {
    let first = window(10);
    let focused = window(20);
    let outputs = outputs();
    let mut backend = FakeBackend {
        focused: Some(first.clone()),
        ..FakeBackend::default()
    };
    backend.frames.insert(first.clone(), frame(50));
    backend.frames.insert(focused.clone(), frame(500));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &first);
    bind(&mut runtime, WorkspaceId::ONE, &focused);
    let cancelled = AtomicBool::new(false);
    runtime
        .toggle_fullscreen_layout(outputs.clone(), &cancelled)
        .unwrap();
    runtime.backend.focused = Some(focused.clone());

    runtime
        .switch(WorkspaceId::ONE, outputs, &cancelled)
        .unwrap();

    assert_eq!(runtime.backend.focused, Some(focused));
}
