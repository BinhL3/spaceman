use serde::{Deserialize, Serialize};

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
