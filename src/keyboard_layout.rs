use std::{ffi::c_void, fmt, slice};

const VIRTUAL_KEY_CODES: std::ops::Range<u16> = 0..128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticKey {
    Tab,
    Workspace(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogicalKey {
    Character(char),
    Semantic(SemanticKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LayoutFingerprint {
    selected_source: String,
    layout_source: String,
    ascii_source: String,
    primary_layout_hash: u64,
    ascii_layout_hash: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct LayoutSnapshot {
    fingerprint: LayoutFingerprint,
    primary: Vec<KeyOutput>,
    ascii_fallback: Vec<KeyOutput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyOutput {
    key_code: u16,
    character: char,
    shift: bool,
    dead: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedKey {
    pub(crate) key_code: u32,
    pub(crate) shift: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveError {
    InvalidSemanticKey(SemanticKey),
    MissingCharacter(char),
    System(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSemanticKey(key) => write!(formatter, "invalid semantic key {key:?}"),
            Self::MissingCharacter(character) => {
                write!(
                    formatter,
                    "current keyboard layout does not produce {character:?}"
                )
            }
            Self::System(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ResolveError {}

impl LayoutSnapshot {
    pub(crate) fn current() -> Result<Self, ResolveError> {
        macos::snapshot()
    }

    pub(crate) fn current_fingerprint() -> Result<LayoutFingerprint, ResolveError> {
        macos::fingerprint()
    }

    pub(crate) fn fingerprint(&self) -> &LayoutFingerprint {
        &self.fingerprint
    }

    pub(crate) fn resolve(&self, key: LogicalKey) -> Result<ResolvedKey, ResolveError> {
        match key {
            LogicalKey::Semantic(key) => semantic_key_code(key).map(|key_code| ResolvedKey {
                key_code,
                shift: false,
            }),
            LogicalKey::Character(character) => reverse_lookup(&self.primary, character)
                .or_else(|error| {
                    if matches!(error, ResolveError::MissingCharacter(_)) && character.is_ascii() {
                        reverse_lookup(&self.ascii_fallback, character)
                    } else {
                        Err(error)
                    }
                })
                .map(|output| ResolvedKey {
                    key_code: u32::from(output.key_code),
                    shift: output.shift,
                }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        primary: Vec<(u16, char, bool, bool)>,
        fallback: Vec<(u16, char, bool, bool)>,
    ) -> Self {
        let convert = |outputs: Vec<(u16, char, bool, bool)>| {
            outputs
                .into_iter()
                .map(|(key_code, character, shift, dead)| KeyOutput {
                    key_code,
                    character,
                    shift,
                    dead,
                })
                .collect()
        };
        Self {
            fingerprint: LayoutFingerprint {
                selected_source: "test".into(),
                layout_source: "test-layout".into(),
                ascii_source: "test-ascii".into(),
                primary_layout_hash: 1,
                ascii_layout_hash: 2,
            },
            primary: convert(primary),
            ascii_fallback: convert(fallback),
        }
    }
}

fn semantic_key_code(key: SemanticKey) -> Result<u32, ResolveError> {
    const WORKSPACE_NUMBER_ROW: [u32; 10] =
        [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d];

    match key {
        SemanticKey::Tab => Ok(0x30),
        // Workspace shortcuts intentionally identify physical number-row keys. Their
        // Option+Shift variants therefore remain usable on layouts where digits need Shift.
        SemanticKey::Workspace(number @ 1..=10) => Ok(WORKSPACE_NUMBER_ROW[number as usize - 1]),
        SemanticKey::Workspace(_) => Err(ResolveError::InvalidSemanticKey(key)),
    }
}

fn reverse_lookup(outputs: &[KeyOutput], character: char) -> Result<KeyOutput, ResolveError> {
    outputs
        .iter()
        .filter(|output| !output.dead && output.character == character)
        .filter(|output| !character.is_alphabetic() || !output.shift)
        .min_by_key(|output| output.shift)
        .cloned()
        .ok_or(ResolveError::MissingCharacter(character))
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    type InputSource = *const c_void;
    const UTF8_ENCODING: u32 = 0x0800_0100;
    const KEY_ACTION_DOWN: u16 = 0;
    const NO_DEAD_KEYS_MASK: u32 = 1;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;

        fn TISCopyCurrentKeyboardInputSource() -> InputSource;
        fn TISCopyCurrentKeyboardLayoutInputSource() -> InputSource;
        fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> InputSource;
        fn TISGetInputSourceProperty(source: InputSource, property: *const c_void)
        -> *const c_void;
        fn UCKeyTranslate(
            layout: *const c_void,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_string_length: u32,
            actual_string_length: *mut u32,
            unicode_string: *mut u16,
        ) -> i32;
        fn LMGetKbdType() -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFDataGetLength(data: *const c_void) -> isize;
        fn CFStringGetLength(string: *const c_void) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut i8,
            buffer_size: isize,
            encoding: u32,
        ) -> bool;
    }

    struct OwnedInputSource(InputSource);

    impl OwnedInputSource {
        fn copy(
            copy: unsafe extern "C" fn() -> InputSource,
            description: &str,
        ) -> Result<Self, ResolveError> {
            // TIS Copy functions follow the Core Foundation create rule. The non-null
            // reference remains alive until this wrapper releases it.
            let source = unsafe { copy() };
            if source.is_null() {
                Err(ResolveError::System(format!(
                    "macOS returned no {description}"
                )))
            } else {
                Ok(Self(source))
            }
        }

        fn id(&self) -> Result<String, ResolveError> {
            let value = unsafe { TISGetInputSourceProperty(self.0, kTISPropertyInputSourceID) };
            cf_string(value).ok_or_else(|| {
                ResolveError::System("keyboard input source has no valid identifier".into())
            })
        }

        fn layout_data(&self) -> Result<Option<(*const u8, usize)>, ResolveError> {
            // TIS owns the property value. Its CFData storage stays valid while this retained
            // input source is alive; callers consume the pointer before `self` can be dropped.
            let data =
                unsafe { TISGetInputSourceProperty(self.0, kTISPropertyUnicodeKeyLayoutData) };
            if data.is_null() {
                return Ok(None);
            }
            let length = unsafe { CFDataGetLength(data) };
            let layout = unsafe { CFDataGetBytePtr(data) };
            if length <= 0 || layout.is_null() {
                return Err(ResolveError::System(
                    "keyboard input source contains empty Unicode layout data".into(),
                ));
            }

            Ok(Some((
                layout,
                usize::try_from(length).map_err(|_| {
                    ResolveError::System("keyboard layout data is too large".into())
                })?,
            )))
        }

        fn outputs(&self) -> Result<Option<Vec<KeyOutput>>, ResolveError> {
            let Some((layout, _)) = self.layout_data()? else {
                return Ok(None);
            };
            let mut outputs = Vec::with_capacity(256);
            for key_code in VIRTUAL_KEY_CODES {
                for shift in [false, true] {
                    if let Some(output) = translate(layout.cast(), key_code, shift)? {
                        outputs.push(output);
                    }
                }
            }
            Ok(Some(outputs))
        }

        fn layout_hash(&self) -> Result<u64, ResolveError> {
            let Some((bytes, length)) = self.layout_data()? else {
                return Ok(0);
            };
            // `layout_data` validated the pointer and length, and the retained source owns the
            // backing CFData for this borrow.
            let bytes = unsafe { slice::from_raw_parts(bytes, length) };
            Ok(bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            }))
        }
    }

    impl Drop for OwnedInputSource {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    struct Sources {
        selected: OwnedInputSource,
        layout: OwnedInputSource,
        ascii: OwnedInputSource,
    }

    impl Sources {
        fn current() -> Result<Self, ResolveError> {
            Ok(Self {
                selected: OwnedInputSource::copy(
                    TISCopyCurrentKeyboardInputSource,
                    "selected keyboard input source",
                )?,
                layout: OwnedInputSource::copy(
                    TISCopyCurrentKeyboardLayoutInputSource,
                    "keyboard layout input source",
                )?,
                ascii: OwnedInputSource::copy(
                    TISCopyCurrentASCIICapableKeyboardLayoutInputSource,
                    "ASCII-capable keyboard layout input source",
                )?,
            })
        }

        fn fingerprint(&self) -> Result<LayoutFingerprint, ResolveError> {
            Ok(LayoutFingerprint {
                selected_source: self.selected.id()?,
                layout_source: self.layout.id()?,
                ascii_source: self.ascii.id()?,
                primary_layout_hash: match self.selected.layout_hash()? {
                    0 => self.layout.layout_hash()?,
                    hash => hash,
                },
                ascii_layout_hash: self.ascii.layout_hash()?,
            })
        }
    }

    pub(super) fn fingerprint() -> Result<LayoutFingerprint, ResolveError> {
        Sources::current()?.fingerprint()
    }

    pub(super) fn snapshot() -> Result<LayoutSnapshot, ResolveError> {
        let sources = Sources::current()?;
        let fingerprint = sources.fingerprint()?;
        // IMEs commonly expose no UCKeyboardLayout. In that case, macOS's current
        // keyboard-layout source is the authoritative layout beneath the IME.
        let primary = match sources.selected.outputs()? {
            Some(outputs) => outputs,
            None => sources.layout.outputs()?.ok_or_else(|| {
                ResolveError::System("current input sources expose no Unicode layout data".into())
            })?,
        };
        let ascii_fallback = sources.ascii.outputs()?.ok_or_else(|| {
            ResolveError::System("ASCII-capable input source exposes no Unicode layout data".into())
        })?;
        Ok(LayoutSnapshot {
            fingerprint,
            primary,
            ascii_fallback,
        })
    }

    fn translate(
        layout: *const c_void,
        key_code: u16,
        shift: bool,
    ) -> Result<Option<KeyOutput>, ResolveError> {
        let modifier_state = if shift { 2 } else { 0 };
        let no_dead_key = translate_raw(layout, key_code, modifier_state, NO_DEAD_KEYS_MASK)?;
        let normal = translate_raw(layout, key_code, modifier_state, 0)?;
        let Some(character) = no_dead_key.text else {
            return Ok(None);
        };
        Ok(Some(KeyOutput {
            key_code,
            character,
            shift,
            dead: normal.dead_key_state != 0,
        }))
    }

    struct Translation {
        text: Option<char>,
        dead_key_state: u32,
    }

    fn translate_raw(
        layout: *const c_void,
        key_code: u16,
        modifier_state: u32,
        options: u32,
    ) -> Result<Translation, ResolveError> {
        let mut dead_key_state = 0;
        let mut actual_length = 0;
        let mut utf16 = [0_u16; 16];
        let status = unsafe {
            UCKeyTranslate(
                layout,
                key_code,
                KEY_ACTION_DOWN,
                modifier_state,
                u32::from(LMGetKbdType()),
                options,
                &raw mut dead_key_state,
                utf16.len() as u32,
                &raw mut actual_length,
                utf16.as_mut_ptr(),
            )
        };
        if status != 0 {
            return Err(ResolveError::System(format!(
                "UCKeyTranslate failed for virtual key {key_code:#04x} ({status})"
            )));
        }
        let length = usize::try_from(actual_length)
            .ok()
            .filter(|length| *length <= utf16.len())
            .ok_or_else(|| ResolveError::System("UCKeyTranslate returned invalid length".into()))?;
        if length == 0 {
            return Ok(Translation {
                text: None,
                dead_key_state,
            });
        }
        let text = String::from_utf16(&utf16[..length]).map_err(|_| {
            ResolveError::System(format!(
                "UCKeyTranslate returned invalid UTF-16 for virtual key {key_code:#04x}"
            ))
        })?;
        Ok(Translation {
            text: one_scalar(&text),
            dead_key_state,
        })
    }

    fn cf_string(value: *const c_void) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let length = unsafe { CFStringGetLength(value) };
        let maximum = unsafe { CFStringGetMaximumSizeForEncoding(length, UTF8_ENCODING) };
        let capacity = maximum.checked_add(1)?;
        let mut bytes = vec![0_i8; usize::try_from(capacity).ok()?];
        if !unsafe { CFStringGetCString(value, bytes.as_mut_ptr(), capacity, UTF8_ENCODING) } {
            return None;
        }
        let byte_length = bytes.iter().position(|byte| *byte == 0)?;
        let bytes = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), byte_length) };
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }
}

fn one_scalar(text: &str) -> Option<char> {
    let mut characters = text.chars();
    let character = characters.next()?;
    characters.next().is_none().then_some(character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(key_code: u16, character: char) -> KeyOutput {
        KeyOutput {
            key_code,
            character,
            shift: false,
            dead: false,
        }
    }

    #[test]
    fn reverse_lookup_resolves_exact_unmodified_character() {
        let outputs = [output(4, 'h'), output(38, 'j')];
        assert_eq!(
            reverse_lookup(&outputs, 'j').map(|key| key.key_code),
            Ok(38)
        );
        assert_eq!(
            reverse_lookup(&outputs, 'H'),
            Err(ResolveError::MissingCharacter('H'))
        );
    }

    #[test]
    fn reverse_lookup_handles_unicode_strings_safely() {
        let outputs = [output(12, 'λ'), output(13, '😀')];
        assert_eq!(
            reverse_lookup(&outputs, 'λ').map(|key| key.key_code),
            Ok(12)
        );
        assert_eq!(
            reverse_lookup(&outputs, '😀').map(|key| key.key_code),
            Ok(13)
        );
    }

    #[test]
    fn translations_must_be_exactly_one_unicode_scalar() {
        assert_eq!(one_scalar("😀"), Some('😀'));
        assert_eq!(one_scalar(""), None);
        assert_eq!(one_scalar("ab"), None);
        assert_eq!(one_scalar("e\u{301}"), None);
    }

    #[test]
    fn semantic_keys_are_independent_of_character_outputs() {
        let snapshot = LayoutSnapshot {
            fingerprint: fingerprint("test"),
            primary: vec![output(99, '1'), output(98, '\t')],
            ascii_fallback: vec![],
        };
        assert_eq!(
            snapshot
                .resolve(LogicalKey::Semantic(SemanticKey::Workspace(1)))
                .unwrap()
                .key_code,
            0x12
        );
        assert_eq!(
            snapshot
                .resolve(LogicalKey::Semantic(SemanticKey::Tab))
                .unwrap()
                .key_code,
            0x30
        );
        assert_eq!(
            snapshot
                .resolve(LogicalKey::Character('1'))
                .unwrap()
                .key_code,
            99
        );
    }

    #[test]
    fn ascii_fallback_only_applies_when_primary_character_is_missing() {
        let snapshot = LayoutSnapshot {
            fingerprint: fingerprint("test"),
            primary: vec![output(1, 'ж'), output(2, '-')],
            ascii_fallback: vec![output(3, 'h'), output(4, '-')],
        };
        assert_eq!(
            snapshot
                .resolve(LogicalKey::Character('h'))
                .unwrap()
                .key_code,
            3
        );
        assert_eq!(
            snapshot
                .resolve(LogicalKey::Character('-'))
                .unwrap()
                .key_code,
            2
        );
        assert_eq!(
            snapshot.resolve(LogicalKey::Character('λ')),
            Err(ResolveError::MissingCharacter('λ'))
        );
    }

    #[test]
    fn punctuation_can_require_shift_but_letters_cannot() {
        let mut shifted_slash = output(9, '/');
        shifted_slash.shift = true;
        let mut shifted_h = output(10, 'h');
        shifted_h.shift = true;
        assert!(reverse_lookup(&[shifted_slash], '/').unwrap().shift);
        assert_eq!(
            reverse_lookup(&[shifted_h], 'h'),
            Err(ResolveError::MissingCharacter('h'))
        );
    }

    #[test]
    fn dead_key_candidates_are_rejected() {
        let mut dead = output(9, '=');
        dead.dead = true;
        assert_eq!(
            reverse_lookup(&[dead], '='),
            Err(ResolveError::MissingCharacter('='))
        );
    }

    #[test]
    fn fingerprint_changes_with_any_input_source_identity() {
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
        let mut changed_bytes = fingerprint("a");
        changed_bytes.primary_layout_hash += 1;
        assert_ne!(fingerprint("a"), changed_bytes);
    }

    fn fingerprint(id: &str) -> LayoutFingerprint {
        LayoutFingerprint {
            selected_source: id.into(),
            layout_source: format!("{id}-layout"),
            ascii_source: format!("{id}-ascii"),
            primary_layout_hash: 1,
            ascii_layout_hash: 2,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn current_dvorak_resolves_logical_hotkey_characters() {
        let snapshot = LayoutSnapshot::current().unwrap();
        let mapping = ['h', 'j', 'k', 'l', '/', 'f', '-', '=']
            .into_iter()
            .map(|character| {
                (
                    character,
                    snapshot.resolve(LogicalKey::Character(character)).unwrap(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        eprintln!(
            "current keyboard source {:?}: {mapping:?}",
            snapshot.fingerprint
        );

        if snapshot
            .fingerprint
            .selected_source
            .to_ascii_lowercase()
            .contains("dvorak")
            || snapshot
                .fingerprint
                .layout_source
                .to_ascii_lowercase()
                .contains("dvorak")
        {
            assert_eq!(mapping[&'-'].key_code, 0x27);
            assert_eq!(mapping[&'='].key_code, 0x1e);
        }
    }
}
