//! Multi-disc Games (ADR 0014): Retool's disc-name normalization, and the check after Selection that a group's
//! selected discs come whole from one release. The check reads a Selection's result only, so it holds for any
//! engine that produces one; the selection algorithm itself is unchanged (ADR 0001).

use std::collections::{BTreeMap, BTreeSet};

use crate::selection::{ChoiceStatus, GroupChoice, Selection, SelectionGroup};

/// Retool's `discRename` table from its `internal-config.json`, in order: each key found in a name is replaced by
/// its value, one after another, so a later key sees what an earlier one wrote. A test checks it against the
/// pinned config.
pub const DISC_RENAME: [(&str, &str); 44] = [
    ("(Disc 0", "(Disc "),
    ("(Disco ", "(Disc "),
    ("(Disk ", "(Disc "),
    ("(Dysk ", "(Disc "),
    ("(CD ", "(Disc "),
    ("(Disque ", "(Disc "),
    ("(Lato ", "(Disc "),
    ("(Seite ", "(Disc "),
    ("(Teil ", "(Disc "),
    ("(Disc )", "(Disc 0)"),
    ("(Disc A)", "(Disc 1)"),
    ("(Disc B)", "(Disc 2)"),
    ("(Disc C)", "(Disc 3)"),
    ("(Disc D)", "(Disc 4)"),
    ("(Disc E)", "(Disc 5)"),
    ("(Disc F)", "(Disc 6)"),
    ("(Disc G)", "(Disc 7)"),
    ("(Disc H)", "(Disc 8)"),
    ("(Disc I)", "(Disc 1)"),
    ("(Disc II)", "(Disc 2)"),
    ("(Disc III)", "(Disc 3)"),
    ("(Disc IV)", "(Disc 4)"),
    ("(Disc V)", "(Disc 5)"),
    ("(Disc VI)", "(Disc 6)"),
    ("(Disc VII)", "(Disc 7)"),
    ("(Disc VIII)", "(Disc 8)"),
    ("(Disc One)", "(Disc 1)"),
    ("(Disc Two)", "(Disc 2)"),
    ("(Disc Three)", "(Disc 3)"),
    ("(Disc Four)", "(Disc 4)"),
    ("(Disc Five)", "(Disc 5)"),
    ("(Disc Six)", "(Disc 6)"),
    ("(Disc Seven)", "(Disc 7)"),
    ("(Disc Eight)", "(Disc 8)"),
    ("(Disc Uno)", "(Disc 1)"),
    ("(Disc Due)", "(Disc 2)"),
    ("(Disc Tre)", "(Disc 3)"),
    ("(Disc Quattro)", "(Disc 4)"),
    ("(Disc Cinque)", "(Disc 5)"),
    ("(Disc Sei)", "(Disc 6)"),
    ("(Disc Sette)", "(Disc 7)"),
    ("(Disc Otto)", "(Disc 8)"),
    ("(Disc-", "(Disc "),
    ("(Disc Writer)", "(Disk Writer)"),
];

/// Retool's `TitleTools.normalize_discs`: the many ways a name tags a disc, rewritten as `(Disc N)`.
pub fn normalize(name: &str) -> String {
    let mut name = name.to_owned();
    for (key, value) in DISC_RENAME {
        if name.contains(key) {
            name = name.replace(key, value);
        }
    }
    name
}

/// One disc of a Multi-disc Game: the release it belongs to and its number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Disc {
    /// The Game's name, normalized, without its disc tag: `Title (USA)` for `Title (USA) (Disk 2)`.
    pub release: String,
    pub number: u32,
}

