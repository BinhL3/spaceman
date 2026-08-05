use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Engine, PhysicalWindowId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WorkspaceId(u8);

impl WorkspaceId {
    pub const COUNT: usize = 10;
    pub const ONE: Self = Self(1);
    pub const SIX: Self = Self(6);

    pub fn new(value: u8) -> Result<Self, String> {
        (1..=Self::COUNT as u8)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| {
                format!(
                    "workspace must be between 1 and {}, got {value}",
                    Self::COUNT
                )
            })
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (1..=Self::COUNT).map(|index| Self(u8::try_from(index).expect("workspace count fits u8")))
    }

    #[must_use]
    pub(crate) const fn index(self) -> usize {
        (self.0 - 1) as usize
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DisplayRole {
    Main,
    Secondary,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DisplaySelector {
    Main,
    Secondary,
    Preferred(String),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceState {
    active: Vec<(DisplaySelector, WorkspaceId)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    previous: Vec<(DisplaySelector, WorkspaceId)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    axes: Vec<crate::Axis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    layouts: Vec<crate::layout_tree::LayoutTree>,
    slots: Vec<Vec<crate::Slot>>,
}

impl WorkspaceState {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.slots.len() > WorkspaceId::COUNT
            || self.layouts.len() > WorkspaceId::COUNT
            || self.axes.len() > WorkspaceId::COUNT
        {
            return Err("workspace state contains more than 10 workspaces".into());
        }
        for (index, slots) in self.slots.iter().enumerate() {
            let next_slot_id = slots
                .iter()
                .map(|slot| slot.id.0)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("workspace slot ID cannot be incremented")?;
            crate::Model {
                workspace: crate::model::Workspace {
                    axis: self.axes.get(index).copied().unwrap_or_default(),
                    slots: slots.clone(),
                },
                continuity: HashMap::new(),
                next_slot_id,
            }
            .validate()
            .map_err(|error| format!("workspace {}: {error}", index + 1))?;
            if let Some(layout) = self.layouts.get(index) {
                layout
                    .validate(slots)
                    .map_err(|error| format!("workspace {}: {error}", index + 1))?;
            }
        }
        if self.layouts.len() > self.slots.len() {
            return Err("workspace layouts have no matching slot state".into());
        }
        Ok(())
    }
}

impl DisplaySelector {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "main" => Self::Main,
            "secondary" => Self::Secondary,
            value => Self::Preferred(value.to_owned()),
        }
    }
}

impl From<DisplayRole> for DisplaySelector {
    fn from(value: DisplayRole) -> Self {
        match value {
            DisplayRole::Main => Self::Main,
            DisplayRole::Secondary => Self::Secondary,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Workspaces {
    engines: [Engine; WorkspaceId::COUNT],
    owners: HashMap<PhysicalWindowId, WorkspaceId>,
    assignments: [DisplaySelector; WorkspaceId::COUNT],
    active: HashMap<DisplaySelector, WorkspaceId>,
    previous: HashMap<DisplaySelector, WorkspaceId>,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            engines: std::array::from_fn(|_| Engine::default()),
            owners: HashMap::new(),
            assignments: std::array::from_fn(|index| {
                if index < 5 {
                    DisplaySelector::Main
                } else {
                    DisplaySelector::Secondary
                }
            }),
            active: HashMap::from([
                (DisplaySelector::Main, WorkspaceId::ONE),
                (DisplaySelector::Secondary, WorkspaceId::SIX),
            ]),
            previous: HashMap::new(),
        }
    }
}

impl Workspaces {
    #[must_use]
    pub(crate) fn state(&self) -> WorkspaceState {
        let mut active: Vec<_> = self
            .active
            .iter()
            .map(|(selector, workspace)| (selector.clone(), *workspace))
            .collect();
        active.sort();
        let mut previous: Vec<_> = self
            .previous
            .iter()
            .map(|(selector, workspace)| (selector.clone(), *workspace))
            .collect();
        previous.sort();
        WorkspaceState {
            active,
            previous,
            axes: Vec::new(),
            layouts: self
                .engines
                .iter()
                .map(|engine| engine.layout().clone())
                .collect(),
            slots: self
                .engines
                .iter()
                .map(|engine| engine.model().workspace.slots.clone())
                .collect(),
        }
    }

