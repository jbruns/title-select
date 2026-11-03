//! Additions and Additions Sets: each way an addition is added (an Ingest Folder item, a Source Set Entry, a
//! Library Folder's Drift), the Selection Profile opting a system in to its Additions Set for builds, checks of an
//! Additions Set for Drift, and recovery from a stop at each step of publishing one; from synthetic DATs and files.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::*;
use sha2::Digest;
use title_select_api::additions::{
    Addition, AdditionSource, AdditionState, AdditionsFinding, EditAddition, NewAddition,
};
use title_select_api::build::{
    AddLibraryFolder, CleanupAction, DriftKind, ItemKind, LibraryFolder, Plan, PlanCleanup,
};
use title_select_api::build::{PlanKeptDeletion, RETENTION};
use title_select_api::ingest::{IngestAction, IngestPlan, PlanIngest, Proposal};
use title_select_api::repair::{PlanRepair, RepairChoice, RepairPlan};
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, SystemSettings, Winner,
};
use title_select_api::{AuditEvidence, Job, JobKind, JobState};
use title_select_engine::torrentzip;
use title_select_store::config::Config;
use title_select_store::{Fault, Store, Workers};

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

/// A store with the Source Set `Test - System` in the Root `sets`, which repairs may write, holding Pong; the Ingest
/// Folder `in`, which settles at once; the writable Root `play` for Library Folders; and the Root `additions`.
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
        env
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("sets").join(SET)
    }

    fn additions(&self) -> PathBuf {
        self.tmp.path().join("additions").join(SET)
    }

    fn play(&self) -> PathBuf {
        self.tmp.path().join("play")
    }

    fn ingest_dir(&self) -> PathBuf {
        self.tmp.path().join("in")
    }

    fn audit(&self) {
        self.store
            .audit_source_set(&self.config, SET, None, AuditEvidence::Full)
            .unwrap();
    }

    /// Puts `name` in the Ingest Folder and scans it twice, so it settles and is assessed.
    fn ingest(&self, name: &str, write: impl FnOnce(&Path)) {
        write(&self.ingest_dir().join(name));
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

    fn item_id(&self, name: &str) -> i64 {
        self.store
            .ingest_items("in")
            .unwrap()
            .into_iter()
            .filter(|i| i.name == name)
            .max_by_key(|i| i.id)
            .unwrap()
            .id
    }

    fn plan_ingest(&self, name: &str, new: NewAddition) -> IngestPlan {
        let id = self.item_id(name);
        self.store
            .plan_ingest(
                &self.config,
                "in",
                &PlanIngest {
                    action: IngestAction::Addition,
                    items: vec![id],
                    provider: None,
                    additions: BTreeMap::from([(id, new)]),
                },
            )
            .unwrap()
    }

    /// Approves `plan`, runs the Additions Job that publishes what it adds, and returns the plan as it ended.
    fn approve_ingest(&self, plan: &IngestPlan) -> IngestPlan {
        self.store
            .approve_ingest_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.settle();
        self.store.ingest_plan(plan.id).unwrap()
    }

    /// Claims and runs each queued Additions Job until none is left, and returns how each ended.
    fn settle(&self) -> Vec<Job> {
        let mut ended = Vec::new();
        while let Some(job) = self.store.claim_job().unwrap() {
            assert_eq!(job.kind, JobKind::Additions, "{job:?}");
            let job = self.store.run_additions_job(&self.config, job.id).unwrap();
            assert_eq!(job.state, JobState::Succeeded, "{job:?}");
            ended.push(job);
        }
        ended
    }

    /// Claims the queued Additions Job and runs it until it stops at `fault`.
    fn stop_additions_at(&self, fault: Fault) {
        let job = self
            .store
            .claim_job()
            .unwrap()
            .expect("an additions job is queued");
        assert_eq!(job.kind, JobKind::Additions, "{job:?}");
        self.store.inject_fault(Some(fault));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.store.run_additions_job(&self.config, job.id)
        }));
        assert!(ran.is_err(), "{fault:?} was reached: {ran:?}");
        self.store.inject_fault(None);
    }

    /// Where the Source Set's kept file of `name` is.
    fn source_kept(&self, name: &str) -> (i64, PathBuf) {
        let kept = self.store.source_kept_files(SET).unwrap();
        let k = kept.iter().find(|k| k.name == name).expect("it is kept");
        let path = self
            .set()
            .join(RETENTION)
            .join(k.generation.to_string())
            .join(&k.kept_name);
        assert!(path.is_file(), "{}", path.display());
        (k.id, path)
    }

    fn plan_repair(&self, key: &str, new: Option<NewAddition>) -> RepairPlan {
        let request = PlanRepair {
            choices: BTreeMap::from([(key.to_owned(), RepairChoice::AddAddition)]),
            additions: new.map(|n| (key.to_owned(), n)).into_iter().collect(),
        };
        self.store.plan_repair(&self.config, SET, &request).unwrap()
    }

    /// Approves `plan` and claims its Repair Job, returning its id.
    fn approve_repair(&self, plan: &RepairPlan) -> i64 {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        job
    }

    /// Approves and runs `plan`, but not the Additions Job it queues.
    fn repair_only(&self, plan: &RepairPlan) {
        let job = self.approve_repair(plan);
        let ended = self.store.run_repair_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    }

    fn repair(&self, plan: &RepairPlan) {
        self.repair_only(plan);
        self.settle();
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

    /// Saves the next Profile version, with this system's Additions Set read by builds or not.
    fn opt_in(&self, additions: bool) {
        let latest = self.store.profile(PROFILE).unwrap();
        let mut profile = SelectionProfile::default();
        if additions {
            profile.systems.insert(
                SET.into(),
                SystemSettings {
                    additions,
                    ..SystemSettings::default()
                },
            );
        }
        self.store
            .save_profile(
                PROFILE,
                &profile,
                latest.latest.map(|v| v.version),
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
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

    fn build(&self, plan: &Plan) {
        self.build_only(plan);
        self.settle();
    }

    /// Approves and runs `plan`, but not an Additions Job it queues.
    fn build_only(&self, plan: &Plan) {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let ended = self
            .store
            .run_build_job(&self.config, job.id, None)
            .unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    }

    /// Reopens the store, as the service does when it starts, and settles what a stop left.
    fn restart(self) -> Env {
        let Env {
            tmp,
            store,
            config,
            sha256,
        } = self;
        let path = store.dir().to_path_buf();
        drop(store);
        let store = Store::open(&path).unwrap();
        store.recover_jobs(WORKERS).unwrap();
        Env {
            tmp,
            store,
            config,
            sha256,
        }
    }

    fn only_addition(&self) -> Addition {
        let sets = self.store.additions_sets().unwrap();
        assert_eq!(sets.len(), 1, "{sets:#?}");
        assert_eq!(sets[0].additions.len(), 1, "{sets:#?}");
        sets[0].additions[0].clone()
    }
}

fn new(name: &str) -> NewAddition {
    NewAddition {
        system: SYSTEM.into(),
        name: name.into(),
        parent: None,
    }
}

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

/// The members of the TorrentZip at `path`, by name, with their payloads.
fn members(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut file = std::fs::File::open(path).unwrap();
    let found = torrentzip::validate(&mut file).expect("an addition is TorrentZip");
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    found
        .iter()
        .map(|m| {
            let mut payload = Vec::new();
            std::io::Read::read_to_end(&mut zip.by_name(&m.name).unwrap(), &mut payload).unwrap();
            (m.name.clone(), payload)
        })
        .collect()
}

/// An active addition holding `payload` as `member`, stored as `<name>.zip` in the Additions Set.
fn assert_published(env: &Env, a: &Addition, name: &str, member: &str, payload: &[u8]) {
    assert_eq!(a.state, AdditionState::Active, "{a:#?}");
    assert_eq!(a.name, name);
    assert_eq!(a.file_name, format!("{name}.zip"));
    let rom = a.rom.as_ref().expect("an active addition records its ROM");
    assert_eq!(rom.size, payload.len() as u64);
    assert!(
        rom.crc.is_some() && rom.md5.is_some() && rom.sha1.is_some() && rom.sha256.is_some(),
        "{rom:?}"
    );
    assert_eq!(
        members(&env.additions().join(&a.file_name)),
        [(member.to_owned(), payload.to_vec())]
    );
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

fn pong(sha256: &str) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: vec![group("Pong (USA)")],
        stale_choices: BTreeMap::new(),
    }
}

#[test]
fn a_loose_ingest_item_is_added_and_its_original_deleted_once_the_copy_is_exactly_it() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    let id = env.item_id("My Hack.a26");
    let item = env.store.ingest_items("in").unwrap();
    let item = item.iter().find(|i| i.id == id).unwrap();
    assert_eq!(
        item.assessment.proposal,
        Some(Proposal::Addition {
            system: Some(SYSTEM.into())
        })
    );

    let plan = env.plan_ingest(
        "My Hack.a26",
        NewAddition {
            parent: Some("Pong (USA)".into()),
            ..new("My Hack")
        },
    );
    assert_eq!(plan.content.items.len(), 1);
    assert!(
        plan.content.items[0].destination.contains("My Hack.zip"),
        "{:?}",
        plan.content.items[0]
    );
    assert!(
        env.store.additions_sets().unwrap().is_empty(),
        "planning adds nothing"
    );
    assert!(!env.additions().exists(), "planning creates nothing");

    let done = env.approve_ingest(&plan);
    assert!(
        done.outcomes.iter().all(|o| o.moved),
        "{:#?}",
        done.outcomes
    );
    let a = env.only_addition();
    assert_published(&env, &a, "My Hack", "My Hack.a26", b"hack");
    assert_eq!(a.source, AdditionSource::Ingest);
    assert_eq!(a.origin, "My Hack.a26");
    assert_eq!(a.parent.as_deref(), Some("Pong (USA)"));
    assert!(
        !env.ingest_dir().join("My Hack.a26").exists(),
        "the original is deleted once the copy is exactly it"
    );
    assert!(env.store.ingest_kept_files("in").unwrap().is_empty());
    assert_eq!(
        files(&env.additions()),
        BTreeSet::from([".title-select".to_owned(), "My Hack.zip".to_owned()])
    );
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.additions().join(".title-select")).unwrap())
            .unwrap();
    assert_eq!(
        marker["additions_set"],
        env.store.additions_sets().unwrap()[0].id
    );
}

