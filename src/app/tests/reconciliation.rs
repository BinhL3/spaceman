use super::*;

#[test]
fn closing_the_only_window_of_a_running_app_reflows_surviving_windows() {
    let terminal = window(10);
    let popup = crate::PhysicalWindowId {
        app: AppInstanceId {
            pid: 2,
            launched_at: 1,
        },
        window_id: 20,
    };
    let backend = FakeBackend {
        frames: HashMap::from([(terminal.clone(), frame(50)), (popup.clone(), frame(500))]),
        observation: Some(crate::NativeObservation {
            cycle: 0,
            mouse_down: false,
            applications: vec![
                crate::AppObservation::Complete {
                    app: crate::ObservedApplication {
                        id: terminal.app.clone(),
                        bundle_id: "com.mitchellh.ghostty".into(),
                    },
                    focused: Some(terminal.clone()),
                    windows: vec![crate::ObservedWindow {
                        id: terminal.clone(),
                        frame: frame(50),
                        resizable: true,
                        title: None,
                    }],
                },
                crate::AppObservation::Complete {
                    app: crate::ObservedApplication {
                        id: popup.app.clone(),
                        bundle_id: "com.raycast.macos".into(),
                    },
                    focused: Some(popup.clone()),
                    windows: vec![crate::ObservedWindow {
                        id: popup.clone(),
                        frame: frame(500),
                        resizable: true,
                        title: None,
                    }],
                },
            ],
        }),
        ..FakeBackend::default()
    };
    let mut runtime = runtime(backend);
    let outputs = outputs();
    let cancelled = AtomicBool::new(false);
    runtime.cycle(outputs.clone(), &cancelled).unwrap();
    assert_eq!(runtime.backend.frames[&terminal].width, 500);

    runtime.backend.frames.remove(&popup);
    runtime.backend.observation = Some(observation_for(
        "com.mitchellh.ghostty",
        &terminal,
        runtime.backend.frames[&terminal],
    ));
    runtime.backend.server_snapshot_unavailable = true;
    runtime.cycle(outputs.clone(), &cancelled).unwrap();
    assert_eq!(runtime.backend.frames[&terminal].width, 500);

    runtime.backend.server_snapshot_unavailable = false;
    runtime.cycle(outputs.clone(), &cancelled).unwrap();

    assert_eq!(runtime.backend.frames[&terminal], outputs.main().frame);
    assert_eq!(
        runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
        vec![terminal]
    );
}

#[test]
fn configured_third_display_has_an_independent_visible_workspace() {
    let config =
        Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'").unwrap();
    let runtime = runtime_with_config(benchmark_backend(0), config);

    let visible = runtime.visible_workspaces(&three_outputs());

    assert_eq!(
        visible
            .iter()
            .filter_map(|resolved| resolved.workspace.number())
            .collect::<Vec<_>>(),
        vec![1, 6, 9]
    );
}

#[test]
fn missing_preferred_display_falls_back_without_double_visibility() {
    let config =
        Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'").unwrap();
    let runtime = runtime_with_config(benchmark_backend(0), config);

    let visible = runtime.visible_workspaces(&outputs());

    assert_eq!(
        visible
            .iter()
            .filter_map(|resolved| resolved.workspace.number())
            .collect::<Vec<_>>(),
        vec![1, 6]
    );
}

#[test]
fn active_preferred_workspace_falls_back_and_returns_after_hotplug() {
    let config =
        Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'").unwrap();
    let mut runtime = runtime_with_config(benchmark_backend(0), config);

    runtime
        .cycle(three_outputs(), &AtomicBool::new(false))
        .unwrap();
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    assert_eq!(
        runtime
            .visible_workspaces(&outputs())
            .iter()
            .filter_map(|resolved| resolved.workspace.number())
            .collect::<Vec<_>>(),
        vec![9, 6]
    );

    runtime
        .cycle(three_outputs(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        runtime
            .visible_workspaces(&three_outputs())
            .iter()
            .filter_map(|resolved| resolved.workspace.number())
            .collect::<Vec<_>>(),
        vec![1, 6, 9]
    );
}

#[test]
fn transient_frame_failure_preserves_inactive_window_ownership() {
    let window = window(10);
    let mut backend = FakeBackend {
        frame_outcome: Some(macos::WindowAccessError::Unavailable),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), frame(50));
    let mut runtime = runtime(backend);
    let workspace = WorkspaceId::new(2).unwrap();
    bind(&mut runtime, workspace, &window);

    let outputs = outputs();
    let visible = runtime.visible_workspaces(&outputs);
    let mut failed = 0;
    let server_frames = runtime.backend.window_server_frames().unwrap();
    runtime.park_inactive(
        &outputs,
        &visible,
        &server_frames,
        &AtomicBool::new(false),
        &mut failed,
    );

    assert_eq!(failed, 1);
    assert_eq!(runtime.workspaces.workspace_of(&window), Some(workspace));
    assert!(runtime.parked.is_empty());
}

