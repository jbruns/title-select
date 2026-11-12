//! The disc layout of Play Libraries: a disc stored as one file directly in the system folder, a single disc of
//! several files in a folder named for its `.cue`, and every file of a Multi-disc Game in `.multidisc/`, listed in
//! disc order by an `.m3u` named for the release; or every file flat. SBI files go beside their disc's `.cue` or
//! `.chd` when the system opts in.
//!
//! The CHD test finds chdman as the store does; where it is missing it is skipped, except in CI, where it fails.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use title_select_api::JobState;
use title_select_api::build::{
    AddLibraryFolder, CleanupAction, DriftKind, FileOutcome, FolderCheck, ItemKind, LibraryFolder,
    MARKER, Plan, PlanBuild, PlanCleanup, RETENTION, RemovalKind,
};
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, Winner,
};
use title_select_api::storage::{DiscLayout, StorageFormat, SystemStorage};
use title_select_store::config::Config;
use title_select_store::{Store, Workers};

const SET: &str = "Test - System";
const SYSTEM: &str = "Redump/Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};
/// A CD of a cue sheet and two tracks.
const CD: &str = "Orbit (USA)";
/// A DVD of one image.
const DVD: &str = "Atlas (USA)";
/// A game released on two CDs.
const SAGA: [&str; 2] = ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"];
const M3U: &str = "Saga (USA).m3u";

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Disc `n` of Saga: a cue sheet and one data track.
fn saga(n: usize) -> Vec<RomSpec> {
    let game = SAGA[n - 1];
    let cue = format!(
        "FILE \"{game} (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n"
    );
    vec![
        rom(leak(format!("{game}.cue")), cue.as_bytes()),
        rom(
            leak(format!("{game} (Track 1).bin")),
            &noise(10 + n as u64, 2352 * 100),
        ),
    ]
}

fn cd() -> Vec<RomSpec> {
    let cue = format!(
        "FILE \"{CD} (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
         FILE \"{CD} (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n"
    );
    vec![
        rom("Orbit (USA).cue", cue.as_bytes()),
        rom("Orbit (USA) (Track 1).bin", &noise(1, 2352 * 300)),
        rom("Orbit (USA) (Track 2).bin", &noise(2, 2352 * 200)),
    ]
}

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (CD, cd()),
        (DVD, vec![rom("Atlas (USA).iso", &noise(3, 2048 * 400))]),
        (SAGA[0], saga(1)),
        (SAGA[1], saga(2)),
    ]
}

const ALL: [&str; 4] = [CD, DVD, SAGA[0], SAGA[1]];

/// The files of the flat layout, loose.
const FLAT_LOOSE: [&str; 8] = [
    "Atlas (USA).iso",
    "Orbit (USA) (Track 1).bin",
    "Orbit (USA) (Track 2).bin",
    "Orbit (USA).cue",
    "Saga (USA) (Disc 1) (Track 1).bin",
    "Saga (USA) (Disc 1).cue",
    "Saga (USA) (Disc 2) (Track 1).bin",
    "Saga (USA) (Disc 2).cue",
];

/// A game released on two DVDs, each one image.
const DUO: [&str; 2] = ["Duo (USA) (Disc 1)", "Duo (USA) (Disc 2)"];

fn duo() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (
            DUO[0],
            vec![rom("Duo (USA) (Disc 1).iso", &noise(20, 2048 * 50))],
        ),
        (
            DUO[1],
            vec![rom("Duo (USA) (Disc 2).iso", &noise(21, 2048 * 50))],
        ),
    ]
}

/// The files of the ES-DE layout, loose.
const ES_DE_LOOSE: [&str; 9] = [
    ".multidisc/Saga (USA) (Disc 1) (Track 1).bin",
    ".multidisc/Saga (USA) (Disc 1).cue",
    ".multidisc/Saga (USA) (Disc 2) (Track 1).bin",
    ".multidisc/Saga (USA) (Disc 2).cue",
    "Atlas (USA).iso",
    "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
    "Orbit (USA).cue/Orbit (USA) (Track 2).bin",
    "Orbit (USA).cue/Orbit (USA).cue",
    M3U,
];

fn chdman() -> Option<PathBuf> {
    match title_select_engine::chd::locate(None) {
        Ok(path) if path.is_file() => Some(path),
        found if std::env::var_os("CI").is_some() => {
            panic!("CI runs these tests with chdman: {found:?}")
        }
        _ => {
            eprintln!("skipped: chdman isn't here; set TITLE_SELECT_CHDMAN to run this test");
            None
        }
    }
}

/// A store with the Redump disc system's Source Set as loose folders, its games stored in play libraries as
/// `format`, and a writable Root `play`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    sha256: String,
}

impl Env {
    fn new(format: StorageFormat) -> Env {
        Env::with(format, Vec::new)
    }

    /// As [`Env::new`], with `more` games in the DAT and the source set besides; a game whose name no folder could
    /// have is in the DAT only.
    fn with(format: StorageFormat, more: fn() -> Vec<(&'static str, Vec<RomSpec>)>) -> Env {
        let all = || games().into_iter().chain(more()).collect::<Vec<_>>();
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        std::fs::create_dir_all(sets.join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("roots/play")).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n",
            sets.display().to_string(),
            tmp.path().join("roots/play").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(&tmp.path().join("in"), "test.dat", "1", &all());
        let (revision, _) = store.import_dat(&dat, "Redump", None).unwrap();
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
        for (game, roms) in all().into_iter().filter(|(g, _)| !g.contains('/')) {
            write_loose_folder(&sets.join(SET), game, &roms);
        }
        store
            .audit_source_set(&config, SET, None, title_select_engine::AuditEvidence::Full)
            .unwrap();
        let env = Env {
            tmp,
            store,
            config,
            sha256: revision.sha256,
        };
        env.set(SystemStorage {
            format: Some(format),
            ..Default::default()
        });
        env
    }

    /// Stores the system's games in play libraries as `storage` says.
    fn set(&self, storage: SystemStorage) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.play_libraries.systems.insert(SYSTEM.into(), storage);
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("roots/play/discs")
    }

