//! The Selection engine's contract beyond conformance: a fail-safe tie is flagged, a Selection never fetches, and a
//! Profile the engine can't read is refused.

mod support;

use std::path::{Path, PathBuf};

use title_select_api::selection::{
    Category, Selection, SelectionOptions, SelectionProfile, SystemSettings,
};
use title_select_selection::{Engine, Error};

fn rom(name: &str) -> String {
    format!(
        "\t<game name=\"{name}\">\n\t\t<category>Games</category>\n\t\t<description>{name}</description>\n\
         \t\t<rom name=\"{name}.bin\" size=\"1000\" crc=\"00000000\" md5=\"00000000000000000000000000000000\" \
         sha1=\"0000000000000000000000000000000000000000\"/>\n\t</game>\n"
    )
}

fn dat(path: &Path, name: &str, titles: &[&str]) {
    let games: String = titles.iter().map(|title| rom(title)).collect();
    std::fs::write(
        path,
        format!(
            "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>{name}</name>\n\
             \t\t<description>{name}</description>\n\t\t<version>2024-01-01 00-00-00</version>\n\
             \t</header>\n{games}</datafile>\n"
        ),
    )
    .unwrap();
}

/// An empty data folder: no clone lists, metadata, MIAs or RetroAchievements.
fn data_dir(root: &Path) -> PathBuf {
    let data = root.join("data");
    for dir in ["clonelists", "metadata", "mias", "retroachievements"] {
        std::fs::create_dir_all(data.join(dir)).unwrap();
    }
    data
}

#[test]
fn a_fail_safe_tie_is_unresolved_and_keeps_retools_pick() {
    let work = tempfile::tempdir().unwrap();
    let data = data_dir(work.path());
    let name = "Title Select - Fail-safe tie";
    std::fs::write(
        data.join("clonelists").join(format!("{name}.json")),
        r#"{
  "description": {"name": "Title Select - Fail-safe tie", "lastUpdated": "2024-01-01 00:00:00", "minimumVersion": "2.4.8"},
  "variants": [
    {"group": "Tie Alpha", "titles": [{"searchTerm": "Tie Alpha"}, {"searchTerm": "Tie Beta"}]}
  ]
}
"#,
    )
    .unwrap();
    let path = work.path().join("tie.dat");
    dat(
        &path,
        name,
        &["Tie Alpha (Europe)", "Tie Beta (Europe)", "Solo (Europe)"],
    );

    let selection = support::engine()
        .with_data_dir(&data)
        .select(&path, &SelectionProfile::default(), &support::options())
        .unwrap();

    let tie = selection
        .groups
        .iter()
        .find(|g| g.group == "tie alpha")
        .unwrap_or_else(|| panic!("no tie group in {:#?}", selection.groups));
    assert!(tie.unresolved, "{tie:#?}");
    assert_eq!(tie.winners.len(), 1, "{tie:#?}");
    assert!(tie.winners[0].unresolved);
    assert_eq!(tie.winners[0].rule.reference.as_deref(), Some("REF0059"));
    assert_eq!(tie.alternatives.len(), 1, "{tie:#?}");

    let solo = selection.groups.iter().find(|g| g.group == "solo").unwrap();
    assert!(!solo.unresolved, "{solo:#?}");
    assert_eq!(selection.summary.unresolved, 1);
}

/// The engine has no network code: without clone lists or metadata it selects from the DAT alone, and the
/// Selection shows what it went without.
#[test]
fn a_selection_without_clone_lists_selects_without_them_and_says_so() {
    let work = tempfile::tempdir().unwrap();
    let empty = work.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - No data", &["Solo (Europe)"]);

    let selection = support::engine()
        .with_data_dir(&empty)
        .select(&path, &SelectionProfile::default(), &support::options())
        .unwrap();

    assert_eq!(selection.summary.winners, 1);
    assert_eq!(
        selection.coverage_drift.titles_without_metadata,
        ["Solo (Europe)"]
    );
}

