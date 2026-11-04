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
    /// result for the component's whole projection under `links`: every unit linked to it in the original runs or
    /// in the rerun, including units only the rerun has, and units of groups only the rerun links to it.
    fn consider(
        &mut self,
        seed: u64,
        links: &Links,
        rerun: &BTreeMap<String, String>,
        builtin: &BTreeMap<String, String>,
        differing: &BTreeMap<String, [Option<String>; 2]>,
    ) {
        if self.witness.is_some() {
            return;
        }
        let reproduces = |unit: &str| rerun.get(unit) == builtin.get(unit);
        if links.projection(&self.units).all(reproduces) {
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
const TRACE_EXCLUDED: &str = "trace excluded\t";
const WARNINGS: &str = "warnings";
const DAT_NAMES: &str = "trace dat_names";

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
        conformance.components = components(&a, &b, &conformance.differing);
        for seed in 1..=u64::from(set_orders) {
            if conformance.components.iter().all(|c| c.witness.is_some()) {
                break;
            }
            conformance.set_orders += 1;
            let Ok(reordered) = run(SelectionEngine::Bundled, Some(seed))? else {
                continue;
            };
            let p = units(&reordered)?;
            let links = Links::new(&[&a, &b, &p]);
            for component in &mut conformance.components {
                component.consider(seed, &links, &p, &b, &conformance.differing);
            }
        }
        Ok(conformance)
    }
}

/// The components of the units that differ between `a` and `b`, as `Links` joins them.
fn components(
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
    differing: &BTreeMap<String, [Option<String>; 2]>,
) -> Vec<Component> {
    if differing.is_empty() {
        return Vec::new();
    }
    let links = Links::new(&[a, b]);
    let mut by_root: BTreeMap<usize, Component> = BTreeMap::new();
    for unit in differing.keys() {
        if let Some(root) = links.root_of(unit) {
            by_root.entry(root).or_default();
        }
    }
    for (root, component) in &mut by_root {
        component.units = links.members[root]
            .iter()
            .map(|&i| links.keys[i].to_owned())
            .collect();
    }
    by_root.into_values().collect()
}

/// The units of some versions of a run (the two engines', and a rerun's), joined where they depend on each other:
/// units about the same group, and a unit that names titles of several groups (a step without a group names the
/// titles it kept and removed; a title's exclusion record names the group it was reported under).
///
/// One exception: an excluded title the bundled engine reports under whichever of its groups it met first, title by
/// title. Its units join each other, not its groups, but only where that provably doesn't depend on choosing or
/// filtering: every version excludes it alike apart from the group, every group reporting it exists in every
/// version, and no choosing record names it.
///
/// A numbered DAT's trace names titles without the number its Selection keeps, so every trace title is read as the
/// DAT name the trace's `dat_names` record maps it to.
struct Links<'k> {
    /// Every unit any version has, sorted.
    keys: Vec<&'k str>,
    /// The component each unit is in, by unit index.
    root: Vec<usize>,
    /// Each component's units, by unit index.
    members: HashMap<usize, Vec<usize>>,
}