    fn folder(&self) -> LibraryFolder {
        self.store
            .add_library_folder(
                &self.config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("discs".into()),
                    template: None,
                    source_set: SET.into(),
                    profile: PROFILE.into(),
                },
            )
            .unwrap()
    }

    /// A Selection of `titles`.
    fn selection(&self, titles: &[&str]) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        let selection = Selection {
            pins: SelectionPins::default(),
            dat_sha256: self.sha256.clone(),
            system: SET.into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups: titles.iter().map(|t| group(t)).collect(),
            stale_choices: Default::default(),
        };
        self.store
            .record_selection(&self.config, job.id, &selection)
            .unwrap()
            .selection
            .unwrap()
    }

    fn plan(&self, folder: &LibraryFolder, selection: i64) -> Plan {
        self.store
            .plan_build(&self.config, folder.id, selection)
            .unwrap()
    }

    /// Approves and runs `plan`, whatever becomes of it.
    fn run(&self, plan: &Plan) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store
            .run_build_job(&self.config, job.id, None)
            .unwrap()
    }

    /// The items of `job`'s Build.
    fn items(&self, job: &title_select_api::Job) -> Vec<title_select_api::build::BuildItem> {
        let build = self.store.build_of_job(job.id).unwrap().unwrap();
        self.store.build(build.id).unwrap().items
    }

    /// Builds `plan`, cancelling it, and waiting until the cancel is recorded, at the first fault `at` matches.
    fn build_cancelled_at(
        &self,
        plan: &Plan,
        at: impl Fn(title_select_store::Fault) -> bool + Send + Sync + 'static,
    ) -> title_select_api::Job {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Mutex, mpsc};
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        let (reached, reached_rx) = mpsc::channel();
        let (go, go_rx) = mpsc::channel::<()>();
        let (reached, go_rx) = (Mutex::new(reached), Mutex::new(go_rx));
        let fired = AtomicBool::new(false);
        self.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if at(fault) && !fired.swap(true, Ordering::SeqCst) {
                    reached.lock().unwrap().send(()).unwrap();
                    go_rx.lock().unwrap().recv().unwrap();
                }
            })));
        let ended = std::thread::scope(|scope| {
            let running = scope.spawn(|| self.store.run_build_job(&self.config, job.id, None));
            reached_rx
                .recv_timeout(std::time::Duration::from_secs(120))
                .expect("the build reached the fault");
            self.store.cancel_job(job.id).unwrap();
            go.send(()).unwrap();
            running.join().unwrap().unwrap()
        });
        self.store.hook_faults(None);
        ended
    }

    fn build(&self, plan: &Plan) {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        let ended = self
            .store
            .run_build_job(&self.config, job.id, None)
            .unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        let build = self.store.build_of_job(ended.id).unwrap().unwrap();
        for item in self.store.build(build.id).unwrap().items {
            assert!(item.error.is_none(), "{item:?}");
        }
    }

    /// Plans and builds `selection` into `folder`, and returns the plan.
    fn built(&self, folder: &LibraryFolder, selection: i64) -> Plan {
        let plan = self.plan(folder, selection);
        assert!(
            plan.content.blockers.is_empty(),
            "{:?}",
            plan.content.blockers
        );
        self.build(&plan);
        plan
    }

    fn check(&self, folder: &LibraryFolder) -> FolderCheck {
        let job = self.store.enqueue_check(folder.id, "Operator").unwrap();
        self.store.claim_job().unwrap().unwrap();
        let done = self
            .store
            .run_check_job(&self.config, job.id, None)
            .unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
        self.store.latest_check(folder.id).unwrap().unwrap()
    }

    /// Every file in the folder, by its path in it, other than Title Select's own.
    fn files(&self) -> Vec<String> {
        files_in(&self.dir())
    }

    fn kept(&self, folder: &LibraryFolder) -> Vec<String> {
        let mut kept: Vec<String> = self
            .store
            .kept_files(folder.id)
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        kept.sort();
        kept
    }
}

/// Every file in `dir`, by its path in it, other than Title Select's own.
fn files_in(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefix.is_empty() && name.starts_with(MARKER) {
                continue;
            }
            let path = format!("{prefix}{name}");
            match entry.file_type().unwrap().is_dir() {
                true => walk(&entry.path(), &format!("{path}/"), out),
                false => out.push(path),
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

fn group(title: &str) -> SelectionGroup {
    SelectionGroup {
        group: title.to_lowercase(),
        unresolved: false,
        winners: vec![Winner {
            title: title.into(),
            rule: Rule {
                stage: "Parent selection".into(),
                reference: Some("REF0001".into()),
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

/// Each file's outcome in `check`, by its target.
fn outcomes(check: &FolderCheck) -> BTreeMap<String, FileOutcome> {
    check
        .files
        .iter()
        .map(|f| (f.target.clone(), f.outcome))
        .collect()
}

fn read(env: &Env, target: &str) -> String {
    std::fs::read_to_string(env.dir().join(target)).unwrap()
}

/// An SBI pack holding SBI files for Orbit and Saga's Disc 1.
fn sbi_pack() -> Vec<u8> {
    sbi_pack_of(&[
        ("Orbit (USA).sbi", b"SBI\0orbit"),
        ("Saga (USA) (Disc 1).sbi", b"SBI\0saga"),
    ])
}

fn sbi_pack_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let path = tempfile::tempdir().unwrap();
    let zip = path.path().join("sbi.zip");
    write_zip(&zip, members);
    std::fs::read(zip).unwrap()
}

/// The plan's items for `game`.
fn items_of<'p>(plan: &'p Plan, game: &str) -> Vec<&'p title_select_api::build::PlanItem> {
    plan.content
        .items
        .iter()
        .filter(|i| i.game == game)
        .collect()
}

#[test]
fn a_loose_build_lays_discs_out_as_es_de_reads_them_with_an_m3u_listing_each_disc_in_order() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);

    assert_eq!(env.files(), ES_DE_LOOSE);
    assert_eq!(
        read(&env, M3U),
        ".multidisc/Saga (USA) (Disc 1).cue\n.multidisc/Saga (USA) (Disc 2).cue\n"
    );
    // Every file, the m3u too, is the folder's: a check finds each as it was built, and a plan again changes nothing.
    let check = env.check(&folder);
    let found = outcomes(&check);
    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        ES_DE_LOOSE
    );
    assert!(
        found.values().all(|o| *o == FileOutcome::Unchanged),
        "{found:?}"
    );
    assert!(check.drift.is_empty(), "{:?}", check.drift);
    let again = env.plan(&folder, selection);
    assert!(
        again
            .content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Unchanged),
        "{:?}",
        again.content.items
    );
    assert!(
        again.content.removals.is_empty(),
        "{:?}",
        again.content.removals
    );
}

#[test]
fn a_chd_build_lays_each_disc_out_as_one_file_with_an_m3u_listing_its_chds() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);

    assert_eq!(
        env.files(),
        [
            ".multidisc/Saga (USA) (Disc 1).chd",
            ".multidisc/Saga (USA) (Disc 2).chd",
            "Atlas (USA).chd",
            "Orbit (USA).chd",
            M3U,
        ]
    );
    assert_eq!(
        read(&env, M3U),
        ".multidisc/Saga (USA) (Disc 1).chd\n.multidisc/Saga (USA) (Disc 2).chd\n"
    );
    let found = outcomes(&env.check(&folder));
    assert!(
        found.values().all(|o| *o == FileOutcome::Unchanged),
        "{found:?}"
    );
}

#[test]
fn the_flat_layout_puts_every_file_in_the_system_folder_and_writes_no_m3u() {
    let env = Env::new(StorageFormat::LooseFolder);
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);

    assert_eq!(
        env.files(),
        [
            "Atlas (USA).iso",
            "Orbit (USA) (Track 1).bin",
            "Orbit (USA) (Track 2).bin",
            "Orbit (USA).cue",
            "Saga (USA) (Disc 1) (Track 1).bin",
            "Saga (USA) (Disc 1).cue",
            "Saga (USA) (Disc 2) (Track 1).bin",
            "Saga (USA) (Disc 2).cue",
        ]
    );
}