#[test]
fn a_zip_ingest_item_is_repacked_and_its_original_kept_rather_than_deleted() {
    let env = Env::new();
    env.audit();
    env.ingest("hack.zip", |p| {
        write_zip(p, &[("Zipped Hack.a26", b"zipped")])
    });

    let done = env.approve_ingest(&env.plan_ingest("hack.zip", new("Zipped Hack")));
    assert!(
        done.outcomes.iter().all(|o| o.moved),
        "{:#?}",
        done.outcomes
    );
    let a = env.only_addition();
    assert_published(&env, &a, "Zipped Hack", "Zipped Hack.a26", b"zipped");
    assert!(
        !env.ingest_dir().join("hack.zip").exists(),
        "the original leaves the folder"
    );
    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(
        kept.len(),
        1,
        "a repacked original is kept, never deleted: {kept:#?}"
    );
    assert_eq!(kept[0].name, "hack.zip");
}

#[test]
fn an_ingest_addition_needs_a_system_a_valid_unique_name_and_an_unmatched_item() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    env.ingest("Combat (USA).a26", |p| write_file(p, b"combat"));
    let plan = |name: &str, new: NewAddition| {
        let id = env.item_id(name);
        env.store.plan_ingest(
            &env.config,
            "in",
            &PlanIngest {
                action: IngestAction::Addition,
                items: vec![id],
                provider: None,
                additions: BTreeMap::from([(id, new)]),
            },
        )
    };
    assert!(
        plan(
            "My Hack.a26",
            NewAddition {
                system: "Test/Nope".into(),
                ..new("x")
            }
        )
        .is_err()
    );
    assert!(plan("My Hack.a26", new("a/b")).is_err());
    assert!(plan("My Hack.a26", new("")).is_err());
    assert!(
        plan("Combat (USA).a26", new("Combat")).is_err(),
        "an item a source set needs is not an addition"
    );
    let missing = env.store.plan_ingest(
        &env.config,
        "in",
        &PlanIngest {
            action: IngestAction::Addition,
            items: vec![env.item_id("My Hack.a26")],
            provider: None,
            additions: BTreeMap::new(),
        },
    );
    assert!(missing.is_err(), "an addition says what to add it as");

    env.approve_ingest(&env.plan_ingest("My Hack.a26", new("Hack")));
    env.ingest("Other.a26", |p| write_file(p, b"other"));
    assert!(
        plan("Other.a26", new("hack")).is_err(),
        "a name is unique in its system, whatever its case"
    );
}

#[test]
fn an_unmatched_source_set_entry_is_kept_aside_by_a_repair_and_added_from_the_kept_file() {
    let env = Env::new();
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    let key = "file:Homebrew.a26";
    let proposed = env
        .store
        .plan_repair(&env.config, SET, &PlanRepair::default())
        .unwrap();
    let item = proposed
        .content
        .items
        .iter()
        .find(|i| i.key == key)
        .unwrap();
    assert_eq!(item.chosen, RepairChoice::KeepAside);
    assert!(
        item.choices.contains(&RepairChoice::AddAddition),
        "{item:?}"
    );
    assert!(
        env.store
            .plan_repair(
                &env.config,
                SET,
                &PlanRepair {
                    choices: BTreeMap::from([(key.to_owned(), RepairChoice::AddAddition)]),
                    additions: BTreeMap::new(),
                }
            )
            .is_err(),
        "adding it says what to add it as"
    );

    let plan = env.plan_repair(key, Some(new("Homebrew")));
    assert_eq!(
        plan.content
            .items
            .iter()
            .find(|i| i.key == key)
            .unwrap()
            .addition,
        Some(new("Homebrew"))
    );
    env.repair(&plan);

    let a = env.only_addition();
    assert_published(&env, &a, "Homebrew", "Homebrew.a26", b"homebrew");
    assert_eq!(a.source, AdditionSource::SourceSet);
    assert_eq!(a.origin, "Homebrew.a26");
    assert!(!env.set().join("Homebrew.a26").exists());
    let kept = env.store.source_kept_files(SET).unwrap();
    assert!(
        kept.iter().any(|k| k.name == "Homebrew.a26"),
        "the kept file stays kept: {kept:#?}"
    );
}

