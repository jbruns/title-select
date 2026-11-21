//! The arcade selector's settings (ADR 0027): the `arcade` block of a Selection Profile, globally and per system, and
//! the names it uses.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// How well a driver works, as MAME rates it, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverStatus {
    Preliminary,
    Imperfect,
    Good,
}

impl DriverStatus {
    pub const ALL: [DriverStatus; 3] = [
        DriverStatus::Good,
        DriverStatus::Imperfect,
        DriverStatus::Preliminary,
    ];

    /// The name ListXML gives it.
    pub fn as_str(self) -> &'static str {
        match self {
            DriverStatus::Good => "good",
            DriverStatus::Imperfect => "imperfect",
            DriverStatus::Preliminary => "preliminary",
        }
    }
}

/// A kind of machine the category filter can drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Computers, consoles, handhelds, calculators, chess computers, instruments and other machines that aren't arcade
    /// games.
    NonArcade,
    /// Gambling and redemption machines.
    Gambling,
    /// Mahjong and hanafuda machines.
    Tabletop,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::NonArcade, Category::Gambling, Category::Tabletop];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::NonArcade => "non-arcade",
            Category::Gambling => "gambling",
            Category::Tabletop => "tabletop",
        }
    }
}

/// A machine that isn't the release it copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Bootleg,
    Hack,
    Prototype,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::Bootleg, Variant::Hack, Variant::Prototype];

    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Bootleg => "bootleg",
            Variant::Hack => "hack",
            Variant::Prototype => "prototype",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// One step of the election of a family's winner, each one Rule. Ties after the last step go to the shortname that
/// sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElectionStep {
    /// Driver status, good over imperfect.
    Status,
    Region,
    Language,
    /// The parent over its clones.
    Parent,
    /// The newest revision.
    Revision,
    /// Not a bootleg or hack.
    Variant,
}

impl ElectionStep {
    /// ADR 0027's order.
    pub const ALL: [ElectionStep; 6] = [
        ElectionStep::Status,
        ElectionStep::Region,
        ElectionStep::Language,
        ElectionStep::Parent,
        ElectionStep::Revision,
        ElectionStep::Variant,
    ];

    /// The step as you read it, in the workbench's editor, its Rules and the problems a Profile check finds.
    pub fn label(self) -> &'static str {
        match self {
            ElectionStep::Status => "Best driver status",
            ElectionStep::Region => "Preferred region",
            ElectionStep::Language => "Preferred language",
            ElectionStep::Parent => "Parent over clone",
            ElectionStep::Revision => "Newest revision",
            ElectionStep::Variant => "Original over bootleg or hack",
        }
    }

    /// The step's stage, as its Rules name it.
    pub fn stage(self) -> &'static str {
        match self {
            ElectionStep::Status => "election:status",
            ElectionStep::Region => "election:region",
            ElectionStep::Language => "election:language",
            ElectionStep::Parent => "election:parent",
            ElectionStep::Revision => "election:revision",
            ElectionStep::Variant => "election:variant",
        }
    }
}

/// An inclusive range; an open end is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Range {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

impl Range {
    pub fn is_open(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    pub fn contains(&self, n: u32) -> bool {
        self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
    }

    pub fn describe(&self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!("{min} to {max}"),
            (Some(min), None) => format!("{min} or more"),
            (None, Some(max)) => format!("{max} or fewer"),
            (None, None) => "any number".into(),
        }
    }
}

/// The region names the arcade selector reads from MAME descriptions, in its default order.
pub const REGIONS: &[&str] = &[
    "World",
    "USA",
    "Europe",
    "Japan",
    "Asia",
    "Korea",
    "Taiwan",
    "Hong Kong",
    "China",
    "Brazil",
    "Hispanic",
    "Spain",
    "France",
    "Germany",
    "Italy",
    "UK",
    "Australia",
];

/// The language names the arcade selector reads from MAME descriptions, in its default order.
pub const LANGUAGES: &[&str] = &[
    "English",
    "Japanese",
    "Spanish",
    "French",
    "German",
    "Italian",
    "Portuguese",
    "Korean",
    "Chinese",
];

/// The control types ListXML names, which the controls filters take.
pub const CONTROL_TYPES: &[&str] = &[
    "joy",
    "doublejoy",
    "triplejoy",
    "stick",
    "paddle",
    "dial",
    "trackball",
    "lightgun",
    "positional",
    "pedal",
    "mouse",
    "keypad",
    "keyboard",
    "mahjong",
    "hanafuda",
    "gambling",
    "only_buttons",
];