#[test]
fn a_changed_m3u_is_drift_and_is_never_overwritten() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    std::fs::write(
        env.dir().join(M3U),
        ".multidisc/Saga (USA) (Disc 2).cue\n.multidisc/Saga (USA) (Disc 1).cue\n",
    )
    .unwrap();

    let found = outcomes(&env.check(&folder));
    assert_eq!(found[M3U], FileOutcome::Conflict, "{found:?}");
    let plan = env.plan(&folder, selection);
    let m3u = plan.content.items.iter().find(|i| i.target == M3U).unwrap();
    assert_eq!(m3u.kind, ItemKind::Conflict, "{m3u:?}");
}

#[test]
fn a_file_added_to_a_disc_layout_folder_is_found_by_a_check() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    std::fs::write(env.dir().join(".multidisc/stray.txt"), b"stray").unwrap();

    let check = env.check(&folder);
    assert!(
        check.drift.iter().any(|d| d.name == ".multidisc/stray.txt"),
        "{:?}",
        check.drift
    );
}

#[test]
fn a_release_no_longer_selected_has_its_discs_and_m3u_kept_and_its_folders_removed() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    env.built(&folder, env.selection(&ALL));

    let fewer = env.selection(&[DVD]);
    let plan = env.plan(&folder, fewer);
    assert!(
        plan.content.blockers.is_empty(),
        "{:?}",
        plan.content.blockers
    );
    env.build(&plan);

    assert_eq!(env.files(), ["Atlas (USA).iso"]);
    let mut expected: Vec<&str> = ES_DE_LOOSE
        .iter()
        .copied()
        .filter(|f| *f != "Atlas (USA).iso")
        .collect();
    expected.sort();
    assert_eq!(env.kept(&folder), expected);
    assert!(env.dir().join(RETENTION).is_dir());
    assert!(!env.dir().join(".multidisc").exists());
    assert!(!env.dir().join("Orbit (USA).cue").exists());
}

#[test]
fn a_disc_built_as_torrentzip_is_laid_out_once_the_policy_stores_it_loose() {
    let env = Env::new(StorageFormat::TorrentZip);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    assert_eq!(
        env.files(),
        [
            "Atlas (USA).zip",
            "Orbit (USA).zip",
            "Saga (USA) (Disc 1).zip",
            "Saga (USA) (Disc 2).zip",
        ]
    );

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        ..Default::default()
    });
    env.built(&folder, selection);
    assert_eq!(env.files(), ES_DE_LOOSE);
    // Each ZIP is kept aside once the files that replace it are in place.
    assert_eq!(
        env.kept(&folder),
        [
            "Atlas (USA).zip",
            "Orbit (USA).zip",
            "Saga (USA) (Disc 1).zip",
            "Saga (USA) (Disc 2).zip",
        ]
    );
}

#[test]
fn sbi_files_go_beside_their_cue_when_the_system_opts_in() {
    let env = Env::new(StorageFormat::LooseFolder);
    env.store.keep_sbi_pack(&sbi_pack(), SYSTEM).unwrap();
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    assert_eq!(
        env.files(),
        ES_DE_LOOSE,
        "no SBI file until the system opts in"
    );

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        sbi: Some(true),
        ..Default::default()
    });
    env.built(&folder, selection);
    let mut expected: Vec<&str> = ES_DE_LOOSE.to_vec();
    expected.extend([
        ".multidisc/Saga (USA) (Disc 1).sbi",
        "Orbit (USA).cue/Orbit (USA).sbi",
    ]);
    expected.sort();
    assert_eq!(env.files(), expected);
    assert_eq!(read(&env, "Orbit (USA).cue/Orbit (USA).sbi"), "SBI\0orbit");
    assert_eq!(
        read(&env, ".multidisc/Saga (USA) (Disc 1).sbi"),
        "SBI\0saga"
    );
    let found = outcomes(&env.check(&folder));
    assert!(
        found.values().all(|o| *o == FileOutcome::Unchanged),
        "{found:?}"
    );

    // Opting out again keeps them aside.
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        sbi: Some(false),
        ..Default::default()
    });
    env.built(&folder, selection);
    assert_eq!(env.files(), ES_DE_LOOSE);
    assert_eq!(
        env.kept(&folder),
        [
            ".multidisc/Saga (USA) (Disc 1).sbi",
            "Orbit (USA).cue/Orbit (USA).sbi",
        ]
    );
}

#[test]
fn a_build_that_stops_while_putting_a_releases_files_in_place_finishes_them_and_its_m3u_on_recovery()
 {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    let plan = env.plan(&folder, selection);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    // Killed once the first of Saga's files is renamed into place, before the rest of its release is.
    let saga = std::sync::atomic::AtomicBool::new(false);
    let killed = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(-1));
    let killed_at = killed.clone();
    let disc1: Vec<i64> = plan
        .content
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.target.starts_with(".multidisc/"))
        .map(|(seq, _)| seq as i64)
        .collect();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if let title_select_store::Fault::PublishedUnsynced(seq) = fault
                && disc1.contains(&seq)
                && !saga.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                killed_at.store(seq, std::sync::atomic::Ordering::SeqCst);
                panic!("killed at {fault:?}");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    assert!(!env.dir().join(M3U).exists());

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let store = Store::open(&path).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    // Recovery syncs the folder the file was renamed into, `.multidisc/`, before it records it.
    let synced = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = synced.clone();
    store.hook_faults(Some(std::sync::Arc::new(move |fault| {
        if let title_select_store::Fault::ParentSynced(seq) = fault {
            seen.lock().unwrap().push(seq);
        }
    })));
    let blocked = store.recover_builds(&config).unwrap();
    store.hook_faults(None);
    assert!(blocked.is_empty(), "{blocked:?}");
    let killed = killed.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        synced.lock().unwrap().contains(&killed),
        "{killed}: {:?}",
        synced.lock().unwrap()
    );
    let env = Env {
        tmp,
        store,
        config,
        sha256,
    };
    let saga: Vec<&str> = ES_DE_LOOSE
        .iter()
        .copied()
        .filter(|f| f.starts_with(".multidisc/") || *f == M3U)
        .collect();
    let files = env.files();
    for file in &saga {
        assert!(files.iter().any(|f| f == file), "{file}: {files:?}");
    }
    let built = env.store.built_files(folder.id).unwrap();
    for file in &saga {
        assert!(built.iter().any(|b| b.target == *file), "{file}: {built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn switching_a_cue_game_to_the_flat_layout_replaces_its_folder_and_keeps_its_files() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    env.built(&folder, selection);

    assert_eq!(env.files(), FLAT_LOOSE);
    let mut expected: Vec<&str> = ES_DE_LOOSE
        .iter()
        .copied()
        .filter(|f| *f != "Atlas (USA).iso")
        .collect();
    expected.sort();
    assert_eq!(env.kept(&folder), expected);
    let found = outcomes(&env.check(&folder));
    assert!(
        found.values().all(|o| *o == FileOutcome::Unchanged),
        "{found:?}"
    );
}

#[test]
fn switching_a_cue_game_to_the_es_de_layout_moves_it_into_its_folder_and_keeps_its_files() {
    let env = Env::new(StorageFormat::LooseFolder);
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        ..Default::default()
    });
    env.built(&folder, selection);

    assert_eq!(env.files(), ES_DE_LOOSE);
    let expected: Vec<&str> = FLAT_LOOSE
        .iter()
        .copied()
        .filter(|f| *f != "Atlas (USA).iso")
        .collect();
    assert_eq!(env.kept(&folder), expected);
}