#[test]
fn drift_in_a_library_folder_is_kept_aside_by_a_cleanup_and_added_from_the_kept_file() {
    let env = Env::new();
    env.audit();
    let selection = env.record(&pong(&env.sha256));
    let folder = env.folder();
    env.build(
        &env.store
            .plan_build(&env.config, folder.id, selection)
            .unwrap(),
    );
    let dir = env.play().join("atari");
    write_file(&dir.join("Extra.a26"), b"extra");
    let job = env.store.enqueue_check(folder.id, "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store.run_check_job(&env.config, job.id, None).unwrap();
    let check = env.store.latest_check(folder.id).unwrap().unwrap();
    assert!(
        check
            .drift
            .iter()
            .any(|d| d.name == "Extra.a26" && d.kind == DriftKind::Unknown),
        "{:?}",
        check.drift
    );

    let entries = BTreeMap::from([("Extra.a26".to_owned(), CleanupAction::AddAddition)]);
    let without = env.store.plan_cleanup(
        &env.config,
        folder.id,
        &PlanCleanup {
            entries: entries.clone(),
            ..PlanCleanup::default()
        },
    );
    assert!(
        without.is_err() || !without.unwrap().approvable(),
        "adding it says what to add it as"
    );
    let plan = env
        .store
        .plan_cleanup(
            &env.config,
            folder.id,
            &PlanCleanup {
                entries,
                additions: BTreeMap::from([("Extra.a26".to_owned(), new("Extra"))]),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    env.build_only(&plan);
    let kept = env.store.kept_files(folder.id).unwrap();
    let extra = kept.iter().find(|k| k.name == "Extra.a26").unwrap().id;
    assert!(
        env.store
            .plan_kept_deletion(
                &env.config,
                folder.id,
                &PlanKeptDeletion {
                    ids: vec![extra],
                    all: false
                }
            )
            .is_err(),
        "the kept file an addition is being added from is reserved until its job ends"
    );
    env.settle();

    let a = env.only_addition();
    assert_published(&env, &a, "Extra", "Extra.a26", b"extra");
    assert_eq!(a.source, AdditionSource::Drift);
    assert!(!dir.join("Extra.a26").exists());
    let kept = env.store.kept_files(folder.id).unwrap();
    assert!(
        kept.iter().any(|k| k.name == "Extra.a26"),
        "the kept file stays kept: {kept:#?}"
    );
}

#[test]
fn a_build_reads_the_additions_set_only_once_the_profile_opts_in_for_the_system() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    env.approve_ingest(&env.plan_ingest("My Hack.a26", new("My Hack")));
    let folder = env.folder();
    let dir = env.play().join("atari");

    let off = env.record(&pong(&env.sha256));
    let plan = env.store.plan_build(&env.config, folder.id, off).unwrap();
    assert!(
        plan.content.items.iter().all(|i| i.addition.is_none()),
        "off by default: {:#?}",
        plan.content.items
    );

    env.opt_in(true);
    let on = env.record(&pong(&env.sha256));
    let plan = env.store.plan_build(&env.config, folder.id, on).unwrap();
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.addition.is_some())
        .expect("the addition is built");
    assert_eq!(item.kind, ItemKind::Copy);
    assert_eq!(item.target, "My Hack.zip");
    env.build(&plan);
    assert_eq!(
        members(&dir.join("My Hack.zip")),
        [("My Hack.a26".to_owned(), b"hack".to_vec())]
    );
    assert!(dir.join("Pong (USA).zip").exists());

    // Renamed, it is built under its new name; retired, or opted out, it is no longer built.
    let a = env.only_addition();
    env.store
        .edit_addition(
            a.id,
            &EditAddition {
                name: Some("Renamed Hack".into()),
                parent: Some(Some("Pong (USA)".into())),
            },
        )
        .unwrap();
    let renamed = env.record(&pong(&env.sha256));
    let plan = env
        .store
        .plan_build(&env.config, folder.id, renamed)
        .unwrap();
    assert!(
        plan.content
            .items
            .iter()
            .any(|i| i.target == "Renamed Hack.zip"),
        "{:#?}",
        plan.content.items
    );
    env.build(&plan);
    assert!(dir.join("Renamed Hack.zip").exists());
    assert!(!dir.join("My Hack.zip").exists());

    env.store.retire_addition(a.id).unwrap();
    env.settle();
    let retired = env.record(&pong(&env.sha256));
    let plan = env
        .store
        .plan_build(&env.config, folder.id, retired)
        .unwrap();
    assert!(plan.content.items.iter().all(|i| i.addition.is_none()));
    env.build(&plan);
    assert!(
        !dir.join("Renamed Hack.zip").exists(),
        "a retired addition is removed from the folder"
    );
    assert!(
        !env.additions().join(&a.file_name).exists(),
        "its file leaves the live additions set"
    );
    let kept = env.store.additions_kept_files(SYSTEM).unwrap();
    assert_eq!(kept.len(), 1, "{kept:#?}");
    assert_eq!(
        kept[0].name, a.file_name,
        "its file is kept in the additions set"
    );
}

#[test]
fn an_addition_named_as_a_selected_game_blocks_only_itself() {
    let env = Env::new();
    env.audit();
    env.ingest("hack.a26", |p| write_file(p, b"hack"));
    env.approve_ingest(&env.plan_ingest("hack.a26", new("pong (usa)")));
    env.opt_in(true);
    let folder = env.folder();
    let selection = env.record(&pong(&env.sha256));
    let plan = env
        .store
        .plan_build(&env.config, folder.id, selection)
        .unwrap();
    let kind = |target: &str| {
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.target == target)
            .unwrap();
        (item.kind, item.reason.clone())
    };
    let (blocked, why) = kind("pong (usa).zip");
    assert_eq!(blocked, ItemKind::Blocked);
    assert!(why.unwrap().contains("rename the addition"));
    assert_eq!(
        kind("Pong (USA).zip").0,
        ItemKind::Repack,
        "the game is built as ever"
    );
}

#[test]
fn a_check_of_an_additions_set_reports_drift_and_changes_nothing() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    env.approve_ingest(&env.plan_ingest("My Hack.a26", new("My Hack")));
    let findings = |env: &Env| -> BTreeMap<String, AdditionsFinding> {
        env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| (e.name, e.finding))
            .collect()
    };
    assert_eq!(
        findings(&env),
        BTreeMap::from([("My Hack.zip".to_owned(), AdditionsFinding::Intact)])
    );
    assert!(
        !env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted()
    );

    write_file(&env.additions().join("stray.bin"), b"stray");
    std::fs::create_dir(env.additions().join("folder")).unwrap();
    let before = files(&env.additions());
    assert_eq!(
        findings(&env),
        BTreeMap::from([
            ("My Hack.zip".to_owned(), AdditionsFinding::Intact),
            ("stray.bin".to_owned(), AdditionsFinding::Unknown),
            ("folder".to_owned(), AdditionsFinding::Other),
        ])
    );
    assert_eq!(files(&env.additions()), before, "a check changes nothing");

    // Changed in place, at the same size and with its time put back: only the hashes tell.
    let path = env.additions().join("My Hack.zip");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.set_modified(modified).unwrap();
    }
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(findings(&env)["My Hack.zip"], AdditionsFinding::Changed);
    bytes[last] ^= 0xff;
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.set_modified(modified).unwrap();
    }
    assert_eq!(
        findings(&env)["My Hack.zip"],
        AdditionsFinding::Intact,
        "the same bytes again, re-hashed"
    );

    let replaced = env.additions().join("replacement");
    std::fs::copy(&path, &replaced).unwrap();
    std::fs::rename(&replaced, &path).unwrap();
    assert_eq!(findings(&env)["My Hack.zip"], AdditionsFinding::Changed);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(findings(&env)["My Hack.zip"], AdditionsFinding::Missing);
    assert!(
        env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted()
    );
}

