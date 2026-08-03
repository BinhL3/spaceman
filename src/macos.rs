use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    ptr,
    ptr::NonNull,
    sync::OnceLock,
};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSRunningApplication, NSScreen, NSWorkspace};
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType,
    CGPoint, CGSize, ConcreteType,
};
use objc2_core_graphics::{
    CGEventSource, CGEventSourceStateID, CGMouseButton, CGWindowID, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};

use crate::{
    AppInstanceId, AppObservation, NativeObservation, ObservedApplication, ObservedWindow,
    PhysicalWindowId, Placement, Rect, Settings, reconcile::DEFAULT_FRAME_TOLERANCE,
};

#[derive(Debug, serde::Serialize)]
pub struct ApplyResult {
    pub window: PhysicalWindowId,
    pub target: Rect,
    pub actual: Option<Rect>,
    pub outcome: ApplyOutcome,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyOutcome {
    Applied,
    Constrained,
    Gone,
    Unavailable,
    Deferred,
}

#[derive(Debug)]
pub enum SnapshotError {
    AccessibilityPermissionRequired,
    MainThreadRequired,
    MainScreenUnavailable,
    WindowListUnavailable,
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
        }
    }
}

type WindowIdFunction = unsafe extern "C" fn(&AXUIElement, *mut CGWindowID) -> AXError;

pub fn snapshot(cycle: u64) -> Result<(NativeObservation, Settings), SnapshotError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SnapshotError::AccessibilityPermissionRequired);
    }

    let marker = MainThreadMarker::new().ok_or(SnapshotError::MainThreadRequired)?;
    let screen = NSScreen::mainScreen(marker).ok_or(SnapshotError::MainScreenUnavailable)?;
    let visible = screen.visibleFrame();
    let full = screen.frame();
    let output = Rect {
        x: visible.origin.x.round() as i32,
        y: (full.size.height - visible.origin.y - visible.size.height).round() as i32,
        width: visible.size.width.round() as i32,
        height: visible.size.height.round() as i32,
    };
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
        Settings {
            output,
            frame_tolerance: DEFAULT_FRAME_TOLERANCE,
        },
    ))
}

pub fn apply(placements: &[Placement], settings: &Settings) -> Vec<ApplyResult> {
    let onscreen = onscreen_windows().unwrap_or_default();
    placements
        .iter()
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

        let before = window_frame(window);
        if before.is_some_and(|frame| frame.approximately_equals(target, settings.frame_tolerance))
        {
            return Some(ApplyResult {
                window: id.clone(),
                target,
                actual: before,
                outcome: ApplyOutcome::Applied,
            });
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
        return Some(ApplyResult {
            window: id.clone(),
            target,
            actual,
            outcome,
        });
    }
    None
}

fn left_mouse_down() -> bool {
    CGEventSource::button_state(CGEventSourceStateID::HIDSystemState, CGMouseButton::Left)
}

fn process_incarnation_matches(id: &AppInstanceId) -> bool {
    let running = NSWorkspace::sharedWorkspace().runningApplications();
    (0..running.count()).any(|index| {
        let application = running.objectAtIndex(index);
        application.processIdentifier() == id.pid && launch_time(&application) == id.launched_at
    })
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
        if let Err(error) = collect_window(&id, window, onscreen, output, &mut observed) {
            return Some(AppObservation::Unavailable { app: id, error });
        }
    }

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
    if let WindowObservation::Observed(window) = observe_window(app, window, onscreen, output)?
        && !observed.iter().any(|existing| existing.id == window.id)
    {
        observed.push(window);
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
        || !is_settable(window, "AXSize").map_err(|error| format!("AXSize settable: {error:?}"))?
    {
        return Ok(WindowObservation::Ignored);
    }

    let frame = window_frame(window).ok_or("AX frame was unavailable")?;
    let id = physical_window_id(app, window).ok_or("physical window ID unavailable")?;
    let center_x = frame.x + frame.width / 2;
    let center_y = frame.y + frame.height / 2;
    if !onscreen.contains(&id.window_id)
        || center_x < output.x
        || center_x >= output.x + output.width
        || center_y < output.y
        || center_y >= output.y + output.height
    {
        return Ok(WindowObservation::Ignored);
    }
    Ok(WindowObservation::Observed(ObservedWindow { id, frame }))
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
