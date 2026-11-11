//! Selection Profiles and the explained Selection the engine derives from one DAT Revision (ADR 0001).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The Operator's rules for choosing releases: global settings, with per-system settings that replace or add to
/// them. It holds no DAT content. Every Retool setting that affects Selection has a field here; settings that only
/// shape Retool's output DAT have none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionProfile {
    /// Regions in order of preference. A region left out is filtered out. Empty means Retool's default order.
    pub region_order: Vec<String>,
    /// Languages in order of preference. Empty means the languages implied by the region order.
    pub language_order: Vec<String>,
    /// Languages for local names, in order. Retool uses it only to rename releases, so it has no effect on which
    /// releases are chosen; it is kept for parity.
    pub localization_order: Vec<String>,
    /// Video standards in order of preference. Empty means Retool's default order.
    pub video_order: Vec<String>,
    /// Kinds of release to leave out.
    pub exclude: BTreeSet<Category>,
    pub options: SelectionOptions,
    pub overrides: Overrides,
    /// Patterns a chosen release must match to stay chosen, in Retool's override syntax.
    pub post_filters: Vec<String>,
    /// Per-system settings, by the system name Retool derives from the DAT header, such as
    /// `Atari - Atari 2600 (No-Intro)`.
    pub systems: BTreeMap<String, SystemSettings>,
    /// The Operator's choices, by system name and then Game group: the releases checked in that group, applied
    /// after the engine runs. A choice is stored only where it differs from the engine's picks, or where it confirms
    /// an unresolved group. An empty set leaves the group out.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

impl SelectionProfile {
    /// Records the releases checked in one group. The choice is stored only if it differs from the engine's
    /// `picks` for the group or confirms an `unresolved` group; otherwise any stored choice is removed, so the
    /// engine's picks apply.
    pub fn set_choice(
        &mut self,
        system: &str,
        group: &str,
        checked: BTreeSet<String>,
        picks: &BTreeSet<String>,
        unresolved: bool,
    ) {
        if &checked == picks && !unresolved {
            self.reset_choices(system, Some(group));
            return;
        }
        self.choices
            .entry(system.to_owned())
            .or_default()
            .insert(group.to_owned(), checked);
    }

    /// Removes the Operator's choices for one group of a system, or, with no group, for the whole system.
    pub fn reset_choices(&mut self, system: &str, group: Option<&str>) {
        let Some(groups) = self.choices.get_mut(system) else {
            return;
        };
        match group {
            Some(group) => {
                groups.remove(group);
            }
            None => groups.clear(),
        }
        if groups.is_empty() {
            self.choices.remove(system);
        }
    }

    /// Removes every choice in the Profile.
    pub fn reset_all_choices(&mut self) {
        self.choices.clear();
    }

    /// The Profile as its canonical JSON: compact, with every map sorted by key.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("a Selection Profile always serializes")
    }
}

/// Settings for one system. An order, `exclude` or `options` that is present replaces the global one; overrides are
/// added to the global ones; post filters are added unless `replace_post_filters` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SystemSettings {
    /// An empty list means Retool's default order, as it does in the global settings; so does an empty
    /// `video_order`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_order: Option<Vec<String>>,
    /// An empty list means the languages implied by the effective region order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub localization_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<BTreeSet<Category>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<SelectionOptions>,
    pub overrides: Overrides,
    pub post_filters: Vec<String>,
    pub replace_post_filters: bool,
    /// Whether builds of this system also read its Additions Set. Off unless you turn it on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub additions: bool,
}

impl SystemSettings {
    /// Whether the system has no settings of its own, so the global settings apply unchanged.
    pub fn is_empty(&self) -> bool {
        self == &SystemSettings::default()
    }
}

/// A kind of release that can be left out, as Retool categorizes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    AddOns,
    Aftermarket,
    Applications,
    Audio,
    BadDumps,
    Bios,
    BonusDiscs,
    Coverdiscs,
    Demos,
    Educational,
    Games,
    Manuals,
    /// Releases with ROMs that are missing in action.
    Mia,
    Multimedia,
    Pirate,
    Preproduction,
    Promotional,
    Unlicensed,
    Video,
}

impl Category {
    pub const ALL: [Category; 19] = [
        Category::AddOns,
        Category::Aftermarket,
        Category::Applications,
        Category::Audio,
        Category::BadDumps,
        Category::Bios,
        Category::BonusDiscs,
        Category::Coverdiscs,
        Category::Demos,
        Category::Educational,
        Category::Games,
        Category::Manuals,
        Category::Mia,
        Category::Multimedia,
        Category::Pirate,
        Category::Preproduction,
        Category::Promotional,
        Category::Unlicensed,
        Category::Video,
    ];
}

