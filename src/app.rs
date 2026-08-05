use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
    thread,
    time::{Duration, Instant},
};

use gpui::{Application, Timer};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use crate::{
    AppInstanceId, Decision, Direction, DisplaySelector, NavigationTile, ResizeDirection, Settings,
    WorkspaceId, Workspaces,
    config::{Config, WindowDisposition},
    control::{self, Command as ControlCommand, Request as ControlRequest},
    hotkeys::{HotkeyPlan, Hotkeys},
    live, macos, status_item,
};

const TICK_INTERVAL: Duration = Duration::from_millis(25);
const FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const APPLICATION_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RESTORE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const APPLICATION_ACTIVATION_SETTLE: Duration = Duration::from_millis(50);
const KEYBOARD_RESIZE_INCREMENT: i32 = 50;

const fn refresh_due(
    window_created: bool,
    event_dirty: bool,
    fallback_due: bool,
    activation_ready: bool,
) -> bool {
    window_created || (activation_ready && (event_dirty || fallback_due))
}

#[derive(Clone, Copy)]
enum ManagementScope {
    Both,
    Main,
    Secondary,
}

impl ManagementScope {
    fn from_environment() -> Result<Self, String> {
        match std::env::var("SPACEMAN_MANAGED_DISPLAY") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::Both),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("SPACEMAN_MANAGED_DISPLAY must be valid UTF-8".into())
            }
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "main" => Ok(Self::Main),
            "secondary" => Ok(Self::Secondary),
            "both" => Ok(Self::Both),
            _ => Err(format!(
                "SPACEMAN_MANAGED_DISPLAY must be main, secondary, or both; got {value:?}"
            )),
        }
    }

    fn manages_selector(self, display: &DisplaySelector) -> bool {
        matches!(
            (self, display),
            (Self::Both, _)
                | (Self::Main, DisplaySelector::Main)
                | (Self::Secondary, DisplaySelector::Secondary)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    ToggleAutomatic,
    Retile,
    SwitchWorkspace(WorkspaceId),
    WorkspaceBackAndForth,
    MoveFocused(WorkspaceId),
    MoveApp(String, WorkspaceId),
    Focus(Direction),
    Reorder(Direction),
    ToggleLayout,
    Resize(ResizeDirection),
    Balance,
    ToggleMaximize,
    OpenAccessibility,
    Restart,
    Quit,
}

enum WorkerCommand {
    Cycle(macos::DisplayOutputs),
    Switch(WorkspaceId, macos::DisplayOutputs),
    WorkspaceBackAndForth(macos::DisplayOutputs),
    MoveFocused(WorkspaceId, macos::DisplayOutputs),
    MoveApp(String, WorkspaceId, macos::DisplayOutputs),
    Focus(Direction, macos::DisplayOutputs),
    Reorder(Direction, macos::DisplayOutputs),
    DropWindow(
        crate::PhysicalWindowId,
        (i32, i32),
        bool,
        macos::DisplayOutputs,
    ),
    ResizeWindow(
        crate::PhysicalWindowId,
        crate::Rect,
        bool,
        macos::DisplayOutputs,
    ),
    ToggleLayout(macos::DisplayOutputs),
    Resize(ResizeDirection, macos::DisplayOutputs),
    Balance(macos::DisplayOutputs),
    ToggleMaximize(macos::DisplayOutputs),
    RestoreAll,
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FinalGesture {
    Drop(crate::PhysicalWindowId, (i32, i32)),
    Resize(crate::PhysicalWindowId, crate::Rect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExitIntent {
    Quit,
    Restart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestorePlan {
    Now,
    AfterWorker,
}

const fn restore_plan(worker_busy: bool) -> RestorePlan {
    if worker_busy {
        RestorePlan::AfterWorker
    } else {
        RestorePlan::Now
    }
}

const fn exit_restore_plan(_intent: ExitIntent, worker_busy: bool) -> RestorePlan {
    restore_plan(worker_busy)
}

struct CycleStatus {
    active: Vec<WorkspaceId>,
    layouts: Vec<(u8, Vec<crate::Rect>)>,
    failed: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
struct RuntimeDebug {
    frontmost: Option<AppInstanceId>,
    focused: Option<crate::PhysicalWindowId>,
    active_workspaces: Vec<WorkspaceId>,
    displays: Vec<DebugDisplay>,
    tiles: Vec<NavigationTile>,
    targets: DebugTargets,
}

#[derive(Clone, Debug, serde::Serialize)]
struct DebugDisplay {
    id: String,
    name: String,
    frame: crate::Rect,
    workspace: WorkspaceId,
    axis: crate::Axis,
    layout: Vec<crate::Rect>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct DebugTargets {
    left: Option<crate::PhysicalWindowId>,
    down: Option<crate::PhysicalWindowId>,
    up: Option<crate::PhysicalWindowId>,
    right: Option<crate::PhysicalWindowId>,
}

struct LayoutStatus {
    applied: usize,
    failed: usize,
    blocked: bool,
}

#[derive(Default)]
struct CycleCounts {
    applied: usize,
    failed: usize,
}

#[derive(Clone)]
struct ResolvedWorkspace {
    workspace: WorkspaceId,
    selector: DisplaySelector,
    display_id: String,
    output: crate::Rect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenWindowAffinity {
    Owned(WorkspaceId),
    Floating,
    ExplicitRoute(WorkspaceId),
    Inferred(WorkspaceId),
    CurrentDisplay,
}

struct FrozenAffinityPolicy {
    windows: HashMap<crate::PhysicalWindowId, FrozenWindowAffinity>,
    visible_workspaces: HashSet<WorkspaceId>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ParkedWindows(
    #[serde(with = "crate::serde_map")] HashMap<crate::PhysicalWindowId, crate::Rect>,
);

#[derive(serde::Serialize)]
struct ParkedWindowsRef<'a>(
    #[serde(with = "crate::serde_map")] &'a HashMap<crate::PhysicalWindowId, crate::Rect>,
);

trait WorkspaceBackend {
    fn snapshot(
        &mut self,
        cycle: u64,
        outputs: &macos::DisplayOutputs,
    ) -> Result<crate::NativeObservation, String>;
    fn is_process_running(&mut self, app: &AppInstanceId) -> bool;
    fn apply_observed(
        &mut self,
        placements: &[crate::Placement],
        settings: &Settings,
        cancelled: &AtomicBool,
    ) -> Vec<macos::ApplyResult> {
        self.apply_managed(placements, settings, cancelled)
    }
    fn frame(
        &mut self,
        window: &crate::PhysicalWindowId,
    ) -> Result<crate::Rect, macos::WindowAccessError>;
    fn set_position(
        &mut self,
        window: &crate::PhysicalWindowId,
        target: crate::Rect,
    ) -> macos::ApplyOutcome;
    fn focused_window(&mut self) -> Option<crate::PhysicalWindowId>;
    fn frontmost_application(&mut self) -> Option<AppInstanceId>;
    fn focus_window(&mut self, window: &crate::PhysicalWindowId) -> bool;
    fn apply_managed(
        &mut self,
        placements: &[crate::Placement],
        settings: &Settings,
        cancelled: &AtomicBool,
    ) -> Vec<macos::ApplyResult>;
    fn apply_managed_during_drag(
        &mut self,
        placements: &[crate::Placement],
        settings: &Settings,
        dragged: &crate::PhysicalWindowId,
        cancelled: &AtomicBool,
    ) -> Vec<macos::ApplyResult>;
    fn persist_parked(
        &mut self,
        parked: &HashMap<crate::PhysicalWindowId, crate::Rect>,
    ) -> Result<(), String>;
    fn persist_workspace_state(
        &mut self,
        state: &crate::workspace::WorkspaceState,
    ) -> Result<(), String>;
}

struct MacWorkspaceBackend;

impl WorkspaceBackend for MacWorkspaceBackend {
    fn snapshot(
        &mut self,
        cycle: u64,
        outputs: &macos::DisplayOutputs,
    ) -> Result<crate::NativeObservation, String> {
        macos::snapshot_in_outputs(cycle, outputs).map_err(|error| error.to_string())
    }

    fn is_process_running(&mut self, app: &AppInstanceId) -> bool {
        macos::is_process_incarnation_running(app)
    }

    fn frame(
        &mut self,
        window: &crate::PhysicalWindowId,
    ) -> Result<crate::Rect, macos::WindowAccessError> {
        macos::frame(window)
    }

    fn set_position(
        &mut self,
        window: &crate::PhysicalWindowId,
        target: crate::Rect,
    ) -> macos::ApplyOutcome {
        macos::set_position_only(window, target)
    }

    fn focused_window(&mut self) -> Option<crate::PhysicalWindowId> {
        macos::focused_window()
    }

    fn frontmost_application(&mut self) -> Option<AppInstanceId> {
        macos::frontmost_application()
    }

    fn focus_window(&mut self, window: &crate::PhysicalWindowId) -> bool {
        macos::focus_window(window)
    }

    fn apply_managed(
        &mut self,
        placements: &[crate::Placement],
        settings: &Settings,
        cancelled: &AtomicBool,
    ) -> Vec<macos::ApplyResult> {
        macos::apply_managed_while(placements, settings, || !cancelled.load(Ordering::Acquire))
    }

    fn apply_managed_during_drag(
        &mut self,
        placements: &[crate::Placement],
        settings: &Settings,
        dragged: &crate::PhysicalWindowId,
        cancelled: &AtomicBool,
    ) -> Vec<macos::ApplyResult> {
        macos::apply_managed_during_drag(placements, settings, dragged, || {
            !cancelled.load(Ordering::Acquire)
        })
    }

    fn persist_parked(
        &mut self,
        parked: &HashMap<crate::PhysicalWindowId, crate::Rect>,
    ) -> Result<(), String> {
        persist_parked(parked)
    }

    fn persist_workspace_state(
        &mut self,
        state: &crate::workspace::WorkspaceState,
    ) -> Result<(), String> {
        persist_workspace_state(state)
    }
}

enum WorkerResult {
    Started {
        failed: usize,
    },
    Status {
        result: Result<CycleStatus, String>,
        debug: Box<RuntimeDebug>,
    },
    Restored {
        failed: usize,
    },
}

struct AppLoop {
    actions: Receiver<Action>,
    actions_sender: Sender<Action>,
    commands: Sender<WorkerCommand>,
    results: Receiver<WorkerResult>,
    cancelled: Arc<AtomicBool>,
    scope: ManagementScope,
    events: macos::WindowEventMonitor,
    hotkey_workspaces: Vec<WorkspaceId>,
    controls: Receiver<ControlRequest>,
    _control_server: control::Server,
}

pub fn run() -> Result<(), String> {
    let scope = ManagementScope::from_environment()?;
    let parked = load_parked()?;
    let (config, workspace_state) =
        recover_startup_metadata(Config::load(), load_workspace_state());
    let mut configured_workspaces = Workspaces::default();
    config.configure_workspaces(&mut configured_workspaces);
    let hotkey_workspaces = WorkspaceId::all()
        .filter(|workspace| scope.manages_selector(configured_workspaces.assignment(*workspace)))
        .collect();
    let (commands, worker_commands) = channel();
    let (worker_results, results) = channel();
    let (actions_sender, actions) = channel();
    let (control_requests, controls) = channel();
    let control_server = control::listen(control_requests)?;
    let signal_actions = actions_sender.clone();
    ctrlc::set_handler(move || {
        let _ = signal_actions.send(Action::Quit);
    })
    .map_err(|error| format!("failed to install app signal handler: {error}"))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    spawn_worker(
        worker_commands,
        worker_results,
        Arc::clone(&cancelled),
        scope,
        parked,
        config,
        workspace_state,
    );

    let application = Application::new();
    application.run(move |cx| {
        if let Some(marker) = MainThreadMarker::new() {
            NSApplication::sharedApplication(marker)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        if let Err(error) = status_item::install(actions_sender.clone()) {
            eprintln!("failed to install Spaceman status item: {error}");
            cx.quit();
            return;
        }
        match scope {
            ManagementScope::Both => status_item::set_workspace_numbers(&[1, 6]),
            ManagementScope::Main => status_item::set_workspace_numbers(&[1]),
            ManagementScope::Secondary => status_item::set_workspace_numbers(&[6]),
        }
        if !macos::accessibility_trusted() {
            status_item::set_status("Paused · Accessibility required");
        }
        let events = macos::WindowEventMonitor::new();
        cx.spawn(async move |cx| {
            run_app_loop(
                cx,
                AppLoop {
                    actions,
                    actions_sender,
                    commands,
                    results,
                    cancelled,
                    scope,
                    events,
                    hotkey_workspaces,
                    controls,
                    _control_server: control_server,
                },
            )
            .await;
        })
        .detach();
    });
    Ok(())
}

async fn run_app_loop(cx: &mut gpui::AsyncApp, app: AppLoop) {
    let AppLoop {
        actions,
        actions_sender,
        commands,
        results,
        cancelled,
        scope,
        events,
        hotkey_workspaces,
        controls,
        _control_server,
    } = app;
    let (mut _hotkeys, hotkey_error) = match HotkeyPlan::current(&hotkey_workspaces)
        .and_then(|plan| Hotkeys::install(actions_sender.clone(), plan))
    {
        Ok(hotkeys) => (Some(hotkeys), None),
        Err(error) => (None, Some(format!("Hotkeys unavailable: {error}"))),
    };
    let mut automatic = _hotkeys.is_some();
    let mut worker_busy = false;
    let mut exit_intent = None;
    let mut restore_pending = false;
    let mut restoring = false;
    let mut next_restore_retry = None;
    let mut active_workspaces = match scope {
        ManagementScope::Both => vec![WorkspaceId::ONE, WorkspaceId::SIX],
        ManagementScope::Main => vec![WorkspaceId::ONE],
        ManagementScope::Secondary => vec![WorkspaceId::SIX],
    };
    let mut last_error = hotkey_error;
    let mut latest_debug: Option<RuntimeDebug> = None;
    let mut pending_action = None;
    let mut pending_gesture = None;
    let mut last_refresh = Instant::now() - FALLBACK_REFRESH_INTERVAL;
    let mut last_application_refresh = last_refresh;
    let mut last_hotkey_refresh = last_refresh;
    let mut mouse_was_down = macos::mouse_down();
    let mut dragged_window = None;
    let mut drag_start_frame = None;
    let mut window_was_dragged = false;
    let mut window_was_resized = false;
    let mut last_manipulated_frame = None;
    let mut last_drag_point = None;
    let mut frontmost_application = macos::frontmost_application();
    let mut activation_refresh_at = None;

    status_item::set_automatic(automatic);
    if automatic {
        status_item::set_status("Automatic tiling enabled");
        send_with_outputs(&commands, WorkerCommand::Cycle, &mut worker_busy);
    } else {
        status_item::set_status(last_error.as_deref().unwrap_or("Paused"));
        send_restore(&commands, &mut worker_busy, &mut restoring);
    }

    loop {
        loop {
            let result = match results.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    _hotkeys = None;
                    status_item::set_automatic(false);
                    status_item::set_status("Worker stopped unexpectedly");
                    if exit_intent.is_some() {
                        let _ = cx.update(|cx| cx.quit());
                        return;
                    }
                    automatic = false;
                    worker_busy = false;
                    break;
                }
            };
            match result {
                WorkerResult::Started { failed: 0 } if !automatic => {
                    status_item::set_status(last_error.as_deref().unwrap_or("Paused"));
                }
                WorkerResult::Started { failed: 0 } => {}
                WorkerResult::Started { failed } => {
                    restoring = true;
                    next_restore_retry = Some(Instant::now() + RESTORE_RETRY_INTERVAL);
                    status_item::set_status(&format!("Restoring windows · {failed} failed"));
                }
                WorkerResult::Status { result, debug } if automatic => {
                    worker_busy = false;
                    cancelled.store(false, Ordering::Release);
                    latest_debug = Some(*debug);
                    last_error = result.as_ref().err().cloned();
                    if let Ok(status) = &result {
                        active_workspaces.clone_from(&status.active);
                    }
                    show_result(result);
                }
                WorkerResult::Status { result, debug } => {
                    worker_busy = false;
                    latest_debug = Some(*debug);
                    if let Err(error) = result {
                        last_error = Some(error);
                    }
                }
                WorkerResult::Restored { failed: 0 } if exit_intent.is_some() => {
                    let _ = commands.send(WorkerCommand::Stop);
                    let _ = cx.update(|cx| cx.quit());
                    return;
                }
                WorkerResult::Restored { failed: 0 } => {
                    worker_busy = false;
                    restoring = false;
                    status_item::set_status(last_error.as_deref().unwrap_or("Paused"));
                }
                WorkerResult::Restored { failed } => {
                    worker_busy = false;
                    restoring = true;
                    next_restore_retry = Some(Instant::now() + RESTORE_RETRY_INTERVAL);
                    status_item::set_status(&format!("Restoring windows · {failed} failed"));
                }
            }
            if restore_pending && !worker_busy {
                restore_pending = false;
                send_restore(&commands, &mut worker_busy, &mut restoring);
                if !worker_busy && exit_intent.is_some() {
                    let _ = cx.update(|cx| cx.quit());
                    return;
                }
            }
        }
        for request in controls.try_iter() {
            let response = match request.command {
                ControlCommand::Status => Ok(serde_json::json!({
                    "mode": if exit_intent == Some(ExitIntent::Restart) {
                        "restarting"
                    } else if exit_intent == Some(ExitIntent::Quit) {
                        "quitting"
                    } else if restoring || restore_pending {
                        "restoring"
                    } else if automatic {
                        "automatic"
                    } else {
                        "paused"
                    },
                    "busy": worker_busy,
                    "last_error": last_error.as_deref(),
                    "active_workspaces": active_workspaces
                        .iter()
                        .map(|workspace| workspace.number())
                        .collect::<Vec<_>>(),
                })
                .to_string()),
                ControlCommand::Debug => latest_debug
                    .as_ref()
                    .ok_or_else(|| "runtime debug state is not ready".into())
                    .and_then(|debug| {
                        serde_json::to_string(debug).map_err(|error| error.to_string())
                    }),
                ControlCommand::Switch(workspace) if automatic => actions_sender
                    .send(Action::SwitchWorkspace(workspace))
                    .map(|()| format!("switch to workspace {} queued", workspace.number()))
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::WorkspaceBackAndForth if automatic => actions_sender
                    .send(Action::WorkspaceBackAndForth)
                    .map(|()| "workspace back-and-forth queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Move(workspace) if automatic => actions_sender
                    .send(Action::MoveFocused(workspace))
                    .map(|()| format!("move to workspace {} queued", workspace.number()))
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::MoveApp(ref bundle_id, workspace) if automatic => actions_sender
                    .send(Action::MoveApp(bundle_id.clone(), workspace))
                    .map(|()| format!("move app to workspace {} queued", workspace.number()))
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Focus(direction) if automatic => actions_sender
                    .send(Action::Focus(direction))
                    .map(|()| "focus queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::MoveDirection(direction) if automatic => actions_sender
                    .send(Action::Reorder(direction))
                    .map(|()| "directional move queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Layout if automatic => actions_sender
                    .send(Action::ToggleLayout)
                    .map(|()| "layout toggle queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Resize(direction) if automatic => actions_sender
                    .send(Action::Resize(direction))
                    .map(|()| "resize queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Balance if automatic => actions_sender
                    .send(Action::Balance)
                    .map(|()| "balance queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Retile if automatic => actions_sender
                    .send(Action::Retile)
                    .map(|()| "retile queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Restart if worker_busy => {
                    Err("Spaceman is busy; retry restart".into())
                }
                ControlCommand::Restart => actions_sender
                    .send(Action::Restart)
                    .map(|()| "restart queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Pause if automatic => actions_sender
                    .send(Action::ToggleAutomatic)
                    .map(|()| "pause queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Pause => Ok("already paused".into()),
                ControlCommand::Resume if restoring || restore_pending => {
                    Err("windows are still being restored".into())
                }
                ControlCommand::Resume if automatic => Ok("already automatic".into()),
                ControlCommand::Resume => actions_sender
                    .send(Action::ToggleAutomatic)
                    .map(|()| "resume queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Quit => actions_sender
                    .send(Action::Quit)
                    .map(|()| "quit queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Switch(_)
                | ControlCommand::WorkspaceBackAndForth
                | ControlCommand::Move(_)
                | ControlCommand::MoveApp(_, _)
                | ControlCommand::Focus(_)
                | ControlCommand::MoveDirection(_)
                | ControlCommand::Layout
                | ControlCommand::Resize(_)
                | ControlCommand::Balance
                | ControlCommand::Retile => Err("automatic tiling is paused".into()),
            };
            request.respond(response);
        }
        for action in actions.try_iter() {
            if exit_intent.is_some() {
                continue;
            }
            match action {
                Action::ToggleAutomatic if automatic => {
                    automatic = false;
                    cancelled.store(true, Ordering::Release);
                    _hotkeys = None;
                    pending_action = None;
                    if worker_busy {
                        restore_pending = true;
                    } else {
                        send_restore(&commands, &mut worker_busy, &mut restoring);
                    }
                    status_item::set_automatic(false);
                    status_item::set_status(if worker_busy {
                        "Paused"
                    } else {
                        "Paused · worker unavailable"
                    });
                }
                Action::ToggleAutomatic if restoring || restore_pending => {
                    status_item::set_status("Restoring windows...");
                }
                Action::ToggleAutomatic => {
                    match HotkeyPlan::current(&hotkey_workspaces)
                        .and_then(|plan| Hotkeys::install(actions_sender.clone(), plan))
                    {
                        Ok(registered) => {
                            _hotkeys = Some(registered);
                            automatic = true;
                            last_error = None;
                            cancelled.store(false, Ordering::Release);
                            events.invalidate();
                            status_item::set_automatic(true);
                            status_item::set_status(match scope {
                                ManagementScope::Both => {
                                    "Automatic tiling enabled · Workspaces 1 / 6"
                                }
                                ManagementScope::Main => "Automatic tiling enabled · Workspace 1",
                                ManagementScope::Secondary => {
                                    "Automatic tiling enabled · Workspace 6"
                                }
                            });
                        }
                        Err(error) => {
                            let error = format!("Hotkeys unavailable: {error}");
                            last_error = Some(error.clone());
                            match restore_plan(worker_busy) {
                                RestorePlan::Now => {
                                    send_restore(&commands, &mut worker_busy, &mut restoring);
                                }
                                RestorePlan::AfterWorker => restore_pending = true,
                            }
                            status_item::set_status(&error);
                        }
                    }
                }
                Action::Retile if automatic && !worker_busy => {
                    pending_action = None;
                    cancelled.store(false, Ordering::Release);
                    send_with_outputs(&commands, WorkerCommand::Cycle, &mut worker_busy)
                }
                Action::Retile if automatic => {
                    queue_latest(&mut pending_action, Action::Retile, &cancelled);
                }
                Action::Retile => {}
                Action::SwitchWorkspace(workspace) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::Switch(workspace, outputs),
                        &mut worker_busy,
                    )
                }
                Action::SwitchWorkspace(workspace) if automatic => {
                    queue_latest(
                        &mut pending_action,
                        Action::SwitchWorkspace(workspace),
                        &cancelled,
                    );
                }
                Action::WorkspaceBackAndForth if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        WorkerCommand::WorkspaceBackAndForth,
                        &mut worker_busy,
                    );
                }
                Action::WorkspaceBackAndForth if automatic => {
                    queue_latest(
                        &mut pending_action,
                        Action::WorkspaceBackAndForth,
                        &cancelled,
                    );
                }
                Action::MoveFocused(workspace) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::MoveFocused(workspace, outputs),
                        &mut worker_busy,
                    );
                }
                Action::MoveFocused(workspace) if automatic => {
                    queue_latest(
                        &mut pending_action,
                        Action::MoveFocused(workspace),
                        &cancelled,
                    );
                }
                Action::MoveApp(bundle_id, workspace) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::MoveApp(bundle_id, workspace, outputs),
                        &mut worker_busy,
                    );
                }
                Action::MoveApp(bundle_id, workspace) if automatic => {
                    queue_latest(
                        &mut pending_action,
                        Action::MoveApp(bundle_id, workspace),
                        &cancelled,
                    );
                }
                Action::Focus(offset) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::Focus(offset, outputs),
                        &mut worker_busy,
                    );
                }
                Action::Focus(offset) if automatic => {
                    queue_latest(&mut pending_action, Action::Focus(offset), &cancelled);
                }
                Action::Reorder(offset) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::Reorder(offset, outputs),
                        &mut worker_busy,
                    );
                }
                Action::Reorder(offset) if automatic => {
                    queue_latest(&mut pending_action, Action::Reorder(offset), &cancelled);
                }
                Action::ToggleLayout if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(&commands, WorkerCommand::ToggleLayout, &mut worker_busy);
                }
                Action::ToggleLayout if automatic => {
                    queue_latest(&mut pending_action, Action::ToggleLayout, &cancelled);
                }
                Action::Resize(direction) if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::Resize(direction, outputs),
                        &mut worker_busy,
                    );
                }
                Action::Resize(direction) if automatic => {
                    queue_latest(&mut pending_action, Action::Resize(direction), &cancelled);
                }
                Action::Balance if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(&commands, WorkerCommand::Balance, &mut worker_busy);
                }
                Action::Balance if automatic => {
                    queue_latest(&mut pending_action, Action::Balance, &cancelled);
                }
                Action::ToggleMaximize if automatic && !worker_busy => {
                    pending_action = None;
                    send_with_outputs(&commands, WorkerCommand::ToggleMaximize, &mut worker_busy);
                }
                Action::ToggleMaximize if automatic => {
                    queue_latest(&mut pending_action, Action::ToggleMaximize, &cancelled);
                }
                Action::SwitchWorkspace(_)
                | Action::WorkspaceBackAndForth
                | Action::MoveFocused(_)
                | Action::MoveApp(_, _)
                | Action::Focus(_)
                | Action::Reorder(_)
                | Action::ToggleLayout
                | Action::Resize(_)
                | Action::Balance
                | Action::ToggleMaximize => {}
                Action::OpenAccessibility => open_accessibility_settings(),
                Action::Restart | Action::Quit => {
                    let intent = if matches!(action, Action::Restart) {
                        ExitIntent::Restart
                    } else {
                        ExitIntent::Quit
                    };
                    automatic = false;
                    exit_intent = Some(intent);
                    cancelled.store(true, Ordering::Release);
                    _hotkeys = None;
                    pending_action = None;
                    match exit_restore_plan(intent, worker_busy) {
                        RestorePlan::Now => {
                            send_restore(&commands, &mut worker_busy, &mut restoring);
                        }
                        RestorePlan::AfterWorker => restore_pending = true,
                    }
                    status_item::set_automatic(false);
                    if worker_busy {
                        status_item::set_status(match intent {
                            ExitIntent::Quit => "Restoring windows...",
                            ExitIntent::Restart => "Restarting · restoring windows...",
                        });
                    } else {
                        let _ = cx.update(|cx| cx.quit());
                        return;
                    }
                }
            }
        }

        if automatic
            && !worker_busy
            && let Some(gesture) = pending_gesture.take()
        {
            cancelled.store(false, Ordering::Release);
            send_final_gesture(&commands, gesture, &mut worker_busy);
        }
        if !worker_busy && let Some(action) = pending_action.take() {
            match action {
                Action::Retile => {
                    cancelled.store(false, Ordering::Release);
                    send_with_outputs(&commands, WorkerCommand::Cycle, &mut worker_busy);
                }
                Action::SwitchWorkspace(workspace) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::Switch(workspace, outputs),
                    &mut worker_busy,
                ),
                Action::WorkspaceBackAndForth if automatic => send_with_outputs(
                    &commands,
                    WorkerCommand::WorkspaceBackAndForth,
                    &mut worker_busy,
                ),
                Action::MoveFocused(workspace) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::MoveFocused(workspace, outputs),
                    &mut worker_busy,
                ),
                Action::MoveApp(bundle_id, workspace) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::MoveApp(bundle_id, workspace, outputs),
                    &mut worker_busy,
                ),
                Action::Focus(offset) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::Focus(offset, outputs),
                    &mut worker_busy,
                ),
                Action::Reorder(offset) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::Reorder(offset, outputs),
                    &mut worker_busy,
                ),
                Action::ToggleLayout if automatic => {
                    send_with_outputs(&commands, WorkerCommand::ToggleLayout, &mut worker_busy)
                }
                Action::Resize(direction) if automatic => send_with_outputs(
                    &commands,
                    |outputs| WorkerCommand::Resize(direction, outputs),
                    &mut worker_busy,
                ),
                Action::Balance if automatic => {
                    send_with_outputs(&commands, WorkerCommand::Balance, &mut worker_busy)
                }
                Action::ToggleMaximize if automatic => {
                    send_with_outputs(&commands, WorkerCommand::ToggleMaximize, &mut worker_busy)
                }
                _ => {}
            }
        }
        if restoring
            && !worker_busy
            && next_restore_retry.is_some_and(|retry| Instant::now() >= retry)
        {
            next_restore_retry = None;
            send_restore(&commands, &mut worker_busy, &mut restoring);
            if !worker_busy {
                if exit_intent.is_some() {
                    let _ = cx.update(|cx| cx.quit());
                    return;
                }
                status_item::set_status("Paused · worker unavailable");
            }
        }
        if automatic {
            if last_hotkey_refresh.elapsed() >= APPLICATION_REFRESH_INTERVAL {
                last_hotkey_refresh = Instant::now();
                let layout_changed = match _hotkeys.as_ref().map(Hotkeys::matches_current_layout) {
                    Some(Ok(matches)) => !matches,
                    Some(Err(error)) => {
                        let error = format!("Hotkeys unavailable: {error}");
                        last_error = Some(error.clone());
                        status_item::set_status(&error);
                        false
                    }
                    None => false,
                };
                if layout_changed {
                    match HotkeyPlan::current(&hotkey_workspaces) {
                        // Resolve and validate while old registrations remain active. Carbon
                        // cannot register the replacement chords until those are released.
                        Ok(plan)
                            if !_hotkeys
                                .as_ref()
                                .is_some_and(|hotkeys| plan.matches(hotkeys)) =>
                        {
                            _hotkeys = None;
                            match Hotkeys::install(actions_sender.clone(), plan) {
                                Ok(hotkeys) => {
                                    _hotkeys = Some(hotkeys);
                                    last_error = None;
                                }
                                Err(error) => {
                                    automatic = false;
                                    cancelled.store(true, Ordering::Release);
                                    let error = format!("Hotkeys unavailable: {error}");
                                    last_error = Some(error.clone());
                                    match restore_plan(worker_busy) {
                                        RestorePlan::Now => send_restore(
                                            &commands,
                                            &mut worker_busy,
                                            &mut restoring,
                                        ),
                                        RestorePlan::AfterWorker => restore_pending = true,
                                    }
                                    status_item::set_automatic(false);
                                    status_item::set_status(&error);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let error = format!("Hotkeys unavailable: {error}");
                            last_error = Some(error.clone());
                            status_item::set_status(&error);
                        }
                    }
                }
            }
            let current_frontmost = macos::frontmost_application();
            if current_frontmost != frontmost_application {
                frontmost_application = current_frontmost;
                activation_refresh_at = Some(Instant::now() + APPLICATION_ACTIVATION_SETTLE);
                if worker_busy {
                    cancelled.store(true, Ordering::Release);
                }
            }
            let activation_ready =
                activation_refresh_at.is_none_or(|refresh_at| Instant::now() >= refresh_at);
            if activation_ready && activation_refresh_at.take().is_some() {
                events.invalidate();
            }
            let mouse_down = macos::mouse_down();
            if !mouse_was_down && mouse_down {
                dragged_window = macos::focused_window();
                drag_start_frame = dragged_window
                    .as_ref()
                    .and_then(|window| macos::frame(window).ok());
                window_was_dragged = false;
                window_was_resized = false;
                last_manipulated_frame = drag_start_frame;
                last_drag_point = None;
            }
            if mouse_was_down && !mouse_down {
                let final_frame = dragged_window
                    .as_ref()
                    .and_then(|window| macos::frame(window).ok());
                let final_point = window_was_dragged.then(macos::mouse_position).flatten();
                let gesture = final_gesture(
                    dragged_window.take(),
                    window_was_dragged,
                    window_was_resized,
                    final_frame,
                    final_point,
                );
                if let Some(gesture) = gesture.and_then(|gesture| {
                    coordinate_final_gesture(worker_busy, &mut pending_gesture, gesture, &cancelled)
                }) {
                    send_final_gesture(&commands, gesture, &mut worker_busy);
                } else if pending_gesture.is_none() {
                    events.invalidate();
                }
                drag_start_frame = None;
                window_was_dragged = false;
                window_was_resized = false;
                last_manipulated_frame = None;
                last_drag_point = None;
            } else if mouse_down
                && !worker_busy
                && let Some(window) = dragged_window.as_ref()
                && let Some(start) = drag_start_frame
                && let Ok(current) = macos::frame(window)
                && last_manipulated_frame != Some(current)
            {
                last_manipulated_frame = Some(current);
                if window_was_resized || is_window_resize(start, current) {
                    window_was_resized = true;
                    let window = window.clone();
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::ResizeWindow(window, current, true, outputs),
                        &mut worker_busy,
                    );
                } else if (window_was_dragged || is_window_drag(start, current))
                    && let Some(point) = macos::mouse_position()
                    && last_drag_point != Some(point)
                {
                    window_was_dragged = true;
                    last_drag_point = Some(point);
                    let window = window.clone();
                    send_with_outputs(
                        &commands,
                        |outputs| WorkerCommand::DropWindow(window, point, true, outputs),
                        &mut worker_busy,
                    );
                }
            }
            mouse_was_down = mouse_down;
            if last_application_refresh.elapsed() >= APPLICATION_REFRESH_INTERVAL {
                events.refresh_applications();
                last_application_refresh = Instant::now();
            }
            if !mouse_down && !worker_busy && pending_gesture.is_none() && {
                let window_created = events.take_window_created();
                let event_dirty = (window_created || activation_ready) && events.take_dirty();
                refresh_due(
                    window_created,
                    event_dirty,
                    last_refresh.elapsed() >= FALLBACK_REFRESH_INTERVAL,
                    activation_ready,
                )
            } {
                send_with_outputs(&commands, WorkerCommand::Cycle, &mut worker_busy);
                if worker_busy {
                    last_refresh = Instant::now();
                }
            }
        }
        Timer::after(TICK_INTERVAL).await;
    }
}

