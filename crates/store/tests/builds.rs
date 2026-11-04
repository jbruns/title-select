//! Library Folders, Change Plans and Builds, from synthetic DATs, Source Sets and hand-made Selections.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
use title_select_api::build::{
    AddLibraryFolder, BlockerKind, BuildState, Cause, CleanupAction, DriftKind, FileOutcome,
    FolderCheck, IncompleteReason, ItemKind, ItemState, KeepReason, LibraryFolder, Plan, PlanBuild,
    PlanChoices, PlanCleanup, PlanKeptDeletion, RemovalKind, RestoreKind,
};
use title_select_api::selection::{
    Alternative, CoverageDrift, Exclusion, Rule, Selection, SelectionGroup, SelectionPins,
    SelectionProfile, SelectionSummary, Winner,
};
use title_select_api::storage::{MULTI_ROM_STAYS_ZIP, StorageFormat, SystemStorage};
use title_select_api::{JobKind, JobState};
use title_select_engine::build::verify;
use title_select_engine::torrentzip::{self, Writer};
use title_select_store::config::Config;
use title_select_store::{Error, Fault, Store, Workers};

const SET: &str = "Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// The Games of the test DAT: each with one or two ROMs.
fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        (
            "Breakout (USA)",
            vec![
                rom("Breakout (USA).a26", b"break"),
                rom("Breakout (USA).txt", b"manual"),
            ],
        ),
        ("Ōkami (Japan)", vec![rom("Ōkami (Japan).a26", b"cafe")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
        ("Missing (USA)", vec![rom("Missing (USA).a26", b"missing")]),
        ("Bad: Name (USA)", vec![rom("Bad (USA).a26", b"bad")]),
        ("Sub (USA)", vec![rom("sub/Sub (USA).a26", b"sub")]),
        ("Back (USA)", vec![rom("..\\Back (USA).a26", b"back")]),
        ("Up (USA)", vec![rom("..", b"up")]),
        ("Root (USA)", vec![rom("/Root (USA).a26", b"root")]),
        ("Device (USA)", vec![rom("CON.a26", b"con")]),
        ("Colon (USA)", vec![rom("C:Colon (USA).a26", b"colon")]),
        ("LPT1", vec![rom("LPT1.a26", b"lpt")]),
        ("aux", vec![rom("aux.a26", b"aux")]),
    ]
}

/// The Games whose names cannot be written into a Library Folder: a ROM name with a path separator, a parent
/// component, an absolute path, a device name or a drive, and Game names that are device names.
const UNSAFE: [&str; 8] = [
    "Sub (USA)",
    "Back (USA)",
    "Up (USA)",
    "Root (USA)",
    "Device (USA)",
    "Colon (USA)",
    "LPT1",
    "aux",
];

/// A store, a read-only Root `sets` holding the Source Set, and a writable Root `play` for Library Folders.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    sha256: String,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        let play = tmp.path().join("roots/play");
        std::fs::create_dir_all(sets.join(SET)).unwrap();
        std::fs::create_dir_all(&play).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = config(&sets, &play, true);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(
            &tmp.path().join("in"),
            "Test - System (1).dat",
            "20260101-000000",
            &games(),
        );
        let (revision, _) = store.import_dat(&dat, "No-Intro", None).unwrap();
        store
            .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
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
        let env = Env {
            tmp,
            store,
            config,
            sha256: revision.sha256,
        };
        env.write_sources();
        env
    }

    fn set_dir(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn play(&self) -> PathBuf {
        self.tmp.path().join("roots/play")
    }

    /// Pong: an exact TorrentZip (copied) and a loose Matched file; Combat: an exact ZIP that is not TorrentZip
    /// (repacked); Breakout: a Misnamed file and a Matched one; Ōkami: a ZIP and a Misnamed file before it in path
    /// order; Yars: only a Conflicting file, named as Combat's ROM. Missing has no source.
    fn write_sources(&self) {
        let dir = self.set_dir();
        torrentzip_file(&dir.join("Pong (USA).zip"), &[("Pong (USA).a26", b"pong")]);
        write_file(&dir.join("Pong (USA).a26"), b"pong");
        write_stored_zip(
            &dir.join("Combat (USA).zip"),
            &[("Combat (USA).a26", b"combat")],
        );
        write_file(&dir.join("breakout.bin"), b"break");
        write_file(&dir.join("Breakout (USA).txt"), b"manual");
        write_zip(
            &dir.join("Ōkami (Japan).zip"),
            &[("Ōkami (Japan).a26", b"cafe")],
        );
        write_file(&dir.join("a-cafe.bin"), b"cafe");
        write_file(&dir.join("Combat (USA).a26"), b"yars");
    }

    fn audit(&self) {
        self.store
            .audit_source_set(
                &self.config,
                SET,
                None,
                title_select_engine::AuditEvidence::Incremental,
            )
            .unwrap();
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

    fn folder(&self, path: &str) -> LibraryFolder {
        self.store
            .add_library_folder(&self.config, &add(path))
            .unwrap()
    }

    /// Approves `plan`, claims its Build Job and runs it with `progress`.
    fn build_with(
        &self,
        plan: &Plan,
        progress: &(dyn Fn(title_select_engine::Progress) + Sync),
    ) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store
            .run_build_job(&self.config, job.id, Some(progress))
            .unwrap()
    }

    /// Approves `plan` and runs its Build Job until `fault` stops it as a kill would. Returns the Job's id.
    fn crash(&self, plan: &Plan, fault: Fault) -> i64 {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store.inject_fault(Some(fault));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.store.run_build_job(&self.config, job.id, None)
        }));
        assert!(ran.is_err(), "{ran:?}");
        job.id
    }

    /// Opens the store again, as the service does when it starts, and recovers. Returns the Builds whose folders
    /// recovery left as they were.
    fn restart(self) -> (Env, Vec<(i64, String)>) {
        let (env, blocked) = self.restart_until(None);
        (env, blocked.unwrap())
    }

    /// Restarts with `stop` hooked to the faults recovery passes; `None` for the blocked folders when it stopped.
    fn restart_until(self, stop: Option<Fault>) -> (Env, Option<Vec<(i64, String)>>) {
        self.restart_hooked(stop.map(|stop| {
            std::sync::Arc::new(move |fault| {
                if fault == stop {
                    panic!("killed at {fault:?}");
                }
            }) as title_select_store::FaultHook
        }))
    }

    /// Restarts with `hook` run at the faults recovery passes; `None` for the blocked folders when it panicked.
    fn restart_hooked(
        self,
        hook: Option<title_select_store::FaultHook>,
    ) -> (Env, Option<Vec<(i64, String)>>) {
        let Env {
            tmp,
            store,
            config,
            sha256,
        } = self;
        let path = store.dir().to_path_buf();
        drop(store);
        let store = Store::open(&path).unwrap();
        if hook.is_some() {
            store.hook_faults(hook);
        }
        let blocked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.recover_jobs(WORKERS).unwrap();
            let blocked = store.recover_builds(&config).unwrap();
            store.recover_kept_deletions(&config).unwrap();
            blocked
        }))
        .ok();
        store.hook_faults(None);
        (
            Env {
                tmp,
                store,
                config,
                sha256,
            },
            blocked,
        )
    }

    fn plan(&self, folder: &LibraryFolder, selection: i64) -> Plan {
        self.store
            .plan_build(&self.config, folder.id, selection)
            .unwrap()
    }

    /// Approves `plan`, then claims and runs its Build Job.
    fn build(&self, plan: &Plan) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(job.kind, JobKind::Build);
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        self.store
            .run_build_job(&self.config, job.id, None)
            .unwrap()
    }
}

fn config(sets: &Path, play: &Path, writable: bool) -> Config {
    Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = {writable}\n",
        sets.display().to_string(),
        play.display().to_string(),
    ))
    .unwrap()
}

fn add(path: &str) -> AddLibraryFolder {
    AddLibraryFolder {
        root: "play".into(),
        path: Some(path.into()),
        template: None,
        source_set: SET.into(),
        profile: PROFILE.into(),
    }
}

fn torrentzip_file(path: &Path, members: &[(&str, &[u8])]) {
    let mut writer = Writer::new(std::fs::File::create(path).unwrap()).unwrap();
    for (name, payload) in members {
        writer
            .add(name, payload.len() as u64, &mut &payload[..])
            .unwrap();
    }
    writer.finish().unwrap();
}

fn validate(path: &Path) -> Vec<torrentzip::Member> {
    torrentzip::validate(&mut std::fs::File::open(path).unwrap()).unwrap()
}

fn rule(stage: &str) -> Rule {
    Rule {
        stage: stage.into(),
        reference: Some("REF0001".into()),
        message: "m".into(),
    }
}

fn group(title: &str) -> SelectionGroup {
    SelectionGroup {
        group: title.to_lowercase(),
        unresolved: false,
        winners: vec![Winner {
            title: title.into(),
            rule: rule("Parent selection"),
            unresolved: false,
        }],
        alternatives: vec![],
        exclusions: vec![],
        choice: None,
        drift: false,
    }
}

fn selection(sha256: &str, groups: Vec<SelectionGroup>) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups,
        stale_choices: BTreeMap::new(),
    }
}

/// Every Game of the DAT a winner.
fn everything(sha256: &str) -> Selection {
    selection(
        sha256,
        games()
            .iter()
            .filter(|(g, _)| !UNSAFE.contains(g))
            .map(|(g, _)| group(g))
            .collect(),
    )
}

fn kinds(plan: &Plan) -> BTreeMap<String, ItemKind> {
    plan.content
        .items
        .iter()
        .map(|i| (i.game.clone(), i.kind))
        .collect()
}

fn blockers(plan: &Plan) -> BTreeSet<BlockerKind> {
    plan.content.blockers.iter().map(|b| b.kind).collect()
}

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn marker(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(".title-select")).unwrap()).unwrap()
}

#[test]
fn a_selection_is_built_from_matched_misnamed_and_conflicting_entries_as_torrentzips_under_dat_names()
 {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(names(&dir), BTreeSet::from([".title-select".to_owned()]));
    assert_eq!(marker(&dir)["generation"], 0);
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();

    let plan = env.plan(&folder, id);

    assert_eq!(
        kinds(&plan),
        BTreeMap::from([
            ("Pong (USA)".to_owned(), ItemKind::Copy),
            ("Combat (USA)".to_owned(), ItemKind::Repack),
            ("Breakout (USA)".to_owned(), ItemKind::Repack),
            ("Ōkami (Japan)".to_owned(), ItemKind::Repack),
            ("Yars (USA)".to_owned(), ItemKind::Repack),
            ("Missing (USA)".to_owned(), ItemKind::NoSource),
            ("Bad: Name (USA)".to_owned(), ItemKind::Blocked),
        ])
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let source = |game: &str| {
        let item = plan.content.items.iter().find(|i| i.game == game).unwrap();
        item.sources
            .iter()
            .map(|s| (s.path.clone(), s.finding.expect("a source set entry")))
            .collect::<Vec<_>>()
    };
    use title_select_api::Finding::*;
    // A copy-able ZIP over a loose Matched file before it in path order; Matched over an earlier Misnamed file.
    assert_eq!(source("Pong (USA)"), [("Pong (USA).zip".into(), Matched)]);
    assert_eq!(
        source("Ōkami (Japan)"),
        [("Ōkami (Japan).zip".into(), Matched)]
    );
    assert_eq!(
        source("Breakout (USA)"),
        [
            ("breakout.bin".into(), Misnamed),
            ("Breakout (USA).txt".into(), Matched)
        ]
    );
    assert_eq!(
        source("Yars (USA)"),
        [("Combat (USA).a26".into(), Conflicting)]
    );

    let before = SystemTime::now() - Duration::from_secs(2);
    let job = env.build(&plan);
    let after = SystemTime::now() + Duration::from_secs(2);

    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error);
    assert_eq!(
        names(&dir),
        BTreeSet::from(
            [
                ".title-select",
                "notes.txt",
                "Pong (USA).zip",
                "Combat (USA).zip",
                "Breakout (USA).zip",
                "Ōkami (Japan).zip",
                "Yars (USA).zip",
            ]
            .map(String::from)
        ),
        "no staging is left, nothing is written for the no-source or blocked items"
    );
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(dir.join("Pong (USA).zip")).unwrap(),
        std::fs::read(env.set_dir().join("Pong (USA).zip")).unwrap(),
        "an exact TorrentZip is copied as it is"
    );
    for item in plan.content.items.iter().filter(|i| i.kind.writes()) {
        let path = dir.join(&item.target);
        validate(&path);
        verify(&std::fs::File::open(&path).unwrap(), &item.roms).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(before <= mtime && mtime <= after, "{}", item.target);
    }
    let okami = validate(&dir.join("Ōkami (Japan).zip"));
    assert!(okami[0].utf8, "a non-ASCII name carries the UTF-8 flag");
    let combat = validate(&dir.join("Combat (USA).zip"));
    assert!(!combat[0].utf8);
    assert_eq!(marker(&dir)["generation"], 1);

    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.generation, 1);
    assert_eq!(folder.files, 5);
    let build = folder.build.as_ref().unwrap();
    assert_eq!(build.state, BuildState::Succeeded);
    assert_eq!((build.applied, build.failed, build.pending), (5, 0, 0));
    assert_eq!(build.planned.no_source, 1);
    assert_eq!(build.planned.blocked, 1);
    let files = env.store.built_files(folder.id).unwrap();
    let pong = files.iter().find(|f| f.game == "Pong (USA)").unwrap();
    assert_eq!(pong.kind, ItemKind::Copy);
    assert_eq!((pong.selection, Some(pong.audit)), (id, plan.content.audit));
    assert_eq!(pong.dat_revision, env.sha256);
    assert_eq!(pong.sources[0].path, "Pong (USA).zip");
    assert_eq!(
        pong.file.size,
        std::fs::metadata(dir.join("Pong (USA).zip")).unwrap().len()
    );

    // Planned again with nothing changed, every built file is unchanged.
    let again = env.plan(&folder, id);
    for (before, now) in plan.content.items.iter().zip(&again.content.items) {
        let expected = if before.kind.writes() {
            ItemKind::Unchanged
        } else {
            before.kind
        };
        assert_eq!(now.kind, expected, "{}", now.target);
    }
}

#[test]
fn the_digest_binds_the_plan_and_a_stale_approval_is_rejected_with_the_new_plan() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");

    let plan = env.plan(&folder, id);
    let same = env.plan(&folder, id);
    assert_eq!(
        plan.digest, same.digest,
        "the same inputs make the same plan"
    );
    assert_eq!(plan.digest.len(), 64);
    let bytes = serde_json::to_vec(&plan.content).unwrap();
    let hex: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(plan.digest, hex);

    let wrong = env
        .store
        .approve_plan(&env.config, plan.id, &"0".repeat(64), "Operator", WORKERS);
    assert!(matches!(wrong, Err(Error::Conflict(_))), "{wrong:?}");

    // A source's mtime is part of the plan.
    let source = env.set_dir().join("Combat (USA).zip");
    let file = std::fs::File::options().write(true).open(&source).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(file);
    let stale = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    let Err(Error::Conflict(message)) = stale else {
        panic!("{stale:?}")
    };
    let newer = env.store.plan(plan.id + 2).unwrap();
    assert!(
        message.contains(&format!("review plan {}", newer.id)),
        "{message}"
    );
    assert_ne!(newer.digest, plan.digest);
    assert!(env.store.plan(plan.id).unwrap().approval.is_none());

    // Approved, then changed before the Build runs: the Job fails and nothing is written.
    let job = env
        .store
        .approve_plan(&env.config, newer.id, &newer.digest, "Operator", WORKERS)
        .unwrap();
    std::fs::write(env.play().join("atari2600/Pong (USA).zip"), b"theirs").unwrap();
    env.store.claim_job().unwrap();
    let failed = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains("the plan changed since you approved it"),
        "{:?}",
        failed.error
    );
    assert_eq!(
        names(&env.play().join("atari2600")),
        BTreeSet::from([".title-select".to_owned(), "Pong (USA).zip".to_owned()])
    );
}

#[test]
fn an_unmanaged_file_at_a_target_is_a_conflict_left_untouched() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    std::fs::write(dir.join("combat (usa).zip"), b"theirs").unwrap();

    let plan = env.plan(&folder, id);

    let combat = plan
        .content
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert_eq!(combat.kind, ItemKind::Conflict);
    assert!(plan.approvable());
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        std::fs::read(dir.join("combat (usa).zip")).unwrap(),
        b"theirs"
    );
    assert!(!names(&dir).contains("Combat (USA).zip"));
}

#[test]
fn a_dat_revision_mismatch_and_an_unresolved_group_block_the_plan() {
    let env = Env::new();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");

    let unaudited = env.plan(&folder, id);
    assert_eq!(blockers(&unaudited), BTreeSet::from([BlockerKind::NoAudit]));

    // The latest Audit is against a newer revision.
    let mut newer = games();
    newer.push(("Extra (USA)", vec![rom("Extra (USA).a26", b"extra")]));
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    env.store.import_dat(&dat, "No-Intro", None).unwrap();
    env.audit();
    let mismatched = env.plan(&folder, id);
    assert_eq!(
        blockers(&mismatched),
        BTreeSet::from([BlockerKind::RevisionMismatch])
    );
    assert!(mismatched.content.items.is_empty());
    let refused = env.store.approve_plan(
        &env.config,
        mismatched.id,
        &mismatched.digest,
        "Operator",
        WORKERS,
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");

    let env = Env::new();
    env.audit();
    let mut unresolved = everything(&env.sha256);
    unresolved.groups[1].unresolved = true;
    let id = env.record(&unresolved);
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Unresolved]));
    assert_eq!(
        plan.content.blockers[0].group.as_deref(),
        Some("combat (usa)")
    );
    assert!(!plan.approvable());
}

const RETAINED: &str = ".title-select-retained";

/// Builds every game into a new folder, then plans the selection of Pong alone: every other built file is no
/// longer selected.
fn built_then_deselected(env: &Env) -> (LibraryFolder, Plan) {
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let fewer = env.record(&selection(&env.sha256, vec![group("Pong (USA)")]));
    let plan = env.plan(&folder, fewer);
    (folder, plan)
}

#[test]
fn a_deselected_group_is_removed_by_keeping_its_file_aside() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    let built: BTreeSet<String> = env
        .store
        .built_files(folder.id)
        .unwrap()
        .into_iter()
        .map(|f| f.target)
        .filter(|t| t != "Pong (USA).zip")
        .collect();
    assert!(built.len() >= 3, "{built:?}");
    let bytes: BTreeMap<String, Vec<u8>> = built
        .iter()
        .map(|t| (t.clone(), std::fs::read(dir.join(t)).unwrap()))
        .collect();
    let gone = built.iter().next().unwrap().clone();
    std::fs::remove_file(dir.join(&gone)).unwrap();
    let plan = env.plan(&folder, plan.content.selection);

    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let removals: BTreeMap<&str, RemovalKind> = plan
        .content
        .removals
        .iter()
        .map(|r| (r.target.as_str(), r.kind))
        .collect();
    let expected: BTreeMap<&str, RemovalKind> = built
        .iter()
        .map(|t| {
            let kind = if *t == gone {
                RemovalKind::Forget
            } else {
                RemovalKind::Keep
            };
            (t.as_str(), kind)
        })
        .collect();
    assert_eq!(removals, expected);
    assert_eq!(plan.summary.remove, built.len() as u64);
    assert_eq!(plan.summary.keep, built.len() as u64 - 1);
    assert!(plan.content.items.iter().all(|i| i.keep.is_none()));

    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    assert_eq!(
        names(&dir),
        BTreeSet::from([
            ".title-select".to_string(),
            RETAINED.to_string(),
            "Pong (USA).zip".to_string()
        ])
    );
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(kept.len(), built.len() - 1);
    let generation = dir.join(RETAINED).join("2");
    for k in &kept {
        assert_eq!(
            (k.generation, k.reason, k.kept_name.clone()),
            (2, KeepReason::Removal, format!("{}.kept", k.name))
        );
        assert_eq!(
            std::fs::read(generation.join(&k.kept_name)).unwrap(),
            bytes[&k.name]
        );
        assert!(k.built_by.is_some() && k.game.is_some());
    }
    let after = env.store.built_files(folder.id).unwrap();
    assert_eq!(after.len(), 1);
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!((folder.generation, folder.incomplete.clone()), (2, None));
    // The retention area is never drift, and its files are not built files.
    let checked = check(&env, &folder);
    assert!(checked.drift.is_empty(), "{:?}", checked.drift);
    assert_eq!(checked.files.len(), 1);
    // Planned again, nothing is left to remove.
    let again = env.plan(&folder, plan.content.selection);
    assert!(again.content.removals.is_empty());
}

#[test]
fn a_kept_file_is_deleted_only_once_you_approve_its_deletion() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let path = |k: &title_select_api::build::KeptFile| {
        dir.join(RETAINED)
            .join(k.generation.to_string())
            .join(&k.kept_name)
    };

    let one = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![kept[0].id],
                all: false,
            },
        )
        .unwrap();
    assert_eq!(one.content.files, vec![kept[0].clone()]);
    assert!(path(&kept[0]).exists(), "planning deletes nothing");
    let stale = env
        .store
        .approve_kept_deletion(&env.config, one.id, "0000", "Operator");
    assert!(matches!(stale, Err(Error::Conflict(_))), "{stale:?}");
    assert!(path(&kept[0]).exists());

    let done = env
        .store
        .approve_kept_deletion(&env.config, one.id, &one.digest, "Operator")
        .unwrap();
    assert!(
        done.outcomes.iter().all(|o| o.deleted),
        "{:?}",
        done.outcomes
    );
    assert!(!path(&kept[0]).exists());
    assert!(kept[1..].iter().all(|k| path(k).exists()));
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept[1..].to_vec());

    let rest = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap();
    let done = env
        .store
        .approve_kept_deletion(&env.config, rest.id, &rest.digest, "Operator")
        .unwrap();
    assert!(
        done.outcomes.iter().all(|o| o.deleted),
        "{:?}",
        done.outcomes
    );
    assert!(env.store.kept_files(folder.id).unwrap().is_empty());
    // The emptied folders are left in place, and are never drift.
    assert_eq!(
        names(&dir.join(RETAINED)),
        BTreeSet::from([".deleting".to_string(), "2".to_string()])
    );
    assert!(names(&dir.join(RETAINED).join("2")).is_empty());
    let checked = check(
        &env,
        &env.store.library_folder(&env.config, folder.id).unwrap(),
    );
    assert!(checked.drift.is_empty(), "{:?}", checked.drift);
    assert_eq!(checked.files.len(), 1);
    let again = env
        .store
        .approve_kept_deletion(&env.config, rest.id, &rest.digest, "Operator");
    assert!(matches!(again, Err(Error::Refused(_))), "{again:?}");
}

