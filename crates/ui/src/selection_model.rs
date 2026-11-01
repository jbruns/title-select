//! The Selection screen's and the Profile editor's pure logic: a Selection's releases indexed for filtering and
//! sorting on every keystroke, the Operator's staged choices, and where each system setting comes from.

use std::collections::{BTreeMap, BTreeSet};

use title_select_api::GameStatus;
use title_select_api::selection::{
    ChoiceStatus, ReleaseRow, Role, Selection, SelectionGroup, SelectionOptions, SelectionProfile,
    SystemSettings,
};

use crate::model::{Sort, status_text};

pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Winner => "Selected",
        Role::Alternative => "Alternative",
        Role::Exclusion => "Excluded",
    }
}

pub fn choice_label(status: ChoiceStatus) -> &'static str {
    match status {
        ChoiceStatus::Applied => "Your choice",
        ChoiceStatus::MatchesDefault => "Matches the default now",
        ChoiceStatus::Conflicting => "Conflicting choice",
        ChoiceStatus::Stale => "Stale choice",
    }
}

/// A group's flags, as a bit set.
pub mod flag {
    pub const UNRESOLVED: u8 = 1;
    pub const STALE: u8 = 2;
    pub const CONFLICTING: u8 = 4;
    pub const DRIFT: u8 = 8;
    /// The group has a choice of yours, in any state.
    pub const CHOICE: u8 = 16;
    pub const NEEDS_REVIEW: u8 = UNRESOLVED | STALE | CONFLICTING;
}

pub fn group_flags(group: &SelectionGroup) -> u8 {
    let mut flags = 0;
    if group.unresolved {
        flags |= flag::UNRESOLVED;
    }
    if group.drift {
        flags |= flag::DRIFT;
    }
    if let Some(choice) = &group.choice {
        flags |= flag::CHOICE;
        match choice.status {
            ChoiceStatus::Stale => flags |= flag::STALE,
            ChoiceStatus::Conflicting => flags |= flag::CONFLICTING,
            ChoiceStatus::Applied | ChoiceStatus::MatchesDefault => {}
        }
    }
    flags
}

/// The words for a group's flags, as the Selection table shows them.
pub fn flag_labels(flags: u8) -> Vec<&'static str> {
    [
        (flag::UNRESOLVED, "Unresolved"),
        (flag::STALE, "Stale choice"),
        (flag::CONFLICTING, "Conflicting choice"),
        (flag::DRIFT, "Coverage drift"),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, label)| label)
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseColumn {
    Group,
    Release,
    Role,
    Availability,
}

/// Which releases to show by availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityFilter {
    Is(GameStatus),
    /// No audit against the Selection's DAT Revision says.
    Unknown,
}

/// Which groups to show by their flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagFilter {
    NeedsReview,
    Unresolved,
    Stale,
    Conflicting,
    Drift,
    Choice,
}

impl FlagFilter {
    pub const ALL: [FlagFilter; 6] = [
        FlagFilter::NeedsReview,
        FlagFilter::Unresolved,
        FlagFilter::Stale,
        FlagFilter::Conflicting,
        FlagFilter::Drift,
        FlagFilter::Choice,
    ];

