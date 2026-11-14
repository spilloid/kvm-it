//! Text read off a captured screen, and matching a state's expected phrases against it.
//!
//! Reading is the host's job (each OS's own text recogniser, see `kvmit-video`). This module only defines what a reader
//! returns and how a phrase is found in it, so the engine and its tests need no recogniser.

/// One line of text as the reader saw it, with where it is on the frame.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    pub text: String,
    /// `[x, y, width, height]` as fractions of the frame's width and height.
    pub rect: [f64; 4],
}

impl TextLine {
    fn centre(&self) -> (f64, f64) {
        (self.rect[0] + self.rect[2] / 2.0, self.rect[1] + self.rect[3] / 2.0)
    }
}

/// Lower case, typographic quotes made plain, everything but letters, digits and `'@.` made a space (hyphens and dashes
/// too: readers see "Sign-in" and "Sign in" alike), and runs of spaces made one. Readers differ in exactly these things,
/// so phrases and screen text are compared in this form.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '`' | '\u{00B4}' => '\'',
            c => c,
        };
        if c.is_alphanumeric() || matches!(c, '\'' | '@' | '.') {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The lines whose centre is inside `region` (`[x, y, width, height]` fractions; all lines when `None`), in reading
/// order (top to bottom, then left to right), joined by spaces and normalised. A heading that wraps onto a second line
/// reads as one phrase.
pub fn screen_text(lines: &[TextLine], region: Option<[f64; 4]>) -> String {
    let mut inside: Vec<&TextLine> = lines
        .iter()
        .filter(|l| match region {
            None => true,
            Some([x, y, w, h]) => {
                let (cx, cy) = l.centre();
                cx >= x && cx <= x + w && cy >= y && cy <= y + h
            }
        })
        .collect();
    // Lines whose tops are within half a line of each other count as one row, read left to right.
    inside.sort_by(|a, b| {
        let row = a.rect[3].max(b.rect[3]) / 2.0;
        if (a.rect[1] - b.rect[1]).abs() <= row {
            a.rect[0].total_cmp(&b.rect[0])
        } else {
            a.rect[1].total_cmp(&b.rect[1])
        }
    });
    normalize(&inside.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" "))
}

/// How well `phrase` appears in `text`, 0..1: one minus the fewest single-character edits (insert, delete, substitute)
/// that turn some run of whole words of `text` into the phrase, over the phrase's length. `text` is expected
/// normalised ([`screen_text`]); the phrase is normalised here. An exact appearance scores 1; one misread letter in a
/// 30-letter phrase scores about 0.97. Matching whole words keeps a phrase from being found inside other words
/// ("license agreement" in "services agreement").
pub fn phrase_score(text: &str, phrase: &str) -> f64 {
    let p: Vec<char> = normalize(phrase).chars().collect();
    if p.is_empty() {
        return 0.0;
    }
    let t: Vec<char> = text.chars().collect();
    // Sellers' approximate substring match, with starts and ends allowed only at word boundaries. `prev` is the row
    // for the first `i` characters of `text`; column 0 is free (0) where a word starts, out of reach elsewhere.
    let far = p.len() + t.len() + 1;
    let mut prev: Vec<usize> = (0..=p.len()).collect();
    let boundary = |i: usize| i == t.len() || t[i] == ' ';
    let mut best = if boundary(0) { prev[p.len()] } else { far };
    let mut cur = vec![0usize; p.len() + 1];
    for (i, tc) in t.iter().enumerate() {
        cur[0] = if *tc == ' ' { 0 } else { far };
        for j in 1..=p.len() {
            let sub = prev[j - 1] + usize::from(p[j - 1] != *tc);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        if boundary(i + 1) {
            best = best.min(cur[p.len()]);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (1.0 - best as f64 / p.len() as f64).max(0.0)
}

/// Whether a typed value shows on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    Shown,
    /// Shown with every letter's case swapped: Caps Lock is on at the target.
    CaseSwapped,
    Absent,
}

/// Whether `value` (just typed) is exactly what one line shows, case and all, start to end: a field holding more
/// (a second copy, text appended) is not it. Spaces are ignored (readers split and join words differently). There
/// is no tolerance for misreading: an uncertain read is a refusal, and the engine looks again. A value without
/// letters cannot show Caps Lock.
pub fn typed_shown(lines: &[TextLine], value: &str) -> Typed {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let want = squash(value);
    if want.is_empty() {
        return Typed::Absent;
    }
    let swapped: String =
        want.chars().flat_map(|c| if c.is_lowercase() { c.to_uppercase().collect::<Vec<_>>() } else { c.to_lowercase().collect() }).collect();
    let shown: Vec<String> = lines.iter().map(|l| squash(&l.text)).collect();
    if shown.contains(&want) {
        Typed::Shown
    } else if swapped != want && shown.contains(&swapped) {
        Typed::CaseSwapped
    } else {
        Typed::Absent
    }
}

/// The characters an address may hold, on screen and as entered (see [`is_address`]).
pub fn address_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-' | '@' | '\'')
}

/// `name@domain`: address characters only, one `@`, something before it and a dot in the domain.
pub fn is_address(s: &str) -> bool {
    s.chars().all(address_char)
        && s.split_once('@').is_some_and(|(name, domain)| !name.is_empty() && domain.contains('.') && !domain.contains('@'))
}

