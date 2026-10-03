//! egui key → USB HID usage. Uses physical keys so the *target's* layout decides what appears.
use egui::Key as K;
use kvmit_hid::Key;

pub fn to_hid(k: K) -> Option<Key> {
    let u = match k {
        K::A => 0x04, K::B => 0x05, K::C => 0x06, K::D => 0x07, K::E => 0x08, K::F => 0x09, K::G => 0x0A,
        K::H => 0x0B, K::I => 0x0C, K::J => 0x0D, K::K => 0x0E, K::L => 0x0F, K::M => 0x10, K::N => 0x11,
        K::O => 0x12, K::P => 0x13, K::Q => 0x14, K::R => 0x15, K::S => 0x16, K::T => 0x17, K::U => 0x18,
        K::V => 0x19, K::W => 0x1A, K::X => 0x1B, K::Y => 0x1C, K::Z => 0x1D,
        K::Num1 => 0x1E, K::Num2 => 0x1F, K::Num3 => 0x20, K::Num4 => 0x21, K::Num5 => 0x22,
        K::Num6 => 0x23, K::Num7 => 0x24, K::Num8 => 0x25, K::Num9 => 0x26, K::Num0 => 0x27,
        K::Enter => 0x28, K::Escape => 0x29, K::Backspace => 0x2A, K::Tab => 0x2B, K::Space => 0x2C,
        K::Minus => 0x2D, K::Equals => 0x2E, K::OpenBracket => 0x2F, K::CloseBracket => 0x30,
        K::Backslash => 0x31, K::Semicolon => 0x33, K::Quote => 0x34, K::Backtick => 0x35,
        K::Comma => 0x36, K::Period => 0x37, K::Slash => 0x38,
        K::F1 => 0x3A, K::F2 => 0x3B, K::F3 => 0x3C, K::F4 => 0x3D, K::F5 => 0x3E, K::F6 => 0x3F,
        K::F7 => 0x40, K::F8 => 0x41, K::F9 => 0x42, K::F10 => 0x43, K::F11 => 0x44, K::F12 => 0x45,
        K::F13 => 0x68, K::F14 => 0x69, K::F15 => 0x6A, K::F16 => 0x6B, K::F17 => 0x6C, K::F18 => 0x6D,
        K::F19 => 0x6E, K::F20 => 0x6F, K::F21 => 0x70, K::F22 => 0x71, K::F23 => 0x72, K::F24 => 0x73,
        K::Insert => 0x49, K::Home => 0x4A, K::PageUp => 0x4B, K::Delete => 0x4C, K::End => 0x4D,
        K::PageDown => 0x4E, K::ArrowRight => 0x4F, K::ArrowLeft => 0x50, K::ArrowDown => 0x51, K::ArrowUp => 0x52,
        _ => return None,
    };
    Some(Key(u))
}

/// Modifier keys implied by an egui `Modifiers` snapshot, in a stable order.
pub fn modifier_keys(m: &egui::Modifiers) -> Vec<Key> {
    let mut v = Vec::new();
    if m.ctrl {
        v.push(Key::LEFT_CTRL);
    }
    if m.shift {
        v.push(Key::LEFT_SHIFT);
    }
    if m.alt {
        v.push(Key::LEFT_ALT);
    }
    if m.mac_cmd || m.command && !m.ctrl {
        v.push(Key::LEFT_GUI);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_agrees_with_name_parser() {
        for (egui_key, name) in [(K::A, "A"), (K::Z, "Z"), (K::Num1, "1"), (K::Num0, "0"), (K::Enter, "ENTER"),
                                 (K::F5, "F5"), (K::ArrowUp, "UP"), (K::Delete, "DELETE"), (K::Space, "SPACE")] {
            assert_eq!(to_hid(egui_key), kvmit_hid::parse_key(name), "{name}");
        }
        assert_eq!(to_hid(K::F20), kvmit_hid::parse_key("F20"));
        assert_eq!(to_hid(K::F25), None);
    }

    #[test]
    fn modifiers() {
        let m = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
        assert_eq!(modifier_keys(&m), vec![Key::LEFT_CTRL, Key::LEFT_ALT]);
    }
}