/// Preferences and switches that change how releases are chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionOptions {
    /// Prefer releases that support RetroAchievements.
    pub prefer_retroachievements: bool,
    /// Prefer the oldest production versions over the newest.
    pub prefer_oldest: bool,
    /// Follow the region order strictly, even when a lower region supports a preferred language.
    pub prefer_regions: bool,
    /// Prefer licensed releases over unlicensed, aftermarket and pirate ones.
    pub prefer_licensed: bool,
    /// Prefer releases ripped from modern rereleases.
    pub prefer_modern_rips: bool,
    /// Leave out releases that support none of the languages in the language order.
    pub filter_languages: bool,
    /// Keep every release instead of one per Game; clone lists are ignored.
    pub disable_1g1r: bool,
    pub compilations: Compilations,
}

/// How compilations compete with individual releases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compilations {
    /// Retool's default: individual releases, unless a compilation ranks higher or holds unique Games.
    #[default]
    Default,
    /// Always prefer individual releases.
    Individual,
    /// Keep individual releases and compilations, comparing each only with its own kind.
    Keep,
    /// Least duplication (a beta mode in Retool).
    Optimize,
}

/// Releases forced out or in, in Retool's override syntax: plain text matches part of a name, `|` a full name and
/// `/` a regular expression; wrapped in `<…>` the match extends to related releases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overrides {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

/// The version of the trace records a Selection is derived from.
pub const SELECTION_TRACE_SCHEMA: u32 = 1;

/// An explained Selection: per Game group, the chosen releases, the alternatives and the exclusions, each with the
/// rule responsible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub pins: SelectionPins,
    /// SHA-256 of the DAT file: the DAT Revision the Selection was made from.
    pub dat_sha256: String,
    /// The system name Retool derived from the DAT header; it keys the per-system settings.
    pub system: String,
    /// Whether the Profile has settings for this system.
    pub system_settings: bool,
    pub summary: SelectionSummary,
    pub coverage_drift: CoverageDrift,
    /// Sorted by group name.
    pub groups: Vec<SelectionGroup>,
    /// The Operator's choices for groups that are gone from the DAT Revision, by group: stale, never dropped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stale_choices: BTreeMap<String, BTreeSet<String>>,
}

/// What a Selection was made with, so that it can be reproduced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionPins {
    /// The Retool commit the bundled engine ran: only on a Selection made before the cutover to the built-in engine
    /// (ADR 0015).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retool_commit: Option<String>,
    /// SHA-256 over the ordered hashes of the patches the bundled engine ran with: only on a Selection made before
    /// the cutover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_set_sha256: Option<String>,
    /// The Title Select version whose engine made the Selection: absent on a Selection made before the cutover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_select_version: Option<String>,
    /// The clone list and metadata commit.
    pub clone_list_commit: String,
    pub trace_schema: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSummary {
    pub groups: usize,
    pub winners: usize,
    pub alternatives: usize,
    pub exclusions: usize,
    pub unresolved: usize,
    /// Groups whose winners the Operator's choice decided.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub choices: usize,
    /// Stale choices, including those for groups that are gone.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stale: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub conflicting: usize,
}

impl SelectionSummary {
    /// Groups that need a decision: unresolved, or with a stale or conflicting choice.
    pub fn needs_review(&self) -> usize {
        self.unresolved + self.stale + self.conflicting
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Where the clone list and metadata no longer match the DAT.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageDrift {
    /// Whether there is a clone list for this system at all.
    pub clone_list: bool,
    /// Whether there is metadata for this system at all.
    pub metadata: bool,
    /// Clone-list search terms that match no release in the DAT.
    pub unmatched_clone_list_terms: Vec<String>,
    /// Releases in the DAT with no metadata entry.
    pub titles_without_metadata: Vec<String>,
}

/// One Game group: what was chosen, what was not, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionGroup {
    pub group: String,
    /// Retool picked at least one winner by a tie-breaker; the pick is provisional and needs a decision.
    pub unresolved: bool,
    /// Usually one. None when every release in the group is excluded.
    pub winners: Vec<Winner>,
    pub alternatives: Vec<Alternative>,
    pub exclusions: Vec<Exclusion>,
    /// The Operator's choice for the group, if the Profile has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<GroupChoice>,
    /// Coverage drift: a release in the group has no metadata entry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drift: bool,
}

impl SelectionGroup {
    /// The engine's picks for the group, before any choice was applied.
    pub fn picks(&self) -> BTreeSet<String> {
        match &self.choice {
            Some(choice) => choice.picks.iter().cloned().collect(),
            None => self.winners.iter().map(|w| w.title.clone()).collect(),
        }
    }

