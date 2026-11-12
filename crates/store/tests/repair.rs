//! Repairing Source Sets: plans proposed from the latest Audit and assessed ingest items, the choices you make before
//! approving one, and what an approved repair does, from synthetic DATs, Source Sets and files.

#[path = "../../engine/tests/common/mod.rs"]
mod common;
#[path = "../../engine/tests/common/sevenz.rs"]
mod sevenz_fixtures;

use std::path::{Path, PathBuf};

use common::*;
use title_select_api::build::PlanKeptDeletion;
use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use title_select_api::storage::{StorageFormat, SystemStorage};
use title_select_api::{AuditEvidence, Finding, GameStatus, ImportOutcome, JobKind, JobState};
use title_select_engine::build::is_torrentzip;
use title_select_store::config::Config;
use title_select_store::{Error, Fault, Store, Workers};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

fn alpha_games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
        (
            "Adventure (USA)",
            vec![
                rom("Adventure (USA).a26", b"adventure"),
                rom("Adventure (USA).txt", b"manual"),
            ],
        ),
    ]
}

/// A store with the Source Set Alpha in the read-only Root `sets`, which lists it among its `writable_paths`, and
/// Gamma, with the same DAT, in the read-only Root `ro`, which does not. `in` is an Ingest Folder that settles at once.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        Env::with("Test", &alpha_games())
    }

    /// Alpha and Gamma bound to the DAT of `games` from `provider`.
    fn with(provider: &str, games: &[(&str, Vec<RomSpec>)]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/Alpha", "roots/ro/Gamma", "in", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"Alpha\"]\n\n[roots.ro]\npath = {:?}\n\n\
             [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\nsettle_secs = 0\n",
            tmp.path().join("roots/sets").display().to_string(),
            tmp.path().join("roots/ro").display().to_string(),
            tmp.path().join("in").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(&tmp.path().join("dats"), "alpha.dat", "1", games);
        store.import_dat(&dat, provider, None).unwrap();
        let system = format!("{provider}/Test - System");
        store
            .add_source_set(&config, "sets", "Alpha", &system)
            .unwrap();
        store
            .add_source_set(&config, "ro", "Gamma", &system)
            .unwrap();
        let env = Env { tmp, store, config };
        env.store.register_ingest_folder(&env.config, "in").unwrap();
        env
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("roots/sets/Alpha")
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        write_file(&self.set().join(rel), bytes);
    }

    fn audit(&self, evidence: AuditEvidence) {
        self.store
            .audit_source_set(&self.config, "Alpha", None, evidence)
            .unwrap();
    }

    /// Puts `name` in the Ingest Folder and scans it twice, so it settles and is assessed.
    fn ingest(&self, name: &str, write: impl FnOnce(&Path)) {
        write(&self.tmp.path().join("in").join(name));
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self
                .store
                .run_ingest_scan_job(&self.config, job.id)
                .unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
    }

    fn plan(&self) -> RepairPlan {
        self.plan_with(&[])
    }

    fn plan_with(&self, choices: &[(&str, RepairChoice)]) -> RepairPlan {
        let request = PlanRepair {
            choices: choices
                .iter()
                .map(|(key, choice)| (key.to_string(), *choice))
                .collect(),
            additions: Default::default(),
        };
        self.store
            .plan_repair(&self.config, "Alpha", &request)
            .unwrap()
    }

    /// Approves `plan`, runs the Repair Job that queues, and queues the Audit it owes.
    fn approve(&self, plan: &RepairPlan) -> RepairPlan {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert!(approved.finished_at.is_none(), "approval only queues it");
        let ended = self.run_repair_job(&approved);
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.fulfil_repair_audits().unwrap();
        let done = self.store.repair_plan(plan.id).unwrap();
        assert!(done.finished_at.is_some(), "{done:?}");
        assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
        done
    }

    /// Claims and runs the Repair Job queued for `plan`.
    fn run_repair_job(&self, plan: &RepairPlan) -> title_select_api::Job {
        let job = plan.job.expect("an approved repair has a job");
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job);
        self.store.run_repair_job(&self.config, job, None).unwrap()
    }

    /// Runs the incremental Audit a repair queued.
    fn run_queued_audit(&self, plan: &RepairPlan) {
        let job = plan.audit_job.expect("a repair queues an audit");
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job);
        let ended = self.store.run_audit_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    }

    fn latest_report(&self) -> title_select_api::Report {
        let latest = &self.store.history("Alpha").unwrap()[0];
        self.store.report(latest.id).unwrap()
    }

    /// Every file under `dir`, as paths relative to it.
    fn files(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                e.path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        found.sort();
        found
    }

    fn listing(&self) -> Vec<String> {
        Env::files(&self.set())
    }

    fn ingest_listing(&self) -> Vec<String> {
        Env::files(&self.tmp.path().join("in"))
            .into_iter()
            .filter(|p| p != ".title-select")
            .collect()
    }
}

fn item<'a>(plan: &'a RepairPlan, key: &str) -> &'a title_select_api::repair::RepairItem {
    plan.content
        .items
        .iter()
        .find(|i| i.key == key)
        .unwrap_or_else(|| panic!("no item {key} in {:#?}", plan.content.items))
}

#[test]
fn a_source_set_its_root_does_not_let_you_write_to_is_refused() {
    let env = Env::new();
    write_file(
        &env.tmp.path().join("roots/ro/Gamma/Pong (Anthology).a26"),
        b"pong",
    );
    env.store
        .audit_source_set(&env.config, "Gamma", None, AuditEvidence::Full)
        .unwrap();

    let refused = env
        .store
        .plan_repair(&env.config, "Gamma", &PlanRepair::default());

    match refused {
        Err(Error::Refused(why)) => assert!(why.contains("writable_paths"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_misnamed_game_is_renamed_as_torrentzip_and_the_first_repair_writes_the_marker() {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let before = env.listing();

    let plan = env.plan();

    let pong = item(&plan, "game:Pong (USA)");
    assert_eq!(pong.kind, RepairKind::Rename);
    assert_eq!(pong.default, RepairChoice::Apply);
    assert!(plan.content.creates_marker);
    assert_eq!(env.listing(), before, "planning changes nothing");

    let done = env.approve(&plan);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Pong (Anthology).a26.kept",
            "Pong (USA).zip"
        ]
    );
    assert!(is_torrentzip(&env.set().join("Pong (USA).zip")));
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "Pong (Anthology).a26");

    env.run_queued_audit(&done);
    let report = env.latest_report();
    assert_eq!(report.evidence, AuditEvidence::Incremental);
    assert_eq!(report.entries.len(), 1, "own entries are not audited");
    assert_eq!(report.entries[0].path, "Pong (USA).zip");
    assert_eq!(report.entries[0].finding, Finding::Matched);
    let pong = report.games.iter().find(|g| g.name == "Pong (USA)");
    assert_eq!(pong.unwrap().status, GameStatus::Complete);
    assert!(
        env.plan().content.items.is_empty(),
        "nothing more to repair"
    );
}

#[test]
fn a_game_not_stored_as_torrentzip_is_repacked_and_its_original_kept() {
    let env = Env::new();
    write_zip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.put("Yars (USA).a26", b"yars");
    write_zip(
        &env.set().join("Adventure (USA).zip"),
        &[
            ("Adventure (USA).a26", b"adventure"),
            ("Adventure (USA).txt", b"manual"),
            ("cover.jpg", b"extra"),
        ],
    );
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    for (game, kind) in [
        ("Combat (USA)", RepairKind::Repack),
        ("Yars (USA)", RepairKind::Convert),
        ("Adventure (USA)", RepairKind::Repack),
    ] {
        let found = item(&plan, &format!("game:{game}"));
        assert_eq!(found.kind, kind, "{game}");
        assert_eq!(found.default, RepairChoice::Apply, "{game}");
    }
    let done = env.approve(&plan);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Adventure (USA).zip.kept",
            ".title-select-retained/1/Combat (USA).zip.kept",
            ".title-select-retained/1/Yars (USA).a26.kept",
            "Adventure (USA).zip",
            "Combat (USA).zip",
            "Yars (USA).zip",
        ]
    );
    for game in ["Combat (USA)", "Yars (USA)", "Adventure (USA)"] {
        assert!(is_torrentzip(&env.set().join(format!("{game}.zip"))));
    }
    env.run_queued_audit(&done);
    let report = env.latest_report();
    assert!(
        report.entries.iter().all(|e| e.finding == Finding::Matched),
        "{:#?}",
        report.entries
    );
    assert_eq!(
        report.entries.len(),
        4,
        "the extra member is only in the kept original"
    );
}

#[test]
fn a_file_with_nothing_a_game_needs_is_kept_aside_and_deleted_only_once_you_approve_it() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let readme = item(&plan, "file:readme.txt");
    assert_eq!(readme.kind, RepairKind::KeepAside);
    assert_eq!(readme.default, RepairChoice::KeepAside);
    env.approve(&plan);
    assert_eq!(
        env.listing(),
        [".title-select", ".title-select-retained/1/readme.txt.kept"]
    );

    let kept = env.store.source_kept_files("Alpha").unwrap();
    let deletion = env
        .store
        .plan_source_kept_deletion(
            &env.config,
            "Alpha",
            &PlanKeptDeletion {
                ids: vec![kept[0].id],
                all: false,
            },
        )
        .unwrap();
    assert_eq!(env.listing().len(), 2, "planning deletes nothing");
    let wrong =
        env.store
            .approve_source_kept_deletion(&env.config, deletion.id, "0000", "Operator");
    assert!(matches!(wrong, Err(Error::Conflict(_))), "{wrong:?}");

    let deleted = env
        .store
        .approve_source_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();

    assert!(deleted.outcomes.iter().all(|o| o.deleted), "{deleted:?}");
    assert_eq!(env.listing(), [".title-select"]);
    assert!(env.store.source_kept_files("Alpha").unwrap().is_empty());
}

/// Rewrites `path` in place, through a descriptor, to `bytes` of the same length, and sets its mtime back.
fn rewrite_unnoticed(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
    file.write_all(bytes).unwrap();
    file.set_modified(mtime).unwrap();
}

#[test]
fn a_kept_file_rewritten_since_you_reviewed_its_deletion_is_never_deleted() {
    // The last only by its ctime: written once moved with the bytes it held.
    for (once_moved, bytes) in [(false, b"jello"), (true, b"jello"), (true, b"hello")] {
        let env = Env::new();
        env.put("readme.txt", b"hello");
        env.audit(AuditEvidence::Full);
        env.approve(&env.plan());
        let id = env.store.source_kept_files("Alpha").unwrap()[0].id;
        let deletion = env
            .store
            .plan_source_kept_deletion(
                &env.config,
                "Alpha",
                &PlanKeptDeletion {
                    ids: vec![id],
                    all: false,
                },
            )
            .unwrap();
        let set = env.set();
        let kept = set.join(".title-select-retained/1/readme.txt.kept");
        if once_moved {
            let deleting = set.join(".title-select-retained/.deleting");
            env.store
                .hook_faults(Some(std::sync::Arc::new(move |fault| {
                    if fault == Fault::SourceKeptMoved(id) {
                        let slot = std::fs::read_dir(&deleting)
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        rewrite_unnoticed(&slot.path(), bytes);
                    }
                })));
        } else {
            rewrite_unnoticed(&kept, bytes);
        }

        let done = env.store.approve_source_kept_deletion(
            &env.config,
            deletion.id,
            &deletion.digest,
            "Operator",
        );
        env.store.hook_faults(None);

        let done = done.unwrap();
        assert!(
            done.outcomes.iter().all(|o| !o.deleted
                && o.detail
                    .as_deref()
                    .is_some_and(|d| d.contains("changed since you reviewed"))),
            "once_moved {once_moved}: {:#?}",
            done.outcomes
        );
        assert_eq!(std::fs::read(&kept).unwrap(), bytes);
        assert_eq!(env.store.source_kept_files("Alpha").unwrap().len(), 1);
    }
}

#[test]
fn a_stopped_deletion_of_a_kept_file_is_finished_only_once_both_folders_are_synced() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    env.approve(&env.plan());
    let id = env.store.source_kept_files("Alpha").unwrap()[0].id;
    let deletion = env
        .store
        .plan_source_kept_deletion(
            &env.config,
            "Alpha",
            &PlanKeptDeletion {
                ids: vec![id],
                all: false,
            },
        )
        .unwrap();
    env.store.inject_fault(Some(Fault::SourceKeptMoved(id)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.approve_source_kept_deletion(
            &env.config,
            deletion.id,
            &deletion.digest,
            "Operator",
        )
    }));
    assert!(ran.is_err(), "{ran:?}");
    let moved = env.listing();
    let slot = moved
        .iter()
        .find(|p| p.starts_with(".title-select-retained/.deleting/"))
        .unwrap_or_else(|| panic!("{moved:?}"))
        .clone();
    let recover_until_synced = || {
        env.store.inject_fault(Some(Fault::SourceKeptSynced(id)));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.store.recover_source_kept_deletions(&env.config)
        }));
        env.store.inject_fault(None);
        assert!(ran.is_err(), "recovery decided before it synced: {ran:?}");
        assert!(
            env.store
                .source_kept_deletion(deletion.id)
                .unwrap()
                .finished_at
                .is_none()
        );
    };

    // Found in the slot it was moved to.
    recover_until_synced();
    assert!(env.listing().contains(&slot));
    // Already gone from both places.
    std::fs::remove_file(env.set().join(&slot)).unwrap();
    recover_until_synced();

    assert!(
        env.store
            .recover_source_kept_deletions(&env.config)
            .unwrap()
            .is_empty()
    );
    let done = env.store.source_kept_deletion(deletion.id).unwrap();
    assert!(done.finished_at.is_some(), "{done:?}");
    assert!(env.store.source_kept_files("Alpha").unwrap().is_empty());
}

#[test]
fn a_plan_is_refused_once_its_digest_its_audit_or_the_source_set_changed() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();

    let wrong = env
        .store
        .approve_repair(&env.config, plan.id, "0000", "Operator", WORKERS);
    assert!(matches!(wrong, Err(Error::Conflict(_))), "{wrong:?}");

    env.put("new arrival.a26", b"new");
    let before = env.listing();
    let changed = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(matches!(changed, Err(Error::Conflict(_))), "{changed:?}");
    assert_eq!(env.listing(), before, "nothing changed");

    std::fs::remove_file(env.set().join("new arrival.a26")).unwrap();
    env.audit(AuditEvidence::Full);
    let audited = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&audited, Err(Error::Conflict(why)) if why.contains("audit")),
        "{audited:?}"
    );

    let first = env.plan();
    let second = env.plan();
    env.approve(&second);
    let stale = env
        .store
        .approve_repair(&env.config, first.id, &first.digest, "Operator", WORKERS);
    assert!(matches!(stale, Err(Error::Conflict(_))), "{stale:?}");
}

