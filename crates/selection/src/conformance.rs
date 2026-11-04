//! Conformance: the built-in engine against the bundled one on the same DAT and Profile.
//!
//! Retool's sets of titles hash by identity, so the order they iterate in follows the addresses CPython allocates.
//! Some of its tie-breaks, and many of the rules it records, depend on that order, which no port can reproduce. So
//! where the engines differ, the bundled engine runs again with its sets iterated in other orders
//! (`RunOptions::set_order`). Differing units that depend on each other (one group's units, and the units that
//! name titles of several groups, such as a compilation's contention) form a component, and a component is
//! order-sensitive only when a single rerun, its witness, gives the built-in engine's result for every unit in it,
//! including those that agreed at first. Anything else stays a difference, or is inconclusive when some rerun
//! reproduced part of it but none all of it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Serialize;
use title_select_api::selection::{EngineComparison, SelectionEngine, SelectionProfile};

use crate::{Engine, Error, Outcome, RunOptions, compare, stable_trace};

/// What a conformance run found on one DAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Both engines made the same Selection and trace, or failed for the same reason.
    Same,
    /// They differ only where reruns of the bundled engine show its own result depends on order.
    OrderSensitive,
    /// They differ, and no rerun of the bundled engine gave the built-in engine's result.
    Different,
    /// They may differ: they failed for different or unknown reasons, or reruns reproduced only part of a
    /// difference before the reruns ran out.
    Inconclusive,
    /// The built-in engine doesn't support something the Profile uses.
    Unsupported,
}

impl Verdict {
    pub fn conforms(self) -> bool {
        matches!(self, Verdict::Same | Verdict::OrderSensitive)
    }
}

/// Differing units that depend on each other, and the rerun that reproduced the built-in engine's result for all of
/// its units, if one did.
#[derive(Debug, Clone, Default)]
pub struct Component {
    /// Every unit in the component, differing or not.
    pub units: BTreeSet<String>,
    /// The set order whose rerun gave the built-in engine's result for every unit.
    pub witness: Option<u64>,
    /// Some rerun gave the built-in engine's result for at least one of its differing units.
    pub partly_reproduced: bool,
}

impl Component {
    /// Takes the rerun with set order `seed`, whose units are `rerun`, as the witness if it gives `builtin`'s
    /// result for every unit; a rerun that reproduces some differing units but changes others, or changes a unit
    /// that agreed, is no witness.
    fn consider(
        &mut self,
        seed: u64,
        rerun: &BTreeMap<String, String>,
        builtin: &BTreeMap<String, String>,
        differing: &BTreeMap<String, [Option<String>; 2]>,
    ) {
        if self.witness.is_some() {
            return;
        }
        let reproduces = |unit: &String| rerun.get(unit) == builtin.get(unit);
        if self.units.iter().all(reproduces) {
            self.witness = Some(seed);
        } else if self
            .units
            .iter()
            .any(|u| differing.contains_key(u) && reproduces(u))
        {
            self.partly_reproduced = true;
        }
    }
}

/// How the engines compare on one DAT with one Profile.
#[derive(Debug, Clone, Default)]
pub struct Conformance {
    /// Why each engine failed, `[bundled, builtin]`.
    pub failures: [Option<String>; 2],
    /// What kind of failure each was, when it's one both engines can recognise (`failure_category`).
    pub failure_categories: [Option<&'static str>; 2],
    /// The built-in engine refused something it doesn't support.
    pub unsupported: bool,
    /// The Selections compared, when both engines succeeded.
    pub comparison: Option<EngineComparison>,
    /// The units compared: each group's placements and each group's trace records.
    pub units: usize,
    /// The units that differ, each with the bundled and built-in versions.
    pub differing: BTreeMap<String, [Option<String>; 2]>,
    /// The components the differing units fall in.
    pub components: Vec<Component>,
    /// The set orders tried.
    pub set_orders: u32,
}

impl Conformance {
    pub fn verdict(&self) -> Verdict {
        if self.unsupported {
            return Verdict::Unsupported;
        }
        match (&self.failures, &self.failure_categories) {
            ([None, None], _) => {}
            ([Some(_), Some(_)], [Some(a), Some(b)]) if a == b => return Verdict::Same,
            ([Some(_), Some(_)], _) => return Verdict::Inconclusive,
            _ => return Verdict::Different,
        }
        if self.components.iter().all(|c| c.witness.is_some()) {
            return match self.differing.is_empty() {
                true => Verdict::Same,
                false => Verdict::OrderSensitive,
            };
        }
        if self
            .components
            .iter()
            .any(|c| c.witness.is_none() && !c.partly_reproduced)
        {
            Verdict::Different
        } else {
            Verdict::Inconclusive
        }
    }

