//! Repairing Source Sets: plans proposed from the latest Audit and assessed ingest items, the choices you make before
//! approving one, and what an approved repair does, from synthetic DATs, Source Sets and files.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};

use common::*;
use title_select_api::build::PlanKeptDeletion;
use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use title_select_api::{AuditEvidence, Finding, GameStatus, JobKind, JobState};
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
        let dat = write_dat_version(&tmp.path().join("dats"), "alpha.dat", "1", &alpha_games());
        store.import_dat(&dat, "Test", None).unwrap();
        store
            .add_source_set(&config, "sets", "Alpha", "Test/Test - System")
            .unwrap();
        store
            .add_source_set(&config, "ro", "Gamma", "Test/Test - System")
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
    for game in ["Combat (USA)", "Yars (USA)", "Adventure (USA)"] {
        let found = item(&plan, &format!("game:{game}"));
        assert_eq!(found.kind, RepairKind::Repack, "{game}");
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