    pub fn key(self) -> &'static str {
        match self {
            FlagFilter::NeedsReview => "needs-review",
            FlagFilter::Unresolved => "unresolved",
            FlagFilter::Stale => "stale",
            FlagFilter::Conflicting => "conflicting",
            FlagFilter::Drift => "drift",
            FlagFilter::Choice => "choice",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FlagFilter::NeedsReview => "Needs review",
            FlagFilter::Unresolved => "Unresolved",
            FlagFilter::Stale => "Stale choice",
            FlagFilter::Conflicting => "Conflicting choice",
            FlagFilter::Drift => "Coverage drift",
            FlagFilter::Choice => "Your choices",
        }
    }

    pub fn parse(key: &str) -> Option<FlagFilter> {
        FlagFilter::ALL.into_iter().find(|f| f.key() == key)
    }

    fn bits(self) -> u8 {
        match self {
            FlagFilter::NeedsReview => flag::NEEDS_REVIEW,
            FlagFilter::Unresolved => flag::UNRESOLVED,
            FlagFilter::Stale => flag::STALE,
            FlagFilter::Conflicting => flag::CONFLICTING,
            FlagFilter::Drift => flag::DRIFT,
            FlagFilter::Choice => flag::CHOICE,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseFilter {
    pub text: String,
    pub role: Option<Role>,
    pub availability: Option<AvailabilityFilter>,
    pub flag: Option<FlagFilter>,
}

/// A Selection's releases with the keys filtering and sorting use, computed once when it loads.
#[derive(Debug)]
pub struct SelectionIndex {
    pub selection: Selection,
    pub rows: Vec<ReleaseRow>,
    /// The release's group, as an index into `selection.groups`.
    group: Vec<u32>,
    availability: Vec<Option<GameStatus>>,
    /// Whether the release's availability rests on Claim Evidence.
    claimed: Vec<bool>,
    /// The lowercase group and title the filter searches.
    search: Vec<String>,
    /// The lowercase title, for sorting.
    title: Vec<String>,
    flags: Vec<u8>,
}

impl SelectionIndex {
    /// Indexes the Selection, looking up each release's availability by its name; `games` is empty when no Audit
    /// says, and `claimed` names the Games whose status rests on Claim Evidence.
    pub fn new(
        selection: Selection,
        games: &BTreeMap<String, GameStatus>,
        claimed: &BTreeSet<String>,
    ) -> SelectionIndex {
        let rows = selection.rows();
        let groups: BTreeMap<&str, u32> = selection
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.group.as_str(), i as u32))
            .collect();
        let n = rows.len();
        let mut claimed_of = Vec::with_capacity(n);
        let (mut group, mut availability, mut search, mut title_key, mut flags) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for row in &rows {
            let g = groups[row.group.as_str()];
            group.push(g);
            availability.push(games.get(&row.title).copied());
            claimed_of.push(claimed.contains(&row.title));
            let title = row.title.to_lowercase();
            search.push(format!("{}\n{title}", row.group.to_lowercase()));
            title_key.push(title);
            flags.push(group_flags(&selection.groups[g as usize]));
        }
        SelectionIndex {
            selection,
            rows,
            group,
            availability,
            claimed: claimed_of,
            search,
            title: title_key,
            flags,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn group_of(&self, i: usize) -> &SelectionGroup {
        &self.selection.groups[self.group[i] as usize]
    }

    pub fn availability(&self, i: usize) -> Option<GameStatus> {
        self.availability[i]
    }

    pub fn claimed(&self, i: usize) -> bool {
        self.claimed[i]
    }

    pub fn flags(&self, i: usize) -> u8 {
        self.flags[i]
    }

    /// The group named `name`, if the Selection has it.
    pub fn group(&self, name: &str) -> Option<&SelectionGroup> {
        self.selection
            .groups
            .binary_search_by(|g| g.group.as_str().cmp(name))
            .ok()
            .map(|i| &self.selection.groups[i])
    }

    /// Release indices in the order `sort` asks for, or the Selection's own order.
    pub fn sorted(&self, sort: Option<Sort<ReleaseColumn>>) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.rows.len() as u32).collect();
        let Some(sort) = sort else {
            return order;
        };
        order.sort_by(|&a, &b| {
            let (a, b) = (a as usize, b as usize);
            let ord = match sort.column {
                ReleaseColumn::Group => self.group[a].cmp(&self.group[b]),
                ReleaseColumn::Release => self.title[a].cmp(&self.title[b]),
                ReleaseColumn::Role => self.rows[a].role.cmp(&self.rows[b].role),
                ReleaseColumn::Availability => availability_rank(self.availability[a])
                    .cmp(&availability_rank(self.availability[b])),
            }
            .then(a.cmp(&b));
            if sort.descending { ord.reverse() } else { ord }
        });
        order
    }

    /// The releases of `order` that `filter` keeps.
    pub fn filtered(&self, order: &[u32], filter: &ReleaseFilter) -> Vec<u32> {
        let text = filter.text.trim().to_lowercase();
        order
            .iter()
            .copied()
            .filter(|&i| {
                let i = i as usize;
                filter.role.is_none_or(|r| self.rows[i].role == r)
                    && filter.availability.is_none_or(|a| match a {
                        AvailabilityFilter::Is(s) => self.availability[i] == Some(s),
                        AvailabilityFilter::Unknown => self.availability[i].is_none(),
                    })
                    && filter.flag.is_none_or(|f| self.flags[i] & f.bits() != 0)
                    && (text.is_empty() || self.search[i].contains(&text))
            })
            .collect()
    }
}