#[test]
fn no_repair_is_proposed_while_an_approved_one_runs() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let request = PlanRepair::default();
    let refused = env.store.plan_repair(&env.config, "Alpha", &request);
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("hasn't finished")),
        "{refused:?}"
    );
    let ended = env.run_repair_job(&approved);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.store.fulfil_repair_audits().unwrap();
    env.plan();
}

#[test]
fn an_approval_is_refused_when_an_audit_finishes_while_it_is_being_checked() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let (reached, wait) = std::sync::mpsc::channel::<()>();
    let (resume, resumed) = std::sync::mpsc::channel::<()>();
    let (reached, resumed) = (
        std::sync::Mutex::new(reached),
        std::sync::Mutex::new(resumed),
    );
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairApproving {
                reached.lock().unwrap().send(()).unwrap();
                resumed.lock().unwrap().recv().unwrap();
            }
        })));

    let approved = std::thread::scope(|s| {
        let approving = s.spawn(|| {
            env.store
                .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        });
        wait.recv().unwrap();
        env.audit(AuditEvidence::Full);
        resume.send(()).unwrap();
        approving.join().unwrap()
    });
    env.store.hook_faults(None);

    assert!(
        matches!(&approved, Err(Error::Conflict(why)) if why.contains("audit")),
        "{approved:?}"
    );
    assert!(env.store.repair_plan(plan.id).unwrap().approval.is_none());
    assert_eq!(env.listing(), ["readme.txt"]);
}

#[test]
fn once_a_source_set_has_been_repaired_a_repair_is_refused_without_its_marker() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    env.approve(&env.plan());
    env.put("notes.txt", b"more");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    assert!(!plan.content.creates_marker);

    std::fs::remove_file(env.set().join(".title-select")).unwrap();
    let before = env.listing();
    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);

    match refused {
        Err(Error::Refused(why)) => assert!(why.contains("marker"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(env.listing(), before);
}

#[test]
fn a_conflicting_entry_is_left_unless_you_choose_otherwise() {
    let env = Env::new();
    env.put("Combat (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let conflict = item(&plan, "entry:Combat (USA).a26");
    assert_eq!(conflict.kind, RepairKind::Conflict);
    assert_eq!(conflict.default, RepairChoice::Leave);
    assert!(plan.content.steps.is_empty(), "{:#?}", plan.content.steps);

    let chosen = env.plan_with(&[("entry:Combat (USA).a26", RepairChoice::Apply)]);
    assert_eq!(
        item(&chosen, "entry:Combat (USA).a26").chosen,
        RepairChoice::Apply
    );
    env.approve(&chosen);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Combat (USA).a26.kept",
            "Pong (USA).zip"
        ]
    );
}

#[test]
fn a_conflicting_entry_you_choose_to_keep_aside_is_kept_aside() {
    let env = Env::new();
    env.put("Combat (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);

    let plan = env.plan_with(&[("entry:Combat (USA).a26", RepairChoice::KeepAside)]);
    let note = item(&plan, "entry:Combat (USA).a26").note.as_deref();
    assert!(
        note.is_some_and(
            |n| n.contains("last copy in the source set of Pong (USA).a26 of Pong (USA)")
        ),
        "{note:?}"
    );
    env.approve(&plan);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Combat (USA).a26.kept"
        ]
    );
}

#[test]
fn a_missing_game_filled_from_a_loose_ingest_item_that_is_exactly_its_rom_deletes_the_original() {
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("pong.a26", |p| write_file(p, b"pong"));

    let plan = env.plan();
    let pong = item(&plan, "game:Pong (USA)");
    assert_eq!(pong.kind, RepairKind::Fill);
    assert_eq!(pong.default, RepairChoice::Apply);
    let done = env.approve(&plan);

    assert_eq!(env.listing(), [".title-select", "Pong (USA).zip"]);
    assert!(is_torrentzip(&env.set().join("Pong (USA).zip")));
    assert!(
        env.ingest_listing().is_empty(),
        "{:?}",
        env.ingest_listing()
    );
    assert!(env.store.ingest_kept_files("in").unwrap().is_empty());
    let items = env.store.ingest_items("in").unwrap();
    let moved = items[0].last_move.as_ref().expect("the repair moved it");
    assert_eq!(
        (moved.repair, moved.job),
        (Some(plan.id), done.job),
        "{moved:?}"
    );
    assert!(!moved.under_way);
    env.run_queued_audit(&done);
    let report = env.latest_report();
    let pong = report.games.iter().find(|g| g.name == "Pong (USA)");
    assert_eq!(pong.unwrap().status, GameStatus::Complete);
}

#[test]
fn a_missing_game_filled_from_a_zip_ingest_item_keeps_the_original_in_the_ingest_folder() {
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("combat.zip", |p| {
        write_zip(p, &[("Combat (USA).a26", b"combat")])
    });

    let plan = env.plan();
    assert_eq!(item(&plan, "game:Combat (USA)").kind, RepairKind::Fill);
    env.approve(&plan);

    assert_eq!(env.listing(), [".title-select", "Combat (USA).zip"]);
    assert!(is_torrentzip(&env.set().join("Combat (USA).zip")));
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "combat.zip");
    assert_eq!(
        env.ingest_listing(),
        [format!(
            ".title-select-retained/{}/combat.zip.kept",
            kept[0].generation
        )]
    );
}

#[test]
fn a_mismatched_entry_is_replaced_from_ingest_and_kept() {
    let env = Env::new();
    env.put("Yars (USA).a26", b"bad dump");
    env.audit(AuditEvidence::Full);
    assert!(
        env.plan().content.steps.is_empty(),
        "without an ingest item nothing is proposed"
    );
    env.ingest("yars.a26", |p| write_file(p, b"yars"));

    let plan = env.plan();
    let yars = item(&plan, "game:Yars (USA)");
    assert_eq!(yars.kind, RepairKind::Replace);
    assert_eq!(yars.default, RepairChoice::Apply);
    env.approve(&plan);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Yars (USA).a26.kept",
            "Yars (USA).zip"
        ]
    );
    assert!(env.ingest_listing().is_empty());
}

#[test]
fn a_file_read_only_by_its_claim_is_never_touched() {
    let env = Env::new();
    write_zip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.audit(AuditEvidence::Quick);

    let plan = env.plan();

    assert!(plan.content.steps.is_empty(), "{:#?}", plan.content.steps);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Combat (USA).zip")),
        "{:?}",
        plan.content.notes
    );
}

#[test]
fn a_new_file_kept_aside_never_takes_the_kept_name_of_an_original_kept_after_it() {
    let adventure = &alpha_games()[3].1;
    // Adventure's ROMs are in old.zip; Pong's is in a file named as Adventure's, so both originals are kept aside
    // once their new files are in place, and the name the second one is kept as is Adventure's new file's name.
    let publish_of = |target: &'static str| move |s: &RepairStep| matches!(s, RepairStep::Publish { target: t, .. } if t == target);
    // The name differs from Adventure's new file's only in case, which a case-insensitive file system ignores.
    for (original, stop) in [
        ("Adventure (USA).zip", None),
        ("Adventure (USA).zip", Some("moved")),
        ("Adventure (USA).zip", Some("done")),
        ("adventure (usa).zip", None),
        ("adventure (usa).zip", Some("moved")),
    ] {
        let incoming = format!("incoming/{original}");
        let env = Env::new();
        let members: Vec<(&str, &[u8])> = adventure
            .iter()
            .map(|r| (r.name, r.payload.as_slice()))
            .collect();
        write_zip(&env.set().join("old.zip"), &members);
        std::fs::create_dir(env.set().join("incoming")).unwrap();
        write_zip(&env.set().join(&incoming), &[("Pong (USA).a26", b"pong")]);
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        let fault = match stop {
            Some("moved") => Some(Fault::RepairMoved(step_seq(
                &plan,
                publish_of("Adventure (USA).zip"),
            ))),
            Some(_) => Some(Fault::RepairStepDone(step_seq(
                &plan,
                publish_of("Pong (USA).zip"),
            ))),
            None => None,
        };
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        // A file appears where Adventure's new file goes, so that new file is kept aside instead.
        env.put("Adventure (USA).zip", b"in the way");
        if fault.is_some() {
            env.store.inject_fault(fault);
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.run_repair_job(&approved)
            }));
            assert!(ran.is_err(), "{stop:?}: {ran:?}");
            env.store.inject_fault(None);
            let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
                .requeued
                .clone()
                .expect("a repair runs on");
            assert_eq!(requeued.repair, Some(plan.id));
            env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
        } else {
            env.run_repair_job(&approved);
        }

        let done = env.store.repair_plan(plan.id).unwrap();
        let pong = done
            .outcomes
            .iter()
            .find(|o| o.item == "game:Pong (USA)")
            .unwrap();
        assert!(pong.done, "{stop:?}: {pong:?}");
        let listing = env.listing();
        assert!(!listing.contains(&incoming), "{stop:?}: {listing:?}");
        assert!(
            listing.contains(&"old.zip".to_string()),
            "{stop:?}: {listing:?}"
        );
        assert!(
            listing.contains(&"Pong (USA).zip".to_string()),
            "{stop:?}: {listing:?}"
        );
        let kept: Vec<&String> = listing
            .iter()
            .filter(|p| p.starts_with(".title-select-retained/"))
            .collect();
        assert_eq!(
            kept.len(),
            2,
            "{stop:?}: both the new Adventure and the old Pong are kept: {listing:?}"
        );
    }
}

#[test]
fn originals_named_alike_but_for_case_are_kept_under_names_that_differ_beyond_case() {
    let env = Env::new();
    let games = alpha_games();
    let adventure: Vec<(&str, &[u8])> = games[3]
        .1
        .iter()
        .map(|r| (r.name, r.payload.as_slice()))
        .collect();
    for dir in ["one", "two"] {
        std::fs::create_dir(env.set().join(dir)).unwrap();
    }
    write_zip(&env.set().join("one/Adventure (USA).zip"), &adventure);
    write_zip(
        &env.set().join("two/adventure (usa).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    env.audit(AuditEvidence::Full);
    let plan = env.plan();

    // A case-insensitive file system takes two names differing only in case for one file.
    let kept: Vec<String> = plan
        .content
        .steps
        .iter()
        .filter_map(|s| match &s.step {
            RepairStep::Keep { kept_name, .. } => Some(kept_name.to_lowercase()),
            _ => None,
        })
        .collect();
    let unique: std::collections::BTreeSet<&String> = kept.iter().collect();
    assert_eq!(kept.len(), 2, "{:#?}", plan.content.steps);
    assert_eq!(unique.len(), 2, "{kept:?}");

    let done = env.approve(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    let listing = env.listing();
    for path in ["Adventure (USA).zip", "Pong (USA).zip"] {
        assert!(listing.contains(&path.to_string()), "{listing:?}");
    }
    assert_eq!(
        listing
            .iter()
            .filter(|p| p.starts_with(".title-select-retained/"))
            .count(),
        2,
        "{listing:?}"
    );
}

#[test]
fn a_repair_the_service_stopped_in_is_finished_when_it_next_starts() {
    for stop in [
        Fault::RepairStaged(0),
        Fault::RepairMoved(1),
        Fault::RepairStepDone(1),
        Fault::RepairMoved(2),
        Fault::RepairStepDone(2),
    ] {
        let env = Env::new();
        env.put("Pong (Anthology).a26", b"pong");
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        assert_eq!(plan.content.steps.len(), 3, "{:#?}", plan.content.steps);

        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.inject_fault(Some(stop));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.run_repair_job(&approved)
        }));
        assert!(ran.is_err(), "{stop:?}: {ran:?}");
        env.store.inject_fault(None);
        let unfinished = env.store.repair_plan(plan.id).unwrap();
        assert!(unfinished.finished_at.is_none(), "{stop:?}");

        let recovered = env.store.recover_jobs(WORKERS).unwrap();

        assert_eq!(recovered.len(), 1, "{stop:?}");
        assert_eq!(recovered[0].job.state, JobState::Interrupted, "{stop:?}");
        let requeued = recovered[0].requeued.clone().expect("a repair runs on");
        assert_eq!(requeued.repair, Some(plan.id), "{stop:?}");
        let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
        assert_eq!(ended.state, JobState::Succeeded, "{stop:?}: {ended:?}");
        env.store.fulfil_repair_audits().unwrap();
        let done = env.store.repair_plan(plan.id).unwrap();
        assert!(done.finished_at.is_some(), "{stop:?}");
        assert!(done.outcomes.iter().all(|o| o.done), "{stop:?}: {done:#?}");
        assert_eq!(
            env.listing(),
            [
                ".title-select",
                ".title-select-retained/1/Pong (Anthology).a26.kept",
                "Pong (USA).zip"
            ],
            "{stop:?}"
        );
        assert!(done.audit_job.is_some(), "{stop:?}");
    }
}

#[test]
fn an_audit_of_a_source_set_and_a_repair_of_it_never_overlap() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let audit = env
        .store
        .enqueue_audit("Alpha", None, "Operator", WORKERS, AuditEvidence::Full)
        .unwrap();

    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains(&format!("audit job {}", audit.id))),
        "{refused:?}"
    );

    env.store.claim_job().unwrap().unwrap();
    env.store
        .run_audit_job(&env.config, audit.id, None)
        .unwrap();
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let queued = env
        .store
        .enqueue_audit("Alpha", None, "Operator", WORKERS, AuditEvidence::Full);
    let direct = env
        .store
        .audit_source_set(&env.config, "Alpha", None, AuditEvidence::Full);
    for refused in [queued.map(|_| ()), direct.map(|_| ())] {
        assert!(
            matches!(
                &refused,
                Err(Error::JobActive { job, kind: JobKind::Repair, .. }) if Some(*job) == approved.job
            ),
            "{refused:?}"
        );
    }
}

#[test]
fn a_finished_repair_queues_the_audit_it_owes_even_after_a_stop_right_after_it_finished() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.inject_fault(Some(Fault::RepairFinished));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let finished = env.store.repair_plan(plan.id).unwrap();
    assert!(finished.finished_at.is_some());
    assert!(finished.audit_owed, "{finished:?}");

    let owed = env.store.fulfil_repair_audits().unwrap();

    assert_eq!(owed.queued.len(), 1, "{owed:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    assert_eq!(done.audit_job, Some(owed.queued[0].id));
    env.run_queued_audit(&done);
}

