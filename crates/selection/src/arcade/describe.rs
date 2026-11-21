//! What a MAME description says in its parentheses, such as "(World 910522)", "(US, rev B)" or "(bootleg)": the
//! regions, languages, revision and variant the election reads. ListXML has no structured region, so this is free text
//! read by MAME's naming conventions (research 0105 §3), and a description that says nothing yields nothing.

use title_select_api::arcade::{LANGUAGES, Variant};

/// Region names as the election orders them by default, `arcade::REGIONS`, each with the spellings MAME descriptions
/// use.
const REGIONS: &[(&str, &[&str])] = &[
    ("World", &["World"]),
    ("USA", &["USA", "US", "America", "North America"]),
    ("Europe", &["Europe", "Euro"]),
    ("Japan", &["Japan"]),
    ("Asia", &["Asia"]),
    ("Korea", &["Korea"]),
    ("Taiwan", &["Taiwan"]),
    ("Hong Kong", &["Hong Kong"]),
    ("China", &["China"]),
    ("Brazil", &["Brazil"]),
    ("Hispanic", &["Hispanic", "Latin America"]),
    ("Spain", &["Spain"]),
    ("France", &["France"]),
    ("Germany", &["Germany"]),
    ("Italy", &["Italy"]),
    ("UK", &["UK"]),
    ("Australia", &["Australia"]),
];

/// What a description says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    /// The regions it names, as `arcade::REGIONS` spells them.
    pub regions: Vec<String>,
    pub languages: Vec<String>,
    /// Its version, revision and date, newest highest; all zeros when it names none.
    pub revision: [u32; 5],
    pub variants: Vec<Variant>,
}

/// Reads `description`, and `manufacturer`, which MAME sets to "bootleg" for a bootleg.
pub fn describe(description: &str, manufacturer: Option<&str>) -> Described {
    let mut out = Described::default();
    for group in parenthesised(description) {
        for part in group.split(',').map(str::trim) {
            for piece in part.split('/').map(str::trim) {
                if let Some(region) = region(piece)
                    && !out.regions.contains(&region)
                {
                    out.regions.push(region);
                }
            }
            if let Some(language) = language(part)
                && !out.languages.contains(&language)
            {
                out.languages.push(language);
            }
            revision(part, &mut out.revision);
            for word in part.split_whitespace() {
                let variant = match word.to_ascii_lowercase().as_str() {
                    "bootleg" => Some(Variant::Bootleg),
                    "hack" => Some(Variant::Hack),
                    "prototype" | "proto" => Some(Variant::Prototype),
                    _ => None,
                };
                if let Some(v) = variant.filter(|v| !out.variants.contains(v)) {
                    out.variants.push(v);
                }
            }
        }
    }
    if manufacturer.is_some_and(|m| m.to_ascii_lowercase().contains("bootleg"))
        && !out.variants.contains(&Variant::Bootleg)
    {
        out.variants.push(Variant::Bootleg);
    }
    out
}

/// The text inside each top-level pair of parentheses.
fn parenthesised(text: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(&text[start..i]);
                }
            }
            _ => {}
        }
    }
    groups
}

/// The region `part` begins with, when what follows it qualifies the release rather than continuing a title: "US set
/// 1" and "World 910522" name regions, "World Heroes 2" doesn't.
fn region(part: &str) -> Option<String> {
    REGIONS.iter().find_map(|(name, spellings)| {
        spellings.iter().find_map(|spelling| {
            let rest = part.strip_prefix(spelling)?;
            let qualifies = rest.is_empty()
                || rest.strip_prefix(' ').is_some_and(|rest| {
                    let word = rest.split_whitespace().next().unwrap_or("");
                    word.starts_with(|c: char| c.is_ascii_digit() || c.is_lowercase())
                        || matches!(
                            word.trim_end_matches('.'),
                            "Rev" | "Revision" | "Ver" | "Version" | "Set" | "V"
                        )
                });
            qualifies.then(|| name.to_string())
        })
    })
}

fn language(part: &str) -> Option<String> {
    let word = part
        .strip_suffix(" version")
        .or_else(|| part.strip_suffix(" Version"))
        .unwrap_or(part);
    LANGUAGES
        .iter()
        .find(|l| **l == word)
        .map(|l| l.to_string())
}

/// Reads "ver 1.2", "v1.2", "rev B", "Rev. 2" and a six- or eight-digit date into `[major, minor, patch, revision,
/// date]`, the date as `YYYYMMDD` (see [`date`]).
fn revision(part: &str, out: &mut [u32; 5]) {
    let words: Vec<&str> = part.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let lower = word.trim_end_matches('.').to_ascii_lowercase();
        let next = words.get(i + 1).copied();
        match lower.as_str() {
            "v" | "ver" | "version" => {
                if let Some(v) = next.and_then(version) {
                    out[..3].copy_from_slice(&v);
                }
            }
            "rev" | "revision" => {
                if let Some(r) = next.and_then(rev) {
                    out[3] = r;
                }
            }
            _ => {
                if let Some(v) = lower
                    .strip_prefix('v')
                    .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                    .and_then(version)
                {
                    out[..3].copy_from_slice(&v);
                } else if let Some(date) = date(word) {
                    out[4] = date;
                }
            }
        }
    }
}