#[test]
fn a_substituted_retention_entry_is_never_deleted() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    assert!(kept.len() >= 3);
    let generation = dir.join(RETAINED).join("2");
    let deletion = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap();
    // After you review the deletion: one kept file is swapped for another file, one for a symlink to a file
    // outside the folder, and one for a directory.
    let outside = env.tmp.path().join("precious");
    std::fs::write(&outside, b"precious").unwrap();
    let swapped = generation.join(&kept[0].kept_name);
    std::fs::remove_file(&swapped).unwrap();
    std::fs::write(&swapped, b"someone else's").unwrap();
    let linked = generation.join(&kept[1].kept_name);
    std::fs::remove_file(&linked).unwrap();
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    let made_dir = generation.join(&kept[2].kept_name);
    std::fs::remove_file(&made_dir).unwrap();
    std::fs::create_dir(&made_dir).unwrap();

    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();

    for o in &done.outcomes[..3] {
        assert!(!o.deleted, "{o:?}");
        assert!(
            o.detail.as_deref().unwrap().contains("never deleted"),
            "{o:?}"
        );
    }
    assert!(done.outcomes[3..].iter().all(|o| o.deleted));
    assert_eq!(std::fs::read(&swapped).unwrap(), b"someone else's");
    assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), b"precious");
    assert!(made_dir.is_dir());
    let left: Vec<i64> = env
        .store
        .kept_files(folder.id)
        .unwrap()
        .iter()
        .map(|k| k.id)
        .collect();
    assert_eq!(left, vec![kept[0].id, kept[1].id, kept[2].id]);

    // A retention area swapped for a symlink is never followed: the plan is blocked and nothing is deleted.
    let env2 = Env::new();
    let (folder, plan) = built_then_deselected(&env2);
    let dir = env2.play().join("atari2600");
    assert_eq!(env2.build(&plan).state, JobState::Succeeded);
    let deletion = env2
        .store
        .plan_kept_deletion(
            &env2.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap();
    let elsewhere = env2.tmp.path().join("elsewhere");
    std::fs::rename(dir.join(RETAINED), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(RETAINED)).unwrap();
    let before = names(&elsewhere.join("2"));
    let done = env2
        .store
        .approve_kept_deletion(&env2.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(
        done.outcomes.iter().all(|o| !o.deleted),
        "{:?}",
        done.outcomes
    );
    assert_eq!(names(&elsewhere.join("2")), before);
    let blocked = env2.plan(&folder, plan.content.selection);
    assert!(
        blocked
            .content
            .blockers
            .iter()
            .any(|b| b.message.contains(RETAINED))
    );
}

#[test]
fn a_replacement_keeps_the_old_file_and_writes_the_new_one_in_one_plan() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let old = std::fs::read(dir.join("Pong (USA).zip")).unwrap();

    // The newer revision keeps Pong's title but lists another ROM for it, which the source set now has.
    let mut newer = games();
    newer
        .iter_mut()
        .find(|(game, _)| *game == "Pong (USA)")
        .unwrap()
        .1 = vec![rom("Pong (USA).a26", b"pong, revised")];
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    let (revision, _) = env.store.import_dat(&dat, "No-Intro", None).unwrap();
    write_file(&env.set_dir().join("pong-revised.bin"), b"pong, revised");
    env.store
        .audit_source_set(
            &env.config,
            SET,
            Some(&revision.sha256),
            title_select_engine::AuditEvidence::Incremental,
        )
        .unwrap();
    let id = env.record(&selection(
        &revision.sha256,
        vec![group("Pong (USA)"), group("Combat (USA)")],
    ));
    let plan = env.plan(&folder, id);

    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let pong = plan
        .content
        .items
        .iter()
        .find(|i| i.game == "Pong (USA)")
        .unwrap();
    assert!(pong.kind.writes(), "{pong:?}");
    let keep = pong.keep.as_ref().unwrap();
    assert_eq!(
        (keep.reason, keep.name.as_str()),
        (KeepReason::Replacement, "Pong (USA).zip")
    );
    assert_eq!(plan.summary.keep, 1);
    assert!(plan.content.removals.is_empty());

    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let members = validate(&dir.join("Pong (USA).zip"));
    assert_eq!(members[0].name, "Pong (USA).a26");
    assert_eq!(members[0].size, b"pong, revised".len() as u64);
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("2").join("Pong (USA).zip.kept")).unwrap(),
        old
    );
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].reason, kept[0].generation),
        (KeepReason::Replacement, 2)
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete, None);
    assert_eq!(
        kinds(&env.plan(&folder, id))["Pong (USA)"],
        ItemKind::Unchanged
    );
}

#[test]
fn a_conflict_is_kept_aside_and_rebuilt_only_when_you_choose_it() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    std::fs::write(dir.join("Combat (USA).zip"), b"theirs").unwrap();

    let left = env.plan(&folder, id);
    let combat = left
        .content
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert_eq!(
        (combat.kind, combat.keepable, combat.keep.is_some()),
        (ItemKind::Conflict, true, false)
    );
    assert_eq!(left.summary.keep, 0);

    let keep = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: false,
            adopt: vec![],
            keep_aside: vec!["Combat (USA).zip".into()],
            remove_all: false,
        },
    );
    assert_ne!(keep.digest, left.digest);
    let combat = keep
        .content
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert!(combat.kind.writes(), "{combat:?}");
    assert_eq!(combat.keep.as_ref().unwrap().reason, KeepReason::Conflict);
    // The choice is part of the digest: the plan you did not choose builds nothing over the conflict.
    assert_eq!(env.build(&left).state, JobState::Succeeded);
    assert_eq!(
        std::fs::read(dir.join("Combat (USA).zip")).unwrap(),
        b"theirs"
    );
    assert!(!dir.join(RETAINED).exists());

    let keep = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: false,
            adopt: vec![],
            keep_aside: vec!["Combat (USA).zip".into()],
            remove_all: false,
        },
    );
    assert_eq!(env.build(&keep).state, JobState::Succeeded);

    validate(&dir.join("Combat (USA).zip"));
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("2").join("Combat (USA).zip.kept")).unwrap(),
        b"theirs"
    );
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(
        (kept.len(), kept[0].reason, kept[0].built_by),
        (1, KeepReason::Conflict, None)
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete, None);

    // Choosing to adopt and to keep aside the same file is refused.
    let both = env.store.plan_build_with(
        &env.config,
        folder.id,
        &PlanBuild {
            selection: id,
            choices: PlanChoices {
                resolved_only: false,
                adopt: vec!["Combat (USA).zip".into()],
                keep_aside: vec!["Combat (USA).zip".into()],
                remove_all: false,
            },
        },
    );
    assert!(both.is_err(), "{both:?}");
}

#[test]
fn a_crash_mid_removal_leaves_the_file_in_place_or_kept_and_recovery_reports_it() {
    for fault in [Fault::BeforeKeep(0), Fault::AfterKeep(0)] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        let first = plan.content.removals[0].target.clone();
        let bytes = std::fs::read(dir.join(&first)).unwrap();
        env.crash(&plan, fault);
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{fault:?}: {blocked:?}");

        let badged = env.store.library_folder(&env.config, folder.id).unwrap();
        let unfinished = badged.unfinished.clone().unwrap();
        let detail = env.store.build(unfinished.build).unwrap();
        let item = detail.items.iter().find(|i| i.target == first).unwrap();
        let kept = env.store.kept_files(folder.id).unwrap();
        let retained = dir.join(RETAINED).join("2").join(format!("{first}.kept"));
        match fault {
            Fault::BeforeKeep(_) => {
                assert_eq!(item.state, ItemState::Failed);
                assert!(
                    item.error.as_deref().unwrap().contains("still in place"),
                    "{item:?}"
                );
                assert_eq!(std::fs::read(dir.join(&first)).unwrap(), bytes);
                assert!(!retained.exists());
                assert!(kept.is_empty());
                assert_eq!(unfinished.kept, 0);
                assert!(
                    env.store
                        .built_files(folder.id)
                        .unwrap()
                        .iter()
                        .any(|f| f.target == first)
                );
            }
            _ => {
                assert_eq!(item.state, ItemState::Applied);
                assert!(
                    item.error.as_deref().unwrap().contains("kept as"),
                    "{item:?}"
                );
                assert!(!dir.join(&first).exists());
                assert_eq!(std::fs::read(&retained).unwrap(), bytes);
                assert_eq!(kept.len(), 1);
                assert_eq!(kept[0].name, first);
                assert_eq!(unfinished.kept, 1);
                assert!(
                    !env.store
                        .built_files(folder.id)
                        .unwrap()
                        .iter()
                        .any(|f| f.target == first)
                );
            }
        }

        // A new plan finishes the removals.
        let next = env.plan(&badged, plan.content.selection);
        assert!(next.approvable(), "{fault:?}: {:?}", next.content.blockers);
        assert_eq!(env.build(&next).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        assert_eq!(kept.len(), plan.content.removals.len(), "{fault:?}");
        assert!(kept.iter().any(|k| k.name == first));
        let done = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!((done.unfinished, done.incomplete), (None, None));
    }
}

/// Plans deleting every kept file of `folder`.
fn plan_delete_all(env: &Env, folder: &LibraryFolder) -> title_select_api::build::KeptDeletion {
    env.store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap()
}

/// Approves `deletion` until `fault` stops it as a kill would.
fn crash_deletion(env: &Env, deletion: &title_select_api::build::KeptDeletion, fault: Fault) {
    env.store.inject_fault(Some(fault));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
}

#[test]
fn a_kept_file_swapped_just_before_it_is_deleted_is_moved_back_and_never_deleted() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let generation = dir.join(RETAINED).join("2");
    let swapped = generation.join(&kept[0].kept_name);
    let deletion = plan_delete_all(&env, &folder);
    // After every check, just before the kept file is moved to be deleted, another file takes its place.
    let target = kept[0].id;
    let at = swapped.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforeDeleteMove(target) {
                std::fs::remove_file(&at).unwrap();
                std::fs::write(&at, b"someone else's").unwrap();
            }
        })));

    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    env.store.hook_faults(None);

    let detail = done.outcomes[0].detail.as_deref().unwrap();
    assert!(!done.outcomes[0].deleted);
    assert!(
        detail.contains("never deleted") && detail.contains("moved back"),
        "{detail}"
    );
    assert!(
        done.outcomes[1..].iter().all(|o| o.deleted),
        "{:?}",
        done.outcomes
    );
    assert_eq!(std::fs::read(&swapped).unwrap(), b"someone else's");
    assert_eq!(
        env.store
            .kept_files(folder.id)
            .unwrap()
            .iter()
            .map(|k| k.id)
            .collect::<Vec<_>>(),
        vec![target]
    );
    assert_eq!(
        names(&dir.join(RETAINED)),
        BTreeSet::from([".deleting".to_string(), "2".to_string()])
    );
    assert!(names(&dir.join(RETAINED).join(".deleting")).is_empty());
}

#[test]
fn recovery_syncs_both_folders_before_it_deletes_a_kept_file_a_crash_left_aside() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let first = kept[0].clone();
    let slot = env
        .play()
        .join("atari2600")
        .join(RETAINED)
        .join(".deleting")
        .join(first.id.to_string());
    let deletion = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![first.id],
                all: false,
            },
        )
        .unwrap();
    crash_deletion(&env, &deletion, Fault::AfterDeleteMove(first.id));
    assert!(slot.exists());

    let (env, stopped) = env.restart_until(Some(Fault::KeptDeletionSynced(first.id)));

    assert!(
        stopped.is_none(),
        "recovery never synced both folders first"
    );
    assert!(slot.exists(), "deleted before both folders were synced");
    let (env, _) = env.restart();
    let done = env.store.kept_deletion(deletion.id).unwrap();
    assert!(done.outcomes[0].deleted, "{:?}", done.outcomes);
    assert!(!slot.exists());
}

#[test]
fn a_crash_mid_deletion_never_loses_a_kept_file_and_recovery_settles_it() {
    for at in ["before the move", "after the move", "after the unlink"] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        assert_eq!(env.build(&plan).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        let first = kept[0].clone();
        let path = dir.join(RETAINED).join("2").join(&first.kept_name);
        let bytes = std::fs::read(&path).unwrap();
        let deletion = env
            .store
            .plan_kept_deletion(
                &env.config,
                folder.id,
                &PlanKeptDeletion {
                    ids: vec![first.id],
                    all: false,
                },
            )
            .unwrap();
        let fault = match at {
            "before the move" => Fault::BeforeDeleteMove(first.id),
            "after the move" => Fault::AfterDeleteMove(first.id),
            _ => Fault::AfterUnlink(first.id),
        };
        crash_deletion(&env, &deletion, fault);
        let unfinished = env.store.kept_deletion(deletion.id).unwrap();
        assert!(unfinished.finished_at.is_none(), "{at}");
        let again = env.plan(&folder, plan.content.selection);
        let approve =
            env.store
                .approve_plan(&env.config, again.id, &again.digest, "Operator", WORKERS);
        assert!(
            matches!(approve, Err(Error::Conflict(_))),
            "{at}: {approve:?}"
        );

        let (env, _) = env.restart();
        let done = env.store.kept_deletion(deletion.id).unwrap();
        assert!(
            done.finished_at.is_some() && done.recovery_error.is_none(),
            "{at}: {done:?}"
        );
        let listed = env.store.kept_files(folder.id).unwrap();
        if at == "before the move" {
            assert!(!done.outcomes[0].deleted, "{at}: {:?}", done.outcomes);
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{at}");
            assert_eq!(listed, kept, "{at}");
        } else {
            assert!(done.outcomes[0].deleted, "{at}: {:?}", done.outcomes);
            assert!(!path.exists(), "{at}");
            assert_eq!(listed, kept[1..].to_vec(), "{at}");
        }
        assert!(
            names(&dir.join(RETAINED).join(".deleting")).is_empty(),
            "{at}"
        );
        assert!(
            kept[1..]
                .iter()
                .all(|k| dir.join(RETAINED).join("2").join(&k.kept_name).exists())
        );
    }
}

/// Overwrites the file at `path` in place, through a descriptor, with as many other bytes, and puts its mtime back:
/// its identity, size and mtime are as they were, and only its ctime and content show it changed.
fn rewrite_keeping_stamp(path: &Path) -> Vec<u8> {
    use std::io::Write;
    let meta = std::fs::metadata(path).unwrap();
    let mtime = meta.modified().unwrap();
    let bytes: Vec<u8> = std::fs::read(path).unwrap().iter().map(|b| !b).collect();
    let mut file = std::fs::File::options().write(true).open(path).unwrap();
    file.write_all(&bytes).unwrap();
    file.set_modified(mtime).unwrap();
    file.sync_all().unwrap();
    bytes
}

#[test]
fn the_deletion_digest_binds_each_kept_files_ctime_and_sha256_read_when_it_was_planned() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let deletion = plan_delete_all(&env, &folder);
    let content = &deletion.content;
    assert_eq!(content.reviewed.len(), content.files.len());
    for (file, reviewed) in content.files.iter().zip(&content.reviewed) {
        assert_eq!(reviewed.id, file.id);
        assert_eq!(Some(&reviewed.sha256), file.sha256.as_ref());
        assert!(reviewed.ctime_ns > 0);
    }
    let digest = |content: &title_select_api::build::KeptDeletionContent| -> String {
        let bytes = serde_json::to_vec(content).unwrap();
        <sha2::Sha256 as sha2::Digest>::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    assert_eq!(digest(content), deletion.digest);
    let mut moved = content.clone();
    moved.reviewed[0].ctime_ns += 1;
    assert_ne!(digest(&moved), deletion.digest);
    let mut rehashed = content.clone();
    rehashed.reviewed[0].sha256 = "0".repeat(64);
    assert_ne!(digest(&rehashed), deletion.digest);
}

#[test]
fn a_kept_file_rewritten_after_you_review_its_deletion_is_never_deleted() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let path = dir.join(RETAINED).join("2").join(&kept[0].kept_name);
    let deletion = plan_delete_all(&env, &folder);
    // Rewritten through a descriptor already open, the same length, with its mtime put back.
    let rewritten = rewrite_keeping_stamp(&path);

    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();

    let detail = done.outcomes[0].detail.as_deref().unwrap();
    assert!(!done.outcomes[0].deleted);
    assert!(detail.contains("changed since you reviewed"), "{detail}");
    assert!(
        done.outcomes[1..].iter().all(|o| o.deleted),
        "{:?}",
        done.outcomes
    );
    assert!(done.finished_at.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), rewritten);
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept[..1].to_vec());
    assert!(names(&dir.join(RETAINED).join(".deleting")).is_empty());
}

#[test]
fn a_kept_file_rewritten_inside_deleting_is_moved_back_and_never_deleted() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let path = dir.join(RETAINED).join("2").join(&kept[0].kept_name);
    let slot = dir
        .join(RETAINED)
        .join(".deleting")
        .join(kept[0].id.to_string());
    let deletion = plan_delete_all(&env, &folder);
    let target = kept[0].id;
    let at = slot.clone();
    let rewritten = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = rewritten.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::AfterDeleteMove(target) {
                *seen.lock().unwrap() = rewrite_keeping_stamp(&at);
            }
        })));

    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    env.store.hook_faults(None);

    let detail = done.outcomes[0].detail.as_deref().unwrap();
    assert!(!done.outcomes[0].deleted);
    assert!(
        detail.contains("changed since you reviewed") && detail.contains("moved back"),
        "{detail}"
    );
    assert!(done.outcomes[1..].iter().all(|o| o.deleted));
    assert_eq!(std::fs::read(&path).unwrap(), *rewritten.lock().unwrap());
    assert!(!slot.exists());
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept[..1].to_vec());
}

#[test]
fn a_stopped_deletion_resumes_only_by_the_ctime_held_across_the_rename_and_the_reviewed_sha256() {
    for at in ["before the ctime is recorded", "rewritten after the move"] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        assert_eq!(env.build(&plan).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        let first = kept[0].clone();
        let path = dir.join(RETAINED).join("2").join(&first.kept_name);
        let slot = dir
            .join(RETAINED)
            .join(".deleting")
            .join(first.id.to_string());
        let bytes = std::fs::read(&path).unwrap();
        let deletion = env
            .store
            .plan_kept_deletion(
                &env.config,
                folder.id,
                &PlanKeptDeletion {
                    ids: vec![first.id],
                    all: false,
                },
            )
            .unwrap();
        let fault = match at {
            "before the ctime is recorded" => Fault::DeleteMoveUnrecorded(first.id),
            _ => Fault::AfterDeleteMove(first.id),
        };
        crash_deletion(&env, &deletion, fault);
        assert!(slot.exists(), "{at}");
        let expected = match at {
            // Unchanged, but nothing read through a held descriptor shows it: a ctime read now by name never does.
            "before the ctime is recorded" => bytes,
            _ => rewrite_keeping_stamp(&slot),
        };

        let (env, _) = env.restart();
        let done = env.store.kept_deletion(deletion.id).unwrap();
        assert!(
            done.finished_at.is_some() && done.recovery_error.is_none(),
            "{at}: {done:?}"
        );
        let detail = done.outcomes[0].detail.as_deref().unwrap();
        assert!(!done.outcomes[0].deleted, "{at}: {detail}");
        assert!(
            detail.contains("never deleted") && detail.contains("moved back"),
            "{at}: {detail}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), expected, "{at}");
        assert!(!slot.exists(), "{at}");
        assert_eq!(env.store.kept_files(folder.id).unwrap(), kept, "{at}");
    }
}

#[test]
fn recovery_deletes_only_what_it_verified_never_what_merely_looks_unchanged() {
    for at in ["before the ctime is recorded", "after the move"] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        assert_eq!(env.build(&plan).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        let first = kept[0].clone();
        let path = dir.join(RETAINED).join("2").join(&first.kept_name);
        let slot = dir
            .join(RETAINED)
            .join(".deleting")
            .join(first.id.to_string());
        let bytes = std::fs::read(&path).unwrap();
        let deletion = env
            .store
            .plan_kept_deletion(
                &env.config,
                folder.id,
                &PlanKeptDeletion {
                    ids: vec![first.id],
                    all: false,
                },
            )
            .unwrap();
        let fault = match at {
            "before the ctime is recorded" => Fault::DeleteMoveUnrecorded(first.id),
            _ => Fault::AfterDeleteMove(first.id),
        };
        crash_deletion(&env, &deletion, fault);

        // While recovery looks, a writer briefly changes its length, then restores its length and mtime.
        let (at_slot, id) = (slot.clone(), first.id);
        let mtime = std::fs::metadata(&slot).unwrap().modified().unwrap();
        let restore = bytes.clone();
        let (env, _) = env.restart_hooked(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::KeptDeletionSynced(id) {
                let file = std::fs::File::options().write(true).open(&at_slot).unwrap();
                file.set_len(restore.len() as u64 + 1).unwrap();
            } else if fault == Fault::DeletingFound(id) {
                use std::io::Write;
                let mut file = std::fs::File::options().write(true).open(&at_slot).unwrap();
                file.set_len(restore.len() as u64).unwrap();
                file.write_all(&restore).unwrap();
                file.set_modified(mtime).unwrap();
            }
        })));
        let done = env.store.kept_deletion(deletion.id).unwrap();
        assert!(
            done.finished_at.is_some() && done.recovery_error.is_none(),
            "{at}: {done:?}"
        );
        let detail = done.outcomes[0].detail.as_deref().unwrap();
        assert!(!done.outcomes[0].deleted, "{at}: {detail}");
        assert!(detail.contains("moved back"), "{at}: {detail}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{at}");
        assert_eq!(env.store.kept_files(folder.id).unwrap(), kept, "{at}");
    }
}