fn availability_rank(status: Option<GameStatus>) -> u8 {
    match status {
        Some(GameStatus::Complete) => 0,
        Some(GameStatus::Incomplete) => 1,
        Some(GameStatus::Missing) => 2,
        None => 3,
    }
}

pub fn availability_label(status: Option<GameStatus>, claimed: bool) -> String {
    status.map_or(String::new(), |s| status_text(s, claimed))
}

/// The releases checked in a group as you have staged them: your staged choice, else the engine's picks.
pub fn checked(
    profile: &SelectionProfile,
    system: &str,
    group: &SelectionGroup,
) -> BTreeSet<String> {
    profile
        .choices
        .get(system)
        .and_then(|groups| groups.get(&group.group))
        .cloned()
        .unwrap_or_else(|| group.picks())
}

/// Checks or unchecks one release of a group in the staged Profile.
pub fn toggle(
    profile: &mut SelectionProfile,
    system: &str,
    group: &SelectionGroup,
    title: &str,
    on: bool,
) {
    let mut set = checked(profile, system, group);
    if on {
        set.insert(title.to_owned());
    } else {
        set.remove(title);
    }
    profile.set_choice(
        system,
        &group.group,
        set,
        &group.picks(),
        group.engine_unresolved(),
    );
}

/// Confirms the releases checked in an unresolved group, so that it no longer needs review.
pub fn confirm(profile: &mut SelectionProfile, system: &str, group: &SelectionGroup) {
    let set = checked(profile, system, group);
    profile
        .choices
        .entry(system.to_owned())
        .or_default()
        .insert(group.group.clone(), set);
}

/// The override that includes exactly one release, by its full name.
pub fn include_override(title: &str) -> String {
    format!("|{title}")
}

/// Adds an include override for an excluded release to the system's settings, and checks the release.
pub fn include_and_check(
    profile: &mut SelectionProfile,
    system: &str,
    group: &SelectionGroup,
    title: &str,
) {
    let rule = include_override(title);
    let include = &mut profile
        .systems
        .entry(system.to_owned())
        .or_default()
        .overrides
        .include;
    if !include.contains(&rule) {
        include.push(rule);
    }
    let mut set = checked(profile, system, group);
    set.insert(title.to_owned());
    profile
        .choices
        .entry(system.to_owned())
        .or_default()
        .insert(group.group.clone(), set);
}

/// How many groups of `system` your staged choices change, and whether its settings changed, compared with `saved`.
pub fn staged_changes(saved: &SelectionProfile, staged: &SelectionProfile, system: &str) -> usize {
    let empty = BTreeMap::new();
    let before = saved.choices.get(system).unwrap_or(&empty);
    let after = staged.choices.get(system).unwrap_or(&empty);
    let groups: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let choices = groups
        .into_iter()
        .filter(|g| before.get(*g) != after.get(*g))
        .count();
    let settings = usize::from(saved.systems.get(system) != staged.systems.get(system));
    let elsewhere = usize::from(
        saved
            .choices
            .iter()
            .any(|(s, c)| s != system && staged.choices.get(s) != Some(c))
            || staged
                .choices
                .keys()
                .any(|s| s != system && !saved.choices.contains_key(s)),
    );
    choices + settings + elsewhere
}

/// Where a system setting comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Global,
    System,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Global => "From global settings",
            Source::System => "Set for this system",
        }
    }
}

/// The system settings a system tab edits: one per field that can replace the global one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    RegionOrder,
    LanguageOrder,
    LocalizationOrder,
    VideoOrder,
    Exclude,
    Options,
}