/// Adds `Homebrew.a26` from the Source Set by a repair stopped at `fault`, then restarts and settles it.
fn stopped_at(fault: Fault) -> Env {
    let env = Env::new();
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    let plan = env.plan_repair("file:Homebrew.a26", Some(new("Homebrew")));
    env.repair_only(&plan);
    env.stop_additions_at(fault);
    assert_ne!(env.only_addition().state, AdditionState::Active);
    let env = env.restart();
    // The Additions Job a stop cut off is queued again, and settles what it was settling.
    assert_eq!(env.settle().len(), 1, "{fault:?}");
    env
}

#[test]
fn a_stop_while_an_additions_set_is_created_or_an_addition_published_is_settled_on_restart() {
    for fault in [
        Fault::AdditionSetMade,
        Fault::AdditionSetPublished,
        Fault::AdditionStaged,
        Fault::AdditionRenamed,
    ] {
        let env = stopped_at(fault);
        let a = env.only_addition();
        assert_published(&env, &a, "Homebrew", "Homebrew.a26", b"homebrew");
        let found = env.store.check_additions(&env.config, SYSTEM).unwrap();
        assert!(!found.drifted(), "{fault:?}: {found:#?}");
        let leftover: Vec<String> = files(&env.additions())
            .into_iter()
            .filter(|f| f.contains(".title-select-new-"))
            .collect();
        assert!(leftover.is_empty(), "{fault:?}: {leftover:?}");
        assert!(
            env.store
                .source_kept_files(SET)
                .unwrap()
                .iter()
                .any(|k| k.name == "Homebrew.a26"),
            "{fault:?}: the kept file stays kept"
        );
    }
}

#[test]
fn an_ingest_addition_cut_off_mid_move_is_finished_by_recovery_and_settled() {
    for fault in [
        Fault::IngestRenamed,
        Fault::IngestPublished,
        Fault::IngestIsolated,
    ] {
        let env = Env::new();
        env.audit();
        env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
        let plan = env.plan_ingest("My Hack.a26", new("My Hack"));
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.stop_additions_at(fault);
        assert_eq!(
            env.only_addition().state,
            AdditionState::Pending,
            "{fault:?}"
        );

        let env = env.restart();
        let left = env.store.recover_ingest_moves(&env.config).unwrap();
        assert!(left.is_empty(), "{fault:?}: {left:?}");
        assert_eq!(env.settle().len(), 1, "{fault:?}");
        let done = env.store.ingest_plan(plan.id).unwrap();
        assert!(done.finished_at.is_some(), "{fault:?}: {done:#?}");
        let a = env.only_addition();
        assert_published(&env, &a, "My Hack", "My Hack.a26", b"hack");
        assert!(
            !env.ingest_dir().join("My Hack.a26").exists(),
            "{fault:?}: the original is deleted once the copy is exactly it"
        );
        let found = env.store.check_additions(&env.config, SYSTEM).unwrap();
        assert!(!found.drifted(), "{fault:?}: {found:#?}");
    }
}

#[test]
fn an_ingest_addition_is_published_by_its_additions_job_not_by_the_approval() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    let plan = env.plan_ingest("My Hack.a26", new("My Hack"));
    let approved = env
        .store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert!(approved.finished_at.is_none(), "{approved:#?}");
    assert_eq!(env.only_addition().state, AdditionState::Pending);
    assert!(
        env.ingest_dir().join("My Hack.a26").exists(),
        "nothing moves until the job runs"
    );
    assert!(!env.additions().exists());
    let queued = env.store.queued_additions_jobs().unwrap();
    assert_eq!(queued.len(), 1, "{queued:#?}");
    assert_eq!(queued[0].additions_set.as_deref(), Some(SYSTEM));

    assert_eq!(env.settle().len(), 1);
    assert_published(
        &env,
        &env.only_addition(),
        "My Hack",
        "My Hack.a26",
        b"hack",
    );
    assert!(!env.ingest_dir().join("My Hack.a26").exists());
    let done = env.store.ingest_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some());
    assert!(
        done.outcomes.iter().all(|o| o.moved),
        "{:#?}",
        done.outcomes
    );
}

#[test]
fn a_repair_addition_binds_its_hashes_when_planned_and_fails_if_the_kept_file_changes() {
    let env = Env::new();
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    let plan = env.plan_repair("file:Homebrew.a26", Some(new("Homebrew")));
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "file:Homebrew.a26")
        .unwrap();
    let rom = item
        .addition_rom
        .as_ref()
        .expect("the plan binds the hashes");
    assert_eq!(rom.size, 8);
    assert_eq!(
        rom.sha256.as_deref(),
        Some(
            sha2::Sha256::digest(b"homebrew")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                .as_str()
        )
    );
    env.repair_only(&plan);
    assert_eq!(
        env.only_addition().rom.as_ref(),
        Some(rom),
        "the addition records the hashes the plan bound"
    );

    // Changed in place before its Additions Job runs, at the same size and with its time put back.
    let (_, path) = env.source_kept("Homebrew.a26");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, b"homebrex").unwrap();
        file.set_modified(modified).unwrap();
    }
    env.settle();
    let a = env.only_addition();
    assert_eq!(a.state, AdditionState::Failed, "{a:#?}");
    assert!(
        !env.additions().join("Homebrew.zip").exists(),
        "nothing is published"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"homebrex",
        "the kept file stays kept"
    );
    env.source_kept("Homebrew.a26");
}