    /// Whether the engine left the group unresolved, before any choice confirmed it.
    pub fn engine_unresolved(&self) -> bool {
        match &self.choice {
            Some(choice) => choice.engine_unresolved,
            None => self.unresolved,
        }
    }

    /// Unresolved, a stale choice or a conflicting choice: the group needs a decision.
    pub fn needs_review(&self) -> bool {
        self.unresolved
            || self.choice.as_ref().is_some_and(|c| {
                matches!(c.status, ChoiceStatus::Stale | ChoiceStatus::Conflicting)
            })
    }
}

/// The Operator's choice for one group and how it relates to the engine's picks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupChoice {
    /// The releases checked.
    pub checked: BTreeSet<String>,
    pub status: ChoiceStatus,
    /// The engine's winners, before the choice was applied.
    pub picks: Vec<String>,
    /// Whether the engine left the group unresolved.
    pub engine_unresolved: bool,
    /// Checked releases that are now excluded (conflicting) or gone from the group (stale).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceStatus {
    /// The choice decided the group's winners.
    Applied,
    /// The engine now picks exactly the checked releases, so the choice can be dropped.
    MatchesDefault,
    /// A checked release is now excluded by the settings; the choice is not applied.
    Conflicting,
    /// A checked release is gone from the group; the choice is not applied.
    Stale,
}

impl ChoiceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChoiceStatus::Applied => "applied",
            ChoiceStatus::MatchesDefault => "matches_default",
            ChoiceStatus::Conflicting => "conflicting",
            ChoiceStatus::Stale => "stale",
        }
    }

    pub fn parse(s: &str) -> Option<ChoiceStatus> {
        [
            ChoiceStatus::Applied,
            ChoiceStatus::MatchesDefault,
            ChoiceStatus::Conflicting,
            ChoiceStatus::Stale,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
    }
}

/// A release's role in its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Winner,
    Alternative,
    Exclusion,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Winner, Role::Alternative, Role::Exclusion];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Winner => "winner",
            Role::Alternative => "alternative",
            Role::Exclusion => "exclusion",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// One release of a Selection, as a row: what `selection_releases` stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRow {
    pub group: String,
    pub role: Role,
    pub title: String,
    /// For an alternative, the winner it is an alternative to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_of: Option<String>,
    /// For an exclusion, Retool's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub rule: Rule,
    pub unresolved: bool,
    pub from_choice: bool,
}

impl Selection {
    /// Every release, group by group, winners then alternatives then exclusions.
    pub fn rows(&self) -> Vec<ReleaseRow> {
        let mut rows = Vec::new();
        for group in &self.groups {
            for w in &group.winners {
                rows.push(ReleaseRow {
                    group: group.group.clone(),
                    role: Role::Winner,
                    title: w.title.clone(),
                    clone_of: None,
                    reason: None,
                    rule: w.rule.clone(),
                    unresolved: w.unresolved,
                    from_choice: w.rule.is_choice(),
                });
            }
            for a in &group.alternatives {
                rows.push(ReleaseRow {
                    group: group.group.clone(),
                    role: Role::Alternative,
                    title: a.title.clone(),
                    clone_of: Some(a.clone_of.clone()),
                    reason: None,
                    rule: a.rule.clone(),
                    unresolved: false,
                    from_choice: a.rule.is_choice(),
                });
            }
            for e in &group.exclusions {
                rows.push(ReleaseRow {
                    group: group.group.clone(),
                    role: Role::Exclusion,
                    title: e.title.clone(),
                    clone_of: None,
                    reason: Some(e.reason.clone()),
                    rule: e.rule.clone(),
                    unresolved: false,
                    from_choice: false,
                });
            }
        }
        rows
    }
}

/// How one release's role changed between two Selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseChange {
    pub group: String,
    pub title: String,
    /// Absent when the release is new.
    pub before: Option<Role>,
    /// Absent when the release is gone.
    pub after: Option<Role>,
    /// The rule responsible now, or before when the release is gone.
    pub rule: Rule,
}

