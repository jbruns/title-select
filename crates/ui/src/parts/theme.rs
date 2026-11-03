//! Light and dark themes. The page follows the system's setting through `prefers-color-scheme`; `ThemeToggle`
//! overrides it, and the override is remembered in local storage. `assets/boot.js` applies a remembered override
//! before the workbench loads, so the page doesn't flash the other theme.

use std::fmt;
use std::str::FromStr;

use codee::string::FromToStringCodec;
use leptos::prelude::*;
use leptos_use::storage::use_local_storage;

use super::icon::{Glyph, IconButton};

/// The local storage key holding your theme; `assets/boot.js` reads it too.
pub const THEME_KEY: &str = "title-select.theme";

/// The theme you chose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// The choice the toggle moves to next.
    pub fn next(self) -> Self {
        match self {
            ThemeChoice::System => ThemeChoice::Light,
            ThemeChoice::Light => ThemeChoice::Dark,
            ThemeChoice::Dark => ThemeChoice::System,
        }
    }

    /// The root element's `data-theme`, or `None` to follow the system.
    pub fn attribute(self) -> Option<&'static str> {
        match self {
            ThemeChoice::System => None,
            ThemeChoice::Light => Some("light"),
            ThemeChoice::Dark => Some("dark"),
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            ThemeChoice::System => Glyph::Monitor,
            ThemeChoice::Light => Glyph::Sun,
            ThemeChoice::Dark => Glyph::Moon,
        }
    }

    /// The toggle's tooltip: the theme now, and what choosing it again does.
    pub fn label(self) -> String {
        let now = match self {
            ThemeChoice::System => "Theme follows your system",
            ThemeChoice::Light => "Light theme",
            ThemeChoice::Dark => "Dark theme",
        };
        let next = match self.next() {
            ThemeChoice::System => "follow your system",
            ThemeChoice::Light => "use the light theme",
            ThemeChoice::Dark => "use the dark theme",
        };
        format!("{now}. Choose to {next}")
    }
}

impl fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.attribute().unwrap_or("system"))
    }
}

impl FromStr for ThemeChoice {
    type Err = ();

    /// Anything unknown follows the system.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "light" => ThemeChoice::Light,
            "dark" => ThemeChoice::Dark,
            _ => ThemeChoice::System,
        })
    }
}

/// The top bar's theme button: system, then light, then dark.
#[component]
pub fn ThemeToggle() -> impl IntoView {
    let (choice, set_choice, _) = use_local_storage::<ThemeChoice, FromToStringCodec>(THEME_KEY);
    Effect::new(move || {
        let Some(root) = document().document_element() else {
            return;
        };
        let _ = match choice.get().attribute() {
            Some(theme) => root.set_attribute("data-theme", theme),
            None => root.remove_attribute("data-theme"),
        };
    });
    move || {
        let now = choice.get();
        view! {
            <IconButton
                glyph=now.glyph()
                label=now.label()
                on_click=move |_| set_choice.set(choice.get_untracked().next())
            />
        }
    }
}

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb` or `#rgb`.
    pub fn parse(hex: &str) -> Option<Rgb> {
        let h = hex.trim().strip_prefix('#')?;
        let digit = |s: &str| u8::from_str_radix(s, 16).ok();
        match h.len() {
            6 => Some(Rgb(digit(&h[0..2])?, digit(&h[2..4])?, digit(&h[4..6])?)),
            3 => {
                let one = |i: usize| digit(&h[i..=i]).map(|d| d * 17);
                Some(Rgb(one(0)?, one(1)?, one(2)?))
            }
            _ => None,
        }
    }

    /// Relative luminance, as WCAG 2 defines it.
    pub fn luminance(self) -> f64 {
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }
}

