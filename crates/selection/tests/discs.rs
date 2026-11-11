//! Multi-disc Games: the disc-name normalization is Retool's own, and the check after Selection flags a group
//! whose discs don't come whole from one release, without changing what the engine picks.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use title_select_api::discs::{self, DISC_RENAME, normalize};
use title_select_api::selection::{ChoiceStatus, Selection, SelectionProfile};

#[test]
fn the_disc_rename_table_is_the_pinned_configs() {
    let path = Path::new(
        &std::env::var_os("TITLE_SELECT_CLONE_LISTS").expect("set in .cargo/config.toml"),
    )
    .join("config/internal-config.json");
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(rename = "discRename")]
        disc_rename: indexmap::IndexMap<String, String>,
    }
    let config: Config = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nthe Selection tests need the clone lists: run tools/clone-lists/fetch",
            path.display()
        )
    }))
    .unwrap();
    let pinned: Vec<(String, String)> = config.disc_rename.into_iter().collect();
    let ours: Vec<(String, String)> = DISC_RENAME
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(ours, pinned);
}

/// Retool's `TitleTools.normalize_discs` on names built from every key of the table, frozen in `tests/oracle/`.
#[test]
fn disc_names_normalize_exactly_as_retools_title_tools_do() {
    let mut differences = Vec::new();
    for line in include_str!("oracle/disc-names.jsonl").lines() {
        let (name, retools): (String, String) = serde_json::from_str(line).unwrap();
        if normalize(&name) != retools {
            differences.push(format!(
                "{name:?}: {:?}, Retool {retools:?}",
                normalize(&name)
            ));
        }
    }
    assert!(differences.is_empty(), "{differences:#?}");
}

fn game(name: &str) -> String {
    format!(
        "\t<game name=\"{name}\">\n\t\t<category>Games</category>\n\t\t<description>{name}</description>\n\
         \t\t<rom name=\"{name}.bin\" size=\"1000\" crc=\"00000000\" md5=\"00000000000000000000000000000000\" \
         sha1=\"0000000000000000000000000000000000000000\"/>\n\t</game>\n"
    )
}

/// A synthetic DAT where only Europe has Disc 2 of `Saga`, beside a single-disc Game.
fn only_europe_has_disc_2(dir: &Path) -> PathBuf {
    write_dat(
        dir,
        "discs.dat",
        &[
            "Saga (USA) (Disc 1)",
            "Saga (Europe) (Disc 1)",
            "Saga (Europe) (Disc 2)",
            "Solo (USA)",
        ],
    )
}

fn write_dat(dir: &Path, file: &str, games: &[&str]) -> PathBuf {
    let name = "Title Select - Discs";
    let games: String = games.iter().map(|g| game(g)).collect();
    let path = dir.join(file);
    std::fs::write(
        &path,
        format!(
            "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>{name}</name>\n\
             \t\t<description>{name}</description>\n\t\t<version>2024-01-01 00-00-00</version>\n\
             \t</header>\n{games}</datafile>\n"
        ),
    )
    .unwrap();
    for sub in ["clonelists", "metadata", "mias", "retroachievements"] {
        std::fs::create_dir_all(dir.join("data").join(sub)).unwrap();
    }
    path
}

fn select(dir: &Path, dat: &Path, profile: &SelectionProfile) -> Selection {
    let engine = support::engine().with_data_dir(&dir.join("data"));
    engine.select(dat, profile, &support::options()).unwrap()
}

fn titles<'a>(titles: impl IntoIterator<Item = &'a String>) -> BTreeSet<&'a str> {
    titles.into_iter().map(String::as_str).collect()
}

