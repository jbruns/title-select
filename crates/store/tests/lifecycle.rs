//! The lifecycle of registrations: Unregistering Source Sets, Library Folders, Additions Sets and Ingest Folders,
//! what holds each, Retiring Selection Profiles, setting whole sets aside, moving Library Folders and forgetting
//! what an Unregistered registration left in the store, from synthetic DATs, Source Sets and files.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::*;
use title_select_api::additions::NewAddition;
use title_select_api::build::{AddLibraryFolder, ChangeLibraryFolder, LibraryFolder, Plan};
use title_select_api::ingest::{IngestAction, IngestPlan, PlanIngest};
use title_select_api::lifecycle::RegistrationKind;
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, Winner,
};
use title_select_api::{AuditEvidence, Job, JobKind, JobState};
use title_select_store::config::Config;
use title_select_store::{Error, Store, Workers};

const SET: &str = "Test - System";
const SYSTEM: &str = "Test/Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

/// A store with the Source Set in the read-only Root `sets`, which lists it among its `writable_paths`; a writable
/// Root `play` for Library Folders; `additions` for Additions Sets; and the Ingest Folder `in`, which settles at once.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    sha256: String,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["sets/Test - System", "in", "play", "additions", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"{SET}\"]\n\n[roots.in]\npath = {:?}\nwritable = true\n\
             kind = \"ingest\"\nsettle_secs = 0\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
             [roots.additions]\npath = {:?}\nwritable = true\n",
            tmp.path().join("sets").display().to_string(),
            tmp.path().join("in").display().to_string(),
            tmp.path().join("play").display().to_string(),
            tmp.path().join("additions").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(&tmp.path().join("dats"), "test.dat", "1", &games());
        let (revision, _) = store.import_dat(&dat, "Test", None).unwrap();
        store.add_source_set(&config, "sets", SET, SYSTEM).unwrap();
        store
            .save_profile(
                PROFILE,
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        let env = Env {
            tmp,
            store,
            config,
            sha256: revision.sha256,
        };
        write_file(&env.set().join("Pong (USA).a26"), b"pong");
        write_file(&env.set().join("Combat (USA).a26"), b"combat");
        env
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("sets").join(SET)
    }

    fn play(&self) -> PathBuf {
        self.tmp.path().join("play")
    }

    fn audit(&self) {
        self.store
            .audit_source_set(&self.config, SET, None, AuditEvidence::Full)
            .unwrap();
    }

    fn set_id(&self) -> i64 {
        self.store.source_set_id(SET).unwrap()
    }

    fn folder(&self) -> LibraryFolder {
        self.store
            .add_library_folder(
                &self.config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("atari".into()),
                    template: None,
                    source_set: SET.into(),
                    profile: PROFILE.into(),
                },
            )
            .unwrap()
    }

    /// Records `selection` as a Selection Job's result, made with the latest Profile version.
    fn record(&self, selection: &Selection) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        assert_eq!(job.kind, JobKind::Selection);
        self.store
            .record_selection(&self.config, job.id, selection)
            .unwrap()
            .selection
            .unwrap()
    }

    /// Approves `plan`, then claims and runs its Build Job.
    fn build(&self, plan: &Plan) -> Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        self.store
            .run_build_job(&self.config, job.id, None)
            .unwrap()
    }

    /// A Library Folder with Pong and Combat built into it.
    fn built(&self) -> LibraryFolder {
        self.audit();
        let selection = self.record(&both(&self.sha256));
        let folder = self.folder();
        let plan = self
            .store
            .plan_build(&self.config, folder.id, selection)
            .unwrap();
        assert_eq!(self.build(&plan).state, JobState::Succeeded);
        folder
    }

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

    /// Adds `name` to the Additions Set from the Ingest Folder, and publishes it.
    fn add_addition(&self, name: &str, payload: &[u8]) {
        let file = format!("{name}.a26");
        self.ingest(&file, |p| write_file(p, payload));
        let id = self
            .store
            .ingest_items("in")
            .unwrap()
            .into_iter()
            .filter(|i| i.name == file)
            .max_by_key(|i| i.id)
            .unwrap()
            .id;
        let plan: IngestPlan = self
            .store
            .plan_ingest(
                &self.config,
                "in",
                &PlanIngest {
                    action: IngestAction::Addition,
                    items: vec![id],
                    provider: None,
                    additions: BTreeMap::from([(
                        id,
                        NewAddition {
                            system: SYSTEM.into(),
                            name: name.into(),
                            parent: None,
                        },
                    )]),
                },
            )
            .unwrap();
        self.store
            .approve_ingest_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.settle();
    }

    /// Claims and runs each queued Additions Job until none is left.
    fn settle(&self) {
        while let Some(job) = self.store.claim_job().unwrap() {
            assert_eq!(job.kind, JobKind::Additions, "{job:?}");
            let job = self.store.run_additions_job(&self.config, job.id).unwrap();
            assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        }
    }
}

