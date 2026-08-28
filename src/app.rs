use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::PathBuf,
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
    live, login_item, macos, permission_guide, settings, sparkle, status_item, termination,
};

/// Timestamped diagnostic line on stderr (redirected to `agent.log` in bundle
/// launches). Never include window titles or other user content.
macro_rules! log_event {
    ($($arg:tt)*) => {{
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        eprintln!(
            "[{}.{:03}] {}",
            now.as_secs(),
            now.subsec_millis(),
            format_args!($($arg)*)
        );
    }};
}

const TICK_INTERVAL: Duration = Duration::from_millis(25);
const FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const APPLICATION_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RESTORE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const APPLICATION_ACTIVATION_SETTLE: Duration = Duration::from_millis(50);
const KEYBOARD_RESIZE_INCREMENT: i32 = 50;
/// How long a fresh park must settle before an on-screen observation of that
/// window is trusted as a genuine reappearance rather than in-flight movement.
const PARK_SETTLE_GRACE: Duration = Duration::from_secs(2);
/// Two confirmed self-un-parks within this window quarantines the window:
/// its application insists on managing it, and fighting back loses.
const QUARANTINE_WINDOW: Duration = Duration::from_secs(300);
/// A restore is confirmed when the window server reports the window near its
/// target. Loose on purpose: apps may clamp the exact position, while a
/// still-parked window sits a full display dimension away.
const RESTORE_CONFIRM_TOLERANCE: i32 = 64;
/// After this many undone re-parks in a row the window's reappearance is
/// honored (re-homed) instead of fought; repeated honors quarantine it.
const REPARK_ATTEMPT_LIMIT: u8 = 3;
/// How long a workspace switch waits for slow applications to actually move
/// their windows on screen before the outgoing workspace is hidden. Keeps the
/// bare desktop from ever showing during a switch.
const REVEAL_SETTLE_LIMIT: Duration = Duration::from_millis(300);
/// Poll interval while waiting for incoming windows to arrive.
const REVEAL_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long a pending reveal waits before the restore write is re-issued.
const REVEAL_REWRITE_INTERVAL: Duration = Duration::from_millis(100);

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
    ToggleFullscreenLayout,
    OpenSettings,
    OpenAccessibility,
    CheckForUpdates,
    InstallUpdate,
    Terminate,
    ToggleLaunchAtLogin,
    Restart,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerAction {
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
    ToggleFullscreenLayout,
}

impl TryFrom<Action> for WorkerAction {
    type Error = ();