#[test]
fn cycle_reconciles_and_applies_through_the_fake_backend() {
    let window = window(10);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation(&window, normal)),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let mut runtime =
        WorkspaceRuntime::with_backend(ManagementScope::Main, backend, HashMap::new());

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .slots
            .len(),
        1
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&window),
        Some(WorkspaceId::ONE)
    );
    assert!(
        runtime
            .backend
            .calls
            .iter()
            .any(|call| matches!(call, BackendCall::Apply(applied, _) if applied == &window))
    );
}

#[test]
fn new_ghostty_window_joins_unique_workspace_across_displays() {
    let existing = window(10);
    let created = window(11);
    let main_frame = frame(50);
    let secondary_frame = frame(1050);
    let mut backend = FakeBackend {
        focused: Some(created.clone()),
        frontmost: Some(created.app.clone()),
        observation: Some(observation_for_windows(
            "com.mitchellh.ghostty",
            &created,
            &[
                (existing.clone(), main_frame),
                (created.clone(), secondary_frame),
            ],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), main_frame);
    backend.frames.insert(created.clone(), secondary_frame);
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &existing);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&existing),
        Some(WorkspaceId::ONE)
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&created),
        Some(WorkspaceId::ONE)
    );
    assert!(runtime.backend.frames[&created].x < 1000);
}

#[test]
fn explicit_route_overrides_unique_app_affinity() {
    let existing = window(10);
    let created = window(11);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &created,
            &[(existing.clone(), frame(50)), (created.clone(), frame(100))],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), frame(50));
    backend.frames.insert(created.clone(), frame(100));
    let config = Config::parse(
        "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'route'\nworkspace = 6",
    )
    .unwrap();
    let mut runtime = runtime_with_config(backend, config);
    bind(&mut runtime, WorkspaceId::ONE, &existing);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&existing),
        Some(WorkspaceId::ONE)
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&created),
        Some(WorkspaceId::SIX)
    );
    assert!(runtime.backend.frames[&created].x >= 1000);
}

#[test]
fn floating_rule_converts_existing_and_new_windows_to_floating_ownership() {
    let existing = window(10);
    let created = window(11);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &created,
            &[
                (existing.clone(), frame(50)),
                (created.clone(), frame(1050)),
            ],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), frame(50));
    backend.frames.insert(created.clone(), frame(1050));
    let config =
        Config::parse("version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'float'")
            .unwrap();
    let mut runtime = runtime_with_config(backend, config);
    bind(&mut runtime, WorkspaceId::ONE, &existing);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(runtime.workspaces.workspace_of(&existing), None);
    assert_eq!(
        runtime.workspaces.floating_workspace_of(&existing),
        Some(WorkspaceId::ONE)
    );
    assert_eq!(runtime.workspaces.workspace_of(&created), None);
    assert_eq!(
        runtime.workspaces.floating_workspace_of(&created),
        Some(WorkspaceId::SIX)
    );
}

#[test]
fn ignore_rule_releases_the_window_and_never_adopts_it() {
    let existing = window(10);
    let ignored = window(11);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &existing,
            &[(existing.clone(), frame(50)), (ignored.clone(), frame(400))],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), frame(50));
    backend.frames.insert(ignored.clone(), frame(400));
    let config =
        Config::parse("version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'ignore'")
            .unwrap();
    let mut runtime = runtime_with_config(backend, config);
    bind(&mut runtime, WorkspaceId::ONE, &existing);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    // Both windows match the ignore rule: the owned one is released and
    // the new one is never adopted; neither is ever parked.
    assert_eq!(runtime.workspaces.workspace_of_any(&existing), None);
    assert_eq!(runtime.workspaces.workspace_of_any(&ignored), None);
    assert!(runtime.parked.is_empty());
}

