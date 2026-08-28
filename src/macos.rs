use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::c_void,
    ptr,
    ptr::NonNull,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{Sender, channel},
    },
    thread::{self, JoinHandle},
};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSScreen, NSWorkspace};
use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXObserver, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFNumberType, CFRetained, CFRunLoop,
    CFRunLoopSource, CFString, CFType, CFUUID, CGPoint, CGSize, ConcreteType,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGMouseButton, CGWindowID,
    CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowBounds, kCGWindowLayer,
    kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{NSNumber, NSString};

use crate::{
    AppInstanceId, AppObservation, DisplaySelector, NativeObservation, ObservedApplication,
    ObservedWindow, PhysicalWindowId, Placement, Rect, Settings,
};

#[derive(Debug, serde::Serialize)]
pub struct ApplyResult {
    pub window: PhysicalWindowId,
    pub target: Rect,
    pub actual: Option<Rect>,
    pub outcome: ApplyOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyOutcome {
    Applied,
    Constrained,
    Gone,
    Unavailable,
    Deferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowAccessError {
    Gone,
    Unavailable,
}

#[derive(Debug)]
pub enum SnapshotError {
    AccessibilityPermissionRequired,
    MainThreadRequired,
    MainScreenUnavailable,
    WindowListUnavailable,
    TargetWindowUnavailable,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessibilityPermissionRequired => {
                formatter.write_str("Accessibility permission is required for live snapshots")
            }
            Self::MainThreadRequired => formatter.write_str("snapshot must run on the main thread"),
            Self::MainScreenUnavailable => formatter.write_str("the main display is unavailable"),
            Self::WindowListUnavailable => {
                formatter.write_str("the on-screen window list is unavailable")
            }
            Self::TargetWindowUnavailable => {
                formatter.write_str("the target window is unavailable on attached displays")
            }
        }
    }
}

type WindowIdFunction = unsafe extern "C" fn(&AXUIElement, *mut CGWindowID) -> AXError;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const CFUUID;
}

thread_local! {
    static MANAGED_WINDOWS: RefCell<HashMap<PhysicalWindowId, CFRetained<AXUIElement>>> =
        RefCell::new(HashMap::new());
}

