//! The built-in engine against the bundled one: both run over the same DATs with the same Selection Profiles, and
//! must leave the same trace, Selection and warnings.

mod support;

use std::path::{Path, PathBuf};

use title_select_api::selection::{Category, SelectionProfile};
use title_select_selection::stable_trace;

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

/// The differences between the two engines' runs, empty when they agree.
fn differences(dat: &Path, profile: &SelectionProfile) -> Vec<String> {
    let engine = support::fixture_engine();
    let work = tempfile::tempdir().unwrap();
    let options = support::options(work.path());
    let bundled = engine.run(dat, profile, &options);
    let builtin = engine.run_builtin(dat, profile, &options);
    let mut out = Vec::new();
    match (bundled, builtin) {
        (Ok(a), Ok(b)) => {
            eprintln!(
                "{}: {} trace lines, {} groups",
                dat.display(),
                a.trace.lines().count(),
                a.selection.groups.len()
            );
            let (ta, tb) = (
                stable_trace(&a.trace).unwrap(),
                stable_trace(&b.trace).unwrap(),
            );
            if ta != tb {
                let la: Vec<&str> = ta.lines().collect();
                let lb: Vec<&str> = tb.lines().collect();
                for l in &la {
                    if !lb.contains(l) {
                        out.push(format!("- {l}"));
                    }
                }
                for l in &lb {
                    if !la.contains(l) {
                        out.push(format!("+ {l}"));
                    }
                }
            }
            if out.is_empty() {
                let mut la: Vec<&str> = a.trace.lines().collect();
                let mut lb: Vec<&str> = b.trace.lines().collect();
                la.sort();
                lb.sort();
                if la != lb {
                    let i = la
                        .iter()
                        .zip(&lb)
                        .position(|(x, y)| x != y)
                        .unwrap_or(la.len().min(lb.len()));
                    out.push(format!(
                        "sorted raw traces differ at line {i}:\n- {}\n+ {}",
                        la.get(i).unwrap_or(&""),
                        lb.get(i).unwrap_or(&"")
                    ));
                }
            }
            if a.warnings != b.warnings {
                out.push(format!("warnings:\n  {:?}\n  {:?}", a.warnings, b.warnings));
            }
            let (sa, sb) = (
                serde_json::to_string_pretty(&a.selection).unwrap(),
                serde_json::to_string_pretty(&b.selection).unwrap(),
            );
            if sa != sb && out.is_empty() {
                out.push("the Selections differ".into());
            }
        }
        (Err(a), Err(b)) => {
            eprintln!("{}: both failed: {a} / {b}", dat.display());
            if !matches!(a, title_select_selection::Error::Retool { .. }) {
                out.push(format!("bundled failed: {a}; built-in failed: {b}"));
            }
        }
        (a, b) => out.push(format!(
            "bundled: {:?}; built-in: {:?}",
            a.err().map(|e| e.to_string()),
            b.err().map(|e| e.to_string())
        )),
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

fn run_all(profile: &SelectionProfile) {
    let dats = fixture_dats();
    let failures: Vec<(String, Vec<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = dats
            .iter()
            .map(|dat| (dat, scope.spawn(move || differences(dat, profile))))
            .collect();
        handles
            .into_iter()
            .filter_map(|(dat, h)| {
                let d = h.join().unwrap();
                (!d.is_empty()).then(|| (dat.display().to_string(), d))
            })
            .collect()
    });
    report(failures);
}

fn profiles(disable_1g1r: bool) -> Vec<SelectionProfile> {
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
        out.push(profile);
    }
    let mut profile = support::profile_from_upstream_config("user-config-regions-1.yaml");
    profile.options.disable_1g1r = disable_1g1r;
    profile.exclude = Category::ALL
        .into_iter()
        .filter(|c| *c != Category::Games)
        .collect();
    out.push(profile);
    out
}

#[test]
fn early_stages_match_on_retools_fixtures() {
    for profile in profiles(true) {
        run_all(&profile);
    }
}
