use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{Engine, PhysicalWindowId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(u8);

impl WorkspaceId {
    pub const NUMBERED_COUNT: usize = 10;
    pub const COUNT: usize = Self::NUMBERED_COUNT + 26;
    pub const ONE: Self = Self(1);
    pub const SIX: Self = Self(6);

    pub fn new(value: u8) -> Result<Self, String> {
        (1..=Self::NUMBERED_COUNT as u8)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("workspace must be 1 through 10 or A through Z, got {value}"))
    }

    pub fn named(letter: char) -> Result<Self, String> {
        let letter = letter.to_ascii_uppercase();
        letter
            .is_ascii_uppercase()
            .then_some(Self(11 + (letter as u8 - b'A')))
            .ok_or_else(|| format!("workspace must be 1 through 10 or A through Z, got {letter:?}"))
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        value.parse::<u8>().ok().map(Self::new).unwrap_or_else(|| {
            let mut characters = value.chars();
            match (characters.next(), characters.next()) {
                (Some(letter), None) => Self::named(letter),
                _ => Err(format!(
                    "workspace must be 1 through 10 or A through Z, got {value:?}"
                )),
            }
        })
    }

    #[must_use]
    pub const fn number(self) -> Option<u8> {
        if self.0 <= Self::NUMBERED_COUNT as u8 {
            Some(self.0)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn letter(self) -> Option<char> {
        if self.0 > Self::NUMBERED_COUNT as u8 {
            Some((b'A' + self.0 - 11) as char)
        } else {
            None
        }
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (1..=Self::COUNT).map(|index| Self(u8::try_from(index).expect("workspace count fits u8")))
    }

    pub fn numbered() -> impl Iterator<Item = Self> {
        (1..=Self::NUMBERED_COUNT)
            .map(|index| Self(u8::try_from(index).expect("workspace count fits u8")))
    }

    pub(crate) fn from_index(index: usize) -> Self {
        Self(u8::try_from(index + 1).expect("workspace index fits u8"))
    }

    #[must_use]
    pub(crate) const fn index(self) -> usize {
        (self.0 - 1) as usize
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.number(), self.letter()) {
            (Some(number), _) => write!(formatter, "{number}"),
            (_, Some(letter)) => write!(formatter, "{letter}"),
            _ => unreachable!("workspace ID is always valid"),
        }
    }
}

impl FromStr for WorkspaceId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for WorkspaceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.number() {
            Some(number) => serializer.serialize_u8(number),
            None => serializer.serialize_char(self.letter().expect("named workspace has a letter")),
        }
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Number(u8),
            Name(String),
        }
        match Value::deserialize(deserializer)? {
            Value::Number(value) => Self::new(value),
            Value::Name(value) => Self::parse(&value),
        }
        .map_err(serde::de::Error::custom)
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    floating: Vec<(crate::PhysicalWindowId, WorkspaceId)>,
}

impl WorkspaceState {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.slots.len() > WorkspaceId::COUNT
            || self.layouts.len() > WorkspaceId::COUNT
            || self.axes.len() > WorkspaceId::COUNT
        {
            return Err("workspace state contains more than 36 workspaces".into());
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
        let tiled: HashSet<_> = self
            .slots
            .iter()
            .flatten()
            .filter_map(crate::Slot::bound_window)
            .collect();
        let mut floating = HashSet::new();
        for (window, _) in &self.floating {
            if tiled.contains(window) {
                return Err("window is both tiled and floating".into());
            }
            if !floating.insert(window) {
                return Err("floating window is owned by multiple workspaces".into());
            }
        }
        Ok(())
    }

