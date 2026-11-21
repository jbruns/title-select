//! The arcade selector (ADR 0027): selects from an Arcade DAT, a MAME ListXML, beside the Retool port. It drops
//! machines by filters on ListXML's signals, elects one machine per family (a parent and its clones) by an ordered
//! chain of steps, each one Rule, and lists each winner's Dependencies once (ADR 0025).
//!
//! Every signal is optional: a filter keeps a machine whose entry doesn't say, and a filter whose signal no machine of
//! the DAT carries is unavailable, which the Selection records. Until the support files are read (#251), category comes
//! from a ListXML-only heuristic, and the Selection says so; the curated list and the rating filter are unavailable
//! when they are on.
//!
//! Its settings are the Selection Profile's `arcade` block (`SelectionProfile::arcade_settings`).

mod category;
mod describe;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::json;
use title_select_api::selection::{
    Alternative, ArcadePins, CategorySource, DEPENDENCY_STAGE, Dependency, Exclusion, Rule,
    Selection, SelectionGroup, Winner,
};
use title_select_engine::dat::{Dat, DriverStatus, Machine, Signal};

pub use title_select_api::arcade::{
    ArcadeSettings, Category, ElectionStep, Orientation, Range, Variant,
};

use crate::Error;
use crate::native::UserRegex;

/// Each filter, by the name the Selection records it unavailable under, with the signals it reads.
const FILTER_SIGNALS: &[(&str, &[Signal])] = &[
    ("mechanical", &[Signal::Mechanical]),
    ("screenless", &[Signal::Displays]),
    ("software_lists", &[Signal::SoftwareLists]),
    (
        "category",
        &[Signal::SourceFile, Signal::SoftwareLists, Signal::Controls],
    ),
    ("quality", &[Signal::Driver]),
    ("year", &[Signal::Year]),
    ("players", &[Signal::Players]),
    ("controls", &[Signal::Controls]),
    ("orientation", &[Signal::Displays]),
    ("samples", &[Signal::Samples]),
];

/// A pattern of an include or exclude list.
enum Matcher {
    Part(String),
    Exact(String),
    Regex(UserRegex),
}

impl Matcher {
    fn new(line: &str) -> Result<Matcher, String> {
        Ok(if let Some(pattern) = line.strip_prefix('/') {
            Matcher::Regex(UserRegex::new(pattern)?)
        } else if let Some(exact) = line.strip_prefix('|') {
            Matcher::Exact(exact.to_lowercase())
        } else {
            Matcher::Part(line.to_lowercase())
        })
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Matcher::Part(part) => text.to_lowercase().contains(part),
            Matcher::Exact(exact) => text.to_lowercase() == *exact,
            Matcher::Regex(re) => re.search(text),
        }
    }
}

fn matchers(lines: &[String], list: &str) -> (Vec<Matcher>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for line in lines {
        match Matcher::new(line) {
            Ok(m) => out.push(m),
            Err(e) => problems.push(format!("{list} list: {line}: {e}")),
        }
    }
    (out, problems)
}

/// One machine as the selector reads it.
struct Candidate<'a> {
    name: &'a str,
    description: &'a str,
    machine: &'a Machine,
    parent: Option<&'a str>,
    described: describe::Described,
    has_disks: bool,
}

impl Candidate<'_> {
    fn family(&self) -> &str {
        self.parent.unwrap_or(self.name)
    }
}

/// What the selector decided, before the Operator's choices apply.
pub(crate) struct Elected {
    pub groups: Vec<SelectionGroup>,
    pub pins: ArcadePins,
    pub trace: Vec<serde_json::Value>,
}

/// Fails when the run must stop.
pub(crate) type Stop<'a> = &'a dyn Fn() -> Result<(), Error>;

/// Checks that the selector can read `settings`, the `scope` block of a Profile (`global` or a system's name): its
/// orders and control types, and the include and exclude lists' patterns.
pub(crate) fn check(settings: &ArcadeSettings, scope: &str) -> Result<(), Error> {
    let mut problems = settings.problems();
    if let Err(Error::Patterns(more)) = lists(settings) {
        problems.extend(more);
    }
    match problems.is_empty() {
        true => Ok(()),
        false => Err(Error::Patterns(
            problems
                .into_iter()
                .map(|p| format!("Your {scope} arcade settings: {p}."))
                .collect(),
        )),
    }
}