#[test]
fn a_repair_you_cancel_finishes_the_item_it_started_and_skips_the_rest() {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let first_keep = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Keep { .. }))
        .unwrap();
    let started = plan.content.steps[first_keep].item.clone();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store
        .inject_fault(Some(Fault::RepairMoved(first_keep as u32)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();

    let cancelling = env.store.cancel_job(requeued.id).unwrap();
    assert!(cancelling.cancel_requested_at.is_some());
    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some());
    for outcome in &done.outcomes {
        match outcome.item == started {
            true => assert!(outcome.done, "{outcome:?}"),
            false => assert!(
                !outcome.done && outcome.error.as_deref().unwrap().contains("cancelled"),
                "{outcome:?}"
            ),
        }
    }
    assert_eq!(done.outcomes.len(), 2, "{:#?}", done.outcomes);
    let listing = env.listing();
    assert!(
        !listing
            .iter()
            .any(|p| p.contains("staging") || p.contains(".new")),
        "{listing:?}"
    );
    assert_eq!(
        listing.contains(&"Pong (USA).zip".to_owned()),
        started == "game:Pong (USA)",
        "{listing:?}"
    );
}

#[test]
fn a_repair_cancelled_after_a_shared_keep_finishes_every_item_that_keep_displaced_for() {
    let env = Env::new();
    write_zip(
        &env.set().join("Mixed.zip"),
        &[("pong.a26", b"pong"), ("Combat (USA).a26", b"cmbt")],
    );
    env.audit(AuditEvidence::Full);
    env.ingest("combat.a26", |p| write_file(p, b"combat"));
    let plan = env.plan();
    let keeps: Vec<usize> = plan
        .content
        .steps
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(&s.step, RepairStep::Keep { path, .. } if path == "Mixed.zip"))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(keeps.len(), 1, "{:#?}", plan.content.steps);
    assert_eq!(item(&plan, "game:Combat (USA)").kind, RepairKind::Replace);
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store
        .inject_fault(Some(Fault::RepairStepDone(keeps[0] as u32)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();

    env.store.cancel_job(requeued.id).unwrap();
    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    let done = env.store.repair_plan(plan.id).unwrap();
    for game in ["game:Pong (USA)", "game:Combat (USA)"] {
        let outcome = done.outcomes.iter().find(|o| o.item == game);
        assert!(
            outcome.is_some_and(|o| o.done),
            "{game}: {:#?}",
            done.outcomes
        );
    }
    let listing = env.listing();
    for game in ["Pong (USA).zip", "Combat (USA).zip"] {
        assert!(listing.contains(&game.to_owned()), "{listing:?}");
    }
}

/// Approves a plan to repack `Pong (Anthology).a26` as Pong and keep `readme.txt` aside, stopped once Pong's Build
/// staged its file, and cancels the job a restart queued again.
fn cancelled_after_a_build_staged(env: &Env) -> RepairPlan {
    env.put("Pong (Anthology).a26", b"pong");
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.inject_fault(Some(Fault::RepairStaged(0)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    env.store.cancel_job(requeued.id).unwrap();
    plan
}

fn assert_nothing_done(env: &Env, plan: &RepairPlan) {
    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some(), "{done:?}");
    assert!(
        done.outcomes.iter().all(|o| !o.done),
        "{:#?}",
        done.outcomes
    );
    assert_eq!(
        env.listing(),
        [".title-select", "Pong (Anthology).a26", "readme.txt"]
    );
}

#[test]
fn a_step_a_cancellation_skipped_never_commits_its_item() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    // A cancellation that skipped Pong's file being kept aside, but not yet the rest of Pong.
    let keep = plan
        .content
        .steps
        .iter()
        .position(|s| s.item == "game:Pong (USA)" && matches!(s.step, RepairStep::Keep { .. }))
        .unwrap();
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    db.execute(
        "UPDATE repair_steps SET state = 'skipped', error = 'cancelled' WHERE seq = ?1",
        [keep as i64],
    )
    .unwrap();

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_nothing_done(&env, &plan);
}

#[test]
fn a_cancellation_a_stop_cut_short_is_never_run_past_after_a_restart() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    env.store.inject_fault(Some(Fault::RepairCancelled));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&env.store.repair_plan(plan.id).unwrap())
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    assert!(requeued.cancel_requested_at.is_some(), "{requeued:?}");

    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_nothing_done(&env, &plan);
}

#[test]
fn a_staged_file_a_stop_left_being_deleted_is_deleted_after_a_restart() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    env.store.inject_fault(Some(Fault::RepairDiscarding(0)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&env.store.repair_plan(plan.id).unwrap())
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    env.store.recover_jobs(WORKERS).unwrap();

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_nothing_done(&env, &plan);
}

#[test]
fn a_file_swapped_in_for_a_staged_file_being_deleted_is_put_back_and_never_deleted() {
    // A file put in its place either gets a new inode number or, as on Linux, may reuse the one freed.
    for reusing_its_inode in [false, true] {
        swapped_in_for_a_staged_file_being_deleted(reusing_its_inode);
    }
}

fn swapped_in_for_a_staged_file_being_deleted(reusing_its_inode: bool) {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    env.store.inject_fault(Some(Fault::RepairDiscarding(0)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&env.store.repair_plan(plan.id).unwrap())
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    // What was moved to be deleted is, by the time the repair goes on, another file.
    let deleting = env.set().join(".title-select-retained/.deleting");
    let slot = std::fs::read_dir(&deleting)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let staged = std::fs::read_dir(env.set())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|n| n.contains("title-select-new"));
    assert!(staged.is_none(), "{staged:?}");
    if reusing_its_inode {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&slot)
            .unwrap();
        std::io::Write::write_all(&mut &file, b"someone else's").unwrap();
    } else {
        std::fs::remove_file(&slot).unwrap();
        std::fs::write(&slot, b"someone else's").unwrap();
    }
    env.store.recover_jobs(WORKERS).unwrap();

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert!(
        std::fs::read_dir(&deleting).unwrap().next().is_none(),
        "{:?}",
        env.listing()
    );
    let back = env
        .listing()
        .into_iter()
        .find(|n| n.contains("title-select-new"))
        .unwrap_or_else(|| panic!("{:?}", env.listing()));
    assert_eq!(
        std::fs::read(env.set().join(back)).unwrap(),
        b"someone else's"
    );
}

#[test]
fn a_staged_file_changed_since_it_was_written_is_kept_and_never_deleted() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    let staged = std::fs::read_dir(env.set())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("title-select-new"))
        .unwrap();
    // Written over in place, as a file put there looks when it reuses the freed inode number, as on Linux.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&staged)
        .unwrap();
    std::io::Write::write_all(&mut &file, b"someone else's").unwrap();
    drop(file);

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some(), "{done:#?}");
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        std::fs::read(
            env.set()
                .join(".title-select-retained/1")
                .join(&kept[0].kept_name)
        )
        .unwrap(),
        b"someone else's"
    );
    assert!(
        std::fs::read_dir(env.set().join(".title-select-retained/.deleting"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_staged_file_written_to_once_moved_to_be_deleted_is_kept_even_with_the_same_bytes() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    let deleting = env.set().join(".title-select-retained/.deleting");
    // Only its ctime tells: written through a descriptor with what it held, its mtime set back.
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::RepairDiscarding(_)) {
                let slot = std::fs::read_dir(&deleting)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let held = std::fs::read(&slot).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(20));
                rewrite_unnoticed(&slot, &held);
            }
        })));

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    env.store.hook_faults(None);

    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some(), "{done:#?}");
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
}

#[test]
fn a_cancelled_repair_whose_staged_file_could_not_be_deleted_stays_unfinished_until_it_is() {
    let env = Env::new();
    let plan = cancelled_after_a_build_staged(&env);
    // The kept files folder can't be made, so nothing can be moved to be deleted.
    let blocker = env.set().join(".title-select-retained");
    let at = blocker.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairCancelled {
                std::fs::write(&at, b"in the way").unwrap();
            }
        })));
    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    env.store.hook_faults(None);

    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert!(
        env.store
            .repair_plan(plan.id)
            .unwrap()
            .finished_at
            .is_none(),
        "a staged file was left untracked"
    );
    std::fs::remove_file(&blocker).unwrap();
    let requeued = env.store.recover_repairs().unwrap();
    assert_eq!(requeued.len(), 1, "{requeued:?}");
    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    assert_nothing_done(&env, &plan);
}

#[test]
fn a_new_file_kept_aside_is_recorded_as_title_selects_only_while_it_matches_what_was_written() {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let publish = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Publish { .. }))
        .unwrap() as u32;
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let target = env.set().join("Pong (USA).zip");
    let kept = env
        .set()
        .join(".title-select-retained/1/Pong (USA).zip.kept");
    // Once moved aside, the file under that inode is rewritten before the service, stopping, records it kept.
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairStaged(0) {
                std::fs::write(&target, b"someone else's").unwrap();
            } else if fault == Fault::RepairMoved(publish) {
                std::fs::write(&kept, b"not what was written").unwrap();
                panic!("stopped");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    env.store.recover_jobs(WORKERS).unwrap();

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert!(
        kept.iter()
            .all(|k| !k.reason.contains("could not be put in place")),
        "{kept:#?}"
    );
}

#[test]
fn a_file_found_in_place_of_a_new_file_being_kept_aside_is_recorded_kept_after_a_stop() {
    for at_staged_name in [false, true] {
        let env = Env::new();
        env.put("Pong (Anthology).a26", b"pong");
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        let publish = plan
            .content
            .steps
            .iter()
            .position(|s| matches!(s.step, RepairStep::Publish { .. }))
            .unwrap() as u32;
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let set = env.set();
        let kept = set.join(".title-select-retained/1/Pong (USA).zip.kept");
        let staged = std::sync::Arc::new(std::sync::Mutex::new(None));
        let found = staged.clone();
        let kept_path = kept.clone();
        // Once the new file is moved aside, another file takes its kept name, or its staged name, before a stop.
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::RepairStaged(0) {
                    std::fs::write(set.join("Pong (USA).zip"), b"someone else's").unwrap();
                    *found.lock().unwrap() = std::fs::read_dir(&set)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .find(|p| {
                            p.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".Pong (USA).zip.title-select-new-")
                        });
                } else if fault == Fault::RepairMoved(publish) {
                    if at_staged_name {
                        std::fs::remove_file(&kept).unwrap();
                        let staged = found.lock().unwrap().clone().unwrap();
                        std::fs::write(staged, b"an intruder").unwrap();
                    } else {
                        substitute(&kept, b"an intruder");
                    }
                    panic!("stopped");
                }
            })));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.run_repair_job(&approved)
        }));
        env.store.hook_faults(None);
        assert!(ran.is_err(), "{ran:?}");
        env.store.recover_jobs(WORKERS).unwrap();

        env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

        let done = env.store.repair_plan(plan.id).unwrap();
        assert!(done.finished_at.is_some(), "{:#?}", done.outcomes);
        let recorded = env.store.source_kept_files("Alpha").unwrap();
        let intruder = recorded
            .iter()
            .find(|k| k.reason.contains("found in place of"))
            .unwrap_or_else(|| panic!("at_staged_name {at_staged_name}: {recorded:#?}"));
        assert_eq!(intruder.kept_name, "Pong (USA).zip.kept");
        assert_eq!(std::fs::read(&kept_path).unwrap(), b"an intruder");
        assert!(staged.lock().unwrap().as_ref().is_some_and(|p| !p.exists()));
    }
}

#[test]
fn a_file_found_in_place_of_a_new_file_being_kept_aside_is_recorded_only_once_both_folders_are_synced()
 {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let publish = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Publish { .. }))
        .unwrap() as u32;
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let set = env.set();
    let kept = set.join(".title-select-retained/1/Pong (USA).zip.kept");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairStaged(0) {
                std::fs::write(set.join("Pong (USA).zip"), b"someone else's").unwrap();
            } else if fault == Fault::RepairMoved(publish) {
                substitute(&kept, b"an intruder");
                panic!("stopped");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    env.store.recover_jobs(WORKERS).unwrap();

    env.store
        .inject_fault(Some(Fault::RepairFoundSynced(publish)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&env.store.repair_plan(plan.id).unwrap())
    }));
    env.store.inject_fault(None);

    assert!(
        ran.is_err(),
        "recorded before both folders were synced: {ran:?}"
    );
    assert!(
        env.store
            .source_kept_files("Alpha")
            .unwrap()
            .iter()
            .all(|k| !k.reason.contains("found in place of"))
    );
    env.store.recover_jobs(WORKERS).unwrap();
    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    assert!(
        env.store
            .source_kept_files("Alpha")
            .unwrap()
            .iter()
            .any(|k| k.reason.contains("found in place of"))
    );
}

#[test]
fn a_new_file_kept_aside_because_its_place_was_taken_is_recorded_kept_after_a_stop() {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let publish = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Publish { .. }))
        .unwrap() as u32;
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    // Something takes Pong's place once its file is staged; the service stops once that file is moved aside.
    let target = env.set().join("Pong (USA).zip");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairStaged(0) {
                std::fs::write(&target, b"someone else's").unwrap();
            } else if fault == Fault::RepairMoved(publish) {
                panic!("stopped");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    env.store.recover_jobs(WORKERS).unwrap();

    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    let kept = env.store.source_kept_files("Alpha").unwrap();
    let new = kept
        .iter()
        .find(|k| k.kept_name == "Pong (USA).zip.kept")
        .unwrap_or_else(|| panic!("{kept:#?}"));
    assert!(new.reason.contains("could not be put in place"), "{new:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    let pong = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Pong (USA)")
        .unwrap();
    assert!(
        !pong.done
            && pong
                .error
                .as_deref()
                .is_some_and(|e| e.contains("kept aside as Pong (USA).zip.kept")),
        "{pong:?}"
    );
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        b"someone else's"
    );
    assert!(
        env.listing()
            .contains(&".title-select-retained/1/Pong (USA).zip.kept".to_owned())
    );
}

#[test]
fn a_repair_cancelled_before_it_ran_changes_nothing_not_even_the_marker() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let before = env.listing();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    env.store.cancel_job(approved.job.unwrap()).unwrap();
    let ended = env.run_repair_job(&approved);

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_eq!(env.listing(), before);
}

#[test]
fn a_kept_file_recorded_before_a_stop_is_accepted_when_its_step_runs_again() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let keep = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Keep { .. }))
        .unwrap();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store
        .inject_fault(Some(Fault::RepairStepDone(keep as u32)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    // As a stop between recording the kept file and ending its step would leave it.
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    db.execute(
        "UPDATE repair_steps SET state = 'moving', kept_id = NULL, finished_at = NULL WHERE seq = ?1",
        [keep as i64],
    )
    .unwrap();
    env.store.recover_jobs(WORKERS).unwrap();

    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(env.store.source_kept_files("Alpha").unwrap().len(), 1);
}

#[test]
fn a_first_marker_a_stop_left_unpublished_is_published_when_the_repair_runs_again() {
    for stop in [Fault::RepairMarkerWritten, Fault::RepairMarkerPublished] {
        let env = Env::new();
        env.put("readme.txt", b"hello");
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.inject_fault(Some(stop));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.run_repair_job(&approved)
        }));
        assert!(ran.is_err(), "{stop:?}: {ran:?}");
        env.store.inject_fault(None);
        env.store.recover_jobs(WORKERS).unwrap();

        let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

        assert_eq!(ended.state, JobState::Succeeded, "{stop:?}: {ended:?}");
        assert_eq!(
            env.listing(),
            [".title-select", ".title-select-retained/1/readme.txt.kept"],
            "{stop:?}"
        );
    }
}

