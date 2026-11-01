//! Ingest Folders: scans, the state of each item, assessment against every Source Set, and the plans that move items
//! out, from synthetic DATs, Source Sets and files.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use title_select_api::JobKind;
use title_select_api::ingest::{IngestAction, IngestItem, ItemState, Need, PlanIngest, Proposal};
use title_select_store::config::Config;
use title_select_store::{Error, Fault, Store, Workers};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// Writes a DAT named `name` holding `games`.
fn write_named_dat(
    dir: &Path,
    file: &str,
    name: &str,
    version: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    let path = write_dat_version(dir, file, version, games);
    let xml = std::fs::read_to_string(&path).unwrap().replace(
        "<name>Test - System</name>",
        &format!("<name>{name}</name>"),
    );
    std::fs::write(&path, xml).unwrap();
    path
}

fn alpha_games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
    ]
}

fn beta_games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (Beta)", vec![rom("Pong (Beta).bin", b"pong")]),
        ("Zap (Beta)", vec![rom("Zap (Beta).bin", b"zap")]),
    ]
}

/// A store with two Source Sets in the read-only Root `sets`: Alpha, which holds Pong and Combat, and Beta, which
/// holds nothing; both audited. `in` is an Ingest Folder that settles at once, `slow` one that never settles within
/// a test.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        for dir in ["Alpha", "Beta"] {
            std::fs::create_dir_all(sets.join(dir)).unwrap();
        }
        for dir in ["in", "slow", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\
             settle_secs = 0\n\n[roots.slow]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            sets.display().to_string(),
            tmp.path().join("in").display().to_string(),
            tmp.path().join("slow").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dats = tmp.path().join("dats");
        let alpha = write_named_dat(&dats, "alpha.dat", "Alpha System", "1", &alpha_games());
        let beta = write_named_dat(&dats, "beta.dat", "Beta System", "1", &beta_games());
        store.import_dat(&alpha, "Test", None).unwrap();
        store.import_dat(&beta, "Test", None).unwrap();
        store
            .add_source_set(&config, "sets", "Alpha", "Test/Alpha System")
            .unwrap();
        store
            .add_source_set(&config, "sets", "Beta", "Test/Beta System")
            .unwrap();
        write_file(&sets.join("Alpha/Pong (USA).a26"), b"pong");
        write_file(&sets.join("Alpha/Combat (USA).a26"), b"combat");
        for set in ["Alpha", "Beta"] {
            store.audit_source_set(&config, set, None).unwrap();
        }
        let env = Env { tmp, store, config };
        env.store.register_ingest_folder(&env.config, "in").unwrap();
        env.store
            .register_ingest_folder(&env.config, "slow")
            .unwrap();
        env
    }

    fn dir(&self, root: &str) -> PathBuf {
        self.tmp.path().join(root)
    }

    fn put(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir("in").join(name);
        write_file(&path, bytes);
        path
    }

    /// Queues, claims and runs a scan of `root`.
    fn scan_root(&self, root: &str) {
        let job = self.store.enqueue_ingest_scan(root, "Operator").unwrap();
        assert_eq!(job.kind, JobKind::IngestScan);
        assert_eq!(job.ingest_folder.as_deref(), Some(root));
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        let ended = self
            .store
            .run_ingest_scan_job(&self.config, job.id)
            .unwrap();
        assert_eq!(
            ended.state,
            title_select_api::JobState::Succeeded,
            "{ended:?}"
        );
    }

    fn scan(&self) {
        self.scan_root("in");
    }

    fn items(&self) -> Vec<IngestItem> {
        self.store.ingest_items("in").unwrap()
    }

    fn item(&self, name: &str) -> IngestItem {
        self.items()
            .into_iter()
            .filter(|i| i.name == name)
            .max_by_key(|i| i.id)
            .unwrap_or_else(|| panic!("no item {name}"))
    }

    fn state(&self, name: &str) -> ItemState {
        self.item(name).state
    }

    /// Everything in the Ingest Folder `in` but the marker, as paths relative to it.
    fn listing(&self) -> Vec<String> {
        let base = self.dir("in");
        let mut found: Vec<String> = walkdir::WalkDir::new(&base)
            .min_depth(1)
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                e.path()
                    .strip_prefix(&base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|p| p != ".title-select")
            .collect();
        found.sort();
        found
    }

    fn plan(&self, action: IngestAction, items: &[&str]) -> title_select_api::ingest::IngestPlan {
        let ids = items.iter().map(|name| self.item(name).id).collect();
        self.store
            .plan_ingest(
                &self.config,
                "in",
                &PlanIngest {
                    action,
                    items: ids,
                    provider: None,
                },
            )
            .unwrap()
    }
}