fn group(title: &str) -> SelectionGroup {
    SelectionGroup {
        group: title.to_lowercase(),
        unresolved: false,
        winners: vec![Winner {
            title: title.into(),
            rule: Rule {
                stage: "Parent selection".into(),
                reference: None,
                message: "m".into(),
            },
            unresolved: false,
        }],
        alternatives: vec![],
        exclusions: vec![],
        choice: None,
        drift: false,
    }
}

fn both(sha256: &str) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SYSTEM.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: vec![group("Pong (USA)"), group("Combat (USA)")],
        stale_choices: BTreeMap::new(),
    }
}

/// Every file under `dir`, relative to it.
fn files(dir: &Path) -> BTreeSet<String> {
    walkdir::WalkDir::new(dir)
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
        .collect()
}

fn conflict(result: Result<impl std::fmt::Debug, Error>) -> String {
    match result {
        Err(Error::Conflict(why)) => why,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn a_source_set_is_held_by_a_library_folder_bound_to_it_until_that_folder_is_unregistered() {
    let env = Env::new();
    let folder = env.built();
    let holds = env.store.source_set_holds(SET).unwrap();
    assert!(
        holds.holds.iter().any(|h| h.contains("play/atari")),
        "{holds:?}"
    );
    let why = conflict(env.store.unregister_source_set(&env.config, SET));
    assert!(why.contains("play/atari"), "{why}");
    assert!(env.store.source_set(SET).is_ok(), "nothing changed");

    let before = files(&env.play().join("atari"));
    let done = env
        .store
        .unregister_library_folder(&env.config, folder.id)
        .unwrap();
    assert_eq!(done.kind, RegistrationKind::LibraryFolder);
    let after = files(&env.play().join("atari"));
    assert!(
        before.contains(".title-select") && !after.contains(".title-select"),
        "its own marker goes: {before:?} -> {after:?}"
    );
    assert_eq!(
        before
            .into_iter()
            .filter(|f| f != ".title-select")
            .collect::<BTreeSet<_>>(),
        after,
        "every built file is left in place"
    );
    assert!(env.store.library_folders(&env.config).unwrap().is_empty());
    assert!(env.store.source_set_holds(SET).unwrap().holds.is_empty());
    env.store.unregister_source_set(&env.config, SET).unwrap();
}

#[test]
fn an_unregistered_source_sets_name_and_folder_are_free_and_its_history_stays_readable() {
    let env = Env::new();
    env.audit();
    let old = env.set_id();
    let done = env.store.unregister_source_set(&env.config, SET).unwrap();
    assert_eq!((done.kind, done.id), (RegistrationKind::SourceSet, old));
    assert!(matches!(env.store.source_set(SET), Err(Error::NotFound(_))));
    assert!(env.store.source_sets().unwrap().is_empty());
    assert_eq!(
        files(&env.set()),
        BTreeSet::from(["Combat (USA).a26".into(), "Pong (USA).a26".into()]),
        "no game file is touched"
    );

    let unregistered = env.store.unregistered().unwrap();
    assert_eq!(unregistered.len(), 1, "{unregistered:#?}");
    assert_eq!(unregistered[0].id, old);
    assert_eq!(unregistered[0].audits, 1);
    let history = env.store.source_set_history_by_id(old).unwrap();
    assert_eq!(history.len(), 1);
    assert!(env.store.report(history[0].id).is_ok());

    env.store
        .add_source_set(&env.config, "sets", SET, SYSTEM)
        .unwrap();
    assert_ne!(
        env.set_id(),
        old,
        "registering the folder again is a new registration"
    );
    assert!(env.store.history(SET).unwrap().is_empty());
    assert!(matches!(
        env.store.unregister_source_set(&env.config, "Nope"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn a_queued_job_holds_its_source_set_and_its_library_folder() {
    let env = Env::new();
    env.audit();
    let folder = env.folder();
    let job = env.store.enqueue_check(folder.id, "Operator").unwrap();
    let why = conflict(env.store.unregister_library_folder(&env.config, folder.id));
    assert!(why.contains(&format!("job {}", job.id)), "{why}");
    env.store.claim_job().unwrap().unwrap();
    env.store.run_check_job(&env.config, job.id, None).unwrap();
    env.store
        .unregister_library_folder(&env.config, folder.id)
        .unwrap();

    let job = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Full)
        .unwrap();
    let why = conflict(env.store.unregister_source_set(&env.config, SET));
    assert!(why.contains(&format!("job {}", job.id)), "{why}");
}

#[test]
fn a_retired_profile_is_hidden_from_new_choices_and_restored_on_request() {
    let env = Env::new();
    env.audit();
    let folder = env.folder();
    let why = conflict(env.store.retire_profile(PROFILE));
    assert!(why.contains("play/atari"), "{why}");

    env.store
        .unregister_library_folder(&env.config, folder.id)
        .unwrap();
    let retired = env.store.retire_profile(PROFILE).unwrap();
    assert!(retired.retired_at.is_some());
    assert!(env.store.profiles().unwrap()[0].retired_at.is_some());
    assert!(
        env.store.profile_versions(PROFILE).unwrap().len() == 1,
        "its versions are kept"
    );
    for refused in [
        env.store
            .add_library_folder(
                &env.config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("other".into()),
                    template: None,
                    source_set: SET.into(),
                    profile: PROFILE.into(),
                },
            )
            .map(|_| ()),
        env.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .map(|_| ()),
        env.store
            .save_profile(
                PROFILE,
                &SelectionProfile::default(),
                Some(1),
                "Operator",
                &[],
                WORKERS,
            )
            .map(|_| ()),
    ] {
        match refused {
            Err(Error::Refused(why)) => assert!(why.contains("retired"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    let restored = env.store.restore_profile(PROFILE).unwrap();
    assert!(restored.retired_at.is_none());
    env.store
        .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
        .unwrap();
}

#[test]
fn a_rebind_to_a_retired_profile_is_refused() {
    let env = Env::new();
    env.audit();
    let folder = env.folder();
    env.store
        .save_profile(
            "Other",
            &SelectionProfile::default(),
            None,
            "Operator",
            &[],
            WORKERS,
        )
        .unwrap();
    env.store.retire_profile("Other").unwrap();
    let refused = env.store.change_library_folder(
        &env.config,
        folder.id,
        &ChangeLibraryFolder {
            profile: Some("Other".into()),
            source_set: None,
        },
    );
    assert!(
        matches!(refused, Err(Error::Refused(ref why)) if why.contains("retired")),
        "{refused:?}"
    );
}

#[test]
fn an_ingest_folder_is_unregistered_and_its_root_then_holds_nothing() {
    let env = Env::new();
    let holds = env.store.root_holds(&env.config, "in").unwrap();
    assert!(
        holds.holds.iter().any(|h| h.contains("ingest folder")),
        "{holds:?}"
    );
    let job = env.store.enqueue_ingest_scan("in", "Operator").unwrap();
    let why = conflict(env.store.unregister_ingest_folder(&env.config, "in"));
    assert!(why.contains(&format!("job {}", job.id)), "{why}");
    env.store.claim_job().unwrap().unwrap();
    env.store.run_ingest_scan_job(&env.config, job.id).unwrap();

    env.store
        .unregister_ingest_folder(&env.config, "in")
        .unwrap();
    assert!(
        !env.tmp.path().join("in/.title-select").exists(),
        "its own marker goes"
    );
    assert!(
        env.store
            .root_holds(&env.config, "in")
            .unwrap()
            .holds
            .is_empty()
    );
    assert!(
        env.store
            .ingest_folder(&env.config, "in")
            .unwrap()
            .id
            .is_none(),
        "it can be registered again"
    );
    env.store.register_ingest_folder(&env.config, "in").unwrap();

    let sets = env.store.root_holds(&env.config, "sets").unwrap();
    assert!(sets.holds.iter().any(|h| h.contains(SET)), "{sets:?}");
}

#[test]
fn an_additions_set_is_unregistered_leaving_its_additions_in_place() {
    let env = Env::new();
    env.audit();
    env.add_addition("My Hack", b"hack");
    let set = env.store.additions_set(SYSTEM).unwrap();
    let dir = env.tmp.path().join("additions").join(SET);
    let before = files(&dir);
    assert!(before.contains("My Hack.zip"), "{before:?}");

    let done = env
        .store
        .unregister_additions_set(&env.config, SYSTEM)
        .unwrap();
    assert_eq!(
        (done.kind, done.id),
        (RegistrationKind::AdditionsSet, set.id)
    );
    assert!(
        files(&dir).contains("My Hack.zip"),
        "the addition is left in place"
    );
    assert!(!files(&dir).contains(".title-select"));
    assert!(env.store.additions_sets().unwrap().is_empty());
    assert!(matches!(
        env.store.additions_set(SYSTEM),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn forgetting_an_unregistered_source_set_purges_its_history_and_the_dat_revision_only_it_held() {
    let env = Env::new();
    env.audit();
    // A newer revision, so the one the Audit read is no longer the newest and only the Audit holds it.
    let dat = write_dat_version(&env.tmp.path().join("dats"), "test2.dat", "2", &games());
    env.store.import_dat(&dat, "Test", None).unwrap();
    let old = env.set_id();
    assert!(
        matches!(
            env.store.forget(RegistrationKind::SourceSet, old),
            Err(Error::Refused(_))
        ),
        "a registered set is unregistered first"
    );
    env.store.unregister_source_set(&env.config, SET).unwrap();
    let audit = env.store.source_set_history_by_id(old).unwrap()[0].id;

    let forgotten = env.store.forget(RegistrationKind::SourceSet, old).unwrap();
    assert_eq!(forgotten.audits, 1);
    assert_eq!(forgotten.dat_revisions, 1);
    assert!(env.store.unregistered().unwrap().is_empty());
    assert!(env.store.report(audit).is_err());
    assert!(
        env.store
            .dat_revisions()
            .unwrap()
            .iter()
            .any(|r| r.sha256 == env.sha256),
        "the revision row stays; only its blob goes"
    );
    assert_eq!(files(&env.set()).len(), 2, "no file is touched");
}

#[test]
fn forgetting_a_source_set_waits_for_every_library_folder_bound_to_it() {
    let env = Env::new();
    let folder = env.built();
    env.store
        .unregister_library_folder(&env.config, folder.id)
        .unwrap();
    env.store.unregister_source_set(&env.config, SET).unwrap();
    let set = env.store.unregistered().unwrap();
    let set = set
        .iter()
        .find(|u| u.kind == RegistrationKind::SourceSet)
        .unwrap();
    assert!(matches!(
        env.store.forget(RegistrationKind::SourceSet, set.id),
        Err(Error::Refused(ref why)) if why.contains("play/atari")
    ));
    let forgotten = env
        .store
        .forget(RegistrationKind::LibraryFolder, folder.id)
        .unwrap();
    assert_eq!(forgotten.builds, 1);
    env.store
        .forget(RegistrationKind::SourceSet, set.id)
        .unwrap();
    assert!(env.store.unregistered().unwrap().is_empty());
    assert!(env.play().join("atari/Pong (USA).zip").exists());
}
