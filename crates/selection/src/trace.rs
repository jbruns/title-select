//! Reads the engine's JSON Lines trace, the patched Retool's trace format, and derives the explained Selection rows from it.
//!
//! The trace has two kinds of step record. A snapshot (`after` present) is the set of candidates a scope holds
//! after a rule; it is deterministic. An action (`after` absent) reports one pairwise decision, and its order and
//! pairing followed Python's set iteration order in Retool, so rows are derived from snapshots wherever one names the
//! release, and from actions only as a fallback.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Deserialize;
use title_select_api::selection::{
    Alternative, CoverageDrift, Exclusion, Rule, SELECTION_TRACE_SCHEMA, SelectionGroup,
    SelectionSummary, Winner,
};

use crate::Error;

/// The stages in which Retool chooses between the releases of a group.
const CHOOSING: [&str; 4] = [
    "Parent selection",
    "Superset parent selection",
    "Compilation parent selection",
    "Compilations",
];
const INCLUDES: &str = "Includes";
/// The snapshot after Retool's fail-safe string comparison (`choose_highest_string`).
const FAIL_SAFE: &str = "REF0059";
/// Actions that break a tie: the fail-safe string comparison, and the compilation tie-breakers.
const TIE_BREAKERS: [&str; 5] = ["REF0057", "REF0058", "REF0075", "REF0093", "REF0105"];
/// Retool's synthetic groups for compilation winners and the releases they replace.
const COMPILATION_GROUPS: &str = "retool_compilations";

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Record {
    Run {
        schema: u32,
        system: String,
        clone_list: bool,
        metadata: bool,
    },
    DatNames {
        schema: u32,
        names: HashMap<String, String>,
    },
    Step {
        schema: u32,
        #[serde(flatten)]
        step: Step,
    },
    Group {
        schema: u32,
        group: String,
        parents: Vec<String>,
        clones: BTreeMap<String, String>,
    },
    Excluded {
        schema: u32,
        title: String,
        group: String,
        reason: String,
    },
    Coverage {
        schema: u32,
        clone_list_unmatched: Vec<String>,
        metadata_missing: Vec<String>,
    },
}