#[test]
fn app_spanning_workspaces_keeps_current_display_routing() {
    let main = window(10);
    let secondary = window(11);
    let created = window(12);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &created,
            &[
                (main.clone(), frame(50)),
                (secondary.clone(), frame(1050)),
                (created.clone(), frame(1100)),
            ],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(main.clone(), frame(50));
    backend.frames.insert(secondary.clone(), frame(1050));
    backend.frames.insert(created.clone(), frame(1100));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &main);
    bind(&mut runtime, WorkspaceId::SIX, &secondary);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
    runtime.workspaces.refresh_ownership(WorkspaceId::SIX);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&created),
        Some(WorkspaceId::SIX)
    );
}

#[test]
fn simultaneous_new_windows_use_the_same_frozen_affinity() {
    let existing = window(10);
    let first = window(11);
    let second = window(12);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &second,
            &[
                (existing.clone(), frame(50)),
                (first.clone(), frame(1050)),
                (second.clone(), frame(1100)),
            ],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), frame(50));
    backend.frames.insert(first.clone(), frame(1050));
    backend.frames.insert(second.clone(), frame(1100));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &existing);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&first),
        Some(WorkspaceId::ONE)
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&second),
        Some(WorkspaceId::ONE)
    );
    assert!(runtime.backend.frames[&first].x < 1000);
    assert!(runtime.backend.frames[&second].x < 1000);
}

#[test]
fn new_window_for_unique_inactive_workspace_is_parked() {
    let existing = window(10);
    let created = window(11);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows(
            "com.example.Test",
            &created,
            &[
                (existing.clone(), frame(50)),
                (created.clone(), frame(1050)),
            ],
        )),
        ..FakeBackend::default()
    };
    backend.frames.insert(existing.clone(), frame(50));
    backend.frames.insert(created.clone(), frame(1050));
    let mut runtime = runtime(backend);
    let inactive = WorkspaceId::new(2).unwrap();
    bind(&mut runtime, inactive, &existing);
    runtime.workspaces.refresh_ownership(inactive);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(runtime.workspaces.workspace_of(&created), Some(inactive));
    assert!(runtime.parked.contains_key(&created));
    assert!(
        !outputs()
            .union()
            .contains_center(runtime.backend.frames[&created])
    );
}

#[test]
fn on_screen_window_omitted_from_one_snapshot_keeps_its_slot() {
    let kept = window(10);
    let closed = window(11);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows("com.example.Test", &kept, &[])),
        ..FakeBackend::default()
    };
    backend.frames.insert(kept.clone(), frame(50));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &kept);
    bind(&mut runtime, WorkspaceId::ONE, &closed);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&kept),
        Some(WorkspaceId::ONE),
        "the window server still shows the window on screen"
    );
    assert_eq!(
        runtime.workspaces.workspace_of(&closed),
        Some(WorkspaceId::ONE),
        "the app's whole snapshot is distrusted while an omission settles"
    );

    runtime
        .omitted_windows
        .insert(kept.clone(), Instant::now() - OMISSION_SETTLE * 2);
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&kept),
        None,
        "an omission that outlasts the settle window is believed"
    );
    assert_eq!(runtime.workspaces.workspace_of(&closed), None);
    assert!(runtime.omitted_windows.is_empty());
}

#[test]
fn omission_settling_ends_when_the_window_is_observed_again() {
    let window = window(10);
    let mut backend = FakeBackend {
        observation: Some(observation_for_windows("com.example.Test", &window, &[])),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), frame(50));
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &window);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
    assert!(runtime.omitted_windows.contains_key(&window));

    runtime.backend.observation = Some(observation(&window, frame(50)));
    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert!(runtime.omitted_windows.is_empty());
    assert_eq!(
        runtime.workspaces.workspace_of(&window),
        Some(WorkspaceId::ONE)
    );
}