/// The include and exclude lists' matchers, or every line of them the selector can't read.
fn lists(settings: &ArcadeSettings) -> Result<(Vec<Matcher>, Vec<Matcher>), Error> {
    let (include, mut problems) = matchers(&settings.include, "include");
    let (exclude, more) = matchers(&settings.exclude, "exclude");
    problems.extend(more);
    match problems.is_empty() {
        true => Ok((include, exclude)),
        false => Err(Error::Patterns(problems)),
    }
}

/// Filters `dat`'s machines and elects one per family.
pub(crate) fn elect(dat: &Dat, settings: &ArcadeSettings, stop: Stop) -> Result<Elected, Error> {
    let (include, exclude) = lists(settings)?;
    let mut has_disks = vec![false; dat.games.len()];
    for rom in dat.roms.iter().filter(|r| r.disk) {
        has_disks[rom.game] = true;
    }
    let candidates: Vec<Candidate> = (0..dat.games.len())
        .map(|i| {
            let description = dat.descriptions.get(i).map_or("", String::as_str);
            Candidate {
                name: &dat.games[i],
                description,
                machine: &dat.machines[i],
                parent: dat.cloneof[i].as_deref(),
                described: describe::describe(description, dat.machines[i].manufacturer.as_deref()),
                has_disks: has_disks[i],
            }
        })
        .collect();

    let mut trace = vec![json!({
        "type": "category",
        "source": "listxml_heuristic",
        "message": "category from the ListXML heuristic: the support files aren't read yet",
    })];
    let mut unavailable: Vec<String> = FILTER_SIGNALS
        .iter()
        .filter(|(_, signals)| !signals.iter().any(|s| dat.carries(*s)))
        .map(|(filter, _)| filter.to_string())
        .collect();
    for filter in &unavailable {
        trace.push(json!({
            "type": "unavailable",
            "filter": filter,
            "message": "no machine of the DAT says, so the filter drops nothing",
        }));
    }
    // The curated list and bestgames aren't read yet (#251): on, they drop nothing, and the Selection says so.
    for (filter, on) in [
        ("curated", settings.curated),
        ("rating", settings.minimum_rating.is_some()),
    ] {
        if on {
            trace.push(json!({
                "type": "unavailable",
                "filter": filter,
                "message": "the support file isn't read yet, so the filter drops nothing",
            }));
            unavailable.push(filter.to_owned());
        }
    }

    let mut families: BTreeMap<&str, Vec<&Candidate>> = BTreeMap::new();
    for candidate in &candidates {
        families
            .entry(candidate.family())
            .or_default()
            .push(candidate);
    }

    let mut groups = Vec::new();
    for (family, members) in families {
        stop()?;
        let mut group = SelectionGroup {
            group: family.to_owned(),
            unresolved: false,
            winners: Vec::new(),
            alternatives: Vec::new(),
            exclusions: Vec::new(),
            dependencies: Vec::new(),
            choice: None,
            drift: false,
        };
        let mut left = Vec::new();
        for candidate in members {
            let included = include
                .iter()
                .any(|m| m.matches(candidate.name) || m.matches(candidate.description));
            match filter(candidate, settings, &exclude, included) {
                Some((stage, reason, message)) => {
                    trace.push(json!({
                        "type": "filter",
                        "machine": candidate.name,
                        "stage": stage,
                        "message": message,
                    }));
                    group.exclusions.push(Exclusion {
                        title: candidate.name.to_owned(),
                        rule: Rule {
                            stage: stage.to_owned(),
                            reference: None,
                            message,
                        },
                        reason,
                    });
                }
                None => left.push(candidate),
            }
        }
        elect_family(&mut group, left, settings, &mut trace);
        groups.push(group);
    }
    Ok(Elected {
        groups,
        pins: ArcadePins {
            build: dat.version.clone(),
            category: CategorySource::ListxmlHeuristic,
            unavailable,
        },
        trace,
    })
}