fn is_window_drag(start: crate::Rect, current: crate::Rect) -> bool {
    let moved = (start.x - current.x).abs().max((start.y - current.y).abs()) >= 6;
    let resized = (start.width - current.width)
        .abs()
        .max((start.height - current.height).abs())
        > 2;
    moved && !resized
}

fn is_window_resize(start: crate::Rect, current: crate::Rect) -> bool {
    (start.width - current.width)
        .abs()
        .max((start.height - current.height).abs())
        >= 6
}

fn final_gesture(
    window: Option<crate::PhysicalWindowId>,
    dragged: bool,
    resized: bool,
    frame: Option<crate::Rect>,
    point: Option<(i32, i32)>,
) -> Option<FinalGesture> {
    let window = window?;
    if resized {
        frame.map(|frame| FinalGesture::Resize(window, frame))
    } else if dragged {
        point.map(|point| FinalGesture::Drop(window, point))
    } else {
        None
    }
}

fn coordinate_final_gesture(
    worker_busy: bool,
    pending: &mut Option<FinalGesture>,
    gesture: FinalGesture,
    cancelled: &AtomicBool,
) -> Option<FinalGesture> {
    if worker_busy {
        cancelled.store(true, Ordering::Release);
        pending.replace(gesture);
        None
    } else {
        Some(gesture)
    }
}

