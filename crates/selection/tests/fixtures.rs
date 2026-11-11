//! The built-in engine over every one of Retool's fixture DATs with each of the test profiles below, against goldens
//! frozen while the patched Retool was still the oracle: the conformance harness that compared the two engines over
//! these same cases passed when they were written, so they record Retool's Selections (ADR 0015).
//!
//! There is one golden per DAT under `tests/goldens/fixtures/`, holding all sixteen profiles side by side; see
//! `render` for the format. A release build checks every DAT, in about a minute; a debug build, which is several
//! times slower, checks only the feature DATs and a few system DATs (`in_debug_run`). `TITLE_SELECT_REGENERATE_GOLDENS=1`
//! with `--release` rewrites them all, and `ONLY_DAT=<file name>` narrows a run to one DAT.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use title_select_api::selection::{Category, Rule, Selection, SelectionProfile};

/// Every DAT under the fixtures' `source/`.
fn fixture_dats() -> Vec<PathBuf> {
    let mut dats = Vec::new();
    let mut stack = vec![support::fixtures().join("source")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "dat") {
                dats.push(path);
            }
        }
    }
    dats.sort();
    dats
}

/// System DATs a debug build checks beside every feature DAT: mid-sized ones with clone lists and metadata, so
/// clone-list, metadata and 1G1R stages run over real-world shapes.
const DEBUG_SYSTEMS: &[&str] = &[
    "Nintendo - Wii U - Datfile.dat",
    "Nintendo - Nintendo 64 (BigEndian) (Private).dat",
];

/// Whether a debug build checks this DAT.
fn in_debug_run(dat: &Path) -> bool {
    dat.parent().is_some_and(|p| p.ends_with("features"))
        || DEBUG_SYSTEMS.contains(&dat.file_name().unwrap().to_str().unwrap())
}

/// The profiles each DAT runs with, in the goldens' column order: each of Retool's test configs with 1G1R off (the
/// stages before parent selection), then the same with 1G1R on.
fn profiles() -> Vec<(String, SelectionProfile)> {
    let mut out = Vec::new();
    for disable_1g1r in [true, false] {
        for config in [
            "user-config-regions-1.yaml",
            "user-config-regions-4.yaml",
            "user-config-languages-1.yaml",
            "user-config-languages-2.yaml",
            "user-config-clone-list-filters-1.yaml",
            "user-config-overrides.yaml",
            "user-config-post-filters.yaml",
        ] {
            let mut profile = support::profile_from_upstream_config(config);
            profile.options.disable_1g1r = disable_1g1r;
            profile.options.filter_languages = config.contains("languages");
            out.push((config.to_owned(), profile));
        }
        let mut profile = support::profile_from_upstream_config("user-config-regions-1.yaml");
        profile.options.disable_1g1r = disable_1g1r;
        profile.exclude = Category::ALL
            .into_iter()
            .filter(|c| *c != Category::Games)
            .collect();
        out.push(("user-config-regions-1.yaml, games only".to_owned(), profile));
    }
    out
}

/// Cases with 1G1R on that Retool itself doesn't finish in reasonable time or memory, so that the oracle never
/// checked: its compilation step traces every combination of a contention group's compilations.
const UNCHECKED: &[(&str, &str)] = &[(
    "user-config-overrides.yaml",
    "Nintendo - Nintendo Entertainment System (Headered) (Private).dat",
)];

/// What one profile made of one DAT.
type Ran = Result<(Selection, Vec<String>), String>;