/// The first filter that drops `c`, as its stage, its short reason and its rule's message; `None` keeps it.
fn filter(
    c: &Candidate,
    settings: &ArcadeSettings,
    exclude: &[Matcher],
    included: bool,
) -> Option<(&'static str, String, String)> {
    let m = c.machine;
    let drop = |stage: &'static str, reason: &str, message: String| {
        Some((stage, reason.to_owned(), message))
    };
    if m.is_bios == Some(true) {
        return drop(
            "structural",
            "BIOS",
            "a BIOS, which isn't played on its own".into(),
        );
    }
    if m.is_device == Some(true) {
        return drop(
            "structural",
            "device",
            "a device, part of other machines".into(),
        );
    }
    if m.runnable == Some(false) {
        return drop(
            "structural",
            "not runnable",
            "MAME can't run it on its own".into(),
        );
    }
    if included {
        return None;
    }
    if exclude
        .iter()
        .any(|x| x.matches(c.name) || x.matches(c.description))
    {
        return drop(
            "list",
            "your exclude list",
            "matches your exclude list".into(),
        );
    }
    if settings.drop_mechanical && m.is_mechanical == Some(true) {
        return drop("hardware", "mechanical", "a mechanical machine".into());
    }
    if settings.drop_screenless && m.displays.as_ref().is_some_and(Vec::is_empty) {
        return drop("hardware", "screenless", "it has no screen".into());
    }
    if settings.drop_software_lists && m.has_software_list == Some(true) {
        return drop(
            "hardware",
            "software lists",
            "it names a software list, as computers and consoles do".into(),
        );
    }
    if let Some((category, why)) = category::heuristic(m)
        && settings.drop_categories.contains(&category)
    {
        return drop(
            "category",
            category.as_str(),
            format!("{} by the ListXML heuristic: {why}", category.as_str()),
        );
    }
    if let (Some(minimum), Some(status)) = (
        settings.minimum_driver_status,
        m.driver.as_ref().and_then(|d| d.status),
    ) && status < minimum
    {
        return drop(
            "quality",
            "driver status",
            format!(
                "driver status {}, below {}",
                status.as_str(),
                minimum.as_str()
            ),
        );
    }
    if let Some(variant) = c
        .described
        .variants
        .iter()
        .find(|v| settings.drop_variants.contains(v))
    {
        return drop(
            "variant",
            variant.as_str(),
            format!("a {}, from its description", variant.as_str()),
        );
    }
    if !settings.years.is_open()
        && let Some(year) = m.year.as_deref().and_then(|y| y.parse::<u32>().ok())
        && !settings.years.contains(year)
    {
        return drop(
            "year",
            "year",
            format!("from {year}, outside {}", settings.years.describe()),
        );
    }
    if !settings.players.is_open()
        && let Some(players) = m.players
        && !settings.players.contains(players)
    {
        return drop(
            "players",
            "players",
            format!(
                "for {players} {}, outside {}",
                if players == 1 { "player" } else { "players" },
                settings.players.describe()
            ),
        );
    }
    if let Some(controls) = &m.controls {
        if let Some(kind) = controls
            .iter()
            .find(|k| settings.forbid_controls.contains(*k))
        {
            return drop("controls", "controls", format!("it has {kind} controls"));
        }
        if !settings.require_controls.is_empty()
            && !controls
                .iter()
                .any(|k| settings.require_controls.contains(k))
        {
            let wanted: Vec<&str> = settings
                .require_controls
                .iter()
                .map(String::as_str)
                .collect();
            return drop(
                "controls",
                "controls",
                format!("it has none of these controls: {}", wanted.join(", ")),
            );
        }
    }
    if let Some(wanted) = settings.orientation
        && let Some(rotate) = m
            .displays
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.rotate)
    {
        let vertical = rotate % 180 == 90;
        if vertical != (wanted == Orientation::Vertical) {
            let (is, not) = match vertical {
                true => ("vertical", "horizontal"),
                false => ("horizontal", "vertical"),
            };
            return drop(
                "orientation",
                "orientation",
                format!("its screen is {is}, not {not}"),
            );
        }
    }
    if settings.drop_disks && c.has_disks {
        return drop("media", "disks", "it needs disks".into());
    }
    if settings.drop_samples && m.samples.as_ref().is_some_and(|s| !s.is_empty()) {
        return drop("media", "samples", "it plays samples".into());
    }
    None
}

/// One election step: each candidate's key (lower is better), and the messages for the candidates it keeps and
/// removes. Its stage is its `ElectionStep`'s.
struct Step {
    key: fn(&Candidate, &ArcadeSettings) -> Vec<i64>,
    describe: fn(&Candidate, &ArcadeSettings) -> String,
}

