//! Light / dark theme: the user's choice (saved in the config) and the status palette for each mode. Every colour that
//! carries meaning comes from here, and the tests hold each one to WCAG AA contrast (4.5:1) against what it is drawn on,
//! so neither mode can quietly regress to unreadable.
use egui::Color32;
use serde::{Deserialize, Serialize};

/// What the person picked. `System` (the default) follows the operating system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// The button cycles System → Light → Dark → System.
    pub fn next(self) -> ThemeChoice {
        match self {
            ThemeChoice::System => ThemeChoice::Light,
            ThemeChoice::Light => ThemeChoice::Dark,
            ThemeChoice::Dark => ThemeChoice::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Theme: auto",
            ThemeChoice::Light => "Theme: light",
            ThemeChoice::Dark => "Theme: dark",
        }
    }

    pub fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// Colours for one mode. `*_fill` carry white text (chips); `*_text` are drawn straight on the panel background.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub good_fill: Color32,
    pub warn_fill: Color32,
    pub bad_fill: Color32,
    pub good_text: Color32,
    pub warn_text: Color32,
    pub bad_text: Color32,
}

pub fn palette(dark: bool) -> Palette {
    // fills are the same in both modes (white text on a saturated, darkened colour reads on either panel)
    let (good_fill, warn_fill, bad_fill) = (Color32::from_rgb(30, 125, 50), Color32::from_rgb(150, 95, 0), Color32::from_rgb(190, 45, 40));
    if dark {
        Palette {
            good_fill,
            warn_fill,
            bad_fill,
            good_text: Color32::from_rgb(95, 205, 115),
            warn_text: Color32::from_rgb(230, 170, 40),
            bad_text: Color32::from_rgb(245, 115, 105),
        }
    } else {
        Palette {
            good_fill,
            warn_fill,
            bad_fill,
            good_text: Color32::from_rgb(20, 110, 40),
            warn_text: Color32::from_rgb(140, 85, 0),
            bad_text: Color32::from_rgb(185, 30, 30),
        }
    }
}

fn luminance(c: Color32) -> f64 {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio, 1.0 to 21.0.
pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_cycles_through_all_three() {
        let mut t = ThemeChoice::System;
        let seen: Vec<_> = (0..3).map(|_| { t = t.next(); t }).collect();
        assert_eq!(seen, [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System]);
    }

    #[test]
    fn a_saved_choice_round_trips_and_an_unknown_one_is_an_error_not_a_panic() {
        assert_eq!(serde_json::to_string(&ThemeChoice::Dark).unwrap(), "\"dark\"");
        assert_eq!(serde_json::from_str::<ThemeChoice>("\"light\"").unwrap(), ThemeChoice::Light);
        assert!(serde_json::from_str::<ThemeChoice>("\"sepia\"").is_err());
    }

    #[test]
    fn chip_text_is_readable_on_every_fill() {
        for dark in [false, true] {
            let p = palette(dark);
            for (n, f) in [("good", p.good_fill), ("warn", p.warn_fill), ("bad", p.bad_fill)] {
                assert!(contrast(Color32::WHITE, f) >= 4.5, "white on {n} fill (dark={dark}): {:.2}", contrast(Color32::WHITE, f));
            }
        }
    }

    #[test]
    fn status_text_is_readable_on_the_panel_in_both_modes() {
        for dark in [false, true] {
            let panel = if dark { egui::Visuals::dark().panel_fill } else { egui::Visuals::light().panel_fill };
            let p = palette(dark);
            for (n, t) in [("good", p.good_text), ("warn", p.warn_text), ("bad", p.bad_text)] {
                assert!(contrast(t, panel) >= 4.5, "{n} text on the {} panel: {:.2}", if dark { "dark" } else { "light" }, contrast(t, panel));
            }
        }
    }
}
