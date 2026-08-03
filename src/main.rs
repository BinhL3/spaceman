use std::{
    collections::{HashMap, HashSet},
    env, fs,
    process::ExitCode,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use spaceman::{
    Model, NativeObservation, Reconciliation, ReconciliationEvidence, Settings, reconcile,
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
                .and_then(|cycles| run_watch(cycles, false))
                .and_then(to_json),
            (Some(cycles), Some(flag), None) if flag == "--apply" => parse_cycles(&cycles)
                .and_then(|cycles| run_watch(cycles, true))
                .and_then(to_json),
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
fn run_watch(cycles: u64, should_apply: bool) -> Result<WatchOutput, String> {
    let mut model = Model::default();
    let mut evidence = ReconciliationEvidence::default();
    let mut known_apps = HashMap::new();
    let mut steps = Vec::new();

    for cycle in 1..=cycles {
        let (mut observation, settings) =
            spaceman::macos::snapshot(cycle).map_err(|error| error.to_string())?;
        add_absent_applications(&mut observation, &mut known_apps);
        let reconciliation = reconcile(&model, &observation, &evidence, &settings);
        let incomplete = observation
            .applications
            .iter()
            .any(|app| matches!(app, spaceman::AppObservation::Unavailable { .. }));
        let apply_results = (should_apply && !incomplete)
            .then(|| spaceman::macos::apply(&reconciliation.placements, &settings));

        evidence.previous_placements = reconciliation
            .placements
            .iter()
            .map(|placement| (placement.slot, placement.frame))
            .collect();
        model = reconciliation.model.clone();
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
    observation.applications.extend(
        known_apps
            .iter()
            .filter(|(app, _)| !present.contains(*app))
            .map(|(id, bundle_id)| spaceman::AppObservation::Complete {
                app: spaceman::ObservedApplication {
                    id: id.clone(),
                    bundle_id: bundle_id.clone(),
                },
                focused: None,
                windows: Vec::new(),
            }),
    );
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