    pub(crate) fn named_workspaces_in_use(&self) -> HashSet<WorkspaceId> {
        let mut used = self
            .active
            .iter()
            .chain(&self.previous)
            .map(|(_, workspace)| *workspace)
            .chain(self.floating.iter().map(|(_, workspace)| *workspace))
            .filter(|workspace| workspace.letter().is_some())
            .collect::<HashSet<_>>();
        for (index, slots) in self.slots.iter().enumerate() {
            let workspace = WorkspaceId::from_index(index);
            if workspace.letter().is_some() && !slots.is_empty() {
                used.insert(workspace);
            }
        }
        used
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
    floating_owners: HashMap<PhysicalWindowId, WorkspaceId>,
    enabled: HashSet<WorkspaceId>,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            engines: std::array::from_fn(|_| Engine::default()),
            owners: HashMap::new(),
            assignments: std::array::from_fn(|index| {
                if !(5..WorkspaceId::NUMBERED_COUNT).contains(&index) {
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
            floating_owners: HashMap::new(),
            enabled: WorkspaceId::numbered().collect(),
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
            floating: {
                let mut floating = self
                    .floating_entries()
                    .map(|(window, workspace)| (window.clone(), workspace))
                    .collect::<Vec<_>>();
                floating.sort();
                floating
            },
        }
    }

    pub(crate) fn restore(&mut self, state: WorkspaceState) {
        self.enabled.extend(state.named_workspaces_in_use());
        let WorkspaceState {
            active,
            previous,
            axes,
            layouts,
            slots,
            floating,
        } = state;
        for (index, slots) in slots.into_iter().take(WorkspaceId::COUNT).enumerate() {
            let workspace = WorkspaceId::from_index(index);
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
        for (window, workspace) in floating {
            if self.workspace_of(&window).is_none() {
                self.floating_owners.insert(window, workspace);
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
        self.enable(workspace);
        let previous = self.assignments[workspace.index()].clone();
        if previous == display {
            return;
        }
        self.assignments[workspace.index()] = display.clone();
        self.active.entry(display).or_insert(workspace);
        if self.active.get(&previous) == Some(&workspace) {
            if let Some(replacement) = WorkspaceId::all().find(|candidate| {
                self.is_enabled(*candidate) && self.assignment(*candidate) == &previous
            }) {
                self.active.insert(previous, replacement);
            } else {
                self.active.remove(&previous);
            }
        }
    }

    pub fn enable(&mut self, workspace: WorkspaceId) {
        self.enabled.insert(workspace);
    }

    #[must_use]
    pub fn is_enabled(&self, workspace: WorkspaceId) -> bool {
        self.enabled.contains(&workspace)
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
            .map(WorkspaceId::from_index)
    }

    #[must_use]
    pub(crate) fn floating_workspace_of(&self, window: &PhysicalWindowId) -> Option<WorkspaceId> {
        self.floating_owners.get(window).copied()
    }

    #[must_use]
    pub(crate) fn workspace_of_any(&self, window: &PhysicalWindowId) -> Option<WorkspaceId> {
        self.workspace_of(window)
            .or_else(|| self.floating_workspace_of(window))
    }

    pub(crate) fn own_floating(&mut self, window: PhysicalWindowId, workspace: WorkspaceId) {
        self.floating_owners.insert(window, workspace);
    }

    pub(crate) fn windows(&self, workspace: WorkspaceId) -> Vec<PhysicalWindowId> {
        let mut windows = self.engine(workspace).bound_windows();
        windows.extend(
            self.floating_entries()
                .filter(|(_, owner)| *owner == workspace)
                .map(|(window, _)| window.clone()),
        );
        windows
    }

    pub(crate) fn floating_windows_for_app(
        &self,
        app: &crate::AppInstanceId,
    ) -> Vec<PhysicalWindowId> {
        self.floating_entries()
            .filter(|(window, _)| &window.app == app)
            .map(|(window, _)| window.clone())
            .collect()
    }

    pub(crate) fn floating_entries(
        &self,
    ) -> impl Iterator<Item = (&PhysicalWindowId, WorkspaceId)> {
        self.floating_owners
            .iter()
            .map(|(window, workspace)| (window, *workspace))
    }

    pub(crate) fn forget_floating(&mut self, window: &PhysicalWindowId) {
        self.floating_owners.remove(window);
    }

    #[must_use]
    pub fn workspace_for_app(&self, app: &crate::AppInstanceId) -> Option<WorkspaceId> {
        let mut matches = WorkspaceId::all().filter(|workspace| {
            self.is_enabled(*workspace)
                && self
                    .engine(*workspace)
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

    pub(crate) fn inactive_windows_except(
        &self,
        visible: &HashSet<WorkspaceId>,
    ) -> Vec<(WorkspaceId, PhysicalWindowId)> {
        WorkspaceId::all()
            .filter(|workspace| self.is_enabled(*workspace) && !visible.contains(workspace))
            .flat_map(|workspace| {
                self.windows(workspace)
                    .into_iter()
                    .map(move |window| (workspace, window))
            })
            .collect()
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
            WorkspaceId::parse("d").unwrap(),
            WorkspaceId::named('D').unwrap()
        );
        assert_eq!(
            Workspaces::default().assignment(WorkspaceId::new(5).unwrap()),
            &DisplaySelector::Main
        );
        assert_eq!(
            Workspaces::default().assignment(WorkspaceId::new(6).unwrap()),
            &DisplaySelector::Secondary
        );
        assert!(!Workspaces::default().is_enabled(WorkspaceId::named('D').unwrap()));
    }

    #[test]
    fn workspace_identity_serializes_numbers_compatibly_and_names_as_letters() {
        assert_eq!(
            serde_json::to_value(WorkspaceId::ONE).unwrap(),
            serde_json::json!(1)
        );
        assert_eq!(
            serde_json::to_value(WorkspaceId::named('D').unwrap()).unwrap(),
            serde_json::json!("D")
        );
        assert_eq!(
            serde_json::from_value::<WorkspaceId>(serde_json::json!("D")).unwrap(),
            WorkspaceId::named('D').unwrap()
        );
    }

    #[test]
    fn switching_one_display_preserves_the_other() {
        let mut workspaces = Workspaces::default();
        workspaces.switch(WorkspaceId::new(3).unwrap());
        assert_eq!(workspaces.active(DisplayRole::Main).number(), Some(3));
        assert_eq!(workspaces.active(DisplayRole::Secondary).number(), Some(6));
    }

    #[test]
    fn switching_to_a_named_workspace_preserves_the_other_display() {
        let mut workspaces = Workspaces::default();
        let named = WorkspaceId::named('D').unwrap();
        workspaces.enable(named);

        workspaces.switch(named);

        assert_eq!(workspaces.active(DisplayRole::Main), named);
        assert_eq!(workspaces.active(DisplayRole::Secondary), WorkspaceId::SIX);
    }

    #[test]
    fn unconfigured_named_workspaces_are_not_display_fallbacks() {
        let mut workspaces = Workspaces::default();
        for workspace in WorkspaceId::numbered().take(5) {
            workspaces.set_assignment(workspace, DisplaySelector::Secondary);
        }

        assert_eq!(workspaces.active_for(&DisplaySelector::Main), None);
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
    fn persisted_state_round_trips_a_named_workspace() {
        let named = WorkspaceId::named('D').unwrap();
        let owned = window(40);
        let mut workspaces = Workspaces::default();
        workspaces.enable(named);
        workspaces.engine_mut(named).insert_slot(Slot {
            id: SlotId(1),
            weight: 1,
            binding: BindingState::Bound(owned.clone()),
        });
        workspaces.refresh_ownership(named);
        workspaces.switch(named);

        let encoded = serde_json::to_value(workspaces.state()).unwrap();
        let state: WorkspaceState = serde_json::from_value(encoded).unwrap();
        state.validate().unwrap();
        let mut restored = Workspaces::default();
        restored.restore(state);

        assert_eq!(restored.active(DisplayRole::Main), named);
        assert_eq!(restored.workspace_of(&owned), Some(named));
        assert!(restored.is_enabled(named));
    }

    #[test]
    fn persisted_state_restores_floating_window_ownership() {
        let floating = window(30);
        let workspace = WorkspaceId::new(3).unwrap();
        let mut workspaces = Workspaces::default();
        workspaces.own_floating(floating.clone(), workspace);

        let encoded = serde_json::to_vec(&workspaces.state()).unwrap();
        let state: WorkspaceState = serde_json::from_slice(&encoded).unwrap();
        state.validate().unwrap();
        let mut restored = Workspaces::default();
        restored.restore(state);

        assert_eq!(restored.floating_workspace_of(&floating), Some(workspace));
        assert_eq!(restored.workspace_of(&floating), None);
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