    /// The engines agree, or differ only where the bundled engine's own result depends on set order.
    pub fn conforms(&self) -> bool {
        self.verdict().conforms()
    }

    /// The witness seed of the component a differing unit falls in, if it has one.
    pub fn witness(&self, unit: &str) -> Option<u64> {
        self.components
            .iter()
            .find(|c| c.units.contains(unit))
            .and_then(|c| c.witness)
    }

    /// The differing units no witness explains.
    pub fn unexplained(&self) -> Vec<&String> {
        self.differing
            .keys()
            .filter(|u| self.witness(u).is_none())
            .collect()
    }

    /// The differing units a witness explains.
    pub fn order_sensitive(&self) -> Vec<&String> {
        self.differing
            .keys()
            .filter(|u| self.witness(u).is_some())
            .collect()
    }

    /// The group a unit is about, if any.
    pub fn group_of(unit: &str) -> Option<&str> {
        let rest = unit
            .strip_prefix(GROUP)
            .or_else(|| unit.strip_prefix(TRACE_GROUP))?;
        Some(rest.split('\t').next().unwrap_or(rest))
    }
}

/// What kind of failure `error` is, in terms both engines' messages map to; `None` when it's not one of them.
pub fn failure_category(error: &Error) -> Option<&'static str> {
    let message = match error {
        Error::Unsupported(_) => return Some("unsupported"),
        Error::Timeout(_) => return Some("time limit"),
        Error::Builtin(m) => m.as_str(),
        Error::Retool { output, .. } => output.as_str(),
        _ => return None,
    };
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.contains("isn't a compatible DAT file") {
        Some("not a DAT")
    } else if flat.contains("No valid titles in input DAT file")
        || flat.contains("the DAT has no titles with a size or a digest")
    {
        Some("no titles")
    } else {
        None
    }
}

const GROUP: &str = "group ";
const TRACE_GROUP: &str = "trace group ";
const TRACE: &str = "trace ";
const WARNINGS: &str = "warnings";

impl Engine {
    /// Runs both engines over `dat` with `profile` and compares them unit by unit. Where they differ, it runs the
    /// bundled engine again with up to `set_orders` other set orders, looking for a witness for each component.
    pub fn conformance(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
        set_orders: u32,
    ) -> Result<Conformance, Error> {
        let run = |engine: SelectionEngine, set_order: Option<u64>| {
            let options = RunOptions {
                engine,
                set_order,
                ..options.clone()
            };
            match self.run(dat, profile, &options) {
                Err(
                    e @ (Error::Cancelled
                    | Error::Install(_)
                    | Error::Profile(_)
                    | Error::Patterns(_)),
                ) => Err(e),
                ran => Ok(ran),
            }
        };
        let bundled = run(SelectionEngine::Bundled, None)?;
        let builtin = run(SelectionEngine::Builtin, None)?;
        let mut conformance = Conformance::default();
        let (bundled, builtin) = match (bundled, builtin) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => {
                let (a, b) = (a.err(), b.err());
                conformance.unsupported = matches!(b, Some(Error::Unsupported(_)));
                conformance.failure_categories = [
                    a.as_ref().and_then(failure_category),
                    b.as_ref().and_then(failure_category),
                ];
                conformance.failures = [a.map(|e| e.to_string()), b.map(|e| e.to_string())];
                return Ok(conformance);
            }
        };
        conformance.comparison = Some(compare(
            &bundled.selection,
            &builtin.selection,
            &bundled.warnings,
            &builtin.warnings,
        ));
        let (a, b) = (units(&bundled)?, units(&builtin)?);
        let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        conformance.units = keys.len();
        for &key in &keys {
            let (x, y) = (a.get(key), b.get(key));
            if x != y {
                conformance
                    .differing
                    .insert(key.clone(), [x.cloned(), y.cloned()]);
            }
        }
        conformance.components = components(&keys, &a, &b, &conformance.differing);
        for seed in 1..=u64::from(set_orders) {
            if conformance.components.iter().all(|c| c.witness.is_some()) {
                break;
            }
            conformance.set_orders += 1;
            let Ok(reordered) = run(SelectionEngine::Bundled, Some(seed))? else {
                continue;
            };
            let p = units(&reordered)?;
            for component in &mut conformance.components {
                component.consider(seed, &p, &b, &conformance.differing);
            }
        }
        Ok(conformance)
    }
}

