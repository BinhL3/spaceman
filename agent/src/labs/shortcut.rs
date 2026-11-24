//! A Labs feature's shortcut on its card: a recorder that writes the chord to `[labs]` in
//! `config.toml` in `[shortcuts]` syntax. The agent reads it back and registers it at once.
//! Backspace while recording puts the default back.

use rune::{El, Shortcut, ShortcutError};
use rune_kit::{ShortcutRecorder, ShortcutRecorderState, shortcut_error_message};
use spaceman::{
    labs::{LabFeature, LabShortcut},
    shortcuts::{Chord, Key, parse_chord},
};

use crate::{Ui, controls::Paint};

pub(super) fn row(ui: &Ui, p: Paint, feature: LabFeature) -> El {
    let state = &ui.labs.recorder;
    // At rest the recorder shows what the file says, so an edit made elsewhere shows too.
    let shown = if state.is_recording() || state.error().is_some() {
        state.clone()
    } else {
        ShortcutRecorderState::new(to_recorder(ui.doc.lab_shortcut(feature).0))
    };
    let recorder = ShortcutRecorder::new("recorder", &shown, |ui: &mut Ui| &mut ui.labs.recorder)
        .label(format!("{} shortcut", feature.name()))
        .width(140.0)
        .on_change(move |ui: &mut Ui, next: Option<&Shortcut>, _| {
            let shortcut = match next {
                None => LabShortcut::default(),
                Some(next) => {
                    LabShortcut(from_recorder(next).map_err(ShortcutError::InvalidAccelerator)?)
                }
            };
            ui.set_lab_shortcut(feature, shortcut);
            match ui.error.clone() {
                Some(error) => Err(ShortcutError::Backend(error)),
                None => Ok(()),
            }
        })
        .error_message(|error, chord| match error {
            ShortcutError::InvalidAccelerator(reason) if chord.is_some() => {
                format!("Spaceman can't use that key: {reason}.")
            }
            ShortcutError::Backend(reason) => reason.clone(),
            _ => shortcut_error_message(error, chord),
        })
        .build(p.k);
    crate::controls::setting_row(
        p,
        "shortcut",
        "Shortcut",
        Some((shortcut_description(feature).into(), p.muted())),
        recorder,
    )
}

const fn shortcut_description(feature: LabFeature) -> &'static str {
    match feature {
        LabFeature::ScrollingStrip => "Turns the strip on or off for the focused workspace.",
    }
}

/// Rune's punctuation key names and the characters `[shortcuts]` names them by.
const PUNCTUATION: [(&str, char); 11] = [
    ("minus", '-'),
    ("equal", '='),
    ("comma", ','),
    ("period", '.'),
    ("slash", '/'),
    ("semicolon", ';'),
    ("quote", '\''),
    ("bracketleft", '['),
    ("bracketright", ']'),
    ("backslash", '\\'),
    ("backquote", '`'),
];

/// The recorder's chord in `[shortcuts]` terms. Rune records keys by their US position, which
/// `[shortcuts]` reads as the characters they type there.
pub(super) fn from_recorder(shortcut: &Shortcut) -> Result<Chord, String> {
    let written = shortcut.to_string();
    let (modifiers, key) = written.rsplit_once('+').unwrap_or(("", &written));
    let key = match key {
        "enter" => "return".to_owned(),
        "delete" => "forwarddelete".to_owned(),
        name => PUNCTUATION
            .iter()
            .find(|(punctuation, _)| *punctuation == name)
            .map_or_else(|| name.to_owned(), |(_, character)| character.to_string()),
    };
    let modifiers = modifiers.replace("super", "cmd");
    let source = if modifiers.is_empty() {
        key
    } else {
        format!("{modifiers}+{key}")
    };
    parse_chord(&source)
}

/// `chord` as the recorder shows it, or `None` for a key it has no name for.
pub(super) fn to_recorder(chord: Chord) -> Option<Shortcut> {
    let mut source = String::new();
    for (held, name) in [
        (chord.modifiers.control, "ctrl+"),
        (chord.modifiers.option, "alt+"),
        (chord.modifiers.shift, "shift+"),
        (chord.modifiers.command, "super+"),
    ] {
        if held {
            source.push_str(name);
        }
    }
    match chord.key {
        Key::Character(character) => {
            match PUNCTUATION.iter().find(|(_, known)| *known == character) {
                Some((name, _)) => source.push_str(name),
                None => source.push(character),
            }
        }
        Key::Number(number) => source.push_str(&(number % 10).to_string()),
        Key::Code(_) => {
            let written = chord.config_name();
            match written
                .rsplit_once('+')
                .map_or(written.as_str(), |(_, key)| key)
            {
                "forwarddelete" => source.push_str("delete"),
                key => source.push_str(key),
            }
        }
    }
    source.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_chords_round_trip_through_the_config_syntax() {
        for (recorded, config, symbols) in [
            ("alt+shift+s", "alt+shift+s", "⌥⇧S"),
            ("ctrl+alt+super+left", "ctrl+alt+cmd+left", "⌃⌥⌘←"),
            ("alt+minus", "alt+-", "⌥-"),
            ("ctrl+enter", "ctrl+return", "⌃↩"),
            ("alt+f5", "alt+f5", "⌥F5"),
            ("super+alt+7", "alt+cmd+7", "⌥⌘7"),
        ] {
            let shortcut: Shortcut = recorded.parse().unwrap();
            let chord = from_recorder(&shortcut).unwrap();
            assert_eq!(chord.config_name(), config, "{recorded}");
            assert_eq!(chord.symbols(), symbols, "{recorded}");
            assert_eq!(to_recorder(chord), Some(shortcut), "{recorded}");
        }
    }

    #[test]
    fn chords_spaceman_cannot_register_are_refused() {
        for recorded in ["shift+s", "alt+f13"] {
            let shortcut: Shortcut = recorded.parse().unwrap();
            assert!(from_recorder(&shortcut).is_err(), "{recorded}");
        }
    }
}
