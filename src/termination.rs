use std::{cell::RefCell, sync::mpsc::Sender};

use objc2::{MainThreadMarker, ffi, runtime::AnyClass, sel};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};

use crate::app::Action;

enum Gate {
    Idle(Sender<Action>),
    Pending,
    Allowed,
}

thread_local! {
    static GATE: RefCell<Option<Gate>> = const { RefCell::new(None) };
}

pub(crate) fn install(actions: Sender<Action>) -> Result<(), String> {
    MainThreadMarker::new().ok_or("termination gate requires the main thread")?;
    let class = AnyClass::get(c"GPUIApplicationDelegate")
        .ok_or("GPUI application delegate class is unavailable")?;
    let selector = sel!(applicationShouldTerminate:);
    if class.instance_method(selector).is_some() {
        return Err("GPUI already implements applicationShouldTerminate:".into());
    }
    let implementation: unsafe extern "C-unwind" fn() = unsafe {
        std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                *mut objc2::runtime::AnyObject,
                objc2::runtime::Sel,
                *mut NSApplication,
            ) -> NSApplicationTerminateReply,
            unsafe extern "C-unwind" fn(),
        >(application_should_terminate)
    };
    let added = unsafe {
        ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            selector,
            implementation,
            c"Q@:@".as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err("could not install application termination gate".into());
    }
    GATE.with(|gate| *gate.borrow_mut() = Some(Gate::Idle(actions)));
    Ok(())
}

pub(crate) fn allow_next() {
    GATE.with(|gate| {
        *gate.borrow_mut() = Some(Gate::Allowed);
    });
}

pub(crate) fn reply(should_terminate: bool) {
    let Some(marker) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(marker).replyToApplicationShouldTerminate(should_terminate);
}

unsafe extern "C-unwind" fn application_should_terminate(
    _delegate: *mut objc2::runtime::AnyObject,
    _selector: objc2::runtime::Sel,
    _application: *mut NSApplication,
) -> NSApplicationTerminateReply {
    GATE.with(|gate| {
        let mut gate = gate.borrow_mut();
        match gate.take() {
            Some(Gate::Allowed) => NSApplicationTerminateReply::TerminateNow,
            Some(Gate::Idle(actions)) => {
                let _ = actions.send(Action::Terminate);
                *gate = Some(Gate::Pending);
                NSApplicationTerminateReply::TerminateLater
            }
            Some(Gate::Pending) | None => {
                *gate = Some(Gate::Pending);
                NSApplicationTerminateReply::TerminateLater
            }
        }
    })
}