/// The disc a Game is, when its normalized name has a `(Disc N)` tag.
pub fn disc(title: &str) -> Option<Disc> {
    let name = normalize(title);
    let mut from = 0;
    while let Some(at) = name[from..].find("(Disc ") {
        let start = from + at;
        let digits_at = start + "(Disc ".len();
        let digits: &str = &name[digits_at..][..name[digits_at..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(0)];
        if !digits.is_empty() && name[digits_at + digits.len()..].starts_with(')') {
            let end = digits_at + digits.len() + 1;
            let release = format!("{} {}", name[..start].trim_end(), name[end..].trim_start());
            return Some(Disc {
                release: release.trim().to_owned(),
                number: digits.parse().ok()?,
            });
        }
        from = digits_at;
    }
    None
}

/// Why a group's selected discs are not one whole release: the releases they come from and the disc numbers the
/// group has between all its releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscCheck {
    /// The disc numbers each selected release must have: those of the group's titles your settings don't exclude,
    /// and every disc of a selected release, excluded or not.
    pub discs: BTreeSet<u32>,
    /// The disc numbers of the selected releases that your settings exclude.
    pub excluded: BTreeSet<u32>,
    /// Each release a selected disc comes from, with its selected disc numbers.
    pub chosen: BTreeMap<String, BTreeSet<u32>>,
}

impl DiscCheck {
    /// The disc numbers of the group that no selected release has.
    pub fn missing(&self) -> BTreeSet<u32> {
        let selected: BTreeSet<u32> = self.chosen.values().flatten().copied().collect();
        self.discs.difference(&selected).copied().collect()
    }

    /// Why the group needs a decision, as a phrase that completes "the group … is unresolved: ".
    pub fn reason(&self) -> String {
        if self.chosen.len() > 1 {
            let mut chosen: Vec<_> = self.chosen.iter().collect();
            chosen.sort_by_key(|(release, discs)| (discs.first().copied(), *release));
            let from: Vec<String> = chosen
                .into_iter()
                .map(|(release, discs)| format!("{} from {release}", numbers("disc", discs)))
                .collect();
            let mut reason = format!(
                "its selected discs come from different releases: {}",
                from.join(", ")
            );
            let missing = self.missing();
            if !missing.is_empty() {
                reason.push_str(&format!(", and none has {}", numbers("disc", &missing)));
            }
            reason
        } else {
            let (release, discs) = self.chosen.iter().next().expect("a check has a release");
            let lacking: BTreeSet<u32> = self.discs.difference(discs).copied().collect();
            let why = if lacking.is_subset(&self.excluded) {
                "which your settings exclude"
            } else {
                "which another release in the group has"
            };
            format!(
                "{release} is selected without {}, {why}",
                numbers("disc", &lacking)
            )
        }
    }
}

/// `disc 2` or `discs 1, 2 and 3`.
fn numbers(word: &str, numbers: &BTreeSet<u32>) -> String {
    let n: Vec<String> = numbers.iter().map(u32::to_string).collect();
    match n.as_slice() {
        [one] => format!("{word} {one}"),
        [rest @ .., last] => format!("{word}s {} and {last}", rest.join(", ")),
        [] => format!("no {word}s"),
    }
}

/// Every title of the group, whatever its role.
fn titles(group: &SelectionGroup) -> impl Iterator<Item = &str> {
    group
        .winners
        .iter()
        .map(|w| w.title.as_str())
        .chain(group.alternatives.iter().map(|a| a.title.as_str()))
        .chain(group.exclusions.iter().map(|e| e.title.as_str()))
}

/// Each release the disc titles among `selected` come from, with their disc numbers.
fn chosen<'a>(selected: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, BTreeSet<u32>> {
    let mut chosen: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for d in selected.into_iter().filter_map(disc) {
        chosen.entry(d.release).or_default().insert(d.number);
    }
    chosen
}

/// The disc numbers the `chosen` releases must cover between them: those of the group's titles your settings don't
/// exclude, and every disc of a chosen release, even an excluded one. So an excluded demo disc never counts, but an
/// excluded disc of the chosen release does.
fn expected(group: &SelectionGroup, chosen: &BTreeMap<String, BTreeSet<u32>>) -> BTreeSet<u32> {
    let excluded: BTreeSet<&str> = group.exclusions.iter().map(|e| e.title.as_str()).collect();
    titles(group)
        .filter_map(|title| Some((excluded.contains(title), disc(title)?)))
        .filter(|(excluded, d)| !excluded || chosen.contains_key(&d.release))
        .map(|(_, d)| d.number)
        .collect()
}

