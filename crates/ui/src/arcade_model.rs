//! The arcade Selection's pure logic (ADR 0027): its stages in plain words, its Dependencies summed up in one line, and
//! the Profile editor's `arcade` block for a system.

use std::collections::BTreeMap;

use title_select_api::GameStatus;
use title_select_api::arcade::{ArcadeSettings, Category, DriverStatus, ElectionStep, Variant};
use title_select_api::selection::{
    ArcadePins, CategorySource, DEPENDENCY_STAGE, Dependency, Selection, SelectionProfile,
};

/// An arcade Rule's stage in plain words, or `None` for a stage the arcade selector doesn't name.
pub fn stage_label(stage: &str) -> Option<&'static str> {
    Some(match stage {
        "structural" => "Not a game",
        "list" => "Your lists",
        "hardware" => "Hardware",
        "category" => "Category",
        "quality" => "Driver status",
        "variant" => "Variant",
        "year" => "Year",
        "players" => "Players",
        "controls" => "Controls",
        "orientation" => "Screen",
        "media" => "Disks and samples",
        "election" => "Only one left",
        "election:shortname" => "Tie-break by shortname",
        DEPENDENCY_STAGE => "Dependency",
        _ => {
            return ElectionStep::ALL
                .into_iter()
                .find(|s| s.stage() == stage)
                .map(ElectionStep::label);
        }
    })
}

/// A filter the arcade Selection records as unavailable, in plain words.
pub fn filter_label(filter: &str) -> &str {
    match filter {
        "mechanical" => "mechanical machines",
        "screenless" => "machines with no screen",
        "software_lists" => "software-list machines",
        "category" => "category",
        "quality" => "driver status",
        "year" => "year",
        "players" => "players",
        "controls" => "controls",
        "orientation" => "screen orientation",
        "samples" => "samples",
        "curated" => "the curated list",
        "rating" => "minimum rating",
        other => other,
    }
}

/// What an arcade Selection's pins say you should know, one sentence each: where categories came from, and the
/// filters that dropped nothing because there was nothing to read.
pub fn notices(pins: &ArcadePins) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(CategorySource::ListxmlHeuristic) = pins.category {
        out.push(
            "Categories come from what the ListXML says about each machine: its driver's folder, software lists and \
             controls. It can miss quiz, mature and most tabletop machines."
                .to_owned(),
        );
    }
    if !pins.unavailable.is_empty() {
        let filters: Vec<&str> = pins.unavailable.iter().map(|f| filter_label(f)).collect();
        out.push(format!(
            "These filters had nothing to read, so they dropped nothing: {}.",
            filters.join(", ")
        ));
    }
    out
}

/// How many Dependencies some Games need, by what the latest Audit says of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub complete: usize,
    pub incomplete: usize,
    pub missing: usize,
    /// Not in the latest Audit, or not audited.
    pub unknown: usize,
}

impl Needs {
    /// Counts `dependencies` by their status in `games`.
    pub fn of<'a>(
        dependencies: impl IntoIterator<Item = &'a Dependency>,
        games: &BTreeMap<String, GameStatus>,
    ) -> Needs {
        let mut needs = Needs::default();
        for d in dependencies {
            match games.get(&d.title) {
                Some(GameStatus::Complete) => needs.complete += 1,
                Some(GameStatus::Incomplete) => needs.incomplete += 1,
                Some(GameStatus::Missing) => needs.missing += 1,
                None => needs.unknown += 1,
            }
        }
        needs
    }

    pub fn count(&self) -> usize {
        self.complete + self.incomplete + self.missing + self.unknown
    }

    /// The count and what you have of them: "3 dependencies, all complete", "2 dependencies: 1 missing, 1 not
    /// audited". `None` when there are none.
    pub fn summary(&self) -> Option<String> {
        let n = self.count();
        if n == 0 {
            return None;
        }
        let what = if n == 1 { "dependency" } else { "dependencies" };
        if self.complete == n {
            let all = if n == 1 { "complete" } else { "all complete" };
            return Some(format!("{n} {what}, {all}"));
        }
        let parts: Vec<String> = [
            (self.missing, "missing"),
            (self.incomplete, "incomplete"),
            (self.unknown, "not audited"),
            (self.complete, "complete"),
        ]
        .into_iter()
        .filter(|(k, _)| *k > 0)
        .map(|(k, label)| format!("{k} {label}"))
        .collect();
        Some(format!("{n} {what}: {}", parts.join(", ")))
    }
}

