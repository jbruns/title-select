//! The Selection engine's contract beyond conformance: unpatched and patched Retool keep the same titles, a
//! fail-safe tie is flagged, a Selection never fetches, and a Profile Retool can't read is refused.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use title_select_api::selection::{
    Category, Selection, SelectionOptions, SelectionProfile, SystemSettings,
};
use title_select_selection::{Engine, Error};

/// Retool's synthetic DATs and settings, as Retool's own tests run them; `@name` is `tests/configs/user-config-name.yaml`. Retool writes the same output DATs
/// whether or not it is patched.
const NEUTRALITY_CASES: &[(&str, &str)] = &[
    ("Retool - Clone list filters.dat", "@regions-1"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-1"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-2"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-3"),
    ("Retool - Compilations.dat", "@regions-1"),
    ("Retool - Compilations.dat", "@regions-1 --compilations i"),
    ("Retool - Compilations.dat", "@regions-1 --compilations k"),
    ("Retool - Compilations.dat", "@regions-1 --compilations o"),
    (
        "Retool - Exclusions.dat",
        "@regions-1 --exclude aAbBcdDefkmMopPruv",
    ),
    ("Retool - Languages.dat", "@languages-1 -l"),
    ("Retool - Languages.dat", "@languages-2 -l"),
    ("Retool - Languages.dat", "@languages-3 -l"),
    ("Retool - Local names.dat", "@local-names -n"),
    ("Retool - Machine and disk.dat", "@regions-1"),
    ("Retool - MIAs.dat", "@regions-1 --labelmia"),
    ("Retool - MIAs.dat", "@regions-1 --exclude k"),
    ("Retool - Missing header data.dat", "@regions-1"),
    ("Retool - Missing title data.dat", "@regions-1"),
    ("Retool - No size or hash.dat", "@regions-1"),
    ("Retool - Numbered.dat", "@regions-1"),
    ("Retool - Post filters.dat", "@post-filters"),
    ("Retool - Prefer licensed.dat", "@regions-1 -y"),
    ("Retool - Prefer modern rips.dat", "@regions-1 -z"),
    ("Retool - Prefer oldest.dat", "@regions-1 -o"),
    ("Retool - Prefer oldest.dat", "@regions-1"),
    ("Retool - Prefer regions.dat", "@regions-1 -r"),
    ("Retool - RetroAchievements.dat", "@regions-1 --labelretro"),
    ("Retool - RetroAchievements.dat", "@regions-1 -c"),
    ("Retool - World is USA-Europe-Japan.dat", "@regions-1"),
];

fn run_retool(engine: &Engine, retool: &Path, dat: &str, args: &str, output: &Path) {
    let mut command = Command::new(engine.python());
    command
        .arg(retool.join("retool.py"))
        .arg(Path::new("tests/source/features").join(dat))
        .args(["--test", "--output"])
        .arg(output);
    for word in args.split_whitespace() {
        match word.strip_prefix('@') {
            Some(config) => command
                .arg("--config")
                .arg(format!("tests/configs/user-config-{config}.yaml")),
            None => command.arg(word),
        };
    }
    let result = command
        .current_dir(support::upstream())
        .env("PYTHONHASHSEED", "0")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {dat} {args}: {}\n{}",
        retool.display(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Every file under `dir`, relative to it, with its contents.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let contents = std::fs::read(&path).unwrap();
                found.push((path.strip_prefix(dir).unwrap().to_path_buf(), contents));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_patches_leave_retools_output_unchanged() {
    let engine = support::engine();
    let pristine = engine
        .pristine()
        .expect("the install has no pristine Retool: run tools/retool/setup");
    let patched = pristine.parent().unwrap().join("retool");
    let work = tempfile::tempdir().unwrap();
    let next = Mutex::new(0);
    let failures = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut next = next.lock().unwrap();
                        *next += 1;
                        *next - 1
                    };
                    let Some((dat, args)) = NEUTRALITY_CASES.get(index) else {
                        break;
                    };
                    let outputs = ["pristine", "patched"]
                        .map(|name| work.path().join(index.to_string()).join(name));
                    run_retool(&engine, &pristine, dat, args, &outputs[0]);
                    run_retool(&engine, &patched, dat, args, &outputs[1]);
                    let [before, after] = outputs.map(|output| files(&output));
                    if before.is_empty() || before != after {
                        failures.lock().unwrap().push(format!("{dat} {args}"));
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "the patched Retool writes other output than the pristine one for: {failures:#?}"
    );
}

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
        .select(
            &path,
            &SelectionProfile::default(),
            &support::options(work.path()),
        )
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

#[test]
fn a_selection_without_clone_lists_fails_instead_of_fetching() {
    let work = tempfile::tempdir().unwrap();
    let empty = work.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - No data", &["Solo (Europe)"]);

    let error = support::engine()
        .with_data_dir(&empty)
        .select(
            &path,
            &SelectionProfile::default(),
            &support::options(work.path()),
        )
        .unwrap_err();

    match error {
        Error::Retool { output, .. } => assert!(output.contains("clonelists"), "{output}"),
        other => panic!("expected Retool to fail, got {other}"),
    }
}

#[test]
fn a_profile_with_names_retool_does_not_know_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - Profile", &["Solo (Europe)"]);
    let engine = support::engine();
    let refused = |profile: SelectionProfile| match engine.select(
        &path,
        &profile,
        &support::options(work.path()),
    ) {
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
    let selected = engine.select(&path, &profile, &support::options(work.path()));

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
    let outcome = engine
        .run(&path, &profile, &support::options(work.path()))
        .unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
}

#[test]
fn an_install_built_from_other_pins_is_refused() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("pins.json"),
        r#"{"retool_commit": "0", "patch_set_sha256": "0", "clone_list_commit": "0"}"#,
    )
    .unwrap();
    match Engine::open(work.path()) {
        Err(Error::Install(message)) => assert!(message.contains("pins.toml"), "{message}"),
        other => panic!("expected the install to be refused, got {other:?}"),
    }
}

fn feature(dat: &str) -> PathBuf {
    support::upstream()
        .join("tests/source/features")
        .join(format!("{dat}.dat"))
}

fn fixture_select(dat: &str, profile: &SelectionProfile) -> Selection {
    let work = tempfile::tempdir().unwrap();
    support::fixture_engine()
        .select(&feature(dat), profile, &support::options(work.path()))
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
    let engine = support::engine();
    let internal = engine
        .python()
        .parent()
        .unwrap()
        .join("../../retool/config/internal-config.json");
    // Python's json keeps the object's key order, which is Retool's default region order.
    let output = Command::new(engine.python())
        .args([
            "-c",
            "import json, sys; print(json.dumps(list(json.load(open(sys.argv[1]))['defaultRegionOrder'])))",
        ])
        .arg(&internal)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let defaults: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
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
    let install = PathBuf::from(std::env::var_os("TITLE_SELECT_RETOOL").unwrap());
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
            &support::options(&relative(work.path())),
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
    let work = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let selection = support::engine()
        .select(
            Path::new(&path),
            &SelectionProfile::default(),
            &support::options(work.path()),
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
        .systems(&[&no_intro, &plain, &unreadable], &work.path().join("work"))
        .unwrap();

    assert_eq!(
        names,
        [
            Some("Nintendo - Game Boy Advance (No-Intro)".to_owned()),
            Some("Title Select - Plain".to_owned()),
            None,
        ]
    );
    assert!(
        std::fs::read_dir(work.path().join("work"))
            .unwrap()
            .next()
            .is_none(),
        "the scratch folder is removed"
    );
}