#[test]
fn a_kept_file_an_addition_is_added_from_is_reserved_until_its_job_ends_and_may_then_be_deleted() {
    let env = Env::new();
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    env.repair_only(&env.plan_repair("file:Homebrew.a26", Some(new("Homebrew"))));
    let (kept, path) = env.source_kept("Homebrew.a26");
    let pick = PlanKeptDeletion {
        ids: vec![kept],
        all: false,
    };
    assert!(
        env.store
            .plan_source_kept_deletion(&env.config, SET, &pick)
            .is_err(),
        "a reserved kept file is not picked"
    );
    let all = env
        .store
        .plan_source_kept_deletion(
            &env.config,
            SET,
            &PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap();
    assert!(
        all.content.files.iter().all(|f| f.id != kept),
        "nor is it among all of them: {all:#?}"
    );
    assert!(path.exists());

    env.settle();
    let a = env.only_addition();
    assert_published(&env, &a, "Homebrew", "Homebrew.a26", b"homebrew");
    // Once published, its kept copy may be deleted through the approved cleanup; the addition is unaffected.
    let deletion = env
        .store
        .plan_source_kept_deletion(&env.config, SET, &pick)
        .unwrap();
    let done = env
        .store
        .approve_source_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(done.outcomes.iter().all(|o| o.deleted), "{done:#?}");
    assert!(!path.exists());
    assert_published(
        &env,
        &env.only_addition(),
        "Homebrew",
        "Homebrew.a26",
        b"homebrew",
    );
    assert!(
        !env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted()
    );
}

/// An active addition `My Hack`, added from the Ingest Folder.
fn with_hack() -> Env {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    env.approve_ingest(&env.plan_ingest("My Hack.a26", new("My Hack")));
    env
}

fn additions_kept(env: &Env) -> Vec<String> {
    env.store
        .additions_kept_files(SYSTEM)
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect()
}

#[test]
fn a_retired_addition_moves_into_the_kept_files_from_where_it_is_restored_or_deleted() {
    let env = with_hack();
    let a = env.only_addition();
    let retiring = env.store.retire_addition(a.id).unwrap();
    assert_eq!(retiring.state, AdditionState::Retiring);
    assert!(
        env.additions().join("My Hack.zip").exists(),
        "nothing moves until the job runs"
    );
    env.settle();
    assert_eq!(env.only_addition().state, AdditionState::Retired);
    assert!(!env.additions().join("My Hack.zip").exists());
    assert_eq!(additions_kept(&env), ["My Hack.zip"]);
    assert!(
        !env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted(),
        "a check does not read the kept files"
    );

    assert_eq!(
        env.store.restore_addition(a.id).unwrap().state,
        AdditionState::Restoring
    );
    env.settle();
    assert_published(
        &env,
        &env.only_addition(),
        "My Hack",
        "My Hack.a26",
        b"hack",
    );
    assert!(additions_kept(&env).is_empty());
    assert!(
        !env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted()
    );

    env.store.retire_addition(a.id).unwrap();
    env.settle();
    let kept = env.store.additions_kept_files(SYSTEM).unwrap();
    let deletion = env
        .store
        .plan_additions_kept_deletion(
            &env.config,
            SYSTEM,
            &PlanKeptDeletion {
                ids: vec![kept[0].id],
                all: false,
            },
        )
        .unwrap();
    let done = env
        .store
        .approve_additions_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(done.outcomes.iter().all(|o| o.deleted), "{done:#?}");
    assert!(additions_kept(&env).is_empty());
    assert_eq!(
        files(&env.additions()),
        BTreeSet::from([".title-select".to_owned()])
    );
    assert!(
        env.store.restore_addition(a.id).is_err(),
        "a deleted file is not restored"
    );
}

#[test]
fn a_kept_file_of_an_addition_being_restored_is_not_deleted() {
    let env = with_hack();
    let a = env.only_addition();
    env.store.retire_addition(a.id).unwrap();
    env.settle();
    let kept = env.store.additions_kept_files(SYSTEM).unwrap();
    env.store.restore_addition(a.id).unwrap();
    assert!(
        env.store
            .plan_additions_kept_deletion(
                &env.config,
                SYSTEM,
                &PlanKeptDeletion {
                    ids: vec![kept[0].id],
                    all: false,
                },
            )
            .is_err()
    );
    env.settle();
    assert_eq!(env.only_addition().state, AdditionState::Active);
}

#[test]
fn a_stop_while_an_addition_is_retired_or_restored_is_settled_on_restart() {
    let env = with_hack();
    let a = env.only_addition();
    env.store.retire_addition(a.id).unwrap();
    env.stop_additions_at(Fault::AdditionRetireMoved);
    let env = env.restart();
    env.settle();
    assert_eq!(env.only_addition().state, AdditionState::Retired);
    assert!(!env.additions().join("My Hack.zip").exists());
    assert_eq!(additions_kept(&env), ["My Hack.zip"]);

    env.store.restore_addition(a.id).unwrap();
    env.stop_additions_at(Fault::AdditionRestoreMoved);
    let env = env.restart();
    env.settle();
    assert_published(
        &env,
        &env.only_addition(),
        "My Hack",
        "My Hack.a26",
        b"hack",
    );
    assert!(additions_kept(&env).is_empty());
    assert!(
        !env.store
            .check_additions(&env.config, SYSTEM)
            .unwrap()
            .drifted()
    );
}

#[test]
fn a_temporary_additions_folder_each_stop_left_is_recorded_and_never_forgotten() {
    let mut env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    let plan = env.plan_ingest("My Hack.a26", new("My Hack"));
    env.store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    for _ in 0..2 {
        env.stop_additions_at(Fault::AdditionSetMade);
        env = env.restart();
    }
    env.settle();
    assert_published(
        &env,
        &env.only_addition(),
        "My Hack",
        "My Hack.a26",
        b"hack",
    );
    let set = env.store.additions_set(SYSTEM).unwrap();
    assert_eq!(set.leftovers.len(), 2, "{:#?}", set.leftovers);
    let root = env.tmp.path().join("additions");
    for leftover in &set.leftovers {
        assert!(leftover.identity.is_some(), "{leftover:?}");
        assert!(
            root.join(&leftover.name).is_dir(),
            "{leftover:?} is left on disk"
        );
    }
}

#[test]
fn a_stop_while_an_ingest_plan_is_approved_records_none_of_it() {
    let env = Env::new();
    env.audit();
    env.ingest("Hack A.a26", |p| write_file(p, b"hack a"));
    env.ingest("Hack B.a26", |p| write_file(p, b"hack b"));
    let (a, b) = (env.item_id("Hack A.a26"), env.item_id("Hack B.a26"));
    let plan = env
        .store
        .plan_ingest(
            &env.config,
            "in",
            &PlanIngest {
                action: IngestAction::Addition,
                items: vec![a, b],
                provider: None,
                additions: BTreeMap::from([(a, new("Hack A")), (b, new("Hack B"))]),
            },
        )
        .unwrap();
    env.store.inject_fault(Some(Fault::IngestAdditionRecorded));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
    }));
    assert!(ran.is_err(), "the fault was reached: {ran:?}");
    env.store.inject_fault(None);
    let env = env.restart();
    assert!(
        env.store
            .additions_sets()
            .unwrap()
            .iter()
            .all(|s| s.additions.is_empty()),
        "no addition of a plan whose approval stopped is recorded"
    );
    assert!(env.store.claim_job().unwrap().is_none(), "no job is queued");
    assert!(env.store.ingest_plan(plan.id).unwrap().approval.is_none());

    let done = env.approve_ingest(&plan);
    assert!(done.finished_at.is_some(), "{done:#?}");
    assert_eq!(done.outcomes.len(), 2, "{:#?}", done.outcomes);
    assert!(
        done.outcomes.iter().all(|o| o.moved),
        "{:#?}",
        done.outcomes
    );
}