#[test]
fn what_is_swapped_into_deleting_after_it_is_recorded_held_aside_is_moved_back_and_recovery_finishes()
 {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let first = kept[0].clone();
    let path = dir.join(RETAINED).join("2").join(&first.kept_name);
    let slot = dir
        .join(RETAINED)
        .join(".deleting")
        .join(first.id.to_string());
    let deletion = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![first.id],
                all: false,
            },
        )
        .unwrap();
    // Another file takes the kept file's place just before it is moved; the service stops once what was moved aside
    // is recorded held aside, and then that is swapped for yet another.
    let (at, away) = (path.clone(), env.tmp.path().join("original"));
    let id = first.id;
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforeDeleteMove(id) {
                std::fs::rename(&at, &away).unwrap();
                std::fs::write(&at, b"someone else's").unwrap();
            } else if fault == Fault::Quarantined(id) {
                panic!("killed");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err());
    std::fs::remove_file(&slot).unwrap();
    std::fs::write(&slot, b"a swap").unwrap();

    let (env, _) = env.restart();
    let done = env.store.kept_deletion(deletion.id).unwrap();
    assert!(
        done.finished_at.is_some() && done.recovery_error.is_none(),
        "{done:?}"
    );
    let detail = done.outcomes[0].detail.as_deref().unwrap();
    assert!(!done.outcomes[0].deleted, "{detail}");
    assert!(detail.contains("moved back"), "{detail}");
    assert!(!slot.exists());
    assert_eq!(std::fs::read(&path).unwrap(), b"a swap");
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);
}

#[test]
fn deletion_recovery_waits_until_the_folder_is_shown_to_be_the_stores() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let deletion = plan_delete_all(&env, &folder);
    let deletion_again = plan_delete_all(&env, &folder);
    crash_deletion(&env, &deletion, Fault::BeforeDeleteMove(kept[0].id));

    // The library is unmounted: its mount point is there, empty.
    let mounted = env.tmp.path().join("unmounted");
    std::fs::rename(&dir, &mounted).unwrap();
    std::fs::create_dir(&dir).unwrap();
    let (env, _) = env.restart();
    let pending = env.store.kept_deletion(deletion.id).unwrap();
    assert!(pending.finished_at.is_none());
    assert!(
        pending
            .recovery_error
            .as_deref()
            .unwrap()
            .contains("marker"),
        "{pending:?}"
    );
    let blocked = env.store.recover_kept_deletions(&env.config).unwrap();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    let again = env.plan(&folder, plan.content.selection);
    assert!(!again.approvable());

    // Mounted again, but the root is read-only: still nothing is settled.
    std::fs::remove_dir(&dir).unwrap();
    std::fs::rename(&mounted, &dir).unwrap();
    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), false);
    let (env, _) = env.restart();
    let pending = env.store.kept_deletion(deletion.id).unwrap();
    assert!(
        pending
            .recovery_error
            .as_deref()
            .unwrap()
            .contains("not writable"),
        "{pending:?}"
    );
    let approve = env.store.approve_kept_deletion(
        &env.config,
        deletion_again.id,
        &deletion_again.digest,
        "Operator",
    );
    assert!(
        matches!(&approve, Err(Error::Conflict(m)) if m.contains("not finished")),
        "{approve:?}"
    );

    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), true);
    let (env, _) = env.restart();
    let done = env.store.kept_deletion(deletion.id).unwrap();
    assert!(
        done.finished_at.is_some() && done.recovery_error.is_none(),
        "{done:?}"
    );
    assert!(!done.outcomes[0].deleted, "{:?}", done.outcomes);
    // The file that was about to be deleted is still there and still kept; the rest were never started.
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);
    assert!(
        dir.join(RETAINED)
            .join("2")
            .join(&kept[0].kept_name)
            .exists()
    );
}

#[test]
fn a_kept_file_that_vanished_is_no_longer_listed_once_its_deletion_is_carried_out() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let deletion = plan_delete_all(&env, &folder);
    let later = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![kept[0].id],
                all: false,
            },
        )
        .unwrap();
    std::fs::remove_file(dir.join(RETAINED).join("2").join(&kept[0].kept_name)).unwrap();

    // Without the folder's marker, nothing can be shown gone, nor planned.
    std::fs::rename(dir.join(".title-select"), env.tmp.path().join("marker")).unwrap();
    let refused = env.store.plan_kept_deletion(
        &env.config,
        folder.id,
        &PlanKeptDeletion {
            ids: vec![],
            all: true,
        },
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(done.outcomes.iter().all(|o| !o.deleted));
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);

    std::fs::rename(env.tmp.path().join("marker"), dir.join(".title-select")).unwrap();
    let done = env
        .store
        .approve_kept_deletion(&env.config, later.id, &later.digest, "Operator")
        .unwrap();
    assert!(!done.outcomes[0].deleted);
    assert!(
        done.outcomes[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("it is gone"),
        "{:?}",
        done.outcomes
    );
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept[1..].to_vec());
}

/// The temporary names, in `dir`, of a folder being created there.
fn creating(dir: &Path) -> Vec<String> {
    names(dir)
        .into_iter()
        .filter(|n| n.starts_with(&format!("{RETAINED}.new-")))
        .collect()
}

#[test]
fn a_retention_area_swapped_while_it_is_created_is_never_used() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    let before = names(&dir);
    let at = dir.clone();
    let elsewhere = env.tmp.path().join("elsewhere");
    let away = elsewhere.clone();
    // Once the area is made under its temporary name, someone else's folder takes its place.
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforePublishDir && !away.exists() {
                let temp = at.join(&creating(&at)[0]);
                std::fs::rename(&temp, &away).unwrap();
                std::fs::create_dir(&temp).unwrap();
                std::fs::write(temp.join("theirs"), b"theirs").unwrap();
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);

    assert_eq!(job.state, JobState::Failed);
    assert!(elsewhere.is_dir());
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("theirs")).unwrap(),
        b"theirs",
        "the folder put in its place is left as it is"
    );
    let mut expected = before.clone();
    expected.insert(RETAINED.to_string());
    assert_eq!(names(&dir), expected, "nothing was kept aside");
    assert!(env.store.kept_files(folder.id).unwrap().is_empty());
    let (env, _) = env.restart();
    let next = env.plan(&folder, plan.content.selection);
    assert!(
        next.content
            .blockers
            .iter()
            .any(|b| b.message.contains(RETAINED)),
        "{:?}",
        next.content.blockers
    );
    assert_eq!(names(&dir), expected);
}

#[test]
fn a_folder_recreated_in_place_of_a_gone_one_is_adopted_by_recovery_when_it_stopped_once_published()
{
    for gone in ["generation", ".deleting"] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        assert_eq!(env.build(&plan).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        assert!(kept.len() >= 2, "{kept:?}");
        let generation = dir.join(RETAINED).join("2");
        let deleting = dir.join(RETAINED).join(".deleting");
        let plan_one = |env: &Env, id: i64| {
            env.store
                .plan_kept_deletion(
                    &env.config,
                    folder.id,
                    &PlanKeptDeletion {
                        ids: vec![id],
                        all: false,
                    },
                )
                .unwrap()
        };
        let (env, target) = if gone == "generation" {
            // Something else takes the kept file's place just before it is moved, and the service stops once that
            // is recorded held aside; then the generation's folder it is to be moved back to is gone. Recovery makes
            // it again, and stops once it is renamed into place.
            let first = kept[0].clone();
            let deletion = plan_one(&env, first.id);
            let (at, id) = (generation.join(&first.kept_name), first.id);
            let away = env.tmp.path().join("original");
            env.store
                .hook_faults(Some(std::sync::Arc::new(move |fault| {
                    if fault == Fault::BeforeDeleteMove(id) {
                        std::fs::rename(&at, &away).unwrap();
                        std::fs::write(&at, b"someone else's").unwrap();
                    } else if fault == Fault::Quarantined(id) {
                        panic!("killed");
                    }
                })));
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.store.approve_kept_deletion(
                    &env.config,
                    deletion.id,
                    &deletion.digest,
                    "Operator",
                )
            }));
            env.store.hook_faults(None);
            assert!(ran.is_err());
            std::fs::rename(&generation, env.tmp.path().join("generation")).unwrap();
            let (env, stopped) = env.restart_until(Some(Fault::PublishedDir));
            assert!(
                stopped.is_none(),
                "it stopped once the folder was published"
            );
            assert!(generation.is_dir());
            (env, deletion.id)
        } else {
            // A deletion makes the private folder kept files are deleted from; it is then gone, and the next
            // deletion makes it again and stops once it is renamed into place.
            let first = plan_one(&env, kept[0].id);
            let done = env
                .store
                .approve_kept_deletion(&env.config, first.id, &first.digest, "Operator")
                .unwrap();
            assert!(done.outcomes[0].deleted, "{done:?}");
            std::fs::remove_dir(&deleting).unwrap();
            let second = plan_one(&env, kept[1].id);
            crash_deletion(&env, &second, Fault::PublishedDir);
            assert!(deleting.is_dir());
            (env, second.id)
        };

        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{gone}: {blocked:?}");
        let done = env.store.kept_deletion(target).unwrap();
        assert!(
            done.finished_at.is_some() && done.recovery_error.is_none(),
            "{gone}: {done:?}"
        );
        assert!(!done.outcomes[0].deleted, "{gone}: {done:?}");
        // The folder made again is the store's: what is kept in it can be deleted.
        let remaining = env.store.kept_files(folder.id).unwrap();
        let file = if gone == "generation" {
            remaining
                .iter()
                .find(|k| k.id == kept[0].id)
                .unwrap()
                .clone()
        } else {
            kept[1].clone()
        };
        if gone == "generation" {
            assert_eq!(
                std::fs::read(generation.join(&file.kept_name)).unwrap(),
                b"someone else's",
                "what was held aside is moved back into the folder made again"
            );
        } else {
            let again = plan_one(&env, file.id);
            let done = env
                .store
                .approve_kept_deletion(&env.config, again.id, &again.digest, "Operator")
                .unwrap();
            assert!(done.outcomes[0].deleted, "{gone}: {done:?}");
        }
        let next = env.plan(&folder, plan.content.selection);
        assert!(
            !blockers(&next).contains(&BlockerKind::Recovery),
            "{gone}: {:?}",
            next.content.blockers
        );
    }
}

#[test]
fn a_retention_area_left_mid_creation_is_recorded_or_adopted_by_recovery_and_an_unknown_one_blocks()
{
    for adopt in [false, true] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        env.crash(&plan, Fault::BeforePublishDir);
        let temps = creating(&dir);
        assert_eq!(temps.len(), 1, "{temps:?}");
        if adopt {
            // As if the stop came just after the rename.
            std::fs::rename(dir.join(&temps[0]), dir.join(RETAINED)).unwrap();
        }
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{adopt}: {blocked:?}");
        // Never removed by name: one never renamed into place stays, recorded as this store's.
        assert_eq!(
            creating(&dir),
            if adopt { vec![] } else { temps.clone() },
            "{adopt}"
        );
        assert_eq!(dir.join(RETAINED).is_dir(), adopt);

        let next = env.plan(&folder, plan.content.selection);
        assert!(next.approvable(), "{adopt}: {:?}", next.content.blockers);
        assert_eq!(env.build(&next).state, JobState::Succeeded);
        assert_eq!(
            env.store.kept_files(folder.id).unwrap().len(),
            plan.content.removals.len()
        );

        // One no build of this store recorded making is never removed, and blocks.
        let unknown = dir.join(format!("{RETAINED}.new-1"));
        std::fs::create_dir(&unknown).unwrap();
        let (env, _) = env.restart();
        assert!(unknown.is_dir());
        let next = env.plan(&folder, plan.content.selection);
        assert!(blockers(&next).contains(&BlockerKind::Recovery), "{adopt}");
        assert!(
            check(
                &env,
                &env.store.library_folder(&env.config, folder.id).unwrap()
            )
            .drift
            .is_empty()
        );
    }
}

#[test]
fn a_move_back_is_recorded_only_once_it_is_found_back_and_made_durable() {
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    let first = kept[0].clone();
    let path = dir.join(RETAINED).join("2").join(&first.kept_name);
    let slot = dir
        .join(RETAINED)
        .join(".deleting")
        .join(first.id.to_string());
    let aside = env.tmp.path().join("aside");
    let deletion = env
        .store
        .plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![first.id],
                all: false,
            },
        )
        .unwrap();
    // Another file takes the kept file's place just before it is moved; the service stops once that is moved back,
    // before either folder is synced.
    let (at, away) = (path.clone(), env.tmp.path().join("original"));
    let id = first.id;
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforeDeleteMove(id) {
                std::fs::rename(&at, &away).unwrap();
                std::fs::write(&at, b"someone else's").unwrap();
            } else if fault == Fault::MovedBack(id) {
                panic!("killed");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err());
    assert!(!slot.exists());
    assert_eq!(std::fs::read(&path).unwrap(), b"someone else's");
    let held = |env: &Env| {
        let pending = env.store.kept_deletion(deletion.id).unwrap();
        assert!(pending.finished_at.is_none(), "{pending:?}");
        assert!(
            !env.store
                .kept_files(folder.id)
                .unwrap()
                .iter()
                .any(|k| k.id == id)
        );
        pending.recovery_error.unwrap_or_default()
    };

    // Found back under its name, it is still held aside until both folders are synced.
    let (env, stopped) = env.restart_until(Some(Fault::MoveBackSynced(id)));
    assert!(
        stopped.is_none(),
        "recovery syncs before it records it kept"
    );
    held(&env);

    // Once both folders are synced, nothing held aside, and something else at its name, it settles: the file is
    // recorded kept, and nothing is deleted.
    std::fs::rename(&path, &aside).unwrap();
    std::fs::write(&path, b"a third").unwrap();
    let (env, _) = env.restart();
    let done = env.store.kept_deletion(deletion.id).unwrap();
    assert!(
        done.finished_at.is_some() && done.recovery_error.is_none(),
        "{done:?}"
    );
    assert!(!done.outcomes[0].deleted);
    let detail = done.outcomes[0].detail.as_deref().unwrap();
    assert!(detail.contains("holds something else"), "{detail}");
    assert_eq!(std::fs::read(&path).unwrap(), b"a third");
    assert_eq!(std::fs::read(&aside).unwrap(), b"someone else's");
    assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);
}

#[test]
fn something_moved_aside_that_cannot_be_moved_back_stays_tracked_until_it_is() {
    for crash in [false, true] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        assert_eq!(env.build(&plan).state, JobState::Succeeded);
        let kept = env.store.kept_files(folder.id).unwrap();
        let first = kept[0].clone();
        let path = dir.join(RETAINED).join("2").join(&first.kept_name);
        let slot = dir
            .join(RETAINED)
            .join(".deleting")
            .join(first.id.to_string());
        let original = env.tmp.path().join("original");
        let deletion = env
            .store
            .plan_kept_deletion(
                &env.config,
                folder.id,
                &PlanKeptDeletion {
                    ids: vec![first.id],
                    all: false,
                },
            )
            .unwrap();
        // Just before the kept file is moved, another file takes its place; once that is moved aside, a third takes
        // its name, so what was moved aside cannot be moved back.
        let (at, away) = (path.clone(), original.clone());
        let id = first.id;
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::BeforeDeleteMove(id) {
                    std::fs::rename(&at, &away).unwrap();
                    std::fs::write(&at, b"someone else's").unwrap();
                } else if fault == Fault::AfterDeleteMove(id) {
                    std::fs::write(&at, b"a third").unwrap();
                } else if crash && fault == Fault::Quarantined(id) {
                    panic!("killed");
                }
            })));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.store
                .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        }));
        env.store.hook_faults(None);
        if crash {
            assert!(ran.is_err());
        } else {
            let done = ran.unwrap().unwrap();
            assert!(done.finished_at.is_none(), "{done:?}");
            let error = done.recovery_error.unwrap();
            assert!(error.contains("could not be moved back"), "{error}");
        }

        let (env, _) = env.restart();
        let pending = env.store.kept_deletion(deletion.id).unwrap();
        assert!(pending.finished_at.is_none(), "{crash}: {pending:?}");
        let error = pending.recovery_error.unwrap();
        assert!(
            error.contains(&format!(".deleting/{}", first.id))
                && error.contains("could not be moved back"),
            "{crash}: {error}"
        );
        // Nothing is lost: each file is where it was left, and the one held aside is still tracked.
        assert_eq!(std::fs::read(&slot).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(&path).unwrap(), b"a third");
        assert!(original.exists());
        assert!(
            !env.store
                .kept_files(folder.id)
                .unwrap()
                .iter()
                .any(|k| k.id == first.id)
        );
        let others = plan_delete_all(&env, &folder);
        let refused =
            env.store
                .approve_kept_deletion(&env.config, others.id, &others.digest, "Operator");
        assert!(
            matches!(&refused, Err(Error::Conflict(m)) if m.contains("not finished")),
            "{refused:?}"
        );

        // Once its name is free, recovery moves it back, and the deletion finishes.
        std::fs::remove_file(&path).unwrap();
        let (env, _) = env.restart();
        let done = env.store.kept_deletion(deletion.id).unwrap();
        assert!(
            done.finished_at.is_some() && done.recovery_error.is_none(),
            "{done:?}"
        );
        assert!(!done.outcomes[0].deleted);
        assert!(
            done.outcomes[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("moved back")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"someone else's");
        assert!(!slot.exists());
        assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);

        // It is still not the file that was kept: a later deletion is refused when it is planned.
        let again = env.store.plan_kept_deletion(
            &env.config,
            folder.id,
            &PlanKeptDeletion {
                ids: vec![first.id],
                all: false,
            },
        );
        assert!(
            matches!(&again, Err(Error::Refused(m)) if m.contains("no longer the file that was kept")),
            "{again:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"someone else's");
        assert_eq!(env.store.kept_files(folder.id).unwrap(), kept);
    }
}

#[test]
fn a_folder_that_is_not_the_empty_private_one_just_made_is_never_used() {
    use std::os::unix::fs::PermissionsExt;
    for tamper in ["a file in it", "a group-writable mode"] {
        let env = Env::new();
        let (folder, plan) = built_then_deselected(&env);
        let dir = env.play().join("atari2600");
        let at = dir.clone();
        // Once the area is made under its temporary name, before it is checked, it is changed.
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::MadeDir {
                    let temp = at.join(&creating(&at)[0]);
                    if tamper == "a file in it" {
                        std::fs::write(temp.join("theirs"), b"theirs").unwrap();
                    } else {
                        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o770))
                            .unwrap();
                    }
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);

        assert_eq!(job.state, JobState::Failed, "{tamper}");
        let temps = creating(&dir);
        assert_eq!(temps.len(), 1, "{tamper}: left in place");
        if tamper == "a file in it" {
            assert_eq!(
                std::fs::read(dir.join(&temps[0]).join("theirs")).unwrap(),
                b"theirs"
            );
        }
        assert!(!dir.join(RETAINED).exists(), "{tamper}");
        assert!(env.store.kept_files(folder.id).unwrap().is_empty());
        let (env, _) = env.restart();
        let next = env.plan(&folder, plan.content.selection);
        assert!(blockers(&next).contains(&BlockerKind::Recovery), "{tamper}");
    }

    // A recorded retention area whose mode was opened up is no longer trusted.
    let env = Env::new();
    let (folder, plan) = built_then_deselected(&env);
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let area = dir.join(RETAINED);
    let deletion = plan_delete_all(&env, &folder);
    std::fs::set_permissions(&area, std::fs::Permissions::from_mode(0o777)).unwrap();
    let done = env
        .store
        .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(
        done.outcomes
            .iter()
            .all(|o| !o.deleted && o.detail.as_deref().unwrap().contains("private"))
    );
    assert_eq!(
        env.store.kept_files(folder.id).unwrap().len(),
        done.outcomes.len()
    );
}

#[test]
fn registration_needs_a_writable_root_and_an_empty_folder_and_a_plan_checks_the_marker() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));

    let mut read_only = add("x");
    read_only.root = "sets".into();
    let refused = env.store.add_library_folder(&env.config, &read_only);
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    std::fs::create_dir_all(env.play().join("full")).unwrap();
    std::fs::write(env.play().join("full/game.zip"), b"x").unwrap();
    let full = env.store.add_library_folder(&env.config, &add("full"));
    assert!(matches!(full, Err(Error::Refused(_))), "{full:?}");

    // Two Library Folders for one system.
    let first = env.folder("atari2600");
    let second = env.folder("atari2600-b");
    assert_ne!(first.id, second.id);
    let again = env.store.add_library_folder(&env.config, &add("atari2600"));
    assert!(again.is_err());

    // Another folder's marker, as a wrong mount would show.
    let dir = env.play().join("atari2600");
    std::fs::copy(
        env.play().join("atari2600-b/.title-select"),
        dir.join(".title-select"),
    )
    .unwrap();
    let plan = env.plan(&first, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Marker]));

    // Another store's marker.
    let mut foreign = marker(&env.play().join("atari2600-b"));
    foreign["store"] = "elsewhere".into();
    foreign["folder"] = first.id.into();
    std::fs::write(dir.join(".title-select"), foreign.to_string()).unwrap();
    assert_eq!(
        blockers(&env.plan(&first, id)),
        BTreeSet::from([BlockerKind::Marker])
    );

    // A generation newer than the store's, as a restored store would show.
    let mut ahead = marker(&env.play().join("atari2600-b"));
    ahead["folder"] = first.id.into();
    ahead["generation"] = 3.into();
    std::fs::write(dir.join(".title-select"), ahead.to_string()).unwrap();
    let plan = env.plan(&first, id);
    assert!(plan.content.blockers[0].message.contains("newer"));

    // A missing marker on a folder with files.
    std::fs::remove_file(dir.join(".title-select")).unwrap();
    std::fs::write(dir.join("readme.txt"), b"x").unwrap();
    assert_eq!(
        blockers(&env.plan(&first, id)),
        BTreeSet::from([BlockerKind::Marker])
    );

    // The second folder builds; a Root made read-only blocks its next plan.
    assert_eq!(env.build(&env.plan(&second, id)).state, JobState::Succeeded);
    let read_only = config(&env.tmp.path().join("roots/sets"), &env.play(), false);
    let plan = env.store.plan_build(&read_only, second.id, id).unwrap();
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::ReadOnlyRoot]));
}