/// WCAG 2's contrast ratio between two colours, from 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::parts::status::Tone;

    #[test]
    fn the_toggle_cycles_through_every_choice_and_remembers_it_as_text() {
        let mut c = ThemeChoice::System;
        for _ in 0..3 {
            assert_eq!(c.to_string().parse::<ThemeChoice>(), Ok(c));
            c = c.next();
        }
        assert_eq!(c, ThemeChoice::System);
        assert_eq!("".parse::<ThemeChoice>(), Ok(ThemeChoice::System));
        assert_eq!(ThemeChoice::System.attribute(), None);
        assert_eq!(ThemeChoice::Dark.attribute(), Some("dark"));
    }

    #[test]
    fn contrast_matches_wcag() {
        let black = Rgb(0, 0, 0);
        let white = Rgb::parse("#fff").unwrap();
        assert!((contrast(black, white) - 21.0).abs() < 1e-9);
        assert!((contrast(white, white) - 1.0).abs() < 1e-9);
        // WCAG's own example: #767676 on white is just over 4.5.
        let grey = Rgb::parse("#767676").unwrap();
        assert!((contrast(grey, white) - 4.54).abs() < 0.01);
    }

    /// Every custom property the tokens and the workbench's stylesheet declare, the stylesheet's winning.
    fn properties() -> HashMap<String, String> {
        let tokens = include_str!("../../../../docs/brand-kit/title-select-tokens.css");
        let main = include_str!("../../style/main.css");
        let mut map = HashMap::new();
        for css in [tokens, main] {
            let mut css = css.to_string();
            while let Some(start) = css.find("/*") {
                let end = css[start..].find("*/").expect("comments close") + start + 2;
                css.replace_range(start..end, "");
            }
            for decl in css.split([';', '{', '}']) {
                if let Some((name, value)) = decl.trim().split_once(':')
                    && let Some(name) = name.trim().strip_prefix("--")
                {
                    map.insert(format!("--{name}"), value.trim().to_string());
                }
            }
        }
        map
    }

    /// The colour `name` has in the light (`dark == false`) or dark theme.
    fn resolve(map: &HashMap<String, String>, value: &str, dark: bool) -> Rgb {
        let value = value.trim();
        if let Some(inner) = value
            .strip_prefix("light-dark(")
            .and_then(|v| v.strip_suffix(')'))
        {
            let mut depth = 0;
            let split = inner
                .char_indices()
                .find(|(_, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        ',' if depth == 0 => return true,
                        _ => {}
                    }
                    false
                })
                .map(|(i, _)| i)
                .expect("light-dark has two values");
            let (light, rest) = inner.split_at(split);
            return resolve(map, if dark { &rest[1..] } else { light }, dark);
        }
        if let Some(name) = value.strip_prefix("var(").and_then(|v| v.strip_suffix(')')) {
            let next = map
                .get(name.trim())
                .unwrap_or_else(|| panic!("{name} is declared"));
            return resolve(map, next, dark);
        }
        Rgb::parse(value).unwrap_or_else(|| panic!("{value} is a hex colour"))
    }

    /// Status colours are text as well as icons, so they meet text contrast (4.5:1), above the 3:1 icons need, on
    /// every background a list shows them on, in both themes.
    #[test]
    fn status_and_text_colours_meet_contrast_in_both_themes() {
        let map = properties();
        let get = |name: &str, dark: bool| resolve(&map, &map[name], dark);
        let mut foregrounds: Vec<&str> = Tone::ALL.iter().map(|t| t.token()).collect();
        foregrounds.extend(["--color-text", "--color-text-secondary", "--color-action"]);
        for dark in [false, true] {
            for bg in ["--color-canvas", "--color-surface", "--color-selected"] {
                for fg in &foregrounds {
                    let ratio = contrast(get(fg, dark), get(bg, dark));
                    assert!(
                        ratio >= 4.5,
                        "{fg} on {bg} is {ratio:.2}:1 in the {} theme",
                        if dark { "dark" } else { "light" }
                    );
                }
            }
            let on_action = contrast(get("--color-on-action", dark), get("--color-action", dark));
            assert!(on_action >= 4.5, "a primary button is {on_action:.2}:1");
        }
    }
}