#[test]
fn a_profile_with_names_the_engine_does_not_know_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - Profile", &["Solo (Europe)"]);
    let engine = support::engine();
    let refused =
        |profile: SelectionProfile| match engine.select(&path, &profile, &support::options()) {
            Err(Error::Profile(message)) => message,
            other => panic!("expected the Profile to be refused, got {other:?}"),
        };

    let profile = SelectionProfile {
        region_order: vec!["Atlantis".into()],
        ..Default::default()
    };
    assert!(refused(profile).contains("Atlantis"));

    let profile = SelectionProfile {
        language_order: vec!["Klingon".into()],
        ..Default::default()
    };
    assert!(refused(profile).contains("Klingon"));

    let mut profile = SelectionProfile::default();
    profile
        .systems
        .insert("../escape".into(), Default::default());
    assert!(refused(profile).contains("escape"));
}

#[test]
fn a_regular_expression_the_engine_cannot_compile_is_refused_rather_than_skipped() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("pair.dat");
    dat(
        &path,
        "Title Select - Patterns",
        &["Solo (Europe)", "Duo (Europe)"],
    );
    let engine = support::engine();
    let mut profile = SelectionProfile {
        post_filters: vec!["/[".into()],
        ..Default::default()
    };
    profile.systems.insert(
        "Title Select - Patterns".into(),
        title_select_api::selection::SystemSettings {
            overrides: title_select_api::selection::Overrides {
                exclude: vec!["</(>".into()],
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let checked = engine.check(&profile);
    let selected = engine.select(&path, &profile, &support::options());

    for refused in [checked.map(|_| ()), selected.map(|_| ())] {
        match refused {
            Err(Error::Patterns(problems)) => {
                assert_eq!(problems.len(), 2, "{problems:?}");
                assert!(problems[0].contains("\"/[\" in your global post filters"));
                assert!(problems[1].contains("\"</(>\" in your Title Select - Patterns exclude"));
            }
            other => panic!("expected the patterns to be refused, got {other:?}"),
        }
    }

    // Python's `re`, as Retool uses, has lookarounds.
    let profile = SelectionProfile {
        post_filters: vec!["/^(?!Duo)".into()],
        ..Default::default()
    };
    engine.check(&profile).unwrap();
    let outcome = engine.run(&path, &profile, &support::options()).unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
}

#[test]
fn a_deeply_nested_or_unbalanced_regular_expression_is_refused_rather_than_crashing_or_cut_short() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("pair.dat");
    dat(
        &path,
        "Title Select - Patterns",
        &["Test (Europe)", "Duo (Europe)"],
    );
    let engine = support::engine();
    let deep = format!("/{}Test{}", "(".repeat(20_000), ")".repeat(20_000));
    for (line, problem) in [
        (deep.as_str(), "nested"),
        ("/Test)NEVER", "unbalanced parenthesis"),
        ("/)", "unbalanced parenthesis"),
    ] {
        let profile = SelectionProfile {
            post_filters: vec![line.to_owned()],
            ..Default::default()
        };
        let checked = engine.check(&profile);
        let selected = engine.select(&path, &profile, &support::options());
        for refused in [checked.map(|_| ()), selected.map(|_| ())] {
            match refused {
                Err(Error::Patterns(problems)) => {
                    assert_eq!(problems.len(), 1, "{problems:?}");
                    assert!(problems[0].contains(problem), "{problems:?}");
                }
                other => panic!("expected {line:.20} to be refused, got {other:?}"),
            }
        }
    }
}

#[test]
fn clone_lists_of_another_commit_are_refused() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("commit"), "0\n").unwrap();
    match Engine::open(work.path()) {
        Err(Error::Install(message)) => assert!(message.contains("pins.toml"), "{message}"),
        other => panic!("expected the clone lists to be refused, got {other:?}"),
    }
}