#[test]
fn a_quick_inventorys_claims_are_never_build_sources() {
    let env = Env::new();
    // Ōkami's ZIP: its directory claims the right CRC32, but its payload is corrupt.
    let okami = env.set_dir().join("Ōkami (Japan).zip");
    write_stored_zip(&okami, &[("Ōkami (Japan).a26", b"cafe")]);
    let mut bytes = std::fs::read(&okami).unwrap();
    let at = bytes.windows(4).position(|w| w == b"cafe").unwrap();
    bytes[at..at + 4].copy_from_slice(b"CAFE");
    std::fs::write(&okami, &bytes).unwrap();
    env.store
        .audit_source_set(
            &env.config,
            SET,
            None,
            title_select_engine::AuditEvidence::Quick,
        )
        .unwrap();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");

    let plan = env.plan(&folder, id);

    let item = |game: &str| plan.content.items.iter().find(|i| i.game == game).unwrap();
    let sources = |game: &str| {
        item(game)
            .sources
            .iter()
            .map(|s| {
                (
                    s.path.clone(),
                    s.finding.expect("a source set entry"),
                    s.evidence,
                )
            })
            .collect::<Vec<_>>()
    };
    use title_select_api::{EvidenceLevel::Payload, Finding::*};
    assert_eq!(
        sources("Ōkami (Japan)"),
        [("a-cafe.bin".into(), Misnamed, Some(Payload))]
    );
    assert_eq!(
        sources("Pong (USA)"),
        [("Pong (USA).a26".into(), Matched, Some(Payload))]
    );
    let combat = item("Combat (USA)");
    assert_eq!(combat.kind, ItemKind::NoSource);
    let reason = combat.reason.as_deref().unwrap();
    assert!(reason.contains("claim evidence"), "{reason}");

    let job = env.build(&plan);

    assert_eq!(job.state, JobState::Succeeded);
    let built = env.play().join("atari2600/Ōkami (Japan).zip");
    assert_eq!(
        validate(&built)
            .into_iter()
            .map(|m| m.name)
            .collect::<Vec<_>>(),
        ["Ōkami (Japan).a26"]
    );
}

#[test]
fn a_source_that_fails_its_rehash_fails_only_its_item() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    // The same size and mtime, so the plan still holds, but another payload.
    let source = env.set_dir().join("Combat (USA).zip");
    let mtime = std::fs::metadata(&source).unwrap().modified().unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let at = bytes.windows(6).position(|w| w == b"combat").unwrap();
    let mut altered = bytes.clone();
    altered[at..at + 6].copy_from_slice(b"COMBAT");
    std::fs::write(&source, &altered).unwrap();
    let file = std::fs::File::options().write(true).open(&source).unwrap();
    file.set_modified(mtime).unwrap();
    drop(file);

    let job = env.build(&plan);

    assert_eq!(job.state, JobState::Succeeded);
    let dir = env.play().join("atari2600");
    assert!(!names(&dir).contains("Combat (USA).zip"));
    assert!(names(&dir).contains("Pong (USA).zip"));
    assert!(!dir.join(".title-select-staging").exists());
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    let combat = detail
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert_eq!(combat.state, ItemState::Failed);
    assert!(combat.error.is_some());
    assert_eq!((detail.build.applied, detail.build.failed), (4, 1));
}

#[test]
fn an_interrupted_build_leaves_state_that_recovery_cleans_up() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();
    let dir = env.play().join("atari2600");

    let stop = |p: title_select_engine::Progress| {
        if p.files_done == 2 {
            panic!("the service stops");
        }
    };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, Some(&stop))
    }));
    assert!(ran.is_err());
    assert!(dir.join(".title-select-staging").is_dir());

    let Env {
        tmp, store, config, ..
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let store = Store::open(&path).unwrap();
    let interrupted = store.recover_jobs(WORKERS).unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].job.state, JobState::Interrupted);
    assert!(
        interrupted[0].requeued.is_none(),
        "a build is never queued again"
    );
    store.recover_builds(&config).unwrap();

    assert!(!dir.join(".title-select-staging").exists());
    assert_eq!(marker(&dir)["generation"], 0);
    let build = store.build_of_job(job.id).unwrap().unwrap();
    let detail = store.build(build.id).unwrap();
    assert_eq!(detail.build.state, BuildState::Interrupted);
    let states: Vec<ItemState> = detail.items.iter().map(|i| i.state).collect();
    assert_eq!(
        states,
        [
            ItemState::Applied,
            ItemState::Applied,
            ItemState::Pending,
            ItemState::Pending,
            ItemState::Pending
        ]
    );
    // A new plan starts from the same generation and keeps the applied files.
    let next = store.plan_build(&config, folder.id, id).unwrap();
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 0);
    assert_eq!(next.summary.unchanged, 2);
    drop(tmp);
}

#[test]
fn the_preview_attributes_each_change_to_settings_choices_or_the_dat_revision() {
    let env = Env::new();
    env.audit();
    let first = selection(
        &env.sha256,
        vec![
            group("Pong (USA)"),
            group("Combat (USA)"),
            group("Breakout (USA)"),
        ],
    );
    let id = env.record(&first);
    let folder = env.folder("atari2600");
    env.build(&env.plan(&folder, id));

    let profile = SelectionProfile {
        region_order: vec!["USA".into()],
        ..SelectionProfile::default()
    };
    env.store
        .save_profile(PROFILE, &profile, Some(1), "Operator", &[], WORKERS)
        .unwrap();
    let mut combat = group("Combat (USA)");
    let lost = combat.winners.pop().unwrap();
    combat.alternatives.push(Alternative {
        title: lost.title,
        clone_of: "Pong (USA)".into(),
        rule: Rule::choice(),
    });
    let mut breakout = group("Breakout (USA)");
    let dropped = breakout.winners.pop().unwrap();
    breakout.exclusions.push(Exclusion {
        title: dropped.title,
        rule: rule("Region"),
        reason: "Region".into(),
    });
    let second = selection(
        &env.sha256,
        vec![group("Pong (USA)"), combat, breakout, group("Yars (USA)")],
    );
    let id = env.record(&second);

    let plan = env.plan(&folder, id);

    let causes: BTreeMap<&str, Cause> = plan
        .changes
        .iter()
        .map(|c| (c.title.as_str(), c.cause))
        .collect();
    assert_eq!(
        causes,
        BTreeMap::from([
            ("Combat (USA)", Cause::Choices),
            ("Breakout (USA)", Cause::Settings),
            ("Yars (USA)", Cause::DatRevision),
        ])
    );
}

#[test]
fn the_current_build_keeps_its_selection_and_audit_from_pruning_until_a_later_build() {
    let mut env = Env::new();
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n\n[audits]\nkeep = 1\n\n\
         [selections]\nkeep = 1\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.play().display().to_string(),
    ))
    .unwrap();
    env.audit();
    let first = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, first);
    let audit = plan.content.audit.unwrap();
    env.build(&plan);

    env.audit();
    let second = env.record(&everything(&env.sha256));

    assert!(!env.store.selection_record(first).unwrap().pruned);
    assert!(!env.store.audit(audit).unwrap().pruned);
    // A later build releases them.
    env.audit();
    let third = env.record(&everything(&env.sha256));
    let plan = env.plan(&folder, third);
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    env.build(&plan);
    env.audit();
    env.record(&everything(&env.sha256));
    assert!(env.store.selection_record(first).unwrap().pruned);
    assert!(env.store.selection_record(second).unwrap().pruned);
    assert!(env.store.audit(audit).unwrap().pruned);
}

/// The files of `plan` a Build writes, in the order it writes them.
fn written(plan: &Plan) -> Vec<String> {
    plan.content
        .items
        .iter()
        .filter(|i| i.kind.writes())
        .map(|i| i.target.clone())
        .collect()
}

#[test]
fn a_crash_between_putting_a_file_in_place_and_recording_it_is_settled_from_its_publish_intent() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterPublish(1));
    assert!(dir.join(&targets[1]).is_file());
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    let build = env.store.build_of_job(job).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.build.state, BuildState::Interrupted);
    assert_eq!(detail.items[1].state, ItemState::Applied, "{detail:?}");
    assert_eq!(detail.items[2].state, ItemState::Pending);
    let files = env.store.built_files(folder.id).unwrap();
    assert!(files.iter().any(|f| f.target == targets[1]));
    assert!(!dir.join(".title-select-staging").exists());
    assert_eq!(marker(&dir)["generation"], 0);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    let item = next
        .content
        .items
        .iter()
        .find(|i| i.target == targets[1])
        .unwrap();
    assert_eq!(item.kind, ItemKind::Unchanged, "adopted, not a conflict");
}

#[test]
fn a_file_changed_at_its_target_before_recovery_is_left_untouched_and_reported_uncertain() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterPublish(1));
    std::fs::write(dir.join(&targets[1]), b"not the build's").unwrap();
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    let build = env.store.build_of_job(job).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Uncertain);
    assert!(
        detail.items[1]
            .error
            .as_ref()
            .unwrap()
            .contains("left as it is")
    );
    assert_eq!(
        std::fs::read(dir.join(&targets[1])).unwrap(),
        b"not the build's"
    );
    let files = env.store.built_files(folder.id).unwrap();
    assert!(!files.iter().any(|f| f.target == targets[1]));
    let next = env.plan(&folder, id);
    let item = next
        .content
        .items
        .iter()
        .find(|i| i.target == targets[1])
        .unwrap();
    assert_eq!(item.kind, ItemKind::Conflict);
}

#[test]
fn a_marker_that_did_not_follow_the_committed_generation_is_moved_forward() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterFinalize);
    assert_eq!(marker(&dir)["generation"], 0, "the store decided first");
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1, "and the marker followed");
    assert!(!dir.join(".title-select.new").exists());
    let build = env.store.build_of_job(job).unwrap().unwrap();
    assert_eq!(build.state, BuildState::Interrupted);
    assert_eq!(build.applied, written(&plan).len() as u64);
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.generation, 1);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 1);
    assert_eq!(next.summary.unchanged, written(&plan).len() as u64);
}

#[test]
fn a_stop_after_the_marker_advanced_needs_nothing_put_back() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    env.crash(&plan, Fault::AfterMarker);
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 1);
}

#[test]
fn a_backup_taken_mid_finalize_and_restored_never_moves_the_marker_back() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");
    let backup = env.tmp.path().join("backup");
    let (from, to) = (env.store.dir().to_path_buf(), backup.clone());
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::AfterFinalize {
            copy_dir(&from, &to);
        }
    })));

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(marker(&dir)["generation"], 1);

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    drop(store);
    let store = Store::open(&backup).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    let blocked = store.recover_builds(&config).unwrap();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1, "never moved back");
    let plan = store.plan_build(&config, folder.id, id).unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(plan.content.generation, 1);
    drop((tmp, sha256));
}

#[test]
fn a_store_restored_one_generation_behind_its_folder_blocks_and_recovery_leaves_the_marker() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    // A backup of the store, taken before the folder's first Build.
    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let backup = tmp.path().join("backup");
    copy_dir(&path, &backup);
    let env = Env {
        store: Store::open(&path).unwrap(),
        tmp,
        config,
        sha256,
    };
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    assert_eq!(marker(&dir)["generation"], 1);

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    drop(store);
    let store = Store::open(&backup).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    let blocked = store.recover_builds(&config).unwrap();
    assert!(
        blocked.is_empty(),
        "the restored store has no build to recover"
    );
    assert_eq!(marker(&dir)["generation"], 1, "the marker is left as it is");
    let plan = store.plan_build(&config, folder.id, id).unwrap();
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Marker]));
    assert!(plan.content.blockers[0].message.contains("restored"));
    drop((tmp, sha256));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn recovery_leaves_a_folder_whose_marker_is_not_the_stores_and_leftovers_no_build_accounts_for() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let other = env.folder("atari2600-b");
    let dir = env.play().join("atari2600");
    let plan = env.plan(&folder, id);
    let job = env.crash(&plan, Fault::AfterPublish(1));
    assert!(dir.join(".title-select-staging").is_dir());
    let mut foreign = marker(&dir);
    foreign["store"] = "elsewhere".into();
    std::fs::write(dir.join(".title-select"), foreign.to_string()).unwrap();
    // A staging directory in a folder no unfinished Build of the store accounts for.
    let other_dir = env.play().join("atari2600-b");
    std::fs::create_dir(other_dir.join(".title-select-staging")).unwrap();
    std::fs::write(other_dir.join(".title-select-staging/0.zip"), b"x").unwrap();

    let (env, blocked) = env.restart();

    let build = env.store.build_of_job(job).unwrap().unwrap();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].0, build.id);
    assert!(blocked[0].1.contains("another store"), "{blocked:?}");
    assert!(dir.join(".title-select-staging").is_dir(), "left as it is");
    assert_eq!(marker(&dir)["store"], "elsewhere");
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Publishing, "not settled");
    let plan = env.plan(&folder, id);
    assert!(blockers(&plan).contains(&BlockerKind::Recovery));
    assert!(other_dir.join(".title-select-staging/0.zip").is_file());
    let plan = env.plan(&other, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Recovery]));
    assert!(plan.content.blockers[0].message.contains("remove it"));

    // Once the marker is the store's again, the next start cleans up.
    let mut ours = marker(&dir);
    ours["store"] = marker(&other_dir)["store"].clone();
    std::fs::write(dir.join(".title-select"), ours.to_string()).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!dir.join(".title-select-staging").exists());
    assert!(env.plan(&folder, id).approvable());
}

#[cfg(unix)]
#[test]
fn a_symlink_planted_at_the_markers_temp_file_is_never_followed() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let victim = env.tmp.path().join("victim.txt");
    std::fs::write(&victim, b"precious").unwrap();
    let plan = env.plan(&folder, id);

    let plant = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            std::os::unix::fs::symlink(&victim, dir.join(".title-select.new")).unwrap();
        }
    };
    let job = env.build_with(&plan, &plant);

    assert_eq!(job.state, JobState::Failed);
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert_eq!(marker(&dir)["generation"], 0, "the marker did not advance");
    assert!(
        std::fs::symlink_metadata(dir.join(".title-select.new"))
            .unwrap()
            .is_symlink(),
        "recovery leaves what no build wrote"
    );
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::Recovery));
}

#[test]
fn a_marker_changed_during_a_build_stops_it_before_the_next_file_is_put_in_place() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);

    let swap = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            let mut m = marker(&dir);
            m["generation"] = 5.into();
            std::fs::write(dir.join(".title-select"), m.to_string()).unwrap();
        }
    };
    let job = env.build_with(&plan, &swap);

    assert_eq!(job.state, JobState::Failed);
    assert!(job.error.as_ref().unwrap().contains("marker"), "{job:?}");
    assert!(dir.join(&targets[0]).is_file());
    for target in &targets[1..] {
        assert!(!dir.join(target).exists(), "{target}");
    }
    assert_eq!(marker(&dir)["generation"], 5, "left as it is");
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    assert_eq!(build.state, BuildState::Failed);
    assert_eq!(build.applied, 1);
}

#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_symlink_mid_build_never_redirects_a_write() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let moved = env.play().join("moved");
    let elsewhere = env.tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join(".title-select"), b"someone else's").unwrap();
    let plan = env.plan(&folder, id);
    let targets = written(&plan);

    let swap = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            std::fs::rename(&dir, &moved).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        }
    };
    let job = env.build_with(&plan, &swap);

    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error);
    assert_eq!(
        names(&elsewhere),
        BTreeSet::from([".title-select".to_owned()])
    );
    assert_eq!(
        std::fs::read(elsewhere.join(".title-select")).unwrap(),
        b"someone else's"
    );
    for target in &targets {
        assert!(moved.join(target).is_file(), "{target}");
    }
    assert_eq!(marker(&moved)["generation"], 1);
    // The path is now a symlink, which a library folder's path never is.
    let next = env.plan(&folder, id);
    assert_eq!(
        blockers(&next),
        BTreeSet::from([BlockerKind::FolderMissing])
    );
    assert!(
        env.store
            .add_library_folder(&env.config, &add("atari2600/sub"))
            .is_err()
    );
}

#[test]
fn rom_names_with_path_parts_and_device_names_are_blocked_items() {
    let env = Env::new();
    env.audit();
    let id = env.record(&selection(
        &env.sha256,
        UNSAFE
            .iter()
            .map(|g| group(g))
            .chain([group("Pong (USA)")])
            .collect(),
    ));
    let folder = env.folder("atari2600");

    let plan = env.plan(&folder, id);

    for game in UNSAFE {
        let item = plan.content.items.iter().find(|i| i.game == game).unwrap();
        assert_eq!(item.kind, ItemKind::Blocked, "{game}");
        assert!(item.reason.is_some(), "{game}");
    }
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        names(&env.play().join("atari2600")),
        BTreeSet::from([".title-select".to_owned(), "Pong (USA).zip".to_owned()])
    );
}

#[test]
fn a_staged_file_replaced_after_it_was_verified_is_never_put_in_place() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");
    let staging = dir.join(".title-select-staging");
    let mode = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen, at) = (mode.clone(), staging.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforePublish(1) {
                use std::os::unix::fs::PermissionsExt;
                *seen.lock().unwrap() =
                    Some(std::fs::metadata(&at).unwrap().permissions().mode() & 0o777);
                std::fs::remove_file(at.join("1.zip")).unwrap();
                std::fs::write(at.join("1.zip"), b"swapped in").unwrap();
            }
        })));

    let job = env.build(&plan);

    assert_eq!(*mode.lock().unwrap(), Some(0o700), "staging is private");
    assert_eq!(job.state, JobState::Failed);
    assert!(
        job.error
            .unwrap()
            .contains("replaced after it was verified")
    );
    assert!(dir.join(&targets[0]).is_file());
    assert!(!dir.join(&targets[1]).exists(), "never renamed");
    assert_eq!(
        std::fs::read(staging.join("1.zip")).unwrap(),
        b"swapped in",
        "left where it is"
    );
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Failed);
    assert_eq!(marker(&dir)["generation"], 0);
    let next = env.plan(&folder, id);
    assert_eq!(blockers(&next), BTreeSet::from([BlockerKind::Recovery]));
    assert!(next.content.blockers[0].message.contains("1.zip"));
}

#[test]
fn a_file_replaced_at_its_target_once_renamed_is_left_and_reported_uncertain() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");
    let target = dir.join(&targets[1]);
    let (at, other) = (target.clone(), env.tmp.path().join("other.zip"));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::AfterPublish(1) {
                std::fs::write(&other, b"someone else's").unwrap();
                std::fs::rename(&other, &at).unwrap();
            }
        })));

    let job = env.build(&plan);

    assert_eq!(job.state, JobState::Failed);
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Uncertain);
    assert!(
        !env.store
            .built_files(folder.id)
            .unwrap()
            .iter()
            .any(|f| f.target == targets[1])
    );
    assert!(
        !dir.join(".title-select-staging").exists(),
        "recovered at once"
    );
    assert!(!blockers(&env.plan(&folder, id)).contains(&BlockerKind::Recovery));
    assert_eq!(marker(&dir)["generation"], 0);
}

#[test]
fn recovery_removes_only_entries_it_recorded_creating() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");
    let staging = dir.join(".title-select-staging");

    // A staging directory swapped for a real one, holding a file named as a staged file.
    env.crash(&plan, Fault::AfterPublish(1));
    std::fs::rename(&staging, env.tmp.path().join("ours")).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("2.zip"), b"precious").unwrap();
    // A marker temp file no build wrote.
    std::fs::write(dir.join(".title-select.new"), b"also precious").unwrap();
    let (env, blocked) = env.restart();

    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains(".title-select.new"), "{blocked:?}");
    assert_eq!(
        std::fs::read(dir.join(".title-select.new")).unwrap(),
        b"also precious"
    );
    assert_eq!(std::fs::read(staging.join("2.zip")).unwrap(), b"precious");

    std::fs::remove_file(dir.join(".title-select.new")).unwrap();
    let (env, blocked) = env.restart();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(
        blocked[0].1.contains(".title-select-staging"),
        "{blocked:?}"
    );
    assert_eq!(std::fs::read(staging.join("2.zip")).unwrap(), b"precious");
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::Recovery));
    assert!(
        next.content
            .blockers
            .iter()
            .any(|b| b.message.contains("Remove it"))
    );

    // With the build's own staging directory back, a file in it the build did not create is still left.
    std::fs::remove_dir_all(&staging).unwrap();
    std::fs::rename(env.tmp.path().join("ours"), &staging).unwrap();
    std::fs::write(staging.join("extra.zip"), b"mine").unwrap();
    let (env, blocked) = env.restart();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains("extra.zip"), "{blocked:?}");
    assert_eq!(std::fs::read(staging.join("extra.zip")).unwrap(), b"mine");

    std::fs::remove_file(staging.join("extra.zip")).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!staging.exists());
    assert!(env.plan(&folder, id).approvable());
}

#[test]
fn recovery_waits_while_the_root_is_read_only() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    env.crash(&plan, Fault::AfterPublish(1));
    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), false);
    let (env, blocked) = env.restart();

    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains("not writable"), "{blocked:?}");
    assert!(dir.join(".title-select-staging").is_dir(), "left as it is");
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::ReadOnlyRoot));
    assert!(blockers(&next).contains(&BlockerKind::Recovery));

    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), true);
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!dir.join(".title-select-staging").exists());
    assert!(env.plan(&folder, id).approvable());
}

#[test]
fn a_builds_selection_and_audit_are_held_from_pruning_while_it_runs_and_released_when_it_fails() {
    let mut env = Env::new();
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n\n[audits]\nkeep = 1\n\n\
         [selections]\nkeep = 1\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.play().display().to_string(),
    ))
    .unwrap();
    env.audit();
    let first = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, first);
    let audit = plan.content.audit.unwrap();

    let prune = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            env.audit();
            env.record(&everything(&env.sha256));
            assert!(!env.store.selection_record(first).unwrap().pruned);
            assert!(!env.store.audit(audit).unwrap().pruned);
        }
    };
    assert_eq!(env.build_with(&plan, &prune).state, JobState::Succeeded);
    assert!(!env.store.selection_record(first).unwrap().pruned);
    assert!(!env.store.audit(audit).unwrap().pruned);

    // A Build that fails releases its hold.
    let second = env.record(&everything(&env.sha256));
    let plan = env.plan(&folder, second);
    let held = plan.content.audit.unwrap();
    env.store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    std::fs::remove_file(env.play().join("atari2600/Pong (USA).zip")).unwrap();
    let job = env.store.claim_job().unwrap().unwrap();
    let job = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(job.state, JobState::Failed, "the plan changed");
    env.audit();
    env.record(&everything(&env.sha256));
    assert!(env.store.selection_record(second).unwrap().pruned);
    assert_ne!(held, audit);
    assert!(env.store.audit(held).unwrap().pruned);
}