#[test]
fn an_item_arrives_settles_is_assessed_goes_stale_when_changed_and_gone_when_removed() {
    let env = Env::new();
    env.put("pong.a26", b"pong");

    env.scan();
    assert_eq!(env.state("pong.a26"), ItemState::Arriving);

    env.scan();
    assert_eq!(env.state("pong.a26"), ItemState::Assessed);

    env.put("pong.a26", b"pong, changed");
    env.scan();
    assert_eq!(env.state("pong.a26"), ItemState::Stale);
    env.scan();
    let item = env.item("pong.a26");
    assert_eq!(item.state, ItemState::Assessed);
    assert!(item.assessment.matches.is_empty(), "{item:?}");

    std::fs::remove_file(env.dir("in").join("pong.a26")).unwrap();
    env.scan();
    assert_eq!(env.state("pong.a26"), ItemState::Gone);

    env.put("pong.a26", b"pong");
    env.scan();
    let again = env.item("pong.a26");
    assert_eq!(again.state, ItemState::Arriving);
    assert_ne!(again.id, item.id, "a new item, not the gone one");
}

#[test]
fn an_item_still_changing_stays_arriving_and_partial_downloads_and_dotfiles_are_ignored() {
    let env = Env::new();
    env.put("pong.a26", b"po");
    env.scan();
    env.put("pong.a26", b"pong");
    env.scan();
    assert_eq!(
        env.state("pong.a26"),
        ItemState::Arriving,
        "it changed since the last scan"
    );
    env.scan();
    assert_eq!(env.state("pong.a26"), ItemState::Assessed);

    for name in [
        "big.zip.part",
        "big.zip.!qB",
        "big.zip.crdownload",
        "big.zip.tmp",
        ".hidden",
    ] {
        env.put(name, b"pong");
    }
    env.scan();
    env.scan();
    let names: Vec<String> = env.items().into_iter().map(|i| i.name).collect();
    assert_eq!(names, ["pong.a26"]);

    write_file(&env.dir("slow").join("pong.a26"), b"pong");
    env.scan_root("slow");
    env.scan_root("slow");
    let slow = env.store.ingest_items("slow").unwrap();
    assert_eq!(
        slow[0].state,
        ItemState::Arriving,
        "within the settle interval"
    );
}

#[test]
fn an_item_is_matched_against_every_source_set_and_proposed_for_the_one_that_needs_it() {
    let env = Env::new();
    env.put("pong.a26", b"pong");
    env.put("combat.a26", b"combat");
    env.put("game.zip", b"");
    write_zip(
        &env.dir("in").join("game.zip"),
        &[("Yars (USA).a26", b"yars")],
    );
    env.scan();
    env.scan();

    let pong = env.item("pong.a26");
    assert_eq!(pong.state, ItemState::Assessed);
    let mut sets: Vec<(&str, Need)> = pong
        .assessment
        .matches
        .iter()
        .map(|m| (m.source_set.as_str(), m.need))
        .collect();
    sets.sort();
    assert_eq!(sets, [("Alpha", Need::Held), ("Beta", Need::Missing)]);
    assert_eq!(
        pong.assessment.proposal,
        Some(Proposal::SourceSet {
            source_set: "Beta".into(),
            game: "Pong (Beta)".into(),
            need: Need::Missing,
        })
    );

    assert_eq!(env.state("combat.a26"), ItemState::AlreadyHeld);

    let yars = env.item("game.zip");
    assert_eq!(yars.state, ItemState::Assessed);
    assert!(yars.assessment.zip);
    assert_eq!(
        yars.assessment.proposal,
        Some(Proposal::SourceSet {
            source_set: "Alpha".into(),
            game: "Yars (USA)".into(),
            need: Need::Missing,
        })
    );
    assert_eq!(
        yars.assessment.matches[0].roms[0].member.as_deref(),
        Some("Yars (USA).a26")
    );
}