/// The components of the units that differ: units join when they're about the same group, or when one names
/// titles of several groups (an excluded title's record names its group; a step without a group names the titles
/// it kept and removed).
fn components(
    keys: &BTreeSet<&String>,
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
    differing: &BTreeMap<String, [Option<String>; 2]>,
) -> Vec<Component> {
    if differing.is_empty() {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..keys.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn join(parent: &mut [usize], x: usize, y: usize) {
        let (x, y) = (root(parent, x), root(parent, y));
        parent[x] = y;
    }
    // Each group's first unit, and the group each title is placed in.
    let mut groups: HashMap<&str, usize> = HashMap::new();
    let mut title_groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        if let Some(group) = Conformance::group_of(key) {
            match groups.get(group) {
                Some(&first) => join(&mut parent, first, i),
                None => {
                    groups.insert(group, i);
                }
            }
            if let Some((_, title)) = key.split_once('\t') {
                title_groups.entry(title).or_default().push(group);
            }
        }
    }
    for (i, key) in keys.iter().enumerate() {
        if Conformance::group_of(key).is_some() || key.as_str() == WARNINGS {
            continue;
        }
        for version in [a.get(*key), b.get(*key)].into_iter().flatten() {
            for line in version.lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let mut named: Vec<&str> = Vec::new();
                if let Some(group) = value["group"].as_str() {
                    named.push(group);
                }
                for field in ["before", "after", "kept", "removed"] {
                    for title in value[field].as_array().into_iter().flatten() {
                        if let Some(in_groups) = title.as_str().and_then(|t| title_groups.get(t)) {
                            named.extend(in_groups.iter().copied());
                        }
                    }
                }
                for group in named {
                    if let Some(&first) = groups.get(group) {
                        join(&mut parent, first, i);
                    }
                }
            }
        }
    }
    let mut by_root: BTreeMap<usize, Component> = BTreeMap::new();
    for (i, key) in keys.iter().enumerate() {
        let r = root(&mut parent, i);
        by_root.entry(r).or_default().units.insert((*key).clone());
    }
    by_root
        .into_values()
        .filter(|c| c.units.iter().any(|u| differing.contains_key(u)))
        .collect()
}