#[test]
fn a_release_whose_disc_2_fails_keeps_its_originals_and_publishes_none_of_its_files() {
    let env = Env::new(StorageFormat::TorrentZip);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        ..Default::default()
    });
    let plan = env.plan(&folder, selection);
    // Disc 2's track changes in the source set once the build has written Disc 1's first file, so Disc 2 can't be.
    let track = env
        .tmp
        .path()
        .join("roots/sets")
        .join(SET)
        .join(SAGA[1])
        .join("Saga (USA) (Disc 2) (Track 1).bin");
    let len = std::fs::metadata(&track).unwrap().len() as usize;
    let first = plan
        .content
        .items
        .iter()
        .position(|i| i.game == SAGA[0])
        .unwrap() as i64;
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == title_select_store::Fault::Staged(first) {
                std::fs::write(&track, noise(99, len)).unwrap();
            }
        })));
    let ended = env.run(&plan);
    env.store.hook_faults(None);

    let files = env.files();
    for zip in ["Saga (USA) (Disc 1).zip", "Saga (USA) (Disc 2).zip"] {
        assert!(files.iter().any(|f| f == zip), "{zip}: {files:?}");
    }
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with(".multidisc/") || f == M3U),
        "{files:?}"
    );
    assert!(
        !env.kept(&folder).iter().any(|k| k.starts_with("Saga")),
        "{:?}",
        env.kept(&folder)
    );
    for item in env.items(&ended) {
        if item.target.starts_with("Saga (USA)") || item.target.starts_with(".multidisc/") {
            assert!(item.error.is_some(), "{item:?}");
        }
    }
}

#[test]
fn a_plan_whose_sbi_pack_changed_before_you_run_it_is_stale() {
    let env = Env::new(StorageFormat::LooseFolder);
    env.store.keep_sbi_pack(&sbi_pack(), SYSTEM).unwrap();
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        sbi: Some(true),
        ..Default::default()
    });
    let folder = env.folder();
    let plan = env.plan(&folder, env.selection(&ALL));
    // A newer pack holding the same SBI files for these games, and one more.
    env.store
        .keep_sbi_pack(
            &sbi_pack_of(&[
                ("Orbit (USA).sbi", b"SBI\0orbit"),
                ("Saga (USA) (Disc 1).sbi", b"SBI\0saga"),
                ("Other (USA).sbi", b"SBI\0other"),
            ]),
            SYSTEM,
        )
        .unwrap();

    let approved = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(approved, Err(title_select_store::Error::Conflict(_))),
        "{approved:?}"
    );
}

#[test]
fn a_file_in_the_way_of_one_member_stops_its_whole_release_and_names_it() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    std::fs::write(env.dir().join(M3U), "mine\n").unwrap();
    let plan = env.plan(&folder, env.selection(&ALL));

    for game in SAGA {
        for item in items_of(&plan, game) {
            assert_eq!(item.kind, ItemKind::Blocked, "{item:?}");
            assert!(
                item.reason.as_deref().is_some_and(|r| r.contains(M3U)),
                "{item:?}"
            );
        }
    }
    let run = env.run(&plan);
    let files = env.files();
    assert!(
        !files.iter().any(|f| f.starts_with(".multidisc/")),
        "{files:?}"
    );
    assert_eq!(read(&env, M3U), "mine\n");
    assert!(
        files.iter().any(|f| f == "Orbit (USA).cue/Orbit (USA).cue"),
        "{run:?}"
    );
}

#[test]
fn a_release_of_one_file_discs_cancelled_after_disc_1_puts_neither_disc_in_place() {
    let env = Env::with(StorageFormat::LooseFolder, duo);
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    let folder = env.folder();
    let plan = env.plan(&folder, env.selection(&DUO));
    let disc1 = plan
        .content
        .items
        .iter()
        .position(|i| i.game == DUO[0])
        .unwrap() as i64;
    let ended = env.build_cancelled_at(&plan, move |f| {
        f == title_select_store::Fault::Staged(disc1)
    });

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert!(env.files().is_empty(), "{:?}", env.files());
    assert!(env.store.built_files(folder.id).unwrap().is_empty());
}

fn injected() -> Vec<(&'static str, Vec<RomSpec>)> {
    let cue = |n: u8| {
        let mut roms = saga(1);
        roms[0].name = leak(format!("Game (Disc {n}).cue"));
        roms[1].name = leak(format!("Game (Disc {n}) (Track 1).bin"));
        roms
    };
    vec![
        (
            ".title-select-retained/1/Injected",
            vec![rom("Injected.iso", &noise(30, 2048 * 10))],
        ),
        ("Sub/Game (Disc 1)", cue(1)),
        ("Sub/Game (Disc 2)", cue(2)),
    ]
}

#[test]
fn a_game_whose_name_is_a_path_or_a_private_folder_is_never_laid_out() {
    let env = Env::with(StorageFormat::LooseFolder, injected);
    let folder = env.folder();
    let games = [
        ".title-select-retained/1/Injected",
        "Sub/Game (Disc 1)",
        "Sub/Game (Disc 2)",
    ];
    let plan = env.plan(&folder, env.selection(&games));

    for item in &plan.content.items {
        assert_eq!(item.kind, ItemKind::Blocked, "{item:?}");
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|r| r.contains("the game's name")),
            "{item:?}"
        );
    }
    env.run(&plan);
    assert!(env.files().is_empty(), "{:?}", env.files());
    for game in games {
        let found = items_of(&plan, game);
        assert!(!found.is_empty(), "{game}");
        for item in found {
            assert!(
                item.reason
                    .as_deref()
                    .is_some_and(|r| r.contains("the game's name")),
                "{item:?}"
            );
        }
    }
}

fn alpha() -> Vec<(&'static str, Vec<RomSpec>)> {
    let mut roms = saga(1);
    roms[0].name = "Alpha.cue";
    roms[1].name = "Alpha (Track 1).bin";
    vec![
        ("Alpha (USA)", roms),
        ("Beta (USA)", vec![rom("Alpha.cue", b"FILE \"x\" BINARY\n")]),
    ]
}

#[test]
fn a_file_and_another_games_cue_folder_at_one_name_are_both_refused_before_you_approve() {
    let env = Env::with(StorageFormat::LooseFolder, alpha);
    let folder = env.folder();
    let plan = env.plan(&folder, env.selection(&["Alpha (USA)", "Beta (USA)"]));

    // Neither is built: which one would win is not yours to guess.
    for item in &plan.content.items {
        assert_eq!(item.kind, ItemKind::Blocked, "{item:?}");
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|r| r.contains("Alpha.cue")),
            "{item:?}"
        );
    }
}

fn cafes() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (
            "Caf\u{e9} (USA)",
            vec![rom("Caf\u{e9} (USA).iso", &noise(40, 2048 * 10))],
        ),
        (
            "Cafe\u{301} (Europe)",
            vec![rom("Cafe\u{301} (USA).iso", &noise(41, 2048 * 10))],
        ),
    ]
}

