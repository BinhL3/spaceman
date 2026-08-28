use std::{collections::BTreeMap, ffi::c_void, ptr, sync::mpsc::Sender};

use crate::{
    Direction, ResizeDirection, WorkspaceId,
    app::Action,
    keyboard_layout::{LayoutFingerprint, LayoutSnapshot, LogicalKey, SemanticKey},
};

const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOTKEY_PRESSED: u32 = 5;
const OPTION_KEY: u32 = 1 << 11;
const SHIFT_KEY: u32 = 1 << 9;
const SIGNATURE: u32 = u32::from_be_bytes(*b"SPMN");

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type EventHandler = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandler,
        event_type_count: u32,
        event_types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut *mut c_void,
    ) -> i32;
    fn RemoveEventHandler(handler_ref: *mut c_void) -> i32;
    fn GetApplicationEventTarget() -> *mut c_void;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hotkey_id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        hotkey_ref: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey_ref: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

pub struct Hotkeys {
    registrations: Vec<*mut c_void>,
    handler: *mut c_void,
    context: Option<Box<Sender<Action>>>,
    layout_fingerprint: LayoutFingerprint,
}

#[derive(Debug)]
pub(crate) struct HotkeyPlan {
    registrations: Vec<Registration>,
    layout_fingerprint: LayoutFingerprint,
}

#[derive(Clone, Debug)]
struct Registration {
    key_code: u32,
    modifiers: u32,
    id: u32,
    description: String,
}

impl HotkeyPlan {
    pub(crate) fn current(enabled: &[WorkspaceId]) -> Result<Self, String> {
        let layout = LayoutSnapshot::current().map_err(|error| error.to_string())?;
        Self::from_layout(enabled, &layout)
    }

    fn from_layout(enabled: &[WorkspaceId], layout: &LayoutSnapshot) -> Result<Self, String> {
        let mut registrations = Vec::with_capacity(33 + enabled.len() * 2);
        let claimed_letters = enabled
            .iter()
            .filter_map(|workspace| workspace.letter().map(|letter| letter.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        for workspace in WorkspaceId::all().filter(|workspace| enabled.contains(workspace)) {
            let (key, switch_id, move_id) = match (workspace.number(), workspace.letter()) {
                (Some(number), _) => (
                    LogicalKey::Semantic(SemanticKey::Workspace(number)),
                    u32::from(number),
                    u32::from(number) + 10,
                ),
                (_, Some(letter)) => {
                    let offset = u32::from(letter as u8 - b'A');
                    (
                        LogicalKey::Character(letter.to_ascii_lowercase()),
                        200 + offset,
                        300 + offset,
                    )
                }
                _ => unreachable!("workspace ID is always valid"),
            };
            let key = layout.resolve(key).map_err(|error| error.to_string())?;
            registrations.push(registration(
                key,
                OPTION_KEY,
                switch_id,
                format!("switch workspace {workspace}"),
            ));
            registrations.push(registration(
                key,
                OPTION_KEY | SHIFT_KEY,
                move_id,
                format!("move window to workspace {workspace}"),
            ));
        }
        for (character, id, direction) in [
            ('h', 104, "left"),
            ('j', 105, "down"),
            ('k', 106, "up"),
            ('l', 107, "right"),
        ] {
            if claimed_letters.contains(&character) {
                continue;
            }
            let key = layout
                .resolve(LogicalKey::Character(character))
                .map_err(|error| error.to_string())?;
            registrations.push(registration(
                key,
                OPTION_KEY,
                id,
                format!("focus {direction}"),
            ));
            registrations.push(registration(
                key,
                OPTION_KEY | SHIFT_KEY,
                id + 10,
                format!("move window {direction}"),
            ));
        }
        for (key, modifiers, id, description) in [
            (
                LogicalKey::Character('f'),
                OPTION_KEY | SHIFT_KEY,
                103,
                "toggle fullscreen layout",
            ),
            (LogicalKey::Character('/'), OPTION_KEY, 108, "toggle layout"),
            (
                LogicalKey::Semantic(SemanticKey::Tab),
                OPTION_KEY,
                109,
                "workspace back-and-forth",
            ),
            (LogicalKey::Character('-'), OPTION_KEY, 110, "shrink layout"),
            (LogicalKey::Character('='), OPTION_KEY, 111, "grow layout"),
        ] {
            if matches!(key, LogicalKey::Character(character) if claimed_letters.contains(&character))
            {
                continue;
            }
            let key = layout.resolve(key).map_err(|error| error.to_string())?;
            registrations.push(registration(key, modifiers, id, description.into()));
        }

        reject_collisions(&registrations)?;
        Ok(Self {
            registrations,
            layout_fingerprint: layout.fingerprint().clone(),
        })
    }

    pub(crate) fn matches(&self, hotkeys: &Hotkeys) -> bool {
        self.layout_fingerprint == hotkeys.layout_fingerprint
    }
}

fn registration(
    key: crate::keyboard_layout::ResolvedKey,
    semantic_modifiers: u32,
    id: u32,
    description: String,
) -> Registration {
    Registration {
        key_code: key.key_code,
        modifiers: semantic_modifiers | if key.shift { SHIFT_KEY } else { 0 },
        id,
        description,
    }
}

fn reject_collisions(registrations: &[Registration]) -> Result<(), String> {
    let mut occupied = BTreeMap::new();
    for registration in registrations {
        if let Some(previous) = occupied.insert(
            (registration.key_code, registration.modifiers),
            registration.description.as_str(),
        ) {
            return Err(format!(
                "keyboard layout maps {previous} and {} to virtual key {:#04x} with modifiers {:#06x}",
                registration.description, registration.key_code, registration.modifiers
            ));
        }
    }
    Ok(())
}

impl Hotkeys {
    pub fn install(sender: Sender<Action>, plan: HotkeyPlan) -> Result<Self, String> {
        let context = Box::new(sender);
        let context_pointer = (&raw const *context).cast_mut();
        let event_type = EventTypeSpec {
            event_class: EVENT_CLASS_KEYBOARD,
            event_kind: EVENT_HOTKEY_PRESSED,
        };
        let mut handler = ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                hotkey_handler,
                1,
                &raw const event_type,
                context_pointer.cast(),
                &raw mut handler,
            )
        };
        if status != 0 {
            return Err(format!("failed to install hotkey handler ({status})"));
        }

        let mut hotkeys = Self {
            registrations: Vec::with_capacity(plan.registrations.len()),
            handler,
            context: Some(context),
            layout_fingerprint: plan.layout_fingerprint,
        };
        for registration in plan.registrations {
            hotkeys.register(
                registration.key_code,
                registration.modifiers,
                registration.id,
            )?;
        }
        Ok(hotkeys)
    }