    pub(crate) fn restore(&mut self, state: WorkspaceState) {
        let WorkspaceState {
            active,
            previous,
            axes,
            layouts,
            slots,
        } = state;
        for (index, slots) in slots.into_iter().take(WorkspaceId::COUNT).enumerate() {
            let workspace = WorkspaceId::new(u8::try_from(index + 1).expect("workspace fits u8"))
                .expect("persisted workspace index is valid");
            for slot in slots {
                self.engine_mut(workspace).insert_slot(slot);
            }
            let layout = layouts.get(index).cloned().unwrap_or_else(|| {
                crate::layout_tree::LayoutTree::weighted_flat(
                    axes.get(index).copied().unwrap_or_default(),
                    self.engine(workspace).model().workspace.slots.iter(),
                )
            });
            self.engine_mut(workspace).restore_layout(layout);
            self.refresh_ownership(workspace);
        }
        for (selector, workspace) in active {
            if self.assignment(workspace) == &selector {
                self.active.insert(selector, workspace);
            }
        }
        for (selector, workspace) in previous {
            if self.assignment(workspace) == &selector
                && self.active_for(&selector) != Some(workspace)
            {
                self.previous.insert(selector, workspace);
            }
        }
    }

    #[must_use]
    pub fn active(&self, display: DisplayRole) -> WorkspaceId {
        self.active_for(&display.into()).unwrap_or(match display {
            DisplayRole::Main => WorkspaceId::ONE,
            DisplayRole::Secondary => WorkspaceId::SIX,
        })
    }

    #[must_use]
    pub fn assignment(&self, workspace: WorkspaceId) -> &DisplaySelector {
        &self.assignments[workspace.index()]
    }

    #[must_use]
    pub fn active_for(&self, display: &DisplaySelector) -> Option<WorkspaceId> {
        self.active.get(display).copied()
    }

    #[must_use]
    pub fn active_workspace(&self, workspace: WorkspaceId) -> Option<WorkspaceId> {
        self.active_for(self.assignment(workspace))
    }

    pub fn active_lanes(&self) -> impl Iterator<Item = (&DisplaySelector, WorkspaceId)> {
        self.active
            .iter()
            .map(|(display, workspace)| (display, *workspace))
    }

    pub fn set_assignment(&mut self, workspace: WorkspaceId, display: DisplaySelector) {
        let previous = self.assignments[workspace.index()].clone();
        if previous == display {
            return;
        }
        self.assignments[workspace.index()] = display.clone();
        self.active.entry(display).or_insert(workspace);
        if self.active.get(&previous) == Some(&workspace) {
            if let Some(replacement) =
                WorkspaceId::all().find(|candidate| self.assignment(*candidate) == &previous)
            {
                self.active.insert(previous, replacement);
            } else {
                self.active.remove(&previous);
            }
        }
    }

    pub fn switch(&mut self, workspace: WorkspaceId) -> WorkspaceId {
        let display = self.assignment(workspace).clone();
        let active = self.active_for(&display).unwrap_or(workspace);
        if active != workspace {
            self.previous.insert(display.clone(), active);
            self.active.insert(display, workspace);
        }
        active
    }

    #[must_use]
    pub fn previous_for(&self, display: &DisplaySelector) -> Option<WorkspaceId> {
        self.previous.get(display).copied()
    }

    #[must_use]
    pub fn engine(&self, workspace: WorkspaceId) -> &Engine {
        &self.engines[workspace.index()]
    }

    pub fn engine_mut(&mut self, workspace: WorkspaceId) -> &mut Engine {
        &mut self.engines[workspace.index()]
    }