#[test]
fn floating_rule_owns_window_without_adding_it_to_layout() {
    let window = window(10);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation(&window, normal)),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let config =
        Config::parse("version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'float'")
            .unwrap();
    let mut runtime = runtime_with_config(backend, config);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .slots
            .is_empty()
    );
    assert_eq!(runtime.workspaces.workspace_of(&window), None);
    assert_eq!(
        runtime.workspaces.floating_workspace_of(&window),
        Some(WorkspaceId::ONE)
    );
    assert!(
        !runtime
            .backend
            .calls
            .iter()
            .any(|call| matches!(call, BackendCall::Apply(_, _)))
    );
}

#[test]
fn tiled_window_becoming_non_resizable_moves_to_floating_ownership() {
    let window = window(10);
    let normal = frame(50);
    let mut observed = observation(&window, normal);
    let crate::AppObservation::Complete { windows, .. } = &mut observed.applications[0] else {
        unreachable!();
    };
    windows[0].resizable = false;
    let mut backend = FakeBackend {
        observation: Some(observed),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let mut runtime = runtime(backend);
    bind(&mut runtime, WorkspaceId::ONE, &window);
    runtime.workspaces.refresh_ownership(WorkspaceId::ONE);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(runtime.workspaces.workspace_of(&window), None);
    assert_eq!(
        runtime.workspaces.floating_workspace_of(&window),
        Some(WorkspaceId::ONE)
    );
    assert!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .slots
            .is_empty()
    );
}

#[test]
fn route_rule_moves_only_inserted_window_to_inactive_workspace() {
    let window = window(10);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation(&window, normal)),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let config = Config::parse(
        "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'route'\nworkspace = 2",
    )
    .unwrap();
    let mut runtime = runtime_with_config(backend, config);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&window),
        Some(WorkspaceId::new(2).unwrap())
    );
    assert!(runtime.parked.contains_key(&window));
    assert!(runtime.backend.workspace_state.is_some());
    assert!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .bound_windows()
            .is_empty()
    );
}

#[test]
fn cross_display_route_survives_later_reconciliation_in_same_cycle() {
    let window = window(10);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation(&window, normal)),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let config = Config::parse(
        "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'route'\nworkspace = 6",
    )
    .unwrap();
    let mut runtime = runtime_with_config(backend, config);

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(
        runtime.workspaces.workspace_of(&window),
        Some(WorkspaceId::SIX)
    );
    assert!(runtime.backend.frames[&window].x >= 1000);
}

#[test]
fn unmanaged_route_is_rejected_before_insertion() {
    let window = window(10);
    let normal = frame(50);
    let mut backend = FakeBackend {
        observation: Some(observation(&window, normal)),
        ..FakeBackend::default()
    };
    backend.frames.insert(window.clone(), normal);
    let config = Config::parse(
        "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'route'\nworkspace = 6",
    )
    .unwrap();
    let mut runtime = WorkspaceRuntime::with_backend_and_config(
        ManagementScope::Main,
        backend,
        HashMap::new(),
        config,
    );

    assert!(runtime.cycle(outputs(), &AtomicBool::new(false)).is_err());
    assert_eq!(runtime.workspaces.workspace_of(&window), None);
}

#[test]
fn restored_slots_for_a_dead_application_are_retired() {
    let dead = window(10);
    let mut persisted = Workspaces::default();
    persisted.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
        id: SlotId(1),
        weight: 1,
        binding: BindingState::Bound(dead.clone()),
    });
    persisted.refresh_ownership(WorkspaceId::ONE);
    let backend = FakeBackend {
        observation: Some(crate::NativeObservation {
            cycle: 0,
            mouse_down: false,
            applications: Vec::new(),
        }),
        running_apps: Some(HashSet::new()),
        ..FakeBackend::default()
    };
    let mut runtime = WorkspaceRuntime::with_backend_config_state(
        ManagementScope::Both,
        backend,
        HashMap::new(),
        Config::default(),
        Some(persisted.state()),
    );

    runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

    assert_eq!(runtime.workspaces.workspace_of(&dead), None);
    assert!(
        runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .model()
            .workspace
            .slots
            .is_empty()
    );
    assert!(runtime.backend.workspace_state.is_some());
}