#[test]
fn files_whose_names_differ_only_in_unicode_normalization_collide() {
    let env = Env::with(StorageFormat::LooseFolder, cafes);
    let folder = env.folder();
    let plan = env.plan(
        &folder,
        env.selection(&["Caf\u{e9} (USA)", "Cafe\u{301} (Europe)"]),
    );

    assert_eq!(plan.content.items.len(), 2, "{:?}", plan.content.items);
    for item in &plan.content.items {
        assert_eq!(item.kind, ItemKind::Blocked, "{item:?}");
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|r| r.contains("Unicode form")),
            "{item:?}"
        );
    }
}

#[test]
fn turning_sbi_files_off_under_the_flat_layout_keeps_them_aside() {
    let env = Env::new(StorageFormat::LooseFolder);
    env.store.keep_sbi_pack(&sbi_pack(), SYSTEM).unwrap();
    let flat = |sbi| SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        sbi: Some(sbi),
        ..Default::default()
    };
    env.set(flat(true));
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    let mut expected: Vec<&str> = FLAT_LOOSE.to_vec();
    expected.extend(["Orbit (USA).sbi", "Saga (USA) (Disc 1).sbi"]);
    expected.sort();
    assert_eq!(env.files(), expected);

    env.set(flat(false));
    env.built(&folder, selection);
    assert_eq!(env.files(), FLAT_LOOSE);
    assert_eq!(
        env.kept(&folder),
        ["Orbit (USA).sbi", "Saga (USA) (Disc 1).sbi"]
    );
}

#[test]
fn a_layout_folder_you_made_is_never_removed_even_once_empty() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    std::fs::create_dir(env.dir().join("Orbit (USA).cue")).unwrap();
    env.built(&folder, env.selection(&ALL));
    assert_eq!(env.files(), ES_DE_LOOSE);

    env.built(&folder, env.selection(&[DVD]));
    assert_eq!(env.files(), ["Atlas (USA).iso"]);
    assert!(env.dir().join("Orbit (USA).cue").is_dir());
    assert!(!env.dir().join(".multidisc").exists());
}

/// Orbit's files in the ES-DE layout, and in the flat one.
const ORBIT_ES_DE: [&str; 3] = [
    "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
    "Orbit (USA).cue/Orbit (USA) (Track 2).bin",
    "Orbit (USA).cue/Orbit (USA).cue",
];
const ORBIT_FLAT: [&str; 3] = [
    "Orbit (USA) (Track 1).bin",
    "Orbit (USA) (Track 2).bin",
    "Orbit (USA).cue",
];

/// Builds Orbit in one disc layout, then switches it to the other with a build killed where `stop` says, given the
/// sequence numbers of the removals that clear the way for the new layout, in order. Recovery puts the new layout in
/// place, and a build after it keeps the rest of the old layout aside.
fn switch_stopped(
    to_flat: bool,
    stop: impl Fn(&[i64], title_select_store::Fault) -> bool + Send + Sync + 'static,
) {
    let layout = |flat| SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(if flat {
            DiscLayout::Flat
        } else {
            DiscLayout::EsDe
        }),
        ..Default::default()
    };
    let env = Env::new(StorageFormat::LooseFolder);
    env.set(layout(!to_flat));
    let folder = env.folder();
    let selection = env.selection(&[CD]);
    env.built(&folder, selection);
    assert_eq!(env.files(), if to_flat { ORBIT_ES_DE } else { ORBIT_FLAT });

    env.set(layout(to_flat));
    let plan = env.plan(&folder, selection);
    let written = plan
        .content
        .items
        .iter()
        .filter(|i| i.kind.writes() || i.kind == ItemKind::Adopt)
        .count();
    let clearing: Vec<i64> = plan
        .content
        .removals
        .iter()
        .filter(|r| r.kind != RemovalKind::Leave)
        .enumerate()
        .filter(|(_, r)| match to_flat {
            true => r.target.starts_with("Orbit (USA).cue/"),
            false => r.target == "Orbit (USA).cue",
        })
        .map(|(i, _)| (written + i) as i64)
        .collect();
    assert_eq!(clearing.len(), if to_flat { 3 } else { 1 }, "{plan:?}");
    let env = killed_and_recovered(env, &plan, move |f| stop(&clearing, f));
    let (new, old) = match to_flat {
        true => (ORBIT_FLAT, ORBIT_ES_DE),
        false => (ORBIT_ES_DE, ORBIT_FLAT),
    };
    let files = env.files();
    for file in new {
        assert!(files.iter().any(|f| f == file), "{file}: {files:?}");
    }
    let kept = env.kept(&folder);
    let obstructions: Vec<&str> = old
        .into_iter()
        .filter(|f| f.starts_with("Orbit (USA).cue"))
        .collect();
    for file in &obstructions {
        assert!(kept.iter().any(|k| k == file), "{file}: {kept:?}");
    }

    env.built(&folder, selection);
    assert_eq!(env.files(), new);
    assert_eq!(env.kept(&folder), old);
}

/// Runs `plan`, killing its build where `stop` says, then reopens the store and recovers it, with nothing blocked.
fn killed_and_recovered(
    env: Env,
    plan: &Plan,
    stop: impl Fn(title_select_store::Fault) -> bool + Send + Sync + 'static,
) -> Env {
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

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let store = Store::open(&path).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    let blocked = store.recover_builds(&config).unwrap();
    assert!(blocked.is_empty(), "{blocked:?}");
    Env {
        tmp,
        store,
        config,
        sha256,
    }
}

#[test]
fn a_switch_to_the_es_de_layout_stopped_while_clearing_the_flat_files_finishes_on_recovery() {
    use title_select_store::Fault;
    switch_stopped(false, |c, f| f == Fault::BeforeKeep(c[0]));
    switch_stopped(false, |c, f| f == Fault::Cleared(c[c.len() - 1]));
}

#[test]
fn a_switch_to_the_flat_layout_stopped_while_clearing_the_cue_folder_finishes_on_recovery() {
    use title_select_store::Fault;
    switch_stopped(true, |c, f| f == Fault::BeforeKeep(c[0]));
    switch_stopped(true, |c, f| f == Fault::Cleared(c[c.len() - 1]));
}

#[test]
fn a_switch_back_that_writes_only_the_cue_and_stops_before_it_goes_in_place_finishes_on_recovery() {
    use title_select_store::Fault;
    let flat = |flat| SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(if flat {
            DiscLayout::Flat
        } else {
            DiscLayout::EsDe
        }),
        ..Default::default()
    };
    let env = Env::new(StorageFormat::LooseFolder);
    env.set(flat(true));
    let folder = env.folder();
    let selection = env.selection(&[CD]);
    env.built(&folder, selection);

    // A switch to the ES-DE layout, recovered: the flat tracks are left for the next build.
    env.set(flat(false));
    let plan = env.plan(&folder, selection);
    let env = killed_and_recovered(env, &plan, |f| matches!(f, Fault::Cleared(_)));
    let mut leftover: Vec<&str> = ORBIT_ES_DE.to_vec();
    leftover.extend(["Orbit (USA) (Track 1).bin", "Orbit (USA) (Track 2).bin"]);
    leftover.sort();
    assert_eq!(env.files(), leftover);

    // Back to flat: only the `.cue` is written, once the cue folder is cleared out of its way.
    env.set(flat(true));
    let plan = env.plan(&folder, selection);
    let writes: Vec<(usize, &str)> = plan
        .content
        .items
        .iter()
        .filter(|i| i.kind.writes())
        .map(|i| i.target.as_str())
        .enumerate()
        .collect();
    assert_eq!(writes, [(0, "Orbit (USA).cue")], "{plan:?}");
    let env = killed_and_recovered(env, &plan, |f| f == Fault::BeforePublish(0));
    assert_eq!(env.files(), ORBIT_FLAT);
    let kept = env.kept(&folder);
    for file in ORBIT_ES_DE {
        assert!(kept.iter().any(|k| k == file), "{file}: {kept:?}");
    }
}