impl Setting {
    pub fn source(self, system: &SystemSettings) -> Source {
        let set = match self {
            Setting::RegionOrder => system.region_order.is_some(),
            Setting::LanguageOrder => system.language_order.is_some(),
            Setting::LocalizationOrder => system.localization_order.is_some(),
            Setting::VideoOrder => system.video_order.is_some(),
            Setting::Exclude => system.exclude.is_some(),
            Setting::Options => system.options.is_some(),
        };
        if set { Source::System } else { Source::Global }
    }

    /// Sets the system's own value to a copy of the global one, so it can be changed for this system.
    pub fn set_for_system(self, profile: &mut SelectionProfile, system: &str) {
        let global = profile.clone();
        let own = profile.systems.entry(system.to_owned()).or_default();
        match self {
            Setting::RegionOrder => own.region_order = Some(global.region_order),
            Setting::LanguageOrder => own.language_order = Some(global.language_order),
            Setting::LocalizationOrder => own.localization_order = Some(global.localization_order),
            Setting::VideoOrder => own.video_order = Some(global.video_order),
            Setting::Exclude => own.exclude = Some(global.exclude),
            Setting::Options => own.options = Some(global.options),
        }
    }

    /// Drops the system's own value, so the global one applies.
    pub fn use_global(self, profile: &mut SelectionProfile, system: &str) {
        let Some(own) = profile.systems.get_mut(system) else {
            return;
        };
        match self {
            Setting::RegionOrder => own.region_order = None,
            Setting::LanguageOrder => own.language_order = None,
            Setting::LocalizationOrder => own.localization_order = None,
            Setting::VideoOrder => own.video_order = None,
            Setting::Exclude => own.exclude = None,
            Setting::Options => own.options = None,
        }
        if own.is_empty() {
            profile.systems.remove(system);
        }
    }
}

/// Drops every setting of a system, so the global settings apply to it. Its choices stay.
pub fn use_global_settings(profile: &mut SelectionProfile, system: &str) {
    profile.systems.remove(system);
}

/// Resets the global settings to the defaults, keeping system settings and choices.
pub fn reset_global(profile: &mut SelectionProfile) {
    *profile = SelectionProfile {
        systems: std::mem::take(&mut profile.systems),
        choices: std::mem::take(&mut profile.choices),
        ..SelectionProfile::default()
    };
}

/// Sets every setting of a system to the defaults, whatever the global settings say, keeping its choices. Global
/// overrides still apply: a system's overrides only add to them.
pub fn reset_system(profile: &mut SelectionProfile, system: &str) {
    profile.systems.insert(
        system.to_owned(),
        SystemSettings {
            region_order: Some(Vec::new()),
            language_order: Some(Vec::new()),
            localization_order: Some(Vec::new()),
            video_order: Some(Vec::new()),
            exclude: Some(BTreeSet::new()),
            options: Some(SelectionOptions::default()),
            replace_post_filters: true,
            ..SystemSettings::default()
        },
    );
}

/// Moves the item at `from` to `to` in an ordered list.
pub fn move_item(list: &mut Vec<String>, from: usize, to: usize) {
    if from >= list.len() || to >= list.len() || from == to {
        return;
    }
    let item = list.remove(from);
    list.insert(to, item);
}

