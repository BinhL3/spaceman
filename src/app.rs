use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel, sync_channel},
    },
    thread,
    time::Duration,
};

use gpui::{Application, Timer};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use crate::{AppInstanceId, Engine, live, macos, status_item};

const TICK_INTERVAL: Duration = Duration::from_millis(250);

enum WorkerCommand {
    Cycle(crate::Rect),
    Stop,
}

struct CycleStatus {
    slots: usize,
    applied: usize,
    failed: usize,
}

pub fn run() -> Result<(), String> {
    let (commands, worker_commands) = sync_channel(1);
    let (worker_results, results) = channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    spawn_worker(worker_commands, worker_results, Arc::clone(&cancelled));

    let application = Application::new();
    application.run(move |cx| {
        if let Some(marker) = MainThreadMarker::new() {
            NSApplication::sharedApplication(marker)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        let actions = match status_item::install() {
            Ok(actions) => actions,
            Err(error) => {
                eprintln!("failed to install Spaceman status item: {error}");
                cx.quit();
                return;
            }
        };
        if !macos::accessibility_trusted() {
            status_item::set_status("Paused · Accessibility required");
        }
        cx.spawn(async move |cx| {
            let mut automatic = false;
            let mut retile_requested = false;
            let mut in_flight = false;

            loop {
                for result in results.try_iter() {
                    in_flight = false;
                    match result {
                        Ok(status) if status.failed > 0 => status_item::set_status(&format!(
                            "Managing {} window{} · {} failed",
                            status.slots,
                            if status.slots == 1 { "" } else { "s" },
                            status.failed,
                        )),
                        Ok(status) => status_item::set_status(&format!(
                            "Managing {} window{} · {} applied",
                            status.slots,
                            if status.slots == 1 { "" } else { "s" },
                            status.applied,
                        )),
                        Err(error) => status_item::set_status(&error),
                    }
                }
                for action in actions.try_iter() {
                    match action {
                        status_item::Action::ToggleAutomatic => {
                            automatic = !automatic;
                            cancelled.store(!automatic, Ordering::Release);
                            status_item::set_automatic(automatic);
                            status_item::set_status(if automatic {
                                "Automatic tiling enabled"
                            } else {
                                "Paused"
                            });
                        }
                        status_item::Action::Retile => retile_requested = true,
                        status_item::Action::OpenAccessibility => open_accessibility_settings(),
                        status_item::Action::Quit => {
                            cancelled.store(true, Ordering::Release);
                            let _ = commands.try_send(WorkerCommand::Stop);
                            let _ = cx.update(|cx| cx.quit());
                            return;
                        }
                    }
                }

                if !in_flight && (automatic || retile_requested) {
                    retile_requested = false;
                    match macos::main_output() {
                        Ok(output) => {
                            cancelled.store(false, Ordering::Release);
                            if commands.try_send(WorkerCommand::Cycle(output)).is_ok() {
                                in_flight = true;
                            }
                        }
                        Err(error) => {
                            status_item::set_status(&format!("Observation error: {error}"))
                        }
                    }
                }
                Timer::after(TICK_INTERVAL).await;
            }
        })
        .detach();
    });
    Ok(())
}

fn spawn_worker(
    commands: Receiver<WorkerCommand>,
    results: std::sync::mpsc::Sender<Result<CycleStatus, String>>,
    cancelled: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut engine = Engine::default();
        let mut known_apps = HashMap::new();
        while let Ok(command) = commands.recv() {
            let WorkerCommand::Cycle(output) = command else {
                return;
            };
            let result = reconcile_and_apply(&mut engine, &mut known_apps, output, &cancelled)
                .map_err(|error| match error {
                    macos::SnapshotError::AccessibilityPermissionRequired => {
                        "Accessibility permission required".into()
                    }
                    _ => format!("Observation error: {error}"),
                });
            if results.send(result).is_err() {
                return;
            }
        }
    });
}

fn reconcile_and_apply(
    engine: &mut Engine,
    known_apps: &mut HashMap<AppInstanceId, String>,
    output: crate::Rect,
    cancelled: &AtomicBool,
) -> Result<CycleStatus, macos::SnapshotError> {
    engine.invalidate();
    let ticket = engine.begin_cycle();
    let (mut observation, settings) = macos::snapshot_in_output(ticket.cycle, output)?;
    live::add_absent_applications(&mut observation, known_apps);
    let reconciliation = engine
        .accept(ticket, &observation, &settings)
        .expect("synchronous app cycle must remain current");
    let placements = live::applicable_placements(&observation, &reconciliation, &settings);
    let results = macos::apply_while(&placements, &settings, || {
        !cancelled.load(Ordering::Acquire)
    });
    let applied = results
        .iter()
        .filter(|result| matches!(result.outcome, macos::ApplyOutcome::Applied))
        .count();
    Ok(CycleStatus {
        slots: reconciliation.model.workspace.slots.len(),
        applied,
        failed: results.len() - applied,
    })
}

fn open_accessibility_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}