/// How a candidate ranks in an order: its place, then after every listed name, then, saying none, `unnamed`.
fn rank(names: &[String], order: &[String], unnamed: i64) -> i64 {
    if names.is_empty() {
        return unnamed;
    }
    names
        .iter()
        .map(|n| order.iter().position(|o| o == n).unwrap_or(order.len()) as i64)
        .min()
        .unwrap_or(order.len() as i64)
}

/// The steps, in `ElectionStep::ALL`'s order.
const STEPS: [Step; 6] = [
    Step {
        key: |c, _| {
            vec![match c.machine.driver.as_ref().and_then(|d| d.status) {
                Some(DriverStatus::Good) => 0,
                Some(DriverStatus::Imperfect) => 1,
                Some(DriverStatus::Preliminary) => 2,
                None => 3,
            }]
        },
        describe: |c, _| match c.machine.driver.as_ref().and_then(|d| d.status) {
            Some(status) => format!("driver status {}", status.as_str()),
            None => "no driver status".into(),
        },
    },
    Step {
        // A machine that names no region ranks after every region: a bootleg rarely names one.
        key: |c, s| {
            let unnamed = s.region_order.len() as i64 + 1;
            vec![rank(&c.described.regions, &s.region_order, unnamed)]
        },
        describe: |c, _| match c.described.regions.is_empty() {
            true => "names no region".into(),
            false => format!("region {}", c.described.regions.join(", ")),
        },
    },
    Step {
        // Most descriptions name no language, so one that names none ties with the first language.
        key: |c, s| vec![rank(&c.described.languages, &s.language_order, 0)],
        describe: |c, _| match c.described.languages.is_empty() {
            true => "names no language".into(),
            false => format!("language {}", c.described.languages.join(", ")),
        },
    },
    Step {
        key: |c, _| vec![c.parent.is_some() as i64],
        describe: |c, _| match c.parent {
            Some(_) => "a clone".into(),
            None => "the parent".into(),
        },
    },
    Step {
        key: |c, _| {
            c.described
                .revision
                .iter()
                .map(|n| -i64::from(*n))
                .collect()
        },
        describe: |c, _| revision_text(c.described.revision),
    },
    Step {
        key: |c, _| {
            vec![
                c.described
                    .variants
                    .iter()
                    .any(|v| matches!(v, Variant::Bootleg | Variant::Hack)) as i64,
            ]
        },
        describe: |c, _| match c
            .described
            .variants
            .iter()
            .find(|v| matches!(v, Variant::Bootleg | Variant::Hack))
        {
            Some(v) => format!("a {}", v.as_str()),
            None => "an original release".into(),
        },
    },
];

/// A revision as its description gives it: "version 1.2.0, rev 2, dated 19910522".
fn revision_text(revision: [u32; 5]) -> String {
    let [major, minor, patch, rev, date] = revision;
    let mut parts = Vec::new();
    if [major, minor, patch] != [0, 0, 0] {
        parts.push(format!("version {major}.{minor}.{patch}"));
    }
    if rev != 0 {
        parts.push(format!("rev {rev}"));
    }
    if date != 0 {
        parts.push(format!("dated {date}"));
    }
    match parts.is_empty() {
        true => "names no revision".into(),
        false => parts.join(", "),
    }
}