/// Checks `selected`, titles of `group`: when some are discs, each release they come from must have every
/// [`expected`] disc number. `None` when they do, or when none is a disc.
pub fn check<'a>(
    group: &SelectionGroup,
    selected: impl IntoIterator<Item = &'a str>,
) -> Option<DiscCheck> {
    let chosen = chosen(selected);
    if chosen.is_empty() {
        return None;
    }
    let discs = expected(group, &chosen);
    // "Different releases" means a release that lacks a disc, not more than one release: several whole releases,
    // such as USA 1-2 and Europe 1-2 when the profile keeps more than one, are each a whole game.
    chosen
        .values()
        .any(|have| !discs.is_subset(have))
        .then(|| DiscCheck {
            excluded: group
                .exclusions
                .iter()
                .filter_map(|e| disc(&e.title))
                .filter(|d| chosen.contains_key(&d.release))
                .map(|d| d.number)
                .collect(),
            discs,
            chosen,
        })
}

/// The check of the group's picks: the engine's winners, before any choice of the Operator's replaced them.
pub fn check_picks(group: &SelectionGroup) -> Option<DiscCheck> {
    let picks = group.picks();
    check(group, picks.iter().map(String::as_str))
}

/// The [`expected`] disc numbers that none of `selected` has, between all their releases: a deliberate mix that
/// has every disc covers the game. Empty when none of `selected` is a disc.
pub fn uncovered<'a>(
    group: &SelectionGroup,
    selected: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<u32> {
    let chosen = chosen(selected);
    let have: BTreeSet<u32> = chosen.values().flatten().copied().collect();
    expected(group, &chosen)
        .difference(&have)
        .copied()
        .collect()
}

/// The Operator's choice for the group, when it is applied.
fn applied(group: &SelectionGroup) -> Option<&GroupChoice> {
    group
        .choice
        .as_ref()
        .filter(|c| c.status == ChoiceStatus::Applied)
}

/// The discs of the group that none of its winners has, each with the titles in the group that have it: what a
/// Library Folder built from the group lacks. Empty when no winner is a disc.
pub fn missing(group: &SelectionGroup) -> BTreeMap<u32, Vec<String>> {
    let missing = uncovered(group, group.winners.iter().map(|w| w.title.as_str()));
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for title in titles(group) {
        if let Some(d) = disc(title).filter(|d| missing.contains(&d.number)) {
            out.entry(d.number).or_default().push(title.to_owned());
        }
    }
    out
}

/// Why the group is unresolved for its discs, as a phrase that completes "the group … is unresolved: ": your
/// applied choice that leaves a disc uncovered, or else the engine's picks that don't come whole from one release.
/// `None` when its discs are not why.
pub fn unresolved_reason(group: &SelectionGroup) -> Option<String> {
    match applied(group) {
        Some(choice) => {
            let uncovered = uncovered(group, choice.checked.iter().map(String::as_str));
            (!uncovered.is_empty())
                .then(|| format!("your choice doesn't cover {}", numbers("disc", &uncovered)))
        }
        None => check_picks(group).map(|c| c.reason()),
    }
}

/// The pass after Selection, before choices are applied: every group whose winners are discs that do not come
/// whole from one release is unresolved, so that its Build is blocked until the Operator's choice settles it. A
/// choice for such a group is then recorded as settling an unresolved group. Winners are never changed.
pub fn mark_unresolved(selection: &mut Selection) {
    for group in &mut selection.groups {
        if !group.unresolved
            && check(group, group.winners.iter().map(|w| w.title.as_str())).is_some()
        {
            group.unresolved = true;
        }
    }
    selection.summary.unresolved = selection.groups.iter().filter(|g| g.unresolved).count();
}