/// An outcome as units that compare independently: each group's flags and each release's placement, each group's
/// trace records (records for a compilation's contention group are keyed by its stage and scope, and each excluded
/// title's record by its title), and the warnings. A release's unit is keyed by its group, a tab, and its title.
fn units(outcome: &Outcome) -> Result<BTreeMap<String, String>, Error> {
    let mut out = BTreeMap::new();
    for group in &outcome.selection.groups {
        let mut v = serde_json::to_value(group).expect("a group serializes");
        strip_references(&mut v);
        let object = v.as_object_mut().expect("a group is an object");
        for (role, key) in [
            ("winner", "winners"),
            ("alternative", "alternatives"),
            ("exclusion", "exclusions"),
        ] {
            let Some(serde_json::Value::Array(releases)) = object.remove(key) else {
                continue;
            };
            for mut release in releases {
                let title = release["title"].as_str().unwrap_or_default().to_owned();
                release["role"] = role.into();
                out.insert(
                    format!("{GROUP}{}\t{title}", group.group),
                    release.to_string(),
                );
            }
        }
        out.insert(format!("{GROUP}{}", group.group), v.to_string());
    }
    let mut trace: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in stable_trace(&outcome.trace)?.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| Error::Trace(e.to_string()))?;
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        if value["type"] == "group" {
            let group = text("group");
            for parent in value["parents"].as_array().into_iter().flatten() {
                let parent = parent.as_str().unwrap_or_default();
                out.insert(
                    format!("{TRACE_GROUP}{group}\t{parent}"),
                    "parent".to_owned(),
                );
            }
            for (clone, of) in value["clones"].as_object().into_iter().flatten() {
                out.insert(
                    format!("{TRACE_GROUP}{group}\t{clone}"),
                    format!("clone of {of}"),
                );
            }
            continue;
        }
        if value["type"] == "excluded" {
            // One record per title: the group it names is that of whichever copy of the title the run met first.
            out.insert(
                format!("{TRACE}excluded\t{}", text("title")),
                line.to_owned(),
            );
            continue;
        }
        let key = match text("group") {
            group if !group.is_empty() => format!("{TRACE_GROUP}{group}"),
            _ if value["type"] == "step" => format!("{TRACE}{} {}", text("stage"), text("scope")),
            _ => format!("{TRACE}{}", text("type")),
        };
        trace.entry(key).or_default().push(line.to_owned());
    }
    out.extend(trace.into_iter().map(|(k, v)| (k, v.join("\n"))));
    out.insert(WARNINGS.to_owned(), outcome.warnings.join("\n"));
    Ok(out)
}

