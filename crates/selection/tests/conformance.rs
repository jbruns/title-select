//! Conformance: Retool's own synthetic DATs and configs, run through the Selection engine. Each case checks the
//! Selection against Retool's committed output for the same DAT and config, then compares the Selection and the
//! trace with this crate's goldens (`tests/goldens/`), which `TITLE_SELECT_REGENERATE_GOLDENS=1` rewrites.

mod support;

use std::collections::BTreeMap;

use title_select_api::selection::{Category, Selection, SelectionEngine, SelectionProfile};
use title_select_selection::RunOptions;

struct Case {
    name: &'static str,
    /// Under Retool's `tests/source/`.
    dat: &'static str,
    /// Under Retool's `tests/configs/`.
    config: &'static str,
    adjust: fn(&mut SelectionProfile),
    /// The DAT's header name, when it must differ from the fixture's so that Retool finds a clone list on a
    /// case-sensitive file system too.
    system: Option<&'static str>,
    /// Retool's output for the same DAT and settings, under its `tests/goldens/`.
    upstream_golden: Option<&'static str>,
    /// Whether this crate's goldens cover the case. Large DATs are checked against Retool's output only, to keep
    /// the goldens reviewable.
    goldens: bool,
}

fn none(_: &mut SelectionProfile) {}

const CASES: &[Case] = &[
    Case {
        name: "region-precedence",
        dat: "features/Retool - World is USA-Europe-Japan.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some(
            "features/world-is-usa-europe-japan/Retool - World is USA-Europe-Japan (-x).dat",
        ),
        goldens: true,
    },
    Case {
        name: "region-precedence-japan-first",
        dat: "systems/Sega - Game Gear.dat",
        config: "user-config-regions-4.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some("japan-world-asia-usa-remainder/Sega - Game Gear (-x).dat"),
        goldens: false,
    },
    Case {
        name: "prefer-regions",
        dat: "features/Retool - Prefer regions.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| p.options.prefer_regions = true,
        system: None,
        upstream_golden: Some("features/prefer-regions-1/Retool - Prefer regions (-rx).dat"),
        goldens: true,
    },
    Case {
        name: "language-precedence",
        dat: "features/Retool - Languages.dat",
        config: "user-config-languages-1.yaml",
        adjust: |p| p.options.filter_languages = true,
        system: None,
        upstream_golden: Some("features/languages-1/Retool - Languages (-lx).dat"),
        goldens: true,
    },
    Case {
        name: "language-precedence-2",
        dat: "features/Retool - Languages.dat",
        config: "user-config-languages-2.yaml",
        adjust: |p| p.options.filter_languages = true,
        system: None,
        upstream_golden: Some("features/languages-2/Retool - Languages (-lx).dat"),
        goldens: true,
    },
    Case {
        name: "revisions-newest",
        dat: "features/Retool - Prefer oldest.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some("features/prefer-oldest-2/Retool - Prefer oldest (-x).dat"),
        goldens: true,
    },
    Case {
        name: "revisions-oldest",
        dat: "features/Retool - Prefer oldest.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| p.options.prefer_oldest = true,
        system: None,
        upstream_golden: Some("features/prefer-oldest-1/Retool - Prefer oldest (-ox).dat"),
        goldens: true,
    },
    Case {
        name: "clone-list-groups",
        dat: "features/Retool - Clone list filters.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some("features/clone-list-filters-1/Retool - Clone list filters (-x).dat"),
        goldens: true,
    },
    Case {
        name: "clone-list-filters",
        dat: "features/Retool - Clone list filters.dat",
        config: "user-config-clone-list-filters-1.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some("features/clone-list-filters-2/Retool - Clone list filters (-x).dat"),
        goldens: true,
    },
    Case {
        name: "category-exclusions",
        dat: "features/Retool - Exclusions.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| {
            p.exclude = Category::ALL
                .into_iter()
                .filter(|c| *c != Category::Games)
                .collect()
        },
        system: None,
        upstream_golden: Some(
            "features/exclusions/Retool - Exclusions (-x) [-aAbBcdDefkmMopPruv].dat",
        ),
        goldens: true,
    },
    Case {
        name: "overrides-upstream",
        dat: "systems/Microsoft - Xbox - Datfile.dat",
        config: "user-config-overrides.yaml",
        adjust: none,
        system: None,
        upstream_golden: Some("features/overrides/Microsoft - Xbox.dat"),
        goldens: false,
    },
    Case {
        name: "overrides",
        dat: "features/Retool - Clone list filters.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| {
            p.region_order = vec!["USA".into()];
            p.overrides.exclude = vec![
                "|Test Title 2 (USA)".into(),
                "/^Test Title 4.*Pirate".into(),
            ];
            p.overrides.include = vec!["|Test Title 1 (Germany)".into()];
        },
        system: None,
        upstream_golden: None,
        goldens: true,
    },
    Case {
        name: "post-filters",
        dat: "features/Retool - Post filters.dat",
        config: "user-config-post-filters.yaml",
        adjust: none,
        system: Some("Retool - Post Filters"),
        upstream_golden: Some("features/post-filters/Retool - Post filters (-x).dat"),
        goldens: true,
    },
];