#[test]
fn an_unmatched_item_is_a_candidate_operator_addition_with_the_system_its_extension_suggests() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.put("mystery.bin", b"homebrew");
    env.put("mystery.xyz", b"homebrew");
    env.scan();
    env.scan();

    let proposal = |name| env.item(name).assessment.proposal;
    assert_eq!(
        proposal("mystery.a26"),
        Some(Proposal::OperatorAddition {
            system: Some("Test/Alpha System".into())
        })
    );
    assert_eq!(
        proposal("mystery.bin"),
        Some(Proposal::OperatorAddition {
            system: Some("Test/Beta System".into())
        })
    );
    assert_eq!(
        proposal("mystery.xyz"),
        Some(Proposal::OperatorAddition { system: None })
    );
}

#[test]
fn an_unsupported_archive_is_never_opened_and_a_damaged_zip_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    for name in ["set.7z", "set.rar"] {
        let path = env.put(name, b"not an archive Title Select reads");
        // Opening either would fail, and make it Failed.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    write_zip(
        &env.dir("in").join("nested.zip"),
        &[("inner.zip", b"PK"), ("Pong (USA).a26", b"pong")],
    );
    write_zip(
        &env.dir("in").join("whole-set.zip"),
        &[("Pong (USA).a26", b"pong"), ("Yars (USA).a26", b"yars")],
    );
    std::fs::create_dir(env.dir("in").join("a folder")).unwrap();
    env.put("damaged.zip", b"PK\x03\x04 not really");
    env.scan();
    env.scan();

    for name in [
        "set.7z",
        "set.rar",
        "nested.zip",
        "whole-set.zip",
        "a folder",
    ] {
        let item = env.item(name);
        assert_eq!(item.state, ItemState::Unsupported, "{item:?}");
        assert!(item.reason.is_some());
        assert!(item.assessment.payloads.is_empty(), "{item:?}");
    }
    assert_eq!(env.state("damaged.zip"), ItemState::Unreadable);
}

#[test]
fn an_unchanged_item_is_not_read_again_but_matched_again() {
    let env = Env::new();
    env.put("pong.a26", b"pong");
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    env.store.hook_faults(Some(Arc::new(move |fault| {
        if fault == Fault::IngestRead {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    })));
    env.scan();
    env.scan();
    env.scan();
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    // Beta gains Pong: the item, read once, is now held by every Source Set it matches.
    write_file(&env.dir("roots/sets/Beta/Pong (Beta).bin"), b"pong");
    env.store
        .audit_source_set(&env.config, "Beta", None)
        .unwrap();
    env.scan();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(env.state("pong.a26"), ItemState::AlreadyHeld);
}

#[test]
fn a_dismissed_item_stays_dismissed_until_it_changes() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.scan();
    env.scan();
    let id = env.item("mystery.a26").id;

    env.store.dismiss_ingest_item(id).unwrap();
    env.scan();
    assert_eq!(env.state("mystery.a26"), ItemState::Dismissed);

    env.put("mystery.a26", b"homebrew, version 2");
    env.scan();
    assert_eq!(env.state("mystery.a26"), ItemState::Stale);
}

#[test]
fn a_discard_changes_nothing_until_approved_then_keeps_the_item() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.put("other.a26", b"other");
    env.scan();
    env.scan();
    let before = env.listing();

    let plan = env.plan(IngestAction::Discard, &["mystery.a26"]);
    assert_eq!(plan.content.items.len(), 1);
    assert_eq!(
        plan.content.items[0].destination,
        ".title-select-retained/1/mystery.a26.kept"
    );
    assert_eq!(env.listing(), before, "planning changes nothing");

    let wrong = env
        .store
        .approve_ingest_plan(&env.config, plan.id, "0000", "Operator", WORKERS);
    assert!(matches!(wrong, Err(Error::Conflict(_))), "{wrong:?}");
    assert_eq!(env.listing(), before);

    let applied = env
        .store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert!(applied.outcomes.iter().all(|o| o.moved), "{applied:?}");
    assert_eq!(
        env.listing(),
        [".title-select-retained/1/mystery.a26.kept", "other.a26"]
    );
    assert_eq!(env.state("mystery.a26"), ItemState::Moved);
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].kept_name, "mystery.a26.kept");

    let again =
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(again.is_err(), "a plan is carried out once");
}

