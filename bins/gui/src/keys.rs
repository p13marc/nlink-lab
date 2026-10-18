//! The keyboard shortcuts. They come from `keyboard::listen()`, which
//! only sees keys no widget took: typing in a text field never triggers
//! one, and the canvas handles no keys of its own.

use iced::keyboard::{self, Key, Modifiers, key::Named};

use crate::message::{Message, Shortcut};

/// The sheet `F1` shows: keys, then what they do.
pub const SHEET: &[(&str, &str)] = &[
    ("Ctrl+O", "Open a topology"),
    ("Ctrl+R, F5", "Reload"),
    ("Ctrl+W", "Close the tab"),
    ("Ctrl+Tab, Ctrl+Shift+Tab", "Next, previous tab"),
    ("Ctrl+H", "Home"),
    ("F, 0", "Fit the topology"),
    ("+, -", "Zoom in, out"),
    ("L", "Next layout"),
    ("Shift+L", "Forget placed nodes and lay out again"),
    ("A", "Addresses on and off"),
    ("M", "Management network on and off"),
    ("Ctrl+E, Ctrl+Shift+E", "Export SVG, PNG"),
    ("Esc", "Clear the selection, close this sheet"),
    ("F1, ?", "This sheet"),
    ("Ctrl+Q", "Quit"),
];

/// The shortcut `key` with `mods` stands for.
pub fn shortcut(key: &Key, mods: Modifiers) -> Option<Shortcut> {
    use Shortcut::*;
    let ctrl = mods.command();
    match key.as_ref() {
        Key::Named(Named::Tab) if ctrl && mods.shift() => Some(PrevTab),
        Key::Named(Named::Tab) if ctrl => Some(NextTab),
        Key::Named(Named::PageDown) if ctrl => Some(NextTab),
        Key::Named(Named::PageUp) if ctrl => Some(PrevTab),
        Key::Named(Named::F5) => Some(Reload),
        Key::Named(Named::F1) => Some(Help),
        Key::Named(Named::Escape) => Some(Escape),
        Key::Character(c) if ctrl && !mods.alt() => match c.to_lowercase().as_str() {
            "o" => Some(Open),
            "w" => Some(CloseTab),
            "h" => Some(Home),
            "r" => Some(Reload),
            "e" if mods.shift() => Some(ExportPng),
            "e" => Some(ExportSvg),
            "q" => Some(Quit),
            _ => None,
        },
        // bare keys: no Ctrl or Alt, so they never shadow a system shortcut
        Key::Character(c) if !ctrl && !mods.alt() => match c {
            "f" | "F" if !mods.shift() => Some(Fit),
            "0" => Some(Fit),
            "+" | "=" => Some(ZoomIn),
            "-" => Some(ZoomOut),
            "l" | "L" if mods.shift() => Some(Unpin),
            "l" => Some(NextLayout),
            "a" => Some(ToggleAddresses),
            "m" => Some(ToggleMgmt),
            "?" => Some(Help),
            _ => None,
        },
        _ => None,
    }
}

/// A key press no widget took, as a message.
pub fn on_event(event: keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed {
            key,
            modified_key,
            modifiers,
            repeat: false,
            ..
        } => {
            // `?` is Shift+/ on most layouts: the modified key says so
            shortcut(&key, modifiers)
                .or_else(|| shortcut(&modified_key, modifiers))
                .map(Message::Shortcut)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn control_shortcuts() {
        let ctrl = Modifiers::CTRL;
        assert_eq!(shortcut(&ch("o"), ctrl), Some(Shortcut::Open));
        assert_eq!(shortcut(&ch("W"), ctrl), Some(Shortcut::CloseTab));
        assert_eq!(shortcut(&ch("e"), ctrl), Some(Shortcut::ExportSvg));
        assert_eq!(
            shortcut(&ch("E"), ctrl | Modifiers::SHIFT),
            Some(Shortcut::ExportPng)
        );
        assert_eq!(
            shortcut(&Key::Named(Named::Tab), ctrl),
            Some(Shortcut::NextTab)
        );
        assert_eq!(
            shortcut(&Key::Named(Named::Tab), ctrl | Modifiers::SHIFT),
            Some(Shortcut::PrevTab)
        );
        assert_eq!(shortcut(&ch("x"), ctrl), None);
    }

    #[test]
    fn bare_keys_only_without_ctrl_or_alt() {
        let none = Modifiers::empty();
        assert_eq!(shortcut(&ch("f"), none), Some(Shortcut::Fit));
        assert_eq!(shortcut(&ch("l"), none), Some(Shortcut::NextLayout));
        assert_eq!(shortcut(&ch("L"), Modifiers::SHIFT), Some(Shortcut::Unpin));
        assert_eq!(shortcut(&ch("a"), none), Some(Shortcut::ToggleAddresses));
        assert_eq!(shortcut(&ch("a"), Modifiers::ALT), None);
        assert_eq!(shortcut(&ch("f"), Modifiers::CTRL), None);
        assert_eq!(shortcut(&Key::Named(Named::Tab), none), None);
    }

    #[test]
    fn repeats_and_releases_do_nothing() {
        let press = |repeat| keyboard::Event::KeyPressed {
            key: ch("f"),
            modified_key: ch("f"),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat,
        };
        assert!(matches!(
            on_event(press(false)),
            Some(Message::Shortcut(Shortcut::Fit))
        ));
        assert!(on_event(press(true)).is_none());
    }

    #[test]
    fn the_sheet_lists_every_shortcut_once() {
        assert_eq!(SHEET.len(), 15);
        let keys: std::collections::BTreeSet<_> = SHEET.iter().map(|(k, _)| k).collect();
        assert_eq!(keys.len(), SHEET.len());
    }
}