fn feature(dat: &str) -> PathBuf {
    support::fixtures()
        .join("source/features")
        .join(format!("{dat}.dat"))
}

fn fixture_select(dat: &str, profile: &SelectionProfile) -> Selection {
    support::fixture_engine()
        .select(&feature(dat), profile, &support::options())
        .unwrap_or_else(|e| panic!("{dat}: {e}"))
}

fn excluded(selection: &Selection) -> Vec<&str> {
    selection
        .groups
        .iter()
        .flat_map(|g| &g.exclusions)
        .map(|e| e.title.as_str())
        .collect()
}

#[test]
fn an_empty_region_order_is_retools_default_order() {
    // The object's key order is the default region order.
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(rename = "defaultRegionOrder")]
        default_region_order: indexmap::IndexMap<String, serde_json::Value>,
    }
    let internal = support::fixtures().join("config/internal-config.json");
    let config: Config = serde_json::from_slice(&std::fs::read(internal).unwrap()).unwrap();
    let defaults: Vec<String> = config.default_region_order.into_keys().collect();
    assert_eq!(defaults[0], "USA");

    for dat in ["Retool - Languages", "Retool - World is USA-Europe-Japan"] {
        let explicit = SelectionProfile {
            region_order: defaults.clone(),
            ..Default::default()
        };
        assert_eq!(
            fixture_select(dat, &SelectionProfile::default()).groups,
            fixture_select(dat, &explicit).groups,
            "{dat}"
        );
    }
}

#[test]
fn a_systems_options_filter_languages_on_and_off() {
    let dat = "Retool - Languages";
    let english = || SelectionProfile {
        language_order: vec!["English".into()],
        ..Default::default()
    };
    let filtering = SelectionOptions {
        filter_languages: true,
        ..Default::default()
    };
    let system = |options: SelectionOptions| {
        [(
            dat.to_owned(),
            SystemSettings {
                options: Some(options),
                ..Default::default()
            },
        )]
        .into()
    };

    let global = fixture_select(
        dat,
        &SelectionProfile {
            options: filtering.clone(),
            ..english()
        },
    );
    assert!(!excluded(&global).is_empty(), "{global:#?}");

    let per_system = fixture_select(
        dat,
        &SelectionProfile {
            systems: system(filtering),
            ..english()
        },
    );
    assert_eq!(excluded(&per_system), excluded(&global));

    let switched_off = fixture_select(
        dat,
        &SelectionProfile {
            options: SelectionOptions {
                filter_languages: true,
                ..Default::default()
            },
            systems: system(SelectionOptions::default()),
            ..english()
        },
    );
    assert_eq!(excluded(&switched_off), Vec::<&str>::new());
}

#[test]
fn a_systems_empty_exclusions_restore_every_globally_excluded_category() {
    let dat = "Retool - Exclusions";
    let aftermarket = "Test Title 49 (USA) (Aftermarket)";
    let everything = SelectionProfile {
        exclude: Category::ALL
            .into_iter()
            .filter(|c| *c != Category::Games)
            .collect(),
        ..Default::default()
    };
    assert!(excluded(&fixture_select(dat, &everything)).contains(&aftermarket));

    let restored = fixture_select(
        dat,
        &SelectionProfile {
            systems: [(
                dat.to_owned(),
                SystemSettings {
                    exclude: Some(Default::default()),
                    ..Default::default()
                },
            )]
            .into(),
            ..everything
        },
    );
    assert_eq!(excluded(&restored), Vec::<&str>::new());
    assert_eq!(
        excluded(&fixture_select(dat, &SelectionProfile::default())),
        Vec::<&str>::new()
    );
}

