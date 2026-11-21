//! The arcade selector (ADR 0027): selects from an Arcade DAT, a MAME ListXML, beside the Retool port. It drops
//! machines by filters on ListXML's signals, elects one machine per family (a parent and its clones) by an ordered
//! chain of steps, each one Rule, and lists each winner's Dependencies once (ADR 0025).
//!
//! Every signal is optional: a filter keeps a machine whose entry doesn't say, and a filter whose signal no machine of
//! the DAT carries is unavailable, which the Selection records. The Support Files are optional too: catver decides
//! category, and for a machine it doesn't list, or without it, a ListXML-only heuristic does; languages.ini gives the
//! language election its languages, else the description does; bestgames rates families. The Selection records the
//! ones it read, and a file behind the ListXML's MAME version as drift.
//!
//! The curated list, `curated-mame-arcade-romset`, is optional: on, it keeps only the families it names and prefers
//! the machine it names.

mod category;
mod describe;
mod support;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::json;
use title_select_api::selection::{
    Alternative, ArcadePins, CategorySource, CuratedPin, DEPENDENCY_STAGE, Dependency, Exclusion,
    Rule, Selection, SelectionGroup, Winner,
};
use title_select_api::support::SupportFileKind;
use title_select_engine::dat::{Dat, DriverStatus, Machine, Signal};

pub use category::Category;
pub use describe::Variant;
pub use support::{SupportFiles, Tier};

use crate::native::UserRegex;
use crate::{CuratedList, Error};

/// What the curated list is called where a Rule cites it.
const CURATED_NAME: &str = "curated-mame-arcade-romset";

/// What the arcade selector keeps and prefers. The defaults are ADR 0027's; the Selection Profile's `arcade` block
/// (#250) will set them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArcadeSettings {
    pub drop_mechanical: bool,
    /// Drop machines with no screen.
    pub drop_screenless: bool,
    /// Drop machines that name a software list: computers and consoles.
    pub drop_software_lists: bool,
    pub drop_categories: BTreeSet<Category>,
    /// Drop machines whose driver status is below this; `None` keeps every status.
    pub minimum_driver_status: Option<DriverStatus>,
    pub drop_variants: BTreeSet<Variant>,
    pub years: Range,
    pub players: Range,
    /// Keep only machines with at least one of these control types; empty keeps every machine.
    pub require_controls: BTreeSet<String>,
    /// Drop machines with any of these control types.
    pub forbid_controls: BTreeSet<String>,
    /// Keep only machines whose first screen is this way round; `None` keeps both.
    pub orientation: Option<Orientation>,
    /// Drop machines that need Disks (CHDs).
    pub drop_disks: bool,
    /// Drop machines that play samples.
    pub drop_samples: bool,
    /// Regions in order of preference, as `World` or `USA`; a region left out ranks after those listed.
    pub region_order: Vec<String>,
    pub language_order: Vec<String>,
    /// Machines kept whatever the filters say, but for BIOSes, devices and machines that can't run, in the override
    /// syntax: plain text matches part of a shortname or description, `|` all of one, `/` a regular expression.
    pub include: Vec<String>,
    /// Machines dropped, in the same syntax; `include` wins.
    pub exclude: Vec<String>,
    /// Drop families whose parent bestgames rates below this, as a tier's lower bound, such as 70 for
    /// `70 to 80 (Good)`; `None` keeps every rating.
    pub minimum_rating: Option<u32>,
    /// Drop families bestgames doesn't rate.
    pub drop_unrated: bool,
    /// Keep only the families the curated list names, and prefer the machine it names.
    pub curated: bool,
}

impl Default for ArcadeSettings {
    fn default() -> Self {
        ArcadeSettings {
            drop_mechanical: true,
            drop_screenless: true,
            drop_software_lists: true,
            drop_categories: [
                Category::NonArcade,
                Category::Gambling,
                Category::Tabletop,
                Category::BoardGame,
                Category::Quiz,
                Category::Mature,
            ]
            .into(),
            minimum_driver_status: Some(DriverStatus::Imperfect),
            drop_variants: BTreeSet::new(),
            years: Range::default(),
            players: Range::default(),
            require_controls: BTreeSet::new(),
            forbid_controls: BTreeSet::new(),
            orientation: None,
            drop_disks: false,
            drop_samples: false,
            region_order: describe::default_region_order(),
            language_order: describe::default_language_order(),
            include: Vec::new(),
            exclude: Vec::new(),
            minimum_rating: None,
            drop_unrated: false,
            curated: false,
        }
    }
}