/// The lines of a line editor: trimmed, with blank lines dropped.
pub fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::{
        Alternative, Category, CoverageDrift, Exclusion, GroupChoice, Rule, SelectionOptions,
        SelectionPins, SelectionSummary, Winner,
    };

    fn rule() -> Rule {
        Rule {
            stage: "Parent selection".into(),
            reference: Some("REF0001".into()),
            message: "Higher region".into(),
        }
    }

    fn group(name: &str, winner: &str, alternatives: &[&str], excluded: &[&str]) -> SelectionGroup {
        SelectionGroup {
            group: name.into(),
            unresolved: false,
            winners: vec![Winner {
                title: winner.into(),
                rule: rule(),
                unresolved: false,
            }],
            alternatives: alternatives
                .iter()
                .map(|t| Alternative {
                    title: (*t).into(),
                    clone_of: winner.into(),
                    rule: rule(),
                })
                .collect(),
            exclusions: excluded
                .iter()
                .map(|t| Exclusion {
                    title: (*t).into(),
                    reason: "Demo".into(),
                    rule: rule(),
                })
                .collect(),
            choice: None,
            drift: false,
        }
    }

    fn selection(groups: Vec<SelectionGroup>) -> Selection {
        Selection {
            pins: SelectionPins::default(),
            dat_sha256: String::new(),
            system: "Atari".into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups,
            stale_choices: BTreeMap::new(),
        }
    }

    fn index() -> SelectionIndex {
        let mut combat = group("combat", "Combat (USA)", &[], &[]);
        combat.choice = Some(GroupChoice {
            checked: BTreeSet::new(),
            status: ChoiceStatus::Conflicting,
            picks: vec!["Combat (USA)".into()],
            engine_unresolved: false,
            problems: vec![],
        });
        let mut maze = group("maze", "Maze (USA)", &[], &[]);
        maze.unresolved = true;
        maze.drift = true;
        let games = BTreeMap::from([
            ("Pong (USA)".to_string(), GameStatus::Missing),
            ("Pong (Europe)".to_string(), GameStatus::Complete),
        ]);
        SelectionIndex::new(
            selection(vec![
                combat,
                maze,
                group(
                    "pong",
                    "Pong (USA)",
                    &["Pong (Europe)"],
                    &["Pong (USA) (Demo)"],
                ),
            ]),
            &games,
            &BTreeSet::from(["Pong (Europe)".to_string()]),
        )
    }

    #[test]
    fn availability_resting_on_claims_says_so() {
        let index = index();
        let at = |title: &str| index.rows.iter().position(|r| r.title == title).unwrap();

        assert!(index.claimed(at("Pong (Europe)")));
        assert!(!index.claimed(at("Pong (USA)")));
        assert_eq!(
            availability_label(Some(GameStatus::Complete), true),
            "Complete (claim evidence only)"
        );
        assert_eq!(
            availability_label(Some(GameStatus::Missing), false),
            "Missing"
        );
        assert_eq!(availability_label(None, false), "");
    }

    fn titles(index: &SelectionIndex, shown: &[u32]) -> Vec<String> {
        shown
            .iter()
            .map(|&i| index.rows[i as usize].title.clone())
            .collect()
    }

    #[test]
    fn releases_filter_by_text_role_availability_and_flag() {
        let index = index();
        let all = index.sorted(None);
        let only = |filter: ReleaseFilter| titles(&index, &index.filtered(&all, &filter));

        assert_eq!(
            only(ReleaseFilter {
                text: "PONG".into(),
                role: Some(Role::Alternative),
                ..Default::default()
            }),
            ["Pong (Europe)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                availability: Some(AvailabilityFilter::Is(GameStatus::Missing)),
                ..Default::default()
            }),
            ["Pong (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                flag: Some(FlagFilter::NeedsReview),
                ..Default::default()
            }),
            ["Combat (USA)", "Maze (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                flag: Some(FlagFilter::Drift),
                ..Default::default()
            }),
            ["Maze (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                availability: Some(AvailabilityFilter::Unknown),
                text: "maze".into(),
                ..Default::default()
            }),
            ["Maze (USA)"]
        );
    }

    #[test]
    fn releases_sort_by_availability_then_their_place() {
        let index = index();
        let order = index.sorted(Some(Sort {
            column: ReleaseColumn::Availability,
            descending: false,
        }));
        assert_eq!(titles(&index, &order[..2]), ["Pong (Europe)", "Pong (USA)"]);
    }

    #[test]
    fn checking_stages_a_choice_and_checking_the_picks_again_drops_it() {
        let index = index();
        let pong = index.group("pong").unwrap();
        let saved = SelectionProfile::default();
        let mut staged = saved.clone();

        toggle(&mut staged, "Atari", pong, "Pong (Europe)", true);
        toggle(&mut staged, "Atari", pong, "Pong (USA)", false);
        assert_eq!(
            checked(&staged, "Atari", pong),
            BTreeSet::from(["Pong (Europe)".to_string()])
        );
        assert_eq!(staged_changes(&saved, &staged, "Atari"), 1);

        toggle(&mut staged, "Atari", pong, "Pong (USA)", true);
        toggle(&mut staged, "Atari", pong, "Pong (Europe)", false);
        assert_eq!(staged, saved, "back to the engine's picks");
    }

    #[test]
    fn confirming_an_unresolved_group_stores_its_picks() {
        let index = index();
        let maze = index.group("maze").unwrap();
        let mut staged = SelectionProfile::default();
        confirm(&mut staged, "Atari", maze);
        assert_eq!(
            staged.choices["Atari"]["maze"],
            BTreeSet::from(["Maze (USA)".to_string()])
        );
    }

    #[test]
    fn including_an_excluded_release_adds_an_override_and_checks_it() {
        let index = index();
        let pong = index.group("pong").unwrap();
        let mut staged = SelectionProfile::default();
        include_and_check(&mut staged, "Atari", pong, "Pong (USA) (Demo)");
        include_and_check(&mut staged, "Atari", pong, "Pong (USA) (Demo)");
        assert_eq!(
            staged.systems["Atari"].overrides.include,
            ["|Pong (USA) (Demo)"]
        );
        assert!(staged.choices["Atari"]["pong"].contains("Pong (USA) (Demo)"));
        assert!(staged.choices["Atari"]["pong"].contains("Pong (USA)"));
    }

    #[test]
    fn a_system_setting_comes_from_the_global_settings_until_set_for_the_system() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into()],
            ..Default::default()
        };
        let source = |p: &SelectionProfile| {
            Setting::RegionOrder
                .source(p.systems.get("Atari").unwrap_or(&SystemSettings::default()))
        };
        assert_eq!(source(&profile), Source::Global);

        Setting::RegionOrder.set_for_system(&mut profile, "Atari");
        assert_eq!(source(&profile), Source::System);
        assert_eq!(
            profile.systems["Atari"].region_order.as_deref(),
            Some(&["USA".to_string()][..])
        );
        Setting::RegionOrder.use_global(&mut profile, "Atari");
        assert!(profile.systems.is_empty(), "nothing of its own is left");

        Setting::Exclude.set_for_system(&mut profile, "Atari");
        use_global_settings(&mut profile, "Atari");
        assert!(profile.systems.is_empty());
    }

    #[test]
    fn resetting_the_global_settings_keeps_system_settings_and_choices() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into()],
            ..Default::default()
        };
        Setting::Exclude.set_for_system(&mut profile, "Atari");
        profile.set_choice("Atari", "pong", BTreeSet::new(), &BTreeSet::new(), true);
        reset_global(&mut profile);
        assert!(profile.region_order.is_empty());
        assert!(profile.systems.contains_key("Atari"));
        assert!(profile.choices.contains_key("Atari"));
    }

    #[test]
    fn resetting_a_system_to_the_defaults_ignores_the_global_settings_and_keeps_choices() {
        let mut profile = SelectionProfile {
            region_order: vec!["Japan".into()],
            exclude: [Category::Demos].into(),
            post_filters: vec!["/USA/".into()],
            ..Default::default()
        };
        Setting::VideoOrder.set_for_system(&mut profile, "Atari");
        profile.systems.get_mut("Atari").unwrap().video_order = Some(vec!["PAL".into()]);
        profile.set_choice("Atari", "pong", BTreeSet::new(), &BTreeSet::new(), true);
        reset_system(&mut profile, "Atari");
        let own = &profile.systems["Atari"];
        assert_eq!(own.region_order, Some(vec![]), "the default order");
        assert_eq!(own.video_order, Some(vec![]), "the default order");
        assert_eq!(own.exclude, Some(BTreeSet::new()));
        assert_eq!(own.options, Some(SelectionOptions::default()));
        assert!(own.replace_post_filters && own.post_filters.is_empty());
        assert!(profile.choices.contains_key("Atari"));
        assert_eq!(profile.region_order, ["Japan"], "the global settings stay");
    }

    #[test]
    fn ordered_lists_move_items_and_line_editors_drop_blank_lines() {
        let mut list = vec!["USA".to_string(), "Europe".into(), "Japan".into()];
        move_item(&mut list, 2, 0);
        assert_eq!(list, ["Japan", "USA", "Europe"]);
        move_item(&mut list, 0, 5);
        assert_eq!(list, ["Japan", "USA", "Europe"]);
        assert_eq!(
            lines(" |Pong (USA) \n\n/Demo/\n"),
            ["|Pong (USA)", "/Demo/"]
        );
    }
}