impl Record {
    fn schema(&self) -> u32 {
        match self {
            Record::Run { schema, .. }
            | Record::DatNames { schema, .. }
            | Record::Step { schema, .. }
            | Record::Group { schema, .. }
            | Record::Excluded { schema, .. }
            | Record::Coverage { schema, .. } => *schema,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Step {
    stage: String,
    group: String,
    scope: String,
    #[serde(rename = "ref")]
    reference: String,
    message: String,
    before: Option<Vec<String>>,
    after: Option<Vec<String>>,
    kept: Vec<String>,
    removed: Vec<String>,
}

/// A step with its release names resolved: a compilation's virtual entry (`:V: <name> • <compilation>`) is the
/// compilation.
struct Resolved {
    step: Step,
    before: BTreeSet<String>,
    after: Option<BTreeSet<String>>,
    kept: BTreeSet<String>,
    removed: BTreeSet<String>,
}

impl Resolved {
    fn new(step: Step) -> Self {
        let set = |names: &[String]| names.iter().map(|n| release(n).to_owned()).collect();
        Resolved {
            before: step.before.as_deref().map(set).unwrap_or_default(),
            after: step.after.as_deref().map(set),
            kept: set(&step.kept),
            removed: set(&step.removed),
            step,
        }
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        self.before
            .iter()
            .chain(self.after.iter().flatten())
            .chain(&self.kept)
            .chain(&self.removed)
    }

    fn choosing(&self) -> bool {
        CHOOSING.contains(&self.step.stage.as_str())
    }

    /// Whether this step can explain a release of `group`. Compilation steps have no group, and compilation
    /// groups gather releases from every group.
    fn concerns(&self, group: &str) -> bool {
        self.step.group == group
            || self.step.group.is_empty()
            || group.starts_with(COMPILATION_GROUPS)
    }

    /// The candidates this snapshot removed, if it is a snapshot.
    fn eliminated(&self) -> Option<impl Iterator<Item = &String>> {
        let after = self.after.as_ref()?;
        Some(self.before.iter().filter(move |n| !after.contains(*n)))
    }

    fn rule(&self) -> Rule {
        let first = self.step.message.lines().next().unwrap_or_default();
        let message = first.trim().trim_end_matches(':').trim_end();
        Rule {
            stage: self.step.stage.clone(),
            reference: Some(self.step.reference.clone()),
            message: message
                .strip_prefix("ACTION: ")
                .unwrap_or(message)
                .to_owned(),
        }
    }
}

fn release(name: &str) -> &str {
    match name.split_once(" • ") {
        Some((virtual_name, compilation)) if virtual_name.starts_with(":V: ") => compilation,
        _ => name,
    }
}

fn synthetic(stage: &str, message: &str) -> Rule {
    Rule {
        stage: stage.to_owned(),
        reference: None,
        message: message.to_owned(),
    }
}

/// What a trace says, as Selection rows.
pub(crate) struct Derived {
    pub system: String,
    pub groups: Vec<SelectionGroup>,
    pub summary: SelectionSummary,
    pub coverage_drift: CoverageDrift,
}

pub(crate) fn derive(trace: &str) -> Result<Derived, Error> {
    let mut run = None;
    let mut dat_names = HashMap::new();
    let mut coverage = None;
    let mut steps: Vec<Resolved> = Vec::new();
    let mut finals: Vec<(String, Vec<String>, BTreeMap<String, String>)> = Vec::new();
    let mut excluded: Vec<(String, String, String)> = Vec::new();
    for (number, line) in trace.lines().enumerate() {
        let record: Record = serde_json::from_str(line)
            .map_err(|e| Error::Trace(format!("line {}: {e}", number + 1)))?;
        if record.schema() != SELECTION_TRACE_SCHEMA {
            return Err(Error::Trace(format!(
                "line {}: schema {} where {SELECTION_TRACE_SCHEMA} is expected",
                number + 1,
                record.schema()
            )));
        }
        match record {
            Record::Run {
                system,
                clone_list,
                metadata,
                ..
            } => run = Some((system, clone_list, metadata)),
            Record::DatNames { names, .. } => dat_names = names,
            Record::Step { step, .. } => steps.push(Resolved::new(step)),
            Record::Group {
                group,
                parents,
                clones,
                ..
            } => finals.push((group, parents, clones)),
            Record::Excluded {
                title,
                group,
                reason,
                ..
            } => excluded.push((title, group, reason)),
            Record::Coverage {
                clone_list_unmatched,
                metadata_missing,
                ..
            } => coverage = Some((clone_list_unmatched, metadata_missing)),
        }
    }
    let (system, clone_list, metadata) = run.ok_or_else(|| Error::Trace("no run record".into()))?;
    let (unmatched, missing) = coverage
        .ok_or_else(|| Error::Trace("no coverage record: the run did not finish".into()))?;

    let mut by_release: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        let names: BTreeSet<&String> = step.names().collect();
        for name in names {
            by_release.entry(name).or_default().push(i);
        }
    }
    let explain = Explainer {
        steps: &steps,
        by_release: &by_release,
    };

    let mut rows: BTreeMap<String, SelectionGroup> = BTreeMap::new();
    for (group, parents, clones) in &finals {
        for parent in parents {
            let (rule, unresolved) = explain.winner(group, parent);
            let row = row(&mut rows, group);
            row.unresolved |= unresolved;
            row.winners.push(Winner {
                title: parent.clone(),
                rule,
                unresolved,
            });
        }
        for (clone, parent) in clones {
            let rule = explain.alternative(group, clone);
            row(&mut rows, group).alternatives.push(Alternative {
                title: clone.clone(),
                clone_of: parent.clone(),
                rule,
            });
        }
    }
    for (title, group, reason) in &excluded {
        let rule = explain.exclusion(title, reason);
        row(&mut rows, group).exclusions.push(Exclusion {
            title: title.clone(),
            rule,
            reason: reason.clone(),
        });
    }

    // Retool works on titles it renamed; a Selection names them as the DAT does.
    let rename = |name: &mut String| {
        if let Some(original) = dat_names.get(name.as_str()) {
            name.clone_from(original);
        }
    };
    let mut summary = SelectionSummary::default();
    let groups: Vec<SelectionGroup> = rows
        .into_values()
        .map(|mut row| {
            row.winners.iter_mut().for_each(|w| rename(&mut w.title));
            for alternative in &mut row.alternatives {
                rename(&mut alternative.title);
                rename(&mut alternative.clone_of);
            }
            row.exclusions.iter_mut().for_each(|e| rename(&mut e.title));
            row.winners.sort_by(|a, b| a.title.cmp(&b.title));
            row.alternatives.sort_by(|a, b| a.title.cmp(&b.title));
            row.exclusions.sort_by(|a, b| a.title.cmp(&b.title));
            summary.groups += 1;
            summary.winners += row.winners.len();
            summary.alternatives += row.alternatives.len();
            summary.exclusions += row.exclusions.len();
            summary.unresolved += usize::from(row.unresolved);
            row
        })
        .collect();
    let mut missing = missing;
    missing.iter_mut().for_each(rename);
    missing.sort();
    Ok(Derived {
        system,
        groups,
        summary,
        coverage_drift: CoverageDrift {
            clone_list,
            metadata,
            unmatched_clone_list_terms: unmatched,
            titles_without_metadata: missing,
        },
    })
}

fn row<'r>(rows: &'r mut BTreeMap<String, SelectionGroup>, name: &str) -> &'r mut SelectionGroup {
    rows.entry(name.to_owned())
        .or_insert_with(|| SelectionGroup {
            group: name.to_owned(),
            unresolved: false,
            winners: Vec::new(),
            alternatives: Vec::new(),
            exclusions: Vec::new(),
            choice: None,
            drift: false,
        })
}