/// Each entry's name, bytes and mtime, for proving the folder did not change.
fn snapshot(dir: &Path) -> BTreeMap<String, (Option<Vec<u8>>, SystemTime)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = std::fs::symlink_metadata(e.path()).unwrap();
            let bytes = meta.is_file().then(|| std::fs::read(e.path()).unwrap());
            (
                e.file_name().to_string_lossy().into_owned(),
                (bytes, meta.modified().unwrap()),
            )
        })
        .collect()
}

fn titles(env: &Env, games: &[&str]) -> i64 {
    env.record(&selection(
        &env.sha256,
        games.iter().map(|g| group(g)).collect(),
    ))
}

fn plan_with(env: &Env, folder: &LibraryFolder, selection: i64, choices: PlanChoices) -> Plan {
    env.store
        .plan_build_with(&env.config, folder.id, &PlanBuild { selection, choices })
        .unwrap()
}

fn check(env: &Env, folder: &LibraryFolder) -> FolderCheck {
    let job = env.store.enqueue_check(folder.id, "Operator").unwrap();
    assert_eq!(job.kind, JobKind::Check);
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);
    let done = env.store.run_check_job(&env.config, job.id, None).unwrap();
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
    let check = env.store.latest_check(folder.id).unwrap().unwrap();
    assert_eq!(check.summary.job, Some(job.id));
    check
}

#[test]
fn a_check_reads_only_changed_files_reports_drift_and_never_writes_to_the_folder() {
    let env = Env::new();
    env.audit();
    let id = titles(
        &env,
        &[
            "Pong (USA)",
            "Combat (USA)",
            "Breakout (USA)",
            "Yars (USA)",
            "Missing (USA)",
        ],
    );
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);

    // Combat touched, Breakout rezipped from the same ROMs, Yars edited.
    let combat = std::fs::File::options()
        .write(true)
        .open(dir.join("Combat (USA).zip"))
        .unwrap();
    combat
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(combat);
    write_stored_zip(
        &dir.join("Breakout (USA).zip"),
        &[
            ("Breakout (USA).a26", b"break"),
            ("Breakout (USA).txt", b"manual"),
        ],
    );
    write_stored_zip(&dir.join("Yars (USA).zip"), &[("Yars (USA).a26", b"yarz")]);
    // Drift of every kind.
    write_file(&dir.join("notes.txt"), b"mine");
    torrentzip_file(&dir.join("pong copy.zip"), &[("Pong (USA).a26", b"pong")]);
    torrentzip_file(
        &dir.join("Ōkami (Japan).zip"),
        &[("Ōkami (Japan).a26", b"cafe")],
    );
    torrentzip_file(
        &dir.join("Missing (USA).zip"),
        &[("Missing (USA).a26", b"missing")],
    );
    std::fs::create_dir(dir.join("extras")).unwrap();
    write_file(&dir.join("extras/inside.zip"), b"never read");
    std::os::unix::fs::symlink(env.set_dir().join("Pong (USA).zip"), dir.join("link.zip")).unwrap();
    let before = snapshot(&dir);
    // Unchanged in size and mtime, Pong is never read: a read would fail.
    let pong = dir.join("Pong (USA).zip");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pong, std::fs::Permissions::from_mode(0o000)).unwrap();

    let first = check(&env, &folder);

    std::fs::set_permissions(&pong, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(snapshot(&dir), before, "a check never writes to the folder");
    let files: BTreeMap<&str, (FileOutcome, bool, bool)> = first
        .files
        .iter()
        .map(|f| (f.game.as_str(), (f.outcome, f.hashed, f.torrentzip)))
        .collect();
    assert_eq!(
        files,
        BTreeMap::from([
            ("Pong (USA)", (FileOutcome::Unchanged, false, true)),
            ("Combat (USA)", (FileOutcome::Refreshed, true, true)),
            ("Breakout (USA)", (FileOutcome::Repacked, true, false)),
            ("Yars (USA)", (FileOutcome::Conflict, true, true)),
        ])
    );
    let drift: BTreeMap<&str, (DriftKind, Option<&str>, bool)> = first
        .drift
        .iter()
        .map(|d| (d.name.as_str(), (d.kind, d.game.as_deref(), d.conflict)))
        .collect();
    assert_eq!(
        drift,
        BTreeMap::from([
            (
                "Missing (USA).zip",
                (DriftKind::Selected, Some("Missing (USA)"), true)
            ),
            ("extras", (DriftKind::Directory, None, false)),
            ("link.zip", (DriftKind::Unknown, None, false)),
            ("notes.txt", (DriftKind::Unknown, None, false)),
            (
                "pong copy.zip",
                (DriftKind::Renamed, Some("Pong (USA)"), false)
            ),
            (
                "Ōkami (Japan).zip",
                (DriftKind::Unselected, Some("Ōkami (Japan)"), false)
            ),
        ])
    );
    assert!(
        first
            .drift
            .iter()
            .all(|d| !d.name.starts_with(".title-select"))
    );

    // The records follow the touched and rezipped files; the rezipped one is not TorrentZip.
    let built = env.store.built_files(folder.id).unwrap();
    let breakout = built.iter().find(|f| f.game == "Breakout (USA)").unwrap();
    assert!(!breakout.torrentzip);
    assert_eq!(
        breakout.file.size,
        std::fs::metadata(dir.join("Breakout (USA).zip"))
            .unwrap()
            .len()
    );
    let again = check(&env, &folder);
    assert!(
        again
            .files
            .iter()
            .filter(|f| f.game != "Yars (USA)")
            .all(|f| f.outcome == FileOutcome::Unchanged && !f.hashed),
        "{:?}",
        again.files
    );

    // Drift is a warning: only the conflicts at selected games' files make the folder incomplete.
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    let counts = folder.incomplete.clone().unwrap();
    assert_eq!(
        (counts.conflict, counts.no_source, counts.total()),
        (1, 1, 2)
    );
    let incomplete = env.store.incomplete(folder.id).unwrap();
    let reasons: BTreeMap<&str, IncompleteReason> = incomplete
        .items
        .iter()
        .map(|i| (i.target.as_str(), i.reason))
        .collect();
    assert_eq!(
        reasons,
        BTreeMap::from([
            ("Missing (USA).zip", IncompleteReason::NoSource),
            ("Yars (USA).zip", IncompleteReason::Conflict),
        ])
    );
    assert!(marker(&dir).get("incomplete").is_none());

    // Planning runs the same check, without a job.
    let plan = env.plan(&folder, id);
    let planned = env.store.latest_check(folder.id).unwrap().unwrap();
    assert_eq!(plan.check, Some(planned.summary.id));
    assert_eq!(planned.summary.job, None);
    assert_eq!(kinds(&plan)["Breakout (USA)"], ItemKind::Unchanged);
    assert_eq!(kinds(&plan)["Yars (USA)"], ItemKind::Conflict);
}

#[test]
fn building_resolved_groups_only_is_a_choice_the_digest_binds_and_leaves_the_rest_incomplete() {
    let env = Env::new();
    env.audit();
    let mut unresolved = everything(&env.sha256);
    unresolved.groups[1].unresolved = true;
    let id = env.record(&unresolved);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    write_file(&dir.join("Combat (USA).zip"), b"theirs");

    let blocked = env.plan(&folder, id);
    assert_eq!(
        blockers(&blocked),
        BTreeSet::from([BlockerKind::Unresolved])
    );
    let partial = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: true,
            adopt: vec![],
            keep_aside: vec![],
            remove_all: false,
        },
    );

    assert!(partial.approvable(), "{:?}", partial.content.blockers);
    assert!(partial.content.choices.resolved_only);
    assert_ne!(partial.digest, blocked.digest);
    assert_eq!(kinds(&partial)["Combat (USA)"], ItemKind::Unresolved);
    let refused = env.store.approve_plan(
        &env.config,
        blocked.id,
        &blocked.digest,
        "Operator",
        WORKERS,
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    assert_eq!(env.build(&partial).state, JobState::Succeeded);

    assert_eq!(
        std::fs::read(dir.join("Combat (USA).zip")).unwrap(),
        b"theirs"
    );
    assert!(names(&dir).contains("Pong (USA).zip"));
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    let counts = folder.incomplete.clone().unwrap();
    assert_eq!(
        (
            counts.unresolved,
            counts.no_source,
            counts.blocked,
            counts.failed,
            counts.conflict
        ),
        (1, 1, 1, 0, 0)
    );
    let incomplete = env.store.incomplete(folder.id).unwrap();
    let combat = incomplete
        .items
        .iter()
        .find(|i| i.reason == IncompleteReason::Unresolved)
        .unwrap();
    assert_eq!(
        (combat.group.as_str(), combat.target.as_str()),
        ("combat (usa)", "Combat (USA).zip")
    );
    assert!(marker(&dir).get("incomplete").is_none());
}

#[test]
fn a_conflict_is_adopted_only_when_its_roms_match_the_dat_and_nothing_is_written() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    write_stored_zip(&dir.join("Pong (USA).zip"), &[("Pong (USA).a26", b"pong")]);
    write_stored_zip(
        &dir.join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat!")],
    );
    let before = snapshot(&dir);

    let left = env.plan(&folder, id);
    let adoptable: BTreeMap<&str, (ItemKind, bool)> = left
        .content
        .items
        .iter()
        .map(|i| (i.game.as_str(), (i.kind, i.adoptable)))
        .collect();
    assert_eq!(
        adoptable,
        BTreeMap::from([
            ("Combat (USA)", (ItemKind::Conflict, false)),
            ("Pong (USA)", (ItemKind::Conflict, true)),
        ])
    );
    let adopt = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: false,
            adopt: vec!["Pong (USA).zip".into(), "Combat (USA).zip".into()],
            keep_aside: vec![],
            remove_all: false,
        },
    );
    assert_ne!(adopt.digest, left.digest);
    assert_eq!(kinds(&adopt)["Pong (USA)"], ItemKind::Adopt);
    assert_eq!(kinds(&adopt)["Combat (USA)"], ItemKind::Conflict);
    assert_eq!(adopt.summary.adopt, 1);

    assert_eq!(env.build(&adopt).state, JobState::Succeeded);

    let after = snapshot(&dir);
    assert_eq!(after.len(), before.len());
    for (name, entry) in &before {
        if name != ".title-select" {
            assert_eq!(&after[name], entry, "{name}");
        }
    }
    let built = env.store.built_files(folder.id).unwrap();
    assert_eq!(built.len(), 1);
    assert_eq!(
        (built[0].kind, built[0].torrentzip),
        (ItemKind::Adopt, false)
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.as_ref().unwrap().conflict, 1);
    // Adopted, it is the folder's own: planned again, it is unchanged.
    assert_eq!(
        kinds(&env.plan(&folder, id))["Pong (USA)"],
        ItemKind::Unchanged
    );
}

#[test]
fn an_interrupted_build_is_badged_from_the_store_and_a_new_plan_completes_the_folder() {
    let env = Env::new();
    env.audit();
    let id = titles(
        &env,
        &["Pong (USA)", "Combat (USA)", "Breakout (USA)", "Yars (USA)"],
    );
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    env.crash(&plan, Fault::AfterPublish(2));
    let (env, _) = env.restart();

    let badged = env.store.library_folder(&env.config, folder.id).unwrap();
    let unfinished = badged.unfinished.clone().unwrap();
    assert_eq!(unfinished.state, BuildState::Interrupted);
    let detail = env.store.build(unfinished.build).unwrap();
    let count = |s: ItemState| detail.items.iter().filter(|i| i.state == s).count() as u64;
    assert_eq!(
        (unfinished.applied, unfinished.uncertain),
        (count(ItemState::Applied), count(ItemState::Uncertain))
    );
    assert!(unfinished.applied + unfinished.uncertain > 0);

    let next = env.plan(&badged, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(env.build(&next).state, JobState::Succeeded);

    let done = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(done.unfinished, None);
    assert_eq!(done.incomplete, None);
    assert_eq!(done.files, 4);
}

/// Replaces `path` with a new file holding `members`, with the modification time it had.
fn replace_keeping_mtime(path: &Path, members: &[(&str, &[u8])]) {
    let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::remove_file(path).unwrap();
    write_stored_zip(path, members);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(mtime).unwrap();
}

#[test]
fn a_file_replaced_with_the_same_size_and_mtime_is_read_again() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let pong = dir.join("Pong (USA).zip");
    write_stored_zip(&pong, &[("Pong (USA).a26", b"pung")]);
    let edited = check(&env, &folder);
    assert_eq!(edited.files[0].outcome, FileOutcome::Conflict);
    let counts = |env: &Env| {
        env.store
            .library_folder(&env.config, folder.id)
            .unwrap()
            .incomplete
            .map(|c| c.conflict)
    };
    assert_eq!(counts(&env), Some(1));

    // The same size and modification time, but another file: what was read of the edited one is not reused.
    replace_keeping_mtime(&pong, &[("Pong (USA).a26", b"pong")]);
    let fixed = check(&env, &folder);

    assert_eq!(
        (fixed.files[0].outcome, fixed.files[0].hashed),
        (FileOutcome::Repacked, true)
    );
    assert_eq!(counts(&env), None);
}

#[test]
fn planning_another_selection_keeps_the_current_builds_incomplete_badge() {
    let env = Env::new();
    env.audit();
    let both = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(
        env.build(&env.plan(&folder, both)).state,
        JobState::Succeeded
    );
    std::fs::remove_file(dir.join("Pong (USA).zip")).unwrap();

    let without = titles(&env, &["Combat (USA)"]);
    let plan = env.plan(&folder, without);
    assert!(plan.check.is_some());

    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.map(|c| c.missing), Some(1));
    let incomplete = env.store.incomplete(folder.id).unwrap();
    assert_eq!(incomplete.items[0].target, "Pong (USA).zip");
}

#[test]
fn an_unresolved_group_is_complete_only_when_its_built_file_still_holds_the_game() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)", "Yars (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    std::fs::remove_file(dir.join("Combat (USA).zip")).unwrap();
    write_stored_zip(&dir.join("Yars (USA).zip"), &[("Yars (USA).a26", b"yarz")]);

    let mut unresolved = selection(
        &env.sha256,
        vec![
            group("Pong (USA)"),
            group("Combat (USA)"),
            group("Yars (USA)"),
        ],
    );
    for g in &mut unresolved.groups {
        g.unresolved = true;
    }
    let id = env.record(&unresolved);
    let plan = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: true,
            adopt: vec![],
            keep_aside: vec![],
            remove_all: false,
        },
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let incomplete = env.store.incomplete(folder.id).unwrap();
    let reasons: BTreeMap<&str, IncompleteReason> = incomplete
        .items
        .iter()
        .map(|i| (i.target.as_str(), i.reason))
        .collect();
    assert_eq!(
        reasons,
        BTreeMap::from([
            ("Combat (USA).zip", IncompleteReason::Unresolved),
            ("Yars (USA).zip", IncompleteReason::Unresolved),
        ]),
        "the missing and the changed file stay incomplete; Pong, still as built, does not"
    );
}

#[test]
fn an_unresolved_group_whose_roms_the_newer_dat_revision_changed_is_incomplete() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);

    // The newer revision keeps Pong's title but lists another ROM for it.
    let mut newer = games();
    newer
        .iter_mut()
        .find(|(game, _)| *game == "Pong (USA)")
        .unwrap()
        .1 = vec![rom("Pong (USA).a26", b"pong, revised")];
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    let (revision, _) = env.store.import_dat(&dat, "No-Intro", None).unwrap();
    env.store
        .audit_source_set(
            &env.config,
            SET,
            Some(&revision.sha256),
            title_select_engine::AuditEvidence::Incremental,
        )
        .unwrap();
    let mut unresolved = selection(
        &revision.sha256,
        vec![group("Pong (USA)"), group("Combat (USA)")],
    );
    unresolved.groups[0].unresolved = true;
    let id = env.record(&unresolved);
    let plan = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: true,
            adopt: vec![],
            keep_aside: vec![],
            remove_all: false,
        },
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let incomplete = env.store.incomplete(folder.id).unwrap();
    let reasons: Vec<(&str, IncompleteReason)> = incomplete
        .items
        .iter()
        .map(|i| (i.target.as_str(), i.reason))
        .collect();
    assert_eq!(
        reasons,
        vec![("Pong (USA).zip", IncompleteReason::Unresolved)],
        "Pong's built file holds the ROM of the earlier revision, not the one the selection expects"
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.map(|c| c.unresolved), Some(1));
}

/// A newer DAT revision that adds Saga, a Multi-disc Game whose Disc 2 only Europe has, with a source for its
/// USA disc.
fn with_saga(env: &Env) -> String {
    let mut newer = games();
    newer.extend([
        (
            "Saga (USA) (Disc 1)",
            vec![rom("Saga (USA) (Disc 1).a26", b"saga u1")],
        ),
        (
            "Saga (Europe) (Disc 1)",
            vec![rom("Saga (Europe) (Disc 1).a26", b"saga e1")],
        ),
        (
            "Saga (Europe) (Disc 2)",
            vec![rom("Saga (Europe) (Disc 2).a26", b"saga e2")],
        ),
    ]);
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    let (revision, _) = env.store.import_dat(&dat, "No-Intro", None).unwrap();
    write_file(&env.set_dir().join("Saga (USA) (Disc 1).a26"), b"saga u1");
    env.store
        .audit_source_set(
            &env.config,
            SET,
            Some(&revision.sha256),
            title_select_engine::AuditEvidence::Incremental,
        )
        .unwrap();
    revision.sha256
}

fn saga(winners: &[&str], others: &[&str]) -> SelectionGroup {
    let mut saga = group(winners[0]);
    saga.group = "saga".into();
    saga.winners = winners.iter().map(|w| group(w).winners.remove(0)).collect();
    saga.alternatives = others
        .iter()
        .map(|title| Alternative {
            title: (*title).into(),
            clone_of: winners[0].into(),
            rule: rule("Region"),
        })
        .collect();
    saga
}

#[test]
fn a_group_whose_discs_come_from_different_releases_blocks_the_plan_and_says_why() {
    let env = Env::new();
    let sha256 = with_saga(&env);
    let folder = env.folder("atari2600");
    let mut mixed = saga(
        &["Saga (USA) (Disc 1)", "Saga (Europe) (Disc 2)"],
        &["Saga (Europe) (Disc 1)"],
    );
    mixed.unresolved = true;
    let id = env.record(&selection(&sha256, vec![group("Pong (USA)"), mixed]));

    let plan = env.plan(&folder, id);
    let blocker = plan
        .content
        .blockers
        .iter()
        .find(|b| b.kind == BlockerKind::Unresolved)
        .unwrap();
    assert_eq!(blocker.group.as_deref(), Some("saga"));
    assert!(
        blocker.message.contains(
            "its selected discs come from different releases: disc 1 from Saga (USA), disc 2 from Saga (Europe)"
        ),
        "{}",
        blocker.message
    );
}

#[test]
fn a_folder_lacking_a_disc_of_a_selected_multi_disc_game_is_incomplete_and_says_why() {
    let env = Env::new();
    let sha256 = with_saga(&env);
    let folder = env.folder("atari2600");
    // Your choice settled the group with the USA disc alone, which has no Disc 2.
    let usa = saga(
        &["Saga (USA) (Disc 1)"],
        &["Saga (Europe) (Disc 1)", "Saga (Europe) (Disc 2)"],
    );
    let id = env.record(&selection(&sha256, vec![group("Pong (USA)"), usa]));
    let plan = env.plan(&folder, id);
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let incomplete = env.store.incomplete(folder.id).unwrap();
    let items: Vec<_> = incomplete
        .items
        .iter()
        .map(|i| (i.reason, i.group.as_str(), i.game.as_str()))
        .collect();
    assert_eq!(
        items,
        vec![(
            IncompleteReason::MissingDisc,
            "saga",
            "Saga (Europe) (Disc 2)"
        )]
    );
    let detail = incomplete.items[0].detail.as_deref().unwrap();
    assert!(
        detail.starts_with("disc 2 of the multi-disc game Saga (Europe) is not selected"),
        "{detail}"
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.map(|c| c.missing_disc), Some(1));
}

#[test]
fn a_build_stopped_after_it_advanced_the_generation_is_still_badged() {
    for fault in [Fault::AfterFinalize, Fault::AfterMarker] {
        let env = Env::new();
        env.audit();
        let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
        let folder = env.folder("atari2600");
        let plan = env.plan(&folder, id);
        env.crash(&plan, fault);
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{fault:?}: {blocked:?}");

        let badged = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(badged.generation, 1, "{fault:?}");
        let unfinished = badged.unfinished.clone().unwrap();
        assert_eq!(
            (unfinished.state, unfinished.applied, unfinished.uncertain),
            (BuildState::Interrupted, 2, 0),
            "{fault:?}"
        );
        assert_eq!(Some(unfinished.build), badged.build.as_ref().map(|b| b.id));

        let next = env.plan(&badged, id);
        assert!(next.approvable(), "{fault:?}: {:?}", next.content.blockers);
        assert_eq!(env.build(&next).state, JobState::Succeeded);
        let done = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(done.unfinished, None, "{fault:?}");
    }
}

/// Pong and Combat built from their sources; Missing selected, with no source, so the folder is incomplete.
fn cleaned() -> (Env, LibraryFolder, PathBuf) {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)", "Missing (USA)"]);
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.as_ref().map(|i| i.no_source), Some(1));
    let dir = env.play().join("atari2600");
    (env, folder, dir)
}

fn cleanup(env: &Env, folder: &LibraryFolder, request: PlanCleanup) -> Plan {
    env.store
        .plan_cleanup(&env.config, folder.id, &request)
        .unwrap()
}

fn choose(entries: &[(&str, CleanupAction)]) -> PlanCleanup {
    PlanCleanup {
        entries: entries.iter().map(|(n, a)| (n.to_string(), *a)).collect(),
        ..PlanCleanup::default()
    }
}

fn restore(ids: &[i64]) -> PlanCleanup {
    PlanCleanup {
        restore: ids.to_vec(),
        ..PlanCleanup::default()
    }
}

/// The targets of the plan's cleanup blockers.
fn refused(plan: &Plan) -> BTreeSet<String> {
    assert!(!plan.approvable());
    plan.content
        .blockers
        .iter()
        .filter(|b| b.kind == BlockerKind::Cleanup)
        .filter_map(|b| b.target.clone())
        .collect()
}

fn kept_named(env: &Env, folder: &LibraryFolder, name: &str) -> title_select_api::build::KeptFile {
    env.store
        .kept_files(folder.id)
        .unwrap()
        .into_iter()
        .find(|k| k.name == name)
        .unwrap_or_else(|| panic!("{name} is not kept"))
}