#[test]
fn a_file_put_in_place_of_a_first_marker_a_stop_left_is_never_written_over() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.inject_fault(Some(Fault::RepairMarkerWritten));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    // Written over in place, as a file put there looks when it reuses the freed inode number, as on Linux.
    let temp = env.set().join(".title-select.new");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&temp)
        .unwrap();
    std::io::Write::write_all(&mut &file, b"someone else's").unwrap();
    drop(file);
    env.store.recover_jobs(WORKERS).unwrap();

    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());

    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert_eq!(std::fs::read(&temp).unwrap(), b"someone else's");
    assert!(!env.set().join(".title-select").exists());
    assert!(env.set().join("readme.txt").exists());
}

#[test]
fn a_file_put_in_place_of_the_first_marker_once_it_is_checked_is_never_written_over() {
    // Fresh, and left by a stop before it was written.
    for after_a_stop in [false, true] {
        let env = Env::new();
        env.put("readme.txt", b"hello");
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        let mut approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        if after_a_stop {
            env.store.inject_fault(Some(Fault::RepairMarkerChecked));
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.run_repair_job(&approved)
            }));
            assert!(ran.is_err(), "{ran:?}");
            env.store.inject_fault(None);
            env.store.recover_jobs(WORKERS).unwrap();
            approved = env.store.repair_plan(plan.id).unwrap();
        }
        let temp = env.set().join(".title-select.new");
        let swapped = temp.clone();
        // A file put in its place, which on Linux may get the inode number just freed.
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::RepairMarkerChecked {
                    std::fs::remove_file(&swapped).unwrap();
                    std::fs::write(&swapped, b"someone else's").unwrap();
                }
            })));

        let ended = env.run_repair_job(&approved);

        env.store.hook_faults(None);
        assert_eq!(ended.state, JobState::Failed, "{after_a_stop}: {ended:?}");
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"someone else's",
            "{after_a_stop}"
        );
        assert!(!env.set().join(".title-select").exists(), "{after_a_stop}");
        assert!(env.set().join("readme.txt").exists(), "{after_a_stop}");
    }
}

#[test]
fn a_marker_that_appears_while_the_first_one_is_written_is_never_replaced() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let marker = env.set().join(".title-select");
    let planted = marker.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairMarkerWritten {
                std::fs::write(&planted, b"someone else's").unwrap();
            }
        })));

    let ended = env.run_repair_job(&approved);

    env.store.hook_faults(None);
    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert!(
        ended.error.as_deref().unwrap().contains("marker appeared"),
        "{ended:?}"
    );
    assert_eq!(std::fs::read(&marker).unwrap(), b"someone else's");
    assert!(env.set().join("readme.txt").exists());
    let ended = env.store.repair_plan(plan.id).unwrap();
    assert!(
        ended.finished_at.is_some(),
        "a repair that never started ends: {ended:#?}"
    );
    assert!(
        ended
            .outcomes
            .iter()
            .all(|o| !o.done && o.error.as_deref().unwrap().contains("marker appeared")),
        "{:#?}",
        ended.outcomes
    );
    assert!(env.store.recover_jobs(WORKERS).unwrap().is_empty());
}

#[test]
fn an_unreadable_entry_is_replaced_only_while_it_is_as_the_audit_found_it() {
    let env = Env::new();
    env.put("Yars (USA).zip", b"PK\x03\x04 not really a zip");
    env.audit(AuditEvidence::Full);
    assert_eq!(
        env.latest_report().entries[0].finding,
        Finding::Unreadable,
        "{:?}",
        env.latest_report().entries
    );
    env.ingest("yars.a26", |p| write_file(p, b"yars"));
    assert_eq!(
        item(&env.plan(), "game:Yars (USA)").kind,
        RepairKind::Replace
    );

    env.put("Yars (USA).zip", b"PK\x03\x04 something else since");
    let plan = env.plan();

    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.key != "game:Yars (USA)"),
        "{:#?}",
        plan.content.items
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Yars (USA).zip changed since the audit")),
        "{:?}",
        plan.content.notes
    );
}

#[test]
fn a_game_filled_from_ingest_never_displaces_a_conflicting_entry_you_left() {
    let env = Env::new();
    write_zip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"pong")],
    );
    env.audit(AuditEvidence::Full);
    env.ingest("combat.a26", |p| write_file(p, b"combat"));

    let plan = env.plan();

    assert_eq!(
        item(&plan, "entry:Combat (USA).zip#Combat (USA).a26").chosen,
        RepairChoice::Leave
    );
    assert!(plan.content.steps.is_empty(), "{:#?}", plan.content.steps);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("conflicting entry you chose to leave")),
        "{:?}",
        plan.content.notes
    );

    let kept = env.plan_with(&[(
        "entry:Combat (USA).zip#Combat (USA).a26",
        RepairChoice::KeepAside,
    )]);
    assert_eq!(
        item(&kept, "game:Combat (USA)").kind,
        RepairKind::Fill,
        "{:#?}",
        kept.content.items
    );
}

/// Writes the TorrentZip of the single ROM `name` holding `payload` to `path`.
fn write_torrentzip(path: &Path, name: &str, payload: &[u8]) {
    let loose = path.with_extension("loose");
    write_file(&loose, payload);
    let rom = title_select_api::build::ExpectedRom {
        name: name.into(),
        size: payload.len() as u64,
        crc: None,
        md5: None,
        sha1: None,
        sha256: None,
        header: None,
    };
    let dest = std::fs::File::create_new(path).unwrap();
    let source = title_select_engine::build::Source {
        path: &loose,
        member: None,
    };
    title_select_engine::build::repack(&dest, &[(&rom, source)]).unwrap();
    std::fs::remove_file(&loose).unwrap();
}

#[test]
fn a_file_swapped_after_its_audit_is_left_alone_even_once_an_interrupted_audit_read_it() {
    let env = Env::new();
    write_torrentzip(&env.set().join("Wrong.zip"), "Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    assert_eq!(
        item(&env.plan(), "game:Pong (USA)").kind,
        RepairKind::Rename
    );
    std::fs::remove_file(env.set().join("Wrong.zip")).unwrap();
    write_torrentzip(&env.set().join("Wrong.zip"), "Combat (USA).a26", b"combat");
    // An Audit that reads the swapped file, and stops before it finishes.
    let job = env
        .store
        .enqueue_audit("Alpha", None, "Operator", WORKERS, AuditEvidence::Full)
        .unwrap();
    env.store.claim_job().unwrap();
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let progress = |p: title_select_engine::Progress| {
            if p.files_done == 1 {
                panic!("stopped");
            }
        };
        env.store
            .run_audit_job(&env.config, job.id, Some(&progress))
    }));
    assert!(stopped.is_err());
    env.store.recover_jobs(WORKERS).unwrap();

    let plan = env.plan();

    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.key != "game:Pong (USA)"),
        "{:#?}",
        plan.content.items
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Wrong.zip changed since the audit")),
        "{:?}",
        plan.content.notes
    );
}

/// Approves `plan` and runs its Repair Job with `swap` called at every fault point, returning the plan as it ended.
fn run_with_hook(
    env: &Env,
    plan: &RepairPlan,
    swap: impl Fn(Fault) + Send + Sync + 'static,
) -> RepairPlan {
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.hook_faults(Some(std::sync::Arc::new(swap)));
    env.run_repair_job(&approved);
    env.store.hook_faults(None);
    env.store.repair_plan(plan.id).unwrap()
}

/// Replaces the file at `path` with another holding `bytes`, as something else renaming one in its place would.
fn substitute(path: &Path, bytes: &[u8]) {
    std::fs::remove_file(path).unwrap();
    write_file(path, bytes);
}

#[test]
fn a_file_moved_in_place_of_one_being_kept_aside_is_recorded_kept_under_its_own_identity() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let kept = env.set().join(".title-select-retained/1/readme.txt.kept");

    let ended = run_with_hook(&env, &plan, move |fault| {
        if matches!(fault, Fault::RepairMoved(_)) {
            substitute(&kept, b"intruder");
        }
    });

    assert!(
        ended.outcomes.iter().all(|o| !o.done),
        "{:#?}",
        ended.outcomes
    );
    let recorded = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert!(recorded[0].reason.contains("found in place of readme.txt"));
    let at = std::fs::metadata(env.set().join(".title-select-retained/1/readme.txt.kept")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        (recorded[0].identity.dev, recorded[0].identity.ino),
        (at.dev(), at.ino())
    );
}

#[test]
fn a_file_moved_in_place_of_one_being_renamed_is_put_back_under_its_old_name() {
    let env = Env::new();
    write_torrentzip(&env.set().join("Wrong.zip"), "Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    assert_eq!(item(&plan, "game:Pong (USA)").kind, RepairKind::Rename);
    let renamed = env.set().join("Pong (USA).zip");

    let ended = run_with_hook(&env, &plan, move |fault| {
        if matches!(fault, Fault::RepairMoved(_)) && renamed.exists() {
            substitute(&renamed, b"intruder");
        }
    });

    assert!(
        ended.outcomes.iter().all(|o| !o.done),
        "{:#?}",
        ended.outcomes
    );
    assert_eq!(env.listing(), [".title-select", "Wrong.zip"]);
    assert_eq!(
        std::fs::read(env.set().join("Wrong.zip")).unwrap(),
        b"intruder"
    );
}

#[test]
fn a_file_published_in_place_of_the_first_marker_is_moved_back_and_never_left_as_the_marker() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let marker = env.set().join(".title-select");

    let ended = run_with_hook(&env, &plan, move |fault| {
        if fault == Fault::RepairMarkerPublished {
            substitute(&marker, b"intruder");
        }
    });

    assert!(ended.finished_at.is_none() || ended.outcomes.iter().all(|o| !o.done));
    assert!(!env.set().join(".title-select").exists());
    assert_eq!(
        std::fs::read(env.set().join(".title-select.new")).unwrap(),
        b"intruder"
    );
    assert!(
        env.set().join("readme.txt").exists(),
        "nothing else changed"
    );
}

#[test]
fn a_file_moved_in_place_of_an_ingest_original_being_kept_is_recorded_kept_under_its_own_identity()
{
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("combat.zip", |p| {
        write_zip(p, &[("Combat (USA).a26", b"combat")])
    });
    let plan = env.plan();
    let ingest = env.tmp.path().join("in");

    let ended = run_with_hook(&env, &plan, move |fault| {
        if fault == Fault::IngestSetAside {
            let retained = ingest.join(".title-select-retained");
            for generation in std::fs::read_dir(&retained).unwrap() {
                let kept = generation.unwrap().path().join("combat.zip.kept");
                if kept.exists() {
                    substitute(&kept, b"intruder");
                }
            }
        }
    });

    assert!(ended.finished_at.is_some(), "{ended:#?}");
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        kept[0].reason.contains("found in place of combat.zip"),
        "{kept:?}"
    );
    assert_eq!(kept[0].file.size, b"intruder".len() as u64);
}

#[test]
fn a_repair_uses_the_stamps_its_own_audit_read_even_when_an_overlapping_audit_read_the_file_since()
{
    let env = Env::new();
    write_torrentzip(&env.set().join("Wrong.zip"), "Pong (USA).a26", b"pong");
    let job = env
        .store
        .enqueue_audit("Alpha", None, "Operator", WORKERS, AuditEvidence::Full)
        .unwrap();
    env.store.claim_job().unwrap();
    // Once this Audit has read Wrong.zip as Pong, the file is swapped and another Audit reads and finishes first.
    let progress = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            std::fs::remove_file(env.set().join("Wrong.zip")).unwrap();
            write_torrentzip(&env.set().join("Wrong.zip"), "Combat (USA).a26", b"combat");
            env.audit(AuditEvidence::Full);
        }
    };
    let ended = env
        .store
        .run_audit_job(&env.config, job.id, Some(&progress))
        .unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert_eq!(
        env.latest_report().entries[0].roms[0].game,
        "Pong (USA)",
        "the latest audit is the one that read Pong"
    );

    let plan = env.plan();

    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.key != "game:Pong (USA)"),
        "{:#?}",
        plan.content.items
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Wrong.zip changed since the audit")),
        "{:?}",
        plan.content.notes
    );
}

#[test]
fn a_file_moved_in_place_of_a_published_fill_ends_the_fill_and_leaves_the_ingest_original_in_place()
{
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("combat.zip", |p| {
        write_zip(p, &[("Combat (USA).a26", b"combat")])
    });
    let plan = env.plan();
    let published = env.set().join("Combat (USA).zip");

    let ended = run_with_hook(&env, &plan, move |fault| {
        if fault == Fault::IngestRenamed {
            substitute(&published, b"intruder");
        }
    });

    assert!(ended.finished_at.is_some(), "{ended:#?}");
    let fill = ended
        .outcomes
        .iter()
        .find(|o| o.item == "game:Combat (USA)")
        .unwrap();
    assert!(!fill.done, "{fill:?}");
    assert_eq!(
        std::fs::read(env.set().join("Combat (USA).zip")).unwrap(),
        b"intruder"
    );
    assert_eq!(env.ingest_listing(), ["combat.zip"]);
    assert!(env.store.ingest_kept_files("in").unwrap().is_empty());
}