    pub fn matches_current_layout(&self) -> Result<bool, String> {
        LayoutSnapshot::current_fingerprint()
            .map(|fingerprint| self.layout_fingerprint == fingerprint)
            .map_err(|error| error.to_string())
    }

    fn register(&mut self, key_code: u32, modifiers: u32, id: u32) -> Result<(), String> {
        let mut registration = ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                key_code,
                modifiers,
                EventHotKeyId {
                    signature: SIGNATURE,
                    id,
                },
                GetApplicationEventTarget(),
                0,
                &raw mut registration,
            )
        };
        if status != 0 {
            return Err(format!("hotkey is unavailable ({status})"));
        }
        self.registrations.push(registration);
        Ok(())
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            unsafe { UnregisterEventHotKey(registration) };
        }
        let removed = unsafe { RemoveEventHandler(self.handler) };
        if removed != 0
            && let Some(context) = self.context.take()
        {
            Box::leak(context);
        }
    }
}

unsafe extern "C" fn hotkey_handler(
    _: *mut c_void,
    event: *mut c_void,
    user_data: *mut c_void,
) -> i32 {
    let sender = unsafe { &*user_data.cast::<Sender<Action>>() };
    let mut hotkey_id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    let status = unsafe {
        GetEventParameter(
            event,
            u32::from_be_bytes(*b"----"),
            u32::from_be_bytes(*b"hkid"),
            ptr::null_mut(),
            u32::try_from(std::mem::size_of::<EventHotKeyId>()).unwrap_or_default(),
            ptr::null_mut(),
            (&raw mut hotkey_id).cast(),
        )
    };
    if status == 0
        && hotkey_id.signature == SIGNATURE
        && let Some(action) = decode_action(hotkey_id.id)
    {
        let _ = sender.send(action);
    }
    0
}

fn decode_action(id: u32) -> Option<Action> {
    if id == 103 {
        return Some(Action::ToggleFullscreenLayout);
    }
    if id == 108 {
        return Some(Action::ToggleLayout);
    }
    if id == 109 {
        return Some(Action::WorkspaceBackAndForth);
    }
    if id == 110 {
        return Some(Action::Resize(ResizeDirection::Shrink));
    }
    if id == 111 {
        return Some(Action::Resize(ResizeDirection::Grow));
    }
    let (reorder, direction_id) = if (114..=117).contains(&id) {
        (true, id - 10)
    } else {
        (false, id)
    };
    let direction = match direction_id {
        104 => Direction::Left,
        105 => Direction::Down,
        106 => Direction::Up,
        107 => Direction::Right,
        _ => return decode_workspace_action(id),
    };
    if reorder {
        Some(Action::Reorder(direction))
    } else {
        Some(Action::Focus(direction))
    }
}