fn kept_path(dir: &Path, k: &title_select_api::build::KeptFile) -> PathBuf {
    dir.join(RETAINED)
        .join(k.generation.to_string())
        .join(&k.kept_name)
}

/// Replaces `path` with a new file of `bytes`, by a rename, so that it is another file at the same name.
fn substitute(path: &Path, bytes: &[u8]) {
    let new = path.with_extension("substitute");
    std::fs::write(&new, bytes).unwrap();
    std::fs::rename(&new, path).unwrap();
}

fn items(env: &Env, folder: &LibraryFolder) -> Vec<title_select_api::build::BuildItem> {
    let latest = env.store.builds(folder.id).unwrap()[0].id;
    env.store.build(latest).unwrap().items
}

fn missing_zip(path: &Path) {
    torrentzip_file(path, &[("Missing (USA).a26", b"missing")]);
}

#[test]
fn drift_is_kept_aside_whole_or_acknowledged_until_it_changes() {
    let (env, folder, dir) = cleaned();
    write_file(&dir.join("notes.txt"), b"mine");
    std::fs::create_dir(dir.join("Disc Game")).unwrap();
    write_file(&dir.join("Disc Game/disc1.bin"), b"one");
    write_file(&dir.join("Disc Game/disc2.bin"), b"two");
    write_file(&dir.join("readme.txt"), b"leave me");
    write_file(&dir.join("untouched.txt"), b"not chosen");

    let plan = cleanup(
        &env,
        &folder,
        choose(&[
            ("notes.txt", CleanupAction::KeepAside),
            ("Disc Game", CleanupAction::KeepAside),
            ("readme.txt", CleanupAction::Acknowledge),
        ]),
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let cleanup_of = plan.content.cleanup.as_ref().unwrap();
    assert_eq!(
        cleanup_of.entries.len(),
        4,
        "every entry is bound by the plan"
    );
    assert_eq!(
        cleanup_of
            .entries
            .iter()
            .find(|e| e.name == "untouched.txt")
            .unwrap()
            .action,
        None
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    assert_eq!(
        names(&dir),
        BTreeSet::from(
            [
                ".title-select",
                RETAINED,
                "Pong (USA).zip",
                "Combat (USA).zip",
                "readme.txt",
                "untouched.txt"
            ]
            .map(String::from)
        )
    );
    let notes = kept_named(&env, &folder, "notes.txt");
    assert_eq!(
        (notes.reason, notes.directory, notes.generation),
        (KeepReason::Drift, false, 2)
    );
    assert!(notes.sha256.is_some());
    assert_eq!(std::fs::read(kept_path(&dir, &notes)).unwrap(), b"mine");
    let disc = kept_named(&env, &folder, "Disc Game");
    assert_eq!((disc.reason, disc.directory), (KeepReason::Drift, true));
    assert_eq!(disc.sha256, None);
    assert_eq!(
        std::fs::read(kept_path(&dir, &disc).join("disc2.bin")).unwrap(),
        b"two"
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.generation, 2);
    assert_eq!(marker(&dir)["generation"], 2);

    let checked = check(&env, &folder);
    let drift: Vec<&str> = checked.drift.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(drift, ["untouched.txt"]);
    let acknowledged: Vec<&str> = checked
        .acknowledged
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(acknowledged, ["readme.txt"]);
    assert_eq!(checked.summary.acknowledged, 1);

    // A kept folder is not deleted in this way.
    let deletion = env.store.plan_kept_deletion(
        &env.config,
        folder.id,
        &PlanKeptDeletion {
            ids: vec![disc.id],
            all: false,
        },
    );
    assert!(matches!(deletion, Err(Error::Refused(_))), "{deletion:?}");

    // Changed, it is drift again.
    write_file(&dir.join("readme.txt"), b"changed by you");
    let checked = check(&env, &folder);
    let drift: BTreeSet<&str> = checked.drift.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(drift, BTreeSet::from(["readme.txt", "untouched.txt"]));
    assert!(checked.acknowledged.is_empty());
}

#[test]
fn acknowledging_alone_writes_nothing_to_the_folder() {
    let (env, folder, dir) = cleaned();
    write_file(&dir.join("readme.txt"), b"leave me");
    let before = snapshot(&dir);
    let plan = cleanup(
        &env,
        &folder,
        choose(&[("readme.txt", CleanupAction::Acknowledge)]),
    );
    assert_eq!(plan.summary.acknowledge, 1);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(snapshot(&dir), before);
    let after = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(after.generation, folder.generation);
    assert_eq!(after.incomplete, folder.incomplete);
    assert_eq!(items(&env, &folder)[0].state, ItemState::Applied);
    let checked = check(&env, &after);
    assert!(checked.drift.is_empty(), "{:?}", checked.drift);
    assert_eq!(checked.acknowledged.len(), 1);

    // Replaced by another file of the same size and time, it is drift again.
    let meta = std::fs::metadata(dir.join("readme.txt")).unwrap();
    substitute(&dir.join("readme.txt"), b"swapped!");
    std::fs::File::options()
        .write(true)
        .open(dir.join("readme.txt"))
        .unwrap()
        .set_modified(meta.modified().unwrap())
        .unwrap();
    let checked = check(&env, &after);
    assert_eq!(checked.drift.len(), 1);
    assert!(checked.acknowledged.is_empty());
}

#[test]
fn a_file_holding_a_selected_game_is_adopted_under_its_name_and_never_over_anything() {
    for name in ["missing copy.zip", "Missing (USA).zip"] {
        let (env, folder, dir) = cleaned();
        missing_zip(&dir.join(name));
        let bytes = std::fs::read(dir.join(name)).unwrap();
        // An unselected game's file is never adopted.
        torrentzip_file(
            &dir.join("Ōkami (Japan).zip"),
            &[("Ōkami (Japan).a26", b"cafe")],
        );
        let unselected = cleanup(
            &env,
            &folder,
            choose(&[("Ōkami (Japan).zip", CleanupAction::Adopt)]),
        );
        assert_eq!(
            refused(&unselected),
            BTreeSet::from(["Ōkami (Japan).zip".to_string()])
        );

        let plan = cleanup(&env, &folder, choose(&[(name, CleanupAction::Adopt)]));
        assert!(plan.approvable(), "{name}: {:?}", plan.content.blockers);
        let entry = plan.content.cleanup.as_ref().unwrap().entries.iter();
        let entry = entry.clone().find(|e| e.name == name).unwrap();
        assert_eq!(entry.target.as_deref(), Some("Missing (USA).zip"));
        assert_eq!(env.build(&plan).state, JobState::Succeeded);

        assert_eq!(std::fs::read(dir.join("Missing (USA).zip")).unwrap(), bytes);
        assert!(!dir.join("missing copy.zip").exists());
        let built = env.store.built_files(folder.id).unwrap();
        let missing = built.iter().find(|f| f.game == "Missing (USA)").unwrap();
        assert_eq!(
            (missing.kind, missing.torrentzip),
            (ItemKind::Adopt, true),
            "{name}"
        );
        let after = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(
            (after.generation, after.incomplete.clone()),
            (2, None),
            "{name}"
        );
        let checked = check(&env, &after);
        let drift: Vec<&str> = checked.drift.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(drift, ["Ōkami (Japan).zip"], "{name}");
        let adopted = checked
            .files
            .iter()
            .find(|f| f.game == "Missing (USA)")
            .unwrap();
        assert_eq!(
            (adopted.outcome, adopted.hashed),
            (FileOutcome::Unchanged, false)
        );
    }

    // A file at its game's file name is never replaced: adopting the copy waits until it is kept aside.
    let (env, folder, dir) = cleaned();
    missing_zip(&dir.join("missing copy.zip"));
    write_file(&dir.join("Missing (USA).zip"), b"not a zip");
    let collides = cleanup(
        &env,
        &folder,
        choose(&[("missing copy.zip", CleanupAction::Adopt)]),
    );
    assert_eq!(
        refused(&collides),
        BTreeSet::from(["missing copy.zip".to_string()])
    );
    let plan = cleanup(
        &env,
        &folder,
        choose(&[
            ("missing copy.zip", CleanupAction::Adopt),
            ("Missing (USA).zip", CleanupAction::KeepAside),
        ]),
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let kept = kept_named(&env, &folder, "Missing (USA).zip");
    assert_eq!(std::fs::read(kept_path(&dir, &kept)).unwrap(), b"not a zip");
    validate(&dir.join("Missing (USA).zip"));
    assert!(
        env.store
            .library_folder(&env.config, folder.id)
            .unwrap()
            .incomplete
            .is_none()
    );
}

#[test]
fn a_kept_file_is_put_back_or_adopted_and_refused_when_its_name_is_taken() {
    let (env, folder, dir) = cleaned();
    missing_zip(&dir.join("Missing (USA).zip"));
    missing_zip(&dir.join("missing copy.zip"));
    write_file(&dir.join("notes.txt"), b"mine");
    std::fs::create_dir(dir.join("Disc Game")).unwrap();
    write_file(&dir.join("Disc Game/disc1.bin"), b"one");
    let plan = cleanup(
        &env,
        &folder,
        choose(&[
            ("Missing (USA).zip", CleanupAction::KeepAside),
            ("missing copy.zip", CleanupAction::KeepAside),
            ("notes.txt", CleanupAction::KeepAside),
            ("Disc Game", CleanupAction::KeepAside),
        ]),
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let missing = kept_named(&env, &folder, "Missing (USA).zip");
    let copy = kept_named(&env, &folder, "missing copy.zip");
    let notes = kept_named(&env, &folder, "notes.txt");
    let disc = kept_named(&env, &folder, "Disc Game");

    // Its name taken, a kept file is not put back.
    write_file(&dir.join("notes.txt"), b"new notes");
    let taken = cleanup(&env, &folder, restore(&[notes.id]));
    assert_eq!(refused(&taken), BTreeSet::from(["notes.txt".to_string()]));
    std::fs::remove_file(dir.join("notes.txt")).unwrap();

    // The copy holds Missing under another name, so it is adopted under Missing's file name.
    let plan = cleanup(&env, &folder, restore(&[copy.id, notes.id, disc.id]));
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let restores: BTreeMap<&str, (RestoreKind, &str)> = plan
        .content
        .cleanup
        .as_ref()
        .unwrap()
        .restores
        .iter()
        .map(|r| (r.kept.name.as_str(), (r.kind, r.target.as_str())))
        .collect();
    assert_eq!(
        restores,
        BTreeMap::from([
            ("Disc Game", (RestoreKind::PutBack, "Disc Game")),
            (
                "missing copy.zip",
                (RestoreKind::Adopt, "Missing (USA).zip")
            ),
            ("notes.txt", (RestoreKind::PutBack, "notes.txt")),
        ])
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(dir.join("Disc Game/disc1.bin")).unwrap(),
        b"one"
    );
    validate(&dir.join("Missing (USA).zip"));
    assert!(!kept_path(&dir, &copy).exists());
    let built = env.store.built_files(folder.id).unwrap();
    assert!(built.iter().any(|f| f.target == "Missing (USA).zip"));
    let after = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!((after.generation, after.incomplete.clone()), (3, None));
    let still: Vec<i64> = env
        .store
        .kept_files(folder.id)
        .unwrap()
        .iter()
        .map(|k| k.id)
        .collect();
    assert_eq!(still, [missing.id]);

    // Missing's file name is taken now, by the adopted copy.
    let taken = cleanup(&env, &folder, restore(&[missing.id]));
    assert_eq!(
        refused(&taken),
        BTreeSet::from(["Missing (USA).zip".to_string()])
    );

    // Freed, the kept file goes back under its own name, which is its game's.
    std::fs::remove_file(dir.join("Missing (USA).zip")).unwrap();
    let plan = cleanup(&env, &folder, restore(&[missing.id]));
    assert_eq!(
        plan.content.cleanup.as_ref().unwrap().restores[0].kind,
        RestoreKind::Restore
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    validate(&dir.join("Missing (USA).zip"));
    assert!(env.store.kept_files(folder.id).unwrap().is_empty());
    let checked = check(&env, &after);
    assert!(
        checked
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged),
        "{:?}",
        checked.files
    );
}

#[test]
fn a_built_file_that_is_not_torrentzip_is_repacked_from_its_contents_with_the_old_copy_kept() {
    let (env, folder, dir) = cleaned();
    write_stored_zip(
        &dir.join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    let old = std::fs::read(dir.join("Combat (USA).zip")).unwrap();
    let repack = |target: &str| PlanCleanup {
        repack: vec![target.into()],
        ..PlanCleanup::default()
    };
    let already = cleanup(&env, &folder, repack("Pong (USA).zip"));
    assert_eq!(
        refused(&already),
        BTreeSet::from(["Pong (USA).zip".to_string()])
    );

    let plan = cleanup(&env, &folder, repack("Combat (USA).zip"));
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let members = validate(&dir.join("Combat (USA).zip"));
    assert_eq!(members.len(), 1);
    let kept = kept_named(&env, &folder, "Combat (USA).zip");
    assert_eq!(kept.reason, KeepReason::Repack);
    assert_eq!(std::fs::read(kept_path(&dir, &kept)).unwrap(), old);
    let built = env.store.built_files(folder.id).unwrap();
    let combat = built.iter().find(|f| f.game == "Combat (USA)").unwrap();
    assert_eq!((combat.kind, combat.torrentzip), (ItemKind::Repack, true));
    assert!(!dir.join(".title-select-staging").exists());
    let checked = check(&env, &folder);
    assert!(checked.drift.is_empty(), "{:?}", checked.drift);
    let combat = checked
        .files
        .iter()
        .find(|f| f.game == "Combat (USA)")
        .unwrap();
    assert_eq!(
        (combat.outcome, combat.torrentzip),
        (FileOutcome::Unchanged, true)
    );
}

#[test]
fn a_substituted_entry_is_never_touched_and_a_stale_cleanup_is_refused() {
    let (env, folder, dir) = cleaned();
    write_file(&dir.join("notes.txt"), b"mine");
    missing_zip(&dir.join("missing copy.zip"));

    // Substituted between planning and approval.
    let plan = cleanup(
        &env,
        &folder,
        choose(&[("notes.txt", CleanupAction::KeepAside)]),
    );
    substitute(&dir.join("notes.txt"), b"mine");
    let stale = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(matches!(stale, Err(Error::Conflict(_))), "{stale:?}");

    // Something new in the folder since planning makes it stale too.
    let plan = cleanup(
        &env,
        &folder,
        choose(&[("notes.txt", CleanupAction::KeepAside)]),
    );
    write_file(&dir.join("late.txt"), b"late");
    let stale = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(matches!(stale, Err(Error::Conflict(_))), "{stale:?}");
    assert!(env.store.kept_files(folder.id).unwrap().is_empty());

    // Substituted after every check, just before it is moved.
    for (name, action, fault) in [
        ("notes.txt", CleanupAction::KeepAside, Fault::BeforeKeep(0)),
        (
            "missing copy.zip",
            CleanupAction::Adopt,
            Fault::BeforeMove(0),
        ),
    ] {
        let plan = cleanup(&env, &folder, choose(&[(name, action)]));
        assert!(plan.approvable(), "{name}: {:?}", plan.content.blockers);
        let at = dir.join(name);
        env.store.hook_faults(Some(std::sync::Arc::new(move |f| {
            if f == fault {
                substitute(&at, b"someone else's");
            }
        })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        assert_eq!(job.state, JobState::Succeeded, "{name}: {:?}", job.error);
        let item = &items(&env, &folder)[0];
        assert_eq!(item.state, ItemState::Failed, "{name}: {item:?}");
        assert_eq!(
            std::fs::read(dir.join(name)).unwrap(),
            b"someone else's",
            "{name}"
        );
    }
    assert!(env.store.kept_files(folder.id).unwrap().is_empty());
    assert!(!dir.join("Missing (USA).zip").exists());
    assert!(
        env.store
            .built_files(folder.id)
            .unwrap()
            .iter()
            .all(|f| f.game != "Missing (USA)")
    );
}

#[test]
fn a_crash_at_each_step_of_a_move_recovers_without_losing_or_untracking_anything() {
    for fault in [
        Fault::BeforeMove(0),
        Fault::AfterMove(0),
        Fault::MoveSynced(0),
    ] {
        // Adopting a file under its game's file name.
        let (env, folder, dir) = cleaned();
        missing_zip(&dir.join("missing copy.zip"));
        let plan = cleanup(
            &env,
            &folder,
            choose(&[("missing copy.zip", CleanupAction::Adopt)]),
        );
        env.crash(&plan, fault);
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{fault:?}: {blocked:?}");
        let built = env
            .store
            .built_files(folder.id)
            .unwrap()
            .into_iter()
            .any(|f| f.target == "Missing (USA).zip");
        let item = &items(&env, &folder)[0];
        if fault == Fault::BeforeMove(0) {
            assert!(dir.join("missing copy.zip").exists(), "{fault:?}");
            assert!(!dir.join("Missing (USA).zip").exists(), "{fault:?}");
            assert!(!built, "{fault:?}");
            assert_eq!(item.state, ItemState::Failed, "{fault:?}");
        } else {
            validate(&dir.join("Missing (USA).zip"));
            assert!(!dir.join("missing copy.zip").exists(), "{fault:?}");
            assert!(built, "{fault:?}");
            assert_eq!(item.state, ItemState::Applied, "{fault:?}");
        }
        let next = cleanup(
            &env,
            &folder,
            choose(&[(
                if built {
                    "Missing (USA).zip"
                } else {
                    "missing copy.zip"
                },
                CleanupAction::KeepAside,
            )]),
        );
        let next_ok = if built {
            // Built, it is no longer drift.
            !next.approvable()
        } else {
            next.approvable()
        };
        assert!(next_ok, "{fault:?}: {:?}", next.content.blockers);

        // Putting a kept file back.
        let (env, folder, dir) = cleaned();
        missing_zip(&dir.join("Missing (USA).zip"));
        let keep = cleanup(
            &env,
            &folder,
            choose(&[("Missing (USA).zip", CleanupAction::KeepAside)]),
        );
        assert_eq!(env.build(&keep).state, JobState::Succeeded);
        let kept = kept_named(&env, &folder, "Missing (USA).zip");
        let plan = cleanup(&env, &folder, restore(&[kept.id]));
        env.crash(&plan, fault);
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{fault:?}: {blocked:?}");
        let listed = env.store.kept_files(folder.id).unwrap();
        if fault == Fault::BeforeMove(0) {
            assert!(kept_path(&dir, &kept).exists(), "{fault:?}");
            assert_eq!(listed, vec![kept.clone()], "{fault:?}");
            assert!(!dir.join("Missing (USA).zip").exists());
        } else {
            assert!(!kept_path(&dir, &kept).exists(), "{fault:?}");
            assert!(listed.is_empty(), "{fault:?}");
            validate(&dir.join("Missing (USA).zip"));
            assert!(
                env.store
                    .built_files(folder.id)
                    .unwrap()
                    .iter()
                    .any(|f| f.target == "Missing (USA).zip"),
                "{fault:?}"
            );
        }
    }

    // Keeping aside, a file or a folder.
    for fault in [
        Fault::BeforeKeep(0),
        Fault::KeptUnsynced(0),
        Fault::AfterKeep(0),
    ] {
        for name in ["notes.txt", "Disc Game"] {
            let (env, folder, dir) = cleaned();
            if name == "Disc Game" {
                std::fs::create_dir(dir.join(name)).unwrap();
                write_file(&dir.join("Disc Game/disc1.bin"), b"one");
            } else {
                write_file(&dir.join(name), b"mine");
            }
            let plan = cleanup(&env, &folder, choose(&[(name, CleanupAction::KeepAside)]));
            env.crash(&plan, fault);
            let (env, blocked) = env.restart();
            assert!(blocked.is_empty(), "{fault:?} {name}: {blocked:?}");
            let listed = env.store.kept_files(folder.id).unwrap();
            if fault == Fault::BeforeKeep(0) {
                assert!(dir.join(name).exists(), "{fault:?} {name}");
                assert!(listed.is_empty(), "{fault:?} {name}");
            } else {
                assert!(!dir.join(name).exists(), "{fault:?} {name}");
                assert_eq!(listed.len(), 1, "{fault:?} {name}");
                assert_eq!(listed[0].directory, name == "Disc Game");
                assert!(kept_path(&dir, &listed[0]).exists(), "{fault:?} {name}");
            }
        }
    }
}

/// Pong and Combat built from their sources, so the folder is complete; then Combat rewritten as a stored ZIP, which
/// is not TorrentZip. Returns Combat's old bytes too.
fn complete_with_a_stored_zip() -> (Env, LibraryFolder, PathBuf, Vec<u8>) {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let dir = env.play().join("atari2600");
    write_stored_zip(
        &dir.join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    let old = std::fs::read(dir.join("Combat (USA).zip")).unwrap();
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete, None);
    (env, folder, dir, old)
}

fn repack(target: &str) -> PlanCleanup {
    PlanCleanup {
        repack: vec![target.into()],
        ..PlanCleanup::default()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    <sha2::Sha256 as sha2::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn a_crash_at_each_step_of_a_repack_recovers_and_a_missing_copy_leaves_the_folder_incomplete() {
    for fault in [
        Fault::MadeStaging,
        Fault::BeforePublishStaging,
        Fault::BeforeKeep(0),
        Fault::KeptUnsynced(0),
        Fault::AfterKeep(0),
        Fault::BeforePublish(0),
        Fault::PublishedUnsynced(0),
        Fault::AfterPublish(0),
    ] {
        let (env, folder, dir, old) = complete_with_a_stored_zip();
        let plan = cleanup(&env, &folder, repack("Combat (USA).zip"));
        assert!(plan.approvable(), "{fault:?}: {:?}", plan.content.blockers);
        env.crash(&plan, fault);
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{fault:?}: {blocked:?}");
        assert!(!dir.join(".title-select-staging").exists(), "{fault:?}");

        let kept = env.store.kept_files(folder.id).unwrap();
        let combat = dir.join("Combat (USA).zip");
        let built = env.store.built_files(folder.id).unwrap();
        let built = built.iter().find(|f| f.target == "Combat (USA).zip");
        let published = matches!(fault, Fault::PublishedUnsynced(_) | Fault::AfterPublish(_));
        let kept_aside = published
            || matches!(
                fault,
                Fault::KeptUnsynced(_) | Fault::AfterKeep(_) | Fault::BeforePublish(_)
            );
        if kept_aside {
            assert_eq!(kept.len(), 1, "{fault:?}");
            assert_eq!(std::fs::read(kept_path(&dir, &kept[0])).unwrap(), old);
        } else {
            assert!(kept.is_empty(), "{fault:?}");
            assert_eq!(std::fs::read(&combat).unwrap(), old, "{fault:?}");
        }
        if published {
            validate(&combat);
            assert_eq!(built.map(|f| f.torrentzip), Some(true), "{fault:?}");
        } else if kept_aside {
            assert!(!combat.exists(), "{fault:?}");
            assert!(built.is_none(), "{fault:?}");
        } else {
            assert_eq!(built.map(|f| f.torrentzip), Some(false), "{fault:?}");
        }
        // Kept aside with no repacked copy in its place, Combat is missing: the folder says so, before and after a
        // check.
        let missing = u64::from(kept_aside && !published);
        let after = env.store.library_folder(&env.config, folder.id).unwrap();
        let failed = |f: &LibraryFolder| f.incomplete.as_ref().map_or(0, |i| i.failed);
        assert_eq!(failed(&after), missing, "{fault:?}");
        check(&env, &after);
        let after = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(failed(&after), missing, "{fault:?}");
        assert_eq!(
            after.incomplete.as_ref().map_or(0, |i| i.missing),
            0,
            "{fault:?}"
        );

        // Nothing is left that blocks the folder: putting the kept copy back, or repacking again, can be planned.
        let next = if kept_aside && !published {
            cleanup(&env, &folder, restore(&[kept[0].id]))
        } else if published {
            env.plan(&after, after.build.as_ref().unwrap().selection)
        } else {
            cleanup(&env, &folder, repack("Combat (USA).zip"))
        };
        assert!(next.approvable(), "{fault:?}: {:?}", next.content.blockers);
        assert_eq!(env.build(&next).state, JobState::Succeeded, "{fault:?}");
        let done = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(done.incomplete, None, "{fault:?}");
    }
}

#[test]
fn a_file_put_in_place_is_recorded_only_once_recovery_makes_its_rename_durable() {
    // A Build's file and a repacked copy.
    for repacking in [false, true] {
        let (env, folder, plan) = if repacking {
            let (env, folder, _, _) = complete_with_a_stored_zip();
            let plan = cleanup(&env, &folder, repack("Combat (USA).zip"));
            (env, folder, plan)
        } else {
            let env = Env::new();
            env.audit();
            let id = titles(&env, &["Pong (USA)"]);
            let folder = env.folder("atari2600");
            let plan = env.plan(&folder, id);
            (env, folder, plan)
        };
        let target = if repacking {
            "Combat (USA).zip"
        } else {
            "Pong (USA).zip"
        };
        env.crash(&plan, Fault::PublishedUnsynced(0));
        // Recovery syncs both folders before it records the file in place, and records nothing until it has.
        let (env, blocked) = env.restart_until(Some(Fault::AfterPublish(0)));
        assert!(
            blocked.is_none(),
            "{repacking}: recovery synced, then stopped"
        );
        let item = items(&env, &folder)
            .into_iter()
            .find(|i| i.target == target)
            .unwrap();
        assert_ne!(item.state, ItemState::Applied, "{repacking}: {item:?}");
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{repacking}: {blocked:?}");
        let item = items(&env, &folder)
            .into_iter()
            .find(|i| i.target == target)
            .unwrap();
        assert_eq!(item.state, ItemState::Applied, "{repacking}: {item:?}");
        validate(&env.play().join("atari2600").join(target));
    }
}

#[test]
fn a_staging_folder_left_mid_creation_is_recorded_by_recovery_and_never_blocks() {
    for fault in [Fault::MadeStaging, Fault::BeforePublishStaging] {
        for renamed in [false, true] {
            if renamed && fault == Fault::MadeStaging {
                continue;
            }
            let env = Env::new();
            env.audit();
            let id = titles(&env, &["Pong (USA)"]);
            let folder = env.folder("atari2600");
            let dir = env.play().join("atari2600");
            env.crash(&env.plan(&folder, id), fault);
            let temps: Vec<String> = names(&dir)
                .into_iter()
                .filter(|n| n.starts_with(".title-select-staging.new-"))
                .collect();
            assert_eq!(temps.len(), 1, "{fault:?}: {temps:?}");
            if renamed {
                // As if the stop came just after the rename.
                std::fs::rename(dir.join(&temps[0]), dir.join(".title-select-staging")).unwrap();
            }
            let (env, blocked) = env.restart();
            assert!(blocked.is_empty(), "{fault:?} {renamed}: {blocked:?}");
            assert!(!dir.join(".title-select-staging").exists());

            let folder = env.store.library_folder(&env.config, folder.id).unwrap();
            assert!(check(&env, &folder).drift.is_empty(), "{fault:?} {renamed}");
            let next = env.plan(&folder, id);
            assert!(
                next.approvable(),
                "{fault:?} {renamed}: {:?}",
                next.content.blockers
            );
            assert_eq!(env.build(&next).state, JobState::Succeeded);
            validate(&dir.join("Pong (USA).zip"));
        }
    }
}

#[test]
fn acknowledging_drift_never_settles_a_build_that_stopped() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    env.crash(&env.plan(&folder, id), Fault::AfterMarker);
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    let stopped = folder.unfinished.clone().unwrap();

    write_file(&env.play().join("atari2600/notes.txt"), b"mine");
    let plan = cleanup(
        &env,
        &folder,
        choose(&[("notes.txt", CleanupAction::Acknowledge)]),
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let after = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(after.unfinished, Some(stopped));
    assert_eq!(
        after.build.as_ref().map(|b| b.selection),
        folder.build.as_ref().map(|b| b.selection)
    );

    // A build that succeeds does settle it.
    let next = env.plan(&after, id);
    assert_eq!(env.build(&next).state, JobState::Succeeded);
    let done = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(done.unfinished, None);
}

#[test]
fn a_check_never_drops_an_acknowledgement_made_after_it_read_the_store() {
    let (env, folder, dir) = cleaned();
    write_file(&dir.join("readme.txt"), b"one");
    let ack = choose(&[("readme.txt", CleanupAction::Acknowledge)]);
    assert_eq!(
        env.build(&cleanup(&env, &folder, ack.clone())).state,
        JobState::Succeeded
    );
    // It changes, so the acknowledgement no longer holds; you acknowledge it again while another plan is being
    // made from a check that read the store before you did.
    write_file(&dir.join("readme.txt"), b"changed");
    write_file(&dir.join("other.txt"), b"other");
    let (scanned, resume) = (
        std::sync::Arc::new(std::sync::Barrier::new(2)),
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );
    let first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (at, go) = (scanned.clone(), resume.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::Scanned && first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                at.wait();
                go.wait();
            }
        })));
    std::thread::scope(|s| {
        let outer = s.spawn(|| {
            cleanup(
                &env,
                &folder,
                choose(&[("other.txt", CleanupAction::KeepAside)]),
            )
        });
        scanned.wait();
        let again = cleanup(&env, &folder, ack.clone());
        assert!(again.approvable(), "{:?}", again.content.blockers);
        assert_eq!(env.build(&again).state, JobState::Succeeded);
        resume.wait();
        outer.join().unwrap();
    });
    env.store.hook_faults(None);

    let checked = check(&env, &folder);
    assert_eq!(
        checked
            .acknowledged
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["readme.txt"],
        "{checked:?}"
    );
    assert_eq!(
        checked
            .drift
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["other.txt"]
    );
}

#[test]
fn a_cleanup_is_refused_at_approval_once_anything_in_the_folder_changed() {
    for change in [
        "a built file touched",
        "a built file gone",
        "a kept file gone",
    ] {
        let (env, folder, dir) = cleaned();
        write_file(&dir.join("notes.txt"), b"mine");
        write_file(&dir.join("old.txt"), b"old");
        assert_eq!(
            env.build(&cleanup(
                &env,
                &folder,
                choose(&[("old.txt", CleanupAction::KeepAside)])
            ))
            .state,
            JobState::Succeeded
        );
        let folder = env.store.library_folder(&env.config, folder.id).unwrap();
        let plan = cleanup(
            &env,
            &folder,
            choose(&[("notes.txt", CleanupAction::Acknowledge)]),
        );
        assert!(plan.approvable(), "{change}: {:?}", plan.content.blockers);
        let pong = dir.join("Pong (USA).zip");
        match change {
            "a built file touched" => std::fs::File::options()
                .write(true)
                .open(&pong)
                .unwrap()
                .set_modified(SystemTime::now() + Duration::from_secs(60))
                .unwrap(),
            "a built file gone" => std::fs::remove_file(&pong).unwrap(),
            _ => {
                let deletion = env
                    .store
                    .plan_kept_deletion(
                        &env.config,
                        folder.id,
                        &PlanKeptDeletion {
                            ids: vec![kept_named(&env, &folder, "old.txt").id],
                            all: false,
                        },
                    )
                    .unwrap();
                env.store
                    .approve_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
                    .unwrap();
            }
        }
        let stale = env
            .store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
        assert!(
            matches!(stale, Err(Error::Conflict(_))),
            "{change}: {stale:?}"
        );
    }
}

#[test]
fn a_repack_keeps_the_old_copy_aside_by_the_hash_it_read_while_rewriting_it() {
    for touched in [false, true] {
        let (env, folder, dir, old) = complete_with_a_stored_zip();
        let plan = cleanup(&env, &folder, repack("Combat (USA).zip"));
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = reads.clone();
        let combat = dir.join("Combat (USA).zip");
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                match fault {
                    Fault::KeepRead(0) => {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    // Its metadata changes after it was read, though not its content, size or modification time.
                    Fault::BeforeKeep(0) if touched => {
                        let mode = std::fs::metadata(&combat).unwrap().permissions();
                        std::fs::set_permissions(&combat, mode).unwrap();
                    }
                    _ => {}
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        assert_eq!(job.state, JobState::Succeeded, "{touched}: {:?}", job.error);
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(touched),
            "{touched}: read again only once it changed"
        );
        let kept = kept_named(&env, &folder, "Combat (USA).zip");
        assert_eq!(kept.sha256.as_deref(), Some(sha256_hex(&old).as_str()));
        assert_eq!(std::fs::read(kept_path(&dir, &kept)).unwrap(), old);
        validate(&dir.join("Combat (USA).zip"));
    }
}

#[test]
fn an_interrupted_repack_of_a_file_no_longer_selected_leaves_the_folder_lacking_nothing() {
    let (env, folder, dir, old) = complete_with_a_stored_zip();
    // Combat's group becomes unresolved, with Yars its winner; building the resolved groups only leaves Combat's
    // built file in place, built but no longer selected.
    let mut unresolved = group("Combat (USA)");
    unresolved.unresolved = true;
    unresolved.winners[0].title = "Yars (USA)".into();
    unresolved.alternatives = vec![Alternative {
        title: "Combat (USA)".into(),
        clone_of: "Yars (USA)".into(),
        rule: rule("Region"),
    }];
    let id = env.record(&selection(
        &env.sha256,
        vec![group("Pong (USA)"), unresolved],
    ));
    let partial = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: true,
            adopt: vec![],
            keep_aside: vec![],
            remove_all: false,
        },
    );
    assert!(partial.approvable(), "{:?}", partial.content.blockers);
    assert_eq!(env.build(&partial).state, JobState::Succeeded);
    let combat = dir.join("Combat (USA).zip");
    assert_eq!(std::fs::read(&combat).unwrap(), old);
    let lacking = |env: &Env| {
        let folder = env.store.library_folder(&env.config, folder.id).unwrap();
        let counts = folder.incomplete.clone().unwrap();
        (counts.unresolved, counts.failed, counts.missing)
    };
    assert_eq!(lacking(&env), (1, 0, 0), "only Yars, unresolved");

    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    let plan = cleanup(&env, &folder, repack("Combat (USA).zip"));
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    env.crash(&plan, Fault::BeforePublish(0));
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!combat.exists());
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read(kept_path(&dir, &kept[0])).unwrap(), old);

    // Combat is not selected, so the folder lacks nothing more for it, before or after a check.
    assert_eq!(lacking(&env), (1, 0, 0));
    check(&env, &folder);
    assert_eq!(lacking(&env), (1, 0, 0));
    let incomplete = env.store.incomplete(folder.id).unwrap();
    assert!(
        incomplete
            .items
            .iter()
            .all(|i| i.target != "Combat (USA).zip"),
        "{incomplete:?}"
    );
}

#[test]
fn a_build_reading_a_source_set_and_a_repair_of_it_never_overlap() {
    let mut env = Env::new();
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.play().display().to_string(),
    ))
    .unwrap();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let build = env.plan(&folder, id);
    let repair = env
        .store
        .plan_repair(&env.config, SET, &Default::default())
        .unwrap();

    let job = env
        .store
        .approve_plan(&env.config, build.id, &build.digest, "Operator", WORKERS)
        .unwrap();
    let refused =
        env.store
            .approve_repair(&env.config, repair.id, &repair.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains(&format!("build job {}", job.id))),
        "{refused:?}"
    );

    env.store.claim_job().unwrap().unwrap();
    let built = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(built.state, JobState::Succeeded, "{built:?}");
    let repair = env
        .store
        .approve_repair(&env.config, repair.id, &repair.digest, "Operator", WORKERS)
        .unwrap();
    let again = env.plan(&folder, id);
    let refused = env
        .store
        .approve_plan(&env.config, again.id, &again.digest, "Operator", WORKERS);
    assert!(
        matches!(
            &refused,
            Err(Error::JobActive { job, kind: JobKind::Repair, .. }) if Some(*job) == repair.job
        ),
        "{refused:?}"
    );
}