#[test]
fn a_repair_planned_from_an_incremental_audit_trusts_the_reads_that_audit_reused() {
    let env = Env::new();
    write_torrentzip(&env.set().join("Wrong.zip"), "Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    env.audit(AuditEvidence::Incremental);
    assert_eq!(env.latest_report().evidence, AuditEvidence::Incremental);

    let plan = env.plan();

    assert_eq!(
        item(&plan, "game:Pong (USA)").kind,
        RepairKind::Rename,
        "{:?}",
        plan.content.notes
    );
}

/// Approves `plan`, stops its Repair Job at `fault`, runs `meanwhile`, then recovers and runs the job again.
fn stop_and_resume(
    env: &Env,
    plan: &RepairPlan,
    fault: Fault,
    meanwhile: impl FnOnce(),
) -> RepairPlan {
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.inject_fault(Some(fault));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    meanwhile();
    env.store.recover_jobs(WORKERS).unwrap();
    env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    env.store.repair_plan(plan.id).unwrap()
}

#[test]
fn a_published_fill_replaced_before_its_original_is_disposed_of_ends_the_fill_once_recovered() {
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("combat.zip", |p| {
        write_zip(p, &[("Combat (USA).a26", b"combat")])
    });
    let plan = env.plan();
    let published = env.set().join("Combat (USA).zip");

    let ended = stop_and_resume(&env, &plan, Fault::IngestPublished, || {
        substitute(&published, b"intruder")
    });

    assert!(ended.finished_at.is_some(), "{ended:#?}");
    let fill = ended
        .outcomes
        .iter()
        .find(|o| o.item == "game:Combat (USA)")
        .unwrap();
    assert!(
        !fill.done
            && fill
                .error
                .as_deref()
                .unwrap()
                .contains("no longer the copy"),
        "{fill:?}"
    );
    assert_eq!(std::fs::read(&published).unwrap(), b"intruder");
    assert_eq!(env.ingest_listing(), ["combat.zip"]);
}

fn set_source_policy(env: &Env, format: StorageFormat, delete_originals: bool) {
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = format;
    policy.source_sets.delete_originals = delete_originals;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

/// The sequence number of the first step of `plan` that `which` picks.
fn step_seq(plan: &RepairPlan, which: impl Fn(&RepairStep) -> bool) -> u32 {
    plan.content
        .steps
        .iter()
        .position(|s| which(&s.step))
        .unwrap_or_else(|| panic!("no such step in {:#?}", plan.content.steps)) as u32
}

#[test]
fn a_source_set_is_converted_to_loose_and_back_with_each_original_kept() {
    let env = Env::new();
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let to_zip = env.plan();
    assert_eq!(item(&to_zip, "game:Pong (USA)").kind, RepairKind::Convert);
    let zipped = env.approve(&to_zip);
    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Pong (USA).a26.kept",
            "Pong (USA).zip"
        ]
    );
    assert!(is_torrentzip(&env.set().join("Pong (USA).zip")));

    env.run_queued_audit(&zipped);
    set_source_policy(&env, StorageFormat::Loose, false);
    let to_loose = env.plan();
    let pong = item(&to_loose, "game:Pong (USA)");
    assert_eq!(pong.kind, RepairKind::Convert);
    assert!(pong.summary.contains("loose"), "{}", pong.summary);
    let adventure_note = to_loose
        .content
        .notes
        .iter()
        .any(|n| n.contains("more than one ROM"));
    assert!(adventure_note, "{:?}", to_loose.content.notes);
    let done = env.approve(&to_loose);
    let listing = env.listing();
    assert!(
        listing.contains(&"Pong (USA).a26".to_string()),
        "{listing:?}"
    );
    assert!(
        !listing.contains(&"Pong (USA).zip".to_string()),
        "{listing:?}"
    );
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).a26")).unwrap(),
        b"pong"
    );
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.iter().any(|k| k.name == "Pong (USA).zip"), "{kept:?}");

    env.run_queued_audit(&done);
    let report = env.latest_report();
    let pong = report.games.iter().find(|g| g.name == "Pong (USA)");
    assert_eq!(pong.unwrap().status, GameStatus::Complete);
    assert!(
        env.plan().content.items.is_empty(),
        "nothing more to convert"
    );
}

#[test]
fn a_game_with_more_than_one_rom_is_never_stored_loose() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::Loose, false);
    env.put("Adventure (USA).a26", b"adventure");
    env.put("Adventure (USA).txt", b"manual");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let adventure = item(&plan, "game:Adventure (USA)");
    assert_ne!(adventure.kind, RepairKind::Convert);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("more than one ROM") && n.contains("TorrentZip")),
        "{:?}",
        plan.content.notes
    );
    env.approve(&plan);
    assert!(is_torrentzip(&env.set().join("Adventure (USA).zip")));
}

#[test]
fn a_systems_own_format_beats_the_default() {
    let env = Env::new();
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = StorageFormat::Loose;
    policy.source_sets.systems.insert(
        "Test/Test - System".into(),
        SystemStorage {
            format: Some(StorageFormat::TorrentZip),
            delete_originals: None,
            ..Default::default()
        },
    );
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    write_zip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    env.put("Yars (USA).a26", b"yars");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    assert_eq!(item(&plan, "game:Yars (USA)").kind, RepairKind::Convert);
    assert_eq!(item(&plan, "game:Pong (USA)").kind, RepairKind::Repack);
    env.approve(&plan);
    assert!(is_torrentzip(&env.set().join("Yars (USA).zip")));
    assert!(is_torrentzip(&env.set().join("Pong (USA).zip")));
}

#[test]
fn a_plan_made_before_the_storage_policy_changed_is_refused() {
    let env = Env::new();
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    set_source_policy(&env, StorageFormat::Loose, false);

    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);

    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("storage policy")),
        "{refused:?}"
    );
}

#[test]
fn with_delete_originals_on_an_original_is_deleted_once_its_converted_file_verifies() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::TorrentZip, true);
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    assert!(
        plan.content
            .steps
            .iter()
            .any(|s| matches!(s.step, RepairStep::Delete { .. })),
        "{:#?}",
        plan.content.steps
    );
    let done = env.approve(&plan);

    assert_eq!(env.listing(), [".title-select", "Pong (USA).zip"]);
    assert!(env.store.source_kept_files("Alpha").unwrap().is_empty());
    let pong = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Pong (USA)")
        .unwrap();
    assert!(
        pong.detail.iter().any(|d| d.contains("was deleted")),
        "{pong:?}"
    );
}

#[test]
fn an_original_that_changed_after_it_was_converted_is_kept() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::TorrentZip, true);
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let delete = step_seq(&plan, |s| matches!(s, RepairStep::Delete { .. }));
    let kept = env
        .set()
        .join(".title-select-retained/1/Pong (USA).a26.kept");
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::ConversionVerified(delete) {
            write_file(&kept, b"pong, changed");
        }
    })));

    let done = env.approve(&plan);

    let listing = env.listing();
    assert!(
        listing.contains(&".title-select-retained/1/Pong (USA).a26.kept".to_string()),
        "{listing:?}"
    );
    assert_eq!(env.store.source_kept_files("Alpha").unwrap().len(), 1);
    let pong = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Pong (USA)")
        .unwrap();
    assert!(
        pong.detail
            .iter()
            .any(|d| d.contains("stays in the kept files")),
        "{pong:?}"
    );
}

#[test]
fn an_original_whose_converted_file_does_not_verify_is_kept() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::TorrentZip, true);
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let delete = step_seq(&plan, |s| matches!(s, RepairStep::Delete { .. }));
    let target = env.set().join("Pong (USA).zip");
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::RepairStepDone(delete - 1) {
            write_file(&target, b"not a zip");
        }
    })));

    let done = env.approve(&plan);

    let listing = env.listing();
    assert!(
        listing.contains(&".title-select-retained/1/Pong (USA).a26.kept".to_string()),
        "{listing:?}"
    );
    let pong = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Pong (USA)")
        .unwrap();
    assert!(
        pong.detail.iter().any(|d| d.contains("did not verify")),
        "{pong:?}"
    );
}

#[test]
fn a_conversion_the_service_stopped_in_ends_with_its_original_deleted_or_kept() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::TorrentZip, true);
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let delete = step_seq(&plan, |s| matches!(s, RepairStep::Delete { .. }));
    let stops = [
        Fault::ConversionVerified(delete),
        Fault::SourceKeptSynced(1),
        Fault::SourceKeptMoved(1),
        Fault::RepairStepDone(delete),
    ];
    drop(env);
    for stop in stops {
        let env = Env::new();
        set_source_policy(&env, StorageFormat::TorrentZip, true);
        env.put("Pong (USA).a26", b"pong");
        env.audit(AuditEvidence::Full);
        let plan = env.plan();
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.inject_fault(Some(stop));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.run_repair_job(&approved)
        }));
        assert!(ran.is_err(), "{stop:?}: {ran:?}");
        env.store.inject_fault(None);

        let recovered = env.store.recover_jobs(WORKERS).unwrap();
        if let Some(requeued) = recovered.first().and_then(|r| r.requeued.clone()) {
            assert_eq!(requeued.repair, Some(plan.id), "{stop:?}");
            let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
            assert_eq!(ended.state, JobState::Succeeded, "{stop:?}: {ended:?}");
        }
        env.store.fulfil_repair_audits().unwrap();
        let done = env.store.repair_plan(plan.id).unwrap();
        assert!(done.finished_at.is_some(), "{stop:?}");
        assert!(done.outcomes.iter().all(|o| o.done), "{stop:?}: {done:#?}");
        assert_eq!(
            env.listing(),
            [".title-select", "Pong (USA).zip"],
            "{stop:?}"
        );
        assert!(
            env.store.source_kept_files("Alpha").unwrap().is_empty(),
            "{stop:?}"
        );
    }
}

#[test]
fn a_conversion_whose_new_file_finds_its_name_taken_leaves_the_original_in_place() {
    let env = Env::new();
    env.put("Pong (USA).a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    let build = step_seq(&plan, |s| matches!(s, RepairStep::Build { .. }));
    let publish = step_seq(&plan, |s| matches!(s, RepairStep::Publish { .. }));
    let keep = step_seq(&plan, |s| matches!(s, RepairStep::Keep { .. }));
    assert!(publish < keep, "the new file is put in place first");
    let target = env.set().join("Pong (USA).zip");
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::RepairStaged(build) {
            write_file(&target, b"someone else's");
        }
    })));

    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.run_repair_job(&approved);

    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).a26")).ok(),
        Some(b"pong".to_vec()),
        "the original stays where it was: {:?}",
        env.listing()
    );
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        b"someone else's"
    );
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert!(kept.iter().all(|k| k.name != "Pong (USA).a26"), "{kept:?}");
}

#[test]
fn a_conversion_keeps_an_approved_occupant_of_its_new_name_aside_before_it_puts_the_new_file_in_place()
 {
    let env = Env::new();
    env.put("Pong (USA).a26", b"pong");
    write_zip(&env.set().join("Pong (USA).zip"), &[("notes.txt", b"mine")]);
    env.audit(AuditEvidence::Full);
    let plan = env.plan();
    assert_eq!(item(&plan, "game:Pong (USA)").kind, RepairKind::Convert);
    let publish = step_seq(&plan, |s| matches!(s, RepairStep::Publish { .. }));
    let occupant = step_seq(
        &plan,
        |s| matches!(s, RepairStep::Keep { path, .. } if path == "Pong (USA).zip"),
    );
    let original = step_seq(
        &plan,
        |s| matches!(s, RepairStep::Keep { path, .. } if path == "Pong (USA).a26"),
    );
    assert!(
        occupant < publish && publish < original,
        "{:#?}",
        plan.content.steps
    );

    env.approve(&plan);

    assert_eq!(
        env.listing(),
        [
            ".title-select",
            ".title-select-retained/1/Pong (USA).a26.kept",
            ".title-select-retained/1/Pong (USA).zip.kept",
            "Pong (USA).zip"
        ]
    );
    assert!(is_torrentzip(&env.set().join("Pong (USA).zip")));
    let kept: std::collections::BTreeSet<String> = env
        .store
        .source_kept_files("Alpha")
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(
        kept,
        std::collections::BTreeSet::from(["Pong (USA).a26".into(), "Pong (USA).zip".into()])
    );
}

#[test]
fn an_ingest_fill_into_a_loose_source_set_is_stored_loose() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::Loose, false);
    env.audit(AuditEvidence::Full);
    env.ingest("pong.a26", |p| write_file(p, b"pong"));
    env.ingest("combat.zip", |p| {
        write_zip(p, &[("Combat (USA).a26", b"combat")])
    });

    let plan = env.plan();
    assert_eq!(item(&plan, "game:Pong (USA)").kind, RepairKind::Fill);
    assert_eq!(item(&plan, "game:Combat (USA)").kind, RepairKind::Fill);
    let done = env.approve(&plan);

    assert_eq!(
        env.listing(),
        [".title-select", "Combat (USA).a26", "Pong (USA).a26"]
    );
    assert_eq!(
        std::fs::read(env.set().join("Combat (USA).a26")).unwrap(),
        b"combat"
    );
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1, "the zip is kept; the loose file was moved");
    assert_eq!(kept[0].name, "combat.zip");
    env.run_queued_audit(&done);
    let report = env.latest_report();
    for game in ["Pong (USA)", "Combat (USA)"] {
        let found = report.games.iter().find(|g| g.name == game);
        assert_eq!(found.unwrap().status, GameStatus::Complete, "{game}");
    }
}

#[test]
fn a_source_sets_summary_shows_its_latest_audit_and_the_repair_open_on_it() {
    let env = Env::new();
    env.put("Pong (Anthology).a26", b"pong");
    let summary = |env: &Env| {
        env.store
            .source_set_summaries()
            .unwrap()
            .into_iter()
            .find(|s| s.set.name == "Alpha")
            .unwrap()
    };
    assert_eq!(summary(&env).latest, None, "never audited");
    env.audit(AuditEvidence::Full);
    let audited = summary(&env);
    let latest = audited.latest.expect("its audit").id;
    assert_eq!(audited.repair, None);

    let older = env.plan();
    let plan = env.plan();
    let open = summary(&env).repair.expect("a proposed repair is open");
    assert_eq!(
        (open.id, open.audit, open.approved),
        (plan.id, latest, false)
    );

    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let open = summary(&env).repair.expect("an approved repair is open");
    assert_eq!(
        (open.id, open.approved, open.job),
        (plan.id, true, approved.job)
    );
    assert_ne!(open.id, older.id);

    env.run_repair_job(&approved);
    env.store.fulfil_repair_audits().unwrap();
    assert_eq!(
        summary(&env).repair,
        None,
        "a finished repair isn't open, and neither is the plan it was approved over"
    );
}

#[test]
fn a_stat_walk_never_holds_a_source_set_against_a_repair_and_waits_for_it() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let walk = env.store.enqueue_stat_walk("Alpha", "Operator").unwrap();
    let plan = env.plan();

    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let repair = env.store.claim_job().unwrap().unwrap();
    assert_eq!(Some(repair.id), approved.job, "the repair goes first");
    assert_eq!(
        env.store.claim_job().unwrap(),
        None,
        "the walk waits while the repair runs"
    );
    env.store
        .run_repair_job(&env.config, repair.id, None)
        .unwrap();
    env.store.fulfil_repair_audits().unwrap();
    let audit = env.store.claim_job().unwrap().unwrap();
    assert_eq!(
        audit.kind,
        JobKind::Audit,
        "the audit the repair owes first"
    );
    env.store
        .run_audit_job(&env.config, audit.id, None)
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, walk.id);
}