#[test]
fn a_layout_switch_that_cant_build_one_disc_moves_none_of_its_release() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    assert_eq!(env.files(), ES_DE_LOOSE);

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    std::fs::write(env.dir().join("Saga (USA) (Disc 2).cue"), b"yours").unwrap();
    let plan = env.plan(&folder, selection);
    env.run(&plan);
    let saga: Vec<String> = env
        .files()
        .into_iter()
        .filter(|f| f.contains("Saga"))
        .collect();
    let mut expected: Vec<&str> = ES_DE_LOOSE
        .iter()
        .copied()
        .filter(|f| f.contains("Saga"))
        .chain(["Saga (USA) (Disc 2).cue"])
        .collect();
    expected.sort();
    assert_eq!(saga, expected);
    assert!(
        env.kept(&folder).iter().all(|k| !k.contains("Saga")),
        "{:?}",
        env.kept(&folder)
    );
}

#[test]
fn switching_to_flat_leaves_a_cue_folder_you_made_and_its_release_as_they_are() {
    let env = Env::new(StorageFormat::LooseFolder);
    let folder = env.folder();
    std::fs::create_dir(env.dir().join("Orbit (USA).cue")).unwrap();
    let selection = env.selection(&ALL);
    env.built(&folder, selection);
    assert_eq!(env.files(), ES_DE_LOOSE);

    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        layout: Some(DiscLayout::Flat),
        ..Default::default()
    });
    let plan = env.plan(&folder, selection);
    let orbit = items_of(&plan, "Orbit (USA)");
    assert!(!orbit.is_empty());
    for item in &orbit {
        assert_eq!(item.kind, ItemKind::Blocked, "{}", item.target);
    }
    assert!(
        orbit.iter().any(|i| i
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("Orbit (USA).cue") && r.contains("you made"))),
        "{orbit:?}"
    );
    env.run(&plan);
    let orbit_files: Vec<String> = env
        .files()
        .into_iter()
        .filter(|f| f.starts_with("Orbit"))
        .collect();
    assert_eq!(
        orbit_files,
        [
            "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
            "Orbit (USA).cue/Orbit (USA) (Track 2).bin",
            "Orbit (USA).cue/Orbit (USA).cue",
        ]
    );
    assert!(env.kept(&folder).iter().all(|k| !k.starts_with("Orbit")));
}

/// A folder you laid out by hand in the ES-DE layout before Title Select built anything in it: `selection`'s discs,
/// as a build into another folder lays them out, copied into it once it is registered. A build into it then writes
/// nothing, every file being in the way, so that a check finds them all as drift.
fn laid_out_by_hand(env: &Env) -> (LibraryFolder, i64) {
    let model = env
        .store
        .add_library_folder(
            &env.config,
            &AddLibraryFolder {
                root: "play".into(),
                path: Some("model".into()),
                template: None,
                source_set: SET.into(),
                profile: PROFILE.into(),
            },
        )
        .unwrap();
    let selection = env.selection(&ALL);
    env.built(&model, selection);
    let from = env.tmp.path().join("roots/play/model");
    let folder = env.folder();
    for f in files_in(&from) {
        let to = env.dir().join(&f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from.join(&f), to).unwrap();
    }
    let plan = env.plan(&folder, selection);
    assert!(
        plan.content
            .items
            .iter()
            .all(|i| matches!(i.kind, ItemKind::Conflict | ItemKind::Blocked)),
        "{:?}",
        plan.content.items
    );
    env.build(&plan);
    (folder, selection)
}

/// Each file in the folder, by its inode: a file adopted is the very file that was there.
fn inodes(env: &Env) -> BTreeMap<String, u64> {
    use std::os::unix::fs::MetadataExt;
    env.files()
        .into_iter()
        .map(|f| {
            let ino = std::fs::metadata(env.dir().join(&f)).unwrap().ino();
            (f, ino)
        })
        .collect()
}

/// Plans a cleanup of `folder` adopting `names`, and runs it.
fn adopt(env: &Env, folder: &LibraryFolder, names: &[String]) -> title_select_api::Job {
    let request = PlanCleanup {
        entries: names
            .iter()
            .map(|n| (n.clone(), CleanupAction::Adopt))
            .collect(),
        ..PlanCleanup::default()
    };
    let plan = env
        .store
        .plan_cleanup(&env.config, folder.id, &request)
        .unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    env.run(&plan)
}

/// Adopts every file a check of `folder` finds, each offered as a selected game's or release's, and checks that a
/// plan then changes nothing, and that once nothing is selected but Atlas, the layout folders adopted go.
fn adopts_all(env: &Env, folder: &LibraryFolder, selection: i64, expected: &[&str]) {
    let before = inodes(env);
    let check = env.check(folder);
    let names: Vec<String> = check.drift.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, expected);
    for d in &check.drift {
        assert_eq!(d.kind, DriftKind::Selected, "{d:?}");
        let game = if d.name == M3U { "Saga (USA)" } else { "" };
        assert!(
            d.game
                .as_deref()
                .is_some_and(|g| game.is_empty() || g == game),
            "{d:?}"
        );
    }

    let ended = adopt(env, folder, &names);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    for item in env.items(&ended) {
        assert!(item.error.is_none(), "{item:?}");
    }
    assert_eq!(inodes(env), before, "nothing was written");
    let check = env.check(folder);
    assert!(check.drift.is_empty(), "{:?}", check.drift);
    let found = outcomes(&check);
    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    assert!(
        found.values().all(|o| *o == FileOutcome::Unchanged),
        "{found:?}"
    );
    let again = env.plan(folder, selection);
    assert!(
        again
            .content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Unchanged),
        "{:?}",
        again.content.items
    );
    assert!(
        again.content.removals.is_empty(),
        "{:?}",
        again.content.removals
    );

    // The folders adopted are recorded as a build's: emptied, they go.
    env.built(folder, env.selection(&[DVD]));
    assert!(!env.dir().join(".multidisc").exists());
    assert!(!env.dir().join("Orbit (USA).cue").exists());
}

#[test]
fn a_cleanup_adopts_a_loose_es_de_tree_you_laid_out_and_a_later_plan_changes_nothing() {
    let env = Env::new(StorageFormat::LooseFolder);
    let (folder, selection) = laid_out_by_hand(&env);
    adopts_all(&env, &folder, selection, &ES_DE_LOOSE);
    assert_eq!(env.files(), ["Atlas (USA).iso"]);
}