/// Every whole e-mail address on the screen, as read: a word (between spaces, or brackets, quotes, commas, colons
/// and semicolons) that is an address in full ([`is_address`]), less only a full stop that ends a sentence. Nothing is
/// trimmed off its name: `other!jane@contoso.com` or `-jane@contoso.com` is not `jane@contoso.com`, and a word with
/// anything else in it is no address at all. Punctuation inside one (`contoso-it.com`, `first.last`) is kept: this
/// is not the prose normalisation.
pub fn accounts_shown(lines: &[TextLine]) -> Vec<String> {
    let between = |c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '[' | ']' | '"' | ',' | ';' | ':');
    let mut out = Vec::new();
    for l in lines {
        for word in l.text.split(between) {
            let word = word.strip_suffix('.').unwrap_or(word);
            if is_address(word) {
                out.push(word.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f64, y: f64) -> TextLine {
        TextLine { text: text.into(), rect: [x, y, 0.3, 0.03] }
    }

    #[test]
    fn normalising_drops_what_readers_disagree_on() {
        assert_eq!(normalize("  Can\u{2019}t  contain\tonly numbers!  "), "can't contain only numbers");
        assert_eq!(normalize("Sign\u{2011}in options"), "sign in options");
        assert_eq!(normalize("someone@example.com"), "someone@example.com");
        assert_eq!(normalize("dashes ( \u{2013} and \u{2014} )"), "dashes and");
        assert_eq!(normalize("and-so"), "and so");
    }

    #[test]
    fn a_phrase_is_found_exactly_with_a_misread_and_not_when_absent() {
        let t = normalize("Is this the right keyboard layout or input method? If you also use another layout");
        assert_eq!(phrase_score(&t, "Is this the right keyboard layout"), 1.0);
        let one_off = phrase_score(&normalize("Is this the rignt keyboard layout"), "is this the right keyboard layout");
        assert!(one_off > 0.95 && one_off < 1.0, "{one_off}");
        assert!(phrase_score(&t, "Name your device") < 0.6);
        assert_eq!(phrase_score(&t, ""), 0.0);
        assert_eq!(phrase_score("", "anything"), 0.0);
        // Whole words only: not inside another word, and words sharing an ending do not get close.
        assert!(phrase_score("microsoft services agreement and", "license agreement") < 0.75);
        assert!(phrase_score("unsigned in", "sign in") < 0.9);
        assert_eq!(phrase_score("please sign in now", "sign in"), 1.0);
        assert_eq!(phrase_score("sign in", "sign in"), 1.0);
    }

    #[test]
    fn a_typed_value_is_shown_whole_and_exactly_or_with_caps_lock_or_not_at_all() {
        let lines = |t: &str| vec![line("Sign in", 0.5, 0.4), line(t, 0.5, 0.47)];
        assert_eq!(typed_shown(&lines("jane.doe@contoso.com"), "jane.doe@contoso.com"), Typed::Shown);
        assert_eq!(typed_shown(&lines("jane. doe@contoso.com"), "jane.doe@contoso.com"), Typed::Shown, "spaces ignored");
        assert_eq!(typed_shown(&lines("JANE.DOE@CONTOSO.COM"), "jane.doe@contoso.com"), Typed::CaseSwapped);
        assert_eq!(typed_shown(&lines("jANE@cONTOSO.COM"), "Jane@Contoso.com"), Typed::CaseSwapped);
        for wrong in [
            "jane.doe@contoso.corn",                  // a misread is not a match: look again
            "Jane.doe@contoso.com",                   // case is part of what was typed
            "jane.doe\"contoso.com",                   // a UK layout types " for @
            "jane.doe@contoso.com.evil",              // more than was typed
            "jane.doe@contoso.comjane.doe@contoso.com", // typed twice into a field that was not empty
            "Sign in to jane.doe@contoso.com",        // a label, not the field
        ] {
            assert_eq!(typed_shown(&lines(wrong), "jane.doe@contoso.com"), Typed::Absent, "{wrong}");
        }
        assert_eq!(typed_shown(&lines("12345"), "12345"), Typed::Shown);
        assert_eq!(typed_shown(&lines(""), ""), Typed::Absent);
    }

    #[test]
    fn accounts_are_read_whole_with_their_punctuation() {
        let l = vec![line("\u{2190} alexander.smith@contoso-it.com", 0.5, 0.2), line("Enter password", 0.5, 0.3)];
        assert_eq!(accounts_shown(&l), ["alexander.smith@contoso-it.com"]);
        let l = vec![line("Sign in as jane@contoso.com. Or bob@fabrikam.co.uk, not @nobody or a@b@c.com", 0.5, 0.2)];
        assert_eq!(accounts_shown(&l), ["jane@contoso.com", "bob@fabrikam.co.uk"]);
    }

    #[test]
    fn screen_text_reads_a_region_in_order_and_joins_wrapped_lines() {
        let lines = [
            line("method?", 0.53, 0.21),
            line("Is this the right keyboard layout or input", 0.53, 0.17),
            line("US", 0.55, 0.40),
            line("Afghanistan", 0.05, 0.40), // left panel: outside the region
        ];
        assert_eq!(screen_text(&lines, Some([0.52, 0.12, 0.40, 0.75])), "is this the right keyboard layout or input method us");
        assert!(screen_text(&lines, None).contains("afghanistan"));
        assert!(phrase_score(&screen_text(&lines, Some([0.52, 0.12, 0.40, 0.75])), "keyboard layout or input method") == 1.0);
    }
}