#[test]
fn approval_is_refused_once_anything_in_the_folder_changed_since_the_plan() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.scan();
    env.scan();
    let plan = env.plan(IngestAction::Discard, &["mystery.a26"]);

    env.put("new arrival.a26", b"new");
    let before = env.listing();
    let refused =
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);

    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    assert_eq!(env.listing(), before);
}

#[test]
fn a_dat_import_imports_the_revision_queues_audits_of_its_source_sets_and_keeps_the_file() {
    let env = Env::new();
    let mut games = alpha_games();
    games.push(("Adventure (USA)", vec![rom("Adventure (USA).a26", b"adv")]));
    write_named_dat(
        &env.dir("in"),
        "Alpha System (2).dat",
        "Alpha System",
        "2",
        &games,
    );
    env.scan();
    env.scan();
    let item = env.item("Alpha System (2).dat");
    assert_eq!(item.state, ItemState::Assessed);
    let dat = item.assessment.dat.clone().unwrap();
    assert_eq!(dat.name, "Alpha System");
    assert_eq!(dat.providers, ["Test"]);
    assert!(!dat.imported);
    assert_eq!(item.assessment.proposal, Some(Proposal::DatImport));
    let revisions = env.store.dat_revisions().unwrap().len();

    let plan = env.plan(IngestAction::DatImport, &["Alpha System (2).dat"]);
    assert_eq!(
        env.store.dat_revisions().unwrap().len(),
        revisions,
        "nothing until approved"
    );
    let applied = env
        .store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    assert!(applied.outcomes[0].moved, "{applied:?}");
    assert_eq!(env.store.dat_revisions().unwrap().len(), revisions + 1);
    let jobs = env.store.jobs(10).unwrap();
    let audits: Vec<&str> = jobs
        .iter()
        .filter(|j| j.kind == JobKind::Audit && j.state == title_select_api::JobState::Queued)
        .map(|j| j.source_set.as_str())
        .collect();
    assert_eq!(audits, ["Alpha"]);
    assert_eq!(
        env.listing(),
        [".title-select-retained/1/Alpha System (2).dat.kept"]
    );
}

#[test]
fn a_dat_import_of_a_file_that_is_not_a_dat_is_refused() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.scan();
    env.scan();
    let id = env.item("mystery.a26").id;
    let refused = env.store.plan_ingest(
        &env.config,
        "in",
        &PlanIngest {
            action: IngestAction::DatImport,
            items: vec![id],
            provider: None,
        },
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
}

#[test]
fn without_its_marker_an_ingest_folder_is_never_written_or_scanned() {
    let env = Env::new();
    env.put("mystery.a26", b"homebrew");
    env.scan();
    env.scan();
    let plan = env.plan(IngestAction::Discard, &["mystery.a26"]);
    std::fs::remove_file(env.dir("in").join(".title-select")).unwrap();

    let refused =
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);

    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    assert_eq!(env.listing(), ["mystery.a26"]);
    let job = env.store.enqueue_ingest_scan("in", "Operator").unwrap();
    env.store.claim_job().unwrap();
    let ended = env.store.run_ingest_scan_job(&env.config, job.id).unwrap();
    assert_eq!(ended.state, title_select_api::JobState::Failed);
    assert_eq!(
        env.state("mystery.a26"),
        ItemState::Assessed,
        "nothing is marked gone"
    );
}

#[test]
fn only_an_ingest_root_is_registered_and_it_holds_no_managed_folder() {
    let env = Env::new();
    let refused = env.store.register_ingest_folder(&env.config, "sets");
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    assert!(env.store.enqueue_ingest_scan("sets", "Operator").is_err());

    let folders = env.store.ingest_folders(&env.config).unwrap();
    let roots: Vec<(&str, bool)> = folders
        .iter()
        .map(|f| (f.root.as_str(), f.id.is_some()))
        .collect();
    assert_eq!(roots, [("in", true), ("slow", true)]);

    let marker = std::fs::read_to_string(env.dir("in").join(".title-select")).unwrap();
    assert!(marker.contains("ingest_folder"), "{marker}");
    // Registering again is a no-op; a marker of another store is refused.
    env.store.register_ingest_folder(&env.config, "in").unwrap();
    std::fs::write(
        env.dir("in").join(".title-select"),
        r#"{"store": "another", "ingest_folder": 1}"#,
    )
    .unwrap();
    assert!(env.store.register_ingest_folder(&env.config, "in").is_err());
}
