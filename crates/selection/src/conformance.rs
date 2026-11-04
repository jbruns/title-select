//! Conformance: the built-in engine against the bundled one on the same DAT and Profile.
//!
//! Retool's sets of titles hash by identity, so the order they iterate in follows the addresses CPython allocates.
//! Some of its tie-breaks, and many of the rules it records, depend on that order, which no port can reproduce. A
//! difference is therefore classed as order-sensitive when the bundled engine itself, run with its sets iterated in
//! another order (`RunOptions::set_order`), gives the built-in engine's result for the same unit; anything else is a
//! real difference.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use title_select_api::selection::{EngineComparison, SelectionEngine, SelectionProfile};

use crate::{Engine, Error, Outcome, RunOptions, compare, stable_trace};

/// How the engines compare on one DAT with one Profile.
#[derive(Debug, Clone, Default)]
pub struct Conformance {
    /// Why each engine failed, `[bundled, builtin]`; failing alike counts as agreeing.
    pub failures: [Option<String>; 2],
    /// The Selections compared, when both engines succeeded.
    pub comparison: Option<EngineComparison>,
    /// The units compared: each group's placements and each group's trace records.
    pub units: usize,
    /// The units that differ, each with the bundled and built-in versions.
    pub differing: BTreeMap<String, [Option<String>; 2]>,
    /// The differing units the bundled engine gave the built-in engine's result for under another set order.
    pub order_sensitive: BTreeSet<String>,
    /// The set orders tried.
    pub set_orders: u32,
}

impl Conformance {
    /// The engines agree, or differ only where the bundled engine's own result depends on set order.
    pub fn conforms(&self) -> bool {
        self.failures[0].is_some() == self.failures[1].is_some() && self.unexplained().is_empty()
    }

    /// The differing units that aren't order-sensitive.
    pub fn unexplained(&self) -> Vec<&String> {
        self.differing
            .keys()
            .filter(|u| !self.order_sensitive.contains(*u))
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

const GROUP: &str = "group ";
const TRACE_GROUP: &str = "trace group ";
const TRACE: &str = "trace ";
const WARNINGS: &str = "warnings";

impl Engine {
    /// Runs both engines over `dat` with `profile` and compares them unit by unit. Where they differ, it runs the
    /// bundled engine again with up to `set_orders` other set orders to find which differences are order-sensitive.
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
                shadow: false,
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
                conformance.failures = [
                    a.err().map(|e| e.to_string()),
                    b.err().map(|e| e.to_string()),
                ];
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
        for key in keys {
            let (x, y) = (a.get(key), b.get(key));
            if x != y {
                conformance
                    .differing
                    .insert(key.clone(), [x.cloned(), y.cloned()]);
            }
        }
        for seed in 1..=u64::from(set_orders) {
            if conformance.differing.len() == conformance.order_sensitive.len() {
                break;
            }
            conformance.set_orders += 1;
            let Ok(reordered) = run(SelectionEngine::Bundled, Some(seed))? else {
                continue;
            };
            let p = units(&reordered)?;
            for (key, [_, built]) in &conformance.differing {
                if p.get(key) == built.as_ref() {
                    conformance.order_sensitive.insert(key.clone());
                }
            }
        }
        Ok(conformance)
    }
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