#[test]
fn relative_install_data_work_and_dat_paths_work() {
    // Cargo runs tests in the crate's folder; every path below is relative to it.
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let workspace = crate_dir.join("../..").canonicalize().unwrap();
    let relative = |path: &Path| {
        Path::new("../..").join(
            path.canonicalize()
                .unwrap()
                .strip_prefix(&workspace)
                .unwrap(),
        )
    };
    let install = PathBuf::from(std::env::var_os("TITLE_SELECT_CLONE_LISTS").unwrap());
    let target = workspace.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let work = tempfile::tempdir_in(&target).unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - Relative", &["Solo (Europe)"]);
    let data = data_dir(work.path());
    std::fs::write(
        data.join("clonelists/Title Select - Relative.json"),
        r#"{"description": {"name": "Title Select - Relative", "lastUpdated": "2024-01-01 00:00:00", "minimumVersion": "2.4.8"}, "variants": []}"#,
    )
    .unwrap();

    let selection = Engine::open(&relative(&install))
        .unwrap()
        .with_data_dir(&relative(&data))
        .select(
            &relative(&path),
            &SelectionProfile::default(),
            &support::options(),
        )
        .unwrap();
    assert_eq!(selection.summary.winners, 1);
}

/// The acceptance run on a real DAT, which can't be committed:
/// `TITLE_SELECT_ATARI_DAT=<path> cargo test -p title-select-selection --test selection -- --ignored --nocapture`.
#[test]
#[ignore = "needs a real DAT in TITLE_SELECT_ATARI_DAT"]
fn a_real_dat_gets_a_rule_for_every_group() {
    let path = std::env::var_os("TITLE_SELECT_ATARI_DAT").expect("set TITLE_SELECT_ATARI_DAT");
    let started = std::time::Instant::now();
    let selection = support::engine()
        .select(
            Path::new(&path),
            &SelectionProfile::default(),
            &support::options(),
        )
        .unwrap();
    let elapsed = started.elapsed();

    for group in &selection.groups {
        for winner in &group.winners {
            assert!(
                winner.rule.reference.is_some(),
                "{}: {group:#?}",
                winner.title
            );
        }
        for alternative in &group.alternatives {
            assert!(!alternative.rule.message.is_empty(), "{group:#?}");
        }
    }
    let unresolved: Vec<&str> = selection
        .groups
        .iter()
        .filter(|g| g.unresolved)
        .map(|g| g.group.as_str())
        .collect();
    println!(
        "{}: {:?} in {elapsed:.2?}\nunresolved: {unresolved:?}\ncoverage drift: clone list {}, metadata {}, \
         {} unmatched clone-list terms, {} titles without metadata",
        selection.system,
        selection.summary,
        selection.coverage_drift.clone_list,
        selection.coverage_drift.metadata,
        selection.coverage_drift.unmatched_clone_list_terms.len(),
        selection.coverage_drift.titles_without_metadata.len(),
    );
}

#[test]
fn a_dats_system_is_named_as_retool_files_its_settings() {
    let work = tempfile::tempdir().unwrap();
    let no_intro = work.path().join("gba.dat");
    std::fs::write(
        &no_intro,
        "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>Nintendo - Game Boy Advance (Parent-Clone)</name>\n\
         \t\t<description>Nintendo - Game Boy Advance (Parent-Clone)</description>\n\
         \t\t<version>20240101-000000</version>\n\t\t<url>https://www.no-intro.org</url>\n\
         \t</header>\n</datafile>\n",
    )
    .unwrap();
    let plain = work.path().join("plain.dat");
    dat(&plain, "Title Select - Plain", &["Solo (Europe)"]);
    let unreadable = work.path().join("not-a.dat");
    std::fs::write(&unreadable, "not a DAT").unwrap();

    let names = support::engine()
        .systems(&[&no_intro, &plain, &unreadable])
        .unwrap();

    assert_eq!(
        names,
        [
            Some("Nintendo - Game Boy Advance (No-Intro)".to_owned()),
            Some("Title Select - Plain".to_owned()),
            None,
        ]
    );
}