#[test]
fn cancelling_an_additions_job_fails_only_its_members_and_a_later_approval_gets_a_new_job() {
    let env = Env::new();
    env.audit();
    env.ingest("Hack A.a26", |p| write_file(p, b"hack a"));
    env.ingest("Hack B.a26", |p| write_file(p, b"hack b"));
    let plan_a = env.plan_ingest("Hack A.a26", new("Hack A"));
    env.store
        .approve_ingest_plan(&env.config, plan_a.id, &plan_a.digest, "Operator", WORKERS)
        .unwrap();
    let first = env.store.queued_additions_jobs().unwrap();
    assert_eq!(first.len(), 1, "{first:#?}");
    env.store.cancel_job(first[0].id).unwrap();

    let plan_b = env.plan_ingest("Hack B.a26", new("Hack B"));
    env.store
        .approve_ingest_plan(&env.config, plan_b.id, &plan_b.digest, "Operator", WORKERS)
        .unwrap();
    let queued = env.store.queued_additions_jobs().unwrap();
    assert_eq!(
        queued.len(),
        2,
        "a cancelled job takes no new member: {queued:#?}"
    );
    let second = queued.iter().find(|j| j.id != first[0].id).unwrap().id;

    let job = env.store.claim_job().unwrap().unwrap();
    assert_eq!(job.id, first[0].id);
    let ended = env.store.run_additions_job(&env.config, job.id).unwrap();
    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    let state = |name: &str| {
        env.store.additions_sets().unwrap()[0]
            .additions
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .state
    };
    assert_eq!(state("Hack A"), AdditionState::Failed);
    assert_eq!(state("Hack B"), AdditionState::Pending);
    assert!(env.ingest_dir().join("Hack A.a26").exists());
    let a = env.store.ingest_plan(plan_a.id).unwrap();
    assert!(
        a.finished_at.is_some() && a.outcomes.iter().all(|o| !o.moved),
        "{a:#?}"
    );

    let ended = env.settle();
    assert_eq!(ended.iter().map(|j| j.id).collect::<Vec<_>>(), [second]);
    assert_eq!(state("Hack B"), AdditionState::Active);
    assert!(
        env.store
            .ingest_plan(plan_b.id)
            .unwrap()
            .finished_at
            .is_some()
    );
}

#[test]
fn a_restore_rolled_back_keeps_what_it_put_back_listed_as_found_and_never_deletes_it() {
    let env = with_hack();
    let a = env.only_addition();
    env.store.retire_addition(a.id).unwrap();
    env.settle();
    env.store.restore_addition(a.id).unwrap();
    let live = env.additions().join("My Hack.zip");
    let swap = live.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::AdditionRestoreMoved {
                std::fs::remove_file(&swap).unwrap();
                write_file(&swap, b"something else");
            }
        })));
    env.settle();
    env.store.hook_faults(None);
    let a = env.only_addition();
    assert_eq!(a.state, AdditionState::Retired, "{a:#?}");
    assert!(
        !live.exists(),
        "what replaced it was put back among the kept files"
    );

    let kept = env.store.additions_kept_files(SYSTEM).unwrap();
    assert_eq!(kept.len(), 1, "it stays listed: {kept:#?}");
    assert_eq!(kept[0].reason, "found");
    assert_eq!(kept[0].sha256, None, "{kept:#?}");
    let path = env
        .additions()
        .join(RETENTION)
        .join(kept[0].generation.to_string())
        .join(&kept[0].kept_name);
    assert_eq!(std::fs::read(&path).unwrap(), b"something else");
    assert!(env.store.restore_addition(a.id).is_err());

    if let Ok(deletion) = env.store.plan_additions_kept_deletion(
        &env.config,
        SYSTEM,
        &PlanKeptDeletion {
            ids: vec![kept[0].id],
            all: false,
        },
    ) {
        let done = env.store.approve_additions_kept_deletion(
            &env.config,
            deletion.id,
            &deletion.digest,
            "Operator",
        );
        assert!(
            done.is_err() || done.unwrap().outcomes.iter().all(|o| !o.deleted),
            "a file found in place is never deleted"
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"something else");
}

#[test]
fn work_given_to_a_member_while_its_job_runs_goes_to_a_fresh_job_when_it_ends() {
    let env = Env::new();
    env.audit();
    env.ingest("Hack A.a26", |p| write_file(p, b"hack a"));
    env.ingest("Hack B.a26", |p| write_file(p, b"hack b"));
    let (a, b) = (env.item_id("Hack A.a26"), env.item_id("Hack B.a26"));
    let plan = env
        .store
        .plan_ingest(
            &env.config,
            "in",
            &PlanIngest {
                action: IngestAction::Addition,
                items: vec![a, b],
                provider: None,
                additions: BTreeMap::from([(a, new("Hack A")), (b, new("Hack B"))]),
            },
        )
        .unwrap();
    env.store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let addition = |name: &str| {
        env.store.additions_sets().unwrap()[0]
            .additions
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .clone()
    };
    let (at, faults) = std::sync::mpsc::channel::<()>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (at, wait) = (std::sync::Mutex::new(at), std::sync::Mutex::new(wait));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |_: Fault| {
            at.lock().unwrap().send(()).unwrap();
            wait.lock().unwrap().recv().unwrap();
        })));
    let job = env.store.claim_job().unwrap().unwrap();
    let mut retired = false;
    std::thread::scope(|scope| {
        let run = scope.spawn(|| env.store.run_additions_job(&env.config, job.id).unwrap());
        while !run.is_finished() {
            if faults
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
            {
                continue;
            }
            // Once A is published, while the job still runs to publish B, you retire A.
            if !retired && addition("Hack A").state == AdditionState::Active {
                env.store.retire_addition(addition("Hack A").id).unwrap();
                retired = true;
            }
            go.send(()).unwrap();
        }
        assert_eq!(run.join().unwrap().state, JobState::Succeeded);
    });
    env.store.hook_faults(None);
    assert!(retired, "A was retired while the job ran");
    assert_eq!(addition("Hack A").state, AdditionState::Retiring);
    assert_eq!(addition("Hack B").state, AdditionState::Active);
    assert_eq!(
        env.store.queued_additions_jobs().unwrap().len(),
        1,
        "the retirement is handed to a fresh job"
    );
    env.settle();
    assert_eq!(addition("Hack A").state, AdditionState::Retired);
    assert_eq!(additions_kept(&env), ["Hack A.zip"]);
}

#[test]
fn a_cancelled_job_interrupted_by_a_stop_finishes_its_ingest_plan_on_restart() {
    let env = Env::new();
    env.audit();
    env.ingest("My Hack.a26", |p| write_file(p, b"hack"));
    let plan = env.plan_ingest("My Hack.a26", new("My Hack"));
    env.store
        .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let queued = env.store.queued_additions_jobs().unwrap();
    env.store.cancel_job(queued[0].id).unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, queued[0].id);
    let env = env.restart();
    assert_eq!(env.only_addition().state, AdditionState::Failed);
    assert!(env.store.claim_job().unwrap().is_none(), "no job is queued");
    let done = env.store.ingest_plan(plan.id).unwrap();
    assert!(done.finished_at.is_some(), "{done:#?}");
    assert!(
        done.outcomes.len() == 1 && done.outcomes.iter().all(|o| !o.moved && o.error.is_some()),
        "{:#?}",
        done.outcomes
    );
    assert!(env.ingest_dir().join("My Hack.a26").exists());
}

#[test]
fn a_stop_after_a_restore_rollback_puts_the_replacement_back_keeps_it_as_found() {
    let env = with_hack();
    let a = env.only_addition();
    env.store.retire_addition(a.id).unwrap();
    env.settle();
    env.store.restore_addition(a.id).unwrap();
    let live = env.additions().join("My Hack.zip");
    let swap = live.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| match at {
            Fault::AdditionRestoreMoved => {
                std::fs::remove_file(&swap).unwrap();
                write_file(&swap, b"something else");
            }
            Fault::AdditionRestoreReturned => panic!("stopped"),
            _ => {}
        })));
    let job = env.store.claim_job().unwrap().unwrap();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_additions_job(&env.config, job.id)
    }));
    assert!(ran.is_err());
    env.store.hook_faults(None);
    let env = env.restart();
    env.settle();
    assert_eq!(env.only_addition().state, AdditionState::Retired);
    assert!(!live.exists());
    let kept = env.store.additions_kept_files(SYSTEM).unwrap();
    assert_eq!(kept.len(), 1, "what was put back stays listed: {kept:#?}");
    assert_eq!(kept[0].reason, "found");
    assert_eq!(kept[0].sha256, None);
    let path = env
        .additions()
        .join(RETENTION)
        .join(kept[0].generation.to_string())
        .join(&kept[0].kept_name);
    assert_eq!(std::fs::read(&path).unwrap(), b"something else");
    assert!(env.store.restore_addition(a.id).is_err());
}

