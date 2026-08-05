use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{AppInstanceId, PhysicalWindowId, Rect};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservedApplication {
    pub id: AppInstanceId,
    pub bundle_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservedWindow {
    pub id: PhysicalWindowId,
    pub frame: Rect,
    #[serde(skip)]
    pub title: Option<Arc<str>>,
}

#[cfg(test)]
mod tests {
    use crate::AppInstanceId;

    use super::*;

    #[test]
    fn diagnostics_do_not_serialize_window_titles() {
        let window = ObservedWindow {
            id: PhysicalWindowId {
                app: AppInstanceId {
                    pid: 1,
                    launched_at: 1,
                },
                window_id: 1,
            },
            frame: Rect::default(),
            title: Some(Arc::from("Private document")),
        };

        assert!(
            !serde_json::to_string(&window)
                .unwrap()
                .contains("Private document")
        );
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppObservation {
    Complete {
        app: ObservedApplication,
        focused: Option<PhysicalWindowId>,
        windows: Vec<ObservedWindow>,
    },
    Unavailable {
        app: AppInstanceId,
        error: String,
    },
    Terminated {
        app: AppInstanceId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeObservation {
    pub cycle: u64,
    pub mouse_down: bool,
    pub applications: Vec<AppObservation>,
}