    fn try_from(action: Action) -> Result<Self, Self::Error> {
        match action {
            Action::Retile => Ok(Self::Retile),
            Action::SwitchWorkspace(workspace) => Ok(Self::SwitchWorkspace(workspace)),
            Action::WorkspaceBackAndForth => Ok(Self::WorkspaceBackAndForth),
            Action::MoveFocused(workspace) => Ok(Self::MoveFocused(workspace)),
            Action::MoveApp(bundle_id, workspace) => Ok(Self::MoveApp(bundle_id, workspace)),
            Action::Focus(direction) => Ok(Self::Focus(direction)),
            Action::Reorder(direction) => Ok(Self::Reorder(direction)),
            Action::ToggleLayout => Ok(Self::ToggleLayout),
            Action::Resize(direction) => Ok(Self::Resize(direction)),
            Action::Balance => Ok(Self::Balance),
            Action::ToggleFullscreenLayout => Ok(Self::ToggleFullscreenLayout),
            Action::ToggleAutomatic
            | Action::OpenSettings
            | Action::OpenAccessibility
            | Action::CheckForUpdates
            | Action::InstallUpdate
            | Action::Terminate
            | Action::ToggleLaunchAtLogin
            | Action::Restart
            | Action::Quit => Err(()),
        }
    }
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
    ToggleFullscreenLayout(macos::DisplayOutputs),
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
    Update,
    Terminate,
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
    layouts: Vec<(String, Vec<crate::Rect>)>,
    failed: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
struct RuntimeDebug {
    frontmost: Option<AppInstanceId>,
    focused: Option<crate::PhysicalWindowId>,
    active_workspaces: Vec<WorkspaceId>,
    displays: Vec<DebugDisplay>,
    tiles: Vec<NavigationTile>,
    floating: Vec<DebugFloatingWindow>,
    /// Quarantined windows left unmanaged because they un-park themselves.
    ignored: Vec<crate::PhysicalWindowId>,
    targets: DebugTargets,
}

#[derive(Clone, Debug, serde::Serialize)]
struct DebugFloatingWindow {
    window: crate::PhysicalWindowId,
    workspace: WorkspaceId,
    /// Present exactly when the window is parked off-screen.
    parked_original_frame: Option<crate::Rect>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct DebugDisplay {
    id: String,
    name: String,
    frame: crate::Rect,
    workspace: WorkspaceId,
    axis: crate::Axis,
    fullscreen_layout: bool,
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
    /// Never tiled, owned, or parked (ignore rule or quarantine).
    Ignored,
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
    /// Authoritative window-server frame, immune to stale Accessibility reads.
    fn window_server_frame(&mut self, window: &crate::PhysicalWindowId) -> Option<crate::Rect>;
    /// Authoritative window-server frames for all windows in one snapshot.
    /// Use for per-cycle checks instead of repeated single-window queries.
    fn window_server_frames(&mut self) -> HashMap<u32, crate::Rect>;
    fn set_position(
        &mut self,
        window: &crate::PhysicalWindowId,
        target: crate::Rect,
    ) -> macos::ApplyOutcome;
    fn focused_window(&mut self) -> Option<crate::PhysicalWindowId>;
    fn frontmost_application(&mut self) -> Option<AppInstanceId>;
    fn raise_window(&mut self, window: &crate::PhysicalWindowId) -> bool;
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

    fn window_server_frame(&mut self, window: &crate::PhysicalWindowId) -> Option<crate::Rect> {
        macos::window_server_frame(window)
    }

    fn window_server_frames(&mut self) -> HashMap<u32, crate::Rect> {
        macos::window_server_frames()
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

    fn raise_window(&mut self, window: &crate::PhysicalWindowId) -> bool {
        macos::raise_window(window)
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

/// Shared per-cycle context for parking decisions.
struct ParkContext<'a> {
    displays: &'a [crate::Rect],
    union: crate::Rect,
    frontmost: &'a Option<AppInstanceId>,
    server_frames: &'a HashMap<u32, crate::Rect>,
    visible: &'a [ResolvedWorkspace],
}

/// Cross-thread coordination flags between the app loop and the worker.
struct WorkerSignals {
    /// Requests abandonment of in-flight interruptible work.
    cancelled: Arc<AtomicBool>,
    /// Whether the currently executing worker command may be cancelled by an
    /// activation change (background cycles yes, explicit user commands no).
    interruptible: Arc<AtomicBool>,
}

struct AppLoop {
    actions: Receiver<Action>,
    actions_sender: Sender<Action>,
    commands: Sender<WorkerCommand>,
    results: Receiver<WorkerResult>,
    cancelled: Arc<AtomicBool>,
    worker_interruptible: Arc<AtomicBool>,
    scope: ManagementScope,
    events: macos::WindowEventMonitor,
    hotkey_workspaces: Vec<WorkspaceId>,
    controls: Receiver<ControlRequest>,
    _control_server: control::Server,
}

pub fn run() -> Result<(), String> {
    log_event!(
        "agent started (pid {}, version {})",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    let scope = ManagementScope::from_environment()?;
    let parked = load_parked()?;
    let (config, workspace_state) =
        recover_startup_metadata(Config::load(), load_workspace_state());
    let mut configured_workspaces = Workspaces::default();
    config.configure_workspaces(&mut configured_workspaces);
    let mut hotkey_workspaces = WorkspaceId::numbered()
        .chain(config.named_workspaces())
        .chain(
            workspace_state
                .as_ref()
                .into_iter()
                .flat_map(crate::workspace::WorkspaceState::named_workspaces_in_use),
        )
        .filter(|workspace| scope.manages_selector(configured_workspaces.assignment(*workspace)))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    hotkey_workspaces.sort();
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
    let worker_interruptible = Arc::new(AtomicBool::new(true));
    spawn_worker(
        worker_commands,
        worker_results,
        WorkerSignals {
            cancelled: Arc::clone(&cancelled),
            interruptible: Arc::clone(&worker_interruptible),
        },
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
        if let Err(error) = termination::install(actions_sender.clone()) {
            log_event!("failed to install application termination gate: {error}");
            cx.quit();
            return;
        }
        if let Err(error) = status_item::install(actions_sender.clone()) {
            log_event!("failed to install Spaceman status item: {error}");
            quit_app(cx);
            return;
        }
        sparkle::start(actions_sender.clone());
        match login_item::status() {
            Ok(status) => status_item::set_launch_at_login(status),
            Err(error) => status_item::set_status(&format!("Launch at Login unavailable: {error}")),
        }
        match scope {
            ManagementScope::Both => status_item::set_workspace_numbers(&[1, 6]),
            ManagementScope::Main => status_item::set_workspace_numbers(&[1]),
            ManagementScope::Secondary => status_item::set_workspace_numbers(&[6]),
        }
        if !macos::accessibility_trusted() {
            status_item::set_status("Paused · Accessibility required");
            if let Err(error) = settings::open(actions_sender.clone(), cx) {
                status_item::set_status(&format!("Could not open Settings: {error}"));
            }
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
                    worker_interruptible,
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

fn quit_app(cx: &mut gpui::App) {
    termination::allow_next();
    cx.quit();
}

async fn run_app_loop(cx: &mut gpui::AsyncApp, app: AppLoop) {
    let AppLoop {
        actions,
        actions_sender,
        commands,
        results,
        cancelled,
        worker_interruptible,
        scope,
        events,
        hotkey_workspaces,
        controls,
        _control_server,
    } = app;
    let (mut _hotkeys, hotkey_error) = if macos::accessibility_trusted() {
        match HotkeyPlan::current(&hotkey_workspaces)
            .and_then(|plan| Hotkeys::install(actions_sender.clone(), plan))
        {
            Ok(hotkeys) => (Some(hotkeys), None),
            Err(error) => (None, Some(format!("Hotkeys unavailable: {error}"))),
        }
    } else {
        (None, Some("Accessibility permission is required".into()))
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
                    if exit_intent.is_some_and(|intent| {
                        matches!(intent, ExitIntent::Update | ExitIntent::Terminate)
                    }) {
                        automatic = false;
                        worker_busy = false;
                        last_error =
                            Some("Exit blocked because window restoration is unavailable".into());
                        status_item::set_status("Exit blocked · restoration unavailable");
                        break;
                    }
                    status_item::set_status("Worker stopped unexpectedly");
                    if exit_intent.is_some_and(|intent| {
                        !matches!(intent, ExitIntent::Update | ExitIntent::Terminate)
                    }) {
                        let _ = cx.update(quit_app);
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
                WorkerResult::Restored { failed: 0 } if exit_intent == Some(ExitIntent::Update) => {
                    let _ = commands.send(WorkerCommand::Stop);
                    worker_busy = false;
                    restoring = false;
                    let continuation = cx
                        .update(|_| sparkle::continue_install())
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    match continuation {
                        Ok(()) => {
                            termination::allow_next();
                            status_item::set_status("Installing update...");
                        }
                        Err(error) => {
                            log_event!("Sparkle continuation failed after restoration: {error}");
                            let _ = cx.update(quit_app);
                            return;
                        }
                    }
                }
                WorkerResult::Restored { failed: 0 }
                    if exit_intent == Some(ExitIntent::Terminate) =>
                {
                    let _ = commands.send(WorkerCommand::Stop);
                    termination::reply(true);
                    return;
                }
                WorkerResult::Restored { failed: 0 } if exit_intent.is_some() => {
                    let _ = commands.send(WorkerCommand::Stop);
                    let _ = cx.update(quit_app);
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
                if !worker_busy
                    && exit_intent.is_some_and(|intent| {
                        !matches!(intent, ExitIntent::Update | ExitIntent::Terminate)
                    })
                {
                    let _ = cx.update(quit_app);
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
                    } else if exit_intent == Some(ExitIntent::Update) {
                        "updating"
                    } else if exit_intent == Some(ExitIntent::Terminate) {
                        "terminating"
                    } else if restoring || restore_pending {
                        "restoring"
                    } else if automatic {
                        "automatic"
                    } else {
                        "paused"
                    },
                    "busy": worker_busy,
                    "last_error": last_error.as_deref(),
                    "active_workspaces": active_workspaces,
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
                    .map(|()| format!("switch to workspace {workspace} queued"))
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::WorkspaceBackAndForth if automatic => actions_sender
                    .send(Action::WorkspaceBackAndForth)
                    .map(|()| "workspace back-and-forth queued".into())
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::Move(workspace) if automatic => actions_sender
                    .send(Action::MoveFocused(workspace))
                    .map(|()| format!("move to workspace {workspace} queued"))
                    .map_err(|_| "Spaceman agent is stopping".into()),
                ControlCommand::MoveApp(ref bundle_id, workspace) if automatic => actions_sender
                    .send(Action::MoveApp(bundle_id.clone(), workspace))
                    .map(|()| format!("move app to workspace {workspace} queued"))
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
                    if !macos::accessibility_trusted() {
                        last_error = Some("Accessibility permission is required".into());
                        status_item::set_status("Paused · Accessibility required");
                        continue;
                    }
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
                Action::OpenSettings => {
                    let actions = actions_sender.clone();
                    let result = cx
                        .update(|cx| settings::open(actions, cx))
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    if let Err(error) = result {
                        status_item::set_status(&format!("Could not open Settings: {error}"));
                    }
                }
                Action::OpenAccessibility => permission_guide::show(),
                Action::CheckForUpdates => {
                    sparkle::check_for_updates();
                    let _ = cx.update(settings::refresh);
                }
                Action::ToggleLaunchAtLogin => match login_item::toggle() {
                    Ok(status) => {
                        status_item::set_launch_at_login(status);
                        let _ = cx.update(settings::refresh);
                        if status == login_item::Status::RequiresApproval {
                            status_item::set_status(
                                "Launch at Login requires approval in System Settings",
                            );
                        }
                    }
                    Err(error) => {
                        status_item::set_status(&format!(
                            "Could not change Launch at Login: {error}"
                        ));
                    }
                },
                Action::Restart | Action::Quit | Action::InstallUpdate | Action::Terminate => {
                    let intent = match action {
                        Action::Restart => ExitIntent::Restart,
                        Action::InstallUpdate => ExitIntent::Update,
                        Action::Terminate => ExitIntent::Terminate,
                        _ => ExitIntent::Quit,
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
                            ExitIntent::Update => "Update ready · restoring windows...",
                            ExitIntent::Terminate => "Terminating · restoring windows...",
                        });
                    } else {
                        match intent {
                            ExitIntent::Update | ExitIntent::Terminate => {
                                last_error = Some(
                                    "Exit blocked because window restoration is unavailable".into(),
                                );
                                status_item::set_status("Exit blocked · restoration unavailable");
                            }
                            ExitIntent::Quit | ExitIntent::Restart => {
                                let _ = cx.update(quit_app);
                                return;
                            }
                        }
                    }
                }
                action => {
                    if let Ok(action) = WorkerAction::try_from(action) {
                        if automatic && !worker_busy {
                            pending_action = None;
                            send_worker_action(&commands, action, &mut worker_busy, &cancelled);
                        } else if automatic {
                            queue_latest(&mut pending_action, action);
                        }
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
        if automatic
            && !worker_busy
            && let Some(action) = pending_action.take()
        {
            send_worker_action(&commands, action, &mut worker_busy, &cancelled);
        }
        if restoring
            && !worker_busy
            && next_restore_retry.is_some_and(|retry| Instant::now() >= retry)
        {
            next_restore_retry = None;
            send_restore(&commands, &mut worker_busy, &mut restoring);
            if !worker_busy {
                match exit_intent {
                    Some(ExitIntent::Update | ExitIntent::Terminate) => {
                        last_error =
                            Some("Exit blocked because window restoration is unavailable".into());
                        status_item::set_status("Exit blocked · restoration unavailable");
                    }
                    Some(ExitIntent::Quit | ExitIntent::Restart) => {
                        let _ = cx.update(quit_app);
                        return;
                    }
                    None => status_item::set_status("Paused · worker unavailable"),
                }
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
                // Only background cycles yield to activation changes. Explicit
                // user commands (switch, move, focus) change the frontmost app
                // themselves; cancelling them here would abort our own work.
                if worker_busy && worker_interruptible.load(Ordering::Acquire) {
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

fn queue_latest(pending_action: &mut Option<WorkerAction>, action: WorkerAction) {
    pending_action.replace(action);
}

fn send_worker_action(
    commands: &Sender<WorkerCommand>,
    action: WorkerAction,
    worker_busy: &mut bool,
    cancelled: &AtomicBool,
) {
    if matches!(action, WorkerAction::Retile) {
        cancelled.store(false, Ordering::Release);
    }
    send_with_outputs(
        commands,
        |outputs| match action {
            WorkerAction::Retile => WorkerCommand::Cycle(outputs),
            WorkerAction::SwitchWorkspace(workspace) => WorkerCommand::Switch(workspace, outputs),
            WorkerAction::WorkspaceBackAndForth => WorkerCommand::WorkspaceBackAndForth(outputs),
            WorkerAction::MoveFocused(workspace) => WorkerCommand::MoveFocused(workspace, outputs),
            WorkerAction::MoveApp(bundle_id, workspace) => {
                WorkerCommand::MoveApp(bundle_id, workspace, outputs)
            }
            WorkerAction::Focus(direction) => WorkerCommand::Focus(direction, outputs),
            WorkerAction::Reorder(direction) => WorkerCommand::Reorder(direction, outputs),
            WorkerAction::ToggleLayout => WorkerCommand::ToggleLayout(outputs),
            WorkerAction::Resize(direction) => WorkerCommand::Resize(direction, outputs),
            WorkerAction::Balance => WorkerCommand::Balance(outputs),
            WorkerAction::ToggleFullscreenLayout => WorkerCommand::ToggleFullscreenLayout(outputs),
        },
        worker_busy,
    );
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
    signals: WorkerSignals,
    scope: ManagementScope,
    parked: HashMap<crate::PhysicalWindowId, crate::Rect>,
    config: Config,
    workspace_state: Option<crate::workspace::WorkspaceState>,
) {
    let WorkerSignals {
        cancelled,
        interruptible,
    } = signals;
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
            // Every command starts uncancelled: a cancellation aimed at previous
            // work must never abort a freshly dispatched command. Background
            // cycles remain interruptible by activation changes; explicit user
            // commands are atomic once native writes begin.
            cancelled.store(false, Ordering::Release);
            interruptible.store(
                matches!(command, WorkerCommand::Cycle(_)),
                Ordering::Release,
            );
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
                WorkerCommand::ToggleFullscreenLayout(outputs) => {
                    let debug_outputs = outputs.clone();
                    let result = runtime.toggle_fullscreen_layout(outputs, &cancelled);
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
    /// When each window was last parked. Apps apply position writes
    /// asynchronously, so a fresh park must settle before an on-screen
    /// observation may be trusted as a genuine reappearance.
    recently_parked: HashMap<crate::PhysicalWindowId, Instant>,
    /// When each window last un-parked itself (confirmed by the window server).
    recent_heals: HashMap<crate::PhysicalWindowId, Instant>,
    /// Consecutive re-parks undone by the app; past the limit the window's
    /// reappearance is honored instead of fought.
    repark_attempts: HashMap<crate::PhysicalWindowId, u8>,
    /// Windows that repeatedly un-park themselves; left unmanaged for the session.
    quarantined: HashSet<crate::PhysicalWindowId>,
    config: Config,
    foreground_lanes: HashMap<String, DisplaySelector>,
    suppressed_focus: Option<crate::PhysicalWindowId>,
    pending_fullscreen_focus_restore: HashSet<WorkspaceId>,
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
        let restored_workspace_state = workspace_state.is_some();
        let mut workspaces = Workspaces::default();
        config.configure_workspaces(&mut workspaces);
        if let Some(state) = workspace_state {
            workspaces.restore(state);
        }
        let pending_fullscreen_focus_restore = if restored_workspace_state {
            WorkspaceId::all()
                .filter(|workspace| {
                    workspaces.uses_fullscreen_layout(*workspace)
                        && scope.manages_selector(workspaces.assignment(*workspace))
                })
                .collect()
        } else {
            HashSet::new()
        };
        let persisted_workspace_state = workspaces.state();
        Self {
            backend,
            scope,
            workspaces,
            known_apps: std::array::from_fn(|_| HashMap::new()),
            constrained: std::array::from_fn(|_| HashMap::new()),
            parked,
            recently_parked: HashMap::new(),
            recent_heals: HashMap::new(),
            repark_attempts: HashMap::new(),
            quarantined: HashSet::new(),
            config,
            foreground_lanes: HashMap::new(),
            suppressed_focus: None,
            pending_fullscreen_focus_restore,
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
                            .min_by_key(|(_, workspace)| *workspace)
                    })
                    .or_else(|| candidates().min_by_key(|(_, workspace)| *workspace))?;
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
            .flat_map(|resolved| self.workspaces.windows(resolved.workspace))
            .collect();
        for window in windows {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            if !self.parked.contains_key(&window) {
                continue;
            }
            // Throttle retries of restores awaiting window-server confirmation
            // so a slow app is not written to and logged about every cycle.
            if self
                .recently_parked
                .get(&window)
                .is_some_and(|attempted_at| attempted_at.elapsed() < RESTORE_RETRY_INTERVAL)
            {
                continue;
            }
            if !matches!(
                self.restore_window(&window),
                macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
            ) {
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
            .and_then(|focused| self.workspaces.workspace_of_any(focused))
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
        // One window-server snapshot serves every authoritative-frame check in
        // this cycle; per-window queries would each copy the full window list.
        let server_frames = self.backend.window_server_frames();
        self.restore_visible(&visible, cancelled, &mut counts.failed);
        self.park_inactive(
            &outputs,
            &visible,
            &server_frames,
            cancelled,
            &mut counts.failed,
        );
        if cancelled.load(Ordering::Acquire) {
            return Err("workspace operation cancelled".into());
        }
        let mut observation = self
            .backend
            .snapshot(0, &outputs)
            .map_err(|error| format!("Observation error: {error}"))?;
        let affinity_policy =
            self.freeze_window_affinities(&observation, &visible, &server_frames)?;
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
        let mut attempted_focus_restore = HashSet::new();
        for resolved in &visible {
            if !self
                .pending_fullscreen_focus_restore
                .contains(&resolved.workspace)
            {
                continue;
            }
            attempted_focus_restore.insert(resolved.workspace);
            let Some(window) = self.workspaces.preferred_focus(resolved.workspace) else {
                self.pending_fullscreen_focus_restore
                    .remove(&resolved.workspace);
                continue;
            };
            let restored = if focus_workspace == Some(resolved.workspace) {
                self.backend.focus_window(&window)
            } else {
                self.backend.raise_window(&window)
            };
            self.pending_fullscreen_focus_restore
                .remove(&resolved.workspace);
            if !restored {
                counts.failed += 1;
            }
        }
        if focused_is_frontmost
            && let Some(focused) = focused.as_ref()
            && let Some(workspace) = self.workspaces.workspace_of(focused)
            && self.workspaces.uses_fullscreen_layout(workspace)
            && !attempted_focus_restore.contains(&workspace)
            && visible
                .iter()
                .any(|resolved| resolved.workspace == workspace)
        {
            self.workspaces.remember_focus(workspace, focused);
        }
        self.persist_workspace_state_if_changed()?;
        Ok(self.status_for_visible(counts.applied, counts.failed, &visible))
    }

    fn freeze_window_affinities(
        &mut self,
        observation: &crate::NativeObservation,
        visible: &[ResolvedWorkspace],
        server_frames: &HashMap<u32, crate::Rect>,
    ) -> Result<FrozenAffinityPolicy, String> {
        let has_floating = self.workspaces.floating_entries().next().is_some();
        if has_floating {
            let floating_apps = self
                .workspaces
                .floating_entries()
                .map(|(window, _)| window.app.clone())
                .collect::<HashSet<_>>();
            for app in floating_apps {
                if !self.backend.is_process_running(&app) {
                    for window in self.workspaces.floating_windows_for_app(&app) {
                        log_event!("forget floating {:?}: app exited", window);
                        self.workspaces.forget_floating(&window);
                    }
                }
            }
        }

        let mut affinities = HashMap::new();
        for application in &observation.applications {
            let crate::AppObservation::Complete { app, windows, .. } = application else {
                continue;
            };
            if has_floating {
                let observed: HashSet<_> = windows.iter().map(|window| &window.id).collect();
                for window in self.workspaces.floating_windows_for_app(&app.id) {
                    if !observed.contains(&window) && !self.parked.contains_key(&window) {
                        log_event!("forget floating {:?}: not observed and not parked", window);
                        self.workspaces.forget_floating(&window);
                    }
                }
            }
            let app_workspace = self.workspaces.workspace_for_app(&app.id);
            for window in windows {
                self.heal_reappeared_window(window, visible, server_frames);
                let disposition = if self.config.has_rules() {
                    self.config.disposition(&app.bundle_id, window)
                } else {
                    WindowDisposition::Managed
                };
                if self.quarantined.contains(&window.id)
                    || disposition == WindowDisposition::Ignored
                {
                    if let Some(owner) = self.workspaces.workspace_of_any(&window.id) {
                        log_event!("release {:?} from workspace {}: ignored", window.id, owner);
                        self.forget_window(owner, &window.id);
                    }
                    affinities.insert(window.id.clone(), FrozenWindowAffinity::Ignored);
                    continue;
                }
                let floating = !window.resizable || disposition == WindowDisposition::Floating;
                if !floating && self.workspaces.floating_workspace_of(&window.id).is_some() {
                    self.workspaces.forget_floating(&window.id);
                }
                let affinity = if let Some(owner) = self.workspaces.workspace_of(&window.id) {
                    FrozenWindowAffinity::Owned(owner)
                } else if floating && self.workspaces.floating_workspace_of(&window.id).is_some() {
                    FrozenWindowAffinity::Floating
                } else if floating {
                    visible
                        .iter()
                        .find(|resolved| resolved.output.contains_center(window.frame))
                        .map_or(FrozenWindowAffinity::CurrentDisplay, |resolved| {
                            log_event!(
                                "adopt floating {:?} -> workspace {}",
                                window.id,
                                resolved.workspace
                            );
                            self.workspaces
                                .own_floating(window.id.clone(), resolved.workspace);
                            FrozenWindowAffinity::Floating
                        })
                } else {
                    match disposition {
                        WindowDisposition::Route(target) => {
                            if !self
                                .scope
                                .manages_selector(self.workspaces.assignment(target))
                            {
                                return Err(format!(
                                    "window rule routes to unmanaged workspace {}",
                                    target
                                ));
                            }
                            FrozenWindowAffinity::ExplicitRoute(target)
                        }
                        WindowDisposition::Managed => app_workspace.map_or(
                            FrozenWindowAffinity::CurrentDisplay,
                            FrozenWindowAffinity::Inferred,
                        ),
                        WindowDisposition::Floating | WindowDisposition::Ignored => unreachable!(),
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

    /// A window observed on-screen while still recorded as parked was brought back by
    /// its app or the user. Accessibility frames can be stale right after a park, so
    /// the reappearance must be confirmed by the window server before trusting it.
    /// Fighting a genuinely reappeared window back off-screen loses, so drop the
    /// stale recovery frame and re-home the window to the workspace it reappeared on.
    fn heal_reappeared_window(
        &mut self,
        window: &crate::ObservedWindow,
        visible: &[ResolvedWorkspace],
        server_frames: &HashMap<u32, crate::Rect>,
    ) {
        let Some(original) = self.parked.get(&window.id).copied() else {
            return;
        };
        if self
            .recently_parked
            .get(&window.id)
            .is_some_and(|parked_at| parked_at.elapsed() < PARK_SETTLE_GRACE)
        {
            // The park write may still be applying inside the app; wait it out.
            return;
        }
        // The observation may be stale; only the window server is trusted.
        let Some(frame) = server_frames.get(&window.id.window_id).copied() else {
            return;
        };
        let Some(target) = visible
            .iter()
            .find(|resolved| resolved.output.contains_center(frame))
            .map(|resolved| resolved.workspace)
        else {
            return;
        };
        if self.backend.frontmost_application().as_ref() != Some(&window.id.app) {
            // A background reappearance is an app re-asserting its saved bounds
            // (Electron does this on screen-parameter changes), not the user
            // summoning the window. Keep the recovery frame; parking pushes it
            // back off-screen instead of re-homing it.
            return;
        }
        self.honor_reappearance(&window.id, frame, original, target);
    }

    /// Honor a window that will not stay parked: drop the recovery frame and
    /// re-home it to the workspace it insists on appearing on. A second honor
    /// within [`QUARANTINE_WINDOW`] quarantines the window for the session.
    fn honor_reappearance(
        &mut self,
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
        original: crate::Rect,
        target: WorkspaceId,
    ) {
        self.commit_unparked(window);
        log_event!(
            "heal: {:?} reappeared at {:?}; dropping recovery frame {:?}",
            window,
            frame,
            original
        );
        self.persist_removals_or_rollback(vec![(window.clone(), original)]);
        if self.parked.contains_key(window) {
            return;
        }
        self.recent_heals
            .retain(|_, healed_at| healed_at.elapsed() < QUARANTINE_WINDOW);
        if self
            .recent_heals
            .insert(window.clone(), Instant::now())
            .is_some()
        {
            // Second confirmed self-un-park in a short window: the app insists
            // on managing this window. Stop fighting it for the session.
            log_event!(
                "quarantine: {:?} keeps un-parking itself; leaving it unmanaged",
                window
            );
            if let Some(owner) = self.workspaces.workspace_of_any(window) {
                self.forget_window(owner, window);
            }
            self.quarantined.insert(window.clone());
            return;
        }
        if self.workspaces.floating_workspace_of(window).is_some() {
            log_event!(
                "heal: re-home floating {:?} -> workspace {}",
                window,
                target
            );
            self.workspaces.own_floating(window.clone(), target);
        } else if let Some(source) = self.workspaces.move_window(window, target) {
            self.clear_relocation_evidence(source, target, window);
            log_event!(
                "heal: re-home tiled {:?} workspace {} -> {}",
                window,
                source,
                target
            );
        }
    }

    /// Bookkeeping shared by every path that moves a window between workspaces.
    fn clear_relocation_evidence(
        &mut self,
        source: WorkspaceId,
        target: WorkspaceId,
        window: &crate::PhysicalWindowId,
    ) {
        self.constrained[source.index()].remove(window);
        self.constrained[target.index()].remove(window);
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
                        FrozenWindowAffinity::Floating | FrozenWindowAffinity::Ignored => false,
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
        let filter_unchanged = routes.is_empty();
        let mut placements = if filter_unchanged {
            reconciliation.placements.clone()
        } else {
            self.workspaces.engine(workspace).placements(&settings)
        };
        self.apply_fullscreen_layout(workspace, &mut placements, output);
        if filter_unchanged {
            placements.retain(|placement| {
                placement
                    .window
                    .as_ref()
                    .and_then(|window| observed_frames.get(window))
                    .is_some_and(|frame| {
                        !frame.approximately_equals(placement.frame, settings.frame_tolerance)
                    })
            });
        }
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
        if !self.workspaces.is_enabled(workspace) {
            return Err(format!("Workspace {workspace} is not configured"));
        }
        let selector = self.workspaces.assignment(workspace).clone();
        if !self.scope.manages_selector(&selector) {
            return Err(format!(
                "Workspace {} is outside the managed display scope",
                workspace
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
            let focused = self.backend.focused_window();
            let windows = self.workspaces.windows(workspace);
            if let Some(focused) = focused.as_ref().filter(|focused| windows.contains(focused)) {
                if self.workspaces.workspace_of(focused) == Some(workspace) {
                    self.workspaces.remember_focus(workspace, focused);
                    self.pending_fullscreen_focus_restore.remove(&workspace);
                    self.persist_workspace_state_if_changed()?;
                }
                return Ok(self.status(0, 0, &outputs));
            }
            let workspace_focus = self
                .workspaces
                .preferred_focus(workspace)
                .filter(|window| windows.contains(window))
                .or_else(|| windows.first().cloned());
            if let Some(window) = workspace_focus {
                let focused_window = self.backend.focus_window(&window);
                if focused_window {
                    self.pending_fullscreen_focus_restore.remove(&workspace);
                }
                self.suppressed_focus = (!focused_window).then_some(focused).flatten();
            } else {
                self.suppressed_focus = focused;
            }
            return Ok(self.status(0, 0, &outputs));
        }
        let switch_started = Instant::now();
        let outgoing = self.workspaces.windows(previous);
        let incoming = self.workspaces.windows(workspace);
        log_event!(
            "switch: workspace {} -> {} (outgoing {}, incoming {})",
            previous,
            workspace,
            outgoing.len(),
            incoming.len()
        );
        let outgoing_focus = self
            .backend
            .focused_window()
            .filter(|focused| outgoing.contains(focused));
        if let Some(focused) = outgoing_focus.as_ref() {
            self.workspaces.remember_focus(previous, focused);
        }
        // Reveal incoming windows first: position writes do not raise, so they
        // arrive and get laid out beneath the outgoing windows. Hiding the
        // outgoing workspace afterwards exposes a fully formed workspace and
        // the bare desktop never shows. Failures before parking leave the
        // screen untouched.
        let mut restored = Vec::new();
        for window in &incoming {
            if cancelled.load(Ordering::Acquire) {
                log_event!("switch cancelled while revealing incoming windows");
                self.rollback_switch(Vec::new(), &restored, output, &outputs);
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
                outcome => {
                    log_event!(
                        "switch rollback: could not restore {:?} ({:?})",
                        window,
                        outcome
                    );
                    self.rollback_switch(Vec::new(), &restored, output, &outputs);
                    return Err(format!(
                        "Workspace {} could not restore all of its windows",
                        workspace
                    ));
                }
            }
        }
        self.wait_for_reveal(&incoming, cancelled);
        let layout = self.layout_workspace(workspace, output, cancelled);
        if layout.blocked {
            log_event!("switch rollback: workspace {} layout blocked", workspace);
            self.rollback_switch(Vec::new(), &incoming, output, &outputs);
            return Err(format!(
                "Workspace {} stayed inactive because its windows could not be laid out",
                workspace
            ));
        }
        let mut parked_outgoing = Vec::new();
        for window in &outgoing {
            if cancelled.load(Ordering::Acquire) {
                log_event!("switch cancelled while parking outgoing windows");
                self.rollback_switch(parked_outgoing, &incoming, output, &outputs);
                return Err("workspace switch cancelled".into());
            }
            match self.park_window(window, output, &outputs) {
                macos::ApplyOutcome::Applied => parked_outgoing.push(window.clone()),
                macos::ApplyOutcome::Gone => {
                    self.forget_window(previous, window);
                }
                outcome => {
                    log_event!(
                        "switch rollback: could not park {:?} ({:?})",
                        window,
                        outcome
                    );
                    self.rollback_switch(parked_outgoing, &incoming, output, &outputs);
                    return Err(format!(
                        "Workspace {} stayed active because a window could not be parked",
                        previous
                    ));
                }
            }
        }
        self.workspaces.switch(workspace);
        self.foreground_lanes.insert(display_id, selector);
        let incoming_focus = self
            .workspaces
            .preferred_focus(workspace)
            .filter(|window| incoming.contains(window))
            .or_else(|| incoming.first().cloned());
        if let Some(window) = incoming_focus {
            let focused_window = self.backend.focus_window(&window);
            if focused_window {
                self.pending_fullscreen_focus_restore.remove(&workspace);
            }
            self.suppressed_focus = (!focused_window).then_some(outgoing_focus).flatten();
        } else {
            self.suppressed_focus = outgoing_focus;
        }
        self.persist_workspace_state_if_changed()?;
        log_event!(
            "switch complete: workspace {} in {}ms",
            workspace,
            switch_started.elapsed().as_millis()
        );
        Ok(self.status(layout.applied, layout.failed, &outputs))
    }

    /// Bounded wait until every incoming window's restore is confirmed by the
    /// window server (pending restores keep their recovery frames). Slow
    /// applications apply moves asynchronously; hiding the outgoing workspace
    /// before they arrive would flash the bare desktop. Polling is
    /// confirmation-only; the restore write is re-issued only for windows
    /// that still have not arrived after [`REVEAL_REWRITE_INTERVAL`].
    fn wait_for_reveal(&mut self, incoming: &[crate::PhysicalWindowId], cancelled: &AtomicBool) {
        let started = Instant::now();
        let mut last_rewrite = started;
        loop {
            let pending: Vec<_> = incoming
                .iter()
                .filter(|window| self.parked.contains_key(window))
                .cloned()
                .collect();
            if pending.is_empty() {
                return;
            }
            if started.elapsed() >= REVEAL_SETTLE_LIMIT || cancelled.load(Ordering::Acquire) {
                log_event!(
                    "reveal timeout: {} window(s) still arriving; proceeding",
                    pending.len()
                );
                return;
            }
            thread::sleep(REVEAL_POLL_INTERVAL);
            let frames = self.backend.window_server_frames();
            let mut removed = Vec::new();
            for window in &pending {
                let Some(original) = self.parked.get(window).copied() else {
                    continue;
                };
                if frames.get(&window.window_id).is_some_and(|actual| {
                    actual.position_approximately_equals(original, RESTORE_CONFIRM_TOLERANCE)
                }) {
                    self.commit_unparked(window);
                    removed.push((window.clone(), original));
                }
            }
            if last_rewrite.elapsed() >= REVEAL_REWRITE_INTERVAL {
                last_rewrite = Instant::now();
                for window in &pending {
                    let Some(frame) = self.parked.get(window).copied() else {
                        continue;
                    };
                    if matches!(
                        self.write_restore_position(window, frame),
                        macos::ApplyOutcome::Gone
                    ) {
                        removed.push((window.clone(), frame));
                    }
                }
            }
            self.persist_removals_or_rollback(removed);
        }
    }

    /// Undo a partially applied switch: bring the outgoing windows back and
    /// hide any incoming windows that were already revealed.
    fn rollback_switch(
        &mut self,
        parked_outgoing: Vec<crate::PhysicalWindowId>,
        revealed: &[crate::PhysicalWindowId],
        output: crate::Rect,
        outputs: &macos::DisplayOutputs,
    ) {
        for window in parked_outgoing {
            self.restore_window(&window);
        }
        for window in revealed {
            if !self.parked.contains_key(window) {
                let _ = self.park_window(window, output, outputs);
            }
        }
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
            .workspace_of_any(&focused)
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
        if !self.workspaces.is_enabled(target) {
            return Err(format!("Workspace {target} is not configured"));
        }
        let target_selector = self.workspaces.assignment(target).clone();
        if !self.scope.manages_selector(&target_selector) {
            return Err(format!(
                "Workspace {} is outside the managed display scope",
                target
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
        self.clear_relocation_evidence(source, target, &window);
        if !target_is_active {
            self.suppressed_focus = Some(window.clone());
        }
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
        if self.workspaces.uses_fullscreen_layout(workspace) {
            return Err("Exit Fullscreen Layout before reordering the split layout".into());
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
        if self.workspaces.uses_fullscreen_layout(source) {
            return Ok(self.status(0, 0, &outputs));
        }
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
        if self.workspaces.uses_fullscreen_layout(workspace) {
            return Ok(self.status(0, 0, &outputs));
        }
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
        let Some((_, target)) = self.relative_target(&focused, direction, &outputs) else {
            return Ok(self.status(0, 0, &outputs));
        };
        if !self.backend.focus_window(&target) {
            return Err("Target window could not be focused".into());
        }
        self.pending_fullscreen_focus_restore.remove(&workspace);
        self.workspaces.remember_focus(workspace, &target);
        self.persist_workspace_state_if_changed()?;
        Ok(self.status(0, 0, &outputs))
    }

    fn relative_target(
        &self,
        focused: &crate::PhysicalWindowId,
        direction: Direction,
        outputs: &macos::DisplayOutputs,
    ) -> Option<(WorkspaceId, crate::PhysicalWindowId)> {
        let workspace = self.workspaces.workspace_of(focused)?;
        if self.workspaces.uses_fullscreen_layout(workspace) {
            let windows = self.workspaces.engine(workspace).bound_windows();
            let index = windows.iter().position(|window| window == focused)?;
            if windows.len() < 2 {
                return None;
            }
            let target = match direction {
                Direction::Left | Direction::Up => {
                    &windows[(index + windows.len() - 1) % windows.len()]
                }
                Direction::Down | Direction::Right => &windows[(index + 1) % windows.len()],
            };
            return Some((workspace, target.clone()));
        }
        self.spatial_target(focused, direction, outputs)
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
            if self.workspaces.uses_fullscreen_layout(resolved.workspace) {
                for placement in &mut placements {
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
                self.relative_target(focused, direction, outputs)
                    .map(|(_, window)| window)
            })
        };
        let targets = DebugTargets {
            left: target(Direction::Left),
            down: target(Direction::Down),
            up: target(Direction::Up),
            right: target(Direction::Right),
        };
        let visible = self.visible_workspaces(outputs);
        let floating = self
            .workspaces
            .floating_entries()
            .map(|(window, workspace)| DebugFloatingWindow {
                parked_original_frame: self.parked.get(window).copied(),
                window: window.clone(),
                workspace,
            })
            .collect();
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
                        fullscreen_layout: self
                            .workspaces
                            .uses_fullscreen_layout(resolved.workspace),
                        layout: self.preview_layout(
                            resolved.workspace,
                            crate::Rect {
                                x: 0,
                                y: 0,
                                width: 1500,
                                height: 1100,
                            },
                        ),
                    })
                })
                .collect(),
            tiles,
            floating,
            ignored: {
                let mut ignored: Vec<_> = self.quarantined.iter().cloned().collect();
                ignored.sort_by_key(|window| (window.app.pid, window.window_id));
                ignored
            },
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
        if self.workspaces.uses_fullscreen_layout(workspace) {
            return Err("Exit Fullscreen Layout before changing the split layout".into());
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
        if self.workspaces.uses_fullscreen_layout(workspace) {
            return Err("Exit Fullscreen Layout before resizing the split layout".into());
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
        if self.workspaces.uses_fullscreen_layout(workspace) {
            return Err("Exit Fullscreen Layout before balancing the split layout".into());
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

    fn toggle_fullscreen_layout(
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
        self.workspaces.remember_focus(workspace, &window);
        if !self.workspaces.toggle_fullscreen_layout(workspace, &window) {
            return Err("Focused window is not managed by Spaceman".into());
        }
        self.pending_fullscreen_focus_restore.remove(&workspace);
        let layout = self.layout_workspace(
            workspace,
            output_for(&self.workspaces, workspace, &outputs),
            cancelled,
        );
        self.persist_workspace_state_if_changed()?;
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
        self.apply_fullscreen_layout(workspace, &mut placements, output);
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
        // Deferred means a drag is in progress and layout retries next cycle;
        // it must not be treated as a failure that rolls back a switch.
        let blocked = results.len() != expected
            || results.iter().any(|result| {
                matches!(
                    result.outcome,
                    macos::ApplyOutcome::Gone | macos::ApplyOutcome::Unavailable
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
        server_frames: &HashMap<u32, crate::Rect>,
        cancelled: &AtomicBool,
        failed: &mut usize,
    ) {
        let visible_workspaces: HashSet<_> =
            visible.iter().map(|resolved| resolved.workspace).collect();
        let inactive = self.workspaces.inactive_windows_except(&visible_workspaces);
        let displays = outputs.frames().collect::<Vec<_>>();
        let union = outputs.union();
        let frontmost = self.backend.frontmost_application();
        let context = ParkContext {
            displays: &displays,
            union,
            frontmost: &frontmost,
            server_frames,
            visible,
        };
        let mut candidates = Vec::new();
        for (workspace, window) in inactive {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let output = output_for(&self.workspaces, workspace, outputs);
            if let Some(original) = self.parked.get(&window).copied() {
                self.repark_drifted_window(&window, original, output, &context);
                continue;
            }
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
            let frame = server_frames
                .get(&window.window_id)
                .copied()
                .unwrap_or(frame);
            if !union.contains_center(frame) {
                // Already off-screen by the app's own doing; leave it alone rather
                // than recording an off-screen frame as recovery data.
                continue;
            }
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
            log_event!("failed to persist parked windows: {error}");
            return;
        }

        let mut restored = Vec::new();
        for (workspace, window, frame, target) in candidates {
            log_event!(
                "park inactive: {:?} (workspace {}) from {:?}",
                window,
                workspace,
                frame
            );
            match self.backend.set_position(&window, target) {
                macos::ApplyOutcome::Applied => {
                    self.recently_parked.insert(window.clone(), Instant::now());
                }
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
                    ) && !self.parked.contains_key(&window)
                    {
                        restored.push((window, frame));
                    }
                }
            }
        }
        self.persist_removals_or_rollback(restored);
    }

    /// A parked window observed on-screen in the background re-asserted its
    /// saved bounds (Electron does this on screen-parameter changes). Push it
    /// back to its parked spot; the recovery frame is already recorded. A
    /// frontmost app's window is left for heal to re-home instead.
    fn repark_drifted_window(
        &mut self,
        window: &crate::PhysicalWindowId,
        original: crate::Rect,
        output: crate::Rect,
        context: &ParkContext<'_>,
    ) {
        let Some(server) = context.server_frames.get(&window.window_id).copied() else {
            return;
        };
        if !context.union.contains_center(server) {
            // Only a window that STAYED parked past the settle grace ends a
            // fight; right after a re-park write the app may still undo it.
            if self
                .recently_parked
                .get(window)
                .is_none_or(|parked_at| parked_at.elapsed() >= PARK_SETTLE_GRACE)
            {
                self.repark_attempts.remove(window);
            }
            return;
        }
        if context.frontmost.as_ref() == Some(&window.app) {
            return;
        }
        if self
            .recently_parked
            .get(window)
            .is_some_and(|parked_at| parked_at.elapsed() < PARK_SETTLE_GRACE)
        {
            return;
        }
        let attempts = self.repark_attempts.entry(window.clone()).or_insert(0);
        *attempts += 1;
        if *attempts > REPARK_ATTEMPT_LIMIT {
            // The app undoes every park. Fighting on would flicker forever;
            // honor the reappearance instead (re-home, then quarantine if it
            // keeps happening).
            log_event!(
                "re-park escalation: {:?} will not stay parked after {} attempts",
                window,
                REPARK_ATTEMPT_LIMIT
            );
            if let Some(target) = context
                .visible
                .iter()
                .find(|resolved| resolved.output.contains_center(server))
                .map(|resolved| resolved.workspace)
            {
                self.honor_reappearance(window, server, original, target);
            }
            return;
        }
        let target = crate::geometry::parked_frame(output, context.displays, original);
        log_event!(
            "re-park: {:?} drifted back on-screen to {:?}",
            window,
            server
        );
        if matches!(
            self.backend.set_position(window, target),
            macos::ApplyOutcome::Applied
        ) {
            self.recently_parked.insert(window.clone(), Instant::now());
        }
    }

    fn restore_all(&mut self) -> usize {
        let parked: Vec<_> = self.parked.keys().cloned().collect();
        let mut restored = Vec::new();
        for window in parked {
            let frame = self.parked[&window];
            match self.restore_window_uncommitted(&window) {
                macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                    if !self.parked.contains_key(&window) =>
                {
                    restored.push((window, frame));
                }
                _ => {}
            }
        }
        self.persist_removals_or_rollback(restored);
        // Persistence failure rolls recovery entries back into this map. The
        // caller may only exit when both native restoration and durable state
        // removal have completed.
        self.parked.len()
    }

    fn forget_window(&mut self, workspace: WorkspaceId, window: &crate::PhysicalWindowId) {
        self.workspaces.forget_window(workspace, window);
        self.workspaces.forget_floating(window);
        self.constrained[workspace.index()].remove(window);
        self.recently_parked.remove(window);
        self.recent_heals.remove(window);
        self.repark_attempts.remove(window);
    }

    fn apply_fullscreen_layout(
        &mut self,
        workspace: WorkspaceId,
        placements: &mut [crate::Placement],
        output: crate::Rect,
    ) {
        if !self.workspaces.uses_fullscreen_layout(workspace) {
            return;
        }
        for placement in placements {
            placement.frame = output;
            self.workspaces
                .engine_mut(workspace)
                .record_placement_target(placement.slot, output);
        }
    }

    fn mark_parked(&mut self, window: crate::PhysicalWindowId, frame: crate::Rect) -> bool {
        if self.parked.contains_key(&window) {
            return true;
        }
        self.parked.insert(window.clone(), frame);
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            self.parked.remove(&window);
            log_event!("failed to persist parked window: {error}");
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
        // Prefer the window server's frame: Accessibility reads can be stale
        // right after a move, which would misclassify the window and record a
        // wrong recovery frame.
        let frame = self.backend.window_server_frame(window).unwrap_or(frame);
        if !outputs.union().contains_center(frame) && !self.parked.contains_key(window) {
            // The app keeps this window off-screen itself. There is nothing to hide
            // and no on-screen frame worth recording for recovery.
            log_event!("park skip: {:?} already off-screen at {:?}", window, frame);
            return macos::ApplyOutcome::Applied;
        }
        if !self.mark_parked(window.clone(), frame) {
            return macos::ApplyOutcome::Unavailable;
        }
        let displays = outputs.frames().collect::<Vec<_>>();
        let parked = crate::geometry::parked_frame(output, &displays, frame);
        let outcome = self.backend.set_position(window, parked);
        log_event!("park: {:?} from {:?} ({:?})", window, frame, outcome);
        if matches!(outcome, macos::ApplyOutcome::Applied) {
            self.recently_parked.insert(window.clone(), Instant::now());
        } else {
            let _ = self.restore_window(window);
        }
        outcome
    }

    fn restore_window(&mut self, window: &crate::PhysicalWindowId) -> macos::ApplyOutcome {
        let frame = self.parked.get(window).copied();
        let outcome = self.restore_window_uncommitted(window);
        if !self.parked.contains_key(window)
            && let Some(frame) = frame
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
        let outcome = self.write_restore_position(window, frame);
        match outcome {
            macos::ApplyOutcome::Gone => {}
            macos::ApplyOutcome::Applied => {
                // Accessibility read-back is optimistic for apps that apply
                // moves asynchronously. Keep the recovery frame until the
                // window server confirms arrival; cycles retry until it does.
                // A window the window server cannot see is not confirmed.
                let confirmed = self
                    .backend
                    .window_server_frame(window)
                    .is_some_and(|actual| {
                        actual.position_approximately_equals(frame, RESTORE_CONFIRM_TOLERANCE)
                    });
                if confirmed {
                    self.commit_unparked(window);
                } else {
                    log_event!(
                        "restore pending: {:?} not yet at {:?} per the window server",
                        window,
                        frame
                    );
                }
            }
            _ => {}
        }
        outcome
    }

    /// Apply a restore write and update transient parking state. Confirmation
    /// and durable removal remain the caller's responsibility so bulk paths
    /// can share one window-server snapshot and one persistence transaction.
    fn write_restore_position(
        &mut self,
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
    ) -> macos::ApplyOutcome {
        let outcome = self.backend.set_position(window, frame);
        log_event!("restore: {:?} to {:?} ({:?})", window, frame, outcome);
        match outcome {
            macos::ApplyOutcome::Applied => {
                // Also throttles the next retry and heal consideration.
                self.recently_parked.insert(window.clone(), Instant::now());
            }
            macos::ApplyOutcome::Gone => self.commit_unparked(window),
            _ => {}
        }
        outcome
    }

    /// The window is confirmed no longer parked; retire all parking state.
    fn commit_unparked(&mut self, window: &crate::PhysicalWindowId) {
        self.parked.remove(window);
        self.recently_parked.remove(window);
        self.repark_attempts.remove(window);
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
            log_event!("failed to persist restored windows: {error}");
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

    fn preview_layout(&self, workspace: WorkspaceId, output: crate::Rect) -> Vec<crate::Rect> {
        let mut layout = self.workspaces.engine(workspace).preview(output);
        if self.workspaces.uses_fullscreen_layout(workspace) {
            layout.fill(output);
        }
        layout
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
                        workspace.to_string(),
                        self.preview_layout(
                            *workspace,
                            crate::Rect {
                                x: 0,
                                y: 0,
                                width: 1500,
                                height: 1100,
                            },
                        ),
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
            log_event!("ignoring invalid Spaceman config during recovery: {error}");
            return (Config::default(), None);
        }
    };
    let workspace_state = workspace_state.unwrap_or_else(|error| {
        log_event!("ignoring invalid Spaceman workspace state during recovery: {error}");
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

#[cfg(test)]
mod tests {
    use crate::{BindingState, DisplayRole, Slot, SlotId};

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum BackendCall {
        Persist,
        SetPosition(crate::PhysicalWindowId, crate::Rect),
        Apply(crate::PhysicalWindowId, crate::Rect),
        Raise(crate::PhysicalWindowId),
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
        server_frames: HashMap<crate::PhysicalWindowId, crate::Rect>,
        /// Simulates apps whose real window movement lags position writes.
        frozen_server_frames: bool,
        focused: Option<crate::PhysicalWindowId>,
        frontmost: Option<AppInstanceId>,
        observation: Option<crate::NativeObservation>,
        persisted: HashMap<crate::PhysicalWindowId, crate::Rect>,
        calls: Vec<BackendCall>,
        fail_persist: bool,
        frame_outcome: Option<macos::WindowAccessError>,
        managed_outcome: Option<macos::ApplyOutcome>,
        focus_outcome: Option<bool>,
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

        fn window_server_frame(&mut self, window: &crate::PhysicalWindowId) -> Option<crate::Rect> {
            self.server_frames
                .get(window)
                .or_else(|| self.frames.get(window))
                .copied()
        }

        fn window_server_frames(&mut self) -> HashMap<u32, crate::Rect> {
            let mut frames: HashMap<u32, crate::Rect> = self
                .frames
                .iter()
                .map(|(window, frame)| (window.window_id, *frame))
                .collect();
            for (window, frame) in &self.server_frames {
                frames.insert(window.window_id, *frame);
            }
            frames
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
            if !self.frozen_server_frames
                && let Some(server) = self.server_frames.get_mut(window)
            {
                server.x = target.x;
                server.y = target.y;
            }
            macos::ApplyOutcome::Applied
        }

        fn focused_window(&mut self) -> Option<crate::PhysicalWindowId> {
            self.focused.clone()
        }

        fn frontmost_application(&mut self) -> Option<AppInstanceId> {
            self.frontmost.clone()
        }

        fn raise_window(&mut self, window: &crate::PhysicalWindowId) -> bool {
            self.calls.push(BackendCall::Raise(window.clone()));
            true
        }

        fn focus_window(&mut self, window: &crate::PhysicalWindowId) -> bool {
            self.calls.push(BackendCall::Focus(window.clone()));
            if self.focus_outcome == Some(false) {
                return false;
            }
            self.focused = Some(window.clone());
            self.frontmost = Some(window.app.clone());
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
                        if !self.frozen_server_frames && self.server_frames.contains_key(&window) {
                            self.server_frames.insert(window.clone(), placement.frame);
                        }
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
                        resizable: true,
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
    fn pending_actions_keep_only_the_latest_intent_without_cancelling_active_work() {
        let mut pending = None;
        let cancelled = AtomicBool::new(false);
        let second = WorkspaceId::new(2).unwrap();
        let third = WorkspaceId::new(3).unwrap();
        queue_latest(&mut pending, WorkerAction::SwitchWorkspace(second));
        queue_latest(&mut pending, WorkerAction::SwitchWorkspace(third));

        assert_eq!(pending.take(), Some(WorkerAction::SwitchWorkspace(third)));
        assert_eq!(pending.take(), None);
        assert!(!cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn every_exit_intent_restores_before_exiting() {
        for intent in [
            ExitIntent::Quit,
            ExitIntent::Restart,
            ExitIntent::Update,
            ExitIntent::Terminate,
        ] {
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
                resizable: true,
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
                .filter_map(|resolved| resolved.workspace.number())
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
                .filter_map(|resolved| resolved.workspace.number())
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
        let server_frames = runtime.backend.window_server_frames();
        runtime.park_inactive(
            &outputs,
            &visible,
            &server_frames,
            &AtomicBool::new(false),
            &mut failed,
        );

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
        let server_frames = runtime.backend.window_server_frames();
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
    fn parked_window_observed_back_on_screen_re_homes_and_drops_recovery_frame() {
        let tiled = window(10);
        let overlay = window(11);
        let mut observation = observation_for_windows(
            "com.example.Test",
            &tiled,
            &[(tiled.clone(), frame(50)), (overlay.clone(), frame(60))],
        );
        if let crate::AppObservation::Complete { windows, .. } = &mut observation.applications[0] {
            windows
                .iter_mut()
                .find(|observed| observed.id == overlay)
                .unwrap()
                .resizable = false;
        }
        let mut backend = FakeBackend {
            observation: Some(observation),
            frontmost: Some(overlay.app.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(overlay.clone(), frame(60));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        runtime.workspaces.own_floating(overlay.clone(), hidden);
        runtime.parked.insert(overlay.clone(), frame(70));

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert!(!runtime.parked.contains_key(&overlay));
        assert!(!runtime.backend.persisted.contains_key(&overlay));
        assert_eq!(
            runtime.workspaces.floating_workspace_of(&overlay),
            Some(WorkspaceId::ONE)
        );
        assert!(
            outputs()
                .union()
                .contains_center(runtime.backend.frames[&overlay])
        );
    }

    #[test]
    fn parked_tiled_window_observed_back_on_screen_re_homes_to_the_visible_workspace() {
        let existing = window(10);
        let returned = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation_for_windows(
                "com.example.Test",
                &existing,
                &[
                    (existing.clone(), frame(50)),
                    (returned.clone(), frame(400)),
                ],
            )),
            frontmost: Some(window(11).app),
            ..FakeBackend::default()
        };
        backend.frames.insert(existing.clone(), frame(50));
        backend.frames.insert(returned.clone(), frame(400));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &existing);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &returned);
        runtime.workspaces.refresh_ownership(hidden);
        runtime.parked.insert(returned.clone(), frame(70));

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert!(!runtime.parked.contains_key(&returned));
        assert_eq!(
            runtime.workspaces.workspace_of(&returned),
            Some(WorkspaceId::ONE)
        );
        assert!(
            outputs()
                .union()
                .contains_center(runtime.backend.frames[&returned])
        );
    }

    #[test]
    fn stale_accessibility_observation_does_not_heal_a_genuinely_parked_window() {
        let tiled = window(10);
        let parked = window(11);
        let offscreen = crate::Rect {
            x: -500,
            y: 1400,
            width: 471,
            height: 1002,
        };
        let mut backend = FakeBackend {
            observation: Some(observation_for_windows(
                "com.example.Test",
                &tiled,
                &[(tiled.clone(), frame(50)), (parked.clone(), frame(400))],
            )),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        // Accessibility still reports the pre-park frame (stale)...
        backend.frames.insert(parked.clone(), frame(400));
        // ...but the window server knows it is parked off-screen.
        backend.server_frames.insert(parked.clone(), offscreen);
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &parked);
        runtime.workspaces.refresh_ownership(hidden);
        runtime.parked.insert(parked.clone(), frame(400));

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert_eq!(runtime.parked.get(&parked), Some(&frame(400)));
        assert_eq!(runtime.workspaces.workspace_of(&parked), Some(hidden));
    }

    #[test]
    fn stale_accessibility_frame_does_not_skip_parking_an_on_screen_window() {
        let window = window(10);
        let stale_offscreen = crate::Rect {
            x: -2559,
            y: 1408,
            width: 2560,
            height: 1409,
        };
        let mut backend = FakeBackend::default();
        // Accessibility reports a stale parked position...
        backend.frames.insert(window.clone(), stale_offscreen);
        // ...but the window server says the window is really on-screen.
        backend.server_frames.insert(window.clone(), frame(50));
        let mut runtime = runtime(backend);

        assert_eq!(
            runtime.park_window(&window, outputs().main().frame, &outputs()),
            macos::ApplyOutcome::Applied
        );
        // The recovery frame is the window server's truth, not the stale read.
        assert_eq!(runtime.parked.get(&window), Some(&frame(50)));
        assert!(
            runtime
                .backend
                .calls
                .iter()
                .any(|call| matches!(call, BackendCall::SetPosition(_, _)))
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
        let config = Config::parse(
            "version = 1\n[[rules]]\nbundle_id = 'com.example.Test'\naction = 'ignore'",
        )
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
    fn second_confirmed_self_un_park_quarantines_the_window() {
        let tiled = window(10);
        let fighter = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation_for_windows(
                "com.example.Test",
                &tiled,
                &[(tiled.clone(), frame(50)), (fighter.clone(), frame(400))],
            )),
            frontmost: Some(window(11).app),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(fighter.clone(), frame(400));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &fighter);
        runtime.workspaces.refresh_ownership(hidden);

        // First confirmed self-un-park: healed and re-homed to workspace 1.
        runtime.parked.insert(fighter.clone(), frame(400));
        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            runtime.workspaces.workspace_of(&fighter),
            Some(WorkspaceId::ONE)
        );

        // The user switches away (window back on a hidden workspace, parked),
        // and the window un-parks itself again shortly after: quarantined.
        runtime.workspaces.move_window(&fighter, hidden);
        runtime.parked.insert(fighter.clone(), frame(400));
        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert!(runtime.quarantined.contains(&fighter));
        assert_eq!(runtime.workspaces.workspace_of_any(&fighter), None);
        assert!(!runtime.parked.contains_key(&fighter));

        // Switching away leaves the quarantined window exactly where it is.
        let before = runtime.backend.frames[&fighter];
        runtime
            .switch(hidden, outputs(), &AtomicBool::new(false))
            .unwrap();
        assert!(!runtime.parked.contains_key(&fighter));
        assert_eq!(runtime.backend.frames[&fighter], before);
    }

    #[test]
    fn background_reappearance_is_re_parked_instead_of_re_homed() {
        let tiled = window(10);
        let drifter = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation_for_windows(
                "com.example.Test",
                &tiled,
                &[(tiled.clone(), frame(50)), (drifter.clone(), frame(400))],
            )),
            // A different application is frontmost: the reappearance is a
            // background bounds re-assertion, not the user summoning it.
            frontmost: Some(AppInstanceId {
                pid: 999,
                launched_at: 999,
            }),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(drifter.clone(), frame(400));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &drifter);
        runtime.workspaces.refresh_ownership(hidden);
        runtime.parked.insert(drifter.clone(), frame(400));

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        // Ownership and recovery metadata are untouched; the window was simply
        // pushed back off-screen.
        assert_eq!(runtime.parked.get(&drifter), Some(&frame(400)));
        assert_eq!(runtime.workspaces.workspace_of(&drifter), Some(hidden));
        assert!(
            !outputs()
                .union()
                .contains_center(runtime.backend.frames[&drifter])
        );
    }

    #[test]
    fn undone_re_parks_escalate_to_honoring_the_reappearance() {
        let tiled = window(10);
        let fighter = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation(&tiled, frame(50))),
            frontmost: Some(AppInstanceId {
                pid: 999,
                launched_at: 999,
            }),
            // The app undoes every position write.
            frozen_server_frames: true,
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(fighter.clone(), frame(400));
        backend.server_frames.insert(fighter.clone(), frame(400));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &fighter);
        runtime.workspaces.refresh_ownership(hidden);
        runtime.parked.insert(fighter.clone(), frame(400));

        for _ in 0..=REPARK_ATTEMPT_LIMIT {
            let server_frames = runtime.backend.window_server_frames();
            let visible = runtime.visible_workspaces(&outputs());
            runtime.park_inactive(
                &outputs(),
                &visible,
                &server_frames,
                &AtomicBool::new(false),
                &mut 0,
            );
            // The settle grace would otherwise defer the next attempt.
            runtime.recently_parked.remove(&fighter);
        }

        // The fight ended by honoring the window: recovery frame dropped and
        // the window re-homed to the visible workspace it appears on.
        assert!(!runtime.parked.contains_key(&fighter));
        assert_eq!(
            runtime.workspaces.workspace_of(&fighter),
            Some(WorkspaceId::ONE)
        );
    }

    #[test]
    fn heal_waits_for_a_fresh_park_to_settle_before_trusting_reappearance() {
        let tiled = window(10);
        let parked = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation_for_windows(
                "com.example.Test",
                &tiled,
                &[(tiled.clone(), frame(50)), (parked.clone(), frame(400))],
            )),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(parked.clone(), frame(400));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let hidden = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, hidden, &parked);
        runtime.workspaces.refresh_ownership(hidden);
        // The park write just happened; the app is still applying it, so both
        // Accessibility and the window server still report the old frame.
        runtime.parked.insert(parked.clone(), frame(400));
        runtime
            .recently_parked
            .insert(parked.clone(), Instant::now());

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();

        assert_eq!(runtime.parked.get(&parked), Some(&frame(400)));
        assert_eq!(runtime.workspaces.workspace_of(&parked), Some(hidden));
    }

    #[test]
    fn restore_keeps_recovery_frame_until_the_window_server_confirms_arrival() {
        let window = window(10);
        let parked_position = crate::Rect {
            x: -2559,
            y: 1408,
            width: 2560,
            height: 1409,
        };
        let mut backend = FakeBackend {
            frozen_server_frames: true,
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), parked_position);
        backend
            .server_frames
            .insert(window.clone(), parked_position);
        let mut runtime = runtime(backend);
        runtime.parked.insert(window.clone(), frame(50));

        // Accessibility read-back reports success, but the window server still
        // shows the window parked: the recovery frame must survive.
        assert_eq!(
            runtime.restore_window(&window),
            macos::ApplyOutcome::Applied
        );
        assert_eq!(runtime.parked.get(&window), Some(&frame(50)));

        // Once the real move lands, the next restore confirms and commits.
        runtime.backend.frozen_server_frames = false;
        assert_eq!(
            runtime.restore_window(&window),
            macos::ApplyOutcome::Applied
        );
        assert!(!runtime.parked.contains_key(&window));
        assert!(!runtime.backend.persisted.contains_key(&window));
    }

    #[test]
    fn mouse_deferred_layout_does_not_roll_back_a_switch() {
        let outgoing = window(10);
        let incoming = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation(&outgoing, frame(50))),
            managed_outcome: Some(macos::ApplyOutcome::Deferred),
            ..FakeBackend::default()
        };
        backend.frames.insert(outgoing.clone(), frame(50));
        backend.frames.insert(incoming.clone(), frame(60));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &outgoing);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        let target = WorkspaceId::new(2).unwrap();
        bind(&mut runtime, target, &incoming);
        runtime.workspaces.refresh_ownership(target);

        let result = runtime.switch(target, outputs(), &AtomicBool::new(false));

        assert!(result.is_ok());
        // The outgoing window stays parked; layout converges on a later cycle.
        assert!(runtime.parked.contains_key(&outgoing));
    }

    #[test]
    fn switching_leaves_self_hidden_windows_alone_instead_of_failing() {
        let tiled = window(10);
        let stashed = window(11);
        let mut backend = FakeBackend {
            observation: Some(observation(&tiled, frame(50))),
            ..FakeBackend::default()
        };
        backend.frames.insert(tiled.clone(), frame(50));
        backend.frames.insert(stashed.clone(), frame(-4000));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &tiled);
        runtime.workspaces.refresh_ownership(WorkspaceId::ONE);
        runtime
            .workspaces
            .own_floating(stashed.clone(), WorkspaceId::ONE);

        runtime
            .switch(
                WorkspaceId::new(2).unwrap(),
                outputs(),
                &AtomicBool::new(false),
            )
            .unwrap();

        assert!(runtime.parked.contains_key(&tiled));
        assert!(!runtime.parked.contains_key(&stashed));
        assert_eq!(runtime.backend.frames[&stashed], frame(-4000));
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
    fn floating_rule_owns_window_without_adding_it_to_layout() {
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
    fn non_resizable_window_parks_and_restores_with_its_workspace() {
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

        runtime.cycle(outputs(), &AtomicBool::new(false)).unwrap();
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

        let other = WorkspaceId::new(2).unwrap();
        runtime
            .switch(other, outputs(), &AtomicBool::new(false))
            .unwrap();
        assert!(runtime.parked.contains_key(&window));
        assert!(
            !outputs()
                .union()
                .contains_center(runtime.backend.frames[&window])
        );

        runtime
            .switch(WorkspaceId::ONE, outputs(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(runtime.backend.frames[&window], normal);
        assert!(!runtime.parked.contains_key(&window));
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
    fn restore_all_repositions_windows_at_their_original_frames() {
        let window = window(10);
        let normal = frame(50);
        let mut backend = FakeBackend::default();
        backend.frames.insert(window.clone(), normal);
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::new(2).unwrap(), &window);
        let outputs = outputs();
        let visible = runtime.visible_workspaces(&outputs);
        let server_frames = runtime.backend.window_server_frames();
        runtime.park_inactive(
            &outputs,
            &visible,
            &server_frames,
            &AtomicBool::new(false),
            &mut 0,
        );

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
        assert_eq!(runtime.restore_all(), 1);
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

        runtime
            .toggle_fullscreen_layout(outputs.clone(), &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.backend.frames[&first], outputs.main().frame);
        assert_eq!(runtime.backend.frames[&second], outputs.main().frame);
        assert!(runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
        assert_eq!(
            runtime.workspaces.engine(WorkspaceId::ONE).bound_windows(),
            vec![first.clone(), second.clone()]
        );

        runtime
            .toggle_fullscreen_layout(outputs, &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.backend.frames[&first].width, 500);
        assert_eq!(runtime.backend.frames[&second].x, 500);
        assert!(!runtime.workspaces.uses_fullscreen_layout(WorkspaceId::ONE));
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
    fn fullscreen_layout_focus_wraps_through_tile_order() {
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
        runtime
            .toggle_fullscreen_layout(outputs.clone(), &cancelled)
            .unwrap();

        runtime
            .focus_relative(Direction::Right, outputs.clone(), &cancelled)
            .unwrap();
        assert_eq!(runtime.backend.focused, Some(third.clone()));
        runtime
            .focus_relative(Direction::Right, outputs.clone(), &cancelled)
            .unwrap();
        assert_eq!(runtime.backend.focused, Some(first.clone()));
        runtime
            .focus_relative(Direction::Up, outputs, &cancelled)
            .unwrap();
        assert_eq!(runtime.backend.focused, Some(third));
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
        assert!(persisted.toggle_fullscreen_layout(WorkspaceId::ONE, &focused));
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
        assert!(persisted.toggle_fullscreen_layout(WorkspaceId::ONE, &persisted_focus));
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
    fn fullscreen_layout_rejects_hidden_split_changes() {
        let window = window(10);
        let mut backend = FakeBackend {
            focused: Some(window.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(window.clone(), frame(50));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &window);
        let outputs = outputs();
        let cancelled = AtomicBool::new(false);
        runtime
            .toggle_fullscreen_layout(outputs.clone(), &cancelled)
            .unwrap();

        let Err(error) = runtime.toggle_layout(outputs, &cancelled) else {
            panic!("split layout changed while Fullscreen Layout was active");
        };

        assert_eq!(
            error,
            "Exit Fullscreen Layout before changing the split layout"
        );
        assert_eq!(
            runtime.workspaces.engine(WorkspaceId::ONE).root_axis(),
            crate::Axis::Horizontal
        );
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
    fn switching_reveals_and_lays_out_incoming_beneath_before_hiding_outgoing() {
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
        // The incoming workspace is fully formed beneath the outgoing windows
        // before anything is hidden, so the desktop never shows.
        assert!(reveal < layout && layout < hide);
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
    fn switching_away_and_back_restores_last_focused_window() {
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

        runtime.switch(target, outputs.clone(), &cancelled).unwrap();
        runtime
            .switch(WorkspaceId::ONE, outputs.clone(), &cancelled)
            .unwrap();

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
    fn selecting_an_active_workspace_focuses_its_window() {
        let target = window(10);
        let other_display = window(20);
        let mut backend = FakeBackend {
            focused: Some(other_display.clone()),
            ..FakeBackend::default()
        };
        backend.frames.insert(target.clone(), frame(50));
        backend.frames.insert(other_display.clone(), frame(1050));
        let mut runtime = runtime(backend);
        bind(&mut runtime, WorkspaceId::ONE, &target);
        bind(&mut runtime, WorkspaceId::SIX, &other_display);

        runtime
            .switch(WorkspaceId::ONE, outputs(), &AtomicBool::new(false))
            .unwrap();

        assert_eq!(runtime.backend.focused, Some(target.clone()));
        assert!(matches!(
            runtime.backend.calls.last(),
            Some(BackendCall::Focus(window)) if window == &target
        ));
    }

    #[test]
    fn selecting_an_active_workspace_preserves_its_current_focus() {
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

    #[test]
    fn switching_to_an_unconfigured_named_workspace_is_rejected() {
        let mut runtime = runtime(FakeBackend::default());

        let Err(error) = runtime.switch(
            WorkspaceId::named('Z').unwrap(),
            outputs(),
            &AtomicBool::new(false),
        ) else {
            panic!("unconfigured workspace unexpectedly switched");
        };

        assert_eq!(error, "Workspace Z is not configured");
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
        let outgoing = window(10);
        let incoming = window(20);
        let outgoing_normal = frame(50);
        let incoming_normal = frame(100);
        let outputs = outputs();
        let displays = outputs.frames().collect::<Vec<_>>();
        let incoming_parked =
            crate::geometry::parked_frame(outputs.main().frame, &displays, incoming_normal);
        let mut backend = FakeBackend {
            managed_outcome: Some(macos::ApplyOutcome::Unavailable),
            ..FakeBackend::default()
        };
        backend.frames.insert(outgoing.clone(), outgoing_normal);
        backend.frames.insert(incoming.clone(), incoming_parked);
        let mut parked = HashMap::new();
        parked.insert(incoming.clone(), incoming_normal);
        let mut runtime = WorkspaceRuntime::with_backend(ManagementScope::Both, backend, parked);
        bind(&mut runtime, WorkspaceId::ONE, &outgoing);
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
        assert_eq!(runtime.backend.frames[&outgoing], outgoing_normal);
        assert!(!runtime.parked.contains_key(&outgoing));
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
    fn already_offscreen_window_is_not_recorded_as_a_recovery_frame() {
        let window = window(10);
        let offscreen = crate::Rect {
            x: -500,
            y: 100,
            width: 400,
            height: 300,
        };
        let mut backend = FakeBackend::default();
        backend.frames.insert(window.clone(), offscreen);
        let mut runtime = runtime(backend);

        assert_eq!(
            runtime.park_window(&window, outputs().main().frame, &outputs()),
            macos::ApplyOutcome::Applied
        );
        assert!(!runtime.parked.contains_key(&window));
        assert!(runtime.backend.persisted.is_empty());
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
