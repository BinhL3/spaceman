use std::collections::{HashMap, HashSet};

use crate::{
    AppInstanceId, AppObservation, NativeObservation, ObservedApplication, Placement,
    Reconciliation, Settings, macos,
};

pub fn add_absent_applications(
    observation: &mut NativeObservation,
    known_apps: &mut HashMap<AppInstanceId, String>,
) {
    let present: HashSet<_> = observation
        .applications
        .iter()
        .map(|app| match app {
            AppObservation::Complete { app, .. } => app.id.clone(),
            AppObservation::Unavailable { app, .. } | AppObservation::Terminated { app } => {
                app.clone()
            }
        })
        .collect();
    for app in &observation.applications {
        if let AppObservation::Complete { app, .. } = app {
            known_apps.insert(app.id.clone(), app.bundle_id.clone());
        }
    }
    let missing: Vec<_> = known_apps
        .iter()
        .filter(|(app, _)| !present.contains(*app))
        .map(|(app, bundle_id)| (app.clone(), bundle_id.clone()))
        .collect();
    for (app, bundle_id) in missing {
        if macos::is_process_incarnation_running(&app) {
            observation.applications.push(AppObservation::Complete {
                app: ObservedApplication { id: app, bundle_id },
                focused: None,
                windows: Vec::new(),
            });
        } else {
            known_apps.remove(&app);
            observation
                .applications
                .push(AppObservation::Terminated { app });
        }
    }
}

pub fn applicable_placements(
    observation: &NativeObservation,
    reconciliation: &Reconciliation,
    settings: &Settings,
) -> Vec<Placement> {
    let observed_frames: HashMap<_, _> = observation
        .applications
        .iter()
        .flat_map(|app| match app {
            AppObservation::Complete { windows, .. } => windows.as_slice(),
            AppObservation::Unavailable { .. } | AppObservation::Terminated { .. } => &[],
        })
        .map(|window| (&window.id, window.frame))
        .collect();
    reconciliation
        .placements
        .iter()
        .filter(|placement| {
            placement
                .window
                .as_ref()
                .and_then(|window| observed_frames.get(window))
                .is_some_and(|frame| {
                    !frame.approximately_equals(placement.frame, settings.frame_tolerance)
                })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{Model, ObservedWindow, PhysicalWindowId, ReconciliationEvidence, Rect, reconcile};

    use super::*;

    #[test]
    fn only_mismatched_observed_frames_need_application() {
        let app = AppInstanceId {
            pid: 10,
            launched_at: 1,
        };
        let window = PhysicalWindowId {
            app: app.clone(),
            window_id: 20,
        };
        let settings = Settings {
            output: Rect {
                width: 200,
                height: 100,
                ..Rect::default()
            },
            frame_tolerance: 2,
        };
        let mut observation = NativeObservation {
            cycle: 1,
            mouse_down: false,
            applications: vec![AppObservation::Complete {
                app: ObservedApplication {
                    id: app,
                    bundle_id: "com.example.Editor".into(),
                },
                focused: Some(window.clone()),
                windows: vec![ObservedWindow {
                    id: window,
                    frame: Rect {
                        width: 100,
                        height: 100,
                        ..Rect::default()
                    },
                }],
            }],
        };
        let reconciliation = reconcile(
            &Model::default(),
            &observation,
            &ReconciliationEvidence::default(),
            &settings,
        );

        assert_eq!(
            applicable_placements(&observation, &reconciliation, &settings).len(),
            1
        );
        let AppObservation::Complete { windows, .. } = &mut observation.applications[0] else {
            unreachable!()
        };
        windows[0].frame = reconciliation.placements[0].frame;
        assert!(applicable_placements(&observation, &reconciliation, &settings).is_empty());
    }
}
