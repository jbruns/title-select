//! The built-in engine against the bundled one: both run over the same DATs with the same Selection Profiles, and
//! must leave the same trace, Selection and warnings. By default this checks a subset that runs every stage (see
//! `in_run`); `CONFORMANCE_FULL=1` checks every fixture DAT.

mod support;

use std::path::{Path, PathBuf};

use title_select_api::selection::{Category, SelectionProfile};

/// Every DAT under Retool's `tests/source/`.
fn fixture_dats() -> Vec<PathBuf> {
    let mut dats = Vec::new();
    let mut stack = vec![support::upstream().join("tests/source")];
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

/// System DATs the pull-request run checks beside every feature DAT: mid-sized ones with clone lists and metadata, so
/// clone-list, metadata and 1G1R stages run over real-world shapes. `CONFORMANCE_FULL=1` checks every DAT instead,
/// as the scheduled conformance workflow does.
const REPRESENTATIVE_SYSTEMS: &[&str] = &[
    "Nintendo - Wii U - Datfile.dat",
    "Nintendo - Nintendo 64 (BigEndian) (Private).dat",
];

/// Whether a run checks this DAT.
fn in_run(dat: &Path) -> bool {
    std::env::var_os("CONFORMANCE_FULL").is_some()
        || dat.parent().is_some_and(|p| p.ends_with("features"))
        || REPRESENTATIVE_SYSTEMS.contains(&dat.file_name().unwrap().to_str().unwrap())
}

/// Set orders the bundled engine is retried with, to tell order-sensitive differences from real ones. A title in
/// many groups (a superset, say) needs many: which copy of it the run meets first decides the group it is reported in.
const SET_ORDERS: u32 = 32;

/// Cases the bundled engine itself doesn't finish in reasonable time or memory: its compilation step traces every
/// combination of a contention group's compilations, which is exponential in the group's size.
const INTRACTABLE: &[(&str, &str)] = &[(
    "user-config-overrides.yaml",
    "Nintendo - Nintendo Entertainment System (Headered) (Private).dat",
)];

/// Totals over the cases run.
#[derive(Default)]
struct Totals {
    cases: usize,
    groups: usize,
    units: usize,
    differing: usize,
    order_sensitive: usize,
}

/// Where the engines' runs differ other than by set order, empty when they conform.
fn check(dat: &Path, profile: &SelectionProfile, totals: &std::sync::Mutex<Totals>) -> Vec<String> {
    let engine = support::fixture_engine();
    let work = tempfile::tempdir().unwrap();
    let options = support::options(work.path());
    let conformance = engine
        .conformance(dat, profile, &options, SET_ORDERS)
        .unwrap();
    {
        let mut totals = totals.lock().unwrap();
        totals.cases += 1;
        totals.groups += conformance.comparison.as_ref().map_or(0, |c| c.groups);
        totals.units += conformance.units;
        totals.differing += conformance.differing.len();
        totals.order_sensitive += conformance.order_sensitive().len();
    }
    let mut out = Vec::new();
    if conformance.conforms() {
        return out;
    }
    out.push(format!(
        "{:?}; bundled: {:?}; built-in: {:?}",
        conformance.verdict(),
        conformance.failures[0],
        conformance.failures[1]
    ));
    for unit in conformance.unexplained() {
        let [a, b] = &conformance.differing[unit];
        out.push(format!("unit {unit}:"));
        let (la, lb): (Vec<&str>, Vec<&str>) = (
            a.as_deref().unwrap_or_default().lines().collect(),
            b.as_deref().unwrap_or_default().lines().collect(),
        );
        out.extend(
            la.iter()
                .filter(|l| !lb.contains(l))
                .map(|l| format!("- {l}")),
        );
        out.extend(
            lb.iter()
                .filter(|l| !la.contains(l))
                .map(|l| format!("+ {l}")),
        );
    }
    out
}

fn report(failures: Vec<(String, Vec<String>)>) {
    if failures.is_empty() {
        return;
    }
    let mut text = String::new();
    for (name, diffs) in &failures {
        text.push_str(&format!("\n== {name}\n"));
        for d in diffs.iter().take(40) {
            text.push_str(&format!("{d}\n"));
        }
    }
    panic!("{} case(s) differ:{text}", failures.len());
}

fn run_all(profiles: &[(&str, SelectionProfile)]) {
    let dats = fixture_dats();
    let totals = std::sync::Mutex::new(Totals::default());
    let mut failures = Vec::new();
    let only: Option<usize> = std::env::var("ONLY_PROFILE")
        .ok()
        .map(|p| p.parse().unwrap());
    for (index, (name, profile)) in profiles.iter().enumerate() {
        if only.is_some_and(|o| o != index) {
            continue;
        }
        let totals = &totals;
        failures.extend(std::thread::scope(|scope| {
            let handles: Vec<_> = dats
                .iter()
                .filter(|dat| in_run(dat))
                .filter(|dat| {
                    let file = dat.file_name().unwrap().to_str().unwrap();
                    profile.options.disable_1g1r || !INTRACTABLE.contains(&(name, file))
                })
                .map(|dat| (dat, scope.spawn(move || check(dat, profile, totals))))
                .collect();
            handles
                .into_iter()
                .filter_map(|(dat, h)| {
                    let d = h.join().unwrap();
                    let file = dat.file_name().unwrap().to_string_lossy();
                    (!d.is_empty()).then(|| (format!("{name}: {file}"), d))
                })
                .collect::<Vec<_>>()
        }));
    }
    let totals = totals.into_inner().unwrap();
    eprintln!(
        "{} cases, {} groups, {} units compared: {} differ, {} of them order-sensitive",
        totals.cases, totals.groups, totals.units, totals.differing, totals.order_sensitive
    );
    report(failures);
}

fn profiles(disable_1g1r: bool) -> Vec<(&'static str, SelectionProfile)> {
    let mut out = Vec::new();
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
        out.push((config, profile));
    }
    let mut profile = support::profile_from_upstream_config("user-config-regions-1.yaml");
    profile.options.disable_1g1r = disable_1g1r;
    profile.exclude = Category::ALL
        .into_iter()
        .filter(|c| *c != Category::Games)
        .collect();
    out.push(("games only", profile));
    out
}

#[test]
fn early_stages_match_on_retools_fixtures() {
    run_all(&profiles(true));
}

#[test]
fn parent_selection_matches_on_retools_fixtures() {
    run_all(&profiles(false));
}

/// Debugging aid: `DUMP_DAT=<path> DUMP_PROFILE=<index> DUMP_TO=<dir> cargo test --test builtin dump -- --ignored`
/// writes both engines' traces for one case.
#[test]
#[ignore]
fn dump() {
    let dat = PathBuf::from(std::env::var("DUMP_DAT").unwrap());
    let index: usize = std::env::var("DUMP_PROFILE").unwrap().parse().unwrap();
    let to = PathBuf::from(std::env::var("DUMP_TO").unwrap());
    let profile = &profiles(false)[index].1;
    let engine = support::fixture_engine();
    let work = tempfile::tempdir().unwrap();
    let options = support::options(work.path());
    let options = title_select_selection::RunOptions {
        set_order: std::env::var("DUMP_SET_ORDER")
            .ok()
            .map(|s| s.parse().unwrap()),
        ..options
    };
    std::fs::write(
        to.join("bundled.jsonl"),
        engine.run(&dat, profile, &options).unwrap().trace,
    )
    .unwrap();
    std::fs::write(
        to.join("builtin.jsonl"),
        engine.run_builtin(&dat, profile, &options).unwrap().trace,
    )
    .unwrap();
}