impl<'k> Links<'k> {
    fn new(versions: &[&'k BTreeMap<String, String>]) -> Links<'k> {
        let keys: Vec<&str> = versions
            .iter()
            .flat_map(|v| v.keys().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
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
        let values = |key: &'k str| versions.iter().filter_map(move |v| v.get(key));
        let records = |key: &'k str| {
            values(key).flat_map(|v| {
                v.lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            })
        };
        let release = |key: &'k str| {
            key.strip_prefix(GROUP)
                .and_then(|rest| rest.split_once('\t'))
        };

        let mut dat_names: HashMap<String, String> = HashMap::new();
        for record in records(DAT_NAMES) {
            for (name, dat_name) in record["names"].as_object().into_iter().flatten() {
                if let Some(dat_name) = dat_name.as_str() {
                    dat_names.insert(name.clone(), dat_name.to_owned());
                }
            }
        }
        let canon = |title: &str| -> String {
            dat_names
                .get(title)
                .cloned()
                .unwrap_or_else(|| title.to_owned())
        };

        // The titles choosing records name: in a step's lists, or in a group's clone structure.
        let mut chosen: BTreeSet<String> = BTreeSet::new();
        for &key in &keys {
            if let Some((_, title)) = key
                .strip_prefix(TRACE_GROUP)
                .and_then(|rest| rest.split_once('\t'))
            {
                chosen.insert(canon(title));
            } else if key.starts_with(TRACE) && !key.starts_with(TRACE_EXCLUDED) {
                for record in records(key) {
                    for field in ["before", "after", "kept", "removed"] {
                        for title in record[field].as_array().into_iter().flatten() {
                            if let Some(title) = title.as_str() {
                                chosen.insert(canon(title));
                            }
                        }
                    }
                }
            }
        }
        // Each title's release units.
        let mut releases: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, &key) in keys.iter().enumerate() {
            if let Some((_, title)) = release(key) {
                releases.entry(title).or_default().push(i);
            }
        }
        // `title` is the trace's name for the title; its release units carry the DAT name.
        let stands_alone = |title: &str| -> bool {
            let record = format!("{TRACE_EXCLUDED}{title}");
            let title = canon(title);
            if chosen.contains(&title) {
                return false;
            }
            let without_group = |v: &String| {
                serde_json::from_str::<serde_json::Value>(v)
                    .ok()
                    .map(|mut v| {
                        v.as_object_mut().map(|o| o.remove("group"));
                        v
                    })
            };
            let reports: Vec<_> = versions.iter().map(|v| v.get(&record)).collect();
            if reports.iter().any(Option::is_none) {
                return false;
            }
            let first = without_group(reports[0].unwrap());
            if first.is_none() || reports.iter().any(|r| without_group(r.unwrap()) != first) {
                return false;
            }
            let mut exclusion: Option<&String> = None;
            for &i in releases
                .get(title.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let (group, _) = release(keys[i]).expect("a release unit");
                if versions
                    .iter()
                    .any(|v| !v.contains_key(&format!("{GROUP}{group}")))
                {
                    return false;
                }
                for value in values(keys[i]) {
                    let is_exclusion = serde_json::from_str::<serde_json::Value>(value)
                        .is_ok_and(|v| v["role"] == "exclusion");
                    if !is_exclusion || exclusion.is_some_and(|e| e != value) {
                        return false;
                    }
                    exclusion = Some(value);
                }
            }
            true
        };
        let alone: BTreeSet<String> = keys
            .iter()
            .filter_map(|k| k.strip_prefix(TRACE_EXCLUDED))
            .filter(|t| stands_alone(t))
            .map(canon)
            .collect();

        let mut excluded: HashMap<String, usize> = HashMap::new();
        let mut groups: HashMap<&str, usize> = HashMap::new();
        let mut title_groups: HashMap<&str, Vec<&str>> = HashMap::new();
        for (i, &key) in keys.iter().enumerate() {
            let title = key
                .strip_prefix(TRACE_EXCLUDED)
                .map(canon)
                .or_else(|| release(key).map(|(_, t)| t.to_owned()));
            if let Some(title) = title.filter(|t| alone.contains(t)) {
                match excluded.get(&title) {
                    Some(&first) => join(&mut parent, first, i),
                    None => {
                        excluded.insert(title, i);
                    }
                }
                continue;
            }
            if let Some(group) = Conformance::group_of(key) {
                match groups.get(group) {
                    Some(&first) => join(&mut parent, first, i),
                    None => {
                        groups.insert(group, i);
                    }
                }
                if let Some((_, title)) = release(key) {
                    title_groups.entry(title).or_default().push(group);
                }
            }
        }
        for (i, &key) in keys.iter().enumerate() {
            if Conformance::group_of(key).is_some() || key == WARNINGS {
                continue;
            }
            if key
                .strip_prefix(TRACE_EXCLUDED)
                .is_some_and(|t| alone.contains(&canon(t)))
            {
                continue;
            }
            for record in records(key) {
                let mut named: Vec<&str> = Vec::new();
                if let Some(group) = record["group"].as_str() {
                    named.extend(groups.get_key_value(group).map(|(g, _)| *g));
                }
                for field in ["before", "after", "kept", "removed"] {
                    for title in record[field].as_array().into_iter().flatten() {
                        if let Some(in_groups) = title
                            .as_str()
                            .and_then(|t| title_groups.get(canon(t).as_str()))
                        {
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
        let root: Vec<usize> = (0..keys.len()).map(|i| root(&mut parent, i)).collect();
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, &r) in root.iter().enumerate() {
            members.entry(r).or_default().push(i);
        }
        Links {
            keys,
            root,
            members,
        }
    }

    fn root_of(&self, key: &str) -> Option<usize> {
        self.keys.binary_search(&key).ok().map(|i| self.root[i])
    }

    /// Every unit in the components `units` fall in.
    fn projection<'a>(&'a self, units: &'a BTreeSet<String>) -> impl Iterator<Item = &'k str> + 'a {
        let roots: BTreeSet<usize> = units.iter().filter_map(|u| self.root_of(u)).collect();
        roots
            .into_iter()
            .flat_map(|r| self.members[&r].iter().map(|&i| self.keys[i]))
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
                format!("{TRACE_EXCLUDED}{}", text("title")),
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
        let mut components = components(a, b, &differing);
        for (seed, rerun) in reruns.iter().enumerate() {
            let links = Links::new(&[a, b, rerun]);
            for c in &mut components {
                c.consider(seed as u64 + 1, &links, rerun, b, &differing);
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
    fn each_excluded_title_is_its_own_component_whichever_group_reports_it() {
        let excluded = |title: &str, group: &str| {
            format!(r#"{{"group":"{group}","title":"{title}","type":"excluded"}}"#)
        };
        let exclusion = r#"{"role":"exclusion"}"#;
        let reported = |fallout: &str, oblivion: &str| {
            let mut m = map(&[
                ("group fallout", "{}"),
                ("group fallout dlc", "{}"),
                (
                    "group fallout dlc\tFallout DLC (Japan)",
                    "{\"role\":\"winner\"}",
                ),
                ("group fallout\tFallout (USA)", "{\"role\":\"winner\"}"),
                (
                    "group fallout\tFallout (Japan)",
                    "{\"role\":\"alternative\"}",
                ),
            ]);
            for (title, group) in [
                ("Fallout GOTY (USA)", fallout),
                ("Fallout GOTY (Europe)", oblivion),
            ] {
                m.insert(format!("group {group}\t{title}"), exclusion.into());
                m.insert(format!("trace excluded\t{title}"), excluded(title, group));
            }
            m
        };
        let a = reported("fallout", "fallout");
        let b = reported("fallout dlc", "fallout dlc");
        // Each rerun moves one title's report, so no single one moves both.
        let c = conformance_of(
            &a,
            &b,
            &[
                reported("fallout dlc", "fallout"),
                reported("fallout", "fallout dlc"),
            ],
        );
        assert_eq!(c.components.len(), 2);
        assert_eq!(c.verdict(), Verdict::OrderSensitive);
        assert_eq!(c.witness("trace excluded\tFallout GOTY (USA)"), Some(1));
        assert_eq!(
            c.witness("group fallout dlc\tFallout GOTY (Europe)"),
            Some(2)
        );
    }

    fn excluded(title: &str, group: &str, reason: &str) -> String {
        format!(r#"{{"group":"{group}","reason":"{reason}","title":"{title}","type":"excluded"}}"#)
    }

    /// A title in groups `pong` and `pong dlc`, excluded for `reason` and reported under `group`.
    fn superset(group: &str, reason: &str) -> BTreeMap<String, String> {
        let mut m = map(&[
            ("group pong", "{}"),
            ("group pong dlc", "{}"),
            ("group pong\tPong (USA)", r#"{"role":"winner"}"#),
            ("group pong dlc\tPong DLC (USA)", r#"{"role":"winner"}"#),
        ]);
        let title = "Pong Pack (USA)";
        m.insert(
            format!("group {group}\t{title}"),
            format!(r#"{{"reason":"{reason}","role":"exclusion"}}"#),
        );
        m.insert(
            format!("trace excluded\t{title}"),
            excluded(title, group, reason),
        );
        m
    }

    #[test]
    fn a_rerun_that_matches_every_original_unit_but_adds_a_clone_is_no_witness() {
        let a = map(&[
            ("group pong", "{}"),
            ("group pong\tPong (USA)", "winner"),
            ("group pong\tPong (Europe)", "alternative"),
        ]);
        let mut b = a.clone();
        b.insert("group pong\tPong (USA)".into(), "alternative".into());
        b.insert("group pong\tPong (Europe)".into(), "winner".into());
        let mut clone = b.clone();
        clone.insert(
            "trace group pong\tPong (Brazil)".into(),
            "clone of Pong (USA)".into(),
        );
        let c = conformance_of(&a, &b, &[clone]);
        assert_eq!(c.verdict(), Verdict::Inconclusive);
        assert_eq!(c.order_sensitive(), Vec::<&String>::new());
        let c = conformance_of(&a, &b, &[b.clone()]);
        assert_eq!(c.verdict(), Verdict::OrderSensitive);
    }

    #[test]
    fn a_rerun_that_links_an_agreeing_group_must_reproduce_it_too() {
        let a = map(&[
            ("group pong", "{}"),
            ("group combo", "{}"),
            ("group pong\tPong (USA)", "winner"),
            ("group pong\tPong (Europe)", "alternative"),
            ("group combo\tCombo (USA)", "winner"),
        ]);
        let mut b = a.clone();
        b.insert("group pong\tPong (USA)".into(), "alternative".into());
        b.insert("group pong\tPong (Europe)".into(), "winner".into());
        // The rerun reproduces group pong, but a compilation step it runs links combo, which it changes.
        let mut linked = b.clone();
        linked.insert("trace compilations global".into(), STEP.into());
        linked.insert("group combo\tCombo (USA)".into(), "alternative".into());
        let c = conformance_of(&a, &b, &[linked]);
        assert_eq!(c.witness("group pong\tPong (USA)"), None);
        assert!(!c.conforms());
    }

    #[test]
    fn a_numbered_dat_s_trace_names_link_to_its_selection_names() {
        // A numbered DAT's trace drops the number its Selection keeps; the trace's own record maps one to the other.
        const NAMES: &str = r#"{"type":"dat_names","names":{"Combo (USA)":"0002 - Combo (USA)","Pong (USA)":"0001 - Pong (USA)"}}"#;
        let a = map(&[
            ("group pong", "{}"),
            ("group combo", "{}"),
            ("group pong\t0001 - Pong (USA)", "winner"),
            ("group combo\t0002 - Combo (USA)", "winner"),
            ("trace dat_names", NAMES),
        ]);
        let mut b = a.clone();
        b.insert("trace compilations global".into(), STEP.into());
        b.insert("group pong\t0001 - Pong (USA)".into(), "exclusion".into());
        let step_only = {
            let mut r = a.clone();
            r.insert("trace compilations global".into(), STEP.into());
            r
        };
        let pong_only = {
            let mut r = a.clone();
            r.insert("group pong\t0001 - Pong (USA)".into(), "exclusion".into());
            r
        };
        // The step removing Pong (USA) and pong's placement are one component: neither rerun alone witnesses it.
        let c = conformance_of(&a, &b, &[step_only.clone(), pong_only.clone()]);
        assert_eq!(c.components.len(), 1, "{:?}", c.components);
        assert_eq!(c.verdict(), Verdict::Inconclusive);
        let c = conformance_of(&a, &b, &[step_only, pong_only, b.clone()]);
        assert_eq!(c.witness("group pong\t0001 - Pong (USA)"), Some(3));
        assert_eq!(c.witness("trace compilations global"), Some(3));
        assert_eq!(c.verdict(), Verdict::OrderSensitive);
    }

    #[test]
    fn an_excluded_title_stays_with_its_groups_unless_its_exclusion_provably_stands_alone() {
        // The same exclusion, reported under either group: the title's units stand alone.
        let a = superset("pong", "Language remove");
        let b = superset("pong dlc", "Language remove");
        let mut moved = b.clone();
        moved.insert(
            "group pong\tPong (USA)".into(),
            r#"{"role":"alternative"}"#.into(),
        );
        let c = conformance_of(&a, &b, &[moved.clone()]);
        assert_eq!(c.components.len(), 1);
        assert_eq!(c.verdict(), Verdict::OrderSensitive);

        // Excluded for different reasons: the exclusion depends on filtering, so the title joins its groups, and the
        // rerun that changed group pong is no witness.
        let b = superset("pong dlc", "Region remove");
        let mut moved = b.clone();
        moved.insert(
            "group pong\tPong (USA)".into(),
            r#"{"role":"alternative"}"#.into(),
        );
        let c = conformance_of(&a, &b, &[moved]);
        assert_eq!(c.components.len(), 1);
        assert!(c.components[0].units.contains("group pong\tPong (USA)"));
        assert_eq!(c.verdict(), Verdict::Inconclusive);

        // A group that exists only to report it: the title joins that group.
        let mut a = superset("pong", "Language remove");
        a.remove("group pong dlc");
        a.remove("group pong dlc\tPong DLC (USA)");
        let b = superset("pong dlc", "Language remove");
        let c = conformance_of(&a, &b, &[]);
        assert_eq!(c.components.len(), 1);
        assert!(
            c.components[0]
                .units
                .contains("group pong dlc\tPong DLC (USA)")
        );

        // A title a choosing step names depends on choosing.
        let mut a = superset("pong", "Language remove");
        a.insert(
            "trace compilations global".into(),
            r#"{"type":"step","removed":["Pong Pack (USA)","Pong (USA)"]}"#.into(),
        );
        let mut b = superset("pong dlc", "Language remove");
        b.insert(
            "trace compilations global".into(),
            a["trace compilations global"].clone(),
        );
        let c = conformance_of(&a, &b, &[]);
        assert_eq!(c.components.len(), 1);
        assert!(c.components[0].units.contains("group pong\tPong (USA)"));
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

        let c = conformance_of(&a, &b, std::slice::from_ref(&a));
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