const ES_DE_CHD: [&str; 5] = [
    ".multidisc/Saga (USA) (Disc 1).chd",
    ".multidisc/Saga (USA) (Disc 2).chd",
    "Atlas (USA).chd",
    "Orbit (USA).chd",
    M3U,
];

#[test]
fn a_cleanup_adopts_an_es_de_tree_of_chds_you_laid_out_by_extracting_each_once() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    adopts_all(&env, &folder, selection, &ES_DE_CHD);
    assert_eq!(env.files(), ["Atlas (USA).chd"]);
}

#[test]
fn a_chd_that_does_not_hold_its_disc_is_not_adopted_and_stays_drift() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    let orbit = env.dir().join("Orbit (USA).chd");
    // Orbit's tracks, as many sectors each, with one byte of track 1 changed.
    let work = env.tmp.path().join("other-chd");
    std::fs::create_dir_all(&work).unwrap();
    let set = env.tmp.path().join("roots/sets").join(SET).join(CD);
    for track in ["Orbit (USA) (Track 1).bin", "Orbit (USA) (Track 2).bin"] {
        std::fs::copy(set.join(track), work.join(track)).unwrap();
    }
    let track = work.join("Orbit (USA) (Track 1).bin");
    let mut changed = std::fs::read(&track).unwrap();
    changed[1000] ^= 0xff;
    std::fs::write(&track, changed).unwrap();
    std::fs::copy(set.join("Orbit (USA).cue"), work.join("Orbit (USA).cue")).unwrap();
    let made = std::process::Command::new(chdman().unwrap())
        .current_dir(&work)
        .args(["createcd", "-i", "Orbit (USA).cue", "-o", "out.chd"])
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    std::fs::copy(work.join("out.chd"), &orbit).unwrap();
    let bytes = std::fs::read(&orbit).unwrap();

    let ended = adopt(&env, &folder, &["Orbit (USA).chd".to_owned()]);
    let item = env
        .items(&ended)
        .into_iter()
        .find(|i| i.target == "Orbit (USA).chd")
        .unwrap();
    assert!(
        item.error
            .as_deref()
            .is_some_and(|e| e.contains("extracting it did not give back the DAT's files")),
        "{item:?}"
    );
    assert_eq!(std::fs::read(&orbit).unwrap(), bytes);
    let check = env.check(&folder);
    assert!(
        check.drift.iter().any(|d| d.name == "Orbit (USA).chd"),
        "{:?}",
        check.drift
    );
    let plan = env.plan(&folder, selection);
    let orbit = plan
        .content
        .items
        .iter()
        .find(|i| i.target == "Orbit (USA).chd")
        .unwrap();
    assert_eq!(orbit.kind, ItemKind::Conflict, "{orbit:?}");
}

#[test]
fn a_hand_edited_m3u_is_never_adopted_and_never_overwritten() {
    let env = Env::new(StorageFormat::LooseFolder);
    let (folder, selection) = laid_out_by_hand(&env);
    let edited = ".multidisc/Saga (USA) (Disc 2).cue\n.multidisc/Saga (USA) (Disc 1).cue\n";
    std::fs::write(env.dir().join(M3U), edited).unwrap();

    let check = env.check(&folder);
    let m3u = check.drift.iter().find(|d| d.name == M3U).unwrap();
    assert_ne!(m3u.kind, DriftKind::Selected, "{m3u:?}");
    assert!(m3u.conflict, "{m3u:?}");
    let refused = env
        .store
        .plan_cleanup(
            &env.config,
            folder.id,
            &PlanCleanup {
                entries: [(M3U.to_owned(), CleanupAction::Adopt)].into(),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(!refused.approvable());
    assert!(
        refused
            .content
            .blockers
            .iter()
            .any(|b| b.target.as_deref() == Some(M3U)),
        "{:?}",
        refused.content.blockers
    );

    // Its discs adopted, the m3u is still in the way of the one a build would write, and stays as you left it.
    let discs: Vec<String> = check
        .drift
        .iter()
        .filter(|d| d.name != M3U)
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(adopt(&env, &folder, &discs).state, JobState::Succeeded);
    let plan = env.plan(&folder, selection);
    let item = plan.content.items.iter().find(|i| i.target == M3U).unwrap();
    assert_eq!(item.kind, ItemKind::Conflict, "{item:?}");
    assert!(!item.adoptable, "{item:?}");
    env.build(&plan);
    assert_eq!(read(&env, M3U), edited);
    let check = env.check(&folder);
    assert_eq!(
        check
            .drift
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        [M3U]
    );
}

#[test]
fn a_plan_adopts_a_loose_es_de_tree_you_laid_out_with_its_m3u_and_its_folders() {
    let env = Env::new(StorageFormat::LooseFolder);
    let (folder, selection) = laid_out_by_hand(&env);
    let before = inodes(&env);
    let mut request = PlanBuild::new(selection);
    request.choices.adopt = ES_DE_LOOSE.iter().map(|t| t.to_string()).collect();
    let plan = env
        .store
        .plan_build_with(&env.config, folder.id, &request)
        .unwrap();
    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Adopt && i.adoptable),
        "{:?}",
        plan.content.items
    );
    env.build(&plan);

    assert_eq!(inodes(&env), before, "nothing was written");
    let again = env.plan(&folder, selection);
    assert!(
        again
            .content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Unchanged),
        "{:?}",
        again.content.items
    );
    env.built(&folder, env.selection(&[DVD]));
    assert_eq!(env.files(), ["Atlas (USA).iso"]);
    assert!(!env.dir().join(".multidisc").exists());
    assert!(!env.dir().join("Orbit (USA).cue").exists());
}

/// Opts the system into SBI files, from [`sbi_pack`], and lays out the loose tree a build would by hand.
fn laid_out_with_sbi_by_hand(env: &Env) -> (LibraryFolder, i64, Vec<&'static str>) {
    env.store.keep_sbi_pack(&sbi_pack(), SYSTEM).unwrap();
    env.set(SystemStorage {
        format: Some(StorageFormat::LooseFolder),
        sbi: Some(true),
        ..Default::default()
    });
    let (folder, selection) = laid_out_by_hand(env);
    let mut expected: Vec<&str> = ES_DE_LOOSE.to_vec();
    expected.extend([
        ".multidisc/Saga (USA) (Disc 1).sbi",
        "Orbit (USA).cue/Orbit (USA).sbi",
    ]);
    expected.sort();
    assert_eq!(env.files(), expected);
    (folder, selection, expected)
}

#[test]
fn a_cleanup_adopts_the_sbi_files_a_build_would_place_beside_their_discs() {
    let env = Env::new(StorageFormat::LooseFolder);
    let (folder, selection, expected) = laid_out_with_sbi_by_hand(&env);
    adopts_all(&env, &folder, selection, &expected);
    assert_eq!(env.files(), ["Atlas (USA).iso"]);
}