#[test]
fn a_running_stat_walk_stops_for_a_repair_and_records_nothing() {
    let env = Env::new();
    env.put("readme.txt", b"hello");
    env.audit(AuditEvidence::Full);
    let walk = env.store.enqueue_stat_walk("Alpha", "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    let plan = env.plan();
    env.store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.put("new.bin", b"new");

    let ended = env.store.run_stat_walk_job(&env.config, walk.id).unwrap();

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert!(ended.error.unwrap().contains("repair or build"));
    assert_eq!(env.store.stat_walks("Alpha").unwrap(), []);
    assert_eq!(env.store.source_set("Alpha").unwrap().stale, []);
}

/// Runs `call`, and `act` on another thread once `call` reaches `fault`. `call` goes on once `act` returns, or after a
/// while should `act` wait for `call`.
fn race<A: Send, B: Send>(
    store: &Store,
    fault: Fault,
    call: impl FnOnce() -> A + Send,
    act: impl FnOnce() -> B + Send,
) -> (A, B) {
    let (reached, wait) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel::<()>();
    let (reached, resumed) = (
        std::sync::Mutex::new(reached),
        std::sync::Mutex::new(resumed),
    );
    store.hook_faults(Some(std::sync::Arc::new(move |f| {
        if f == fault {
            let _ = reached.lock().unwrap().send(());
            let _ = resumed
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_millis(500));
        }
    })));
    let out = std::thread::scope(|s| {
        let calling = s.spawn(call);
        wait.recv().unwrap();
        let acted = s.spawn(act).join().unwrap();
        let _ = resume.send(());
        (calling.join().unwrap(), acted)
    });
    store.hook_faults(None);
    out
}

#[test]
fn an_approved_fill_holds_its_ingest_folder_against_unregistering() {
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("pong.a26", |p| write_file(p, b"pong"));
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    let refused = env.store.unregister_ingest_folder(&env.config, "in");
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("repair")),
        "{refused:?}"
    );
    let ended = env.run_repair_job(&approved);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert!(env.ingest_listing().is_empty());
}

#[test]
fn a_fill_is_refused_when_its_ingest_folder_is_registered_anew_while_the_repair_is_being_approved()
{
    let env = Env::new();
    env.audit(AuditEvidence::Full);
    env.ingest("pong.a26", |p| write_file(p, b"pong"));
    let plan = env.plan();

    let (approved, ()) = race(
        &env.store,
        Fault::RepairApproving,
        || {
            env.store
                .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        },
        || {
            env.store
                .unregister_ingest_folder(&env.config, "in")
                .unwrap();
            env.store.register_ingest_folder(&env.config, "in").unwrap();
        },
    );

    assert!(
        matches!(&approved, Err(Error::Conflict(why)) if why.contains("unregistered")),
        "{approved:?}"
    );
    assert!(env.store.claim_job().unwrap().is_none(), "nothing queued");
    assert_eq!(env.ingest_listing(), ["pong.a26"]);
}

const DISC_SYSTEM: &str = "Redump/Test - System";

/// A Redump DAT of disc games: one of two tracks, a game on two discs, and a disc with no tracks of its own.
fn disc_games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Quest (USA)", disc("Quest (USA)", 3, 70_000, 1)),
        (
            "Saga (USA) (Disc 1)",
            disc("Saga (USA) (Disc 1)", 1, 9_000, 20),
        ),
        (
            "Saga (USA) (Disc 2)",
            disc("Saga (USA) (Disc 2)", 1, 9_000, 30),
        ),
    ]
}

fn disc_of(game: &str) -> Vec<RomSpec> {
    disc_games()
        .into_iter()
        .find(|(name, _)| *name == game)
        .unwrap()
        .1
}

/// Sets the storage policy's format for the disc system's Source Sets.
fn set_disc_policy(env: &Env, format: StorageFormat, delete_originals: bool) {
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.systems.insert(
        DISC_SYSTEM.into(),
        SystemStorage {
            format: Some(format),
            delete_originals: Some(delete_originals),
            ..Default::default()
        },
    );
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

fn status_of(env: &Env, game: &str) -> GameStatus {
    env.latest_report()
        .games
        .iter()
        .find(|g| g.name == game)
        .unwrap_or_else(|| panic!("no game {game}"))
        .status
}

#[test]
fn a_disc_systems_loose_tracks_are_moved_into_loose_folders_by_default() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    for r in &quest {
        env.put(r.name, &r.payload);
    }
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let item = item(&plan, "game:Quest (USA)");
    assert_eq!(item.kind, RepairKind::Rename, "{item:?}");
    assert!(
        item.summary.contains("into Quest (USA)/"),
        "{}",
        item.summary
    );
    let done = env.approve(&plan);

    let mut wanted = vec![".title-select".to_string()];
    wanted.extend(quest.iter().map(|r| format!("Quest (USA)/{}", r.name)));
    wanted.sort();
    assert_eq!(env.listing(), wanted);
    for r in &quest {
        assert_eq!(
            std::fs::read(env.set().join("Quest (USA)").join(r.name)).unwrap(),
            r.payload
        );
    }
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
    assert!(
        !env.plan()
            .content
            .items
            .iter()
            .any(|i| i.key == "game:Quest (USA)"),
        "a loose folder as the DAT names it is left as it is"
    );
}

#[test]
fn a_loose_folder_converts_to_torrentzip_and_back_deleting_each_original_once_verified() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest);
    env.audit(AuditEvidence::Full);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
    set_disc_policy(&env, StorageFormat::TorrentZip, true);

    let to_zip = env.plan();
    let item_zip = item(&to_zip, "game:Quest (USA)");
    assert_eq!(item_zip.kind, RepairKind::Convert, "{item_zip:?}");
    assert!(
        item_zip.summary.contains("Quest (USA)/") && item_zip.summary.contains("TorrentZip"),
        "{}",
        item_zip.summary
    );
    let deletes = to_zip
        .content
        .steps
        .iter()
        .filter(|s| matches!(s.step, RepairStep::Delete { .. }))
        .count();
    assert_eq!(deletes, quest.len(), "{:#?}", to_zip.content.steps);
    let zipped = env.approve(&to_zip);
    assert_eq!(env.listing(), [".title-select", "Quest (USA).zip"]);
    assert!(is_torrentzip(&env.set().join("Quest (USA).zip")));
    assert!(env.store.source_kept_files("Alpha").unwrap().is_empty());
    env.run_queued_audit(&zipped);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);

    set_disc_policy(&env, StorageFormat::LooseFolder, true);
    let to_folder = env.plan();
    let item_folder = item(&to_folder, "game:Quest (USA)");
    assert_eq!(item_folder.kind, RepairKind::Convert, "{item_folder:?}");
    assert!(
        item_folder.summary.contains("loose folder"),
        "{}",
        item_folder.summary
    );
    // Every track is extracted in one pass over the ZIP: when the first is staged, every other is too.
    let build = step_seq(&to_folder, |s| matches!(s, RepairStep::Build { .. }));
    let set = env.set();
    let staged_together = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = staged_together.clone();
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::RepairStaged(build) {
            let staged = Env::files(&set)
                .iter()
                .filter(|p| p.contains(".title-select-new-"))
                .count();
            *seen.lock().unwrap() = Some(staged);
        }
    })));
    let foldered = env.approve(&to_folder);
    env.store.hook_faults(None);
    assert_eq!(*staged_together.lock().unwrap(), Some(quest.len()));
    let mut wanted = vec![".title-select".to_string()];
    wanted.extend(quest.iter().map(|r| format!("Quest (USA)/{}", r.name)));
    wanted.sort();
    assert_eq!(env.listing(), wanted);
    let outcome = foldered
        .outcomes
        .iter()
        .find(|o| o.item == "game:Quest (USA)")
        .unwrap();
    assert!(
        outcome.detail.iter().any(|d| d.contains("was deleted")),
        "{outcome:?}"
    );
    env.run_queued_audit(&foldered);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn a_conversion_stopped_once_its_tracks_were_extracted_together_finishes_them_all() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest);
    env.audit(AuditEvidence::Full);
    set_disc_policy(&env, StorageFormat::TorrentZip, true);
    let zipped = env.approve(&env.plan());
    env.run_queued_audit(&zipped);
    set_disc_policy(&env, StorageFormat::LooseFolder, true);
    let plan = env.plan();
    let build = step_seq(&plan, |s| matches!(s, RepairStep::Build { .. }));
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.inject_fault(Some(Fault::RepairStaged(build)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    assert_eq!(ended.id, requeued.id);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");

    // The tracks staged with the first are checked and put in place. The ZIP's one read was recorded with the
    // finished first track, so it is still trusted, and the original goes once the folder verifies.
    for r in &quest {
        assert_eq!(
            std::fs::read(env.set().join("Quest (USA)").join(r.name)).unwrap(),
            r.payload
        );
    }
    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(
        done.outcomes[0]
            .detail
            .iter()
            .any(|d| d.contains("was deleted once its converted file was verified")),
        "{:#?}",
        done.outcomes
    );
}

#[test]
fn every_disc_of_a_game_stays_torrentzip_when_one_cannot_be_a_loose_folder() {
    let mut games = disc_games();
    // A ROM named under a folder of its own can't be a file directly in a loose folder.
    games[2].1[1].name = "tracks/Saga (USA) (Disc 2).bin";
    let env = Env::with("Redump", &games);
    write_games_zipped(
        &env,
        &games,
        &["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"],
    );
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    for game in ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"] {
        let saga = item(&plan, &format!("game:{game}"));
        assert_eq!(
            saga.path.as_deref(),
            Some(format!("{game}.zip").as_str()),
            "{saga:?}"
        );
    }
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Saga (USA) (Disc 1)") && n.contains("another disc")),
        "{:?}",
        plan.content.notes
    );
}

/// Writes each game of `games` named `names` as a ZIP of its ROMs, as the DAT names them.
fn write_games_zipped(env: &Env, games: &[(&str, Vec<RomSpec>)], names: &[&str]) {
    for (game, roms) in games.iter().filter(|(g, _)| names.contains(g)) {
        let members: Vec<(&str, &[u8])> = roms
            .iter()
            .map(|r| (r.name, r.payload.as_slice()))
            .collect();
        write_zip(&env.set().join(format!("{game}.zip")), &members);
    }
}

#[test]
fn leaving_one_disc_of_a_game_leaves_every_disc_and_a_split_choice_is_refused() {
    let games = disc_games();
    let env = Env::with("Redump", &games);
    let discs = ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"];
    write_games_zipped(&env, &games, &discs);
    env.audit(AuditEvidence::Full);
    let both = env.plan();
    for disc in discs {
        let saga = item(&both, &format!("game:{disc}"));
        assert_eq!(saga.chosen, RepairChoice::Apply, "{saga:?}");
        assert_eq!(saga.path.as_deref(), Some(disc));
    }

    let left = env.plan_with(&[("game:Saga (USA) (Disc 2)", RepairChoice::Leave)]);
    let first = item(&left, "game:Saga (USA) (Disc 1)");
    assert_eq!(first.chosen, RepairChoice::Leave, "{first:?}");
    assert!(
        first
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Saga (USA) (Disc 2)") && n.contains("every disc")),
        "{first:?}"
    );
    assert!(
        !left
            .content
            .steps
            .iter()
            .any(|s| s.item.starts_with("game:Saga")),
        "{:#?}",
        left.content.steps
    );

    let request = PlanRepair {
        choices: [
            ("game:Saga (USA) (Disc 1)".to_string(), RepairChoice::Apply),
            ("game:Saga (USA) (Disc 2)".to_string(), RepairChoice::Leave),
        ]
        .into(),
        additions: Default::default(),
    };
    let split = env.store.plan_repair(&env.config, "Alpha", &request);
    assert!(
        matches!(&split, Err(Error::Refused(why)) if why.contains("every disc")),
        "{split:?}"
    );
}

#[test]
fn under_a_loose_policy_every_disc_stays_torrentzip_when_one_has_more_than_one_rom() {
    let mut games = disc_games();
    // Disc 1 is one ROM, which alone could be loose; Disc 2 has two.
    games[1].1.remove(0);
    let env = Env::with("Redump", &games);
    set_disc_policy(&env, StorageFormat::Loose, false);
    let track = &games[1].1[0];
    env.put(track.name, &track.payload);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 2)"]);
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let first = item(&plan, "game:Saga (USA) (Disc 1)");
    assert_eq!(
        first.path.as_deref(),
        Some("Saga (USA) (Disc 1).zip"),
        "{first:?}"
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Saga (USA) (Disc 1)") && n.contains("another disc")),
        "{:?}",
        plan.content.notes
    );
}

#[test]
fn a_disc_is_not_converted_while_another_disc_of_its_game_cannot_be_repaired() {
    let mut games = disc_games();
    let env = Env::with("Redump", &games);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 1)"]);
    // Disc 2 lacks a track no folder holds, so it stays the ZIP it is.
    games[2].1.truncate(1);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 2)"]);
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let first = item(&plan, "game:Saga (USA) (Disc 1)");
    assert_eq!(first.chosen, RepairChoice::Leave, "{first:?}");
    assert!(
        first
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Saga (USA) (Disc 2)") && n.contains("every disc")),
        "{first:?}"
    );
    assert!(
        !plan
            .content
            .steps
            .iter()
            .any(|s| s.item.starts_with("game:Saga")),
        "{:#?}",
        plan.content.steps
    );
}

#[test]
fn a_disc_zipped_in_a_subfolder_is_not_taken_for_a_loose_folder() {
    let mut games = disc_games();
    let env = Env::with("Redump", &games);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 1)"]);
    // Disc 2, a ZIP lacking a track no folder holds, in a subfolder.
    games[2].1.truncate(1);
    let members: Vec<(&str, &[u8])> = games[2]
        .1
        .iter()
        .map(|r| (r.name, r.payload.as_slice()))
        .collect();
    write_zip(&env.set().join("region/Saga (USA) (Disc 2).zip"), &members);
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let first = item(&plan, "game:Saga (USA) (Disc 1)");
    assert_eq!(first.chosen, RepairChoice::Leave, "{first:?}");
}

#[test]
fn a_disc_is_not_converted_while_another_disc_of_its_game_is_unreadable() {
    let games = disc_games();
    let env = Env::with("Redump", &games);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 1)"]);
    env.put("Saga (USA) (Disc 2).zip", b"PK\x03\x04 not a whole ZIP");
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    let first = item(&plan, "game:Saga (USA) (Disc 1)");
    assert_eq!(first.chosen, RepairChoice::Leave, "{first:?}");
    assert!(
        first
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Saga (USA) (Disc 2)")),
        "{first:?}"
    );
}