const SYSTEM: &str = "No-Intro/Test - System";

/// Sets the play libraries' storage policy: its default, and, when `own` is given, this system's own format.
fn set_play_policy(env: &Env, default: StorageFormat, own: Option<StorageFormat>, delete: bool) {
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.format = default;
    policy.play_libraries.delete_originals = delete;
    policy.play_libraries.systems.clear();
    if let Some(format) = own {
        policy.play_libraries.systems.insert(
            SYSTEM.into(),
            SystemStorage {
                format: Some(format),
                delete_originals: None,
            },
        );
    }
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

/// Builds Pong, Combat and Breakout into a new folder as TorrentZips. Returns the folder and the selection.
fn built_as_zips(env: &Env) -> (LibraryFolder, i64) {
    env.audit();
    let id = titles(env, &["Pong (USA)", "Combat (USA)", "Breakout (USA)"]);
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    (folder, id)
}

fn item<'a>(plan: &'a Plan, game: &str) -> &'a title_select_api::build::PlanItem {
    plan.content
        .items
        .iter()
        .find(|i| i.game == game)
        .unwrap_or_else(|| panic!("no {game} in {:#?}", plan.content.items))
}

/// Approves `plan` and runs its Build Job with `hook` called at each fault it passes.
fn build_hooked(
    env: &Env,
    plan: &Plan,
    hook: impl Fn(Fault) + Send + Sync + 'static,
) -> title_select_api::Job {
    env.store.hook_faults(Some(std::sync::Arc::new(hook)));
    let job = env.build(plan);
    env.store.hook_faults(None);
    job
}

#[test]
fn a_loose_file_is_adopted_where_the_storage_policy_stores_its_game_loose() {
    let env = Env::new();
    set_play_policy(&env, StorageFormat::Loose, None, false);
    env.audit();
    let id = titles(&env, &["Pong (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    write_file(&dir.join("Pong (USA).a26"), b"pong");
    let plan = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: false,
            adopt: vec!["Pong (USA).a26".into()],
            keep_aside: vec![],
            remove_all: false,
        },
    );
    assert_eq!(kinds(&plan)["Pong (USA)"], ItemKind::Adopt);

    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let built = env.store.built_files(folder.id).unwrap();
    assert_eq!(
        (built[0].target.as_str(), built[0].kind),
        ("Pong (USA).a26", ItemKind::Adopt),
        "{built:?}"
    );
    assert_eq!(std::fs::read(dir.join("Pong (USA).a26")).unwrap(), b"pong");
}

#[test]
fn a_cleanup_adopts_a_loose_file_where_the_storage_policy_stores_its_game_loose() {
    let (env, folder, dir) = cleaned();
    set_play_policy(&env, StorageFormat::Loose, None, false);
    write_file(&dir.join("missing copy.a26"), b"missing");

    let plan = cleanup(
        &env,
        &folder,
        choose(&[("missing copy.a26", CleanupAction::Adopt)]),
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let entry = plan.content.cleanup.as_ref().unwrap().entries.iter();
    let entry = entry
        .clone()
        .find(|e| e.name == "missing copy.a26")
        .unwrap();
    assert_eq!(entry.target.as_deref(), Some("Missing (USA).a26"));
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    assert_eq!(
        std::fs::read(dir.join("Missing (USA).a26")).unwrap(),
        b"missing"
    );
    let built = env.store.built_files(folder.id).unwrap();
    let missing = built.iter().find(|f| f.game == "Missing (USA)").unwrap();
    assert_eq!(
        (missing.target.as_str(), missing.kind),
        ("Missing (USA).a26", ItemKind::Adopt)
    );
}

#[test]
fn a_loose_file_is_written_only_from_the_source_file_the_plan_found() {
    for swap in ["symlink", "other file"] {
        let env = Env::new();
        set_play_policy(&env, StorageFormat::Loose, None, false);
        env.audit();
        let id = titles(&env, &["Combat (USA)"]);
        let folder = env.folder("atari2600");
        let plan = env.plan(&folder, id);
        let combat = item(&plan, "Combat (USA)");
        assert_eq!((combat.kind, combat.loose), (ItemKind::Repack, true));
        let source = env.set_dir().join(&combat.sources[0].path);
        let job = env
            .store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let meta = std::fs::metadata(&source).unwrap();
        let elsewhere = env.set_dir().join("elsewhere.zip");
        std::fs::copy(&source, &elsewhere).unwrap();
        std::fs::remove_file(&source).unwrap();
        if swap == "symlink" {
            std::os::unix::fs::symlink(&elsewhere, &source).unwrap();
        } else {
            std::fs::copy(&elsewhere, &source).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&source)
                .unwrap()
                .set_modified(meta.modified().unwrap())
                .unwrap();
        }

        env.store.claim_job().unwrap().unwrap();
        env.store.run_build_job(&env.config, job.id, None).unwrap();

        let dir = env.play().join("atari2600");
        assert!(!dir.join("Combat (USA).a26").exists(), "{swap}");
        assert!(
            env.store.built_files(folder.id).unwrap().is_empty(),
            "{swap}"
        );
    }
}

#[test]
fn a_play_library_is_converted_to_loose_and_back_with_each_original_kept() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    let dir = env.play().join("atari2600");
    let zipped = std::fs::read(dir.join("Pong (USA).zip")).unwrap();

    // The default stays TorrentZip; this system's own format says loose.
    set_play_policy(
        &env,
        StorageFormat::TorrentZip,
        Some(StorageFormat::Loose),
        false,
    );
    let plan = env.plan(&folder, id);
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(
        (pong.target.as_str(), pong.loose, pong.kind),
        ("Pong (USA).a26", true, ItemKind::Repack)
    );
    let converts = pong.converts.as_ref().unwrap();
    assert_eq!(
        (converts.reason, converts.name.as_str()),
        (KeepReason::Replacement, "Pong (USA).zip")
    );
    // A game with more than one ROM stays TorrentZip, and the plan says why.
    let breakout = item(&plan, "Breakout (USA)");
    assert_eq!(
        (breakout.target.as_str(), breakout.loose, breakout.kind),
        ("Breakout (USA).zip", false, ItemKind::Unchanged)
    );
    assert_eq!(
        plan.content.notes,
        [format!("1 game is not stored loose: {MULTI_ROM_STAYS_ZIP}")]
    );
    assert!(
        plan.content.removals.is_empty(),
        "{:?}",
        plan.content.removals
    );

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(
        names(&dir),
        BTreeSet::from(
            [
                ".title-select",
                RETAINED,
                "Breakout (USA).zip",
                "Combat (USA).a26",
                "Pong (USA).a26"
            ]
            .map(String::from)
        )
    );
    assert_eq!(std::fs::read(dir.join("Pong (USA).a26")).unwrap(), b"pong");
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("2").join("Pong (USA).zip.kept")).unwrap(),
        zipped
    );
    let kept = env.store.kept_files(folder.id).unwrap();
    let kept: BTreeSet<(KeepReason, &str)> =
        kept.iter().map(|k| (k.reason, k.name.as_str())).collect();
    assert_eq!(
        kept,
        BTreeSet::from([
            (KeepReason::Replacement, "Combat (USA).zip"),
            (KeepReason::Replacement, "Pong (USA).zip")
        ])
    );
    let folder = env.store.library_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete, None);

    // A check finds the loose files as they were built, and planning again changes nothing.
    let checked = check(&env, &folder);
    assert!(checked.drift.is_empty(), "{:?}", checked.drift);
    assert!(
        checked
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged && f.torrentzip && f.selected),
        "{:?}",
        checked.files
    );
    let again = env.plan(&folder, id);
    assert!(
        again
            .content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Unchanged),
        "{:?}",
        again.content.items
    );
    assert!(again.content.removals.is_empty());

    // Back to TorrentZip: the loose files are converted again, and kept in their turn.
    set_play_policy(&env, StorageFormat::TorrentZip, None, false);
    let plan = env.plan(&folder, id);
    assert!(plan.content.notes.is_empty());
    assert_eq!(
        item(&plan, "Pong (USA)").converts.as_ref().unwrap().name,
        "Pong (USA).a26"
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(
        verify(
            &std::fs::File::open(dir.join("Pong (USA).zip")).unwrap(),
            &item(&plan, "Pong (USA)").roms
        )
        .is_ok()
    );
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("3").join("Pong (USA).a26.kept")).unwrap(),
        b"pong"
    );
    assert!(!dir.join("Pong (USA).a26").exists());
}

#[test]
fn a_loose_file_renamed_in_a_play_library_is_found_as_drift() {
    let env = Env::new();
    set_play_policy(&env, StorageFormat::Loose, None, false);
    env.audit();
    let id = titles(&env, &["Pong (USA)"]);
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    assert_eq!(item(&plan, "Pong (USA)").target, "Pong (USA).a26");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let dir = env.play().join("atari2600");
    std::fs::rename(dir.join("Pong (USA).a26"), dir.join("pong.bin")).unwrap();

    let checked = check(&env, &folder);
    assert_eq!(checked.files[0].outcome, FileOutcome::Missing);
    assert_eq!(
        checked
            .drift
            .iter()
            .map(|d| (d.name.as_str(), d.kind, d.game.as_deref()))
            .collect::<Vec<_>>(),
        [("pong.bin", DriftKind::Renamed, Some("Pong (USA)"))]
    );
}