pub fn snapshot(cycle: u64) -> Result<(NativeObservation, Settings), SnapshotError> {
    if !accessibility_trusted() {
        return Err(SnapshotError::AccessibilityPermissionRequired);
    }
    snapshot_in_output(cycle, main_output()?)
}

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn main_output() -> Result<Rect, SnapshotError> {
    Ok(outputs()?.main().frame)
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct DisplayOutput {
    pub id: String,
    pub name: String,
    pub frame: Rect,
    pub is_main: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputs {
    displays: Vec<DisplayOutput>,
}

impl DisplayOutputs {
    pub fn new(displays: Vec<DisplayOutput>) -> Result<Self, SnapshotError> {
        if displays.iter().any(|display| display.is_main) {
            Ok(Self { displays })
        } else {
            Err(SnapshotError::MainScreenUnavailable)
        }
    }

    #[must_use]
    pub fn displays(&self) -> &[DisplayOutput] {
        &self.displays
    }

    #[must_use]
    pub fn main(&self) -> &DisplayOutput {
        self.displays
            .iter()
            .find(|display| display.is_main)
            .expect("display topology always contains a main display")
    }

    #[must_use]
    pub fn secondary(&self) -> Option<&DisplayOutput> {
        self.displays.iter().find(|display| !display.is_main)
    }

    #[must_use]
    pub fn resolve_exact(&self, selector: &DisplaySelector) -> Option<&DisplayOutput> {
        match selector {
            DisplaySelector::Main => Some(self.main()),
            DisplaySelector::Secondary => self.secondary(),
            DisplaySelector::Preferred(preferred) => self
                .displays
                .iter()
                .find(|display| display.id == *preferred || display.name == *preferred),
        }
    }

    #[must_use]
    pub fn resolve(&self, selector: &DisplaySelector) -> &DisplayOutput {
        self.resolve_exact(selector).unwrap_or_else(|| self.main())
    }

    pub fn frames(&self) -> impl Iterator<Item = Rect> + '_ {
        self.displays.iter().map(|display| display.frame)
    }

    #[must_use]
    pub fn union(&self) -> Rect {
        self.frames()
            .reduce(union_rect)
            .expect("display topology is non-empty")
    }
}

pub struct WindowEventMonitor {
    invalidation: Arc<EventInvalidation>,
    applications: Sender<Vec<AppInstanceId>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct EventInvalidation {
    dirty: AtomicBool,
    window_created: AtomicBool,
}

impl WindowEventMonitor {
    pub fn new() -> Self {
        let invalidation = Arc::new(EventInvalidation {
            dirty: AtomicBool::new(true),
            window_created: AtomicBool::new(false),
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let (applications, receiver) = channel::<Vec<AppInstanceId>>();
        let thread_invalidation = Arc::clone(&invalidation);
        let thread_stopped = Arc::clone(&stopped);
        let thread = thread::Builder::new()
            .name("spaceman-ax-events".into())
            .spawn(move || {
                let run_loop = CFRunLoop::current().expect("AX event thread has no run loop");
                let mut registrations = HashMap::<AppInstanceId, EventRegistration>::new();
                let mut known_applications = HashSet::new();
                while !thread_stopped.load(Ordering::Acquire) {
                    let mut latest = None;
                    while let Ok(pids) = receiver.try_recv() {
                        latest = Some(pids);
                    }
                    if let Some(pids) = latest {
                        let desired: HashSet<_> = pids.into_iter().collect();
                        if desired != known_applications {
                            thread_invalidation.dirty.store(true, Ordering::Release);
                            known_applications.clone_from(&desired);
                        }
                        let removed: Vec<_> = registrations
                            .keys()
                            .filter(|app| !desired.contains(app))
                            .cloned()
                            .collect();
                        for app in removed {
                            if let Some(registration) = registrations.remove(&app) {
                                run_loop.remove_source(Some(&registration.source), unsafe {
                                    kCFRunLoopDefaultMode
                                });
                            }
                        }
                        for app in desired {
                            if !registrations.contains_key(&app)
                                && let Some(registration) =
                                    EventRegistration::new(app.pid, &thread_invalidation, &run_loop)
                            {
                                registrations.insert(app, registration);
                            }
                        }
                    }
                    CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.1, true);
                }
                for registration in registrations.into_values() {
                    run_loop.remove_source(Some(&registration.source), unsafe {
                        kCFRunLoopDefaultMode
                    });
                }
            })
            .expect("failed to start AX event thread");
        let monitor = Self {
            invalidation,
            applications,
            stopped,
            thread: Some(thread),
        };
        monitor.refresh_applications();
        monitor
    }

    pub fn take_dirty(&self) -> bool {
        self.invalidation.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn take_window_created(&self) -> bool {
        self.invalidation
            .window_created
            .swap(false, Ordering::AcqRel)
    }

    pub fn invalidate(&self) {
        self.invalidation.dirty.store(true, Ordering::Release);
    }

    pub fn refresh_applications(&self) {
        let _ = self.applications.send(running_application_instances());
    }
}

impl Default for WindowEventMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WindowEventMonitor {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct EventRegistration {
    _observer: CFRetained<AXObserver>,
    _application: CFRetained<AXUIElement>,
    source: CFRetained<CFRunLoopSource>,
}

impl EventRegistration {
    fn new(pid: i32, invalidation: &Arc<EventInvalidation>, run_loop: &CFRunLoop) -> Option<Self> {
        let application = unsafe { AXUIElement::new_application(pid) };
        let _ = unsafe { application.set_messaging_timeout(0.5) };
        let mut raw_observer = ptr::null_mut();
        let result = unsafe {
            AXObserver::create(
                pid,
                Some(window_event_callback),
                NonNull::from(&mut raw_observer),
            )
        };
        if result != AXError::Success {
            return None;
        }
        let observer = unsafe { CFRetained::from_raw(NonNull::new(raw_observer)?) };
        let refcon = Arc::as_ptr(invalidation).cast_mut().cast::<c_void>();
        let mut window_creation_subscribed = false;
        for notification in ["AXWindowCreated", "AXFocusedWindowChanged"] {
            let result = unsafe {
                observer.add_notification(
                    &application,
                    &CFString::from_static_str(notification),
                    refcon,
                )
            };
            if notification == "AXWindowCreated" {
                window_creation_subscribed = matches!(
                    result,
                    AXError::Success | AXError::NotificationAlreadyRegistered
                );
            }
        }
        if !window_creation_subscribed {
            return None;
        }
        subscribe_window_notifications(&observer, &application, refcon);
        let source = unsafe { observer.run_loop_source() };
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        Some(Self {
            _observer: observer,
            _application: application,
            source,
        })
    }
}

unsafe extern "C-unwind" fn window_event_callback(
    observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    refcon: *mut c_void,
) {
    let notification = unsafe { notification.as_ref() }.to_string();
    if let Some(invalidation) = unsafe { refcon.cast::<EventInvalidation>().as_ref() } {
        invalidation.dirty.store(true, Ordering::Release);
        if notification == "AXWindowCreated" {
            invalidation.window_created.store(true, Ordering::Release);
        }
    }
    if notification == "AXWindowCreated" {
        subscribe_one_window(
            unsafe { observer.as_ref() },
            unsafe { element.as_ref() },
            refcon,
        );
    }
}

fn subscribe_window_notifications(
    observer: &AXObserver,
    application: &AXUIElement,
    refcon: *mut c_void,
) {
    let Ok(windows) = attribute::<CFArray>(application, "AXWindows") else {
        return;
    };
    for index in 0..windows.count() {
        let raw = unsafe { windows.value_at_index(index) };
        let Some(window) = (unsafe { raw.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<AXUIElement>())
        else {
            continue;
        };
        subscribe_one_window(observer, window, refcon);
    }
}

fn subscribe_one_window(observer: &AXObserver, window: &AXUIElement, refcon: *mut c_void) {
    for notification in [
        "AXUIElementDestroyed",
        "AXMoved",
        "AXResized",
        "AXWindowMiniaturized",
        "AXWindowDeminiaturized",
        "AXTitleChanged",
    ] {
        let _ = unsafe {
            observer.add_notification(window, &CFString::from_static_str(notification), refcon)
        };
    }
}

fn running_application_instances() -> Vec<AppInstanceId> {
    let applications = NSWorkspace::sharedWorkspace().runningApplications();
    (0..applications.count())
        .filter_map(|index| {
            let application = applications.objectAtIndex(index);
            let pid = application.processIdentifier();
            (pid > 0 && pid != std::process::id() as i32).then(|| AppInstanceId {
                pid,
                launched_at: launch_time(&application),
            })
        })
        .collect()
}

pub fn snapshot_in_outputs(
    cycle: u64,
    outputs: &DisplayOutputs,
) -> Result<NativeObservation, SnapshotError> {
    snapshot_in_output(cycle, outputs.union()).map(|(observation, _)| observation)
}

pub fn outputs() -> Result<DisplayOutputs, SnapshotError> {
    let marker = MainThreadMarker::new().ok_or(SnapshotError::MainThreadRequired)?;
    let primary_height = primary_screen_height(marker)?;
    let screens = NSScreen::screens(marker);
    let mut displays = Vec::with_capacity(screens.count());
    for index in 0..screens.count() {
        let screen = screens.objectAtIndex(index);
        let frame = screen.frame();
        let display_id = screen_display_id(&screen).ok_or(SnapshotError::MainScreenUnavailable)?;
        displays.push(DisplayOutput {
            id: display_uuid(display_id).unwrap_or_else(|| display_id.to_string()),
            name: screen.localizedName().to_string(),
            frame: screen_output(&screen, primary_height),
            is_main: frame.origin.x == 0.0 && frame.origin.y == 0.0,
        });
    }
    DisplayOutputs::new(displays)
}

fn screen_display_id(screen: &NSScreen) -> Option<u32> {
    let description = screen.deviceDescription();
    let key = NSString::from_str("NSScreenNumber");
    let number = description.objectForKey(&key)?;
    number
        .downcast_ref::<NSNumber>()
        .map(NSNumber::unsignedIntValue)
}

fn display_uuid(display_id: u32) -> Option<String> {
    let raw = unsafe { CGDisplayCreateUUIDFromDisplayID(display_id) };
    let uuid = unsafe { CFRetained::from_raw(NonNull::new(raw.cast_mut())?) };
    CFUUID::new_string(None, Some(&uuid)).map(|uuid| uuid.to_string())
}

pub fn snapshot_for_window(
    cycle: u64,
    window_id: u32,
) -> Result<(NativeObservation, Settings), SnapshotError> {
    if !accessibility_trusted() {
        return Err(SnapshotError::AccessibilityPermissionRequired);
    }
    let outputs = outputs()?;
    let (observation, _) = snapshot_in_output(cycle, outputs.union())?;
    let frame = observation
        .applications
        .iter()
        .find_map(|app| match app {
            AppObservation::Complete { windows, .. } => windows
                .iter()
                .find(|window| window.id.window_id == window_id)
                .map(|window| window.frame),
            _ => None,
        })
        .ok_or(SnapshotError::TargetWindowUnavailable)?;
    let output = outputs
        .frames()
        .find(|output| output.contains_center(frame))
        .ok_or(SnapshotError::TargetWindowUnavailable)?;
    snapshot_in_output(cycle, output)
}

pub fn snapshot_in_output(
    cycle: u64,
    output: Rect,
) -> Result<(NativeObservation, Settings), SnapshotError> {
    if !accessibility_trusted() {
        return Err(SnapshotError::AccessibilityPermissionRequired);
    }
    let onscreen = onscreen_windows().ok_or(SnapshotError::WindowListUnavailable)?;

    let running = NSWorkspace::sharedWorkspace().runningApplications();
    let applications = (0..running.count())
        .filter_map(|index| {
            let application = running.objectAtIndex(index);
            let windows = onscreen.get(&application.processIdentifier())?;
            observe_application(&application, windows, output)
        })
        .collect();

    Ok((
        NativeObservation {
            cycle,
            mouse_down: left_mouse_down(),
            applications,
        },
        Settings::for_output(output),
    ))
}

fn primary_screen_height(marker: MainThreadMarker) -> Result<f64, SnapshotError> {
    let screens = NSScreen::screens(marker);
    (0..screens.count())
        .map(|index| screens.objectAtIndex(index).frame())
        .find(|frame| frame.origin.x == 0.0 && frame.origin.y == 0.0)
        .map(|frame| frame.size.height)
        .ok_or(SnapshotError::MainScreenUnavailable)
}

fn screen_output(screen: &NSScreen, primary_height: f64) -> Rect {
    let visible = screen.visibleFrame();
    Rect {
        x: visible.origin.x.round() as i32,
        y: (primary_height - visible.origin.y - visible.size.height).round() as i32,
        width: visible.size.width.round() as i32,
        height: visible.size.height.round() as i32,
    }
}

fn union_rect(left: Rect, right: Rect) -> Rect {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    Rect {
        x,
        y,
        width: (left.x + left.width).max(right.x + right.width) - x,
        height: (left.y + left.height).max(right.y + right.height) - y,
    }
}

pub fn apply(placements: &[Placement], settings: &Settings) -> Vec<ApplyResult> {
    apply_while(placements, settings, || true)
}

pub fn apply_while(
    placements: &[Placement],
    settings: &Settings,
    mut should_continue: impl FnMut() -> bool,
) -> Vec<ApplyResult> {
    let onscreen = onscreen_windows().unwrap_or_default();
    placements
        .iter()
        .take_while(|_| should_continue())
        .filter_map(|placement| {
            placement.window.as_ref().map(|window| {
                apply_window(window, placement.frame, settings, &onscreen).unwrap_or_else(|| {
                    ApplyResult {
                        window: window.clone(),
                        target: placement.frame,
                        actual: None,
                        outcome: ApplyOutcome::Gone,
                    }
                })
            })
        })
        .collect()
}

pub fn apply_managed_while(
    placements: &[Placement],
    settings: &Settings,
    should_continue: impl FnMut() -> bool,
) -> Vec<ApplyResult> {
    apply_managed_while_excluding(placements, settings, None, should_continue)
}

pub fn apply_managed_during_drag(
    placements: &[Placement],
    settings: &Settings,
    dragged: &PhysicalWindowId,
    should_continue: impl FnMut() -> bool,
) -> Vec<ApplyResult> {
    apply_managed_while_excluding(placements, settings, Some(dragged), should_continue)
}

fn apply_managed_while_excluding(
    placements: &[Placement],
    settings: &Settings,
    dragged: Option<&PhysicalWindowId>,
    mut should_continue: impl FnMut() -> bool,
) -> Vec<ApplyResult> {
    let placements: Vec<_> = placements
        .iter()
        .take_while(|_| should_continue())
        .filter_map(|placement| {
            placement
                .window
                .as_ref()
                .map(|window| (window, placement.frame))
        })
        .collect();
    if left_mouse_down() && dragged.is_none() {
        return placements
            .into_iter()
            .map(|(window, target)| ApplyResult {
                window: window.clone(),
                target,
                actual: None,
                outcome: ApplyOutcome::Deferred,
            })
            .collect();
    }
    let groups = group_placements_by_app(&placements);
    let mut results: Vec<Option<ApplyResult>> = (0..placements.len()).map(|_| None).collect();
    'applications: for (app_id, indices) in groups {
        if !should_continue() {
            break;
        }
        if !process_incarnation_matches(&app_id) {
            fill_missing_results(&placements, &indices, &mut results, ApplyOutcome::Gone);
            continue;
        }
        let app = unsafe { AXUIElement::new_application(app_id.pid) };
        let _ = unsafe { app.set_messaging_timeout(0.5) };
        let windows = match attribute::<CFArray>(&app, "AXWindows") {
            Ok(windows) => windows,
            Err(AXError::InvalidUIElement) => {
                fill_missing_results(&placements, &indices, &mut results, ApplyOutcome::Gone);
                continue;
            }
            Err(_) => {
                fill_missing_results(
                    &placements,
                    &indices,
                    &mut results,
                    ApplyOutcome::Unavailable,
                );
                continue;
            }
        };
        let _animations = WindowAnimationGuard::new(&app);
        let mut pending = HashMap::<u32, Vec<usize>>::new();
        for index in &indices {
            pending
                .entry(placements[*index].0.window_id)
                .or_default()
                .push(*index);
        }
        let mut lookup_failed = false;
        for index in 0..windows.count() {
            let raw = unsafe { windows.value_at_index(index) };
            let Some(window) = (unsafe { raw.cast::<CFType>().as_ref() })
                .and_then(|value| value.downcast_ref::<AXUIElement>())
            else {
                continue;
            };
            let Some(window_id) = physical_window_id(&app_id, window) else {
                lookup_failed = true;
                continue;
            };
            let Some(placement_indices) = pending.remove(&window_id.window_id) else {
                continue;
            };
            let _ = unsafe { window.set_messaging_timeout(0.5) };
            for placement_index in placement_indices {
                if !should_continue() {
                    break 'applications;
                }
                let (id, target) = placements[placement_index];
                cache_managed_window(id, window);
                results[placement_index] = Some(if dragged == Some(id) {
                    ApplyResult {
                        window: id.clone(),
                        target,
                        actual: window_frame(window),
                        outcome: ApplyOutcome::Deferred,
                    }
                } else {
                    apply_element(id, window, target, settings)
                });
            }
            if pending.is_empty() {
                break;
            }
        }
        fill_missing_results(
            &placements,
            &pending.into_values().flatten().collect::<Vec<_>>(),
            &mut results,
            if lookup_failed {
                ApplyOutcome::Unavailable
            } else {
                ApplyOutcome::Gone
            },
        );
    }
    results.into_iter().flatten().collect()
}

fn group_placements_by_app(
    placements: &[(&PhysicalWindowId, Rect)],
) -> Vec<(AppInstanceId, Vec<usize>)> {
    let mut groups = Vec::<(AppInstanceId, Vec<usize>)>::new();
    let mut group_indices = HashMap::<AppInstanceId, usize>::new();
    for (index, (window, _)) in placements.iter().enumerate() {
        let group = *group_indices.entry(window.app.clone()).or_insert_with(|| {
            groups.push((window.app.clone(), Vec::new()));
            groups.len() - 1
        });
        groups[group].1.push(index);
    }
    groups
}

fn fill_missing_results(
    placements: &[(&PhysicalWindowId, Rect)],
    indices: &[usize],
    results: &mut [Option<ApplyResult>],
    outcome: ApplyOutcome,
) {
    for index in indices {
        let (window, target) = placements[*index];
        if matches!(outcome, ApplyOutcome::Gone) {
            evict_managed_window(window);
        }
        results[*index] = Some(ApplyResult {
            window: window.clone(),
            target,
            actual: None,
            outcome,
        });
    }
}

pub fn frame(id: &PhysicalWindowId) -> Result<Rect, WindowAccessError> {
    match with_managed_window_result(id, window_frame) {
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => {
            evict_managed_window(id);
            match with_managed_window_result(id, window_frame) {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) | Err(ManagedWindowError::Unavailable) => {
                    Err(WindowAccessError::Unavailable)
                }
                Err(ManagedWindowError::Gone) => Err(WindowAccessError::Gone),
            }
        }
        Err(ManagedWindowError::Gone) => Err(WindowAccessError::Gone),
        Err(ManagedWindowError::Unavailable) => Err(WindowAccessError::Unavailable),
    }
}

/// Point file descriptor 2 (stderr) at the given open file so diagnostics from
/// bundle launches land in a log instead of nowhere.
pub fn redirect_stderr_to(file: &std::fs::File) {
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(file);
    // SAFETY: dup2 atomically replaces descriptor 2 with a duplicate of a
    // descriptor we own; the original File may close independently afterwards.
    let _ = unsafe { libc::dup2(fd, 2) };
}

pub fn set_position_only(id: &PhysicalWindowId, target: Rect) -> ApplyOutcome {
    let outcome = match with_managed_window_result(id, |window| {
        let result = without_window_animations(&id.app, || set_position(window, target));
        let actual = window_frame(window);
        if actual.is_some_and(|actual| position_matches(actual, target)) {
            ApplyOutcome::Applied
        } else if matches!(result, AXError::InvalidUIElement) {
            ApplyOutcome::Gone
        } else {
            ApplyOutcome::Unavailable
        }
    }) {
        Ok(outcome) => outcome,
        Err(ManagedWindowError::Gone) => ApplyOutcome::Gone,
        Err(ManagedWindowError::Unavailable) => ApplyOutcome::Unavailable,
    };
    if matches!(outcome, ApplyOutcome::Gone) {
        evict_managed_window(id);
    }
    outcome
}

fn position_matches(actual: Rect, target: Rect) -> bool {
    actual.position_approximately_equals(target, crate::reconcile::DEFAULT_FRAME_TOLERANCE)
}

pub fn focused_window() -> Option<PhysicalWindowId> {
    let application = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let id = AppInstanceId {
        pid: application.processIdentifier(),
        launched_at: launch_time(&application),
    };
    let element = unsafe { AXUIElement::new_application(id.pid) };
    let _ = unsafe { element.set_messaging_timeout(0.5) };
    let focused = attribute::<AXUIElement>(&element, "AXFocusedWindow").ok()?;
    physical_window_id(&id, &focused)
}

pub fn frontmost_application() -> Option<AppInstanceId> {
    let application = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    Some(AppInstanceId {
        pid: application.processIdentifier(),
        launched_at: launch_time(&application),
    })
}

pub fn focus_window(id: &PhysicalWindowId) -> bool {
    let raised = raise_window_with_focus(id, true);
    let activated = NSRunningApplication::runningApplicationWithProcessIdentifier(id.app.pid)
        .is_some_and(|application| {
            launch_time(&application) == id.app.launched_at
                && application.activateWithOptions(NSApplicationActivationOptions::empty())
        });
    raised && activated
}

pub fn raise_window(id: &PhysicalWindowId) -> bool {
    raise_window_with_focus(id, false)
}

fn raise_window_with_focus(id: &PhysicalWindowId, focus: bool) -> bool {
    let raised = with_managed_window(id, |window| {
        if focus {
            let _ = unsafe {
                window.set_attribute_value(
                    &CFString::from_static_str("AXFocused"),
                    CFBoolean::new(true),
                )
            };
        }
        (unsafe { window.perform_action(&CFString::from_static_str("AXRaise")) })
            == AXError::Success
    })
    .unwrap_or(false);
    if !raised {
        evict_managed_window(id);
    }
    raised
}

fn apply_window(
    id: &PhysicalWindowId,
    target: Rect,
    settings: &Settings,
    onscreen: &HashMap<i32, HashSet<CGWindowID>>,
) -> Option<ApplyResult> {
    if left_mouse_down() {
        return Some(ApplyResult {
            window: id.clone(),
            target,
            actual: None,
            outcome: ApplyOutcome::Deferred,
        });
    }
    if !process_incarnation_matches(&id.app)
        || !onscreen
            .get(&id.app.pid)
            .is_some_and(|windows| windows.contains(&id.window_id))
    {
        return Some(ApplyResult {
            window: id.clone(),
            target,
            actual: None,
            outcome: ApplyOutcome::Gone,
        });
    }
    let app = unsafe { AXUIElement::new_application(id.app.pid) };
    let _ = unsafe { app.set_messaging_timeout(0.5) };
    let windows = attribute::<CFArray>(&app, "AXWindows").ok()?;
    for index in 0..windows.count() {
        let raw = unsafe { windows.value_at_index(index) };
        let Some(window) = (unsafe { raw.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<AXUIElement>())
        else {
            continue;
        };
        let _ = unsafe { window.set_messaging_timeout(0.5) };
        if physical_window_id(&id.app, window).as_ref() != Some(id) {
            continue;
        }
        if !matches!(
            observe_window(&id.app, window, onscreen.get(&id.app.pid)?, settings.output,),
            Ok(WindowObservation::Observed(_))
        ) {
            return Some(ApplyResult {
                window: id.clone(),
                target,
                actual: window_frame(window),
                outcome: ApplyOutcome::Unavailable,
            });
        }

        return Some(without_window_animations(&id.app, || {
            apply_element(id, window, target, settings)
        }));
    }
    None
}

fn apply_element(
    id: &PhysicalWindowId,
    window: &AXUIElement,
    target: Rect,
    settings: &Settings,
) -> ApplyResult {
    let before = window_frame(window);
    if before.is_some_and(|frame| frame.approximately_equals(target, settings.frame_tolerance)) {
        return ApplyResult {
            window: id.clone(),
            target,
            actual: before,
            outcome: ApplyOutcome::Applied,
        };
    }
    let wrote = set_size(window, target) == AXError::Success
        && set_position(window, target) == AXError::Success
        && set_size(window, target) == AXError::Success;
    let actual = window_frame(window);
    let outcome = match (wrote, actual) {
        (_, Some(actual)) if actual.approximately_equals(target, settings.frame_tolerance) => {
            ApplyOutcome::Applied
        }
        (true, Some(_)) => ApplyOutcome::Constrained,
        _ => ApplyOutcome::Unavailable,
    };
    ApplyResult {
        window: id.clone(),
        target,
        actual,
        outcome,
    }
}

fn without_window_animations<T>(app: &AppInstanceId, operation: impl FnOnce() -> T) -> T {
    let application = unsafe { AXUIElement::new_application(app.pid) };
    let _ = unsafe { application.set_messaging_timeout(0.5) };
    let _animations = WindowAnimationGuard::new(&application);
    operation()
}

struct WindowAnimationGuard<'a> {
    application: &'a AXUIElement,
    restore_enhanced_ui: bool,
}

impl<'a> WindowAnimationGuard<'a> {
    fn new(application: &'a AXUIElement) -> Self {
        let restore_enhanced_ui = optional_bool_attribute(application, "AXEnhancedUserInterface")
            .ok()
            .unwrap_or(false);
        if restore_enhanced_ui {
            let _ = unsafe {
                application.set_attribute_value(
                    &CFString::from_static_str("AXEnhancedUserInterface"),
                    CFBoolean::new(false),
                )
            };
        }
        Self {
            application,
            restore_enhanced_ui,
        }
    }
}

impl Drop for WindowAnimationGuard<'_> {
    fn drop(&mut self) {
        if self.restore_enhanced_ui {
            let _ = unsafe {
                self.application.set_attribute_value(
                    &CFString::from_static_str("AXEnhancedUserInterface"),
                    CFBoolean::new(true),
                )
            };
        }
    }
}

fn with_managed_window<T>(
    id: &PhysicalWindowId,
    operation: impl FnMut(&AXUIElement) -> T,
) -> Option<T> {
    with_managed_window_result(id, operation).ok()
}

enum ManagedWindowError {
    Gone,
    Unavailable,
}

fn with_managed_window_result<T>(
    id: &PhysicalWindowId,
    mut operation: impl FnMut(&AXUIElement) -> T,
) -> Result<T, ManagedWindowError> {
    if let Some(window) = MANAGED_WINDOWS.with(|windows| windows.borrow().get(id).cloned()) {
        return Ok(operation(&window));
    }
    if !process_incarnation_matches(&id.app) {
        evict_managed_app(&id.app);
        return Err(ManagedWindowError::Gone);
    }
    let app = unsafe { AXUIElement::new_application(id.app.pid) };
    let _ = unsafe { app.set_messaging_timeout(0.5) };
    let windows = attribute::<CFArray>(&app, "AXWindows").map_err(|error| match error {
        AXError::InvalidUIElement => ManagedWindowError::Gone,
        _ => ManagedWindowError::Unavailable,
    })?;
    for index in 0..windows.count() {
        let raw = unsafe { windows.value_at_index(index) };
        let Some(window) = (unsafe { raw.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<AXUIElement>())
        else {
            continue;
        };
        let _ = unsafe { window.set_messaging_timeout(0.5) };
        if physical_window_id(&id.app, window).as_ref() == Some(id) {
            cache_managed_window(id, window);
            return Ok(operation(window));
        }
    }
    Err(ManagedWindowError::Gone)
}

fn cache_managed_window(id: &PhysicalWindowId, window: &AXUIElement) {
    let retained = unsafe { CFRetained::retain(NonNull::from(window)) };
    MANAGED_WINDOWS.with(|windows| {
        windows.borrow_mut().insert(id.clone(), retained);
    });
}

fn evict_managed_window(id: &PhysicalWindowId) {
    MANAGED_WINDOWS.with(|windows| {
        windows.borrow_mut().remove(id);
    });
}

fn evict_managed_app(app: &AppInstanceId) {
    MANAGED_WINDOWS.with(|windows| {
        windows.borrow_mut().retain(|window, _| &window.app != app);
    });
}

fn prune_managed_app(app: &AppInstanceId, current: &HashSet<PhysicalWindowId>) {
    MANAGED_WINDOWS.with(|windows| {
        windows
            .borrow_mut()
            .retain(|window, _| &window.app != app || current.contains(window));
    });
}

fn left_mouse_down() -> bool {
    CGEventSource::button_state(CGEventSourceStateID::HIDSystemState, CGMouseButton::Left)
}

pub fn mouse_down() -> bool {
    left_mouse_down()
}

pub fn mouse_position() -> Option<(i32, i32)> {
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some((point.x.round() as i32, point.y.round() as i32))
}

fn process_incarnation_matches(id: &AppInstanceId) -> bool {
    NSRunningApplication::runningApplicationWithProcessIdentifier(id.pid)
        .is_some_and(|application| launch_time(&application) == id.launched_at)
}

pub fn is_process_incarnation_running(id: &AppInstanceId) -> bool {
    let running = process_incarnation_matches(id);
    if !running {
        evict_managed_app(id);
    }
    running
}

fn launch_time(application: &NSRunningApplication) -> u64 {
    application.launchDate().map_or(0, |date| {
        (date.timeIntervalSince1970() * 1000.0).round() as u64
    })
}

fn set_position(window: &AXUIElement, frame: Rect) -> AXError {
    let mut point = CGPoint {
        x: f64::from(frame.x),
        y: f64::from(frame.y),
    };
    let Some(value) = (unsafe {
        AXValue::new(
            AXValueType::CGPoint,
            NonNull::from(&mut point).cast::<c_void>(),
        )
    }) else {
        return AXError::Failure;
    };
    unsafe { window.set_attribute_value(&CFString::from_static_str("AXPosition"), &value) }
}

fn set_size(window: &AXUIElement, frame: Rect) -> AXError {
    let mut size = CGSize {
        width: f64::from(frame.width),
        height: f64::from(frame.height),
    };
    let Some(value) = (unsafe {
        AXValue::new(
            AXValueType::CGSize,
            NonNull::from(&mut size).cast::<c_void>(),
        )
    }) else {
        return AXError::Failure;
    };
    unsafe { window.set_attribute_value(&CFString::from_static_str("AXSize"), &value) }
}

fn window_frame(window: &AXUIElement) -> Option<Rect> {
    let position = ax_point(window, "AXPosition")?;
    let size = ax_size(window, "AXSize")?;
    Some(Rect {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        width: size.width.round() as i32,
        height: size.height.round() as i32,
    })
}

fn observe_application(
    application: &NSRunningApplication,
    onscreen: &HashSet<CGWindowID>,
    output: Rect,
) -> Option<AppObservation> {
    let pid = application.processIdentifier();
    if pid <= 0 || pid == std::process::id() as i32 || application.isHidden() {
        return None;
    }

    let id = AppInstanceId {
        pid,
        launched_at: launch_time(application),
    };
    let bundle_id = application
        .bundleIdentifier()
        .map_or_else(|| "unknown".into(), |value| value.to_string());
    let app_element = unsafe { AXUIElement::new_application(pid) };
    let _ = unsafe { app_element.set_messaging_timeout(0.5) };

    let windows = match attribute::<CFArray>(&app_element, "AXWindows") {
        Ok(windows) => windows,
        Err(error) => {
            return Some(AppObservation::Unavailable {
                app: id,
                error: format!("AXWindows: {error:?}"),
            });
        }
    };

    let focused_element = match attribute::<AXUIElement>(&app_element, "AXFocusedWindow") {
        Ok(window) => Some(window),
        Err(AXError::NoValue | AXError::AttributeUnsupported) => None,
        Err(error) => {
            return Some(AppObservation::Unavailable {
                app: id,
                error: format!("AXFocusedWindow: {error:?}"),
            });
        }
    };
    let focused = focused_element
        .as_ref()
        .and_then(|window| physical_window_id(&id, window))
        .filter(|window| onscreen.contains(&window.window_id));
    let mut observed = Vec::new();
    let mut current_windows = HashSet::new();
    if let Some(window) = focused_element
        && let Err(error) = collect_window(&id, &window, onscreen, output, &mut observed)
    {
        return Some(AppObservation::Unavailable { app: id, error });
    }
    for index in 0..windows.count() {
        let raw = unsafe { windows.value_at_index(index) };
        let Some(window) = (unsafe { raw.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<AXUIElement>())
        else {
            continue;
        };
        if let Some(window_id) = physical_window_id(&id, window) {
            current_windows.insert(window_id);
        }
        if let Err(error) = collect_window(&id, window, onscreen, output, &mut observed) {
            return Some(AppObservation::Unavailable { app: id, error });
        }
    }
    prune_managed_app(&id, &current_windows);

    if id.launched_at == 0 && !observed.is_empty() {
        return Some(AppObservation::Unavailable {
            app: id,
            error: "application launch date is unavailable".into(),
        });
    }

    Some(AppObservation::Complete {
        app: ObservedApplication { id, bundle_id },
        focused,
        windows: observed,
    })
}

fn collect_window(
    app: &AppInstanceId,
    window: &AXUIElement,
    onscreen: &HashSet<CGWindowID>,
    output: Rect,
    observed: &mut Vec<ObservedWindow>,
) -> Result<(), String> {
    let _ = unsafe { window.set_messaging_timeout(0.5) };
    if let WindowObservation::Observed(observation) = observe_window(app, window, onscreen, output)?
        && !observed
            .iter()
            .any(|existing| existing.id == observation.id)
    {
        cache_managed_window(&observation.id, window);
        observed.push(observation);
    }
    Ok(())
}

enum WindowObservation {
    Observed(ObservedWindow),
    Ignored,
}

fn observe_window(
    app: &AppInstanceId,
    window: &AXUIElement,
    onscreen: &HashSet<CGWindowID>,
    output: Rect,
) -> Result<WindowObservation, String> {
    let role = attribute::<CFString>(window, "AXRole")
        .map_err(|error| format!("AXRole: {error:?}"))?
        .to_string();
    let subrole = match attribute::<CFString>(window, "AXSubrole") {
        Ok(subrole) => subrole.to_string(),
        Err(AXError::NoValue | AXError::AttributeUnsupported) => {
            return Ok(WindowObservation::Ignored);
        }
        Err(error) => return Err(format!("AXSubrole: {error:?}")),
    };
    if role != "AXWindow" || subrole != "AXStandardWindow" {
        return Ok(WindowObservation::Ignored);
    }

    if optional_bool_attribute(window, "AXMinimized")
        .map_err(|error| format!("AXMinimized: {error:?}"))?
        || optional_bool_attribute(window, "AXFullScreen")
            .map_err(|error| format!("AXFullScreen: {error:?}"))?
        || !is_settable(window, "AXPosition")
            .map_err(|error| format!("AXPosition settable: {error:?}"))?
    {
        return Ok(WindowObservation::Ignored);
    }
    let resizable =
        is_settable(window, "AXSize").map_err(|error| format!("AXSize settable: {error:?}"))?;

    let frame = window_frame(window).ok_or("AX frame was unavailable")?;
    let id = physical_window_id(app, window).ok_or("physical window ID unavailable")?;
    if !onscreen.contains(&id.window_id) || !output.contains_center(frame) {
        return Ok(WindowObservation::Ignored);
    }
    let title = match attribute::<CFString>(window, "AXTitle") {
        Ok(title) => Some(std::sync::Arc::from(title.to_string())),
        Err(_) => None,
    };
    Ok(WindowObservation::Observed(ObservedWindow {
        id,
        frame,
        resizable,
        title,
    }))
}

fn onscreen_windows() -> Option<HashMap<i32, HashSet<CGWindowID>>> {
    let list = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0,
    )?;
    let mut windows = HashMap::<i32, HashSet<CGWindowID>>::new();
    for index in 0..list.count() {
        let entry = unsafe { list.value_at_index(index) };
        let Some(entry) = (unsafe { entry.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<CFDictionary>())
        else {
            continue;
        };
        if dictionary_number(entry, unsafe { kCGWindowLayer }) == Some(0)
            && let Some(id) = dictionary_number(entry, unsafe { kCGWindowNumber })
            && let Ok(id) = u32::try_from(id)
            && let Some(pid) = dictionary_number(entry, unsafe { kCGWindowOwnerPID })
            && let Ok(pid) = i32::try_from(pid)
        {
            windows.entry(pid).or_default().insert(id);
        }
    }
    Some(windows)
}

/// The window server's authoritative frame for a window. Accessibility frame
/// reads can lag behind reality for apps with asynchronous accessibility
/// servers (Electron); Core Graphics reflects the real position immediately.
/// The full window list is queried because the single-window variant
/// (`kCGWindowListOptionIncludingWindow`) silently returns nothing for some
/// applications' windows.
pub fn window_server_frame(id: &PhysicalWindowId) -> Option<Rect> {
    window_server_frames()?.remove(&id.window_id)
}

/// Window-server frames for every ordinary window in one list copy. Use this
/// instead of [`window_server_frame`] when checking more than one window.
pub fn window_server_frames() -> Option<HashMap<CGWindowID, Rect>> {
    let mut frames = HashMap::new();
    let list = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
        0,
    )?;
    for index in 0..list.count() {
        let entry = unsafe { list.value_at_index(index) };
        let Some(entry) = (unsafe { entry.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<CFDictionary>())
        else {
            continue;
        };
        if dictionary_number(entry, unsafe { kCGWindowLayer }) != Some(0) {
            continue;
        }
        let Some(id) = dictionary_number(entry, unsafe { kCGWindowNumber })
            .and_then(|id| u32::try_from(id).ok())
        else {
            continue;
        };
        let raw = unsafe { entry.value((kCGWindowBounds as *const CFString).cast::<c_void>()) };
        let Some(bounds) = (unsafe { raw.cast::<CFType>().as_ref() })
            .and_then(|value| value.downcast_ref::<CFDictionary>())
        else {
            continue;
        };
        let field = |name: &'static str| {
            dictionary_double(bounds, &CFString::from_static_str(name))
                .map(|value| value.round() as i32)
        };
        if let (Some(x), Some(y), Some(width), Some(height)) =
            (field("X"), field("Y"), field("Width"), field("Height"))
        {
            frames.insert(
                id,
                Rect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        }
    }
    Some(frames)
}

fn dictionary_double(dictionary: &CFDictionary, key: &CFString) -> Option<f64> {
    let raw = unsafe { dictionary.value(key as *const CFString as *const c_void) };
    let number = unsafe { raw.cast::<CFType>().as_ref() }?.downcast_ref::<CFNumber>()?;
    let mut value = 0_f64;
    unsafe {
        number
            .value(CFNumberType::Float64Type, ptr::from_mut(&mut value).cast())
            .then_some(value)
    }
}

fn dictionary_number(dictionary: &CFDictionary, key: &CFString) -> Option<i64> {
    let raw = unsafe { dictionary.value(key as *const CFString as *const c_void) };
    let number = unsafe { raw.cast::<CFType>().as_ref() }?.downcast_ref::<CFNumber>()?;
    let mut value = 0_i64;
    unsafe {
        number
            .value(CFNumberType::SInt64Type, ptr::from_mut(&mut value).cast())
            .then_some(value)
    }
}

fn physical_window_id(app: &AppInstanceId, element: &AXUIElement) -> Option<PhysicalWindowId> {
    static FUNCTION: OnceLock<Option<WindowIdFunction>> = OnceLock::new();
    let function = FUNCTION
        .get_or_init(|| {
            let symbol =
                unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_AXUIElementGetWindow".as_ptr()) };
            (!symbol.is_null()).then(|| unsafe { std::mem::transmute(symbol) })
        })
        .as_ref()?;
    let mut window_id = 0;
    let error = unsafe { function(element, &mut window_id) };
    (error == AXError::Success && window_id != 0).then(|| PhysicalWindowId {
        app: app.clone(),
        window_id,
    })
}

fn is_settable(element: &AXUIElement, name: &'static str) -> Result<bool, AXError> {
    let attribute = CFString::from_static_str(name);
    let mut settable = 0;
    let error = unsafe { element.is_attribute_settable(&attribute, NonNull::from(&mut settable)) };
    (error == AXError::Success)
        .then_some(settable != 0)
        .ok_or(error)
}

fn bool_attribute(element: &AXUIElement, name: &'static str) -> Result<bool, AXError> {
    attribute::<CFBoolean>(element, name).map(|value| value.value())
}

fn optional_bool_attribute(element: &AXUIElement, name: &'static str) -> Result<bool, AXError> {
    match bool_attribute(element, name) {
        Ok(value) => Ok(value),
        Err(AXError::NoValue | AXError::AttributeUnsupported) => Ok(false),
        Err(error) => Err(error),
    }
}

fn attribute<T: ConcreteType>(
    element: &AXUIElement,
    name: &'static str,
) -> Result<CFRetained<T>, AXError> {
    let name = CFString::from_static_str(name);
    let mut raw: *const CFType = ptr::null();
    let error = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut raw)) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(raw) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::NoValue);
    };
    unsafe { CFRetained::from_raw(raw) }
        .downcast()
        .map_err(|_| AXError::IllegalArgument)
}

fn ax_point(element: &AXUIElement, name: &'static str) -> Option<CGPoint> {
    let value = attribute::<AXValue>(element, name).ok()?;
    let mut result = CGPoint::ZERO;
    unsafe {
        value
            .value(
                AXValueType::CGPoint,
                NonNull::from(&mut result).cast::<c_void>(),
            )
            .then_some(result)
    }
}

fn ax_size(element: &AXUIElement, name: &'static str) -> Option<CGSize> {
    let value = attribute::<AXValue>(element, name).ok()?;
    let mut result = CGSize::ZERO;
    unsafe {
        value
            .value(
                AXValueType::CGSize,
                NonNull::from(&mut result).cast::<c_void>(),
            )
            .then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_only_validation_ignores_window_size() {
        let target = Rect {
            x: 100,
            y: 200,
            width: 800,
            height: 600,
        };
        let differently_sized = Rect {
            width: 320,
            height: 240,
            ..target
        };

        assert!(position_matches(differently_sized, target));
    }

    #[test]
    fn managed_layout_reads_each_app_window_list_once() {
        let app = AppInstanceId {
            pid: 1,
            launched_at: 1,
        };
        let windows: Vec<_> = (1..=256)
            .map(|window_id| PhysicalWindowId {
                app: app.clone(),
                window_id,
            })
            .collect();
        let placements: Vec<_> = windows
            .iter()
            .map(|window| (window, Rect::default()))
            .collect();

        let groups = group_placements_by_app(&placements);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, app);
        assert_eq!(groups[0].1.len(), 256);
    }

    #[test]
    fn arbitrary_display_topology_resolves_preferences_and_fallbacks() {
        let outputs = DisplayOutputs::new(vec![
            DisplayOutput {
                id: "main-id".into(),
                name: "Built-in".into(),
                frame: Rect::default(),
                is_main: true,
            },
            DisplayOutput {
                id: "second-id".into(),
                name: "Studio Display".into(),
                frame: Rect {
                    x: 100,
                    ..Rect::default()
                },
                is_main: false,
            },
            DisplayOutput {
                id: "third-id".into(),
                name: "Projector".into(),
                frame: Rect {
                    x: 200,
                    ..Rect::default()
                },
                is_main: false,
            },
        ])
        .unwrap();

        assert_eq!(outputs.displays().len(), 3);
        assert_eq!(
            outputs
                .resolve(&DisplaySelector::Preferred("third-id".into()))
                .id,
            "third-id"
        );
        assert_eq!(
            outputs
                .resolve(&DisplaySelector::Preferred("missing".into()))
                .id,
            "main-id"
        );
    }
}