#[test]
fn only_europe_having_disc_2_leaves_the_group_unresolved_until_your_choice_settles_it() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());

    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let saga = selection.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    // The engine's pick is unchanged: each disc is chosen by its own name, so Disc 2 can only come from Europe.
    assert_eq!(
        titles(saga.winners.iter().map(|w| &w.title)),
        BTreeSet::from(["Saga (USA) (Disc 1)", "Saga (Europe) (Disc 2)"])
    );
    assert!(saga.winners.iter().all(|w| !w.unresolved));
    let solo = selection.groups.iter().find(|g| g.group == "solo").unwrap();
    assert!(!solo.unresolved);
    assert_eq!(selection.summary.unresolved, 1);

    let europe: BTreeSet<String> = ["Saga (Europe) (Disc 1)", "Saga (Europe) (Disc 2)"]
        .map(String::from)
        .into();
    let mut profile = SelectionProfile::default();
    profile.set_choice(
        &selection.system,
        "saga",
        europe.clone(),
        &saga.picks(),
        saga.engine_unresolved(),
    );
    let chosen = select(work.path(), &dat, &profile);
    let saga = chosen.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(!saga.unresolved && !saga.needs_review(), "{saga:#?}");
    assert_eq!(saga.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
    assert_eq!(
        titles(saga.winners.iter().map(|w| &w.title)),
        titles(&europe)
    );
    assert_eq!(chosen.summary.needs_review(), 0);

    // Confirming the engine's mixed pick is an explicit choice too, and it is kept.
    let mut profile = SelectionProfile::default();
    let picks = selection
        .groups
        .iter()
        .find(|g| g.group == "saga")
        .unwrap()
        .picks();
    profile.set_choice(&selection.system, "saga", picks.clone(), &picks, true);
    assert_eq!(
        profile.choices[&selection.system],
        BTreeMap::from([("saga".to_owned(), picks)])
    );
    let confirmed = select(work.path(), &dat, &profile);
    let saga = confirmed.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(!saga.unresolved, "{saga:#?}");
    assert_eq!(saga.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
    assert_eq!(discs::unresolved_reason(saga), None);
}

fn europe() -> BTreeSet<String> {
    ["Saga (Europe) (Disc 1)", "Saga (Europe) (Disc 2)"]
        .map(String::from)
        .into()
}

fn choosing(selection: &Selection, checked: BTreeSet<String>) -> SelectionProfile {
    let saga = selection.groups.iter().find(|g| g.group == "saga").unwrap();
    let mut profile = SelectionProfile::default();
    profile.set_choice(
        &selection.system,
        "saga",
        checked,
        &saga.picks(),
        saga.engine_unresolved(),
    );
    profile
}

#[test]
fn a_choice_that_leaves_out_a_disc_keeps_the_group_unresolved_and_is_kept() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());
    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let partial = BTreeSet::from(["Saga (Europe) (Disc 1)".to_owned()]);
    let profile = choosing(&selection, partial.clone());

    let chosen = select(work.path(), &dat, &profile);
    let saga = chosen.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    let choice = saga.choice.as_ref().unwrap();
    assert_eq!(
        (choice.status, &choice.checked),
        (ChoiceStatus::Applied, &partial)
    );
    assert_eq!(
        discs::unresolved_reason(saga).as_deref(),
        Some("your choice doesn't cover disc 2")
    );
    assert_eq!(chosen.summary.unresolved, 1);
}

#[test]
fn a_saved_whole_choice_is_flagged_again_when_a_newer_dat_adds_a_disc() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());
    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let profile = choosing(&selection, europe());
    let saga = select(work.path(), &dat, &profile)
        .groups
        .into_iter()
        .find(|g| g.group == "saga");
    assert!(!saga.unwrap().unresolved);

    let newer = write_dat(
        work.path(),
        "discs (newer).dat",
        &[
            "Saga (USA) (Disc 1)",
            "Saga (Europe) (Disc 1)",
            "Saga (Europe) (Disc 2)",
            "Saga (Europe) (Disc 3)",
            "Solo (USA)",
        ],
    );
    let again = select(work.path(), &newer, &profile);
    let saga = again.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    let choice = saga.choice.as_ref().unwrap();
    assert_eq!(
        (choice.status, &choice.checked),
        (ChoiceStatus::Applied, &europe())
    );
    assert_eq!(
        discs::unresolved_reason(saga).as_deref(),
        Some("your choice doesn't cover disc 3")
    );
    assert!(
        !again
            .groups
            .iter()
            .find(|g| g.group == "solo")
            .unwrap()
            .unresolved
    );
}