#[test]
fn a_systems_own_play_library_format_beats_the_default() {
    let env = Env::new();
    set_play_policy(
        &env,
        StorageFormat::Loose,
        Some(StorageFormat::TorrentZip),
        false,
    );
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    assert!(
        plan.content
            .items
            .iter()
            .all(|i| !i.loose && i.target.ends_with(".zip")),
        "{:?}",
        plan.content.items
    );
    assert!(plan.content.notes.is_empty());
    assert_eq!(plan.content.storage.format, StorageFormat::TorrentZip);
}

#[test]
fn a_plan_made_before_the_play_library_policy_changed_is_refused() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    let plan = env.plan(&folder, id);
    set_play_policy(&env, StorageFormat::Loose, None, false);
    let refused = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");

    // Approved first, then changed before its build runs: the build writes nothing.
    set_play_policy(&env, StorageFormat::TorrentZip, None, false);
    let plan = env.plan(&folder, id);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    set_play_policy(&env, StorageFormat::Loose, None, false);
    env.store.claim_job().unwrap().unwrap();
    let done = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("the plan changed"));
    assert!(env.play().join("atari2600/Pong (USA).zip").exists());
}

#[test]
fn with_delete_originals_on_a_converted_original_is_deleted_once_its_new_file_verifies() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    let dir = env.play().join("atari2600");
    set_play_policy(&env, StorageFormat::Loose, None, true);
    let plan = env.plan(&folder, id);
    assert!(plan.content.storage.delete_originals);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    assert!(env.store.kept_files(folder.id).unwrap().is_empty());
    assert_eq!(
        names(&dir.join(RETAINED).join("2")),
        BTreeSet::new(),
        "every original is deleted"
    );
    assert_eq!(std::fs::read(dir.join("Pong (USA).a26")).unwrap(), b"pong");
    assert_eq!(
        std::fs::read(dir.join("Combat (USA).a26")).unwrap(),
        b"combat"
    );
}

#[test]
fn an_original_that_changed_once_it_was_kept_is_never_deleted() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    let dir = env.play().join("atari2600");
    set_play_policy(&env, StorageFormat::Loose, None, true);
    let plan = env.plan(&folder, id);
    let kept = dir.join(RETAINED).join("2").join("Pong (USA).zip.kept");
    let original = std::fs::read(dir.join("Pong (USA).zip")).unwrap();
    // Rewritten with the same bytes and modification time: only its ctime shows it was written to.
    let path = kept.clone();
    let job = build_hooked(&env, &plan, move |fault| {
        if matches!(fault, Fault::ConversionKept(_)) && path.exists() {
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    });
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(std::fs::read(&kept).unwrap(), original);
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(
        kept.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["Pong (USA).zip"]
    );
}

#[test]
fn an_original_whose_converted_file_does_not_verify_is_kept() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    let dir = env.play().join("atari2600");
    set_play_policy(&env, StorageFormat::Loose, None, true);
    let plan = env.plan(&folder, id);
    let target = dir.join("Pong (USA).a26");
    let job = build_hooked(&env, &plan, move |fault| {
        if matches!(fault, Fault::ConversionKept(_)) && target.exists() {
            std::fs::write(&target, b"pung").unwrap();
        }
    });
    assert_eq!(job.state, JobState::Succeeded);
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(
        kept.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["Pong (USA).zip"]
    );
    assert!(
        dir.join(RETAINED)
            .join("2")
            .join("Pong (USA).zip.kept")
            .exists()
    );
}

#[test]
fn each_conversion_disposes_of_its_original_before_the_next_is_written() {
    for delete in [false, true] {
        let env = Env::new();
        let (folder, id) = built_as_zips(&env);
        let dir = env.play().join("atari2600");
        set_play_policy(&env, StorageFormat::Loose, None, delete);
        let plan = env.plan(&folder, id);
        let converted = plan.content.items.iter().filter(|i| i.converts.is_some());
        assert_eq!(converted.count(), 2);
        // Never both of one game's files at once: the folder needs room for one more file, not a second library.
        let both = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = both.clone();
        let at = dir.clone();
        let job = build_hooked(&env, &plan, move |fault| {
            if let Fault::BeforePublish(seq) = fault {
                for game in ["Pong (USA)", "Combat (USA)"] {
                    let zip = at.join(format!("{game}.zip")).exists();
                    let loose = at.join(format!("{game}.a26")).exists();
                    if zip && loose {
                        seen.lock().unwrap().push((seq, game));
                    }
                }
            }
        });
        assert_eq!(job.state, JobState::Succeeded);
        assert_eq!(*both.lock().unwrap(), [], "delete {delete}");
        let kept = env.store.kept_files(folder.id).unwrap();
        assert_eq!(kept.len(), if delete { 0 } else { 2 }, "{kept:?}");
    }
}

#[test]
fn an_original_left_by_a_conversion_the_service_stopped_after_publishing_is_disposed_of_by_the_next_build()
 {
    for delete in [false, true] {
        let env = Env::new();
        env.audit();
        let id = titles(&env, &["Pong (USA)"]);
        let folder = env.folder("atari2600");
        assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
        set_play_policy(&env, StorageFormat::Loose, None, delete);
        let plan = env.plan(&folder, id);
        crash_at(&env, &plan, |f| matches!(f, Fault::AfterPublish(_)));
        let (env, _) = env.restart();
        let dir = env.play().join("atari2600");
        assert!(dir.join("Pong (USA).zip").exists() && dir.join("Pong (USA).a26").exists());

        let next = env.plan(&folder, id);
        assert!(next.approvable(), "{:?}", next.content.blockers);
        assert_eq!(kinds(&next)["Pong (USA)"], ItemKind::Unchanged);
        let removal = &next.content.removals[0];
        assert_eq!(
            (
                removal.target.as_str(),
                removal.kind,
                removal.converts_to.as_deref()
            ),
            ("Pong (USA).zip", RemovalKind::Keep, Some("Pong (USA).a26")),
            "{removal:?}"
        );
        assert_eq!(env.build(&next).state, JobState::Succeeded);

        assert!(!dir.join("Pong (USA).zip").exists(), "delete {delete}");
        assert_eq!(std::fs::read(dir.join("Pong (USA).a26")).unwrap(), b"pong");
        let kept = env.store.kept_files(folder.id).unwrap();
        assert_eq!(kept.len(), if delete { 0 } else { 1 }, "{kept:?}");
        let built = env.store.built_files(folder.id).unwrap();
        assert_eq!(
            built.iter().map(|f| f.target.as_str()).collect::<Vec<_>>(),
            ["Pong (USA).a26"]
        );
    }
}

#[test]
fn a_build_you_cancel_stops_between_two_files_and_a_new_plan_finishes_the_folder() {
    // A conversion, and an ordinary build of new files.
    for convert in [true, false] {
        let env = Env::new();
        let (folder, id) = if convert {
            let built = built_as_zips(&env);
            set_play_policy(&env, StorageFormat::Loose, None, false);
            built
        } else {
            env.audit();
            let id = titles(&env, &["Pong (USA)", "Combat (USA)", "Breakout (USA)"]);
            (env.folder("atari2600"), id)
        };
        let dir = env.play().join("atari2600");
        let plan = env.plan(&folder, id);
        let writes = plan.content.items.iter().filter(|i| i.kind.writes());
        assert!(writes.count() >= 2, "{convert}");
        let job = env
            .store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.claim_job().unwrap().unwrap();
        // Cancelled, as you would from the workbench, once the first file is in place.
        let (reached, wait) = std::sync::mpsc::channel();
        let (resume, go) = std::sync::mpsc::channel::<()>();
        let go = std::sync::Mutex::new(go);
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::AfterPublish(0) {
                    reached.send(()).unwrap();
                    go.lock().unwrap().recv().unwrap();
                }
            })));
        let ended = std::thread::scope(|scope| {
            let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
            wait.recv().unwrap();
            let cancelled = env.store.cancel_job(job.id);
            resume.send(()).unwrap();
            cancelled.unwrap();
            running.join().unwrap().unwrap()
        });
        env.store.hook_faults(None);

        assert_eq!(ended.state, JobState::Interrupted, "{convert}");
        assert!(
            ended.error.as_deref().unwrap().contains("you cancelled"),
            "{ended:?}"
        );
        let first = &plan.content.items.iter().find(|i| i.kind.writes()).unwrap();
        let built = env.store.built_files(folder.id).unwrap();
        assert!(
            built.iter().any(|f| f.target == first.target),
            "{convert}: {built:?}"
        );
        assert!(dir.join(&first.target).exists(), "{convert}");
        assert!(
            !dir.join(title_select_api::build::STAGING).exists(),
            "{convert}"
        );
        if convert {
            // The first conversion finished: its original is kept aside, and the second's is in place.
            let kept = env.store.kept_files(folder.id).unwrap();
            assert_eq!(kept.len(), 1, "{kept:?}");
        }
        let after = env.store.library_folder(&env.config, folder.id).unwrap();
        assert_eq!(
            after.generation,
            folder.generation + u64::from(convert),
            "{convert}"
        );
        let unfinished = after.unfinished.unwrap();
        assert_eq!(unfinished.applied, 1 + u64::from(convert), "{convert}");
        assert!(unfinished.not_written >= 1, "{convert}");
        let detail = env.store.build(unfinished.build).unwrap();
        assert!(
            detail
                .items
                .iter()
                .filter(|i| i.state == ItemState::Pending)
                .all(|i| i
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("you cancelled"))),
            "{detail:?}"
        );

        let next = env.plan(&folder, id);
        assert!(next.approvable(), "{convert}: {:?}", next.content.blockers);
        assert_eq!(env.build(&next).state, JobState::Succeeded, "{convert}");
        assert!(
            env.plan(&folder, id)
                .content
                .items
                .iter()
                .all(|i| i.kind == ItemKind::Unchanged)
        );
    }
}

/// Approves `plan` and runs its Build Job until the first fault `stop` picks stops it as a kill would.
#[test]
fn a_surviving_original_that_changed_since_its_conversion_read_it_is_kept_never_deleted() {
    // Touched, so a check refreshes its record; rezipped from the same ROM, so a check records it repacked.
    for change in ["touched", "repacked"] {
        let env = Env::new();
        env.audit();
        let id = titles(&env, &["Pong (USA)"]);
        let folder = env.folder("atari2600");
        assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
        set_play_policy(&env, StorageFormat::Loose, None, true);
        let plan = env.plan(&folder, id);
        crash_at(&env, &plan, |f| matches!(f, Fault::AfterPublish(_)));
        let (env, _) = env.restart();
        let dir = env.play().join("atari2600");
        let zip = dir.join("Pong (USA).zip");
        if change == "touched" {
            std::fs::File::options()
                .write(true)
                .open(&zip)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(3600))
                .unwrap();
        } else {
            write_stored_zip(&zip, &[("Pong (USA).a26", b"pong")]);
        }
        let checked = check(&env, &folder);
        let outcome = checked
            .files
            .iter()
            .find(|f| f.target == "Pong (USA).zip")
            .map(|f| f.outcome);
        assert!(
            matches!(
                outcome,
                Some(FileOutcome::Refreshed | FileOutcome::Repacked)
            ),
            "{change}: {outcome:?}"
        );

        let next = env.plan(&folder, id);
        assert!(next.approvable(), "{:?}", next.content.blockers);
        let removal = &next.content.removals[0];
        assert_eq!(
            (removal.target.as_str(), removal.kind),
            ("Pong (USA).zip", RemovalKind::Keep),
            "{change}"
        );
        assert_eq!(env.build(&next).state, JobState::Succeeded);

        assert!(!zip.exists(), "{change}");
        let kept = env.store.kept_files(folder.id).unwrap();
        assert_eq!(
            kept.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
            ["Pong (USA).zip"],
            "{change}: it changed since the conversion read it, so it is never deleted"
        );
    }
}

#[test]
fn a_game_whose_roms_changed_is_replaced_not_converted_and_its_original_is_never_deleted() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    let old = std::fs::read(dir.join("Pong (USA).zip")).unwrap();
    set_play_policy(&env, StorageFormat::Loose, None, true);

    // The newer revision lists another ROM for Pong, under the same name, which the source set now has.
    let mut newer = games();
    newer
        .iter_mut()
        .find(|(game, _)| *game == "Pong (USA)")
        .unwrap()
        .1 = vec![rom("Pong (USA).a26", b"pong, revised")];
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    let (revision, _) = env.store.import_dat(&dat, "No-Intro", None).unwrap();
    write_file(&env.set_dir().join("pong-revised.bin"), b"pong, revised");
    env.store
        .audit_source_set(
            &env.config,
            SET,
            Some(&revision.sha256),
            title_select_engine::AuditEvidence::Incremental,
        )
        .unwrap();
    let id = env.record(&selection(
        &revision.sha256,
        vec![group("Pong (USA)"), group("Combat (USA)")],
    ));
    let plan = env.plan(&folder, id);
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let pong = item(&plan, "Pong (USA)");
    assert!(pong.kind.writes() && pong.loose, "{pong:?}");
    assert_eq!(pong.converts, None, "another payload is no conversion");
    let removal = plan
        .content
        .removals
        .iter()
        .find(|r| r.target == "Pong (USA).zip")
        .unwrap();
    assert_eq!(
        (removal.kind, removal.converts_to.as_deref()),
        (RemovalKind::Keep, None),
        "{removal:?}"
    );

    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    assert_eq!(
        std::fs::read(dir.join("Pong (USA).a26")).unwrap(),
        b"pong, revised"
    );
    assert!(!dir.join("Pong (USA).zip").exists());
    let kept = env.store.kept_files(folder.id).unwrap();
    let pong = kept.iter().find(|k| k.name == "Pong (USA).zip");
    assert_eq!(
        pong.map(|k| k.reason),
        Some(KeepReason::Replacement),
        "{kept:?}"
    );
    let pong = pong.unwrap();
    assert_eq!(
        std::fs::read(
            dir.join(RETAINED)
                .join(pong.generation.to_string())
                .join(&pong.kept_name)
        )
        .unwrap(),
        old
    );
}

#[test]
fn surviving_originals_are_disposed_of_before_the_next_conversion_is_written() {
    let env = Env::new();
    let (folder, id) = built_as_zips(&env);
    set_play_policy(&env, StorageFormat::Loose, None, false);
    let plan = env.plan(&folder, id);
    crash_at(&env, &plan, |f| matches!(f, Fault::AfterPublish(0)));
    let (env, _) = env.restart();
    let dir = env.play().join("atari2600");
    let survivors: Vec<String> = ["Pong (USA)", "Combat (USA)", "Breakout (USA)"]
        .into_iter()
        .filter(|g| dir.join(format!("{g}.zip")).exists() && dir.join(format!("{g}.a26")).exists())
        .map(|g| format!("{g}.zip"))
        .collect();
    assert_eq!(survivors.len(), 1, "{:?}", names(&dir));

    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert!(next.content.items.iter().any(|i| i.converts.is_some()));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (at, survivor) = (seen.clone(), dir.join(&survivors[0]));
    let job = build_hooked(&env, &next, move |fault| {
        if matches!(fault, Fault::BeforePublish(_)) {
            at.lock().unwrap().push(survivor.exists());
        }
    });

    assert_eq!(job.state, JobState::Succeeded);
    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|there| !there),
        "{} was still in place while a new conversion was written: {seen:?}",
        survivors[0]
    );
    let kept = env.store.kept_files(folder.id).unwrap();
    assert!(kept.iter().any(|k| k.name == survivors[0]), "{kept:?}");
}

fn crash_at(env: &Env, plan: &Plan, stop: impl Fn(Fault) -> bool + Send + Sync + 'static) {
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if stop(fault) {
                panic!("killed at {fault:?}");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
}

#[test]
fn a_conversion_the_service_stopped_in_ends_with_its_original_deleted_or_kept() {
    // Where the stop came, and whether the original is deleted once recovery settles it.
    type Stop = fn(Fault) -> bool;
    let stops: [(Stop, bool); 4] = [
        (|f| matches!(f, Fault::ConversionKept(_)), false),
        (|f| matches!(f, Fault::BeforeDeleteMove(_)), false),
        (|f| matches!(f, Fault::AfterDeleteMove(_)), true),
        (|f| matches!(f, Fault::AfterUnlink(_)), true),
    ];
    for (n, (stop, deleted)) in stops.into_iter().enumerate() {
        let env = Env::new();
        env.audit();
        let id = titles(&env, &["Pong (USA)"]);
        let folder = env.folder("atari2600");
        assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
        set_play_policy(&env, StorageFormat::Loose, None, true);
        let plan = env.plan(&folder, id);
        crash_at(&env, &plan, stop);
        let (env, _) = env.restart();
        let dir = env.play().join("atari2600");
        assert_eq!(
            std::fs::read(dir.join("Pong (USA).a26")).unwrap(),
            b"pong",
            "stop {n}"
        );
        let kept = env.store.kept_files(folder.id).unwrap();
        let generation = dir.join(RETAINED).join("2");
        if deleted {
            assert!(kept.is_empty(), "stop {n}: {kept:?}");
            assert!(names(&generation).is_empty(), "stop {n}");
        } else {
            assert_eq!(kept.len(), 1, "stop {n}");
            assert!(generation.join("Pong (USA).zip.kept").exists(), "stop {n}");
        }
        assert!(
            !dir.join(RETAINED).join(".deleting").exists()
                || names(&dir.join(RETAINED).join(".deleting")).is_empty(),
            "stop {n}"
        );
        // Nothing is left blocking the folder.
        let again = env.plan(&folder, id);
        assert!(again.approvable(), "stop {n}: {:?}", again.content.blockers);
    }
}

#[test]
fn an_original_changed_while_it_waited_to_be_deleted_is_moved_back_by_recovery() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)"]);
    let folder = env.folder("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    set_play_policy(&env, StorageFormat::Loose, None, true);
    let plan = env.plan(&folder, id);
    crash_at(&env, &plan, |f| matches!(f, Fault::AfterDeleteMove(_)));
    let dir = env.play().join("atari2600");
    let deleting = dir.join(RETAINED).join(".deleting");
    let slot = names(&deleting).into_iter().next().unwrap();
    // The same bytes written again: its ctime moves, so nothing shows it is still the original.
    let bytes = std::fs::read(deleting.join(&slot)).unwrap();
    let mtime = std::fs::metadata(deleting.join(&slot))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(deleting.join(&slot), &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(deleting.join(&slot))
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    let (env, _) = env.restart();
    let kept = env.store.kept_files(folder.id).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        std::fs::read(dir.join(RETAINED).join("2").join("Pong (USA).zip.kept")).unwrap(),
        bytes
    );
    assert!(names(&deleting).is_empty());
}

#[test]
fn a_selection_names_the_library_folders_that_plan_from_it_and_lists_by_profile() {
    let env = Env::new();
    env.audit();
    let planned = env.record(&everything(&env.sha256));
    let what_if = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    env.plan(&folder, planned);

    let records = env.store.selections_of(None, Some(PROFILE)).unwrap();

    let used: Vec<(i64, Vec<i64>)> = records
        .iter()
        .map(|r| (r.id, r.library_folders.clone()))
        .collect();
    assert_eq!(used, [(what_if, vec![]), (planned, vec![folder.id])]);
    assert_eq!(env.store.selections(SET).unwrap(), records);
    assert!(env.store.selections_of(None, Some("Nobody's")).is_err());
}

#[test]
fn a_folder_is_stale_once_its_source_set_or_profile_changes_after_its_build_and_a_plan_is_open_until_built()
 {
    use title_select_api::build::Stale;
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let read = || env.store.library_folder(&env.config, folder.id).unwrap();
    assert!(!read().plan_open);
    assert!(read().stale.is_empty(), "never built, so never stale");

    let plan = env.plan(&folder, id);
    assert!(read().plan_open);
    assert!(read().stale.is_empty());
    env.build(&plan);
    assert!(!read().plan_open, "built, so its plan is no longer open");
    assert!(read().stale.is_empty(), "nothing changed since the build");

    // A plan from before the build is not open, even unapproved.
    let replaced = env.plan(&folder, id);
    env.build(&env.plan(&folder, id));
    assert!(env.store.plan(replaced.id).unwrap().approval.is_none());
    assert!(!read().plan_open);

    env.audit();
    assert!(
        read().stale.is_empty(),
        "an audit that found what the build's did changes nothing: {:?}",
        read().stale
    );

    std::fs::remove_file(env.set_dir().join("breakout.bin")).unwrap();
    env.audit();
    let built_audit = read().build.unwrap().audit;
    let latest = match read().stale.as_slice() {
        [Stale::SourceSetChanged { built, latest }] if *built == built_audit => *latest,
        other => panic!("{other:?}"),
    };
    assert!(latest > built_audit);

    let changed = SelectionProfile {
        region_order: vec!["USA".into()],
        ..Default::default()
    };
    env.store
        .save_profile(PROFILE, &changed, Some(1), "Operator", &[], WORKERS)
        .unwrap();
    assert!(
        read().stale.contains(&Stale::ProfileSaved {
            built: 1,
            latest: 2
        }),
        "{:?}",
        read().stale
    );

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
    std::fs::create_dir_all(env.tmp.path().join("roots/sets/Another")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Another", "No-Intro/Test - System")
        .unwrap();
    env.store
        .change_library_folder(
            &env.config,
            folder.id,
            &title_select_api::build::ChangeLibraryFolder {
                source_set: Some("Another".into()),
                profile: Some("Other".into()),
            },
        )
        .unwrap();
    assert_eq!(
        read().stale,
        [
            Stale::SourceSetRebound { built: SET.into() },
            Stale::ProfileRebound {
                built: PROFILE.into()
            },
        ],
        "a rebound folder is stale for its binding, not for the old source set's audits or profile's versions"
    );
}

#[test]
fn a_stat_walk_stops_for_a_build_that_waits_and_never_delays_its_approval() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let walk = env.store.enqueue_stat_walk(SET, "Operator").unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, walk.id);

    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let ended = env.store.run_stat_walk_job(&env.config, walk.id).unwrap();

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);
}