#[test]
fn a_restore_asked_again_while_its_job_runs_is_handed_to_a_fresh_job() {
    let env = Env::new();
    env.audit();
    env.ingest("Hack A.a26", |p| write_file(p, b"hack a"));
    env.ingest("Hack B.a26", |p| write_file(p, b"hack b"));
    let (a, b) = (env.item_id("Hack A.a26"), env.item_id("Hack B.a26"));
    let plan = env
        .store
        .plan_ingest(
            &env.config,
            "in",
            &PlanIngest {
                action: IngestAction::Addition,
                items: vec![a, b],
                provider: None,
                additions: BTreeMap::from([(a, new("Hack A")), (b, new("Hack B"))]),
            },
        )
        .unwrap();
    env.approve_ingest(&plan);
    let addition = |name: &str| {
        env.store.additions_sets().unwrap()[0]
            .additions
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .clone()
    };
    let (a, b) = (addition("Hack A").id, addition("Hack B").id);
    env.store.retire_addition(a).unwrap();
    env.store.retire_addition(b).unwrap();
    env.settle();
    let conflict = env.additions().join("Hack A.zip");
    write_file(&conflict, b"in the way");
    env.store.restore_addition(a).unwrap();
    env.store.restore_addition(b).unwrap();

    let (at, faults) = std::sync::mpsc::channel::<Fault>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (at, wait) = (std::sync::Mutex::new(at), std::sync::Mutex::new(wait));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |f: Fault| {
            at.lock().unwrap().send(f).unwrap();
            wait.lock().unwrap().recv().unwrap();
        })));
    let job = env.store.claim_job().unwrap().unwrap();
    let mut asked = false;
    std::thread::scope(|scope| {
        let run = scope.spawn(|| env.store.run_additions_job(&env.config, job.id).unwrap());
        while !run.is_finished() {
            let Ok(fault) = faults.recv_timeout(std::time::Duration::from_millis(50)) else {
                continue;
            };
            // While the job restores B, A has gone back to retired: you clear the way and ask again.
            if fault == Fault::AdditionRestoreMoved && !asked {
                assert_eq!(addition("Hack A").state, AdditionState::Retired);
                std::fs::remove_file(&conflict).unwrap();
                env.store.restore_addition(a).unwrap();
                asked = true;
            }
            go.send(()).unwrap();
        }
        assert_eq!(run.join().unwrap().state, JobState::Succeeded);
    });
    env.store.hook_faults(None);
    assert!(asked);
    assert_eq!(addition("Hack A").state, AdditionState::Restoring);
    assert_eq!(addition("Hack B").state, AdditionState::Active);
    assert_eq!(
        env.store.queued_additions_jobs().unwrap().len(),
        1,
        "the restore asked again is handed to a fresh job"
    );
    env.settle();
    assert_eq!(addition("Hack A").state, AdditionState::Active);
}

#[test]
fn an_addition_is_stored_as_the_storage_policy_says_and_built_in_the_play_librarys_format() {
    use title_select_api::storage::{StorageFormat, StoragePolicy, SystemStorage};
    let env = Env::new();
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    let mut policy = StoragePolicy::default();
    policy.source_sets.systems.insert(
        SYSTEM.into(),
        SystemStorage {
            format: Some(StorageFormat::Loose),
            delete_originals: None,
        },
    );
    env.store.set_storage_policy(&policy, "Operator").unwrap();

    // From an ingest folder, by the move, and from a source set entry, from its kept file: each loose, as its ROM
    // under the addition's name.
    env.ingest("hack.a26", |p| write_file(p, b"hack"));
    let plan = env.plan_ingest("hack.a26", new("My Hack"));
    assert!(
        plan.content.items[0].destination.contains("My Hack.a26"),
        "{:?}",
        plan.content.items[0]
    );
    env.approve_ingest(&plan);
    env.repair(&env.plan_repair("file:Homebrew.a26", Some(new("Homebrew"))));
    let sets = env.store.additions_sets().unwrap();
    let mut additions = sets[0].additions.clone();
    additions.sort_by(|a, b| a.name.cmp(&b.name));
    for (a, file, payload) in [
        (&additions[0], "Homebrew.a26", &b"homebrew"[..]),
        (&additions[1], "My Hack.a26", &b"hack"[..]),
    ] {
        assert_eq!(a.state, AdditionState::Active, "{a:#?}");
        assert!(a.loose, "{a:#?}");
        assert_eq!(a.file_name, file);
        assert_eq!(std::fs::read(env.additions().join(file)).unwrap(), payload);
    }
    assert!(
        !env.ingest_dir().join("hack.a26").exists(),
        "the original is deleted once the loose copy is exactly it"
    );

    // A build writes it in its play library's format: TorrentZip by default, loose when the policy says so.
    env.opt_in(true);
    let folder = env.folder();
    let dir = env.play().join("atari");
    let plan = env
        .store
        .plan_build(&env.config, folder.id, env.record(&pong(&env.sha256)))
        .unwrap();
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.game == "My Hack")
        .unwrap();
    assert_eq!((item.target.as_str(), item.loose), ("My Hack.zip", false));
    env.build(&plan);
    assert_eq!(
        members(&dir.join("My Hack.zip")),
        [("hack.a26".to_owned(), b"hack".to_vec())]
    );

    policy.play_libraries.format = StorageFormat::Loose;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let plan = env
        .store
        .plan_build(&env.config, folder.id, env.record(&pong(&env.sha256)))
        .unwrap();
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.game == "My Hack")
        .unwrap();
    assert_eq!((item.target.as_str(), item.loose), ("My Hack.a26", true));
    env.build(&plan);
    assert_eq!(std::fs::read(dir.join("My Hack.a26")).unwrap(), b"hack");
}