#[test]
fn an_sbi_file_other_than_the_one_a_build_places_is_never_adopted_and_never_overwritten() {
    let env = Env::new(StorageFormat::LooseFolder);
    let (folder, selection, expected) = laid_out_with_sbi_by_hand(&env);
    let sbi = "Orbit (USA).cue/Orbit (USA).sbi";
    std::fs::write(env.dir().join(sbi), b"SBI\0yours").unwrap();

    let check = env.check(&folder);
    let found = check.drift.iter().find(|d| d.name == sbi).unwrap();
    assert_ne!(found.kind, DriftKind::Selected, "{found:?}");
    let refused = env
        .store
        .plan_cleanup(
            &env.config,
            folder.id,
            &PlanCleanup {
                entries: [(sbi.to_owned(), CleanupAction::Adopt)].into(),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(!refused.approvable());

    let others: Vec<String> = expected
        .iter()
        .filter(|f| **f != sbi)
        .map(|f| f.to_string())
        .collect();
    let ended = adopt(&env, &folder, &others);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let plan = env.plan(&folder, selection);
    for item in &plan.content.items {
        match item.target == sbi {
            true => assert!(
                item.kind == ItemKind::Conflict && !item.adoptable,
                "{item:?}"
            ),
            false => assert_eq!(item.kind, ItemKind::Unchanged, "{item:?}"),
        }
    }
    env.build(&plan);
    assert_eq!(read(&env, sbi), "SBI\0yours");
    let check = env.check(&folder);
    let names: Vec<&str> = check.drift.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, [sbi]);
}

/// Puts in place of Orbit's CHD one chdman makes from Orbit's own tracks, as the source set holds them, laid out by
/// `cue` in place of the DAT's cue sheet.
fn orbit_chd_laid_out_as(env: &Env, cue: &str) {
    let work = env.tmp.path().join("orbit-chd");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let set = env.tmp.path().join("roots/sets").join(SET).join(CD);
    for track in ["Orbit (USA) (Track 1).bin", "Orbit (USA) (Track 2).bin"] {
        std::fs::copy(set.join(track), work.join(track)).unwrap();
    }
    std::fs::write(work.join("Orbit (USA).cue"), cue).unwrap();
    let made = std::process::Command::new(chdman().unwrap())
        .current_dir(&work)
        .args(["createcd", "-i", "Orbit (USA).cue", "-o", "out.chd"])
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    std::fs::copy(work.join("out.chd"), env.dir().join("Orbit (USA).chd")).unwrap();
}

/// Adopting Orbit's CHD fails, saying `why`, and leaves it as it was: drift, and a conflict in a plan.
fn orbit_chd_is_refused(env: &Env, folder: &LibraryFolder, selection: i64, why: &str) {
    let orbit = env.dir().join("Orbit (USA).chd");
    let bytes = std::fs::read(&orbit).unwrap();
    let ended = adopt(env, folder, &["Orbit (USA).chd".to_owned()]);
    let item = env
        .items(&ended)
        .into_iter()
        .find(|i| i.target == "Orbit (USA).chd")
        .unwrap();
    assert!(
        item.error.as_deref().is_some_and(|e| e.contains(why)),
        "{item:?}"
    );
    assert_eq!(std::fs::read(&orbit).unwrap(), bytes);
    let check = env.check(folder);
    assert!(
        check.drift.iter().any(|d| d.name == "Orbit (USA).chd"),
        "{:?}",
        check.drift
    );
    let plan = env.plan(folder, selection);
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.target == "Orbit (USA).chd")
        .unwrap();
    assert_eq!(item.kind, ItemKind::Conflict, "{item:?}");
}

#[test]
fn a_chd_of_the_right_tracks_in_another_track_mode_is_not_adopted() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    orbit_chd_laid_out_as(
        &env,
        &format!(
            "FILE \"{CD} (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
             FILE \"{CD} (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n"
        ),
    );
    orbit_chd_is_refused(&env, &folder, selection, "track 1 is MODE2/2352");
}

#[test]
fn a_chd_of_the_right_tracks_with_another_index_is_not_adopted() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    orbit_chd_laid_out_as(
        &env,
        &format!(
            "FILE \"{CD} (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
             FILE \"{CD} (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:01:00\n"
        ),
    );
    orbit_chd_is_refused(&env, &folder, selection, "track 2 starts INDEX 01 at");
}

#[test]
fn a_chd_laid_out_as_the_dats_cue_sheet_in_other_words_is_adopted() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, _) = laid_out_by_hand(&env);
    orbit_chd_laid_out_as(
        &env,
        &format!(
            "FILE \"{CD} (Track 1).bin\" BINARY\r\nTRACK 01 MODE1/2352\r\nINDEX 01 00:00:00\r\n\
             FILE   \"{CD} (Track 2).bin\"   BINARY\n\tTRACK 02 AUDIO\n\tINDEX 00 00:00:00\n\tINDEX 01 00:02:00\n"
        ),
    );
    let ended = adopt(&env, &folder, &["Orbit (USA).chd".to_owned()]);
    for item in env.items(&ended) {
        assert!(item.error.is_none(), "{item:?}");
    }
    let check = env.check(&folder);
    assert!(
        !check.drift.iter().any(|d| d.name == "Orbit (USA).chd"),
        "{:?}",
        check.drift
    );
}

#[test]
fn a_chd_whose_cue_sheet_the_source_set_no_longer_holds_is_not_adopted() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, _) = laid_out_by_hand(&env);
    std::fs::remove_file(
        env.tmp
            .path()
            .join("roots/sets")
            .join(SET)
            .join(CD)
            .join("Orbit (USA).cue"),
    )
    .unwrap();
    let refused = env
        .store
        .plan_cleanup(
            &env.config,
            folder.id,
            &PlanCleanup {
                entries: [("Orbit (USA).chd".to_owned(), CleanupAction::Adopt)].into(),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(
        refused
            .content
            .blockers
            .iter()
            .any(|b| b.message.contains("no source for its cue sheet")),
        "{:?}",
        refused.content.blockers
    );
}

#[test]
fn a_chd_whose_header_says_it_holds_more_than_its_disc_is_refused_before_it_is_copied() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    let orbit = env.dir().join("Orbit (USA).chd");
    let mut bytes = std::fs::read(&orbit).unwrap();
    bytes[32..40].copy_from_slice(&(1u64 << 40).to_be_bytes());
    std::fs::write(&orbit, &bytes).unwrap();
    orbit_chd_is_refused(
        &env,
        &folder,
        selection,
        "its header says it expands to 1099511627776 bytes",
    );
}

#[test]
fn a_chd_whose_track_metadata_says_it_holds_more_than_its_disc_is_refused_before_it_is_extracted() {
    let Some(_) = chdman() else { return };
    let env = Env::new(StorageFormat::Chd);
    let (folder, selection) = laid_out_by_hand(&env);
    let orbit = env.dir().join("Orbit (USA).chd");
    let mut bytes = std::fs::read(&orbit).unwrap();
    // Track 1's FRAMES, inflated in place: the header's logical size is left as it was.
    let at = bytes
        .windows(11)
        .position(|w| w == b"FRAMES:300 ")
        .expect("track 1's metadata");
    bytes[at..at + 11].copy_from_slice(b"FRAMES:900 ");
    std::fs::write(&orbit, &bytes).unwrap();
    orbit_chd_is_refused(&env, &folder, selection, "track 1 has 900 frames");
}
