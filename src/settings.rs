use std::{cell::RefCell, sync::mpsc::Sender, time::Duration};

use gpui::{
    App, Bounds, Context, Render, Subscription, Timer, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, prelude::*, px, size,
};

use crate::{
    app::Action,
    desktop_ui::{
        compact_button, primary_button, settings_panel, settings_row, success_badge, window_frame,
    },
    login_item, macos, sparkle,
};

const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 300.0;

thread_local! {
    static WINDOW: RefCell<Option<WindowHandle<SettingsWindow>>> = const { RefCell::new(None) };
}

pub(crate) fn open(actions: Sender<Action>, cx: &mut App) -> Result<(), String> {
    if let Some(handle) = WINDOW.with(|window| *window.borrow())
        && handle
            .update(cx, |settings, window, cx| {
                settings.refresh();
                cx.notify();
                window.activate_window();
            })
            .is_ok()
    {
        cx.activate(true);
        return Ok(());
    }

    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spaceman".into()),
                    ..Default::default()
                }),
                is_resizable: false,
                is_minimizable: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SettingsWindow::new(actions, window, cx)),
        )
        .map_err(|error| error.to_string())?;
    WINDOW.with(|window| *window.borrow_mut() = Some(handle));
    cx.activate(true);
    Ok(())
}

pub(crate) fn refresh(cx: &mut App) {
    WINDOW.with(|window| {
        let Some(handle) = *window.borrow() else {
            return;
        };
        let _ = handle.update(cx, |settings, _, cx| {
            if settings.refresh() {
                cx.notify();
            }
        });
    });
}

struct SettingsWindow {
    actions: Sender<Action>,
    accessibility: bool,
    launch_at_login: login_item::Status,
    launch_error: Option<String>,
    update_status: sparkle::UpdateStatus,
    _activation: Subscription,
}

impl SettingsWindow {
    fn new(actions: Sender<Action>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |settings, cx| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                if settings
                    .update(cx, |settings, cx| {
                        if settings.refresh() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let activation = cx.observe_window_activation(window, |settings, window, cx| {
            if window.is_window_active() && settings.refresh() {
                cx.notify();
            }
        });
        let (launch_at_login, launch_error) = launch_status();
        Self {
            actions,
            accessibility: macos::accessibility_trusted(),
            launch_at_login,
            launch_error,
            update_status: sparkle::status(),
            _activation: activation,
        }
    }

    fn refresh(&mut self) -> bool {
        let accessibility = macos::accessibility_trusted();
        let (launch_at_login, launch_error) = launch_status();
        let update_status = sparkle::status();
        let changed = self.accessibility != accessibility
            || self.launch_at_login != launch_at_login
            || self.launch_error != launch_error
            || self.update_status != update_status;
        self.accessibility = accessibility;
        self.launch_at_login = launch_at_login;
        self.launch_error = launch_error;
        self.update_status = update_status;
        changed
    }

    fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accessibility = self.accessibility;
        let login_enabled = self.launch_at_login == login_item::Status::Enabled;
        let login_description = self.launch_error.clone().unwrap_or_else(|| {
            if login_enabled {
                "Starts automatically after sign-in."
            } else {
                "Start automatically after sign-in."
            }
            .into()
        });
        let checking = self.update_status == sparkle::UpdateStatus::Checking;
        let update_description = match self.update_status {
            sparkle::UpdateStatus::Unavailable => "Updates are available in release builds.".into(),
            sparkle::UpdateStatus::Checking => "Checking for a signed update...".into(),
            sparkle::UpdateStatus::UpToDate => {
                format!("Spaceman {} is up to date.", env!("CARGO_PKG_VERSION"))
            }
            sparkle::UpdateStatus::UpdateAvailable => "An update is ready to review.".into(),
            sparkle::UpdateStatus::Idle => {
                format!(
                    "Version {} · signed updates enabled.",
                    env!("CARGO_PKG_VERSION")
                )
            }
        };
        let accessibility_control = if accessibility {
            success_badge("Granted").into_any_element()
        } else {
            primary_button("Grant Access")
                .id("open-accessibility")
                .on_click(cx.listener(|settings, _, _, _| {
                    settings.send(Action::OpenAccessibility);
                }))
                .into_any_element()
        };

        window_frame().flex().items_center().justify_center().child(
            settings_panel()
                .w(px(472.0))
                .child(settings_row(
                    "Accessibility",
                    if accessibility {
                        "Spaceman can observe and arrange your windows."
                    } else {
                        "Permission required to observe and arrange windows."
                    },
                    accessibility_control,
                ))
                .child(settings_row(
                    "Launch at Login",
                    login_description,
                    compact_button(if login_enabled { "Disable" } else { "Enable" })
                        .id("toggle-launch-at-login")
                        .on_click(cx.listener(|settings, _, _, _| {
                            settings.send(Action::ToggleLaunchAtLogin);
                        })),
                ))
                .child(
                    settings_row(
                        "Software Update",
                        update_description,
                        compact_button(if checking { "Checking..." } else { "Check Now" })
                            .id("check-for-updates")
                            .when(!checking, |button| {
                                button.on_click(cx.listener(|settings, _, _, _| {
                                    settings.send(Action::CheckForUpdates);
                                }))
                            }),
                    )
                    .border_b_0(),
                ),
        )
    }
}

fn launch_status() -> (login_item::Status, Option<String>) {
    match login_item::status() {
        Ok(status) => (status, None),
        Err(error) => (login_item::Status::Disabled, Some(error)),
    }
}