fn send_final_gesture(
    commands: &Sender<WorkerCommand>,
    gesture: FinalGesture,
    worker_busy: &mut bool,
) {
    send_with_outputs(
        commands,
        |outputs| match gesture {
            FinalGesture::Drop(window, point) => {
                WorkerCommand::DropWindow(window, point, false, outputs)
            }
            FinalGesture::Resize(window, frame) => {
                WorkerCommand::ResizeWindow(window, frame, false, outputs)
            }
        },
        worker_busy,
    );
}

fn send_restore(commands: &Sender<WorkerCommand>, worker_busy: &mut bool, restoring: &mut bool) {
    *worker_busy = commands.send(WorkerCommand::RestoreAll).is_ok();
    *restoring = *worker_busy;
}

fn queue_latest(pending_action: &mut Option<Action>, action: Action, cancelled: &AtomicBool) {
    cancelled.store(true, Ordering::Release);
    pending_action.replace(action);
}

fn send_with_outputs(
    commands: &Sender<WorkerCommand>,
    command: impl FnOnce(macos::DisplayOutputs) -> WorkerCommand,
    worker_busy: &mut bool,
) {
    match macos::outputs() {
        Ok(outputs) => {
            if commands.send(command(outputs)).is_ok() {
                *worker_busy = true;
            }
        }
        Err(error) => status_item::set_status(&format!("Display error: {error}")),
    }
}

fn show_result(result: Result<CycleStatus, String>) {
    match result {
        Ok(status) => {
            status_item::set_workspace_layouts(&status.layouts);
            if status.failed > 0 {
                status_item::set_status(&format!(
                    "{} window operation{} failed",
                    status.failed,
                    if status.failed == 1 { "" } else { "s" },
                ));
            } else {
                status_item::clear_status();
            }
        }
        Err(error) => status_item::set_status(&error),
    }
}