impl ArcadeSettings {
    fn rates(&self) -> bool {
        self.minimum_rating.is_some() || self.drop_unrated
    }
}

/// An inclusive range; an open end is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Range {
    pub min: Option<u32>,
    pub max: Option<u32>,
}

impl Range {
    fn is_open(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    fn contains(&self, n: u32) -> bool {
        self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
    }

    fn describe(&self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!("{min} to {max}"),
            (Some(min), None) => format!("{min} or more"),
            (None, Some(max)) => format!("{max} or fewer"),
            (None, None) => "any number".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

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
            Err(e) => problems.push(format!("arcade {list} list: {line}: {e}")),
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
    /// From languages.ini where it lists the machine, else from the description.
    languages: Vec<String>,
    has_disks: bool,
    /// Whether the curated list names this machine.
    curated: bool,
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

/// Checks the include and exclude lists' patterns.
pub(crate) fn check(settings: &ArcadeSettings) -> Result<(), Error> {
    lists(settings).map(|_| ())
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

/// What every filter reads beside the machine.
struct Context<'a> {
    settings: &'a ArcadeSettings,
    support: &'a SupportFiles,
    /// The families the curated list names a machine of, when the settings keep to it.
    curated: Option<HashSet<&'a str>>,
}

/// Filters `dat`'s machines and elects one per family. `curated` is the curated list's pin and shortnames, when
/// the settings keep to it.
pub(crate) fn elect(
    dat: &Dat,
    settings: &ArcadeSettings,
    support: &SupportFiles,
    curated: Option<CuratedList>,
    stop: Stop,
) -> Result<Elected, Error> {
    let (include, exclude) = lists(settings)?;
    let mut has_disks = vec![false; dat.games.len()];
    for rom in dat.roms.iter().filter(|r| r.disk) {
        has_disks[rom.game] = true;
    }
    let index: HashMap<&str, usize> = dat
        .games
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let curated_url = curated
        .as_ref()
        .map(|list| format!("{}/blob/{}/ROMSET", list.pin.repository, list.pin.commit));
    let mut named = HashSet::new();
    let mut unmatched = Vec::new();
    for name in curated.iter().flat_map(|list| &list.names) {
        match index.get(name.as_str()) {
            Some(&i) => {
                named.insert(i);
            }
            None if !unmatched.contains(name) => unmatched.push(name.clone()),
            None => {}
        }
    }
    let candidates: Vec<Candidate> = (0..dat.games.len())
        .map(|i| {
            let description = dat.descriptions.get(i).map_or("", String::as_str);
            let described =
                describe::describe(description, dat.machines[i].manufacturer.as_deref());
            let languages = support
                .languages
                .as_ref()
                .and_then(|l| l.get(&dat.games[i]))
                .cloned()
                .unwrap_or_else(|| described.languages.clone());
            Candidate {
                name: &dat.games[i],
                description,
                machine: &dat.machines[i],
                parent: dat.cloneof[i].as_deref(),
                described,
                languages,
                has_disks: has_disks[i],
                curated: named.contains(&i),
            }
        })
        .collect();
    let context = Context {
        settings,
        support,
        curated: curated_url.as_ref().map(|_| {
            candidates
                .iter()
                .filter(|c| c.curated)
                .map(Candidate::family)
                .collect()
        }),
    };

    let mut trace = Vec::new();
    let mut category_fallbacks = 0;
    match (&support.catver, support.version(SupportFileKind::Catver)) {
        (Some(catver), Some(version)) => {
            trace.push(json!({
                "type": "category",
                "source": "catver",
                "message": format!(
                    "category from catver.ini, MAME {version}, and for a machine it doesn't list, from the ListXML \
                     heuristic"
                ),
            }));
            // A BIOS, a device or a machine that can't run is dropped before category, so it falls back to nothing.
            let structural = |c: &&Candidate| {
                c.machine.is_bios == Some(true)
                    || c.machine.is_device == Some(true)
                    || c.machine.runnable == Some(false)
            };
            for candidate in candidates
                .iter()
                .filter(|c| !catver.contains_key(c.name) && !structural(c))
            {
                category_fallbacks += 1;
                trace.push(json!({
                    "type": "category",
                    "machine": candidate.name,
                    "source": "listxml_heuristic",
                    "message": "catver.ini doesn't list it, so the ListXML heuristic decides its category",
                }));
            }
        }
        _ => trace.push(json!({
            "type": "category",
            "source": "listxml_heuristic",
            "message": "category from the ListXML heuristic: no catver.ini was read",
        })),
    }
    let build = support::version_number(&dat.version);
    let mut support_pins = support.pins.clone();
    for pin in &mut support_pins {
        pin.drift = support::version_number(&pin.mame_version) < build;
        if pin.drift {
            trace.push(json!({
                "type": "drift",
                "file": pin.kind.file_name(),
                "mame_version": pin.mame_version,
                "message": format!(
                    "{} is for MAME {}, behind this DAT's MAME {}: machines added since aren't in it, so they're \
                     read as they would be without it",
                    pin.kind.file_name(),
                    pin.mame_version,
                    dat.version.split_whitespace().next().unwrap_or_default()
                ),
            }));
        }
    }
    if !unmatched.is_empty() {
        trace.push(json!({
            "type": "curated",
            "unmatched": unmatched,
            "message": format!("{CURATED_NAME} names shortnames this DAT doesn't have, which match nothing"),
        }));
    }
    let mut unavailable: Vec<String> = FILTER_SIGNALS
        .iter()
        .filter(|(filter, _)| !(*filter == "category" && support.catver.is_some()))
        .filter(|(_, signals)| !signals.iter().any(|s| dat.carries(*s)))
        .map(|(filter, _)| filter.to_string())
        .collect();
    if settings.rates() && support.bestgames.is_none() {
        unavailable.push("rating".into());
    }
    for filter in &unavailable {
        trace.push(json!({
            "type": "unavailable",
            "filter": filter,
            "message": "no machine of the DAT says, so the filter drops nothing",
        }));
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
            match filter(candidate, &context, &exclude, included) {
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
        if !left.is_empty()
            && settings.rates()
            && let Some((bestgames, version)) = support.bestgames()
        {
            let message = match bestgames.get(family) {
                Some(tier) => rated(tier, version),
                None => format!(
                    "{}, and kept, as you keep unrated families",
                    unrated(version)
                ),
            };
            trace.push(json!({
                "type": "rating",
                "group": family,
                "message": message,
            }));
        }
        elect_family(
            &mut group,
            left,
            settings,
            curated_url.as_deref(),
            &mut trace,
        );
        groups.push(group);
    }
    Ok(Elected {
        groups,
        pins: ArcadePins {
            build: dat.version.clone(),
            category: match support.catver {
                Some(_) => CategorySource::Catver,
                None => CategorySource::ListxmlHeuristic,
            },
            unavailable,
            category_fallbacks,
            support_files: support_pins,
            curated: curated.map(|list| CuratedPin {
                repository: list.pin.repository,
                commit: list.pin.commit,
                unmatched,
            }),
        },
        trace,
    })
}

/// "rated 70–80 (Good) by progetto-SNAPS, MAME 0.280".
fn rated(tier: &Tier, version: &str) -> String {
    format!(
        "rated {} by progetto-SNAPS, MAME {version}",
        tier.describe()
    )
}

/// "unrated by progetto-SNAPS, MAME 0.280".
fn unrated(version: &str) -> String {
    format!("unrated by progetto-SNAPS, MAME {version}")
}

/// The first filter that drops `c`, as its stage, its short reason and its rule's message; `None` keeps it.
fn filter(
    c: &Candidate,
    context: &Context,
    exclude: &[Matcher],
    included: bool,
) -> Option<(&'static str, String, String)> {
    let settings = context.settings;
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
    if let Some(families) = &context.curated
        && !families.contains(c.family())
    {
        return drop(
            "curated",
            "not in the curated list",
            format!("{CURATED_NAME} names no machine of its family"),
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
    let catver = context.support.catver.as_ref();
    match catver.and_then(|catver| catver.get(c.name)) {
        Some(raw) => {
            if let Some(category) = category::from_catver(raw)
                .into_iter()
                .find(|k| settings.drop_categories.contains(k))
            {
                return drop(
                    "category",
                    category.as_str(),
                    format!("{} by catver.ini: {raw}", category.as_str()),
                );
            }
        }
        None => {
            if let Some((category, why)) = category::heuristic(m)
                && settings.drop_categories.contains(&category)
            {
                let by = match catver {
                    Some(_) => "the ListXML heuristic, as catver.ini doesn't list it",
                    None => "the ListXML heuristic",
                };
                return drop(
                    "category",
                    category.as_str(),
                    format!("{} by {by}: {why}", category.as_str()),
                );
            }
        }
    }
    if settings.rates()
        && let Some((bestgames, version)) = context.support.bestgames()
    {
        match bestgames.get(c.family()) {
            Some(tier)
                if settings
                    .minimum_rating
                    .is_some_and(|minimum| tier.min < minimum) =>
            {
                return drop(
                    "rating",
                    "rating",
                    format!(
                        "its family is {}, below {}",
                        rated(tier, version),
                        settings.minimum_rating.unwrap_or_default()
                    ),
                );
            }
            None if settings.drop_unrated => {
                return drop(
                    "rating",
                    "unrated",
                    format!(
                        "its family is {}, and you drop unrated families",
                        unrated(version)
                    ),
                );
            }
            _ => {}
        }
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
                status_name(status),
                status_name(minimum)
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

fn status_name(status: DriverStatus) -> &'static str {
    match status {
        DriverStatus::Good => "good",
        DriverStatus::Imperfect => "imperfect",
        DriverStatus::Preliminary => "preliminary",
    }
}

/// One election step: its stage, each candidate's key (lower is better), and the messages for the candidates it
/// keeps and removes.
struct Step {
    stage: &'static str,
    key: fn(&Candidate, &ArcadeSettings) -> Vec<i64>,
    describe: fn(&Candidate, &ArcadeSettings) -> String,
}

/// The election step the curated list decides, whose Rules cite the list.
const CURATED_STAGE: &str = "election:curated";

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

const STEPS: &[Step] = &[
    Step {
        stage: CURATED_STAGE,
        key: |c, _| vec![!c.curated as i64],
        describe: |c, _| match c.curated {
            true => format!("named by {CURATED_NAME}"),
            false => format!("not the machine {CURATED_NAME} names"),
        },
    },
    Step {
        stage: "election:status",
        key: |c, _| {
            vec![match c.machine.driver.as_ref().and_then(|d| d.status) {
                Some(DriverStatus::Good) => 0,
                Some(DriverStatus::Imperfect) => 1,
                Some(DriverStatus::Preliminary) => 2,
                None => 3,
            }]
        },
        describe: |c, _| match c.machine.driver.as_ref().and_then(|d| d.status) {
            Some(status) => format!("driver status {}", status_name(status)),
            None => "no driver status".into(),
        },
    },
    Step {
        stage: "election:region",
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
        stage: "election:language",
        // Most descriptions name no language, so one that names none ties with the first language.
        key: |c, s| vec![rank(&c.languages, &s.language_order, 0)],
        describe: |c, _| match c.languages.is_empty() {
            true => "names no language".into(),
            false => format!("language {}", c.languages.join(", ")),
        },
    },
    Step {
        stage: "election:parent",
        key: |c, _| vec![c.parent.is_some() as i64],
        describe: |c, _| match c.parent {
            Some(_) => "a clone".into(),
            None => "the parent".into(),
        },
    },
    Step {
        stage: "election:revision",
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
        stage: "election:variant",
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
/// removed a candidate. A Rule the curated list decides cites `curated_url`.
fn elect_family(
    group: &mut SelectionGroup,
    mut left: Vec<&Candidate>,
    settings: &ArcadeSettings,
    curated_url: Option<&str>,
    trace: &mut Vec<serde_json::Value>,
) {
    if left.is_empty() {
        return;
    }
    let mut removed: Vec<(&str, Rule)> = Vec::new();
    let mut decided = None;
    if let [only] = left[..] {
        decided = Some(match only.curated {
            true => Rule {
                stage: CURATED_STAGE.into(),
                reference: curated_url.map(str::to_owned),
                message: format!("named by {CURATED_NAME}"),
            },
            false => Rule {
                stage: "election".into(),
                reference: None,
                message: "the only machine its filters left in its family".into(),
            },
        });
    }
    for step in STEPS {
        if decided.is_some() {
            break;
        }
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
            let reference = curated_url
                .filter(|_| step.stage == CURATED_STAGE)
                .map(str::to_owned);
            trace.push(json!({
                "type": "election",
                "group": group.group,
                "stage": step.stage,
                "kept": kept.iter().map(|c| c.name).collect::<Vec<_>>(),
                "removed": lost.iter().map(|c| c.name).collect::<Vec<_>>(),
                "message": winning,
            }));
            for c in lost {
                removed.push((
                    c.name,
                    Rule {
                        stage: step.stage.into(),
                        reference: reference.clone(),
                        message: format!(
                            "{}, which ranks below {winning}",
                            (step.describe)(c, settings)
                        ),
                    },
                ));
            }
            if kept.len() == 1 {
                decided = Some(Rule {
                    stage: step.stage.into(),
                    reference,
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