/// Sets the storage policy of source sets, and so of Additions Sets, for `system` to `format`.
fn source_sets_store(env: &Env, system: &str, format: title_select_api::storage::StorageFormat) {
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.systems.insert(
        system.into(),
        title_select_api::storage::SystemStorage {
            format: Some(format),
            delete_originals: None,
        },
    );
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

#[test]
fn a_play_library_converts_its_additions_one_at_a_time_deleting_originals_only_when_asked() {
    use title_select_api::storage::StorageFormat;
    let env = Env::new();
    env.audit();
    for (file, name) in [("hack.a26", "My Hack"), ("demo.a26", "Demo")] {
        env.ingest(file, |p| write_file(p, name.as_bytes()));
        env.approve_ingest(&env.plan_ingest(file, new(name)));
    }
    env.opt_in(true);
    let folder = env.folder();
    let dir = env.play().join("atari");
    let selection = env.record(&pong(&env.sha256));
    env.build(
        &env.store
            .plan_build(&env.config, folder.id, selection)
            .unwrap(),
    );
    assert!(dir.join("My Hack.zip").is_file() && dir.join("Demo.zip").is_file());

    // Loose, with originals deleted: each converted file verifies before its original is deleted, and the next
    // conversion keeps its original aside only once the one before is done.
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.format = StorageFormat::Loose;
    policy.play_libraries.delete_originals = true;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let plan = env
        .store
        .plan_build(&env.config, folder.id, selection)
        .unwrap();
    for (game, original) in [("My Hack", "My Hack.zip"), ("Demo", "Demo.zip")] {
        let item = plan.content.items.iter().find(|i| i.game == game).unwrap();
        assert_eq!(
            item.converts.as_ref().map(|k| k.name.as_str()),
            Some(original),
            "{item:#?}"
        );
    }
    assert!(
        plan.content.removals.is_empty(),
        "{:?}",
        plan.content.removals
    );
    let retained = dir.join(".title-select-retained");
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = held.clone();
    let count = move |dir: &Path| -> usize {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|g| std::fs::read_dir(g.path()).map_or(0, |e| e.count()))
            .sum()
    };
    let at = retained.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::ConversionKept(_)) {
                seen.lock().unwrap().push(count(&at));
            }
        })));
    env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(
        *held.lock().unwrap(),
        [1, 1, 1],
        "one original, of Pong and each addition, kept aside at a time"
    );
    assert_eq!(std::fs::read(dir.join("My Hack.a26")).unwrap(), b"My Hack");
    assert_eq!(std::fs::read(dir.join("Demo.a26")).unwrap(), b"Demo");
    assert!(!dir.join("My Hack.zip").exists() && !dir.join("Demo.zip").exists());
    assert!(
        env.store.kept_files(folder.id).unwrap().is_empty(),
        "each original is deleted once its loose file verifies"
    );

    // Back to TorrentZip, with originals kept: each loose file is kept aside, not deleted.
    policy.play_libraries.format = StorageFormat::TorrentZip;
    policy.play_libraries.delete_originals = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let plan = env
        .store
        .plan_build(&env.config, folder.id, selection)
        .unwrap();
    assert!(
        plan.content
            .items
            .iter()
            .filter(|i| i.addition.is_some())
            .all(|i| i.converts.is_some()),
        "{:#?}",
        plan.content.items
    );
    env.build(&plan);
    assert!(dir.join("My Hack.zip").is_file() && dir.join("Demo.zip").is_file());
    let kept: BTreeSet<String> = env
        .store
        .kept_files(folder.id)
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(
        kept,
        BTreeSet::from(["My Hack.a26", "Demo.a26", "Pong (USA).a26"].map(String::from))
    );
}

#[test]
fn an_ingest_addition_is_published_only_where_its_plan_said() {
    use title_select_api::storage::StorageFormat;
    let env = Env::new();
    env.ingest("hack.a26", |p| write_file(p, b"hack"));
    let plan = env.plan_ingest("hack.a26", new("My Hack"));
    let target = plan.content.items[0].addition_target.clone().unwrap();
    assert_eq!(
        (target.file_name.as_str(), target.loose),
        ("My Hack.zip", false)
    );

    source_sets_store(&env, SYSTEM, StorageFormat::Loose);
    let refused =
        env.store
            .approve_ingest_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(title_select_store::Error::Conflict(why)) if why.contains("storage policy changed")),
        "{refused:?}"
    );
    assert!(env.settle().is_empty());
    assert!(
        env.store
            .additions_sets()
            .unwrap()
            .iter()
            .all(|s| s.additions.is_empty())
    );
    assert_eq!(
        std::fs::read(env.ingest_dir().join("hack.a26")).unwrap(),
        b"hack"
    );

    // Planned again, it is reviewed loose, and published so.
    let plan = env.plan_ingest("hack.a26", new("My Hack"));
    assert!(plan.content.items[0].destination.contains("My Hack.a26"));
    env.approve_ingest(&plan);
    assert_eq!(
        std::fs::read(env.additions().join("My Hack.a26")).unwrap(),
        b"hack"
    );
}

#[test]
fn a_repair_addition_of_another_system_is_published_only_where_its_plan_said() {
    use title_select_api::storage::StorageFormat;
    let env = Env::new();
    let dat = write_dat_version(&env.tmp.path().join("dats"), "other.dat", "other", &games());
    env.store.import_dat(&dat, "Other", None).unwrap();
    const OTHER: &str = "Other/Test - System";
    write_file(&env.set().join("Homebrew.a26"), b"homebrew");
    env.audit();
    let plan = env.plan_repair(
        "file:Homebrew.a26",
        Some(NewAddition {
            system: OTHER.into(),
            name: "Homebrew".into(),
            parent: None,
        }),
    );
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.addition.is_some())
        .unwrap();
    assert_eq!(
        item.addition_target
            .as_ref()
            .map(|t| (t.file_name.as_str(), t.loose)),
        Some(("Homebrew.zip", false))
    );

    // Only the other system's policy changes: the repaired source set's own is as the plan saw it.
    source_sets_store(&env, OTHER, StorageFormat::Loose);
    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(title_select_store::Error::Conflict(why)) if why.contains("storage policy changed")),
        "{refused:?}"
    );
    assert!(env.store.claim_job().unwrap().is_none());
    assert!(
        env.store
            .additions_sets()
            .unwrap()
            .iter()
            .all(|s| s.additions.is_empty())
    );
    assert_eq!(
        std::fs::read(env.set().join("Homebrew.a26")).unwrap(),
        b"homebrew"
    );
}

#[test]
fn a_drift_addition_is_published_only_where_its_cleanup_plan_said() {
    use title_select_api::storage::StorageFormat;
    let env = Env::new();
    env.audit();
    let selection = env.record(&pong(&env.sha256));
    let folder = env.folder();
    env.build(
        &env.store
            .plan_build(&env.config, folder.id, selection)
            .unwrap(),
    );
    let dir = env.play().join("atari");
    write_file(&dir.join("Extra.a26"), b"extra");
    let job = env.store.enqueue_check(folder.id, "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store.run_check_job(&env.config, job.id, None).unwrap();
    let plan = env
        .store
        .plan_cleanup(
            &env.config,
            folder.id,
            &PlanCleanup {
                entries: BTreeMap::from([("Extra.a26".to_owned(), CleanupAction::AddAddition)]),
                additions: BTreeMap::from([("Extra.a26".to_owned(), new("Extra"))]),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let entry = plan.content.cleanup.as_ref().unwrap().entries.iter();
    let entry = entry.clone().find(|e| e.name == "Extra.a26").unwrap();
    assert_eq!(
        entry
            .addition_target
            .as_ref()
            .map(|t| (t.file_name.as_str(), t.loose)),
        Some(("Extra.zip", false))
    );

    source_sets_store(&env, SYSTEM, StorageFormat::Loose);
    let refused = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(title_select_store::Error::Conflict(_))),
        "{refused:?}"
    );
    assert!(env.store.claim_job().unwrap().is_none());
    assert!(
        env.store
            .additions_sets()
            .unwrap()
            .iter()
            .all(|s| s.additions.is_empty())
    );
    assert_eq!(std::fs::read(dir.join("Extra.a26")).unwrap(), b"extra");
}
