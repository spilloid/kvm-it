//! USB HID Keyboard/Keypad page (0x07) usage codes and name parsing. The firmware only ever sees usages;
//! everything text-shaped lives on this side.

/// A keyboard usage ID (page 0x07). Modifiers are `0xE0..=0xE7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(pub u8);

impl Key {
    pub const A: Key = Key(0x04);
    pub const ENTER: Key = Key(0x28);
    pub const ESC: Key = Key(0x29);
    pub const BACKSPACE: Key = Key(0x2A);
    pub const TAB: Key = Key(0x2B);
    pub const SPACE: Key = Key(0x2C);
    pub const CAPS_LOCK: Key = Key(0x39);
    pub const LEFT_CTRL: Key = Key(0xE0);
    pub const LEFT_SHIFT: Key = Key(0xE1);
    pub const LEFT_ALT: Key = Key(0xE2);
    pub const LEFT_GUI: Key = Key(0xE3);

    pub fn is_modifier(self) -> bool {
        (0xE0..=0xE7).contains(&self.0)
    }
    /// Usages the protocol accepts (0x00..=0x03 are reserved/rollover-error values).
    pub fn is_valid(self) -> bool {
        self.0 >= 0x04
    }
}

pub const MOUSE_LEFT: u8 = 0x01;
pub const MOUSE_RIGHT: u8 = 0x02;
pub const MOUSE_MIDDLE: u8 = 0x04;

/// Parse a key name as used in scripts (case-insensitive): `A`, `7`, `ENTER`, `F5`, `CTRL`, `RSHIFT`, `GUI`...
pub fn parse_key(name: &str) -> Option<Key> {
    let n = name.trim().to_ascii_uppercase();
    let b = n.as_bytes();
    if b.len() == 1 {
        return match b[0] {
            b'A'..=b'Z' => Some(Key(0x04 + (b[0] - b'A'))),
            b'1'..=b'9' => Some(Key(0x1E + (b[0] - b'1'))),
            b'0' => Some(Key(0x27)),
            _ => None,
        };
    }
    if let Some(f) = n.strip_prefix('F') {
        if let Ok(i) = f.parse::<u8>() {
            return match i {
                1..=12 => Some(Key(0x3A + i - 1)),
                13..=24 => Some(Key(0x68 + i - 13)),
                _ => None,
            };
        }
    }
    Some(Key(match n.as_str() {
        "ENTER" | "RETURN" => 0x28,
        "ESC" | "ESCAPE" => 0x29,
        "BACKSPACE" | "BKSP" => 0x2A,
        "TAB" => 0x2B,
        "SPACE" => 0x2C,
        "MINUS" => 0x2D,
        "EQUAL" => 0x2E,
        "CAPSLOCK" | "CAPS_LOCK" => 0x39,
        "PRINTSCREEN" | "PRTSC" => 0x46,
        "SCROLLLOCK" => 0x47,
        "PAUSE" | "BREAK" => 0x48,
        "INSERT" => 0x49,
        "HOME" => 0x4A,
        "PAGEUP" | "PGUP" => 0x4B,
        "DELETE" | "DEL" => 0x4C,
        "END" => 0x4D,
        "PAGEDOWN" | "PGDN" => 0x4E,
        "RIGHT" | "RIGHTARROW" => 0x4F,
        "LEFT" | "LEFTARROW" => 0x50,
        "DOWN" | "DOWNARROW" => 0x51,
        "UP" | "UPARROW" => 0x52,
        "NUMLOCK" => 0x53,
        "MENU" | "APP" => 0x65,
        "CTRL" | "CONTROL" | "LCTRL" => 0xE0,
        "SHIFT" | "LSHIFT" => 0xE1,
        "ALT" | "LALT" | "OPTION" => 0xE2,
        "GUI" | "WIN" | "WINDOWS" | "CMD" | "COMMAND" | "META" | "SUPER" | "LGUI" => 0xE3,
        "RCTRL" => 0xE4,
        "RSHIFT" => 0xE5,
        "RALT" | "ALTGR" => 0xE6,
        "RGUI" | "RWIN" => 0xE7,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_names() {
        assert_eq!(parse_key("a"), Some(Key(0x04)));
        assert_eq!(parse_key("Z"), Some(Key(0x1D)));
        assert_eq!(parse_key("1"), Some(Key(0x1E)));
        assert_eq!(parse_key("0"), Some(Key(0x27)));
        assert_eq!(parse_key("enter"), Some(Key::ENTER));
        assert_eq!(parse_key("F1"), Some(Key(0x3A)));
        assert_eq!(parse_key("F12"), Some(Key(0x45)));
        assert_eq!(parse_key("F13"), Some(Key(0x68)));
        assert_eq!(parse_key("F24"), Some(Key(0x73)));
        assert_eq!(parse_key("F25"), None);
        assert_eq!(parse_key("WIN"), Some(Key::LEFT_GUI));
        assert_eq!(parse_key("nonsense"), None);
    }

    #[test]
    fn modifier_and_validity() {
        assert!(Key::LEFT_SHIFT.is_modifier());
        assert!(!Key::A.is_modifier());
        assert!(!Key(2).is_valid());
        assert!(Key::A.is_valid());
    }
}
