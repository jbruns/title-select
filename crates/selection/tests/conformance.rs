//! Conformance: Retool's own synthetic DATs and configs, run through the Selection engine. Each case compares the
//! Selection and the trace with this crate's goldens (`tests/goldens/`), which `TITLE_SELECT_REGENERATE_GOLDENS=1`
//! rewrites. The goldens were made by the bundled Retool, and checked against its own output, until the cutover
//! (ADR 0015); `tests/fixtures.rs` covers every fixture DAT.

mod support;

use std::collections::BTreeMap;

use title_select_api::selection::{Category, Selection, SelectionProfile};

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
}

fn none(_: &mut SelectionProfile) {}

const CASES: &[Case] = &[
    Case {
        name: "region-precedence",
        dat: "features/Retool - World is USA-Europe-Japan.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
    },
    Case {
        name: "prefer-regions",
        dat: "features/Retool - Prefer regions.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| p.options.prefer_regions = true,
        system: None,
    },
    Case {
        name: "language-precedence",
        dat: "features/Retool - Languages.dat",
        config: "user-config-languages-1.yaml",
        adjust: |p| p.options.filter_languages = true,
        system: None,
    },
    Case {
        name: "language-precedence-2",
        dat: "features/Retool - Languages.dat",
        config: "user-config-languages-2.yaml",
        adjust: |p| p.options.filter_languages = true,
        system: None,
    },
    Case {
        name: "revisions-newest",
        dat: "features/Retool - Prefer oldest.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
    },
    Case {
        name: "revisions-oldest",
        dat: "features/Retool - Prefer oldest.dat",
        config: "user-config-regions-1.yaml",
        adjust: |p| p.options.prefer_oldest = true,
        system: None,
    },
    Case {
        name: "clone-list-groups",
        dat: "features/Retool - Clone list filters.dat",
        config: "user-config-regions-1.yaml",
        adjust: none,
        system: None,
    },
    Case {
        name: "clone-list-filters",
        dat: "features/Retool - Clone list filters.dat",
        config: "user-config-clone-list-filters-1.yaml",
        adjust: none,
        system: None,
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
    },
    Case {
        name: "post-filters",
        dat: "features/Retool - Post filters.dat",
        config: "user-config-post-filters.yaml",
        adjust: none,
        system: Some("Retool - Post Filters"),
    },
];

fn run(case: &Case) -> Selection {
    let work = tempfile::tempdir().unwrap();
    let mut profile = support::profile_from_upstream_config(case.config);
    (case.adjust)(&mut profile);
    let mut dat = support::fixtures().join("source").join(case.dat);
    if let Some(system) = case.system {
        let text = std::fs::read_to_string(&dat).unwrap();
        let (start, rest) = text.split_once("<name>").unwrap();
        let (_, end) = rest.split_once("</name>").unwrap();
        dat = work.path().join("input.dat");
        std::fs::write(&dat, format!("{start}<name>{system}</name>{end}")).unwrap();
    }
    let outcome = support::fixture_engine()
        .run(&dat, &profile, &support::options())
        .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    let selection = outcome.selection;

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

    // The pins change with every clone-list update and release; the goldens describe the Selection itself.
    let mut golden = selection.clone();
    golden.pins = Default::default();
    let mut json = serde_json::to_string_pretty(&golden).unwrap();
    json.push('\n');
    let trace = title_select_selection::stable_trace(&outcome.trace).unwrap();
    support::golden(&format!("{}.selection.json", case.name), &json);
    support::golden(&format!("{}.trace.jsonl", case.name), &trace);
    selection
}

#[test]
fn conformance_cases_match_the_goldens() {
    let failures: BTreeMap<&str, String> = std::thread::scope(|scope| {
        let handles: Vec<_> = CASES
            .iter()
            .map(|case| {
                (
                    case.name,
                    scope.spawn(move || std::panic::catch_unwind(|| drop(run(case)))),
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
