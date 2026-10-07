use std::{
    cell::RefCell,
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Render, Subscription, Timer, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, prelude::*, px, size,
};

use crate::{
    app::Action,
    desktop_ui::{
        compact_button, primary_button, settings_panel, settings_row, success_badge, toggle,
        window_frame,
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
                settings.refresh(cx);
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
        let _ = handle.update(cx, |settings, _, cx| settings.refresh(cx));
    });
}

struct SettingsWindow {
    actions: Sender<Action>,
    accessibility: bool,
    launch_at_login: login_item::Status,
    launch_at_login_toggle: ToggleSpring,
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
                    .update(cx, |settings, cx| settings.refresh(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let activation = cx.observe_window_activation(window, |settings, window, cx| {
            if window.is_window_active() {
                settings.refresh(cx);
            }
        });
        let (launch_at_login, launch_error) = launch_status();
        Self {
            actions,
            accessibility: macos::accessibility_trusted(),
            launch_at_login,
            launch_at_login_toggle: ToggleSpring::new(
                launch_at_login == login_item::Status::Enabled,
            ),
            launch_error,
            update_status: sparkle::status(),
            _activation: activation,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let accessibility = macos::accessibility_trusted();
        let (launch_at_login, launch_error) = launch_status();
        let update_status = sparkle::status();
        let changed = self.accessibility != accessibility
            || self.launch_at_login != launch_at_login
            || self.launch_error != launch_error
            || self.update_status != update_status;
        self.accessibility = accessibility;
        self.launch_at_login = launch_at_login;
        self.launch_at_login_toggle
            .set_enabled(launch_at_login == login_item::Status::Enabled);
        self.launch_error = launch_error;
        self.update_status = update_status;
        if changed {
            cx.notify();
        }
    }

    fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let launch_at_login_control =
            if self.launch_at_login == login_item::Status::RequiresApproval {
                compact_button("Open Settings").into_any_element()
            } else {
                toggle(self.launch_at_login_toggle.render_position(window))
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
                .child(
                    settings_row(
                        "Launch at Login",
                        login_description,
                        launch_at_login_control,
                    )
                    .id("toggle-launch-at-login")
                    .on_click(cx.listener(|settings, _, _, cx| {
                        if settings.launch_at_login != login_item::Status::RequiresApproval {
                            settings.launch_at_login_toggle.set_enabled(
                                settings.launch_at_login != login_item::Status::Enabled,
                            );
                        }
                        settings.send(Action::ToggleLaunchAtLogin);
                        cx.notify();
                    })),
                )
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

struct ToggleSpring {
    position: f32,
    velocity: f32,
    target: f32,
    last_frame: Instant,
}

impl ToggleSpring {
    fn new(enabled: bool) -> Self {
        let position = if enabled { 1.0 } else { 0.0 };
        Self {
            position,
            velocity: 0.0,
            target: position,
            last_frame: Instant::now(),
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        let target = if enabled { 1.0 } else { 0.0 };
        if self.target != target {
            self.target = target;
            self.last_frame = Instant::now();
        }
    }

    fn advance(&mut self, elapsed: Duration) {
        const STIFFNESS: f32 = 40.0;
        let mut remaining = elapsed.as_secs_f32().min(0.1);
        while remaining > 0.0 {
            let dt = remaining.min(1.0 / 240.0);
            let acceleration = -STIFFNESS.powi(2) * (self.position - self.target)
                - 2.0 * STIFFNESS * self.velocity;
            self.velocity += acceleration * dt;
            self.position += self.velocity * dt;
            remaining -= dt;
        }
        if self.is_settled() {
            self.position = self.target;
            self.velocity = 0.0;
        }
    }

    fn render_position(&mut self, window: &mut Window) -> f32 {
        let now = Instant::now();
        self.advance(now.duration_since(self.last_frame));
        self.last_frame = now;
        if !self.is_settled() {
            window.request_animation_frame();
        }
        self.position
    }

    fn is_settled(&self) -> bool {
        (self.position - self.target).abs() < 0.001 && self.velocity.abs() < 0.01
    }
}

fn launch_status() -> (login_item::Status, Option<String>) {
    match login_item::status() {
        Ok(status) => (status, None),
        Err(error) => (login_item::Status::Disabled, Some(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_spring_is_frame_rate_independent_and_settles() {
        let simulate = |frame_rate: u32| {
            let mut spring = ToggleSpring::new(false);
            spring.set_enabled(true);
            for _ in 0..frame_rate {
                spring.advance(Duration::from_secs_f32(1.0 / frame_rate as f32));
            }
            spring
        };

        let at_60_hz = simulate(60);
        let at_120_hz = simulate(120);
        assert!(at_60_hz.is_settled());
        assert!(at_120_hz.is_settled());
        assert!((at_60_hz.position - at_120_hz.position).abs() < 0.001);
        assert_eq!(at_60_hz.position, 1.0);
    }
}