/// Every Dependency of `selection`, in group order.
pub fn dependencies(selection: &Selection) -> Vec<&Dependency> {
    selection
        .groups
        .iter()
        .flat_map(|g| &g.dependencies)
        .collect()
}

/// The Dependencies the Game `title` needs.
pub fn dependencies_of<'a>(selection: &'a Selection, title: &str) -> Vec<&'a Dependency> {
    dependencies(selection)
        .into_iter()
        .filter(|d| d.needed_by.iter().any(|n| n == title))
        .collect()
}

pub fn category_label(category: Category) -> &'static str {
    match category {
        Category::NonArcade => "Not arcade games: computers, consoles, handhelds, instruments",
        Category::Gambling => "Gambling and redemption",
        Category::Tabletop => "Mahjong and hanafuda",
    }
}

pub fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Bootleg => "Bootlegs",
        Variant::Hack => "Hacks",
        Variant::Prototype => "Prototypes",
    }
}

pub fn driver_status_label(status: Option<DriverStatus>) -> &'static str {
    match status {
        Some(DriverStatus::Good) => "Good only",
        Some(DriverStatus::Imperfect) => "Imperfect or better",
        Some(DriverStatus::Preliminary) | None => "Any status",
    }
}

/// Where a system's arcade settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcadeSource {
    Defaults,
    Global,
    System,
}

impl ArcadeSource {
    pub fn of(profile: &SelectionProfile, system: &str) -> ArcadeSource {
        if profile
            .systems
            .get(system)
            .is_some_and(|s| s.arcade.is_some())
        {
            ArcadeSource::System
        } else if profile.arcade.is_some() {
            ArcadeSource::Global
        } else {
            ArcadeSource::Defaults
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArcadeSource::Defaults => "Defaults",
            ArcadeSource::Global => "From global settings",
            ArcadeSource::System => "Set for this system",
        }
    }
}

/// Changes `system`'s arcade settings, starting from the ones that apply to it now, so that the change applies to
/// this system only.
pub fn update_arcade(
    profile: &mut SelectionProfile,
    system: &str,
    f: impl FnOnce(&mut ArcadeSettings),
) {
    let mut settings = profile.arcade_settings(system);
    f(&mut settings);
    profile.systems.entry(system.to_owned()).or_default().arcade = Some(settings);
}

/// Drops `system`'s own arcade settings, so the global ones, or the defaults, apply.
pub fn use_global_arcade(profile: &mut SelectionProfile, system: &str) {
    let Some(own) = profile.systems.get_mut(system) else {
        return;
    };
    own.arcade = None;
    if own.is_empty() {
        profile.systems.remove(system);
    }
}