/// What the arcade selector keeps and prefers. A setting left out of the block takes its default, ADR 0027's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArcadeSettings {
    pub drop_mechanical: bool,
    /// Drop machines with no screen.
    pub drop_screenless: bool,
    /// Drop machines that name a software list: computers and consoles.
    pub drop_software_lists: bool,
    pub drop_categories: BTreeSet<Category>,
    /// Drop machines whose driver status is below this; `None` keeps every status.
    pub minimum_driver_status: Option<DriverStatus>,
    pub drop_variants: BTreeSet<Variant>,
    pub years: Range,
    pub players: Range,
    /// Keep only machines with at least one of these control types; empty keeps every machine.
    pub require_controls: BTreeSet<String>,
    /// Drop machines with any of these control types.
    pub forbid_controls: BTreeSet<String>,
    /// Keep only machines whose first screen is this way round; `None` keeps both.
    pub orientation: Option<Orientation>,
    /// Drop machines that need Disks (CHDs).
    pub drop_disks: bool,
    /// Drop machines that play samples.
    pub drop_samples: bool,
    /// The election's steps, in order; a step left out doesn't run. The shortname breaks any tie left.
    pub election: Vec<ElectionStep>,
    /// Regions in order of preference, from [`REGIONS`]; a region left out ranks after those listed.
    pub region_order: Vec<String>,
    /// Languages in order of preference, from [`LANGUAGES`].
    pub language_order: Vec<String>,
    /// Machines kept whatever the filters say, but for BIOSes, devices and machines that can't run, in the override
    /// syntax: plain text matches part of a shortname or description, `|` all of one, `/` a regular expression.
    pub include: Vec<String>,
    /// Machines dropped, in the same syntax; `include` wins.
    pub exclude: Vec<String>,
    /// Keep only the families the curated list names, preferring the machine it names. Inert until the selector
    /// reads the list (#251): the Selection records it as unavailable.
    pub curated: bool,
    /// Drop families rated below this by bestgames. Inert until the selector reads bestgames (#251).
    pub minimum_rating: Option<u32>,
    /// Drop families bestgames doesn't rate, when `minimum_rating` is set.
    pub drop_unrated: bool,
}

impl Default for ArcadeSettings {
    fn default() -> Self {
        ArcadeSettings {
            drop_mechanical: true,
            drop_screenless: true,
            drop_software_lists: true,
            drop_categories: Category::ALL.into(),
            minimum_driver_status: Some(DriverStatus::Imperfect),
            drop_variants: BTreeSet::new(),
            years: Range::default(),
            players: Range::default(),
            require_controls: BTreeSet::new(),
            forbid_controls: BTreeSet::new(),
            orientation: None,
            drop_disks: false,
            drop_samples: false,
            election: ElectionStep::ALL.to_vec(),
            region_order: REGIONS.iter().map(|r| r.to_string()).collect(),
            language_order: LANGUAGES.iter().map(|l| l.to_string()).collect(),
            include: Vec::new(),
            exclude: Vec::new(),
            curated: false,
            minimum_rating: None,
            drop_unrated: false,
        }
    }
}

impl ArcadeSettings {
    /// What the selector can't read in these settings, one message each, as you read it after "your arcade
    /// settings": a step or name listed twice, or a region, language or control type it doesn't know. The include
    /// and exclude lists' patterns are the selector's to check.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen = BTreeSet::new();
        for step in &self.election {
            if !seen.insert(*step) {
                problems.push(format!("election order: {} is listed twice", step.label()));
            }
        }
        for (list, names, known) in [
            ("region", &self.region_order, REGIONS),
            ("language", &self.language_order, LANGUAGES),
        ] {
            let mut seen = BTreeSet::new();
            for name in names {
                if !known.contains(&name.as_str()) {
                    problems.push(format!(
                        "{list} order: {name} is not a {list} the arcade selection reads"
                    ));
                } else if !seen.insert(name) {
                    problems.push(format!("{list} order: {name} is listed twice"));
                }
            }
        }
        for (list, controls) in [
            ("required controls", &self.require_controls),
            ("forbidden controls", &self.forbid_controls),
        ] {
            for control in controls {
                if !CONTROL_TYPES.contains(&control.as_str()) {
                    problems.push(format!(
                        "{list}: {control} is not a control type ListXML names"
                    ));
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_have_no_problems() {
        assert_eq!(ArcadeSettings::default().problems(), Vec::<String>::new());
    }

    #[test]
    fn problems_name_each_step_or_name_the_selector_cant_read() {
        let settings = ArcadeSettings {
            election: vec![ElectionStep::Region, ElectionStep::Region],
            region_order: vec!["World".into(), "Atlantis".into(), "World".into()],
            language_order: vec!["Klingon".into()],
            forbid_controls: ["telepathy".to_owned()].into(),
            ..ArcadeSettings::default()
        };
        assert_eq!(
            settings.problems(),
            [
                "election order: Preferred region is listed twice",
                "region order: Atlantis is not a region the arcade selection reads",
                "region order: World is listed twice",
                "language order: Klingon is not a language the arcade selection reads",
                "forbidden controls: telepathy is not a control type ListXML names",
            ]
        );
    }
}