/// A `YYMMDD` or `YYYYMMDD` date as `YYYYMMDD`, so both forms compare as one calendar. Research 0105 doesn't say how
/// a date is read: MAME's arcade machines date from the 1970s on, so a two-digit year from 70 is in the 1900s and one
/// below 70 in the 2000s. A number that is no calendar date, such as `000000` or `991332`, is none: like any revision
/// that can't be read, it ranks as naming none.
fn date(word: &str) -> Option<u32> {
    if !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = word.parse().ok()?;
    let date = match word.len() {
        8 => n,
        6 if n / 10_000 >= 70 => 19_000_000 + n,
        6 => 20_000_000 + n,
        _ => return None,
    };
    let (year, month, day) = (date / 10_000, date / 100 % 100, date % 100);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&day).then_some(date)
}

fn version(text: &str) -> Option<[u32; 3]> {
    let mut out = [0; 3];
    let mut parts = text.split('.');
    for slot in &mut out {
        match parts.next() {
            Some(p) if !p.is_empty() => {
                let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
                *slot = digits.parse().ok()?;
            }
            _ => break,
        }
    }
    Some(out)
}

/// A revision letter, A first, or number.
fn rev(text: &str) -> Option<u32> {
    let text = text.trim_end_matches([')', ',']);
    if let Ok(n) = text.parse() {
        return Some(n);
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => {
            Some(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_regions_read_are_the_ones_a_profile_names() {
        let names: Vec<&str> = REGIONS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, title_select_api::arcade::REGIONS);
    }

    fn regions(description: &str) -> Vec<String> {
        describe(description, None).regions
    }

    #[test]
    fn regions_are_read_from_the_parentheses_by_mames_spellings() {
        assert_eq!(
            regions("Street Fighter II: The World Warrior (World 910522)"),
            ["World"]
        );
        assert_eq!(regions("Shooter (US set 1)"), ["USA"]);
        assert_eq!(regions("Shooter (US, rev B)"), ["USA"]);
        assert_eq!(regions("Shooter (US/Europe)"), ["USA", "Europe"]);
        assert_eq!(regions("Puck Man (Japan set 1)"), ["Japan"]);
        assert!(regions("World Heroes (World Heroes 2)").is_empty());
        assert!(regions("Pac-Man (Midway)").is_empty());
        assert!(regions("World Cup").is_empty());
    }

    #[test]
    fn revisions_rank_newest_highest() {
        let rev = |d: &str| describe(d, None).revision;
        assert!(rev("Shooter (US, rev B)") > rev("Shooter (US, rev A)"));
        assert!(rev("Shooter (US, rev A)") > rev("Shooter (US)"));
        assert!(rev("Shooter (ver 1.2)") > rev("Shooter (ver 1.1)"));
        assert!(rev("Shooter (v1.10)") > rev("Shooter (v1.2)"));
        assert_eq!(rev("Shooter (US, V 1.2)"), rev("Shooter (US, ver 1.2)"));
        assert!(rev("Fighter (World 910522)") > rev("Fighter (World 910411)"));
    }

    #[test]
    fn six_and_eight_digit_dates_compare_as_one_calendar() {
        let rev = |d: &str| describe(d, None).revision;
        assert!(rev("Fighter (World 990101)") > rev("Fighter (World 19900101)"));
        assert_eq!(
            rev("Fighter (World 990101)"),
            rev("Fighter (World 19990101)")
        );
        // Two-digit years from 70 are the 1900s, the rest the 2000s.
        assert!(rev("Fighter (World 010101)") > rev("Fighter (World 991231)"));
        assert_eq!(
            rev("Fighter (World 700101)"),
            rev("Fighter (World 19700101)")
        );
        assert_eq!(
            rev("Fighter (World 691231)"),
            rev("Fighter (World 20691231)")
        );
        assert_eq!(
            describe("Fighter (World 990101)", None).revision[4],
            19990101
        );
    }

    #[test]
    fn a_number_that_is_no_calendar_date_names_no_date() {
        let date = |d: &str| describe(d, None).revision[4];
        for impossible in [
            "000000", "991332", "990001", "991300", "990230", "19990230", "20000000", "19991232",
        ] {
            assert_eq!(
                date(&format!("Fighter (World {impossible})")),
                0,
                "{impossible}"
            );
        }
        assert_eq!(date("Fighter (World 960229)"), 19960229);
        assert_eq!(date("Fighter (World 20000229)"), 20000229);
        assert_eq!(date("Fighter (World 990229)"), 0);
        assert_eq!(date("Fighter (World 19000229)"), 0);
        assert_eq!(date("Fighter (World 991231)"), 19991231);
    }

    #[test]
    fn variants_come_from_the_parentheses_and_a_bootleg_manufacturer() {
        assert_eq!(
            describe("Shooter (World, bootleg)", None).variants,
            [Variant::Bootleg]
        );
        assert_eq!(describe("Shooter (hack)", None).variants, [Variant::Hack]);
        assert_eq!(
            describe("Proto Blaster (prototype)", None).variants,
            [Variant::Prototype]
        );
        assert_eq!(
            describe("Shooter (set 2)", Some("bootleg (Acme)")).variants,
            [Variant::Bootleg]
        );
        assert!(describe("Proto Blaster", None).variants.is_empty());
    }

    #[test]
    fn languages_are_read_only_where_the_description_names_one() {
        assert_eq!(
            describe("Sailor (Asia, English)", None).languages,
            ["English"]
        );
        assert!(describe("Sailor (Japan)", None).languages.is_empty());
        assert_eq!(
            describe("Sailor (English, English version)", None).languages,
            ["English"]
        );
    }
}