struct Explainer<'a> {
    steps: &'a [Resolved],
    by_release: &'a HashMap<&'a str, Vec<usize>>,
}

impl Explainer<'_> {
    fn steps_of<'s>(
        &'s self,
        title: &str,
    ) -> impl DoubleEndedIterator<Item = (usize, &'s Resolved)> {
        self.by_release
            .get(title)
            .into_iter()
            .flatten()
            .map(|&i| (i, &self.steps[i]))
    }

    /// The rule that made `title` a winner, and whether a tie-breaker decided it.
    fn winner(&self, group: &str, title: &str) -> (Rule, bool) {
        if let Some((_, include)) = self.steps_of(title).rev().find(|(_, s)| {
            s.step.stage == INCLUDES && s.after.as_ref().is_some_and(|a| a.contains(title))
        }) {
            return (include.rule(), false);
        }
        let choosing = || {
            self.steps_of(title)
                .filter(move |(_, s)| s.choosing() && s.concerns(group))
        };
        let deciding = choosing().rev().find(|(_, s)| {
            s.after.as_ref().is_some_and(|a| a.contains(title))
                && s.eliminated().is_some_and(|mut e| e.next().is_some())
        });
        if let Some((i, step)) = deciding {
            return (step.rule(), self.tie_broken(i, title));
        }
        match choosing().find(|(_, s)| s.after.as_ref().is_some_and(|a| a.contains(title))) {
            Some((_, sole)) => (sole.rule(), false),
            None => (
                synthetic("Parent selection", "No other release competed"),
                false,
            ),
        }
    }

    /// Whether the snapshot at `deciding` follows a tie-breaker that kept `title`: the actions since the previous
    /// snapshot of the same scope.
    fn tie_broken(&self, deciding: usize, title: &str) -> bool {
        let snapshot = &self.steps[deciding];
        if snapshot.step.reference == FAIL_SAFE {
            return true;
        }
        let same_scope = |s: &Resolved| {
            s.step.stage == snapshot.step.stage
                && s.step.group == snapshot.step.group
                && s.step.scope == snapshot.step.scope
        };
        self.steps[..deciding]
            .iter()
            .rev()
            .filter(|s| same_scope(s))
            .take_while(|s| s.after.is_none())
            .any(|s| TIE_BREAKERS.contains(&s.step.reference.as_str()) && s.kept.contains(title))
    }

    /// The rule that removed `title` in favour of another release of its group.
    fn alternative(&self, group: &str, title: &str) -> Rule {
        let choosing = || {
            self.steps_of(title)
                .filter(move |(_, s)| s.choosing() && s.concerns(group))
        };
        if let Some((_, step)) = choosing()
            .rev()
            .find(|(_, s)| s.eliminated().is_some_and(|mut e| e.any(|n| n == title)))
        {
            return step.rule();
        }
        if let Some((_, step)) = choosing()
            .rev()
            .find(|(_, s)| s.after.is_none() && s.removed.contains(title))
        {
            return step.rule();
        }
        synthetic("Parent selection", "Assigned as a clone")
    }

    /// The rule that excluded `title`.
    fn exclusion(&self, title: &str, reason: &str) -> Rule {
        self.steps_of(title)
            .rev()
            .find(|(_, s)| {
                !s.choosing()
                    && s.step.stage != INCLUDES
                    && (s.after.as_ref().is_some_and(|a| a.contains(title))
                        || s.removed.contains(title))
            })
            .map(|(_, s)| s.rule())
            .unwrap_or_else(|| synthetic("Exclusions", reason))
    }
}