/// Sets `system`'s arcade settings to the defaults, whatever the global ones say.
pub fn reset_arcade(profile: &mut SelectionProfile, system: &str) {
    profile.systems.entry(system.to_owned()).or_default().arcade = Some(ArcadeSettings::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::{Rule, SelectionGroup, SystemSettings};

    fn dependency(title: &str, needed_by: &[&str]) -> Dependency {
        Dependency {
            title: title.into(),
            needed_by: needed_by.iter().map(|n| n.to_string()).collect(),
            rule: Rule {
                stage: DEPENDENCY_STAGE.into(),
                reference: None,
                message: format!("needed by {}", needed_by.join(", ")),
            },
        }
    }

    fn statuses(of: &[(&str, GameStatus)]) -> BTreeMap<String, GameStatus> {
        of.iter().map(|(t, s)| (t.to_string(), *s)).collect()
    }

    #[test]
    fn arcade_stages_read_in_plain_words_and_retool_stages_are_left_alone() {
        assert_eq!(stage_label("election:parent"), Some("Parent over clone"));
        assert_eq!(stage_label("structural"), Some("Not a game"));
        assert_eq!(stage_label(DEPENDENCY_STAGE), Some("Dependency"));
        assert_eq!(stage_label("Parent selection"), None);
        for step in ElectionStep::ALL {
            assert!(stage_label(step.stage()).is_some(), "{step:?}");
        }
    }

    #[test]
    fn dependencies_sum_up_in_one_line() {
        let deps = [
            dependency("neogeo", &["mslug"]),
            dependency("z80", &["mslug"]),
            dependency("ym2610", &["mslug"]),
        ];
        let all = statuses(&[
            ("neogeo", GameStatus::Complete),
            ("z80", GameStatus::Complete),
            ("ym2610", GameStatus::Complete),
        ]);
        assert_eq!(
            Needs::of(&deps, &all).summary().as_deref(),
            Some("3 dependencies, all complete")
        );
        let some = statuses(&[
            ("neogeo", GameStatus::Missing),
            ("z80", GameStatus::Complete),
        ]);
        assert_eq!(
            Needs::of(&deps, &some).summary().as_deref(),
            Some("3 dependencies: 1 missing, 1 not audited, 1 complete")
        );
        assert_eq!(
            Needs::of(&deps[..1], &all).summary().as_deref(),
            Some("1 dependency, complete")
        );
        assert_eq!(Needs::of(&[], &all).summary(), None);
    }

    #[test]
    fn a_games_dependencies_are_those_that_name_it() {
        let mut bios = SelectionGroup {
            group: "neogeo".into(),
            unresolved: false,
            winners: Vec::new(),
            alternatives: Vec::new(),
            exclusions: Vec::new(),
            dependencies: vec![dependency("neogeo", &["kof98", "mslug"])],
            choice: None,
            drift: false,
        };
        let mut devices = bios.clone();
        devices.group = "z80".into();
        devices.dependencies = vec![dependency("z80", &["pacman"])];
        bios.group = "neogeo".into();
        let selection = Selection {
            pins: Default::default(),
            dat_sha256: String::new(),
            system: "MAME".into(),
            system_settings: false,
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups: vec![bios, devices],
            stale_choices: Default::default(),
        };
        assert_eq!(dependencies(&selection).len(), 2);
        let titles = |t| {
            dependencies_of(&selection, t)
                .iter()
                .map(|d| d.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("mslug"), ["neogeo"]);
        assert_eq!(titles("pacman"), ["z80"]);
        assert!(titles("galaga").is_empty());
    }

    #[test]
    fn the_notices_say_where_categories_came_from_and_what_dropped_nothing() {
        let pins = ArcadePins {
            build: "0.289".into(),
            category: Some(CategorySource::ListxmlHeuristic),
            unavailable: vec!["year".into(), "curated".into()],
        };
        let said = notices(&pins);
        assert!(said[0].starts_with("Categories come from what the ListXML says"));
        assert_eq!(
            said[1],
            "These filters had nothing to read, so they dropped nothing: year, the curated list."
        );
        // With the category filter off, nothing says where categories came from.
        let off = ArcadePins {
            category: None,
            unavailable: Vec::new(),
            ..pins
        };
        assert!(notices(&off).is_empty());
    }

    #[test]
    fn a_change_for_one_system_starts_from_what_applies_to_it_and_using_global_drops_it() {
        let mut profile = SelectionProfile {
            arcade: Some(ArcadeSettings {
                drop_samples: true,
                ..ArcadeSettings::default()
            }),
            ..SelectionProfile::default()
        };
        assert_eq!(ArcadeSource::of(&profile, "MAME"), ArcadeSource::Global);
        update_arcade(&mut profile, "MAME", |a| a.drop_disks = true);
        assert_eq!(ArcadeSource::of(&profile, "MAME"), ArcadeSource::System);
        let own = profile.arcade_settings("MAME");
        assert!(own.drop_samples && own.drop_disks);

        use_global_arcade(&mut profile, "MAME");
        assert!(!profile.systems.contains_key("MAME"));
        profile.systems.insert(
            "MAME".into(),
            SystemSettings {
                additions: true,
                ..SystemSettings::default()
            },
        );
        reset_arcade(&mut profile, "MAME");
        assert_eq!(profile.arcade_settings("MAME"), ArcadeSettings::default());
        use_global_arcade(&mut profile, "MAME");
        assert!(profile.systems["MAME"].additions);
        profile.arcade = None;
        assert_eq!(ArcadeSource::of(&profile, "Other"), ArcadeSource::Defaults);
    }
}