/// Elects one of `left`, the family's machines that every filter kept, as its winner, recording each step that
/// removed a candidate.
fn elect_family(
    group: &mut SelectionGroup,
    mut left: Vec<&Candidate>,
    settings: &ArcadeSettings,
    trace: &mut Vec<serde_json::Value>,
) {
    if left.is_empty() {
        return;
    }
    let mut removed: Vec<(&str, Rule)> = Vec::new();
    let mut decided = None;
    if left.len() == 1 {
        decided = Some(Rule {
            stage: "election".into(),
            reference: None,
            message: "the only machine its filters left in its family".into(),
        });
    }
    for &which in &settings.election {
        if decided.is_some() {
            break;
        }
        let step = &STEPS[which as usize];
        let best = left
            .iter()
            .map(|c| (step.key)(c, settings))
            .min()
            .expect("candidates are left");
        let (kept, lost): (Vec<&Candidate>, Vec<&Candidate>) = left
            .into_iter()
            .partition(|c| (step.key)(c, settings) == best);
        if !lost.is_empty() {
            let winning = (step.describe)(kept[0], settings);
            trace.push(json!({
                "type": "election",
                "group": group.group,
                "stage": which.stage(),
                "kept": kept.iter().map(|c| c.name).collect::<Vec<_>>(),
                "removed": lost.iter().map(|c| c.name).collect::<Vec<_>>(),
                "message": winning,
            }));
            for c in lost {
                removed.push((
                    c.name,
                    Rule {
                        stage: which.stage().into(),
                        reference: None,
                        message: format!(
                            "{}, which ranks below {winning}",
                            (step.describe)(c, settings)
                        ),
                    },
                ));
            }
            if kept.len() == 1 {
                decided = Some(Rule {
                    stage: which.stage().into(),
                    reference: None,
                    message: winning,
                });
            }
        }
        left = kept;
    }
    if decided.is_none() {
        left.sort_by_key(|c| c.name);
        trace.push(json!({
            "type": "election",
            "group": group.group,
            "stage": "election:shortname",
            "kept": [left[0].name],
            "removed": left[1..].iter().map(|c| c.name).collect::<Vec<_>>(),
            "message": "its shortname sorts first",
        }));
        for c in &left[1..] {
            removed.push((
                c.name,
                Rule {
                    stage: "election:shortname".into(),
                    reference: None,
                    message: format!("its shortname sorts after {}", left[0].name),
                },
            ));
        }
        decided = Some(Rule {
            stage: "election:shortname".into(),
            reference: None,
            message: "its shortname sorts first".into(),
        });
    }
    let winner = left[0].name;
    group.winners.push(Winner {
        title: winner.to_owned(),
        rule: decided.expect("an election decides"),
        unresolved: false,
    });
    for (title, rule) in removed {
        group.alternatives.push(Alternative {
            title: title.to_owned(),
            clone_of: winner.to_owned(),
            rule,
        });
    }
}

/// Lists each Dependency of `selection`'s winners once, in its own group (ADR 0025): every BIOS or device a winner
/// reaches through `romof` and `device_ref`, transitively. A Dependency moves out of its group's exclusions, where the
/// structural filter put it.
///
/// This is the selector's own listing. Counting a Game's family and its Dependencies' Game Status is #246's, which may
/// take this walk over.
pub(crate) fn list_dependencies(
    selection: &mut Selection,
    dat: &Dat,
    trace: &mut Vec<serde_json::Value>,
) {
    let index: HashMap<&str, usize> = dat
        .games
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let mut needed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for group in &selection.groups {
        for winner in &group.winners {
            for dependency in dependencies_of(&winner.title, dat, &index) {
                needed
                    .entry(dependency)
                    .or_default()
                    .insert(winner.title.clone());
            }
        }
    }
    let families: HashMap<&str, usize> = selection
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.group.as_str(), i))
        .collect();
    let mut placed = Vec::new();
    for (dependency, winners) in needed {
        let i = index[dependency.as_str()];
        let family = dat.cloneof[i].as_deref().unwrap_or(&dat.games[i]);
        let Some(&g) = families.get(family) else {
            continue;
        };
        let needed_by: Vec<String> = winners.into_iter().collect();
        trace.push(json!({
            "type": "dependency",
            "machine": dependency,
            "needed_by": needed_by,
        }));
        placed.push((g, dependency, needed_by));
    }
    for (g, title, needed_by) in placed {
        let group = &mut selection.groups[g];
        group.exclusions.retain(|e| e.title != title);
        group.dependencies.push(Dependency {
            rule: Rule {
                stage: DEPENDENCY_STAGE.into(),
                reference: None,
                message: format!("needed by {}", needed_by.join(", ")),
            },
            title,
            needed_by,
        });
    }
}

/// The BIOSes and devices `game` reaches through `romof` and `device_ref`, transitively.
fn dependencies_of(game: &str, dat: &Dat, index: &HashMap<&str, usize>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut seen = BTreeSet::from([game.to_owned()]);
    let mut queue = vec![game.to_owned()];
    while let Some(name) = queue.pop() {
        let Some(&i) = index.get(name.as_str()) else {
            continue;
        };
        let m = &dat.machines[i];
        if name != game && (m.is_bios == Some(true) || m.is_device == Some(true)) {
            found.insert(name.clone());
        }
        let next = m
            .romof
            .iter()
            .chain(m.device_refs.iter().flatten())
            .filter(|n| seen.insert(n.to_string()))
            .cloned()
            .collect::<Vec<_>>();
        queue.extend(next);
    }
    found
}