    pub fn move_window(
        &mut self,
        window: &PhysicalWindowId,
        target: WorkspaceId,
    ) -> Option<WorkspaceId> {
        let source = self.workspace_of(window)?;
        let source_index = source.index();
        let target_index = target.index();
        if source_index == target_index {
            return None;
        }
        let (source_engine, target_engine) = if source_index < target_index {
            let (before, after) = self.engines.split_at_mut(target_index);
            (&mut before[source_index], &mut after[0])
        } else {
            let (before, after) = self.engines.split_at_mut(source_index);
            (&mut after[0], &mut before[target_index])
        };
        let slot = source_engine.take_slot(window)?;
        target_engine.insert_slot(slot);
        self.owners.insert(window.clone(), target);
        Some(source)
    }

    #[must_use]
    pub fn workspace_of(&self, window: &PhysicalWindowId) -> Option<WorkspaceId> {
        if let Some(workspace) = self.owners.get(window) {
            return Some(*workspace);
        }
        self.engines
            .iter()
            .position(|engine| {
                engine
                    .model()
                    .workspace
                    .slots
                    .iter()
                    .any(|slot| slot.bound_window() == Some(window))
            })
            .and_then(|index| WorkspaceId::new(u8::try_from(index + 1).ok()?).ok())
    }

    #[must_use]
    pub fn workspace_for_app(&self, app: &crate::AppInstanceId) -> Option<WorkspaceId> {
        let mut matches = WorkspaceId::all().filter(|workspace| {
            self.engine(*workspace)
                .model()
                .workspace
                .slots
                .iter()
                .filter_map(crate::Slot::bound_window)
                .any(|window| &window.app == app)
        });
        let workspace = matches.next()?;
        matches.next().is_none().then_some(workspace)
    }

    #[must_use]
    pub fn owned_by_other(&self, workspace: WorkspaceId, window: &PhysicalWindowId) -> bool {
        self.owners
            .get(window)
            .copied()
            .is_some_and(|owner| owner != workspace)
    }

    pub fn refresh_ownership(&mut self, workspace: WorkspaceId) {
        self.owners.retain(|_, owner| *owner != workspace);
        for window in self.engine(workspace).bound_windows() {
            self.owners.insert(window, workspace);
        }
    }

    pub fn forget_window(&mut self, workspace: WorkspaceId, window: &PhysicalWindowId) {
        let _ = self.engine_mut(workspace).take_slot(window);
        self.owners.remove(window);
    }

    pub fn inactive_bound_windows(
        &self,
    ) -> impl Iterator<Item = (WorkspaceId, PhysicalWindowId)> + '_ {
        let visible: HashSet<_> = self.active.values().copied().collect();
        self.inactive_bound_windows_except(visible)
    }