#[test]
fn a_disc_is_not_converted_while_another_disc_of_its_game_is_an_unreadable_folder() {
    use std::os::unix::fs::PermissionsExt;
    let games = disc_games();
    let env = Env::with("Redump", &games);
    write_games_zipped(&env, &games, &["Saga (USA) (Disc 1)"]);
    let folder = env.set().join("Saga (USA) (Disc 2)");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o000)).unwrap();
    env.audit(AuditEvidence::Full);
    // A plan reads the source set again, so the folder is readable once more by then, though still empty.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = env.plan();

    assert!(
        env.latest_report()
            .entries
            .iter()
            .any(|e| e.path == "Saga (USA) (Disc 2)" && e.finding == Finding::Unreadable),
        "the audit reports the folder as unreadable"
    );
    let first = item(&plan, "game:Saga (USA) (Disc 1)");
    assert_eq!(first.chosen, RepairChoice::Leave, "{first:?}");
    assert!(
        first
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Saga (USA) (Disc 2)")),
        "{first:?}"
    );
}

#[test]
fn a_shared_original_stays_in_place_while_a_disc_written_from_it_could_not_be_put_in_place() {
    let games = disc_games();
    let env = Env::with("Redump", &games);
    // One ZIP holds both discs, so their repairs share it and are carried out together.
    let members: Vec<(&str, &[u8])> = games[1..]
        .iter()
        .flat_map(|(_, roms)| roms.iter().map(|r| (r.name, r.payload.as_slice())))
        .collect();
    let shared = env.set().join("Saga (USA).zip");
    write_zip(&shared, &members);
    let before = std::fs::read(&shared).unwrap();
    env.audit(AuditEvidence::Full);

    let plan = env.plan();
    for disc in ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"] {
        let it = item(&plan, &format!("game:{disc}"));
        assert_eq!(it.chosen, RepairChoice::Apply, "{it:?}");
    }
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    // A file appears where Disc 2's track would be put in place.
    let folder = env.set().join("Saga (USA) (Disc 2)");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Saga (USA) (Disc 2).bin"), b"in the way").unwrap();
    env.run_repair_job(&approved);

    assert_eq!(
        std::fs::read(&shared).ok().as_deref(),
        Some(before.as_slice()),
        "the original Disc 2 was read from stays where it was"
    );
}

/// Plans converting both discs of Saga, zipped, into loose folders, and stops the repair once the first disc's
/// conversion has changed something, then cancels the job a restart queued again.
fn cancelled_once_one_disc_converted(env: &Env, before_rerun: impl FnOnce()) -> RepairPlan {
    let plan = env.plan();
    let first = plan
        .content
        .steps
        .iter()
        .position(|s| {
            s.item.starts_with("game:Saga") && !matches!(s.step, RepairStep::Build { .. })
        })
        .unwrap();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store
        .inject_fault(Some(Fault::RepairStepDone(first as u32)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.run_repair_job(&approved)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    env.store.cancel_job(requeued.id).unwrap();
    before_rerun();
    let ended = env.run_repair_job(&env.store.repair_plan(plan.id).unwrap());
    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    env.store.repair_plan(plan.id).unwrap()
}

#[test]
fn a_cancelled_repair_converts_every_disc_of_a_game_once_one_is_converted() {
    let games = disc_games();
    let env = Env::with("Redump", &games);
    let discs = ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"];
    write_games_zipped(&env, &games, &discs);
    env.audit(AuditEvidence::Full);

    let done = cancelled_once_one_disc_converted(&env, || {});
    for disc in discs {
        let outcome = done
            .outcomes
            .iter()
            .find(|o| o.item == format!("game:{disc}"))
            .unwrap();
        assert!(outcome.done, "{:#?}", done.outcomes);
        assert!(env.set().join(disc).is_dir(), "{:?}", env.listing());
    }
}

#[test]
fn a_disc_left_converted_alone_says_its_game_is_stored_apart_and_the_next_plan_converts_it() {
    let games = disc_games();
    let env = Env::with("Redump", &games);
    let discs = ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"];
    write_games_zipped(&env, &games, &discs);
    env.audit(AuditEvidence::Full);

    let set = env.set();
    // Disc 2's ZIP is written again after the plan is made, so it stays where it is, and a file appears where its
    // track would be put in place, so its conversion can't finish.
    let done = cancelled_once_one_disc_converted(&env, || {
        let roms: Vec<(&str, &[u8])> = games[2]
            .1
            .iter()
            .map(|r| (r.name, r.payload.as_slice()))
            .collect();
        let zip = set.join("Saga (USA) (Disc 2).zip");
        std::fs::remove_file(&zip).unwrap();
        write_zip(&zip, &roms);
        let folder = set.join("Saga (USA) (Disc 2)");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Saga (USA) (Disc 2).bin"), b"in the way").unwrap();
    });
    let second = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Saga (USA) (Disc 2)")
        .unwrap();
    assert!(!second.done, "{second:?}");
    assert!(
        second.detail.iter().any(|d| d.contains("stored apart")),
        "{second:?}"
    );

    env.store.fulfil_repair_audits().unwrap();
    env.run_queued_audit(&env.store.repair_plan(done.id).unwrap());
    let next = env.plan();
    let again = item(&next, "game:Saga (USA) (Disc 2)");
    assert_eq!(again.chosen, RepairChoice::Apply, "{again:?}");
    assert_eq!(
        again.path.as_deref(),
        Some("Saga (USA) (Disc 2)"),
        "{again:?}"
    );
}

/// Imports a cue pack holding `cues` for the disc system's DAT.
fn import_cue_pack(env: &Env, cues: &[(&str, &[u8])]) {
    let pack = env.tmp.path().join("Test - System (cues).zip");
    write_zip(&pack, cues);
    env.store
        .import_cue_pack(&pack, DISC_SYSTEM, Some("https://example.invalid/cues"))
        .unwrap();
}

#[test]
fn a_plan_never_reads_a_cue_pack_deleted_while_it_was_made() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[1..]);
    env.audit(AuditEvidence::Full);
    let cue = ("Quest (USA).cue", quest[0].payload.as_slice());
    import_cue_pack(&env, &[cue]);
    let older = env.store.cue_packs().unwrap()[0].sha256.clone();

    // While the plan is made from the older pack, a newer one is imported, which deletes the older.
    let (paused, pause) = std::sync::mpsc::channel::<()>();
    let (resume, resumed) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some((paused, resumed)));
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::RepairPlanned
            && let Some((paused, resumed)) = gate.lock().unwrap().take()
        {
            paused.send(()).unwrap();
            resumed.recv().unwrap();
        }
    })));
    let plan = std::thread::scope(|s| {
        let planning = s.spawn(|| env.plan());
        pause.recv().unwrap();
        import_cue_pack(
            &env,
            &[
                cue,
                ("Other (USA).cue", b"FILE \"Other (USA).bin\" BINARY\n"),
            ],
        );
        resume.send(()).unwrap();
        planning.join().unwrap()
    });
    env.store.hook_faults(None);
    let packs = env.store.cue_packs().unwrap();
    assert!(!packs[0].blob_present, "{packs:?}");

    let read: Vec<&str> = plan
        .content
        .steps
        .iter()
        .filter_map(|s| match &s.step {
            RepairStep::Build { roms, .. } => Some(roms),
            _ => None,
        })
        .flatten()
        .filter_map(|r| r.cue_pack.as_deref())
        .collect();
    assert_eq!(read, [packs[1].sha256.as_str()], "not {older}");
    env.approve(&plan);
    assert_eq!(
        std::fs::read(env.set().join("Quest (USA)/Quest (USA).cue")).unwrap(),
        quest[0].payload
    );
}

#[test]
fn a_missing_cue_sheet_is_filled_from_a_cue_pack() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[1..]);
    env.audit(AuditEvidence::Full);
    assert_ne!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
    import_cue_pack(
        &env,
        &[
            ("Quest (USA).cue", quest[0].payload.as_slice()),
            ("Other (USA).cue", b"FILE \"Other (USA).bin\" BINARY\n"),
        ],
    );

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
    assert!(fill.summary.contains("cue pack"), "{}", fill.summary);
    let done = env.approve(&plan);

    assert_eq!(
        std::fs::read(env.set().join("Quest (USA)/Quest (USA).cue")).unwrap(),
        quest[0].payload
    );
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn a_cue_pack_imported_under_another_spelling_of_its_provider_is_kept_under_its_dat_and_used() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[1..]);
    env.audit(AuditEvidence::Full);
    let pack = env.tmp.path().join("Test - System (cues).zip");
    write_zip(&pack, &[("Quest (USA).cue", quest[0].payload.as_slice())]);
    let lower = DISC_SYSTEM.replacen("Redump", "redump", 1);
    let (record, _) = env.store.import_cue_pack(&pack, &lower, None).unwrap();
    assert_eq!(record.dat, DISC_SYSTEM);

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
}

#[test]
fn a_mismatched_cue_sheet_is_replaced_from_a_cue_pack_and_kept() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest);
    env.put(
        "Quest (USA)/Quest (USA).cue",
        b"FILE \"wrong.bin\" BINARY\n",
    );
    env.audit(AuditEvidence::Full);
    import_cue_pack(
        &env,
        &[("cues/Quest (USA).cue", quest[0].payload.as_slice())],
    );

    let plan = env.plan();
    let replace = item(&plan, "game:Quest (USA)");
    assert_eq!(replace.kind, RepairKind::Replace, "{replace:?}");
    let done = env.approve(&plan);

    assert_eq!(
        std::fs::read(env.set().join("Quest (USA)/Quest (USA).cue")).unwrap(),
        quest[0].payload
    );
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].name, "Quest (USA)/Quest (USA).cue");
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn a_cue_pack_for_a_dat_not_imported_or_without_cue_sheets_is_refused() {
    let env = Env::with("Redump", &disc_games());
    let pack = env.tmp.path().join("pack.zip");
    write_zip(&pack, &[("Quest (USA).cue", b"FILE")]);
    let unknown = env.store.import_cue_pack(&pack, "Redump/Other", None);
    assert!(matches!(unknown, Err(Error::Refused(_))), "{unknown:?}");
    write_zip(&pack, &[("readme.txt", b"hello")]);
    let empty = env.store.import_cue_pack(&pack, DISC_SYSTEM, None);
    assert!(matches!(empty, Err(Error::Refused(_))), "{empty:?}");
    assert!(env.store.cue_packs().unwrap().is_empty());
}

#[test]
fn a_cue_pack_expanding_past_its_budget_or_listing_too_many_members_is_refused() {
    let env = Env::with("Redump", &disc_games());
    let pack = env.tmp.path().join("pack.zip");
    let sheet = vec![b' '; 1 << 20];
    let names: Vec<String> = (0..65).map(|i| format!("Game {i}.cue")).collect();
    let members: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), sheet.as_slice()))
        .collect();
    write_zip(&pack, &members);
    let expanding = env.store.import_cue_pack(&pack, DISC_SYSTEM, None);
    assert!(
        matches!(&expanding, Err(Error::Refused(why)) if why.contains("together")),
        "{expanding:?}"
    );

    let names: Vec<String> = (0..32769).map(|i| format!("{i}.txt")).collect();
    let mut members: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    members.push(("Quest (USA).cue", b"FILE"));
    write_zip(&pack, &members);
    let crowded = env.store.import_cue_pack(&pack, DISC_SYSTEM, None);
    assert!(matches!(crowded, Err(Error::Refused(_))), "{crowded:?}");
    assert!(env.store.cue_packs().unwrap().is_empty());
}

#[test]
fn planning_matches_cue_sheets_indexed_at_import_without_reading_the_pack() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[1..]);
    env.audit(AuditEvidence::Full);
    import_cue_pack(&env, &[("Quest (USA).cue", quest[0].payload.as_slice())]);
    let pack = env.store.cue_packs().unwrap().remove(0);
    // The pack's file no longer reads as a ZIP: planning matches the sheet's checksums from the index all the same,
    // and only the repair, reading the sheet, finds it unreadable and writes nothing.
    std::fs::write(env.store.cue_pack_path(&pack.sha256), b"not a zip").unwrap();

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
    assert!(fill.summary.contains("cue pack"), "{}", fill.summary);
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.run_repair_job(&approved);
    let done = env.store.repair_plan(plan.id).unwrap();
    let outcome = &done.outcomes[0];
    assert!(
        !outcome.done && outcome.error.as_deref() == Some("it is not a ZIP archive"),
        "{outcome:?}"
    );
    assert!(!env.set().join("Quest (USA)/Quest (USA).cue").exists());
    assert!(
        env.listing()
            .iter()
            .all(|name| !name.contains("title-select-new")),
        "{:?}",
        env.listing()
    );
}

#[test]
fn an_older_cue_pack_is_deleted_unless_a_repair_plan_refers_to_it() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[1..]);
    env.audit(AuditEvidence::Full);
    import_cue_pack(&env, &[("Quest (USA).cue", quest[0].payload.as_slice())]);
    env.plan();
    import_cue_pack(
        &env,
        &[("Other (USA).cue", b"FILE \"Other (USA).bin\" BINARY\n")],
    );
    import_cue_pack(
        &env,
        &[("Another (USA).cue", b"FILE \"Another (USA).bin\" BINARY\n")],
    );

    let packs = env.store.cue_packs().unwrap();
    let present: Vec<(bool, bool)> = packs
        .iter()
        .map(|p| (p.blob_present, env.store.cue_pack_path(&p.sha256).is_file()))
        .collect();
    assert_eq!(
        present,
        [(true, true), (false, false), (true, true)],
        "the planned pack is kept, the older unplanned one deleted, the latest kept"
    );

    let older = env.tmp.path().join("older.zip");
    write_zip(
        &older,
        &[("Other (USA).cue", b"FILE \"Other (USA).bin\" BINARY\n")],
    );
    let (restored, outcome) = env
        .store
        .import_cue_pack(&older, DISC_SYSTEM, None)
        .unwrap();
    assert_eq!(outcome, ImportOutcome::Restored);
    assert!(restored.blob_present);
    env.store.retain_blobs().unwrap();
    assert!(env.store.cue_pack_path(&restored.sha256).is_file());
}

#[test]
fn garbage_collection_finishes_deleting_a_cue_pack_and_removes_a_stray_file() {
    let env = Env::with("Redump", &disc_games());
    import_cue_pack(&env, &[("A (USA).cue", b"FILE \"A (USA).bin\" BINARY\n")]);
    import_cue_pack(&env, &[("B (USA).cue", b"FILE \"B (USA).bin\" BINARY\n")]);
    let packs = env.store.cue_packs().unwrap();
    assert!(!packs[0].blob_present && packs[1].blob_present, "{packs:?}");
    // A stop after the older pack's deletion was recorded, before its file was deleted, left the file; and a file
    // no pack is.
    let dropped = env.store.cue_pack_path(&packs[0].sha256);
    std::fs::write(&dropped, b"left by a stop").unwrap();
    let stray = dropped.with_file_name(format!("{:064x}", 1));
    std::fs::write(&stray, b"no row").unwrap();

    let removed = env.store.collect_garbage().unwrap();

    assert!(!dropped.exists() && !stray.exists(), "{removed:?}");
    assert!(env.store.cue_pack_path(&packs[1].sha256).is_file());
}