fn spawn_worker(
    commands: Receiver<WorkerCommand>,
    results: Sender<WorkerResult>,
    cancelled: Arc<AtomicBool>,
    scope: ManagementScope,
    parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
    config: Config,
    workspace_state: Option<crate::workspace::WorkspaceState>,
) {
    thread::spawn(move || {
        let restore_on_start = workspace_state.is_none();
        let mut runtime = WorkspaceRuntime::new(scope, parked, config, workspace_state);
        let startup_failed = if restore_on_start {
            runtime.restore_all()
        } else {
            0
        };
        if results
            .send(WorkerResult::Started {
                failed: startup_failed,
            })
            .is_err()
        {
            return;
        }
        while let Ok(command) = commands.recv() {
            let result = match command {
                WorkerCommand::Cycle(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.cycle(outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::Switch(workspace, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.switch(workspace, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::WorkspaceBackAndForth(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.workspace_back_and_forth(outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::MoveFocused(workspace, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.move_focused(workspace, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::MoveApp(bundle_id, workspace, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.move_app(&bundle_id, workspace, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::Focus(direction, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.focus_relative(direction, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::Reorder(direction, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.reorder_focused(direction, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::DropWindow(window, point, preview, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.drop_window(window, point, preview, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::ResizeWindow(window, frame, preview, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.resize_window(window, frame, preview, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::ToggleLayout(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.toggle_layout(outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::Resize(direction, outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.resize_focused(direction, outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::Balance(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.balance_focused(outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::ToggleMaximize(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.toggle_maximize(outputs, &cancelled);
                    WorkerResult::Status {
                        result,
                        debug: Box::new(runtime.debug_state(&debug_outputs)),
                    }
                }
                WorkerCommand::RestoreAll => {
                    let failed = runtime.restore_all();
                    WorkerResult::Restored { failed }
                }
                WorkerCommand::Stop => return,
            };
            if results.send(result).is_err() {
                return;
            }
        }
    });
}

struct WorkspaceRuntime<B = MacWorkspaceBackend> {
    backend: B,
    scope: ManagementScope,
    workspaces: Workspaces,
    known_apps: [HashMap<AppInstanceId, String>; WorkspaceId::COUNT],
    constrained: [HashMap<crate::PhysicalWindowId, (crate::Rect, crate::Rect)>; WorkspaceId::COUNT],
    parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
    config: Config,
    foreground_lanes: HashMap<String, DisplaySelector>,
    suppressed_focus: Option<crate::PhysicalWindowId>,
    maximized: [Option<crate::PhysicalWindowId>; WorkspaceId::COUNT],
    persisted_workspace_state: crate::workspace::WorkspaceState,
}

impl WorkspaceRuntime<MacWorkspaceBackend> {
    fn new(
        scope: ManagementScope,
        parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
        config: Config,
        workspace_state: Option<crate::workspace::WorkspaceState>,
    ) -> Self {
        Self::with_backend_config_state(scope, MacWorkspaceBackend, parked, config, workspace_state)
    }
}

impl<B: WorkspaceBackend> WorkspaceRuntime<B> {
    #[cfg(test)]
    fn with_backend(
        scope: ManagementScope,
        backend: B,
        parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
    ) -> Self {
        Self::with_backend_and_config(scope, backend, parked, Config::default())
    }

    #[cfg(test)]
    fn with_backend_and_config(
        scope: ManagementScope,
        backend: B,
        parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
        config: Config,
    ) -> Self {
        Self::with_backend_config_state(scope, backend, parked, config, None)
    }

    fn with_backend_config_state(
        scope: ManagementScope,
        backend: B,
        parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
        config: Config,
        workspace_state: Option<crate::workspace::WorkspaceState>,
    ) -> Self {
        let mut workspaces = Workspaces::default();
        config.configure_workspaces(&mut workspaces);
        if let Some(state) = workspace_state {
            workspaces.restore(state);
        }
        let persisted_workspace_state = workspaces.state();
        Self {
            backend,
            scope,
            workspaces,
            known_apps: std::array::from_fn(|_| HashMap::new()),
            constrained: std::array::from_fn(|_| HashMap::new()),
            parked,
            config,
            foreground_lanes: HashMap::new(),
            suppressed_focus: None,
            maximized: std::array::from_fn(|_| None),
            persisted_workspace_state,
        }
    }

    fn visible_workspaces(&self, outputs: &macos::DisplayOutputs) -> Vec<ResolvedWorkspace> {
        outputs
            .displays()
            .iter()
            .filter_map(|display| {
                let candidates = || {
                    self.workspaces
                        .active_lanes()
                        .filter(|(selector, _)| outputs.resolve(selector).id == display.id)
                };
                let selected = self
                    .foreground_lanes
                    .get(&display.id)
                    .and_then(|foreground| {
                        candidates().find(|(selector, _)| *selector == foreground)
                    })
                    .or_else(|| {
                        candidates()
                            .filter(|(selector, _)| selector_available(selector, outputs))
                            .min_by_key(|(_, workspace)| workspace.number())
                    })
                    .or_else(|| candidates().min_by_key(|(_, workspace)| workspace.number()))?;
                Some(ResolvedWorkspace {
                    workspace: selected.1,
                    selector: selected.0.clone(),
                    display_id: display.id.clone(),
                    output: display.frame,
                })
            })
            .collect()
    }

    fn restore_visible(
        &mut self,
        visible: &[ResolvedWorkspace],
        cancelled: &AtomicBool,
        failed: &mut usize,
    ) {
        if self.parked.is_empty() {
            return;
        }
        let windows: Vec<_> = visible
            .iter()
            .flat_map(|resolved| self.workspaces.engine(resolved.workspace).bound_windows())
            .collect();
        for window in windows {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            if self.parked.contains_key(&window)
                && !matches!(
                    self.restore_window(&window),
                    macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                )
            {
                *failed += 1;
            }
        }
    }

    fn is_visible(&self, workspace: WorkspaceId, outputs: &macos::DisplayOutputs) -> bool {
        self.visible_workspaces(outputs)
            .iter()
            .any(|resolved| resolved.workspace == workspace)
    }

    fn synchronize_foreground(&mut self, outputs: &macos::DisplayOutputs) {
        let disconnected: Vec<_> = self
            .foreground_lanes
            .iter()
            .filter(|(display_id, _)| {
                !outputs
                    .displays()
                    .iter()
                    .any(|display| &display.id == *display_id)
            })
            .map(|(display_id, selector)| (display_id.clone(), selector.clone()))
            .collect();
        for (display_id, selector) in disconnected {
            self.foreground_lanes.remove(&display_id);
            self.foreground_lanes
                .insert(outputs.resolve(&selector).id.clone(), selector);
        }
    }

    fn cycle(
        &mut self,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let mut counts = CycleCounts::default();
        if cancelled.load(Ordering::Acquire) {
            return Err("workspace operation cancelled".into());
        }
        self.synchronize_foreground(&outputs);
        let visible = self.visible_workspaces(&outputs);
        let frontmost = self.backend.frontmost_application();
        let focused = self.backend.focused_window();
        let focused_is_frontmost = focused.as_ref().is_some_and(|focused| {
            frontmost
                .as_ref()
                .is_none_or(|frontmost| &focused.app == frontmost)
        });
        let focus_workspace = focused
            .as_ref()
            .filter(|_| focused_is_frontmost)
            .and_then(|focused| self.workspaces.workspace_of(focused))
            .or_else(|| {
                frontmost
                    .as_ref()
                    .and_then(|app| self.workspaces.workspace_for_app(app))
            });
        let suppressed = focused_is_frontmost
            && focused
                .as_ref()
                .is_some_and(|focused| self.suppressed_focus.as_ref() == Some(focused));
        if !suppressed {
            self.suppressed_focus = None;
            if let Some(workspace) = focus_workspace
                && !visible
                    .iter()
                    .any(|resolved| resolved.workspace == workspace)
            {
                return self.switch(workspace, outputs, cancelled);
            }
        }
        for resolved in &visible {
            self.foreground_lanes
                .insert(resolved.display_id.clone(), resolved.selector.clone());
        }
        self.restore_visible(&visible, cancelled, &mut counts.failed);
        self.park_inactive(&outputs, &visible, cancelled, &mut counts.failed);
        if cancelled.load(Ordering::Acquire) {
            return Err("workspace operation cancelled".into());
        }
        let mut observation = self
            .backend
            .snapshot(0, &outputs)
            .map_err(|error| format!("Observation error: {error}"))?;
        let affinity_policy = self.freeze_window_affinities(&observation, &visible)?;
        for resolved in &visible {
            if !self.scope.manages_selector(&resolved.selector) {
                continue;
            }
            self.reconcile_display(
                resolved,
                &outputs,
                &mut observation,
                &affinity_policy,
                cancelled,
                &mut counts,
            )?;
        }
        self.persist_workspace_state_if_changed()?;
        Ok(self.status_for_visible(counts.applied, counts.failed, &visible))
    }

    fn freeze_window_affinities(
        &self,
        observation: &crate::NativeObservation,
        visible: &[ResolvedWorkspace],
    ) -> Result<FrozenAffinityPolicy, String> {
        let mut affinities = HashMap::new();
        for application in &observation.applications {
            let crate::AppObservation::Complete { app, windows, .. } = application else {
                continue;
            };
            let app_workspace = self.workspaces.workspace_for_app(&app.id);
            for window in windows {
                let affinity = if let Some(owner) = self.workspaces.workspace_of(&window.id) {
                    FrozenWindowAffinity::Owned(owner)
                } else {
                    match self.config.disposition(&app.bundle_id, window) {
                        WindowDisposition::Floating => FrozenWindowAffinity::Floating,
                        WindowDisposition::Route(target) => {
                            if !self
                                .scope
                                .manages_selector(self.workspaces.assignment(target))
                            {
                                return Err(format!(
                                    "window rule routes to unmanaged workspace {}",
                                    target.number()
                                ));
                            }
                            FrozenWindowAffinity::ExplicitRoute(target)
                        }
                        WindowDisposition::Managed => app_workspace.map_or(
                            FrozenWindowAffinity::CurrentDisplay,
                            FrozenWindowAffinity::Inferred,
                        ),
                    }
                };
                affinities.insert(window.id.clone(), affinity);
            }
        }
        Ok(FrozenAffinityPolicy {
            windows: affinities,
            visible_workspaces: visible.iter().map(|resolved| resolved.workspace).collect(),
        })
    }

    fn reconcile_display(
        &mut self,
        resolved: &ResolvedWorkspace,
        outputs: &macos::DisplayOutputs,
        observation: &mut crate::NativeObservation,
        affinity_policy: &FrozenAffinityPolicy,
        cancelled: &AtomicBool,
        counts: &mut CycleCounts,
    ) -> Result<(), String> {
        let workspace = resolved.workspace;
        let output = resolved.output;
        let ticket = {
            let engine = self.workspaces.engine_mut(workspace);
            engine.invalidate();
            engine.begin_cycle()
        };
        let mut scoped_observation = observation.clone();
        scoped_observation.cycle = ticket.cycle;
        let settings = Settings::for_output(output);
        let mut dispositions = HashMap::new();
        for app in &mut scoped_observation.applications {
            if let crate::AppObservation::Complete {
                app: _,
                focused,
                windows,
            } = app
            {
                windows.retain(|window| {
                    let affinity = affinity_policy
                        .windows
                        .get(&window.id)
                        .copied()
                        .unwrap_or(FrozenWindowAffinity::CurrentDisplay);
                    let admitted = match affinity {
                        FrozenWindowAffinity::Owned(owner) => owner == workspace,
                        FrozenWindowAffinity::Floating => false,
                        FrozenWindowAffinity::Inferred(target)
                            if affinity_policy.visible_workspaces.contains(&target) =>
                        {
                            target == workspace
                        }
                        FrozenWindowAffinity::ExplicitRoute(_)
                        | FrozenWindowAffinity::Inferred(_)
                        | FrozenWindowAffinity::CurrentDisplay => {
                            output.contains_center(window.frame)
                                && !self.workspaces.owned_by_other(workspace, &window.id)
                        }
                    };
                    if admitted {
                        let disposition = match affinity {
                            FrozenWindowAffinity::ExplicitRoute(target)
                            | FrozenWindowAffinity::Inferred(target) => {
                                WindowDisposition::Route(target)
                            }
                            _ => WindowDisposition::Managed,
                        };
                        dispositions.insert(window.id.clone(), disposition);
                    }
                    admitted
                });
                if focused
                    .as_ref()
                    .is_some_and(|focused| !windows.iter().any(|window| &window.id == focused))
                {
                    *focused = None;
                }
            }
        }
        live::add_absent_applications_with(
            &mut scoped_observation,
            &mut self.known_apps[workspace.index()],
            |app| self.backend.is_process_running(app),
        );
        let reconciliation = self
            .workspaces
            .engine_mut(workspace)
            .accept(ticket, &scoped_observation, &settings)
            .map_err(|error| format!("stale workspace cycle: {error:?}"))?;
        let routes: Vec<_> = reconciliation
            .decisions
            .iter()
            .filter_map(|decision| match decision {
                Decision::WindowInserted { window, .. } => dispositions
                    .get(window)
                    .copied()
                    .and_then(|disposition| match disposition {
                        WindowDisposition::Route(target) if target != workspace => {
                            Some((window.clone(), target))
                        }
                        _ => None,
                    }),
                _ => None,
            })
            .collect();
        let mut routed_targets = HashSet::new();
        for (window, target) in &routes {
            if self.workspaces.move_window(window, *target).is_none() {
                continue;
            }
            if !affinity_policy.visible_workspaces.contains(target) {
                let target_output = output_for(&self.workspaces, *target, outputs);
                match self.park_window(window, target_output, outputs) {
                    macos::ApplyOutcome::Applied => {}
                    macos::ApplyOutcome::Gone => self.forget_window(*target, window),
                    _ => {
                        let _ = self.workspaces.move_window(window, workspace);
                        counts.failed += 1;
                        continue;
                    }
                }
            }
            routed_targets.insert(*target);
        }
        self.workspaces.refresh_ownership(workspace);
        for target in &routed_targets {
            self.workspaces.refresh_ownership(*target);
        }
        let bound: HashSet<_> = self
            .workspaces
            .engine(workspace)
            .bound_windows()
            .into_iter()
            .collect();
        self.constrained[workspace.index()].retain(|window, _| bound.contains(window));
        let observed_frames = live::observed_frames(&scoped_observation);
        let mut placements = if routes.is_empty() {
            live::applicable_placements_with_frames(&reconciliation, &settings, &observed_frames)
        } else {
            self.workspaces.engine(workspace).placements(&settings)
        };
        self.apply_maximize(workspace, &mut placements, output);
        placements.retain(|placement| {
            let Some(window) = placement.window.as_ref() else {
                return false;
            };
            !self.constrained[workspace.index()]
                .get(window)
                .zip(observed_frames.get(window))
                .is_some_and(|((target, accepted), observed)| {
                    *target == placement.frame
                        && observed.approximately_equals(*accepted, settings.frame_tolerance)
                })
        });
        if !placements.is_empty() {
            let results = self
                .backend
                .apply_observed(&placements, &settings, cancelled);
            self.record_results(workspace, &results);
            count_results(&results, &mut counts.applied, &mut counts.failed);
        }
        for target in routed_targets {
            if affinity_policy.visible_workspaces.contains(&target) {
                let target_output = output_for(&self.workspaces, target, outputs);
                let layout = self.layout_workspace(target, target_output, cancelled);
                counts.applied += layout.applied;
                counts.failed += layout.failed;
                update_observed_frames(
                    observation,
                    &self
                        .workspaces
                        .engine(target)
                        .placements(&Settings::for_output(target_output)),
                );
            }
        }
        Ok(())
    }

    fn switch(
        &mut self,
        workspace: WorkspaceId,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let selector = self.workspaces.assignment(workspace).clone();
        if !self.scope.manages_selector(&selector) {
            return Err(format!(
                "Workspace {} is outside the managed display scope",
                workspace.number()
            ));
        }
        let output = output_for(&self.workspaces, workspace, &outputs);
        let display_id = outputs.resolve(&selector).id.clone();
        let previous = self
            .visible_workspaces(&outputs)
            .into_iter()
            .find(|resolved| resolved.display_id == display_id)
            .map_or(workspace, |resolved| resolved.workspace);
        if previous == workspace {
            return self.cycle(outputs, cancelled);
        }
        let outgoing = self.workspaces.engine(previous).bound_windows();
        let incoming = self.workspaces.engine(workspace).bound_windows();
        let outgoing_focus = self
            .backend
            .focused_window()
            .filter(|focused| outgoing.contains(focused));
        let mut restored = Vec::new();
        for window in &incoming {
            if cancelled.load(Ordering::Acquire) {
                for window in restored {
                    let _ = self.park_window(&window, output, &outputs);
                }
                return Err("workspace switch cancelled".into());
            }
            if !self.parked.contains_key(window) {
                continue;
            }
            match self.restore_window(window) {
                macos::ApplyOutcome::Applied => {
                    restored.push(window.clone());
                }
                macos::ApplyOutcome::Gone => {
                    self.forget_window(workspace, window);
                }
                _ => {
                    for restored in restored {
                        let _ = self.park_window(&restored, output, &outputs);
                    }
                    return Err(format!(
                        "Workspace {} could not restore all of its windows",
                        workspace.number()
                    ));
                }
            }
        }
        let layout = self.layout_workspace(workspace, output, cancelled);
        if layout.blocked {
            for window in &incoming {
                if !self.parked.contains_key(window) {
                    let _ = self.park_window(window, output, &outputs);
                }
            }
            return Err(format!(
                "Workspace {} stayed inactive because its windows could not be laid out",
                workspace.number()
            ));
        }
        let mut parked_outgoing = Vec::new();
        for window in &outgoing {
            if cancelled.load(Ordering::Acquire) {
                for window in parked_outgoing {
                    self.restore_window(&window);
                }
                for window in restored {
                    let _ = self.park_window(&window, output, &outputs);
                }
                return Err("workspace switch cancelled".into());
            }
            match self.park_window(window, output, &outputs) {
                macos::ApplyOutcome::Applied => parked_outgoing.push(window.clone()),
                macos::ApplyOutcome::Gone => {
                    self.forget_window(previous, window);
                }
                _ => {
                    for window in parked_outgoing {
                        self.restore_window(&window);
                    }
                    for window in restored {
                        let _ = self.park_window(&window, output, &outputs);
                    }
                    return Err(format!(
                        "Workspace {} stayed active because a window could not be parked",
                        previous.number()
                    ));
                }
            }
        }
        self.workspaces.switch(workspace);
        self.foreground_lanes.insert(display_id, selector);
        if let Some(window) = self.workspaces.engine(workspace).bound_windows().first() {
            self.suppressed_focus = (!self.backend.focus_window(window))
                .then_some(outgoing_focus)
                .flatten();
        } else {
            self.suppressed_focus = outgoing_focus;
        }
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn workspace_back_and_forth(
        &mut self,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let focused = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = self
            .workspaces
            .workspace_of(&focused)
            .ok_or("Focused window is not managed")?;
        let selector = self.workspaces.assignment(workspace);
        let previous = self
            .workspaces
            .previous_for(selector)
            .ok_or("Focused display has no previous workspace")?;
        self.switch(previous, outputs, cancelled)
    }

    fn move_focused(
        &mut self,
        target: WorkspaceId,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        self.move_window(window, target, outputs, cancelled)
    }

    fn move_app(
        &mut self,
        bundle_id: &str,
        target: WorkspaceId,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let mut windows = self.windows_for_bundle(bundle_id);
        if windows.is_empty() {
            self.cycle(outputs.clone(), cancelled)?;
            windows = self.windows_for_bundle(bundle_id);
        }
        match windows.as_slice() {
            [] => Err(format!("No managed window found for {bundle_id}")),
            [window] => self.move_window(window.clone(), target, outputs, cancelled),
            _ => Err(format!(
                "{bundle_id} has multiple managed windows; use move on the focused window"
            )),
        }
    }

    fn windows_for_bundle(&self, bundle_id: &str) -> Vec<crate::PhysicalWindowId> {
        WorkspaceId::all()
            .flat_map(|workspace| self.workspaces.engine(workspace).bound_windows())
            .filter(|window| {
                self.known_apps.iter().any(|apps| {
                    apps.get(&window.app)
                        .is_some_and(|known| known == bundle_id)
                })
            })
            .collect()
    }

    fn move_window(
        &mut self,
        window: crate::PhysicalWindowId,
        target: WorkspaceId,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let target_selector = self.workspaces.assignment(target).clone();
        if !self.scope.manages_selector(&target_selector) {
            return Err(format!(
                "Workspace {} is outside the managed display scope",
                target.number()
            ));
        }
        let source = match self.workspaces.workspace_of(&window) {
            Some(source) => source,
            None => {
                self.cycle(outputs.clone(), cancelled)?;
                self.workspaces
                    .workspace_of(&window)
                    .ok_or("Window is not managed by Spaceman")?
            }
        };
        if source == target {
            return Ok(self.status(0, 0, &outputs));
        }
        let source_output = output_for(&self.workspaces, source, &outputs);
        let target_output = output_for(&self.workspaces, target, &outputs);
        let target_is_active = self.is_visible(target, &outputs);
        if !target_is_active
            && !matches!(
                self.park_window(&window, target_output, &outputs),
                macos::ApplyOutcome::Applied
            )
        {
            return Err("Window could not be parked in the target workspace".into());
        }
        self.workspaces
            .move_window(&window, target)
            .ok_or("Window could not be moved")?;
        if self.maximized[source.index()].as_ref() == Some(&window) {
            self.maximized[source.index()] = None;
        }
        if !target_is_active {
            self.suppressed_focus = Some(window.clone());
        }
        self.constrained[source.index()].remove(&window);
        self.constrained[target.index()].remove(&window);
        let mut applied = 0;
        let mut failed = 0;
        if self.is_visible(source, &outputs) {
            let layout = self.layout_workspace(source, source_output, cancelled);
            applied += layout.applied;
            failed += layout.failed;
        }
        if target_is_active {
            if self.parked.contains_key(&window)
                && !matches!(
                    self.restore_window(&window),
                    macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                )
            {
                failed += 1;
            }
            let layout = self.layout_workspace(target, target_output, cancelled);
            applied += layout.applied;
            failed += layout.failed;
            let _ = self.backend.focus_window(&window);
        }
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(applied, failed, &outputs))
    }

    fn reorder_focused(
        &mut self,
        direction: Direction,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = match self.workspaces.workspace_of(&window) {
            Some(workspace) => workspace,
            None => {
                self.cycle(outputs.clone(), cancelled)?;
                self.workspaces
                    .workspace_of(&window)
                    .ok_or("Focused window is not managed by Spaceman")?
            }
        };
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        let windows = self.workspaces.engine(workspace).bound_windows();
        let axis = self.workspaces.engine(workspace).root_axis();
        let perpendicular = matches!(
            (axis, direction),
            (crate::Axis::Horizontal, Direction::Down | Direction::Up)
                | (crate::Axis::Vertical, Direction::Left | Direction::Right)
        );
        let local_tiles = self
            .navigation_tiles(&outputs)
            .into_iter()
            .filter(|tile| tile.workspace == workspace)
            .collect::<Vec<_>>();
        if let Some(target) = crate::navigation::target(&local_tiles, &window, direction) {
            if !self
                .workspaces
                .engine_mut(workspace)
                .swap_windows(&window, &target.window)
            {
                return Ok(self.status(0, 0, &outputs));
            }
            let layout = self.layout_workspace(
                workspace,
                output_for(&self.workspaces, workspace, &outputs),
                cancelled,
            );
            self.persist_workspace_state_if_changed()?;
            return Ok(self.status(layout.applied, layout.failed, &outputs));
        }
        if windows.len() >= 2
            && perpendicular
            && self
                .workspaces
                .engine_mut(workspace)
                .create_implicit_split(&window, direction)
        {
            let layout = self.layout_workspace(
                workspace,
                output_for(&self.workspaces, workspace, &outputs),
                cancelled,
            );
            self.persist_workspace_state_if_changed()?;
            return Ok(self.status(layout.applied, layout.failed, &outputs));
        }
        let Some((target_workspace, target_window)) =
            self.spatial_target(&window, direction, &outputs)
        else {
            return Ok(self.status(0, 0, &outputs));
        };
        if target_workspace != workspace {
            return self.move_window(window, target_workspace, outputs, cancelled);
        }
        if !self
            .workspaces
            .engine_mut(workspace)
            .swap_windows(&window, &target_window)
        {
            return Ok(self.status(0, 0, &outputs));
        }
        let layout = self.layout_workspace(
            workspace,
            output_for(&self.workspaces, workspace, &outputs),
            cancelled,
        );
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn drop_window(
        &mut self,
        window: crate::PhysicalWindowId,
        point: (i32, i32),
        preview: bool,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let Some(source) = self.workspaces.workspace_of(&window) else {
            return self.cycle(outputs, cancelled);
        };
        let target = self
            .navigation_tiles(&outputs)
            .into_iter()
            .find(|tile| tile.window != window && tile.frame.contains(point.0, point.1));
        if let Some(target) = target {
            if target.workspace != source {
                if preview {
                    return Ok(self.status(0, 0, &outputs));
                }
                return self.move_window(window, target.workspace, outputs, cancelled);
            }
            let direction = drop_direction(point, target.frame);
            let changed = match direction {
                Some(direction) => self.workspaces.engine_mut(source).move_window_relative(
                    &window,
                    &target.window,
                    direction,
                ),
                None => self
                    .workspaces
                    .engine_mut(source)
                    .swap_windows(&window, &target.window),
            };
            if changed {
                let layout = self.layout_workspace_during_drag(
                    source,
                    output_for(&self.workspaces, source, &outputs),
                    preview.then_some(&window),
                    cancelled,
                );
                if !preview {
                    self.persist_workspace_state_if_changed()?;
                }
                return Ok(self.status(layout.applied, layout.failed, &outputs));
            }
        }
        if let Some(target) = self
            .visible_workspaces(&outputs)
            .into_iter()
            .find(|resolved| resolved.output.contains(point.0, point.1))
            .map(|resolved| resolved.workspace)
            && target != source
        {
            if preview {
                return Ok(self.status(0, 0, &outputs));
            }
            return self.move_window(window, target, outputs, cancelled);
        }
        let layout = self.layout_workspace(
            source,
            output_for(&self.workspaces, source, &outputs),
            cancelled,
        );
        if !preview {
            self.persist_workspace_state_if_changed()?;
        }
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn resize_window(
        &mut self,
        window: crate::PhysicalWindowId,
        frame: crate::Rect,
        preview: bool,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        let Some(workspace) = self.workspaces.workspace_of(&window) else {
            return self.cycle(outputs, cancelled);
        };
        let output = output_for(&self.workspaces, workspace, &outputs);
        if !self
            .workspaces
            .engine_mut(workspace)
            .resize_window(&window, frame, output)
        {
            if !preview {
                self.persist_workspace_state_if_changed()?;
            }
            return Ok(self.status(0, 0, &outputs));
        }
        let layout = self.layout_workspace_during_drag(workspace, output, Some(&window), cancelled);
        if !preview {
            self.persist_workspace_state_if_changed()?;
        }
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn focus_relative(
        &mut self,
        direction: Direction,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let focused = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = match self.workspaces.workspace_of(&focused) {
            Some(workspace) => workspace,
            None => {
                self.cycle(outputs.clone(), cancelled)?;
                self.workspaces
                    .workspace_of(&focused)
                    .ok_or("Focused window is not managed by Spaceman")?
            }
        };
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        let Some((_, target)) = self.spatial_target(&focused, direction, &outputs) else {
            return Ok(self.status(0, 0, &outputs));
        };
        if !self.backend.focus_window(&target) {
            return Err("Target window could not be focused".into());
        }
        Ok(self.status(0, 0, &outputs))
    }

    fn spatial_target(
        &self,
        focused: &crate::PhysicalWindowId,
        direction: Direction,
        outputs: &macos::DisplayOutputs,
    ) -> Option<(WorkspaceId, crate::PhysicalWindowId)> {
        let tiles = self.navigation_tiles(outputs);
        crate::navigation::target(&tiles, focused, direction)
            .map(|target| (target.workspace, target.window.clone()))
    }

    fn navigation_tiles(&self, outputs: &macos::DisplayOutputs) -> Vec<NavigationTile> {
        let mut tiles = Vec::new();
        for resolved in self.visible_workspaces(outputs) {
            let mut placements = self
                .workspaces
                .engine(resolved.workspace)
                .placements(&Settings::for_output(resolved.output));
            if let Some(maximized) = self.maximized[resolved.workspace.index()].as_ref() {
                placements.retain(|placement| placement.window.as_ref() == Some(maximized));
                if let Some(placement) = placements.first_mut() {
                    placement.frame = resolved.output;
                }
            }
            tiles.extend(placements.into_iter().filter_map(|placement| {
                placement.window.map(|window| NavigationTile {
                    workspace: resolved.workspace,
                    window,
                    frame: placement.frame,
                })
            }));
        }
        tiles
    }

    fn debug_state(&mut self, outputs: &macos::DisplayOutputs) -> RuntimeDebug {
        let tiles = self.navigation_tiles(outputs);
        let focused = self.backend.focused_window();
        let target = |direction| {
            focused.as_ref().and_then(|focused| {
                crate::navigation::target(&tiles, focused, direction)
                    .map(|tile| tile.window.clone())
            })
        };
        let targets = DebugTargets {
            left: target(Direction::Left),
            down: target(Direction::Down),
            up: target(Direction::Up),
            right: target(Direction::Right),
        };
        let visible = self.visible_workspaces(outputs);
        RuntimeDebug {
            frontmost: self.backend.frontmost_application(),
            focused,
            active_workspaces: visible.iter().map(|resolved| resolved.workspace).collect(),
            displays: visible
                .into_iter()
                .filter_map(|resolved| {
                    let display = outputs
                        .displays()
                        .iter()
                        .find(|display| display.id == resolved.display_id)?;
                    Some(DebugDisplay {
                        id: display.id.clone(),
                        name: display.name.clone(),
                        frame: display.frame,
                        workspace: resolved.workspace,
                        axis: self
                            .workspaces
                            .engine(resolved.workspace)
                            .model()
                            .workspace
                            .axis,
                        layout: self
                            .workspaces
                            .engine(resolved.workspace)
                            .preview(crate::Rect {
                                x: 0,
                                y: 0,
                                width: 1500,
                                height: 1100,
                            }),
                    })
                })
                .collect(),
            tiles,
            targets,
        }
    }

    fn toggle_layout(
        &mut self,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = self
            .workspaces
            .workspace_of(&window)
            .ok_or("Focused window is not managed by Spaceman")?;
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        self.workspaces.engine_mut(workspace).toggle_axis();
        let layout = self.layout_workspace(
            workspace,
            output_for(&self.workspaces, workspace, &outputs),
            cancelled,
        );
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn resize_focused(
        &mut self,
        direction: ResizeDirection,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = self
            .workspaces
            .workspace_of(&window)
            .ok_or("Focused window is not managed by Spaceman")?;
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        let output = output_for(&self.workspaces, workspace, &outputs);
        if !self.workspaces.engine_mut(workspace).resize_focused_tile(
            &window,
            direction,
            KEYBOARD_RESIZE_INCREMENT,
            output,
        ) {
            return Ok(self.status(0, 0, &outputs));
        }
        let layout = self.layout_workspace(workspace, output, cancelled);
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn balance_focused(
        &mut self,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = self
            .workspaces
            .workspace_of(&window)
            .ok_or("Focused window is not managed by Spaceman")?;
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        let output = output_for(&self.workspaces, workspace, &outputs);
        if !self
            .workspaces
            .engine_mut(workspace)
            .balance_focused_parent(&window, output)
        {
            return Ok(self.status(0, 0, &outputs));
        }
        let layout = self.layout_workspace(workspace, output, cancelled);
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn toggle_maximize(
        &mut self,
        outputs: macos::DisplayOutputs,
        cancelled: &AtomicBool,
    ) -> Result<CycleStatus, String> {
        self.synchronize_foreground(&outputs);
        let window = self
            .backend
            .focused_window()
            .ok_or("Focused window is unavailable")?;
        let workspace = match self.workspaces.workspace_of(&window) {
            Some(workspace) => workspace,
            None => {
                self.cycle(outputs.clone(), cancelled)?;
                self.workspaces
                    .workspace_of(&window)
                    .ok_or("Focused window is not managed by Spaceman")?
            }
        };
        if !self.is_visible(workspace, &outputs) {
            return Err("Focused window is not on an active workspace".into());
        }
        let maximized = &mut self.maximized[workspace.index()];
        if maximized.as_ref() == Some(&window) {
            *maximized = None;
        } else {
            *maximized = Some(window);
        }
        let layout = self.layout_workspace(
            workspace,
            output_for(&self.workspaces, workspace, &outputs),
            cancelled,
        );
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    fn layout_workspace(
        &mut self,
        workspace: WorkspaceId,
        output: crate::Rect,
        cancelled: &AtomicBool,
    ) -> LayoutStatus {
        self.layout_workspace_during_drag(workspace, output, None, cancelled)
    }

    fn layout_workspace_during_drag(
        &mut self,
        workspace: WorkspaceId,
        output: crate::Rect,
        dragged: Option<&crate::PhysicalWindowId>,
        cancelled: &AtomicBool,
    ) -> LayoutStatus {
        let settings = Settings::for_output(output);
        let mut placements = self.workspaces.engine(workspace).placements(&settings);
        self.apply_maximize(workspace, &mut placements, output);
        let results = match dragged {
            Some(dragged) => {
                self.backend
                    .apply_managed_during_drag(&placements, &settings, dragged, cancelled)
            }
            None => self
                .backend
                .apply_managed(&placements, &settings, cancelled),
        };
        // Managed layout runs without a fresh observation; the next cycle validates readback.
        self.record_results(workspace, &results);
        let mut applied = 0;
        let mut failed = 0;
        count_results(&results, &mut applied, &mut failed);
        let expected = placements
            .iter()
            .filter(|placement| placement.window.is_some())
            .count();
        let blocked = results.len() != expected
            || results.iter().any(|result| {
                matches!(
                    result.outcome,
                    macos::ApplyOutcome::Gone
                        | macos::ApplyOutcome::Unavailable
                        | macos::ApplyOutcome::Deferred
                )
            });
        LayoutStatus {
            applied,
            failed,
            blocked,
        }
    }

    fn park_inactive(
        &mut self,
        outputs: &macos::DisplayOutputs,
        visible: &[ResolvedWorkspace],
        cancelled: &AtomicBool,
        failed: &mut usize,
    ) {
        let visible = visible.iter().map(|resolved| resolved.workspace).collect();
        let inactive: Vec<_> = self
            .workspaces
            .inactive_bound_windows_except(visible)
            .collect();
        let displays = outputs.frames().collect::<Vec<_>>();
        let mut candidates = Vec::new();
        for (workspace, window) in inactive {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            if self.parked.contains_key(&window) {
                continue;
            }
            let output = output_for(&self.workspaces, workspace, outputs);
            let frame = match self.backend.frame(&window) {
                Ok(frame) => frame,
                Err(macos::WindowAccessError::Gone) => {
                    self.forget_window(workspace, &window);
                    continue;
                }
                Err(macos::WindowAccessError::Unavailable) => {
                    *failed += 1;
                    continue;
                }
            };
            let target = crate::geometry::parked_frame(output, &displays, frame);
            candidates.push((workspace, window, frame, target));
        }
        if candidates.is_empty() {
            return;
        }
        for (_, window, frame, _) in &candidates {
            self.parked.insert(window.clone(), *frame);
        }
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            for (_, window, _, _) in &candidates {
                self.parked.remove(window);
            }
            *failed += candidates.len();
            eprintln!("failed to persist parked windows: {error}");
            return;
        }

        let mut restored = Vec::new();
        for (workspace, window, frame, target) in candidates {
            match self.backend.set_position(&window, target) {
                macos::ApplyOutcome::Applied => {}
                macos::ApplyOutcome::Gone => {
                    self.parked.remove(&window);
                    restored.push((window.clone(), frame));
                    self.forget_window(workspace, &window);
                }
                _ => {
                    *failed += 1;
                    if matches!(
                        self.restore_window_uncommitted(&window),
                        macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                    ) {
                        restored.push((window, frame));
                    }
                }
            }
        }
        self.persist_removals_or_rollback(restored);
    }

    fn restore_all(&mut self) -> usize {
        let parked: Vec<_> = self.parked.keys().cloned().collect();
        let mut failed = 0;
        let mut restored = Vec::new();
        for window in parked {
            let frame = self.parked[&window];
            match self.restore_window_uncommitted(&window) {
                macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone => {
                    restored.push((window, frame));
                }
                _ => failed += 1,
            }
        }
        self.persist_removals_or_rollback(restored);
        failed
    }

    fn forget_window(&mut self, workspace: WorkspaceId, window: &crate::PhysicalWindowId) {
        self.workspaces.forget_window(workspace, window);
        self.constrained[workspace.index()].remove(window);
        if self.maximized[workspace.index()].as_ref() == Some(window) {
            self.maximized[workspace.index()] = None;
        }
    }

    fn apply_maximize(
        &mut self,
        workspace: WorkspaceId,
        placements: &mut Vec<crate::Placement>,
        output: crate::Rect,
    ) {
        let Some(maximized) = self.maximized[workspace.index()].as_ref() else {
            return;
        };
        let Some(mut placement) = placements
            .iter()
            .find(|placement| placement.window.as_ref() == Some(maximized))
            .cloned()
        else {
            self.maximized[workspace.index()] = None;
            return;
        };
        placement.frame = output;
        placements.clear();
        placements.push(placement);
    }

    fn mark_parked(&mut self, window: crate::PhysicalWindowId, frame: crate::Rect) -> bool {
        if self.parked.contains_key(&window) {
            return true;
        }
        self.parked.insert(window.clone(), frame);
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            self.parked.remove(&window);
            eprintln!("failed to persist parked window: {error}");
            return false;
        }
        true
    }

    fn park_window(
        &mut self,
        window: &crate::PhysicalWindowId,
        output: crate::Rect,
        outputs: &macos::DisplayOutputs,
    ) -> macos::ApplyOutcome {
        let frame = match self.backend.frame(window) {
            Ok(frame) => frame,
            Err(macos::WindowAccessError::Gone) => return macos::ApplyOutcome::Gone,
            Err(macos::WindowAccessError::Unavailable) => {
                return macos::ApplyOutcome::Unavailable;
            }
        };
        if !self.mark_parked(window.clone(), frame) {
            return macos::ApplyOutcome::Unavailable;
        }
        let displays = outputs.frames().collect::<Vec<_>>();
        let parked = crate::geometry::parked_frame(output, &displays, frame);
        let outcome = self.backend.set_position(window, parked);
        if !matches!(outcome, macos::ApplyOutcome::Applied) {
            let _ = self.restore_window(window);
        }
        outcome
    }

    fn restore_window(&mut self, window: &crate::PhysicalWindowId) -> macos::ApplyOutcome {
        let frame = self.parked.get(window).copied();
        let outcome = self.restore_window_uncommitted(window);
        if matches!(
            outcome,
            macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
        ) && let Some(frame) = frame
        {
            self.persist_removals_or_rollback(vec![(window.clone(), frame)]);
        }
        outcome
    }

    fn restore_window_uncommitted(
        &mut self,
        window: &crate::PhysicalWindowId,
    ) -> macos::ApplyOutcome {
        let Some(frame) = self.parked.get(window).copied() else {
            return macos::ApplyOutcome::Applied;
        };
        let outcome = self.backend.set_position(window, frame);
        if matches!(
            outcome,
            macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
        ) {
            self.parked.remove(window);
        }
        outcome
    }

    fn persist_removals_or_rollback(
        &mut self,
        restored: Vec<(crate::PhysicalWindowId, crate::Rect)>,
    ) {
        if restored.is_empty() {
            return;
        }
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            self.parked.extend(restored);
            eprintln!("failed to persist restored windows: {error}");
        }
    }

    fn record_results(&mut self, workspace: WorkspaceId, results: &[macos::ApplyResult]) {
        let evidence = &mut self.constrained[workspace.index()];
        for result in results {
            match (result.outcome, result.actual) {
                (macos::ApplyOutcome::Constrained, Some(actual)) => {
                    evidence.insert(result.window.clone(), (result.target, actual));
                }
                (macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone, _) => {
                    evidence.remove(&result.window);
                }
                _ => {}
            }
        }
    }

    fn status(
        &self,
        applied: usize,
        failed: usize,
        outputs: &macos::DisplayOutputs,
    ) -> CycleStatus {
        self.status_for_visible(applied, failed, &self.visible_workspaces(outputs))
    }

    fn persist_workspace_state_if_changed(&mut self) -> Result<(), String> {
        let state = self.workspaces.state();
        if state == self.persisted_workspace_state {
            return Ok(());
        }
        self.backend.persist_workspace_state(&state)?;
        self.persisted_workspace_state = state;
        Ok(())
    }

    fn status_for_visible(
        &self,
        _applied: usize,
        failed: usize,
        visible: &[ResolvedWorkspace],
    ) -> CycleStatus {
        let active: Vec<_> = visible
            .iter()
            .filter(|resolved| self.scope.manages_selector(&resolved.selector))
            .map(|resolved| resolved.workspace)
            .collect();
        CycleStatus {
            layouts: active
                .iter()
                .map(|workspace| {
                    (
                        workspace.number(),
                        self.workspaces.engine(*workspace).preview(crate::Rect {
                            x: 0,
                            y: 0,
                            width: 1500,
                            height: 1100,
                        }),
                    )
                })
                .collect(),
            active,
            failed,
        }
    }
}

fn drop_direction(point: (i32, i32), frame: crate::Rect) -> Option<Direction> {
    let distances = [
        (point.0 - frame.x, Direction::Left, frame.width),
        (
            frame.x + frame.width - point.0,
            Direction::Right,
            frame.width,
        ),
        (point.1 - frame.y, Direction::Up, frame.height),
        (
            frame.y + frame.height - point.1,
            Direction::Down,
            frame.height,
        ),
    ];
    distances
        .into_iter()
        .filter(|(distance, _, extent)| *distance <= *extent / 4)
        .min_by_key(|(distance, _, _)| *distance)
        .map(|(_, direction, _)| direction)
}

fn output_for(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    outputs: &macos::DisplayOutputs,
) -> crate::Rect {
    outputs.resolve(workspaces.assignment(workspace)).frame
}

fn selector_available(selector: &DisplaySelector, outputs: &macos::DisplayOutputs) -> bool {
    outputs.resolve_exact(selector).is_some()
}

fn update_observed_frames(
    observation: &mut crate::NativeObservation,
    placements: &[crate::Placement],
) {
    let frames: HashMap<_, _> = placements
        .iter()
        .filter_map(|placement| {
            placement
                .window
                .as_ref()
                .map(|window| (window, placement.frame))
        })
        .collect();
    for application in &mut observation.applications {
        if let crate::AppObservation::Complete { windows, .. } = application {
            for window in windows {
                if let Some(frame) = frames.get(&window.id) {
                    window.frame = *frame;
                }
            }
        }
    }
}

fn count_results(results: &[macos::ApplyResult], applied: &mut usize, failed: &mut usize) {
    for result in results {
        if matches!(result.outcome, macos::ApplyOutcome::Applied) {
            *applied += 1;
        } else {
            *failed += 1;
        }
    }
}

fn parked_path() -> Option<PathBuf> {
    crate::config::application_support_path("parked-windows.json")
}

fn workspace_state_path() -> Option<PathBuf> {
    crate::config::application_support_path("workspace-state.json")
}

fn load_parked() -> Result<HashMap<crate::PhysicalWindowId, crate::Rect>, String> {
    let path = parked_path().ok_or("HOME is unavailable")?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(format!("failed to read parked-window state: {error}")),
    };
    serde_json::from_slice::<ParkedWindows>(&bytes)
        .map(|parked| parked.0)
        .map_err(|error| format!("parked-window state is invalid: {error}"))
}

fn persist_parked(parked: &HashMap<crate::PhysicalWindowId, crate::Rect>) -> Result<(), String> {
    let path = parked_path().ok_or("HOME is unavailable")?;
    let parent = path
        .parent()
        .ok_or("parked-window state path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    let bytes = serde_json::to_vec(&ParkedWindowsRef(parked))
        .map_err(|error| format!("failed to serialize parked windows: {error}"))?;
    let mut file = atomic_write_file::AtomicWriteFile::open(&path)
        .map_err(|error| format!("failed to open parked-window state: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("failed to write parked windows: {error}"))?;
    file.commit()
        .map_err(|error| format!("failed to commit parked windows: {error}"))
}

fn load_workspace_state() -> Result<Option<crate::workspace::WorkspaceState>, String> {
    let path = workspace_state_path().ok_or("HOME is unavailable")?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read workspace state: {error}")),
    };
    let state: crate::workspace::WorkspaceState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("workspace state is invalid: {error}"))?;
    state
        .validate()
        .map_err(|error| format!("workspace state is invalid: {error}"))?;
    Ok(Some(state))
}

fn recover_startup_metadata(
    config: Result<Config, String>,
    workspace_state: Result<Option<crate::workspace::WorkspaceState>, String>,
) -> (Config, Option<crate::workspace::WorkspaceState>) {
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("ignoring invalid Spaceman config during recovery: {error}");
            return (Config::default(), None);
        }
    };
    let workspace_state = workspace_state.unwrap_or_else(|error| {
        eprintln!("ignoring invalid Spaceman workspace state during recovery: {error}");
        None
    });
    (config, workspace_state)
}

fn persist_workspace_state(state: &crate::workspace::WorkspaceState) -> Result<(), String> {
    let path = workspace_state_path().ok_or("HOME is unavailable")?;
    let parent = path.parent().ok_or("workspace state path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("failed to serialize workspace state: {error}"))?;
    let mut file = atomic_write_file::AtomicWriteFile::open(&path)
        .map_err(|error| format!("failed to open workspace state: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("failed to write workspace state: {error}"))?;
    file.commit()
        .map_err(|error| format!("failed to commit workspace state: {error}"))
}

fn open_accessibility_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

#[cfg(test)]
mod tests {
    use crate::{BindingState, DisplayRole, Slot, SlotId};

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum BackendCall {
        Persist,
        SetPosition(crate::PhysicalWindowId, crate::Rect),
        Apply(crate::PhysicalWindowId, crate::Rect),
        Focus(crate::PhysicalWindowId),
    }

    #[test]
    fn mouse_gesture_requires_position_change_without_resize() {
        let start = crate::Rect {
            x: 100,
            y: 100,
            width: 800,
            height: 600,
        };
        assert!(!is_window_drag(start, start));
        assert!(is_window_drag(
            start,
            crate::Rect {
                x: 120,
                y: 110,
                ..start
            }
        ));
        assert!(!is_window_drag(
            start,
            crate::Rect {
                x: 120,
                width: 780,
                ..start
            }
        ));
        assert!(is_window_resize(
            start,
            crate::Rect {
                width: 760,
                ..start
            }
        ));
    }

    #[derive(Default)]
    struct FakeBackend {
        frames: HashMap<crate::PhysicalWindowId, crate::Rect>,
        focused: Option<crate::PhysicalWindowId>,
        frontmost: Option<AppInstanceId>,
        observation: Option<crate::NativeObservation>,
        persisted: HashMap<crate::PhysicalWindowId, crate::Rect>,
        calls: Vec<BackendCall>,
        fail_persist: bool,
        frame_outcome: Option<macos::WindowAccessError>,
        managed_outcome: Option<macos::ApplyOutcome>,
        snapshot_count: usize,
        observed_apply_count: usize,
        workspace_state: Option<crate::workspace::WorkspaceState>,
    }

    impl WorkspaceBackend for FakeBackend {
        fn snapshot(
            &mut self,
            cycle: u64,
            outputs: &macos::DisplayOutputs,
        ) -> Result<crate::NativeObservation, String> {
            self.snapshot_count += 1;
            let mut observation = self.observation.clone().ok_or("missing fake observation")?;
            observation.cycle = cycle;
            for app in &mut observation.applications {
                if let crate::AppObservation::Complete {
                    focused, windows, ..
                } = app
                {
                    windows.retain_mut(|window| {
                        let Some(frame) = self.frames.get(&window.id).copied() else {
                            return false;
                        };
                        window.frame = frame;
                        outputs.union().contains_center(frame)
                    });
                    if focused
                        .as_ref()
                        .is_some_and(|focused| !windows.iter().any(|window| &window.id == focused))
                    {
                        *focused = None;
                    }
                }
            }
            Ok(observation)
        }

        fn is_process_running(&mut self, _: &AppInstanceId) -> bool {
            true
        }

        fn apply_observed(
            &mut self,
            placements: &[crate::Placement],
            settings: &Settings,
            cancelled: &AtomicBool,
        ) -> Vec<macos::ApplyResult> {
            self.observed_apply_count += 1;
            self.apply_managed(placements, settings, cancelled)
        }

        fn frame(
            &mut self,
            window: &crate::PhysicalWindowId,
        ) -> Result<crate::Rect, macos::WindowAccessError> {
            if let Some(outcome) = self.frame_outcome {
                return Err(outcome);
            }
            self.frames
                .get(window)
                .copied()
                .ok_or(macos::WindowAccessError::Gone)
        }

        fn set_position(
            &mut self,
            window: &crate::PhysicalWindowId,
            target: crate::Rect,
        ) -> macos::ApplyOutcome {
            self.calls
                .push(BackendCall::SetPosition(window.clone(), target));
            let Some(frame) = self.frames.get_mut(window) else {
                return macos::ApplyOutcome::Gone;
            };
            frame.x = target.x;
            frame.y = target.y;
            macos::ApplyOutcome::Applied
        }

        fn focused_window(&mut self) -> Option<crate::PhysicalWindowId> {
            self.focused.clone()
        }

        fn frontmost_application(&mut self) -> Option<AppInstanceId> {
            self.frontmost.clone()
        }

        fn focus_window(&mut self, window: &crate::PhysicalWindowId) -> bool {
            self.calls.push(BackendCall::Focus(window.clone()));
            self.focused = Some(window.clone());
            true
        }

        fn apply_managed(
            &mut self,
            placements: &[crate::Placement],
            _: &Settings,
            cancelled: &AtomicBool,
        ) -> Vec<macos::ApplyResult> {
            placements
                .iter()
                .take_while(|_| !cancelled.load(Ordering::Acquire))
                .filter_map(|placement| {
                    let window = placement.window.clone()?;
                    self.calls
                        .push(BackendCall::Apply(window.clone(), placement.frame));
                    let outcome = self.managed_outcome.unwrap_or(macos::ApplyOutcome::Applied);
                    if matches!(outcome, macos::ApplyOutcome::Applied) {
                        self.frames.insert(window.clone(), placement.frame);
                    }
                    Some(macos::ApplyResult {
                        actual: self.frames.get(&window).copied(),
                        window,
                        target: placement.frame,
                        outcome,
                    })
                })
                .collect()
        }

        fn apply_managed_during_drag(
            &mut self,
            placements: &[crate::Placement],
            settings: &Settings,
            dragged: &crate::PhysicalWindowId,
            cancelled: &AtomicBool,
        ) -> Vec<macos::ApplyResult> {
            let placements = placements
                .iter()
                .filter(|placement| placement.window.as_ref() != Some(dragged))
                .cloned()
                .collect::<Vec<_>>();
            self.apply_managed(&placements, settings, cancelled)
        }

        fn persist_parked(
            &mut self,
            parked: &HashMap<crate::PhysicalWindowId, crate::Rect>,
        ) -> Result<(), String> {
            self.calls.push(BackendCall::Persist);
            if self.fail_persist {
                return Err("injected persistence failure".into());
            }
            self.persisted = parked.clone();
            Ok(())
        }

        fn persist_workspace_state(
            &mut self,
            state: &crate::workspace::WorkspaceState,
        ) -> Result<(), String> {
            self.workspace_state = Some(state.clone());
            Ok(())
        }
    }

    fn window(id: u32) -> crate::PhysicalWindowId {
        crate::PhysicalWindowId {
            app: AppInstanceId {
                pid: 1,
                launched_at: 1,
            },
            window_id: id,
        }
    }

    fn frame(x: i32) -> crate::Rect {
        crate::Rect {
            x,
            y: 20,
            width: 400,
            height: 300,
        }
    }

    fn outputs() -> macos::DisplayOutputs {
        macos::DisplayOutputs::new(vec![
            macos::DisplayOutput {
                id: "main-id".into(),
                name: "Main".into(),
                frame: crate::Rect {
                    width: 1000,
                    height: 800,
                    ..crate::Rect::default()
                },
                is_main: true,
            },
            macos::DisplayOutput {
                id: "secondary-id".into(),
                name: "Secondary".into(),
                frame: crate::Rect {
                    x: 1000,
                    width: 1000,
                    height: 800,
                    ..crate::Rect::default()
                },
                is_main: false,
            },
        ])
        .unwrap()
    }

    fn three_outputs() -> macos::DisplayOutputs {
        let mut displays = outputs().displays().to_vec();
        displays.push(macos::DisplayOutput {
            id: "third-id".into(),
            name: "Projector".into(),
            frame: crate::Rect {
                x: 2000,
                width: 1000,
                height: 800,
                ..crate::Rect::default()
            },
            is_main: false,
        });
        macos::DisplayOutputs::new(displays).unwrap()
    }

    fn bind(
        runtime: &mut WorkspaceRuntime<FakeBackend>,
        workspace: WorkspaceId,
        window: &crate::PhysicalWindowId,
    ) {
        runtime.workspaces.engine_mut(workspace).insert_slot(Slot {
            id: SlotId(u64::from(window.window_id)),
            weight: 1,
            binding: BindingState::Bound(window.clone()),
        });
    }

    fn runtime(backend: FakeBackend) -> WorkspaceRuntime<FakeBackend> {
        WorkspaceRuntime::with_backend(ManagementScope::Both, backend, HashMap::new())
    }

    fn runtime_with_config(backend: FakeBackend, config: Config) -> WorkspaceRuntime<FakeBackend> {
        WorkspaceRuntime::with_backend_and_config(
            ManagementScope::Both,
            backend,
            HashMap::new(),
            config,
        )
    }

    fn observation(
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
    ) -> crate::NativeObservation {
        observation_for("com.example.Test", window, frame)
    }

    fn observation_for(
        bundle_id: &str,
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
    ) -> crate::NativeObservation {
        observation_for_windows(bundle_id, window, &[(window.clone(), frame)])
    }

    fn observation_for_windows(
        bundle_id: &str,
        focused: &crate::PhysicalWindowId,
        windows: &[(crate::PhysicalWindowId, crate::Rect)],
    ) -> crate::NativeObservation {
        crate::NativeObservation {
            cycle: 0,
            mouse_down: false,
            applications: vec![crate::AppObservation::Complete {
                app: crate::ObservedApplication {
                    id: focused.app.clone(),
                    bundle_id: bundle_id.into(),
                },
                focused: Some(focused.clone()),
                windows: windows
                    .iter()
                    .map(|(window, frame)| crate::ObservedWindow {
                        id: window.clone(),
                        frame: *frame,
                        title: None,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn management_scope_rejects_unknown_values() {
        assert!(ManagementScope::parse("secondary").is_ok());
        assert!(ManagementScope::parse("typo").is_err());
    }

    #[test]
    fn event_driven_runtime_reduces_idle_fallback_scans() {
        let previous_scans_per_minute = 60_000 / 250;
        let fallback_scans_per_minute = 60 / FALLBACK_REFRESH_INTERVAL.as_secs();

        assert_eq!(previous_scans_per_minute, 240);
        assert_eq!(fallback_scans_per_minute, 60);
    }

    #[test]
    fn activation_settling_defers_refresh_without_an_ax_event() {
        assert!(!refresh_due(false, false, true, false));
        assert!(refresh_due(false, false, true, true));
    }

    #[test]
    fn window_creation_bypasses_activation_settling() {
        assert!(refresh_due(true, true, false, false));
        assert!(!refresh_due(false, true, false, false));
    }

    #[test]
    fn pending_actions_keep_only_the_latest_intent() {
        let mut pending = None;
        let second = WorkspaceId::new(2).unwrap();
        let third = WorkspaceId::new(3).unwrap();
        pending.replace(Action::SwitchWorkspace(second));
        pending.replace(Action::SwitchWorkspace(third));

        assert_eq!(pending.take(), Some(Action::SwitchWorkspace(third)));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn quit_and_restart_both_restore_before_exiting() {
        for intent in [ExitIntent::Quit, ExitIntent::Restart] {
            assert_eq!(exit_restore_plan(intent, false), RestorePlan::Now);
            assert_eq!(exit_restore_plan(intent, true), RestorePlan::AfterWorker);
        }
    }

    #[test]
    fn hotkey_failure_restores_immediately_or_after_the_active_worker() {
        assert_eq!(restore_plan(false), RestorePlan::Now);
        assert_eq!(restore_plan(true), RestorePlan::AfterWorker);
    }

    #[test]
    fn busy_preview_worker_queues_final_cross_display_drop() {
        let window = window(10);
        let drop = FinalGesture::Drop(window, (1700, 400));
        let mut pending = None;
        let cancelled = AtomicBool::new(false);

        assert_eq!(
            coordinate_final_gesture(true, &mut pending, drop.clone(), &cancelled),
            None
        );
        assert_eq!(pending.take(), Some(drop));
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn mouse_up_uses_final_resize_frame_instead_of_losing_busy_preview() {
        let window = window(10);
        let final_frame = frame(700);

        assert_eq!(
            final_gesture(
                Some(window.clone()),
                true,
                true,
                Some(final_frame),
                Some((1700, 400)),
            ),
            Some(FinalGesture::Resize(window, final_frame))
        );
    }

    #[test]
    fn invalid_startup_metadata_falls_back_so_parked_recovery_can_start() {
        let (config, workspace_state) =
            recover_startup_metadata(Err("bad config".into()), Err("bad state".into()));

        assert!(!config.has_rules());
        assert!(workspace_state.is_none());

        let valid_state = Workspaces::default().state();
        let (_, workspace_state) =
            recover_startup_metadata(Err("bad config".into()), Ok(Some(valid_state)));
        assert!(workspace_state.is_none());
    }

    #[test]
    fn parked_state_round_trips_and_rejects_malformed_data() {
        let window = window(10);
        let mut parked = HashMap::new();
        parked.insert(window.clone(), frame(50));
        let encoded = serde_json::to_vec(&ParkedWindows(parked.clone())).unwrap();

        assert_eq!(
            serde_json::from_slice::<ParkedWindows>(&encoded).unwrap().0,
            parked
        );
        assert!(serde_json::from_slice::<ParkedWindows>(b"not json").is_err());
        let duplicate =
            serde_json::to_vec(&vec![(window.clone(), frame(50)), (window, frame(100))]).unwrap();
        assert!(serde_json::from_slice::<ParkedWindows>(&duplicate).is_err());
    }

    fn benchmark_backend(window_count: u32) -> FakeBackend {
        let app = AppInstanceId {
            pid: 1,
            launched_at: 1,
        };
        let mut frames = HashMap::new();
        let mut windows = Vec::new();
        for id in 1..=window_count {
            let window = crate::PhysicalWindowId {
                app: app.clone(),
                window_id: id,
            };
            let frame = crate::Rect {
                x: if id <= window_count / 2 { 50 } else { 1050 },
                y: 20,
                width: 400,
                height: 300,
            };
            frames.insert(window.clone(), frame);
            windows.push(crate::ObservedWindow {
                id: window,
                frame,
                title: None,
            });
        }
        FakeBackend {
            frames,
            observation: Some(crate::NativeObservation {
                cycle: 0,
                mouse_down: false,
                applications: vec![crate::AppObservation::Complete {
                    app: crate::ObservedApplication {
                        id: app,
                        bundle_id: "com.example.Benchmark".into(),
                    },
                    focused: None,
                    windows,
                }],
            }),
            ..FakeBackend::default()
        }
    }

    #[test]
    fn dual_display_cycle_call_budget_is_explicit() {
        let mut runtime = runtime(benchmark_backend(2));

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert_eq!(runtime.backend.snapshot_count, 2);
        assert_eq!(runtime.backend.observed_apply_count, 2);
    }

    #[test]
    fn configured_third_display_has_an_independent_visible_workspace() {
        let config =
            Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'")
                .unwrap();
        let runtime = runtime_with_config(benchmark_backend(0), config);

        let visible = runtime.visible_workspaces(&three_outputs());

        assert_eq!(
            visible
                .iter()
                .map(|resolved| resolved.workspace.number())
                .collect::<Vec<_>>(),
            vec![1, 6, 9]
        );
    }

    #[test]
    fn missing_preferred_display_falls_back_without_double_visibility() {
        let config =
            Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'")
                .unwrap();
        let runtime = runtime_with_config(benchmark_backend(0), config);

        let visible = runtime.visible_workspaces(&outputs());

        assert_eq!(
            visible
                .iter()
                .map(|resolved| resolved.workspace.number())
                .collect::<Vec<_>>(),
            vec![1, 6]
        );
    }

    #[test]
    fn active_preferred_workspace_falls_back_and_returns_after_hotplug() {
        let config =
            Config::parse("version = 1\n[[workspaces]]\nworkspace = 9\nmonitor = 'third-id'")
                .unwrap();
        let mut runtime = runtime_with_config(benchmark_backend(0), config);

        runtime
            .cycle(three_outputs(), &AtomicBool::new(false))
            .unwrap();
        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            runtime
                .visible_workspaces(&outputs())
                .iter()
                .map(|resolved| resolved.workspace.number())
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
                .map(|resolved| resolved.workspace.number())
                .collect::<Vec<_>>(),
            vec![1, 6, 9]
        );
    }

    #[test]
    #[ignore = "performance benchmark"]
    fn benchmark_dual_display_steady_state_256_windows() {
        let mut runtime = runtime(benchmark_backend(256));
        let cancelled = AtomicBool::new(false);
        for _ in 0..10 {
            runtime.cycle(outputs(), &cancelled).unwrap();
        }
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = std::time::Instant::now();
            for _ in 0..100 {
                std::hint::black_box(runtime.cycle(outputs(), &cancelled).unwrap());
            }
            samples.push(start.elapsed().as_secs_f64() * 10_000.0);
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        println!("METRIC steady_state_cycle_us={median:.3}");
        println!("METRIC snapshot_calls={}", runtime.backend.snapshot_count);
        println!(
            "METRIC observed_apply_calls={}",
            runtime.backend.observed_apply_count
        );
    }

    #[test]
    #[ignore = "performance benchmark"]
    fn benchmark_new_window_event_to_correct_frame() {
        let existing = window(10);
        let created = window(11);
        let main_frame = frame(50);
        let secondary_frame = frame(1050);
        let mut samples = Vec::new();

        // One warmup followed by nine measured event-loop phases. The AX dirty bit
        // remains set until the production dispatch predicate accepts the refresh.
        for first_tick_ms in [1_u64, 1, 4, 7, 10, 13, 16, 19, 22, 25] {
            let mut backend = FakeBackend {
                focused: Some(existing.clone()),
                frontmost: Some(existing.app.clone()),
                observation: Some(observation_for_windows(
                    "com.mitchellh.ghostty",
                    &existing,
                    &[(existing.clone(), main_frame)],
                )),
                ..FakeBackend::default()
            };
            backend.frames.insert(existing.clone(), main_frame);
            let mut runtime = runtime(backend);
            runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

            runtime.backend.focused = Some(created.clone());
            runtime.backend.frontmost = Some(created.app.clone());
            runtime
                .backend
                .frames
                .insert(created.clone(), secondary_frame);
            runtime.backend.observation = Some(observation_for_windows(
                "com.mitchellh.ghostty",
                &created,
                &[
                    (existing.clone(), main_frame),
                    (created.clone(), secondary_frame),
                ],
            ));

            let mut dispatch_ms = first_tick_ms;
            let activation_ready_ms =
                first_tick_ms + APPLICATION_ACTIVATION_SETTLE.as_millis() as u64;
            while !refresh_due(true, true, false, dispatch_ms >= activation_ready_ms) {
                dispatch_ms += TICK_INTERVAL.as_millis() as u64;
            }
            let apply_started = Instant::now();
            runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
            let latency_ms = dispatch_ms as f64 + apply_started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(
                runtime.workspaces.workspace_of(&created),
                Some(WorkspaceId::ONE)
            );
            assert!(runtime.backend.frames[&created].x < 1000);
            samples.push(latency_ms);
        }

        samples.remove(0);
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        let spread = samples.last().unwrap() - samples.first().unwrap();
        println!("METRIC new_window_correct_frame_ms={median:.3}");
        println!("METRIC new_window_correct_frame_spread_ms={spread:.3}");
        println!("METRIC new_window_correct_frame_best_ms={:.3}", samples[0]);
        println!(
            "METRIC new_window_correct_frame_worst_ms={:.3}",
            samples[samples.len() - 1]
        );
    }

    #[test]
    fn inactive_windows_are_positioned_offscreen_without_a_minimize_operation() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend::default();
        backend.frames.insert(window.clone(), normal);
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::new(2).unwrap(), &window);

        let mut failed = 0;
        let outputs = outputs();
        let visible = runtime.visible_workspaces(&outputs);
        runtime.park_inactive(&outputs, &visible, &AtomicBool::new(false), &mut failed);

        let displays = outputs.frames().collect::<Vec<_>>();
        let expected = crate::geometry::parked_frame(outputs.main().frame, &displays, normal);
        assert_eq!(failed, 0);
        assert_eq!(runtime.backend.frames[&window], expected);
        assert_eq!(runtime.backend.persisted[&window], normal);
        assert!(matches!(runtime.backend.calls[0], BackendCall::Persist));
        assert!(matches!(
            runtime.backend.calls[1],
            BackendCall::SetPosition(_, target) if target == expected
        ));
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
        runtime.park_inactive(&outputs, &visible, &AtomicBool::new(false), &mut failed);

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
    fn floating_rule_excludes_new_window_despite_unique_app_affinity() {
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
        let config = Config::parse(
            "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'float'",
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
        assert_eq!(runtime.workspaces.workspace_of(&created), None);
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
    fn floating_rule_excludes_window_from_layout() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend {
            observation: Some(observation(&window, normal)),
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), normal);
        let config = Config::parse(
            "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'float'",
        )
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
        assert!(
            !runtime
                .backend
                .calls
                .iter()
                .any(|call| matches!(call, BackendCall::Apply(_, _)))
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
    fn failed_route_parking_rolls_back_to_source_workspace() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend {
            observation: Some(observation(&window, normal)),
            fail_persist: true,
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
            Some(WorkspaceId::ONE)
        );
        assert!(!runtime.parked.contains_key(&window));
    }

    #[test]
    fn ghostty_native_tab_replacement_is_not_rerouted() {
        let previous = window(10);
        let replacement = window(11);
        let normal = frame(50);
        let mut backend = FakeBackend {
            observation: Some(observation_for("com.mitchellh.ghostty", &previous, normal)),
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
        let accepted = runtime.backend.frames[&previous];
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
        assert!(!runtime.parked.contains_key(&replacement));
    }

    #[test]
    fn restore_all_repositions_windows_at_their_original_frames() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend::default();
        backend.frames.insert(window.clone(), normal);
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::new(2).unwrap(), &window);
        let outputs = outputs();
        let visible = runtime.visible_workspaces(&outputs);
        runtime.park_inactive(&outputs, &visible, &AtomicBool::new(false), &mut 0);

        assert_eq!(runtime.restore_all(), 0);
        assert_eq!(runtime.backend.frames[&window], normal);
        assert!(runtime.backend.persisted.is_empty());
    }

    #[test]
    fn restore_persistence_failure_keeps_retry_state() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend {
            fail_persist: true,
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), frame(5000));
        let mut parked = HashMap::new();
        parked.insert(window.clone(), normal);
        let mut runtime = WorkspaceRuntime::with_backend_and_config(
            ManagementScope::Both,
            backend,
            parked,
            Config::default(),
        );

        assert_eq!(
            runtime.restore_window(&window),
            macos::ApplyOutcome::Applied
        );

        assert_eq!(runtime.backend.frames[&window], normal);
        assert_eq!(runtime.parked[&window], normal);
    }

    #[test]
    fn moving_to_an_inactive_workspace_does_not_switch() {
        let window = window(10);
        let mut backend = FakeBackend {
            focused: Some(window.clone()),
            observation: Some(observation(&window, frame(50))),
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), frame(50));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &window);
        let target = WorkspaceId::new(2).unwrap();

        runtime
            .move_focused(target, outputs(), &AtomicBool::new(false))
            .unwrap();

        assert_eq!(
            runtime.workspaces.active(DisplayRole::Main),
            WorkspaceId::ONE
        );
        assert_eq!(runtime.workspaces.workspace_of(&window), Some(target));
        assert!(runtime.parked.contains_key(&window));
        assert!(
            !runtime
                .backend
                .calls
                .iter()
                .any(|call| matches!(call, BackendCall::Focus(_)))
        );

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert_eq!(
            runtime.workspaces.active(DisplayRole::Main),
            WorkspaceId::ONE
        );
    }

    #[test]
    fn focusing_a_parked_window_activates_its_workspace() {
        let window = window(10);
        let normal = frame(50);
        let outputs = outputs();
        let displays = outputs.frames().collect::<Vec<_>>();
        let parked_frame = crate::geometry::parked_frame(outputs.main().frame, &displays, normal);
        let mut backend = FakeBackend {
            focused: Some(window.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), parked_frame);
        let mut parked = HashMap::new();
        parked.insert(window.clone(), normal);
        let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
        let workspace = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, workspace, &window);

        let expected = outputs.main().frame;
        let status = runtime.cycle(outputs, &AtomicBool::new(false)).unwrap();

        assert_eq!(status.active, vec![workspace, WorkspaceId::SIX]);
        assert_eq!(runtime.backend.frames[&window], expected);
    }

    #[test]
    fn frontmost_app_activates_before_ax_focus_catches_up() {
        let old = window(10);
        let current = crate::PhysicalWindowId {
            app: AppInstanceId {
                pid: 2,
                launched_at: 2,
            },
            window_id: 20,
        };
        let normal = frame(50);
        let outputs = outputs();
        let displays = outputs.frames().collect::<Vec<_>>();
        let parked_frame = crate::geometry::parked_frame(outputs.main().frame, &displays, normal);
        let mut backend = FakeBackend {
            focused: Some(old.clone()),
            frontmost: Some(current.app.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(old.clone(), normal);
        backend.frames.insert(current.clone(), parked_frame);
        let mut parked = HashMap::new();
        parked.insert(current.clone(), normal);
        let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
        bind(&mut runtime, WorkspaceId::ONE, &old);
        let target = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, target, &current);

        let status = runtime.cycle(outputs, &AtomicBool::new(false)).unwrap();

        assert_eq!(status.active, vec![target, WorkspaceId::SIX]);
    }

    #[test]
    fn maximize_toggles_without_changing_tile_order() {
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

        runtime
            .toggle_maximize(outputs.clone(), &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.backend.frames[&second], outputs.main().frame);
        assert_eq!(
            runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
            vec![first.clone(), second.clone()]
        );

        runtime
            .toggle_maximize(outputs, &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.backend.frames[&first].width, 500);
        assert_eq!(runtime.backend.frames[&second].x, 500);
        assert_eq!(
            runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
            vec![first, second]
        );
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
    fn dragging_to_a_tile_edge_previews_a_nested_insertion() {
        let moving = window(10);
        let target = window(20);
        let third = window(30);
        let mut backend = FakeBackend {
            focused: Some(moving.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(moving.clone(), frame(50));
        backend.frames.insert(target.clone(), frame(400));
        backend.frames.insert(third.clone(), frame(750));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &moving);
        bind(&mut runtime, WorkspaceId::ONE, &target);
        bind(&mut runtime, WorkspaceId::ONE, &third);

        runtime
            .drop_window(
                moving.clone(),
                (500, 10),
                true,
                outputs(),
                &AtomicBool::new(false),
            )
            .unwrap();

        let placements = runtime
            .workspaces
            .engine(WorkspaceId::ONE)
            .placements(&Settings::for_output(outputs().main().frame));
        assert_eq!(
            placements[0].frame,
            crate::Rect {
                x: 0,
                y: 0,
                width: 500,
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
                y: 0,
                width: 500,
                height: 800
            }
        );
    }

    #[test]
    fn dragging_a_tiled_window_over_its_sibling_live_previews_the_swap() {
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
            .drop_window(
                first.clone(),
                (750, 400),
                true,
                outputs(),
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(
            runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
            vec![second.clone(), first.clone()]
        );
        assert!(runtime.backend.calls.contains(&BackendCall::Apply(
            second,
            crate::Rect {
                x: 0,
                y: 0,
                width: 500,
                height: 800,
            }
        )));
        assert!(
            !runtime
                .backend
                .calls
                .iter()
                .any(|call| matches!(call, BackendCall::Apply(window, _) if window == &first))
        );
    }

    #[test]
    fn dropping_a_tiled_window_on_another_display_moves_it() {
        let moving = window(10);
        let target = window(20);
        let mut backend = FakeBackend {
            focused: Some(moving.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(moving.clone(), frame(50));
        backend.frames.insert(target.clone(), frame(1050));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &moving);
        bind(&mut runtime, WorkspaceId::SIX, &target);

        runtime
            .drop_window(
                moving.clone(),
                (1500, 400),
                false,
                outputs(),
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(
            runtime.workspaces.workspace_of(&moving),
            Some(WorkspaceId::SIX)
        );
    }

    #[test]
    fn switching_reveals_and_lays_out_incoming_before_parking_outgoing() {
        let outgoing = window(10);
        let incoming = window(20);
        let outgoing_normal = frame(50);
        let incoming_normal = frame(100);
        let outputs = outputs();
        let displays = outputs.frames().collect::<Vec<_>>();
        let incoming_parked =
            crate::geometry::parked_frame(outputs.main().frame, &displays, incoming_normal);
        let mut backend = FakeBackend::default();
        backend.frames.insert(outgoing.clone(), outgoing_normal);
        backend.frames.insert(incoming.clone(), incoming_parked);
        let mut parked = HashMap::new();
        parked.insert(incoming.clone(), incoming_normal);
        let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
        bind(&mut runtime, WorkspaceId::ONE, &outgoing);
        let target = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, target, &incoming);

        runtime
            .switch(target, outputs.clone(), &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.workspaces.active(DisplayRole::Main), target);
        assert_eq!(runtime.backend.focused, Some(incoming.clone()));
        assert!(runtime.parked.contains_key(&outgoing));
        assert!(!runtime.parked.contains_key(&incoming));
        let reveal = runtime
            .backend
            .calls
            .iter()
            .position(|call| {
                matches!(call, BackendCall::SetPosition(window, frame) if window == &incoming && frame == &incoming_normal)
            })
            .unwrap();
        let layout = runtime
            .backend
            .calls
            .iter()
            .position(|call| matches!(call, BackendCall::Apply(window, _) if window == &incoming))
            .unwrap();
        let hide = runtime
            .backend
            .calls
            .iter()
            .position(
                |call| matches!(call, BackendCall::SetPosition(window, _) if window == &outgoing),
            )
            .unwrap();
        assert!(reveal < layout && layout < hide);
    }

    #[test]
    fn workspace_back_and_forth_tracks_each_display_lane_independently() {
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
        let main_target = WorkspaceId::new(2).unwrap();
        let secondary_target = WorkspaceId::new(7).unwrap();
        let cancelled = AtomicBool::new(false);

        runtime.switch(main_target, outputs(), &cancelled).unwrap();
        runtime.backend.focused = Some(secondary.clone());
        runtime
            .switch(secondary_target, outputs(), &cancelled)
            .unwrap();

        runtime.backend.focused = Some(main);
        runtime
            .workspace_back_and_forth(outputs(), &cancelled)
            .unwrap();
        assert_eq!(
            runtime.workspaces.active(DisplayRole::Main),
            WorkspaceId::ONE
        );
        assert_eq!(
            runtime.workspaces.active(DisplayRole::Secondary),
            secondary_target
        );

        runtime.backend.focused = Some(secondary);
        runtime
            .workspace_back_and_forth(outputs(), &cancelled)
            .unwrap();
        assert_eq!(
            runtime.workspaces.active(DisplayRole::Main),
            WorkspaceId::ONE
        );
        assert_eq!(
            runtime.workspaces.active(DisplayRole::Secondary),
            WorkspaceId::SIX
        );
        assert_eq!(
            runtime.workspaces.previous_for(&DisplaySelector::Main),
            Some(main_target)
        );
        assert_eq!(
            runtime.workspaces.previous_for(&DisplaySelector::Secondary),
            Some(secondary_target)
        );
    }

    #[test]
    fn switching_aborts_when_the_incoming_workspace_cannot_be_laid_out() {
        let incoming = window(20);
        let incoming_normal = frame(100);
        let outputs = outputs();
        let displays = outputs.frames().collect::<Vec<_>>();
        let incoming_parked =
            crate::geometry::parked_frame(outputs.main().frame, &displays, incoming_normal);
        let mut backend = FakeBackend {
            managed_outcome: Some(macos::ApplyOutcome::Unavailable),
            ..FakeBackend::default()
        };
        backend.frames.insert(incoming.clone(), incoming_parked);
        let mut parked = HashMap::new();
        parked.insert(incoming.clone(), incoming_normal);
        let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
        let target = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, target, &incoming);

        assert!(
            runtime
                .switch(target, outputs.clone(), &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(
            runtime.workspaces.active(DisplayRole::Main),
            WorkspaceId::ONE
        );
        assert!(runtime.parked.contains_key(&incoming));
    }

    #[test]
    fn persistence_failure_prevents_offscreen_positioning() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend {
            fail_persist: true,
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), normal);
        let mut runtime = runtime(backend);

        assert_eq!(
            runtime.park_window(&window, outputs().main().frame, &outputs()),
            macos::ApplyOutcome::Unavailable
        );
        assert_eq!(runtime.backend.frames[&window], normal);
        assert!(
            !runtime
                .backend
                .calls
                .iter()
                .any(|call| matches!(call, BackendCall::SetPosition(_, _)))
        );
    }

    #[test]
    fn persisted_frames_recover_after_a_simulated_crash() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend::default();
        backend.frames.insert(window.clone(), normal);
        let mut first = runtime(backend);
        assert_eq!(
            first.park_window(&window, outputs().main().frame, &outputs()),
            macos::ApplyOutcome::Applied
        );
        let parked_frame = first.backend.frames[&window];
        let persisted = first.backend.persisted.clone();

        let mut second_backend = FakeBackend::default();
        second_backend.frames.insert(window.clone(), parked_frame);
        let mut second =
            WorkspaceRuntime::with_backend(ManagementScope::Both, second_backend, persisted);

        assert_eq!(second.restore_all(), 0);
        assert_eq!(second.backend.frames[&window], normal);
        assert!(second.backend.persisted.is_empty());
    }
}