    pub fn inactive_bound_windows_except(
        &self,
        visible: HashSet<WorkspaceId>,
    ) -> impl Iterator<Item = (WorkspaceId, PhysicalWindowId)> + '_ {
        self.engines
            .iter()
            .enumerate()
            .filter_map(move |(index, engine)| {
                let workspace = WorkspaceId::new(u8::try_from(index + 1).ok()?).ok()?;
                (!visible.contains(&workspace)).then_some((workspace, engine))
            })
            .flat_map(|(workspace, engine)| {
                engine
                    .bound_windows()
                    .into_iter()
                    .map(move |window| (workspace, window))
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppInstanceId, BindingState, Slot, SlotId};

    use super::*;

    fn window(id: u32) -> PhysicalWindowId {
        PhysicalWindowId {
            app: AppInstanceId {
                pid: 1,
                launched_at: 1,
            },
            window_id: id,
        }
    }

    #[test]
    fn workspace_range_and_default_assignment_match_aerospace() {
        assert!(WorkspaceId::new(0).is_err());
        assert!(WorkspaceId::new(11).is_err());
        assert_eq!(
            Workspaces::default().assignment(WorkspaceId::new(5).unwrap()),
            &DisplaySelector::Main
        );
        assert_eq!(
            Workspaces::default().assignment(WorkspaceId::new(6).unwrap()),
            &DisplaySelector::Secondary
        );
    }

    #[test]
    fn switching_one_display_preserves_the_other() {
        let mut workspaces = Workspaces::default();
        workspaces.switch(WorkspaceId::new(3).unwrap());
        assert_eq!(workspaces.active(DisplayRole::Main).number(), 3);
        assert_eq!(workspaces.active(DisplayRole::Secondary).number(), 6);
    }

    #[test]
    fn back_and_forth_history_is_independent_per_display() {
        let mut workspaces = Workspaces::default();
        let main_target = WorkspaceId::new(3).unwrap();
        let secondary_target = WorkspaceId::new(8).unwrap();

        workspaces.switch(main_target);
        workspaces.switch(secondary_target);

        assert_eq!(
            workspaces.previous_for(&DisplaySelector::Main),
            Some(WorkspaceId::ONE)
        );
        assert_eq!(
            workspaces.previous_for(&DisplaySelector::Secondary),
            Some(WorkspaceId::SIX)
        );
        workspaces.switch(WorkspaceId::ONE);
        assert_eq!(
            workspaces.previous_for(&DisplaySelector::Main),
            Some(main_target)
        );
        assert_eq!(
            workspaces.previous_for(&DisplaySelector::Secondary),
            Some(WorkspaceId::SIX)
        );
    }

    #[test]
    fn switching_to_the_active_workspace_preserves_history() {
        let mut workspaces = Workspaces::default();
        let target = WorkspaceId::new(3).unwrap();
        workspaces.switch(target);

        workspaces.switch(target);

        assert_eq!(
            workspaces.previous_for(&DisplaySelector::Main),
            Some(WorkspaceId::ONE)
        );
    }

    #[test]
    fn persisted_state_restores_active_workspaces_and_tile_order() {
        let first = window(10);
        let second = window(20);
        let mut workspaces = Workspaces::default();
        workspaces.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
            id: SlotId(1),
            weight: 1,
            binding: BindingState::Bound(first.clone()),
        });
        workspaces.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
            id: SlotId(2),
            weight: 1,
            binding: BindingState::Bound(second.clone()),
        });
        workspaces.refresh_ownership(WorkspaceId::ONE);
        workspaces.engine_mut(WorkspaceId::ONE).toggle_axis();
        let third = WorkspaceId::new(3).unwrap();
        workspaces.switch(third);

        let encoded = serde_json::to_vec(&workspaces.state()).unwrap();
        let state = serde_json::from_slice(&encoded).unwrap();
        let mut restored = Workspaces::default();
        restored.restore(state);

        assert_eq!(restored.active(DisplayRole::Main), third);
        assert_eq!(
            restored.previous_for(&DisplaySelector::Main),
            Some(WorkspaceId::ONE)
        );
        assert_eq!(
            restored.engine(WorkspaceId::ONE).model().workspace.axis,
            crate::Axis::Vertical
        );
        assert_eq!(
            restored.engine(WorkspaceId::ONE).bound_windows(),
            vec![first.clone(), second]
        );
        assert_eq!(restored.workspace_of(&first), Some(WorkspaceId::ONE));
    }

    #[test]
    fn nested_layout_round_trips_without_flattening() {
        let first = window(10);
        let second = window(20);
        let third = window(30);
        let mut workspaces = Workspaces::default();
        for (id, window) in [(1, &first), (2, &second), (3, &third)] {
            workspaces.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
                id: SlotId(id),
                weight: 1,
                binding: BindingState::Bound(window.clone()),
            });
        }
        assert!(
            workspaces
                .engine_mut(WorkspaceId::ONE)
                .create_implicit_split(&second, crate::Direction::Up)
        );

        let expected = workspaces.engine(WorkspaceId::ONE).preview(crate::Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 800,
        });
        let encoded = serde_json::to_value(workspaces.state()).unwrap();
        assert!(encoded.get("layouts").is_some());
        assert!(encoded.get("axes").is_none());
        let mut restored = Workspaces::default();
        restored.restore(serde_json::from_value(encoded).unwrap());

        assert_eq!(
            restored.engine(WorkspaceId::ONE).preview(crate::Rect {
                x: 0,
                y: 0,
                width: 1200,
                height: 800,
            }),
            expected
        );
    }

    #[test]
    fn persisted_state_rejects_layout_with_wrong_slot_membership() {
        let mut state = Workspaces::default().state();
        state.slots[0].push(Slot {
            id: SlotId(1),
            weight: 1,
            binding: BindingState::Bound(window(10)),
        });

        assert!(state.validate().is_err());
    }

    #[test]
    fn persisted_state_rejects_zero_weight_slots() {
        let mut state = Workspaces::default().state();
        state.slots[0].push(Slot {
            id: SlotId(1),
            weight: 0,
            binding: BindingState::Bound(window(10)),
        });
        state.layouts[0] =
            crate::layout_tree::LayoutTree::flat(crate::Axis::Horizontal, [SlotId(1)]);

        assert!(state.validate().is_err());
    }

    #[test]
    fn legacy_flat_state_migrates_to_a_root_split() {
        let first = window(10);
        let state = serde_json::json!({
            "active": [],
            "axes": ["vertical"],
            "slots": [[{
                "id": 7,
                "weight": 3,
                "binding": {
                    "state": "bound",
                    "app": { "pid": 1, "launched_at": 1 },
                    "window_id": 10
                }
            }]]
        });
        let mut workspaces = Workspaces::default();

        workspaces.restore(serde_json::from_value(state).unwrap());

        assert_eq!(
            workspaces.engine(WorkspaceId::ONE).root_axis(),
            crate::Axis::Vertical
        );
        assert_eq!(
            workspaces.engine(WorkspaceId::ONE).bound_windows(),
            vec![first]
        );
    }

    #[test]
    fn preferred_monitor_creates_an_independent_active_lane() {
        let mut workspaces = Workspaces::default();
        let ninth = WorkspaceId::new(9).unwrap();

        workspaces.set_assignment(ninth, DisplaySelector::Preferred("third-id".into()));

        assert_eq!(
            workspaces.active_for(&DisplaySelector::Main),
            Some(WorkspaceId::ONE)
        );
        assert_eq!(
            workspaces.active_for(&DisplaySelector::Secondary),
            Some(WorkspaceId::SIX)
        );
        assert_eq!(
            workspaces.active_for(&DisplaySelector::Preferred("third-id".into())),
            Some(ninth)
        );
    }

    #[test]
    fn moving_preserves_the_logical_slot() {
        let mut workspaces = Workspaces::default();
        let window = window(10);
        let slot = Slot {
            id: SlotId(7),
            weight: 3,
            binding: BindingState::Bound(window.clone()),
        };
        workspaces.engine_mut(WorkspaceId::ONE).insert_slot(slot);

        assert_eq!(
            workspaces.move_window(&window, WorkspaceId::new(3).unwrap()),
            Some(WorkspaceId::ONE)
        );
        assert!(
            workspaces
                .engine(WorkspaceId::ONE)
                .model()
                .workspace
                .slots
                .is_empty()
        );
        let moved = &workspaces
            .engine(WorkspaceId::new(3).unwrap())
            .model()
            .workspace
            .slots[0];
        assert_eq!((moved.id, moved.weight), (SlotId(7), 3));
        assert_eq!(moved.bound_window(), Some(&window));
    }

    #[test]
    fn moving_into_a_populated_workspace_avoids_slot_id_collisions() {
        let mut workspaces = Workspaces::default();
        let source = window(10);
        let existing = window(20);
        workspaces.engine_mut(WorkspaceId::ONE).insert_slot(Slot {
            id: SlotId(1),
            weight: 1,
            binding: BindingState::Bound(source.clone()),
        });
        let target = WorkspaceId::new(2).unwrap();
        workspaces.engine_mut(target).insert_slot(Slot {
            id: SlotId(1),
            weight: 1,
            binding: BindingState::Bound(existing),
        });

        workspaces.move_window(&source, target).unwrap();
        let ids: std::collections::HashSet<_> = workspaces
            .engine(target)
            .model()
            .workspace
            .slots
            .iter()
            .map(|slot| slot.id)
            .collect();
        assert_eq!(ids.len(), 2);
        workspaces.engine(target).model().validate().unwrap();
    }
}
