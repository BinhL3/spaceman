pub mod geometry;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
pub mod model;
pub mod observation;
pub mod reconcile;
mod serde_map;

pub use geometry::{Axis, Rect};
pub use model::{AppInstanceId, BindingState, Model, PhysicalWindowId, Slot, SlotId};
pub use observation::{AppObservation, NativeObservation, ObservedApplication, ObservedWindow};
pub use reconcile::{
    Decision, Placement, Reconciliation, ReconciliationEvidence, Settings, reconcile,
};