/// The pass after choices are applied: a group whose applied choice leaves a disc of its Multi-disc Game uncovered
/// is unresolved again, such as when a newer DAT Revision adds a disc. A deliberate mix of releases that covers
/// every disc is the Operator's settled decision, and stays resolved. The choice is kept either way.
pub fn mark_uncovered_choices(selection: &mut Selection) {
    for group in &mut selection.groups {
        if applied(group).is_some_and(|choice| {
            !uncovered(group, choice.checked.iter().map(String::as_str)).is_empty()
        }) {
            group.unresolved = true;
        }
    }
    selection.summary.unresolved = selection.groups.iter().filter(|g| g.unresolved).count();
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::selection::{
        Alternative, CoverageDrift, Exclusion, Rule, SelectionPins, SelectionSummary, Winner,
    };

    #[test]
    fn disc_names_normalize_as_retool_does_including_its_order() {
        for (name, normalized) in [
            ("Title (USA) (Disc 01)", "Title (USA) (Disc 1)"),
            ("Title (Europe) (Disk 2)", "Title (Europe) (Disc 2)"),
            ("Title (France) (Disque 1)", "Title (France) (Disc 1)"),
            ("Title (Japan) (CD 3)", "Title (Japan) (Disc 3)"),
            ("Title (Poland) (Dysk 2)", "Title (Poland) (Disc 2)"),
            ("Title (Germany) (Seite A)", "Title (Germany) (Disc 1)"),
            ("Title (Germany) (Teil B)", "Title (Germany) (Disc 2)"),
            ("Title (USA) (Disc II)", "Title (USA) (Disc 2)"),
            ("Title (USA) (Disc I)", "Title (USA) (Disc 1)"),
            ("Title (USA) (Disc Two)", "Title (USA) (Disc 2)"),
            ("Title (Italy) (Disc Tre)", "Title (Italy) (Disc 3)"),
            ("Title (USA) (Disc-1)", "Title (USA) (Disc 1)"),
            // `(Disc-` is rewritten after the letters are, so a lettered disc keeps its letter.
            ("Title (USA) (Disc-B)", "Title (USA) (Disc B)"),
            ("Title (USA) (Disc )", "Title (USA) (Disc 0)"),
            ("Title (USA) (Disc 0)", "Title (USA) (Disc 0)"),
            ("Title (USA) (Disc Writer)", "Title (USA) (Disk Writer)"),
            ("Title (USA) (disc 1)", "Title (USA) (disc 1)"),
            ("Title (USA)", "Title (USA)"),
        ] {
            assert_eq!(normalize(name), normalized, "{name}");
        }
    }

    #[test]
    fn a_disc_is_a_normalized_disc_tag_and_its_release_is_the_name_without_it() {
        let d = |release: &str, number| {
            Some(Disc {
                release: release.into(),
                number,
            })
        };
        assert_eq!(disc("Title (USA) (Disc 2)"), d("Title (USA)", 2));
        assert_eq!(disc("Title (Europe) (Disk B)"), d("Title (Europe)", 2));
        assert_eq!(
            disc("Title (USA) (Disc 1) (Rev 1)"),
            d("Title (USA) (Rev 1)", 1)
        );
        assert_eq!(disc("Title (USA) (Disc 10)"), d("Title (USA)", 10));
        assert_eq!(disc("Title (USA)"), None);
        assert_eq!(disc("Title (USA) (Disc Writer)"), None);
        assert_eq!(disc("Title (USA) (Disc-B)"), None);
    }

    fn rule() -> Rule {
        Rule {
            stage: "Parent selection".into(),
            reference: Some("REF0001".into()),
            message: "m".into(),
        }
    }

    fn group(winners: &[&str], exclusions: &[&str]) -> SelectionGroup {
        SelectionGroup {
            group: "title".into(),
            unresolved: false,
            winners: winners
                .iter()
                .map(|t| Winner {
                    title: t.to_string(),
                    rule: rule(),
                    unresolved: false,
                })
                .collect(),
            alternatives: vec![],
            exclusions: exclusions
                .iter()
                .map(|t| Exclusion {
                    title: t.to_string(),
                    rule: rule(),
                    reason: "Regions".into(),
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
            system: "Test".into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups,
            stale_choices: BTreeMap::new(),
        }
    }

    #[test]
    fn discs_from_different_releases_leave_the_group_unresolved_and_the_winners_unchanged() {
        let mixed = group(&["Title (USA) (Disc 1)", "Title (Europe) (Disc 2)"], &[]);
        let single = group(&["Other (USA)"], &[]);
        let mut s = selection(vec![mixed.clone(), single]);

        mark_unresolved(&mut s);

        assert!(s.groups[0].unresolved);
        assert_eq!(s.groups[0].winners, mixed.winners);
        assert!(!s.groups[1].unresolved);
        assert_eq!(s.summary.unresolved, 1);
        assert_eq!(
            check_picks(&s.groups[0]).unwrap().reason(),
            "its selected discs come from different releases: disc 1 from Title (USA), disc 2 from Title (Europe)"
        );
    }

    #[test]
    fn a_selected_release_without_a_disc_another_release_has_is_unresolved() {
        let mut usa = group(&["Title (USA) (Disc 1)"], &[]);
        usa.alternatives = ["Title (Europe) (Disc 1)", "Title (Europe) (Disc 2)"]
            .into_iter()
            .map(|title| Alternative {
                title: title.into(),
                clone_of: "Title (USA) (Disc 1)".into(),
                rule: rule(),
            })
            .collect();
        let mut s = selection(vec![usa]);
        mark_unresolved(&mut s);
        assert!(s.groups[0].unresolved);
        assert_eq!(
            check_picks(&s.groups[0]).unwrap().reason(),
            "Title (USA) is selected without disc 2, which another release in the group has"
        );
        assert_eq!(
            missing(&s.groups[0]),
            BTreeMap::from([(2, vec!["Title (Europe) (Disc 2)".to_owned()])])
        );
    }

    #[test]
    fn an_excluded_disc_of_another_release_never_flags_the_group() {
        let mut s = selection(vec![group(
            &["Title (USA) (Disc 1)", "Title (USA) (Disc 2)"],
            &[
                "Title (USA) (Demo) (Disc 3)",
                "Title (Europe) (Disc 1)",
                "Title (Europe) (Disc 2)",
                "Title (Europe) (Disc 3)",
            ],
        )]);
        mark_unresolved(&mut s);
        assert!(!s.groups[0].unresolved);
        assert!(missing(&s.groups[0]).is_empty());
    }

    #[test]
    fn an_excluded_disc_of_the_selected_release_still_flags_the_group() {
        let mut s = selection(vec![group(
            &["Title (USA) (Disc 1)"],
            &["Title (USA) (Disc 2)"],
        )]);
        mark_unresolved(&mut s);
        assert!(s.groups[0].unresolved);
        assert_eq!(
            check_picks(&s.groups[0]).unwrap().reason(),
            "Title (USA) is selected without disc 2, which your settings exclude"
        );
        assert_eq!(
            missing(&s.groups[0]),
            BTreeMap::from([(2, vec!["Title (USA) (Disc 2)".to_owned()])])
        );
    }

    #[test]
    fn whole_releases_pass_even_several_of_them() {
        let mut s = selection(vec![
            group(
                &["Title (Europe) (Disk 1)", "Title (Europe) (Disc 2)"],
                &["Title (USA) (Disc 1)"],
            ),
            group(
                &[
                    "Title (USA) (Disc 1)",
                    "Title (USA) (Disc 2)",
                    "Title (Europe) (Disc 1)",
                    "Title (Europe) (Disc 2)",
                ],
                &[],
            ),
            group(
                &["Title (USA)"],
                &["Title (Japan) (Disc 1)", "Title (Japan) (Disc 2)"],
            ),
        ]);
        mark_unresolved(&mut s);
        assert!(s.groups.iter().all(|g| !g.unresolved), "{s:#?}");
        assert!(s.groups.iter().all(|g| missing(g).is_empty()));
    }
}