/// Drops the references from rules: mirror references that share a message make them order-dependent.
fn strip_references(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(o) => {
            if o.contains_key("stage") && o.contains_key("message") {
                o.remove("ref");
            }
            o.values_mut().for_each(strip_references);
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(strip_references),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn differing(
        a: &BTreeMap<String, String>,
        b: &BTreeMap<String, String>,
    ) -> BTreeMap<String, [Option<String>; 2]> {
        a.keys()
            .chain(b.keys())
            .filter(|k| a.get(*k) != b.get(*k))
            .map(|k| (k.clone(), [a.get(k).cloned(), b.get(k).cloned()]))
            .collect()
    }

    fn conformance_of(
        a: &BTreeMap<String, String>,
        b: &BTreeMap<String, String>,
        reruns: &[BTreeMap<String, String>],
    ) -> Conformance {
        let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        let differing = differing(a, b);
        let mut components = components(&keys, a, b, &differing);
        for (seed, rerun) in reruns.iter().enumerate() {
            for c in &mut components {
                c.consider(seed as u64 + 1, rerun, b, &differing);
            }
        }
        Conformance {
            units: keys.len(),
            differing,
            components,
            set_orders: reruns.len() as u32,
            ..Default::default()
        }
    }

    const STEP: &str = r#"{"type":"step","kept":["Combo (USA)"],"removed":["Pong (USA)"]}"#;

    #[test]
    fn a_step_naming_titles_of_several_groups_joins_them_in_one_component() {
        let a = map(&[
            ("group pong\tPong (USA)", "winner"),
            ("group combo\tCombo (USA)", "winner"),
            ("group other\tOther (USA)", "winner"),
            ("trace compilations global", STEP),
            ("warnings", ""),
        ]);
        let mut b = a.clone();
        b.insert("group pong\tPong (USA)".into(), "alternative".into());
        b.insert("warnings".into(), "careful".into());
        let c = conformance_of(&a, &b, &[]);
        let units: Vec<Vec<&str>> = c
            .components
            .iter()
            .map(|c| c.units.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            units,
            [
                vec![
                    "group combo\tCombo (USA)",
                    "group pong\tPong (USA)",
                    "trace compilations global"
                ],
                vec!["warnings"]
            ]
        );
    }

    #[test]
    fn only_a_single_rerun_reproducing_a_whole_component_witnesses_it() {
        let a = map(&[
            ("group pong\tPong (USA)", "winner"),
            ("group pong\tPong (Europe)", "alternative"),
            ("group pong\tPong (Japan)", "alternative"),
        ]);
        let b = map(&[
            ("group pong\tPong (USA)", "alternative"),
            ("group pong\tPong (Europe)", "winner"),
            ("group pong\tPong (Japan)", "alternative"),
        ]);
        // One rerun reproduces each differing unit, and neither both.
        let usa = map(&[
            ("group pong\tPong (USA)", "alternative"),
            ("group pong\tPong (Europe)", "alternative"),
            ("group pong\tPong (Japan)", "alternative"),
        ]);
        let europe = map(&[
            ("group pong\tPong (USA)", "winner"),
            ("group pong\tPong (Europe)", "winner"),
            ("group pong\tPong (Japan)", "alternative"),
        ]);
        // This one reproduces both but changes the unit that agreed.
        let japan = map(&[
            ("group pong\tPong (USA)", "alternative"),
            ("group pong\tPong (Europe)", "winner"),
            ("group pong\tPong (Japan)", "winner"),
        ]);
        let c = conformance_of(&a, &b, &[usa.clone(), europe.clone(), japan.clone()]);
        assert_eq!(c.verdict(), Verdict::Inconclusive);
        assert!(!c.conforms());
        assert_eq!(c.order_sensitive(), Vec::<&String>::new());

        let c = conformance_of(&a, &b, &[usa, europe, japan, b.clone()]);
        assert_eq!(c.verdict(), Verdict::OrderSensitive);
        assert_eq!(c.witness("group pong\tPong (USA)"), Some(4));

        let c = conformance_of(&a, &b, &[a.clone()]);
        assert_eq!(c.verdict(), Verdict::Different);
        assert_eq!(conformance_of(&a, &a, &[]).verdict(), Verdict::Same);
    }

    #[test]
    fn two_failures_agree_only_when_they_fail_for_the_same_reason() {
        let failed = |bundled: Error, builtin: Error| {
            let unsupported = matches!(builtin, Error::Unsupported(_));
            Conformance {
                failure_categories: [failure_category(&bundled), failure_category(&builtin)],
                failures: [Some(bundled.to_string()), Some(builtin.to_string())],
                unsupported,
                ..Default::default()
            }
            .verdict()
        };
        let bundled_not_a_dat = || Error::Retool {
            status: "exit status: 1".into(),
            output: "• \"/x/text.dat\" isn't a\n  compatible DAT file.".into(),
        };
        let builtin_not_a_dat = || Error::Builtin("/x/text.dat isn't a compatible DAT file".into());
        assert_eq!(
            failed(bundled_not_a_dat(), builtin_not_a_dat()),
            Verdict::Same
        );
        let no_titles =
            Error::Builtin("x.dat: the DAT has no titles with a size or a digest".into());
        assert_eq!(
            failed(bundled_not_a_dat(), no_titles),
            Verdict::Inconclusive
        );
        let traceback = Error::Retool {
            status: "exit status: 1".into(),
            output: "EOFError: EOF when reading a line".into(),
        };
        assert_eq!(
            failed(traceback, builtin_not_a_dat()),
            Verdict::Inconclusive
        );
        assert_eq!(
            failed(
                bundled_not_a_dat(),
                Error::Unsupported("the regular expression \"/\\N{DIGIT ONE}\"".into())
            ),
            Verdict::Unsupported
        );
        let only_one = Conformance {
            failures: [None, Some("x".into())],
            ..Default::default()
        };
        assert_eq!(only_one.verdict(), Verdict::Different);
    }
}
