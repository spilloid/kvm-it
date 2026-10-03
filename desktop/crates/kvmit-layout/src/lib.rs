//! Text → key strokes. The firmware never sees text, so adding a layout never needs a firmware update.
use kvmit_hid::Key;

/// One key press for a character: the key plus whether Shift must be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    pub key: Key,
    pub shift: bool,
}

pub trait Layout {
    fn name(&self) -> &'static str;
    /// `None` if the layout cannot type this character (never guessed).
    fn stroke(&self, c: char) -> Option<Stroke>;
}

pub struct UsAnsi;

impl Layout for UsAnsi {
    fn name(&self) -> &'static str {
        "US ANSI"
    }

    fn stroke(&self, c: char) -> Option<Stroke> {
        let s = |k: u8, shift: bool| Some(Stroke { key: Key(k), shift });
        match c {
            'a'..='z' => s(0x04 + (c as u8 - b'a'), false),
            'A'..='Z' => s(0x04 + (c as u8 - b'A'), true),
            '1'..='9' => s(0x1E + (c as u8 - b'1'), false),
            '0' => s(0x27, false),
            '!' => s(0x1E, true),
            '@' => s(0x1F, true),
            '#' => s(0x20, true),
            '$' => s(0x21, true),
            '%' => s(0x22, true),
            '^' => s(0x23, true),
            '&' => s(0x24, true),
            '*' => s(0x25, true),
            '(' => s(0x26, true),
            ')' => s(0x27, true),
            '\n' | '\r' => s(0x28, false),
            '\t' => s(0x2B, false),
            ' ' => s(0x2C, false),
            '-' => s(0x2D, false),
            '_' => s(0x2D, true),
            '=' => s(0x2E, false),
            '+' => s(0x2E, true),
            '[' => s(0x2F, false),
            '{' => s(0x2F, true),
            ']' => s(0x30, false),
            '}' => s(0x30, true),
            '\\' => s(0x31, false),
            '|' => s(0x31, true),
            ';' => s(0x33, false),
            ':' => s(0x33, true),
            '\'' => s(0x34, false),
            '"' => s(0x34, true),
            '`' => s(0x35, false),
            '~' => s(0x35, true),
            ',' => s(0x36, false),
            '<' => s(0x36, true),
            '.' => s(0x37, false),
            '>' => s(0x37, true),
            '/' => s(0x38, false),
            '?' => s(0x38, true),
            _ => None,
        }
    }
}

/// Characters of `text` the layout cannot type, in order of first appearance (so callers can refuse up front
/// instead of typing half a password).
pub fn untypable(layout: &dyn Layout, text: &str) -> Vec<char> {
    let mut bad = Vec::new();
    for c in text.chars() {
        if layout.stroke(c).is_none() && !bad.contains(&c) {
            bad.push(c);
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_ascii_char_has_a_unique_stroke() {
        let l = UsAnsi;
        let mut seen = std::collections::HashSet::new();
        for c in (0x20u8..0x7F).map(|b| b as char) {
            let st = l.stroke(c).unwrap_or_else(|| panic!("no stroke for {c:?}"));
            assert!(st.key.is_valid());
            assert!(seen.insert((st.key.0, st.shift)), "duplicate stroke for {c:?}");
        }
    }

    #[test]
    fn spot_checks() {
        let l = UsAnsi;
        assert_eq!(l.stroke('a'), Some(Stroke { key: Key(0x04), shift: false }));
        assert_eq!(l.stroke('A'), Some(Stroke { key: Key(0x04), shift: true }));
        assert_eq!(l.stroke('!'), Some(Stroke { key: Key(0x1E), shift: true }));
        assert_eq!(l.stroke('~'), Some(Stroke { key: Key(0x35), shift: true }));
        assert_eq!(l.stroke('\n'), Some(Stroke { key: Key::ENTER, shift: false }));
    }

    #[test]
    fn untypable_reports_instead_of_guessing() {
        assert_eq!(untypable(&UsAnsi, "pässword€ä"), vec!['ä', '€']);
        assert!(untypable(&UsAnsi, "plain Text 123!").is_empty());
    }
}