/// The records of a trace that do not depend on Python's set iteration order — every record but the actions —
/// one per line and sorted, for comparing traces across runs.
pub fn stable_trace(trace: &str) -> Result<String, Error> {
    let mut lines = Vec::new();
    for (number, line) in trace.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| Error::Trace(format!("line {}: {e}", number + 1)))?;
        if value["type"] == "step" && value["after"].is_null() {
            continue;
        }
        lines.push(serde_json::to_string(&value).expect("a JSON value serializes"));
    }
    lines.sort();
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(
        stage: &str,
        group: &str,
        scope: &str,
        reference: &str,
        before: Option<&[&str]>,
        after: Option<&[&str]>,
        kept: &[&str],
    ) -> String {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        serde_json::json!({
            "schema": 1, "type": "step", "stage": stage, "group": group, "scope": scope, "ref": reference,
            "message": format!("{reference} message:\nmore"), "before": before.map(names), "after": after.map(names),
            "kept": names(kept), "removed": [], "variables": [], "details": [],
        })
        .to_string()
    }

    fn trace(steps: &[String], groups: &[serde_json::Value]) -> String {
        let mut lines = vec![r#"{"schema":1,"type":"run","system":"Test (No-Intro)","clone_list":true,"metadata":false}"#.to_owned()];
        lines.extend(steps.iter().cloned());
        lines.extend(groups.iter().map(|g| g.to_string()));
        lines.push(r#"{"schema":1,"type":"coverage","clone_list_unmatched":["Gone"],"metadata_missing":[]}"#.to_owned());
        lines.join("\n")
    }

    const A_US: &str = "A (USA)";
    const A_EU: &str = "A (Europe)";
    const B_EU: &str = "B (Europe)";

    #[test]
    fn the_last_reduction_a_winner_survives_decides_it_and_a_fail_safe_before_it_does_not() {
        let steps = [
            step(
                "Parent selection",
                "a",
                "USA | a",
                "REF0074",
                None,
                Some(&[A_US]),
                &[],
            ),
            step(
                "Parent selection",
                "a",
                "Europe | a",
                "REF0058",
                Some(&[A_EU, B_EU]),
                None,
                &[B_EU],
            ),
            step(
                "Parent selection",
                "a",
                "Europe | a",
                FAIL_SAFE,
                Some(&[A_EU, B_EU]),
                Some(&[B_EU]),
                &[],
            ),
            step(
                "Parent selection",
                "a",
                "All regions",
                "REF0014",
                Some(&[A_US, B_EU]),
                Some(&[A_US]),
                &[],
            ),
        ];
        let group = serde_json::json!({"schema":1,"type":"group","group":"a","parents":[A_US],"clones":{A_EU: A_US, B_EU: A_US}});
        let derived = derive(&trace(&steps, &[group])).unwrap();
        let row = &derived.groups[0];
        assert!(!row.unresolved);
        assert_eq!(row.winners[0].rule.reference.as_deref(), Some("REF0014"));
        assert_eq!(row.winners[0].rule.message, "REF0014 message");
        assert_eq!(row.alternatives[0].title, A_EU);
        assert_eq!(
            row.alternatives[0].rule.reference.as_deref(),
            Some(FAIL_SAFE)
        );
        assert_eq!(
            row.alternatives[1].rule.reference.as_deref(),
            Some("REF0014")
        );
        assert_eq!(derived.coverage_drift.unmatched_clone_list_terms, ["Gone"]);
    }

    #[test]
    fn a_winner_decided_by_the_fail_safe_is_unresolved() {
        let steps = [
            step(
                "Parent selection",
                "b",
                "Europe | b",
                "REF0058",
                Some(&[A_EU, B_EU]),
                None,
                &[B_EU],
            ),
            step(
                "Parent selection",
                "b",
                "Europe | b",
                FAIL_SAFE,
                Some(&[A_EU, B_EU]),
                Some(&[B_EU]),
                &[],
            ),
            step(
                "Parent selection",
                "b",
                "All regions",
                "REF0013",
                None,
                Some(&[B_EU]),
                &[],
            ),
        ];
        let group = serde_json::json!({"schema":1,"type":"group","group":"b","parents":[B_EU],"clones":{A_EU: B_EU}});
        let derived = derive(&trace(&steps, &[group])).unwrap();
        assert!(derived.groups[0].unresolved);
        assert!(derived.groups[0].winners[0].unresolved);
        assert_eq!(derived.summary.unresolved, 1);
    }

    #[test]
    fn a_compilation_tie_breaker_before_the_deciding_snapshot_is_unresolved() {
        let compilation = "2 in 1 - A + B (USA)";
        let virtual_name = format!(":V: A (USA) • {compilation}");
        let steps = [
            step(
                "Compilations",
                "",
                "a",
                "REF0070",
                Some(&[A_US, &virtual_name]),
                Some(&[A_US, compilation]),
                &[],
            ),
            step(
                "Compilations",
                "",
                "a",
                "REF0075",
                Some(&[A_US, compilation]),
                None,
                &[A_US],
            ),
            step(
                "Compilations",
                "",
                "a",
                "REF0072",
                Some(&[A_US, compilation]),
                Some(&[A_US]),
                &[],
            ),
        ];
        let group = serde_json::json!({"schema":1,"type":"group","group":"a","parents":[A_US],"clones":{compilation: A_US}});
        let derived = derive(&trace(&steps, &[group])).unwrap();
        assert!(derived.groups[0].unresolved);
        assert_eq!(
            derived.groups[0].alternatives[0].rule.reference.as_deref(),
            Some("REF0072")
        );
    }

    #[test]
    fn a_trace_from_another_schema_is_refused() {
        let err =
            derive(r#"{"schema":2,"type":"run","system":"x","clone_list":true,"metadata":true}"#)
                .err()
                .unwrap();
        assert!(err.to_string().contains("schema 2"), "{err}");
    }
}
