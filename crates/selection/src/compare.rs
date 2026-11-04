//! Compares two engines' Selections from the same DAT Revision and Profile, group by group: for conformance and
//! shadow mode.

use std::collections::{BTreeMap, BTreeSet};

use title_select_api::selection::{
    EngineComparison, GroupDifference, Placement, PlacementRole, ReleaseDifference, Rule,
    Selection, SelectionGroup,
};

/// How `bundled` and `builtin` differ, with the warnings each engine gave.
pub fn compare(
    bundled: &Selection,
    builtin: &Selection,
    bundled_warnings: &[String],
    builtin_warnings: &[String],
) -> EngineComparison {
    let index = |s: &Selection| -> BTreeMap<String, SelectionGroup> {
        s.groups
            .iter()
            .map(|g| (g.group.clone(), g.clone()))
            .collect()
    };
    let (a, b) = (index(bundled), index(builtin));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut comparison = EngineComparison {
        groups: names.len(),
        ..Default::default()
    };
    for name in names {
        if let Some(difference) = compare_group(name, a.get(name), b.get(name)) {
            if difference.explanation_only {
                comparison.explained_differently += 1;
            } else {
                comparison.differing += 1;
            }
            comparison.differences.push(difference);
        }
    }
    if bundled_warnings != builtin_warnings {
        comparison.warnings = Some([bundled_warnings.to_vec(), builtin_warnings.to_vec()]);
    }
    comparison
}

/// Where each engine put each release of a group.
fn placements(group: &SelectionGroup) -> BTreeMap<String, Placement> {
    let placement = |role, clone_of: Option<&String>, rule: &Rule| Placement {
        role,
        clone_of: clone_of.cloned(),
        stage: rule.stage.clone(),
        message: rule.message.clone(),
    };
    let mut out = BTreeMap::new();
    for w in &group.winners {
        out.insert(
            w.title.clone(),
            placement(PlacementRole::Winner, None, &w.rule),
        );
    }
    for a in &group.alternatives {
        out.insert(
            a.title.clone(),
            placement(PlacementRole::Alternative, Some(&a.clone_of), &a.rule),
        );
    }
    for e in &group.exclusions {
        out.insert(
            e.title.clone(),
            placement(PlacementRole::Exclusion, None, &e.rule),
        );
    }
    out
}

fn compare_group(
    name: &str,
    a: Option<&SelectionGroup>,
    b: Option<&SelectionGroup>,
) -> Option<GroupDifference> {
    let empty = BTreeMap::new();
    let (pa, pb) = (
        a.map(placements).unwrap_or_default(),
        b.map(placements).unwrap_or(empty),
    );
    let titles: BTreeSet<&String> = pa.keys().chain(pb.keys()).collect();
    let mut releases = Vec::new();
    let mut placed_alike = a.is_some() == b.is_some();
    for title in titles {
        let (x, y) = (pa.get(title), pb.get(title));
        if x == y {
            continue;
        }
        let same_place = match (x, y) {
            (Some(x), Some(y)) => x.role == y.role && x.clone_of == y.clone_of,
            _ => false,
        };
        placed_alike &= same_place;
        releases.push(ReleaseDifference {
            title: title.clone(),
            placements: [x.cloned(), y.cloned()],
        });
    }
    let unresolved = match (a, b) {
        (Some(a), Some(b)) if a.unresolved != b.unresolved => Some([a.unresolved, b.unresolved]),
        _ => None,
    };
    placed_alike &= unresolved.is_none();
    if releases.is_empty() && unresolved.is_none() && a.is_some() == b.is_some() {
        return None;
    }
    Some(GroupDifference {
        group: name.to_owned(),
        explanation_only: placed_alike,
        unresolved,
        releases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::{Alternative, Winner};

    fn rule(message: &str) -> Rule {
        Rule {
            stage: "Parent selection".into(),
            reference: Some("REF0001".into()),
            message: message.into(),
        }
    }

    fn selection(groups: Vec<SelectionGroup>) -> Selection {
        Selection {
            pins: Default::default(),
            dat_sha256: String::new(),
            system: String::new(),
            system_settings: false,
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups,
            stale_choices: Default::default(),
        }
    }

    fn group(name: &str, winner: &str, clone: &str, message: &str) -> SelectionGroup {
        SelectionGroup {
            group: name.into(),
            unresolved: false,
            winners: vec![Winner {
                title: winner.into(),
                rule: rule("Winner"),
                unresolved: false,
            }],
            alternatives: vec![Alternative {
                title: clone.into(),
                clone_of: winner.into(),
                rule: rule(message),
            }],
            exclusions: vec![],
            choice: None,
            drift: false,
        }
    }

    #[test]
    fn a_different_winner_differs_and_a_different_rule_is_only_explained_differently() {
        let bundled = selection(vec![
            group("a", "A (USA)", "A (Japan)", "Region order"),
            group("b", "B (USA)", "B (Japan)", "Region order"),
            group("c", "C (USA)", "C (Japan)", "Region order"),
        ]);
        let builtin = selection(vec![
            group("a", "A (USA)", "A (Japan)", "Region order"),
            group("b", "B (Japan)", "B (USA)", "Region order"),
            group("c", "C (USA)", "C (Japan)", "Language order"),
        ]);
        let comparison = compare(&bundled, &builtin, &[], &[]);
        assert_eq!(
            (
                comparison.groups,
                comparison.differing,
                comparison.explained_differently
            ),
            (3, 1, 1)
        );
        let names: Vec<_> = comparison
            .differences
            .iter()
            .map(|d| (d.group.as_str(), d.explanation_only))
            .collect();
        assert_eq!(names, [("b", false), ("c", true)]);
        // The rule's reference doesn't count: mirror references share their message.
        let mut mirrored = bundled.clone();
        mirrored.groups[0].alternatives[0].rule.reference = Some("REF0002".into());
        assert!(compare(&bundled, &mirrored, &[], &[]).is_identical());
    }

    #[test]
    fn a_group_only_one_engine_reports_differs() {
        let bundled = selection(vec![group("a", "A (USA)", "A (Japan)", "Region order")]);
        let builtin = selection(vec![]);
        let comparison = compare(&bundled, &builtin, &["w".into()], &[]);
        assert_eq!(comparison.differing, 1);
        assert_eq!(comparison.differences[0].releases.len(), 2);
        assert!(comparison.warnings.is_some());
    }
}