fn decode_workspace_action(id: u32) -> Option<Action> {
    let (move_window, workspace) = match id {
        1..=10 => (false, WorkspaceId::new(u8::try_from(id).ok()?).ok()?),
        11..=20 => (true, WorkspaceId::new(u8::try_from(id - 10).ok()?).ok()?),
        200..=225 => (
            false,
            WorkspaceId::named(char::from(b'A' + u8::try_from(id - 200).ok()?)).ok()?,
        ),
        300..=325 => (
            true,
            WorkspaceId::named(char::from(b'A' + u8::try_from(id - 300).ok()?)).ok()?,
        ),
        _ => return None,
    };
    Some(if move_window {
        Action::MoveFocused(workspace)
    } else {
        Action::SwitchWorkspace(workspace)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> LayoutSnapshot {
        LayoutSnapshot::for_test(
            vec![
                (60, 'h', false, false),
                (61, 'j', false, false),
                (62, 'k', false, false),
                (63, 'l', false, false),
                (64, 'f', false, false),
                (65, '/', false, false),
                (66, '-', false, false),
                (67, '=', false, false),
            ],
            vec![],
        )
    }

    #[test]
    fn decodes_switch_and_move_hotkeys_including_zero_key() {
        assert!(matches!(
            decode_action(1),
            Some(Action::SwitchWorkspace(id)) if id.number() == Some(1)
        ));
        assert!(matches!(
            decode_action(103),
            Some(Action::ToggleFullscreenLayout)
        ));
        assert!(matches!(decode_action(108), Some(Action::ToggleLayout)));
        assert!(matches!(
            decode_action(109),
            Some(Action::WorkspaceBackAndForth)
        ));
        assert!(matches!(
            decode_action(110),
            Some(Action::Resize(ResizeDirection::Shrink))
        ));
        assert!(matches!(
            decode_action(111),
            Some(Action::Resize(ResizeDirection::Grow))
        ));
        assert!(matches!(
            decode_action(104),
            Some(Action::Focus(Direction::Left))
        ));
        assert!(matches!(
            decode_action(107),
            Some(Action::Focus(Direction::Right))
        ));
        assert!(matches!(
            decode_action(114),
            Some(Action::Reorder(Direction::Left))
        ));
        assert!(matches!(
            decode_action(117),
            Some(Action::Reorder(Direction::Right))
        ));
        assert!(matches!(
            decode_action(10),
            Some(Action::SwitchWorkspace(id)) if id.number() == Some(10)
        ));
        assert!(matches!(
            decode_action(20),
            Some(Action::MoveFocused(id)) if id.number() == Some(10)
        ));
    }

    #[test]
    fn full_plan_contains_all_thirty_three_registrations() {
        let enabled = WorkspaceId::numbered().collect::<Vec<_>>();
        let plan = HotkeyPlan::from_layout(&enabled, &layout()).unwrap();
        assert_eq!(plan.registrations.len(), 33);
        assert!(plan.registrations.capacity() >= 33);
    }

    #[test]
    fn named_workspace_claims_its_letter_shortcuts() {
        let mut enabled = WorkspaceId::numbered().collect::<Vec<_>>();
        enabled.push(WorkspaceId::named('H').unwrap());
        let plan = HotkeyPlan::from_layout(&enabled, &layout()).unwrap();
        assert!(
            plan.registrations
                .iter()
                .any(|registration| registration.id == 207)
        );
        assert!(
            plan.registrations
                .iter()
                .any(|registration| registration.id == 307)
        );
        assert!(
            !plan
                .registrations
                .iter()
                .any(|registration| registration.id == 104)
        );
        assert!(
            !plan
                .registrations
                .iter()
                .any(|registration| registration.id == 114)
        );
        assert!(matches!(
            decode_action(207),
            Some(Action::SwitchWorkspace(workspace)) if workspace == WorkspaceId::named('H').unwrap()
        ));
    }

    #[test]
    fn layout_required_shift_is_composed_with_command_modifiers() {
        let layout = LayoutSnapshot::for_test(
            vec![
                (60, 'h', false, false),
                (61, 'j', false, false),
                (62, 'k', false, false),
                (63, 'l', false, false),
                (64, 'f', false, false),
                (65, '/', true, false),
                (66, '-', false, false),
                (67, '=', false, false),
            ],
            vec![],
        );
        let plan = HotkeyPlan::from_layout(&[], &layout).unwrap();
        let toggle_layout = plan
            .registrations
            .iter()
            .find(|registration| registration.id == 108)
            .unwrap();
        assert_eq!(toggle_layout.modifiers, OPTION_KEY | SHIFT_KEY);
    }

    #[test]
    fn duplicate_chords_are_rejected_before_registration() {
        let layout = LayoutSnapshot::for_test(
            vec![
                (60, 'h', false, false),
                (60, 'j', false, false),
                (62, 'k', false, false),
                (63, 'l', false, false),
                (64, 'f', false, false),
                (65, '/', false, false),
                (66, '-', false, false),
                (67, '=', false, false),
            ],
            vec![],
        );
        let error = HotkeyPlan::from_layout(&[], &layout).unwrap_err();
        assert!(error.contains("focus left and focus down"), "{error}");
    }

    #[test]
    fn unsupported_layout_cannot_produce_a_replacement_plan() {
        let non_latin_without_ascii_fallback = LayoutSnapshot::for_test(
            vec![
                (60, 'ж', false, false),
                (61, 'д', false, false),
                (62, 'л', false, false),
                (63, 'о', false, false),
            ],
            vec![],
        );

        let error = HotkeyPlan::from_layout(&[], &non_latin_without_ascii_fallback).unwrap_err();
        assert!(error.contains("does not produce 'h'"), "{error}");
    }
}