/// Release by release, what changed from one Selection to another. Releases whose role is unchanged are left out.
pub fn diff_rows(before: &[ReleaseRow], after: &[ReleaseRow]) -> Vec<ReleaseChange> {
    let key = |r: &ReleaseRow| (r.group.clone(), r.title.clone());
    let old: BTreeMap<_, _> = before.iter().map(|r| (key(r), r)).collect();
    let new: BTreeMap<_, _> = after.iter().map(|r| (key(r), r)).collect();
    let mut changes = Vec::new();
    for (k, a) in &new {
        let previous = old.get(k).map(|b| b.role);
        if previous != Some(a.role) {
            changes.push(ReleaseChange {
                group: k.0.clone(),
                title: k.1.clone(),
                before: previous,
                after: Some(a.role),
                rule: a.rule.clone(),
            });
        }
    }
    for (k, b) in &old {
        if !new.contains_key(k) {
            changes.push(ReleaseChange {
                group: k.0.clone(),
                title: k.1.clone(),
                before: Some(b.role),
                after: None,
                rule: b.rule.clone(),
            });
        }
    }
    changes.sort_by(|a, b| (&a.group, &a.title).cmp(&(&b.group, &b.title)));
    changes
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Winner {
    pub title: String,
    /// The rule that decided it.
    pub rule: Rule,
    pub unresolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alternative {
    pub title: String,
    /// The winner it is an alternative to.
    pub clone_of: String,
    /// The rule that removed it.
    pub rule: Rule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    pub title: String,
    pub rule: Rule,
    /// Retool's reason, such as `Demos, kiosks, and samples`.
    pub reason: String,
}

/// The stage of the rule recorded for a release the Operator's choice decided.
pub const CHOICE_STAGE: &str = "choice";

impl Rule {
    /// The rule recorded for a release decided by the Operator's choice.
    pub fn choice() -> Rule {
        Rule {
            stage: CHOICE_STAGE.to_owned(),
            reference: None,
            message: "your choice".to_owned(),
        }
    }

    /// Whether the Operator's choice decided the release.
    pub fn is_choice(&self) -> bool {
        self.stage == CHOICE_STAGE
    }
}

/// A rule in Retool's selection, as its trace reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub stage: String,
    /// Retool's trace reference, such as `REF0014`; absent when no trace record names the release.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(titles: &[&str]) -> BTreeSet<String> {
        titles.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn pins_from_before_the_cutover_still_read_and_new_pins_name_the_version() {
        let old: SelectionPins = serde_json::from_str(
            r#"{"retool_commit":"59f169f","patch_set_sha256":"ab","clone_list_commit":"cdeb391","trace_schema":1}"#,
        )
        .unwrap();
        assert_eq!(old.retool_commit.as_deref(), Some("59f169f"));
        assert_eq!(old.title_select_version, None);

        let new = SelectionPins {
            title_select_version: Some("0.7.0".into()),
            clone_list_commit: "cdeb391".into(),
            trace_schema: 1,
            ..SelectionPins::default()
        };
        assert_eq!(
            serde_json::to_string(&new).unwrap(),
            r#"{"title_select_version":"0.7.0","clone_list_commit":"cdeb391","trace_schema":1}"#
        );
    }

    #[test]
    fn a_choice_is_stored_only_where_it_differs_from_the_picks_or_confirms_an_unresolved_group() {
        let mut profile = SelectionProfile::default();
        let picks = set(&["Pong (USA)"]);

        profile.set_choice("Atari", "pong", picks.clone(), &picks, false);
        assert!(profile.choices.is_empty());
        profile.set_choice("Atari", "pong", set(&["Pong (Europe)"]), &picks, false);
        profile.set_choice("Atari", "combat", set(&[]), &set(&["Combat (USA)"]), false);
        profile.set_choice(
            "Atari",
            "maze",
            set(&["Maze (USA)"]),
            &set(&["Maze (USA)"]),
            true,
        );
        assert_eq!(profile.choices["Atari"].len(), 3);

        profile.set_choice("Atari", "pong", picks.clone(), &picks, false);
        assert!(
            !profile.choices["Atari"].contains_key("pong"),
            "back to the picks"
        );
    }

    #[test]
    fn choices_reset_per_group_per_system_and_for_the_whole_profile() {
        let mut profile = SelectionProfile::default();
        let none = BTreeSet::new();
        for (system, group) in [("Atari", "pong"), ("Atari", "combat"), ("Coleco", "zaxxon")] {
            profile.set_choice(system, group, set(&["x"]), &none, false);
        }

        profile.reset_choices("Atari", Some("pong"));
        assert_eq!(
            profile.choices["Atari"].keys().collect::<Vec<_>>(),
            ["combat"]
        );
        profile.reset_choices("Atari", None);
        assert!(!profile.choices.contains_key("Atari"));
        profile.reset_all_choices();
        assert!(profile.choices.is_empty());
        assert_eq!(
            profile.canonical_json(),
            SelectionProfile::default().canonical_json()
        );
    }
}