fn run(dat: &Path, profiles: &[(String, SelectionProfile)]) -> Vec<Ran> {
    let engine = support::fixture_engine();
    let options = support::options();
    profiles
        .iter()
        .map(|(_, profile)| {
            engine
                .run(dat, profile, &options)
                .map(|o| (o.selection, o.warnings))
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Profile numbers, 1-based, as compact ranges: `1-3,5`.
fn ranges(profiles: &BTreeSet<usize>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut iter = profiles.iter().copied().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while iter.peek() == Some(&(end + 1)) {
            end = iter.next().unwrap();
        }
        out.push(match end == start {
            true => format!("{}", start + 1),
            false => format!("{}-{}", start + 1, end + 1),
        });
    }
    out.join(",")
}

fn rule_text(rule: &Rule, reason: Option<&str>) -> String {
    let mut text = rule.stage.clone();
    if let Some(reference) = &rule.reference {
        write!(text, " {reference}").unwrap();
    }
    write!(text, ": {}", rule.message).unwrap();
    if let Some(reason) = reason.filter(|r| *r != rule.message) {
        write!(text, " (reason: {reason})").unwrap();
    }
    text
}

/// The golden for one DAT: every profile's Selection side by side.
///
/// ```text
/// <header: the DAT, the profiles by column, the format>
/// system: <name> [profiles]
/// coverage: ... [profiles]
/// warning: <text> [profiles]
/// error: <text> [profiles]
/// rule r1: <stage> <ref>: <message> (reason: <exclusion reason>)
/// ...
/// == <group>
///    <one cell per profile for the group, unless it is g in every one> |
///    <one cell per profile for the release> | <release>
///      alternative to <winner> [profiles]
/// ```
///
/// A group's cell is `.` where the profile has no such group, `g` where it has, `u` where it is unresolved, with `d`
/// added where a release in it has no metadata. A release's cell is `.` where it isn't in the group, else its
/// placement and the rule that decided it: `w` winner, `u` unresolved winner, `a` alternative, `x` exclusion. An
/// alternative's winner is listed beneath it, unless it is the group's only winner.
fn render(dat: &Path, profiles: &[(String, SelectionProfile)], ran: &[Ran]) -> String {
    let n = profiles.len();
    let mut header: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut rules: BTreeMap<String, usize> = BTreeMap::new();
    /// Per group: its cells, and per release its cells and the winners it is an alternative to.
    #[derive(Default)]
    struct Group {
        cells: BTreeMap<usize, String>,
        releases: BTreeMap<String, BTreeMap<usize, (char, String)>>,
        clone_of: BTreeMap<(String, String), BTreeSet<usize>>,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (p, ran) in ran.iter().enumerate() {
        let (selection, warnings) = match ran {
            Ok(ran) => ran,
            Err(e) => {
                header.entry(format!("error: {e}")).or_default().insert(p);
                continue;
            }
        };
        let mut note = |line: String| header.entry(line).or_default().insert(p);
        note(format!("system: {}", selection.system));
        let drift = &selection.coverage_drift;
        note(format!(
            "coverage: clone list {}, metadata {}",
            drift.clone_list, drift.metadata
        ));
        for term in &drift.unmatched_clone_list_terms {
            note(format!("unmatched clone-list term: {term}"));
        }
        for warning in warnings {
            note(format!("warning: {warning}"));
        }
        for group in &selection.groups {
            let entry = groups.entry(group.group.clone()).or_default();
            let mut cell = String::from(if group.unresolved { "u" } else { "g" });
            if group.drift {
                cell.push('d');
            }
            entry.cells.insert(p, cell);
            let sole_winner = match group.winners.as_slice() {
                [only] => Some(only.title.as_str()),
                _ => None,
            };
            let mut place = |title: &str, kind: char, rule: String| {
                rules.entry(rule.clone()).or_default();
                entry
                    .releases
                    .entry(title.to_owned())
                    .or_default()
                    .insert(p, (kind, rule));
            };
            for w in &group.winners {
                let kind = if w.unresolved { 'u' } else { 'w' };
                place(&w.title, kind, rule_text(&w.rule, None));
            }
            for a in &group.alternatives {
                place(&a.title, 'a', rule_text(&a.rule, None));
                if sole_winner != Some(a.clone_of.as_str()) {
                    entry
                        .clone_of
                        .entry((a.title.clone(), a.clone_of.clone()))
                        .or_default()
                        .insert(p);
                }
            }
            for x in &group.exclusions {
                place(&x.title, 'x', rule_text(&x.rule, Some(&x.reason)));
            }
        }
    }
    for (i, number) in rules.values_mut().enumerate() {
        *number = i + 1;
    }
    let width = 1 + rules.len().max(1).to_string().len();

    let mut out = String::new();
    writeln!(
        out,
        "# The built-in engine's Selections from {:?}, frozen while Retool was its oracle.",
        dat.file_name().unwrap()
    )
    .unwrap();
    writeln!(out, "# Profiles, one column each:").unwrap();
    for (p, (name, profile)) in profiles.iter().enumerate() {
        let mode = if profile.options.disable_1g1r {
            "1G1R off"
        } else {
            "1G1R on"
        };
        writeln!(out, "#   {:>2}: {name}, {mode}", p + 1).unwrap();
    }
    writeln!(
        out,
        "# Group cells: . absent, g present, u unresolved, d with a release without metadata. Release cells: . \
         absent, else w winner, u unresolved winner, a alternative, x exclusion, and the rule that decided it."
    )
    .unwrap();
    let file = dat.file_name().unwrap().to_str().unwrap();
    for (p, (name, profile)) in profiles.iter().enumerate() {
        if !profile.options.disable_1g1r && UNCHECKED.contains(&(name.as_str(), file)) {
            writeln!(
                out,
                "# Profile {} was never checked against Retool, which doesn't finish it in reasonable time.",
                p + 1
            )
            .unwrap();
        }
    }
    let all: BTreeSet<usize> = (0..n).collect();
    for (line, profiles) in &header {
        if *profiles == all {
            writeln!(out, "{line}").unwrap();
        } else {
            writeln!(out, "{line} [{}]", ranges(profiles)).unwrap();
        }
    }
    for (rule, number) in &rules {
        writeln!(out, "rule r{number}: {rule}").unwrap();
    }
    let cells = |get: &dyn Fn(usize) -> Option<String>, width: usize| {
        (0..n)
            .map(|p| format!("{:<width$}", get(p).unwrap_or_else(|| ".".into())))
            .collect::<Vec<_>>()
            .join(" ")
    };
    for (name, group) in &groups {
        if group.cells.len() == n && group.cells.values().all(|c| c == "g") {
            writeln!(out, "== {name}").unwrap();
        } else {
            let row = cells(&|p| group.cells.get(&p).cloned(), width);
            writeln!(out, "== {name}\n   {row} |").unwrap();
        }
        for (title, placed) in &group.releases {
            let row = cells(
                &|p| {
                    placed
                        .get(&p)
                        .map(|(kind, rule)| format!("{kind}{}", rules[rule]))
                },
                width,
            );
            writeln!(out, "   {row} | {title}").unwrap();
            for ((_, winner), profiles) in group
                .clone_of
                .range((title.clone(), String::new())..(format!("{title}\0"), String::new()))
            {
                writeln!(out, "     alternative to {winner} [{}]", ranges(profiles)).unwrap();
            }
        }
    }
    out
}

#[test]
fn the_built_in_engine_keeps_its_selections_on_retools_fixtures() {
    let profiles = profiles();
    let only = std::env::var("ONLY_DAT").ok();
    let dats: Vec<PathBuf> = fixture_dats()
        .into_iter()
        .filter(|d| match only.as_deref() {
            Some(only) => d.file_name().unwrap() == only,
            None => !cfg!(debug_assertions) || in_debug_run(d),
        })
        .collect();
    assert!(!dats.is_empty(), "no fixture DATs");
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let failures: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads.min(dats.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut failures = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(dat) = dats.get(i) else {
                            return failures;
                        };
                        let ran = run(dat, &profiles);
                        let golden = render(dat, &profiles, &ran);
                        let name = format!(
                            "fixtures/{}.txt",
                            dat.file_stem().unwrap().to_string_lossy()
                        );
                        if let Err(panic) =
                            std::panic::catch_unwind(|| support::golden(&name, &golden))
                        {
                            failures.push(
                                panic
                                    .downcast_ref::<String>()
                                    .cloned()
                                    .unwrap_or_else(|| name.clone()),
                            );
                        }
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    assert!(
        failures.is_empty(),
        "{} of {} DATs differ:\n{}",
        failures.len(),
        dats.len(),
        failures.join("\n")
    );
}