#[test]
fn a_disc_game_arriving_as_one_zip_is_extracted_into_its_loose_folder_and_the_zip_kept() {
    let env = Env::with("Redump", &disc_games());
    env.audit(AuditEvidence::Full);
    let quest = disc_of("Quest (USA)");
    env.ingest("quest.zip", |p| {
        let members: Vec<(&str, &[u8])> = quest
            .iter()
            .map(|r| (r.name, r.payload.as_slice()))
            .collect();
        write_zip(p, &members)
    });

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
    assert!(fill.summary.contains("quest.zip"), "{}", fill.summary);
    let done = env.approve(&plan);

    let mut wanted = vec![".title-select".to_string()];
    wanted.extend(quest.iter().map(|r| format!("Quest (USA)/{}", r.name)));
    wanted.sort();
    assert_eq!(env.listing(), wanted);
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].name, "quest.zip");
    assert_eq!(
        env.ingest_listing(),
        [format!(
            ".title-select-retained/{}/quest.zip.kept",
            kept[0].generation
        )]
    );
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn a_disc_game_arriving_as_one_zip_is_read_once_as_its_tracks_are_extracted() {
    let env = Env::with("Redump", &disc_games());
    env.audit(AuditEvidence::Full);
    let quest = disc_of("Quest (USA)");
    env.ingest("quest.zip", |p| {
        let members: Vec<(&str, &[u8])> = quest
            .iter()
            .map(|r| (r.name, r.payload.as_slice()))
            .collect();
        write_zip(p, &members)
    });
    let plan = env.plan();
    let whole = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = whole.clone();
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::IngestReadWhole {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    })));
    let done = env.approve(&plan);
    env.store.hook_faults(None);

    // Each track is read from the ZIP's member alone, trusting the rest by the stamp the scan hashed it under.
    assert_eq!(whole.load(std::sync::atomic::Ordering::SeqCst), 0);
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn tracks_arriving_one_by_one_are_moved_into_the_loose_folder_and_the_plan_says_what_is_missing() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    assert_eq!(quest.len(), 4, "a cue sheet and three tracks");
    write_loose_folder(&env.set(), "Quest (USA)", &quest[..2]);
    env.audit(AuditEvidence::Full);
    env.ingest("track 2.bin", |p| write_file(p, &quest[2].payload));

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
    assert!(
        fill.summary.contains(quest[3].name) && fill.summary.contains("still missing"),
        "{}",
        fill.summary
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains(quest[3].name) && n.contains("still missing")),
        "{:?}",
        plan.content.notes
    );
    let done = env.approve(&plan);

    assert_eq!(
        std::fs::read(env.set().join("Quest (USA)").join(quest[2].name)).unwrap(),
        quest[2].payload
    );
    assert!(
        env.ingest_listing().is_empty(),
        "a loose track written exactly as its ROM leaves nothing behind: {:?}",
        env.ingest_listing()
    );
    env.run_queued_audit(&done);
    assert_ne!(status_of(&env, "Quest (USA)"), GameStatus::Complete);

    env.ingest("track 3.bin", |p| write_file(p, &quest[3].payload));
    let plan = env.plan();
    assert_eq!(item(&plan, "game:Quest (USA)").kind, RepairKind::Fill);
    let done = env.approve(&plan);
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
    let mut wanted = vec![".title-select".to_string()];
    wanted.extend(quest.iter().map(|r| format!("Quest (USA)/{}", r.name)));
    wanted.sort();
    assert_eq!(env.listing(), wanted);
}

#[test]
fn one_loose_ingest_file_fills_every_track_it_is_and_goes_only_after_the_last() {
    let mut games = disc_games();
    // Tracks 2 and 3 of Quest read alike, so one file is both.
    games[0].1[3].payload = games[0].1[2].payload.clone();
    let env = Env::with("Redump", &games);
    let quest = &games[0].1;
    write_loose_folder(&env.set(), "Quest (USA)", &quest[..2]);
    env.audit(AuditEvidence::Full);
    env.ingest("track.bin", |p| write_file(p, &quest[2].payload));

    let plan = env.plan();
    let fill = item(&plan, "game:Quest (USA)");
    assert_eq!(fill.kind, RepairKind::Fill, "{fill:?}");
    let done = env.approve(&plan);
    assert_eq!(done.outcomes[0].error, None, "{:#?}", done.outcomes);

    for r in &quest[2..] {
        assert_eq!(
            std::fs::read(env.set().join("Quest (USA)").join(r.name)).unwrap(),
            r.payload
        );
    }
    assert!(
        env.ingest_listing().is_empty(),
        "{:?}",
        env.ingest_listing()
    );
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

#[test]
fn a_mismatched_track_is_replaced_from_ingest_and_kept_aside() {
    let env = Env::with("Redump", &disc_games());
    let quest = disc_of("Quest (USA)");
    write_loose_folder(&env.set(), "Quest (USA)", &quest);
    let track = format!("Quest (USA)/{}", quest[1].name);
    env.put(&track, b"not the track");
    env.audit(AuditEvidence::Full);
    env.ingest("track.bin", |p| write_file(p, &quest[1].payload));

    let plan = env.plan();
    let replace = item(&plan, "game:Quest (USA)");
    assert_eq!(replace.kind, RepairKind::Replace, "{replace:?}");
    let done = env.approve(&plan);

    assert_eq!(
        std::fs::read(env.set().join(&track)).unwrap(),
        quest[1].payload
    );
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert!(
        kept.iter().any(|k| k.name.ends_with(quest[1].name)),
        "{kept:?}"
    );
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Quest (USA)"), GameStatus::Complete);
}

/// A Redump DAT where `Ball (USA) (Canada)`, renamed since it was dumped, shares its second track's payload with
/// `Wrestle (Europe)`, which the Source Set lacks. With `weaker`, the DAT lists Ball's track without the SHA-256 it gives
/// Wrestle's, so writing Ball's says less about the bytes than Wrestle's copy needs. The Source Set holds Ball's tracks under its old name, `Ball (USA).zip`, with its old
/// `.cue`, and the cue pack holds its new one.
fn renamed_with_a_shared_track(weaker: bool) -> Env {
    let mut ball = disc("Ball (USA) (Canada)", 2, 9_000, 40);
    let mut wrestle = disc("Wrestle (Europe)", 2, 9_000, 50);
    wrestle[2].payload = ball[2].payload.clone();
    ball[2].sha256 = !weaker;
    let [cue, track_1, track_2] = [0, 1, 2].map(|n| ball[n].payload.clone());
    let env = Env::with(
        "Redump",
        &[("Ball (USA) (Canada)", ball), ("Wrestle (Europe)", wrestle)],
    );
    set_disc_policy(&env, StorageFormat::TorrentZip, false);
    write_zip(
        &env.set().join("Ball (USA).zip"),
        &[
            (
                "Ball (USA).cue",
                b"FILE \"Ball (USA) (Track 1).bin\" BINARY\n",
            ),
            ("Ball (USA) (Track 1).bin", &track_1),
            ("Ball (USA) (Track 2).bin", &track_2),
        ],
    );
    env.audit(AuditEvidence::Full);
    import_cue_pack(&env, &[("Ball (USA) (Canada).cue", &cue)]);
    env
}

#[test]
fn a_renamed_game_whose_track_another_game_shares_is_written_under_its_new_name() {
    let env = renamed_with_a_shared_track(false);

    let plan = env.plan();
    let fill = item(&plan, "game:Ball (USA) (Canada)");
    assert_eq!(fill.chosen, RepairChoice::Apply, "{fill:?}");
    let done = env.approve(&plan);

    assert!(is_torrentzip(&env.set().join("Ball (USA) (Canada).zip")));
    let kept = env.store.source_kept_files("Alpha").unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].name, "Ball (USA).zip");
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Ball (USA) (Canada)"), GameStatus::Complete);
}

#[test]
fn a_renamed_game_is_left_when_its_track_is_the_last_copy_of_one_the_dat_pins_more_closely() {
    let env = renamed_with_a_shared_track(true);
    let before = env.listing();

    let plan = env.plan();

    let left = item(&plan, "game:Ball (USA) (Canada)");
    assert_eq!(left.chosen, RepairChoice::Leave, "{left:?}");
    assert_eq!(
        left.note.as_deref(),
        Some(
            "left as it is: it would keep aside the only copy of a ROM no repair in this plan writes"
        )
    );
    env.approve(&plan);
    assert_eq!(env.listing()[1..], before[..], "only the marker is new");
}

#[test]
fn a_game_whose_track_is_only_inside_another_games_zip_is_filled_from_it() {
    let ball = disc("Ball (USA)", 2, 9_000, 40);
    let mut wrestle = disc("Wrestle (Europe)", 2, 9_000, 50);
    wrestle[2].payload = ball[2].payload.clone();
    // Ball's ZIP holds its tracks under an old title, so each counts towards every Game its payload matches.
    let members: Vec<(String, Vec<u8>)> = ball
        .iter()
        .map(|r| (r.name.replace("Ball", "Old Ball"), r.payload.clone()))
        .collect();
    let (cue, track_1) = (wrestle[0].payload.clone(), wrestle[1].payload.clone());
    let env = Env::with(
        "Redump",
        &[("Ball (USA)", ball), ("Wrestle (Europe)", wrestle)],
    );
    set_disc_policy(&env, StorageFormat::TorrentZip, false);
    let members: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, p)| (n.as_str(), p.as_slice()))
        .collect();
    write_zip(&env.set().join("Ball (USA).zip"), &members);
    env.put("Wrestle (Europe).cue", &cue);
    env.put("Wrestle (Europe) (Track 1).bin", &track_1);
    env.audit(AuditEvidence::Full);
    let report = env.latest_report();
    let before = report
        .games
        .iter()
        .find(|g| g.name == "Wrestle (Europe)")
        .unwrap();
    assert_eq!(before.status, GameStatus::Incomplete);
    assert_eq!(before.available_elsewhere[0].games, ["Ball (USA)"]);

    let plan = env.plan();
    let fill = item(&plan, "game:Wrestle (Europe)");
    assert_eq!(fill.chosen, RepairChoice::Apply, "{fill:?}");
    let done = env.approve(&plan);

    assert!(is_torrentzip(&env.set().join("Wrestle (Europe).zip")));
    env.run_queued_audit(&done);
    assert_eq!(status_of(&env, "Wrestle (Europe)"), GameStatus::Complete);
    assert_eq!(status_of(&env, "Ball (USA)"), GameStatus::Complete);
}

type Tracks = Vec<(&'static str, Vec<u8>)>;

/// One disc Game per 7z method, solid and not: three tracks each, which no other Game shares.
fn sevenz_discs() -> Vec<(sevenz_fixtures::Method, bool, &'static str, Tracks)> {
    let mut discs = Vec::new();
    for (m, method) in sevenz_fixtures::Method::ALL.into_iter().enumerate() {
        for solid in [true, false] {
            let game: &'static str = format!("Disc {method:?} {solid} (USA)").leak();
            let seed = (m * 2 + usize::from(solid)) as u64 * 10;
            let tracks = vec![
                ("Track 01.bin", noise(seed + 1, 40_000)),
                ("Track 02.bin", noise(seed + 2, 3_000)),
                (
                    "Game.cue",
                    format!("FILE \"Track 01.bin\" BINARY {seed}").into_bytes(),
                ),
            ];
            discs.push((method, solid, game, tracks));
        }
    }
    discs
}

#[test]
fn missing_games_are_filled_from_7z_ingest_items_of_every_method_solid_or_not_in_nested_folders() {
    let discs = sevenz_discs();
    let games: Vec<(&str, Vec<RomSpec>)> = discs
        .iter()
        .map(|(_, _, game, tracks)| {
            let roms = tracks
                .iter()
                .map(|(name, bytes)| {
                    let name: &'static str = format!("{game} {name}").leak();
                    rom(name, bytes)
                })
                .collect();
            (*game, roms)
        })
        .collect();
    let env = Env::with("Test", &games);
    env.audit(AuditEvidence::Full);
    for (method, solid, game, tracks) in &discs {
        let members: Vec<(String, &[u8])> = tracks
            .iter()
            .map(|(name, bytes)| (format!("{game}/Disc 1/{name}"), bytes.as_slice()))
            .collect();
        let members: Vec<(&str, &[u8])> = members.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        env.ingest(&format!("{method:?}-{solid}.7z"), |p| {
            sevenz_fixtures::write_7z(p, &members, *method, *solid)
        });
    }

    let plan = env.plan();
    for (_, _, game, _) in &discs {
        assert_eq!(
            item(&plan, &format!("game:{game}")).kind,
            RepairKind::Fill,
            "{game}"
        );
    }
    let done = env.approve(&plan);

    // Members that waited in scratch for their turn in a repack leave nothing behind.
    let scratch: Vec<_> = std::fs::read_dir(env.store.dir().join("tmp"))
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().starts_with("7z-"))
        .collect();
    assert!(scratch.is_empty(), "{scratch:?}");
    let mut want: Vec<String> = discs
        .iter()
        .map(|(_, _, game, _)| format!("{game}.zip"))
        .collect();
    want.push(".title-select".into());
    want.sort();
    assert_eq!(env.listing(), want);
    for (_, _, game, _) in &discs {
        assert!(
            is_torrentzip(&env.set().join(format!("{game}.zip"))),
            "{game}"
        );
    }
    // Each 7z is kept in the Ingest Folder; none is written into the Source Set.
    assert_eq!(
        env.store.ingest_kept_files("in").unwrap().len(),
        discs.len()
    );
    env.run_queued_audit(&done);
    let report = env.latest_report();
    for (_, _, game, _) in &discs {
        let found = report.games.iter().find(|g| g.name == *game);
        assert_eq!(found.unwrap().status, GameStatus::Complete, "{game}");
    }
}

#[test]
fn a_game_filled_from_a_7z_into_a_loose_source_set_is_stored_loose() {
    let env = Env::new();
    set_source_policy(&env, StorageFormat::Loose, false);
    env.audit(AuditEvidence::Full);
    env.ingest("pong.7z", |p| {
        sevenz_fixtures::write_7z(
            p,
            &[("Atari/Pong (USA).a26", b"pong")],
            sevenz_fixtures::Method::Lzma2,
            true,
        )
    });

    let plan = env.plan();
    assert_eq!(item(&plan, "game:Pong (USA)").kind, RepairKind::Fill);
    env.approve(&plan);

    assert_eq!(env.listing(), [".title-select", "Pong (USA).a26"]);
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).a26")).unwrap(),
        b"pong"
    );
}
