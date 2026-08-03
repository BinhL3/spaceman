use std::{
    cell::RefCell,
    sync::mpsc::{Receiver, Sender, channel},
};

use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, msg_send, rc::Retained, sel};
use objc2_app_kit::{
    NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

#[derive(Clone, Copy, Debug)]
pub enum Action {
    ToggleAutomatic,
    Retile,
    OpenAccessibility,
    Quit,
}

struct TargetIvars {
    actions: Sender<Action>,
}

objc2::define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(toggleAutomatic:))]
        fn toggle_automatic(&self, _sender: &NSObject) {
            let _ = self.ivars().actions.send(Action::ToggleAutomatic);
        }

        #[unsafe(method(retile:))]
        fn retile(&self, _sender: &NSObject) {
            let _ = self.ivars().actions.send(Action::Retile);
        }

        #[unsafe(method(openAccessibility:))]
        fn open_accessibility(&self, _sender: &NSObject) {
            let _ = self.ivars().actions.send(Action::OpenAccessibility);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSObject) {
            let _ = self.ivars().actions.send(Action::Quit);
        }
    }
);

impl Target {
    fn new(actions: Sender<Action>, marker: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(marker).set_ivars(TargetIvars { actions });
        unsafe { msg_send![super(this), init] }
    }
}

struct Controller {
    _item: Retained<NSStatusItem>,
    _menu: Retained<NSMenu>,
    _target: Retained<Target>,
    status: Retained<NSMenuItem>,
    automatic: Retained<NSMenuItem>,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}

pub fn install() -> Result<Receiver<Action>, String> {
    let marker = MainThreadMarker::new().ok_or("status item requires the main thread")?;
    let (sender, receiver) = channel();
    let target = Target::new(sender, marker);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(marker), &NSString::from_str("Spaceman"));
    let status = plain_item("Starting...", marker);
    status.setEnabled(false);
    menu.addItem(&status);
    menu.addItem(&NSMenuItem::separatorItem(marker));
    let automatic = action_item("Automatic Tiling", &target, sel!(toggleAutomatic:), marker);
    automatic.setState(NSControlStateValueOff);
    menu.addItem(&automatic);
    menu.addItem(&action_item("Retile Now", &target, sel!(retile:), marker));
    menu.addItem(&NSMenuItem::separatorItem(marker));
    menu.addItem(&action_item(
        "Open Accessibility Settings...",
        &target,
        sel!(openAccessibility:),
        marker,
    ));
    menu.addItem(&NSMenuItem::separatorItem(marker));
    menu.addItem(&action_item("Quit Spaceman", &target, sel!(quit:), marker));

    let item = NSStatusBar::systemStatusBar().statusItemWithLength(-2.0);
    let button = item
        .button(marker)
        .ok_or("status item button is unavailable")?;
    let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("rectangle.split.3x1"),
        Some(&NSString::from_str("Spaceman")),
    )
    .ok_or("Spaceman status symbol is unavailable")?;
    symbol.setTemplate(true);
    button.setImage(Some(&symbol));
    item.setMenu(Some(&menu));

    CONTROLLER.with(|controller| {
        *controller.borrow_mut() = Some(Controller {
            _item: item,
            _menu: menu,
            _target: target,
            status,
            automatic,
        });
    });
    set_status("Paused");
    Ok(receiver)
}

pub fn set_status(status: &str) {
    CONTROLLER.with(|controller| {
        if let Some(controller) = controller.borrow().as_ref() {
            controller.status.setTitle(&NSString::from_str(status));
        }
    });
}

pub fn set_automatic(enabled: bool) {
    CONTROLLER.with(|controller| {
        if let Some(controller) = controller.borrow().as_ref() {
            controller.automatic.setState(if enabled {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    });
}

fn plain_item(title: &str, marker: MainThreadMarker) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(marker),
            &NSString::from_str(title),
            None,
            &NSString::new(),
        )
    }
}

fn action_item(
    title: &str,
    target: &Target,
    action: objc2::runtime::Sel,
    marker: MainThreadMarker,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(marker),
            &NSString::from_str(title),
            Some(action),
            &NSString::new(),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}
