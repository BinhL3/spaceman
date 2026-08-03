use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{self, Write},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use spaceman::{
    Engine, Model, NativeObservation, Reconciliation, ReconciliationEvidence, Settings, reconcile,
};

#[derive(Deserialize)]
struct Replay {
    #[serde(default)]
    model: Model,
    #[serde(default)]
    evidence: ReconciliationEvidence,
    settings: Settings,
    observations: Vec<NativeObservation>,
}

#[derive(Serialize)]
struct ReplayOutput {
    steps: Vec<spaceman::Reconciliation>,
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
struct SnapshotOutput {
    observation: NativeObservation,
    reconciliation: Reconciliation,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_results: Option<Vec<spaceman::macos::ApplyResult>>,
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
struct WatchOutput {
    steps: Vec<SnapshotOutput>,
}

fn main() -> ExitCode {
    let mut args = env::args();
    let executable = args.next().unwrap_or_else(|| "spaceman".into());
    let Some(command) = args.next() else {
        print_usage(&executable);
        return ExitCode::from(2);
    };

    let result = match command.as_str() {
        "replay" => match (args.next(), args.next()) {
            (Some(path), None) => run_replay(&path).and_then(to_json),
            _ => {
                print_usage(&executable);
                return ExitCode::from(2);
            }
        },
        #[cfg(target_os = "macos")]
        "snapshot" if args.next().is_none() => run_snapshot().and_then(to_json),
        #[cfg(target_os = "macos")]
        "retile" => match (args.next(), args.next()) {
            (None, None) => run_retile(false).and_then(to_json),
            (Some(flag), None) if flag == "--apply" => run_retile(true).and_then(to_json),
            _ => {
                print_usage(&executable);
                return ExitCode::from(2);
            }
        },
        #[cfg(target_os = "macos")]
        "watch" => match (args.next(), args.next(), args.next()) {
            (Some(cycles), None, None) => parse_cycles(&cycles)
                .and_then(|cycles| run_watch(cycles, false, None))
                .and_then(to_json),
            (Some(cycles), Some(flag), None) if flag == "--apply" => parse_cycles(&cycles)
                .and_then(|cycles| run_watch(cycles, true, None))
                .and_then(to_json),
            _ => {
                print_usage(&executable);
                return ExitCode::from(2);
            }
        },
        #[cfg(target_os = "macos")]
        "watch-window" => match (args.next(), args.next(), args.next(), args.next()) {
            (Some(window_id), Some(cycles), None, None) => parse_window_id(&window_id)
                .and_then(|window_id| parse_cycles(&cycles).map(|cycles| (window_id, cycles)))
                .and_then(|(window_id, cycles)| run_watch(cycles, false, Some(window_id)))
                .and_then(to_json),
            (Some(window_id), Some(cycles), Some(flag), None) if flag == "--apply" => {
                parse_window_id(&window_id)
                    .and_then(|window_id| parse_cycles(&cycles).map(|cycles| (window_id, cycles)))
                    .and_then(|(window_id, cycles)| run_watch(cycles, true, Some(window_id)))
                    .and_then(to_json)
            }
            _ => {
                print_usage(&executable);
                return ExitCode::from(2);
            }
        },
        #[cfg(target_os = "macos")]
        "run" => match (args.next(), args.next()) {
            (None, None) => run_persistent(false),
            (Some(flag), None) if flag == "--apply" => run_persistent(true),
            _ => {
                print_usage(&executable);
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("unknown command: {command}");
            print_usage(&executable);
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage(executable: &str) {
    eprintln!("usage: {executable} replay <fixture.json>");
    #[cfg(target_os = "macos")]
    eprintln!("       {executable} snapshot");
    #[cfg(target_os = "macos")]
    eprintln!("       {executable} retile [--apply]");
    #[cfg(target_os = "macos")]
    eprintln!("       {executable} watch <cycles> [--apply]");
    #[cfg(target_os = "macos")]
    eprintln!("       {executable} watch-window <window-id> <cycles> [--apply]");
    #[cfg(target_os = "macos")]
    eprintln!("       {executable} run [--apply]");
}

fn parse_window_id(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|window_id| *window_id > 0)
        .ok_or_else(|| "window ID must be a positive integer".into())
}

fn parse_cycles(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|cycles| *cycles > 0)
        .ok_or_else(|| "cycles must be a positive integer".into())
}

fn to_json(value: impl Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(&value)
        .map_err(|error| format!("failed to encode output: {error}"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn only_mismatched_observed_frames_need_application() {
        let app = spaceman::AppInstanceId {
            pid: 10,
            launched_at: 1,
        };
        let window = spaceman::PhysicalWindowId {
            app: app.clone(),
            window_id: 20,
        };
        let settings = Settings {
            output: spaceman::Rect {
                width: 200,
                height: 100,
                ..spaceman::Rect::default()
            },
            frame_tolerance: 2,
        };
        let mut observation = NativeObservation {
            cycle: 1,
            mouse_down: false,
            applications: vec![spaceman::AppObservation::Complete {
                app: spaceman::ObservedApplication {
                    id: app,
                    bundle_id: "com.example.Editor".into(),
                },
                focused: Some(window.clone()),
                windows: vec![spaceman::ObservedWindow {
                    id: window,
                    frame: spaceman::Rect {
                        width: 100,
                        height: 100,
                        ..spaceman::Rect::default()
                    },
                }],
            }],
        };
        let reconciliation = reconcile(
            &Model::default(),
            &observation,
            &ReconciliationEvidence::default(),
            &settings,
        );

        assert_eq!(
            applicable_placements(&observation, &reconciliation, &settings).len(),
            1
        );
        let spaceman::AppObservation::Complete { windows, .. } = &mut observation.applications[0]
        else {
            unreachable!()
        };
        windows[0].frame = reconciliation.placements[0].frame;
        assert!(applicable_placements(&observation, &reconciliation, &settings).is_empty());
    }
}

#[cfg(target_os = "macos")]
fn run_snapshot() -> Result<SnapshotOutput, String> {
    run_retile(false)
}

#[cfg(target_os = "macos")]
fn run_retile(should_apply: bool) -> Result<SnapshotOutput, String> {
    let (observation, settings) =
        spaceman::macos::snapshot(1).map_err(|error| error.to_string())?;
    if should_apply
        && observation
            .applications
            .iter()
            .any(|app| matches!(app, spaceman::AppObservation::Unavailable { .. }))
    {
        return Err("refusing to apply an incomplete window observation".into());
    }
    let reconciliation = reconcile(
        &Model::default(),
        &observation,
        &ReconciliationEvidence::default(),
        &settings,
    );
    let apply_results =
        should_apply.then(|| spaceman::macos::apply(&reconciliation.placements, &settings));
    Ok(SnapshotOutput {
        observation,
        reconciliation,
        apply_results,
    })
}

#[cfg(target_os = "macos")]
fn run_watch(
    cycles: u64,
    should_apply: bool,
    initial_window_id: Option<u32>,
) -> Result<WatchOutput, String> {
    let mut engine = Engine::default();
    let mut known_apps = HashMap::new();
    let mut scope: Option<WindowScope> = None;
    let mut steps = Vec::new();

    for cycle in 1..=cycles {
        engine.invalidate();
        let ticket = engine.begin_cycle();
        let (mut observation, mut settings) = match (initial_window_id, scope.as_ref()) {
            (Some(window_id), None) => {
                spaceman::macos::snapshot_for_window(ticket.cycle, window_id)
            }
            (Some(_), Some(scope)) => {
                spaceman::macos::snapshot_in_output(ticket.cycle, scope.output)
            }
            (None, _) => spaceman::macos::snapshot(ticket.cycle),
        }
        .map_err(|error| error.to_string())?;
        if let Some(initial_window_id) = initial_window_id {
            let active_scope = match &mut scope {
                Some(scope) => scope,
                None => scope.insert(WindowScope::discover(
                    &observation,
                    initial_window_id,
                    settings.output,
                )?),
            };
            active_scope.filter(&mut observation);
            settings.output = active_scope.frame;
        } else {
            add_absent_applications(&mut observation, &mut known_apps);
        }
        let reconciliation = engine
            .accept(ticket, &observation, &settings)
            .map_err(|error| format!("stale synchronous cycle: {error:?}"))?;
        let placements = applicable_placements(&observation, &reconciliation, &settings);
        let apply_results = should_apply.then(|| spaceman::macos::apply(&placements, &settings));

        steps.push(SnapshotOutput {
            observation,
            reconciliation,
            apply_results,
        });

        if cycle < cycles {
            thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(WatchOutput { steps })
}

#[cfg(target_os = "macos")]
fn run_persistent(should_apply: bool) -> Result<String, String> {
    let running = Arc::new(AtomicBool::new(true));
    let stop = Arc::clone(&running);
    ctrlc::set_handler(move || stop.store(false, Ordering::Release))
        .map_err(|error| format!("failed to install signal handler: {error}"))?;

    let mut engine = Engine::default();
    let mut known_apps = HashMap::new();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut completed_cycles = 0_u64;

    while running.load(Ordering::Acquire) {
        engine.invalidate();
        let ticket = engine.begin_cycle();
        let (mut observation, settings) = match spaceman::macos::snapshot(ticket.cycle) {
            Ok(snapshot) => snapshot,
            Err(spaceman::macos::SnapshotError::AccessibilityPermissionRequired) => {
                return Err(
                    "Accessibility permission is required for the persistent runtime".into(),
                );
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "type": "snapshot_error",
                        "cycle": ticket.cycle,
                        "error": error.to_string(),
                    })
                );
                thread::sleep(Duration::from_millis(250));
                continue;
            }
        };
        if !running.load(Ordering::Acquire) {
            engine.invalidate();
            break;
        }
        add_absent_applications(&mut observation, &mut known_apps);
        let reconciliation = engine
            .accept(ticket, &observation, &settings)
            .map_err(|error| format!("stale synchronous cycle: {error:?}"))?;
        let placements = applicable_placements(&observation, &reconciliation, &settings);
        let apply_results = (should_apply && running.load(Ordering::Acquire)).then(|| {
            spaceman::macos::apply_while(&placements, &settings, || running.load(Ordering::Acquire))
        });
        let step = SnapshotOutput {
            observation,
            reconciliation,
            apply_results,
        };
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({ "type": "cycle", "step": step }),
        )
        .map_err(|error| format!("failed to encode runtime step: {error}"))?;
        output
            .write_all(b"\n")
            .and_then(|()| output.flush())
            .map_err(|error| format!("failed to write runtime step: {error}"))?;
        completed_cycles += 1;

        for _ in 0..10 {
            if !running.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    Ok(serde_json::json!({
        "type": "stopped",
        "cycles": completed_cycles,
        "revision": engine.revision(),
    })
    .to_string())
}

#[cfg(target_os = "macos")]
struct WindowScope {
    app: spaceman::AppInstanceId,
    bundle_id: String,
    frame: spaceman::Rect,
    output: spaceman::Rect,
    excluded_window_ids: HashSet<u32>,
    authorized_window_ids: HashSet<u32>,
    missing_cycles: u8,
}

#[cfg(target_os = "macos")]
impl WindowScope {
    fn discover(
        observation: &NativeObservation,
        window_id: u32,
        output: spaceman::Rect,
    ) -> Result<Self, String> {
        observation
            .applications
            .iter()
            .find_map(|app| match app {
                spaceman::AppObservation::Complete { app, windows, .. } => windows
                    .iter()
                    .find(|window| window.id.window_id == window_id)
                    .map(|window| Self {
                        app: app.id.clone(),
                        bundle_id: app.bundle_id.clone(),
                        frame: window.frame,
                        output,
                        excluded_window_ids: windows
                            .iter()
                            .filter(|existing| existing.id.window_id != window_id)
                            .map(|existing| existing.id.window_id)
                            .collect(),
                        authorized_window_ids: HashSet::from([window_id]),
                        missing_cycles: 0,
                    }),
                _ => None,
            })
            .ok_or_else(|| format!("window {window_id} is not currently eligible"))
    }

    fn filter(&mut self, observation: &mut NativeObservation) {
        let mut scoped = observation.applications.drain(..).find(|app| match app {
            spaceman::AppObservation::Complete { app, .. } => app.id == self.app,
            spaceman::AppObservation::Unavailable { app, .. }
            | spaceman::AppObservation::Terminated { app } => app == &self.app,
        });
        if let Some(spaceman::AppObservation::Complete {
            focused, windows, ..
        }) = &mut scoped
        {
            windows.retain(|window| {
                !self.excluded_window_ids.contains(&window.id.window_id)
                    && window.frame.approximately_equals(self.frame, 100)
            });
            let selected = focused
                .as_ref()
                .and_then(|focused| windows.iter().find(|window| window.id == *focused))
                .or_else(|| {
                    windows
                        .iter()
                        .find(|window| self.authorized_window_ids.contains(&window.id.window_id))
                })
                .or_else(|| (windows.len() == 1).then(|| &windows[0]))
                .map(|window| window.id.clone());
            windows.retain(|window| selected.as_ref() == Some(&window.id));
            if let Some(selected) = selected {
                self.authorized_window_ids.insert(selected.window_id);
            }
            if focused
                .as_ref()
                .is_some_and(|focused| !windows.iter().any(|window| window.id == *focused))
            {
                *focused = None;
            }
            if windows.is_empty() {
                self.missing_cycles = self.missing_cycles.saturating_add(1);
            } else {
                self.missing_cycles = 0;
            }
        }
        if scoped.is_none() {
            self.missing_cycles = self.missing_cycles.saturating_add(1);
        }
        if self.missing_cycles <= 8
            && !matches!(
                scoped,
                Some(spaceman::AppObservation::Complete { ref windows, .. }) if !windows.is_empty()
            )
        {
            scoped = Some(spaceman::AppObservation::Unavailable {
                app: self.app.clone(),
                error: "scoped window is settling or absent".into(),
            });
        }
        observation.applications =
            vec![
                scoped.unwrap_or_else(|| spaceman::AppObservation::Complete {
                    app: spaceman::ObservedApplication {
                        id: self.app.clone(),
                        bundle_id: self.bundle_id.clone(),
                    },
                    focused: None,
                    windows: Vec::new(),
                }),
            ];
    }
}

#[cfg(target_os = "macos")]
fn add_absent_applications(
    observation: &mut NativeObservation,
    known_apps: &mut HashMap<spaceman::AppInstanceId, String>,
) {
    let present: HashSet<_> = observation
        .applications
        .iter()
        .map(|app| match app {
            spaceman::AppObservation::Complete { app, .. } => app.id.clone(),
            spaceman::AppObservation::Unavailable { app, .. }
            | spaceman::AppObservation::Terminated { app } => app.clone(),
        })
        .collect();
    for app in &observation.applications {
        if let spaceman::AppObservation::Complete { app, .. } = app {
            known_apps.insert(app.id.clone(), app.bundle_id.clone());
        }
    }
    let missing: Vec<_> = known_apps
        .iter()
        .filter(|(app, _)| !present.contains(*app))
        .map(|(app, bundle_id)| (app.clone(), bundle_id.clone()))
        .collect();
    for (app, bundle_id) in missing {
        if spaceman::macos::is_process_incarnation_running(&app) {
            observation
                .applications
                .push(spaceman::AppObservation::Complete {
                    app: spaceman::ObservedApplication { id: app, bundle_id },
                    focused: None,
                    windows: Vec::new(),
                });
        } else {
            known_apps.remove(&app);
            observation
                .applications
                .push(spaceman::AppObservation::Terminated { app });
        }
    }
}

#[cfg(target_os = "macos")]
fn applicable_placements(
    observation: &NativeObservation,
    reconciliation: &Reconciliation,
    settings: &Settings,
) -> Vec<spaceman::Placement> {
    let observed_frames: HashMap<_, _> = observation
        .applications
        .iter()
        .flat_map(|app| match app {
            spaceman::AppObservation::Complete { windows, .. } => windows.as_slice(),
            spaceman::AppObservation::Unavailable { .. }
            | spaceman::AppObservation::Terminated { .. } => &[],
        })
        .map(|window| (&window.id, window.frame))
        .collect();
    reconciliation
        .placements
        .iter()
        .filter(|placement| {
            placement
                .window
                .as_ref()
                .and_then(|window| observed_frames.get(window))
                .is_some_and(|frame| {
                    !frame.approximately_equals(placement.frame, settings.frame_tolerance)
                })
        })
        .cloned()
        .collect()
}

fn run_replay(path: &str) -> Result<ReplayOutput, String> {
    let input =
        fs::read_to_string(path).map_err(|error| format!("failed to read {path}: {error}"))?;
    let replay: Replay =
        serde_json::from_str(&input).map_err(|error| format!("invalid fixture {path}: {error}"))?;
    replay
        .model
        .validate()
        .map_err(|error| format!("invalid fixture model: {error}"))?;
    if replay.settings.output.width <= 0
        || replay.settings.output.height <= 0
        || replay.settings.frame_tolerance < 0
    {
        return Err("invalid fixture settings".into());
    }
    let mut model = replay.model;
    let mut evidence = replay.evidence;
    let mut steps = Vec::new();

    for observation in replay.observations {
        let result = reconcile(&model, &observation, &evidence, &replay.settings);
        evidence.previous_placements = result
            .placements
            .iter()
            .map(|placement| (placement.slot, placement.frame))
            .collect();
        model = result.model.clone();
        steps.push(result);
    }

    Ok(ReplayOutput { steps })
}