/// Every title the Selection keeps, with the winner it is an alternative to, as Retool's parent/clone output
/// lists them. Without `cloneof` attributes, only the winners.
fn kept(selection: &Selection, with_clones: bool) -> Vec<(String, Option<String>)> {
    let mut kept: Vec<(String, Option<String>)> = Vec::new();
    for group in &selection.groups {
        kept.extend(group.winners.iter().map(|w| (w.title.clone(), None)));
        if with_clones {
            kept.extend(
                group
                    .alternatives
                    .iter()
                    .map(|a| (a.title.clone(), Some(a.clone_of.clone()))),
            );
        }
    }
    kept.sort();
    kept
}

fn run(case: &Case) -> Selection {
    run_with(case, SelectionEngine::Bundled)
}

/// Runs the case with `engine`. Only the bundled engine's run rewrites the goldens.
fn run_with(case: &Case, engine: SelectionEngine) -> Selection {
    let work = tempfile::tempdir().unwrap();
    let mut profile = support::profile_from_upstream_config(case.config);
    (case.adjust)(&mut profile);
    let tests = support::upstream().join("tests");
    let mut dat = tests.join("source").join(case.dat);
    if let Some(system) = case.system {
        let text = std::fs::read_to_string(&dat).unwrap();
        let (start, rest) = text.split_once("<name>").unwrap();
        let (_, end) = rest.split_once("</name>").unwrap();
        dat = work.path().join("input.dat");
        std::fs::write(&dat, format!("{start}<name>{system}</name>{end}")).unwrap();
    }
    let options = RunOptions {
        engine,
        ..support::options(work.path())
    };
    let outcome = support::fixture_engine()
        .run(&dat, &profile, &options)
        .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    let selection = outcome.selection;

    if let Some(upstream_golden) = case.upstream_golden {
        let upstream = support::parent_clone(&tests.join("goldens").join(upstream_golden));
        let with_clones = upstream.iter().any(|(_, parent)| parent.is_some());
        assert_eq!(
            kept(&selection, with_clones),
            upstream,
            "{}: the Selection keeps other titles than Retool's own golden",
            case.name
        );
    }
    for group in &selection.groups {
        for winner in &group.winners {
            assert!(
                winner.rule.reference.is_some(),
                "{}: {} has no rule",
                case.name,
                winner.title
            );
        }
    }

    if case.goldens {
        // The pins change with every patch and clone-list update; the goldens describe the Selection itself.
        let mut golden = selection.clone();
        golden.pins = Default::default();
        let mut json = serde_json::to_string_pretty(&golden).unwrap();
        json.push('\n');
        let trace = title_select_selection::stable_trace(&outcome.trace).unwrap();
        let selection_golden = format!("{}.selection.json", case.name);
        if engine == SelectionEngine::Bundled || !support::regenerating() {
            support::golden(&selection_golden, &json);
            support::golden(&format!("{}.trace.jsonl", case.name), &trace);
        }
    }
    selection
}

#[test]
fn conformance_cases_match_retools_output_and_the_goldens() {
    run_cases(SelectionEngine::Bundled);
}

#[test]
fn the_built_in_engine_matches_retools_output_and_the_goldens() {
    run_cases(SelectionEngine::Builtin);
}

fn run_cases(engine: SelectionEngine) {
    let failures: BTreeMap<&str, String> = std::thread::scope(|scope| {
        let handles: Vec<_> = CASES
            .iter()
            .map(|case| {
                (
                    case.name,
                    scope.spawn(move || std::panic::catch_unwind(|| drop(run_with(case, engine)))),
                )
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|(name, handle)| {
                let result = handle.join().unwrap();
                result.err().map(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    (name, message)
                })
            })
            .collect()
    });
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn overrides_exclude_and_include_titles_by_name_regex_and_full_name() {
    let case = CASES.iter().find(|c| c.name == "overrides").unwrap();
    let selection = run(case);
    let group = |name: &str| selection.groups.iter().find(|g| g.group == name).unwrap();
    let excluded = |group: &str, title: &str| {
        let row = group_rows(&selection, group);
        row.exclusions.iter().find(|e| e.title == title).cloned()
    };
    assert!(excluded("test title 2", "Test Title 2 (USA)").is_some());
    assert!(excluded("test title 4 (pirate)", "Test Title 4 (USA) (Pirate)").is_some());
    let included = group("test title 1")
        .winners
        .iter()
        .find(|w| w.title == "Test Title 1 (Germany)")
        .expect("the include override keeps a title the region order removes");
    assert_eq!(included.rule.stage, "Includes");
}

fn group_rows<'s>(
    selection: &'s Selection,
    name: &str,
) -> &'s title_select_api::selection::SelectionGroup {
    selection
        .groups
        .iter()
        .find(|g| g.group == name)
        .unwrap_or_else(|| panic!("no group {name}"))
}
