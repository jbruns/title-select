//! CHD in Play Libraries: a disc Game built to CHD with chdman, verified once by extraction against the DAT, and
//! checked afterwards by its header alone. CHD is never a Source Set's format.
//!
//! The tests that run chdman find it as the store does: `TITLE_SELECT_CHDMAN`, else `chdman` on the `PATH`. Where
//! it is missing they are skipped, except in CI, where they fail.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::common::*;
use title_select_api::JobState;
use title_select_api::build::{
    AddLibraryFolder, FileOutcome, FolderCheck, ItemState, LibraryFolder, Plan,
};
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, Winner,
};
use title_select_api::storage::{StorageFormat, StoragePolicy, SystemStorage};
use title_select_store::config::Config;
use title_select_store::{Store, Workers};

const SET: &str = "Test - System";
const SYSTEM: &str = "Redump/Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};
const CD: &str = "Orbit (USA)";
const DVD: &str = "Atlas (USA)";
/// A disc whose track is not whole sectors: it can't come back whole from a CHD, so it stays TorrentZip.
const ODD: &str = "Ragged (USA)";
/// A CD whose cue sheet names its first track twice and its second never.
const ECHO: &str = "Echo (USA)";
/// A game released on two CDs.
const SAGA: [&str; 2] = ["Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"];

/// Disc `n` of Saga: a cue sheet and one data track.
fn saga(n: usize) -> Vec<RomSpec> {
    let game = SAGA[n - 1];
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
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

/// A CD of a data track and an audio track, each whole 2352-byte sectors, with its cue sheet.
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

fn echo() -> Vec<RomSpec> {
    let track = |n: u8| {
        format!(
            "FILE \"{ECHO} (Track {n}).bin\" BINARY\n  TRACK 0{n} AUDIO\n    INDEX 01 00:00:00\n"
        )
    };
    let cue = [track(1), track(3), track(1)].concat();
    vec![
        rom("Echo (USA).cue", cue.as_bytes()),
        rom("Echo (USA) (Track 1).bin", &noise(5, 2352 * 10)),
        rom("Echo (USA) (Track 2).bin", &noise(6, 2352 * 10)),
        rom("Echo (USA) (Track 3).bin", &noise(7, 2352 * 10)),
    ]
}

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (CD, cd()),
        (DVD, vec![rom("Atlas (USA).iso", &noise(3, 2048 * 400))]),
        (ODD, disc(ODD, 1, 2352 * 4 + 1, 4)),
        (ECHO, echo()),
        (SAGA[0], saga(1)),
        (SAGA[1], saga(2)),
    ]
}

/// chdman, as the store finds it; `None`, outside CI, where it isn't there.
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

/// A store with the Redump disc system's Source Set as loose folders, and a writable Root `play`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    sha256: String,
}

impl Env {
    /// An Env whose configuration names `chdman`, or leaves it to be found.
    fn new(chdman: Option<&Path>) -> Env {
        Env::with(chdman)
    }

    /// As [`Env::new`], with `chd` added to the configuration's `[chd]` table.
    fn with(chdman: Option<&Path>) -> Env {
        Env::setup(chdman, &[])
    }

    /// As [`Env::with`], with the games `zipped` in the source set as ZIPs rather than loose folders.
    fn setup(chdman: Option<&Path>, zipped: &[&str]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        let play = tmp.path().join("roots/play");
        std::fs::create_dir_all(sets.join(SET)).unwrap();
        std::fs::create_dir_all(&play).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = config_with(tmp.path(), chdman);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_from(&tmp.path().join("in"), "test.dat", "1", "Redump", &games());
        let (revision, _) = store.import_dat(&dat, None).unwrap();
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
        for (game, roms) in games() {
            if zipped.contains(&game) {
                let members: Vec<(&str, &[u8])> = roms
                    .iter()
                    .map(|r| (r.name, r.payload.as_slice()))
                    .collect();
                write_zip(&sets.join(SET).join(format!("{game}.zip")), &members);
            } else {
                write_loose_folder(&sets.join(SET), game, &roms);
            }
        }
        store
            .audit_source_set(&config, SET, None, title_select_engine::AuditEvidence::Full)
            .unwrap();
        let mut policy = store.storage_policy().unwrap();
        policy.play_libraries.systems.insert(
            SYSTEM.into(),
            SystemStorage {
                format: Some(StorageFormat::Chd),
                delete_originals: None,
                ..Default::default()
            },
        );
        store.set_storage_policy(&policy, "Operator").unwrap();
        Env {
            tmp,
            store,
            config,
            sha256: revision.sha256,
        }
    }

    /// Stores the system's games in play libraries as `format`, else the default, deleting converted originals if
    /// `delete`.
    fn set_format(&self, format: Option<StorageFormat>, delete: bool) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.play_libraries.systems.insert(
            SYSTEM.into(),
            SystemStorage {
                format,
                delete_originals: Some(delete),
                ..Default::default()
            },
        );
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    /// Runs `plan`'s Build with `hook` called at each fault it passes.
    fn build_hooked(
        &self,
        plan: &Plan,
        hook: impl Fn(title_select_store::Fault) + Send + Sync + 'static,
    ) -> title_select_api::Job {
        self.store.hook_faults(Some(std::sync::Arc::new(hook)));
        let job = self.build(plan);
        self.store.hook_faults(None);
        job
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
                .recv_timeout(Duration::from_secs(120))
                .expect("the build reached the fault");
            self.store.cancel_job(job.id).unwrap();
            go.send(()).unwrap();
            running.join().unwrap().unwrap()
        });
        self.store.hook_faults(None);
        ended
    }

    /// The Build of `job`'s items, by target.
    fn items(&self, job: &title_select_api::Job) -> Vec<title_select_api::build::BuildItem> {
        let build = self.store.build_of_job(job.id).unwrap().unwrap();
        self.store.build(build.id).unwrap().items
    }

    /// Reopens the store as after a crash, and recovers what it left unfinished.
    fn restart(self) -> Env {
        let (env, blocked) = self.reopen();
        assert!(blocked.is_empty(), "{blocked:?}");
        env
    }

    /// Reopens the store as after a crash, recovers what it left unfinished, and says why what is still unfinished is.
    fn reopen(self) -> (Env, Vec<String>) {
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
        let blocked = store.recover_builds(&config).unwrap();
        let env = Env {
            tmp,
            store,
            config,
            sha256,
        };
        (env, blocked.into_iter().map(|(_, why)| why).collect())
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("roots/play/discs")
    }

    /// A Library Folder `discs`, and a Selection of `titles`.
    fn folder(&self, titles: &[&str]) -> (LibraryFolder, i64) {
        let folder = self
            .store
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
            .unwrap();
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
        let id = self
            .store
            .record_selection(job.id, &selection)
            .unwrap()
            .selection
            .unwrap();
        (folder, id)
    }

    fn plan(&self, folder: &LibraryFolder, selection: i64) -> Plan {
        self.store
            .plan_build(&self.config, folder.id, selection)
            .unwrap()
    }

    fn build(&self, plan: &Plan) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store
            .run_build_job(&self.config, job.id, None)
            .unwrap()
    }

    /// Checks `folder` with `config`.
    fn check(&self, config: &Config, folder: &LibraryFolder) -> FolderCheck {
        let job = self.store.enqueue_check(folder.id, "Operator").unwrap();
        self.store.claim_job().unwrap().unwrap();
        let done = self.store.run_check_job(config, job.id, None).unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
        self.store.latest_check(folder.id).unwrap().unwrap()
    }

    /// What the store's scratch directory holds for CHDs.
    fn chd_scratch(&self) -> Vec<String> {
        std::fs::read_dir(self.store.dir().join("tmp"))
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("chd-"))
            .collect()
    }
}

fn config(root: &Path, chdman: Option<&Path>) -> Config {
    config_with(root, chdman)
}

fn config_with(root: &Path, chdman: Option<&Path>) -> Config {
    let mut text = format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n",
        root.join("roots/sets").display().to_string(),
        root.join("roots/play").display().to_string(),
    );
    if let Some(chdman) = chdman {
        text += &format!("\n[chd]\nchdman = {:?}\n", chdman.display().to_string());
    }
    Config::parse(&text).unwrap()
}

fn group(title: &str) -> SelectionGroup {
    SelectionGroup {
        dependencies: Vec::new(),
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

fn outcome(check: &FolderCheck, target: &str) -> (FileOutcome, bool) {
    let file = check.files.iter().find(|f| f.target == target).unwrap();
    (file.outcome, file.hashed)
}

/// A shell script standing in for chdman: `name` in `tests/data/chdman`, linked into `dir`, where it finds and leaves
/// the files it says. Linked, not written: a script written while another thread forks may be held open for writing
/// in the child, and running it then fails as a busy text file.
#[cfg(unix)]
fn fake_chdman(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join("chdman");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/chdman")
        .join(name);
    std::os::unix::fs::symlink(script, &path).unwrap();
    path
}

#[test]
fn a_cd_and_a_dvd_build_to_chd_verify_and_pass_later_checks_by_their_header_alone() {
    let Some(chdman) = chdman() else { return };
    let env = Env::new(None);
    let (folder, selection) = env.folder(&[CD, DVD, ODD]);
    let plan = env.plan(&folder, selection);
    let items: Vec<_> = plan
        .content
        .items
        .iter()
        .map(|i| (i.game.as_str(), i.target.as_str(), i.chd))
        .collect();
    assert!(items.contains(&(CD, "Orbit (USA).chd", true)), "{items:?}");
    assert!(items.contains(&(DVD, "Atlas (USA).chd", true)), "{items:?}");
    assert!(
        items.contains(&(ODD, "Ragged (USA).zip", false)),
        "{items:?}"
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("not stored as CHD")),
        "{:?}",
        plan.content.notes
    );

    let ended = env.build(&plan);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let dir = env.dir();
    for name in ["Orbit (USA).chd", "Atlas (USA).chd", "Ragged (USA).zip"] {
        assert!(dir.join(name).is_file(), "{name}");
    }
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    let built = env.store.built_files(folder.id).unwrap();
    assert_eq!(built.len(), 3, "{built:?}");

    // Later checks never run chdman: this configuration names one that isn't there.
    let without = config(env.tmp.path(), Some(&env.tmp.path().join("no-chdman")));
    let check = env.check(&without, &folder);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd"),
        (FileOutcome::Unchanged, false)
    );

    // Recompressed by another chdman, and so rewritten: the same data, read from its header only.
    let cd = dir.join("Orbit (USA).chd");
    let copy = env.tmp.path().join("copy.chd");
    let status = std::process::Command::new(&chdman)
        .args(["copy", "-i"])
        .arg(&cd)
        .arg("-o")
        .arg(&copy)
        .args(["-c", "zlib"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_ne!(std::fs::read(&copy).unwrap(), std::fs::read(&cd).unwrap());
    std::fs::rename(&copy, &cd).unwrap();
    let check = env.check(&without, &folder);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd"),
        (FileOutcome::Refreshed, false)
    );
    assert!(check.drift.is_empty(), "{:?}", check.drift);
    let check = env.check(&without, &folder);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd"),
        (FileOutcome::Unchanged, false)
    );

    // Its data changed: the header's SHA-1s are no longer the ones verified.
    let mut bytes = std::fs::read(&cd).unwrap();
    bytes[64] ^= 0xff;
    bytes[84] ^= 0xff;
    std::fs::write(&cd, &bytes).unwrap();
    let file = std::fs::File::options().write(true).open(&cd).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let check = env.check(&without, &folder);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd"),
        (FileOutcome::Conflict, false)
    );

    // Nothing to do while every CHD is as built.
    let again = env.plan(&folder, selection);
    let dvd = again.content.items.iter().find(|i| i.game == DVD);
    assert!(dvd.is_none_or(|i| !i.kind.writes()), "{dvd:?}");
}

#[test]
fn a_chd_whose_metadata_changed_over_the_same_data_is_drift() {
    let Some(chdman) = chdman() else { return };
    let env = Env::new(None);
    let (folder, selection) = env.folder(&[CD]);
    assert_eq!(
        env.build(&env.plan(&folder, selection)).state,
        JobState::Succeeded
    );
    let cd = env.dir().join("Orbit (USA).chd");
    let header = |path: &Path| {
        title_select_engine::chd::header(&std::fs::File::open(path).unwrap()).unwrap()
    };
    let before = header(&cd);

    // The same data with another track list. chdman adds metadata only to an uncompressed CHD, which holds no
    // SHA-1s, so compress it again.
    let scratch = tempfile::tempdir().unwrap();
    let edited = scratch.path().join("edited.chd");
    let run = |args: &[&std::ffi::OsStr]| {
        let status = std::process::Command::new(&chdman)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{args:?}");
    };
    run(&[
        "copy".as_ref(),
        "-c".as_ref(),
        "none".as_ref(),
        "-i".as_ref(),
        cd.as_os_str(),
        "-o".as_ref(),
        edited.as_os_str(),
    ]);
    run(&[
        "addmeta".as_ref(),
        "-i".as_ref(),
        edited.as_os_str(),
        "-t".as_ref(),
        "CHT2".as_ref(),
        "-ix".as_ref(),
        "2".as_ref(),
        "-vt".as_ref(),
        "TRACK:3 TYPE:AUDIO SUBTYPE:NONE FRAMES:150".as_ref(),
    ]);
    std::fs::remove_file(&cd).unwrap();
    run(&[
        "copy".as_ref(),
        "-i".as_ref(),
        edited.as_os_str(),
        "-o".as_ref(),
        cd.as_os_str(),
    ]);
    let after = header(&cd);
    assert_eq!(after.raw_sha1, before.raw_sha1);
    assert_ne!(after.sha1, before.sha1);
    let file = std::fs::File::options().write(true).open(&cd).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();

    let check = env.check(&env.config, &folder);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd"),
        (FileOutcome::Conflict, false)
    );
    let detail = check
        .files
        .iter()
        .find(|f| f.target == "Orbit (USA).chd")
        .unwrap()
        .detail
        .as_deref();
    assert!(detail.unwrap().contains("its metadata"), "{detail:?}");
}

#[cfg(unix)]
#[test]
fn a_chd_whose_extraction_does_not_match_the_dat_is_never_put_in_place() {
    let root = tempfile::tempdir().unwrap();
    // It makes a CHD, and gives back tracks of the right sizes but other bytes.
    let fake = fake_chdman(root.path(), "mismatched");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let ended = env.build(&plan);

    // The build finishes; only the disc's item fails.
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert!(!env.dir().join("Orbit (USA).chd").exists());
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    assert!(env.store.built_files(folder.id).unwrap().is_empty());
    let build = env.store.build_of_job(ended.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items.len(), 1, "{detail:?}");
    let item = &detail.items[0];
    assert_eq!(item.state, ItemState::Failed, "{item:?}");
    let error = item.error.as_deref().unwrap();
    assert!(
        error.contains("did not give back the DAT's files"),
        "{error}"
    );
    assert!(error.contains("Orbit (USA) (Track 1).bin"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_build_you_cancel_while_chdman_runs_stops_it_and_leaves_no_work_space() {
    let root = tempfile::tempdir().unwrap();
    let fake = fake_chdman(root.path(), "slow");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let running = root.path().join("running");
    let ended = std::thread::scope(|scope| {
        let (ended, end) = std::sync::mpsc::channel();
        let (env, job) = (&env, &job);
        scope.spawn(move || {
            let _ = ended.send(env.store.run_build_job(&env.config, job.id, None));
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !running.exists() {
            assert!(std::time::Instant::now() < deadline, "chdman never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        env.store.cancel_job(job.id).unwrap();
        // Left to run, chdman would take a minute.
        end.recv_timeout(Duration::from_secs(30))
            .expect("chdman was stopped")
            .unwrap()
    });

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    assert!(!env.dir().join("Orbit (USA).chd").exists());
    let build = env.store.library_folder(&env.config, folder.id).unwrap();
    let detail = env.store.build(build.unfinished.unwrap().build).unwrap();
    assert_eq!(detail.items[0].state, ItemState::Pending, "{detail:?}");
}

#[cfg(unix)]
#[test]
fn a_pace_changed_during_a_build_reaches_its_next_chd_but_not_the_one_compressing() {
    let root = tempfile::tempdir().unwrap();
    let (log, held) = (root.path().join("args"), root.path().join("held"));
    // Each run records its arguments; the first waits until the pace has changed, then each fails, which is enough.
    std::fs::write(&held, "").unwrap();
    let fake = fake_chdman(root.path(), "records");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[CD, DVD]);
    let plan = env.plan(&folder, selection);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let runs = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    std::thread::scope(|scope| {
        let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while runs().is_empty() {
            assert!(std::time::Instant::now() < deadline, "chdman never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        let unthrottled = title_select_api::settings::SettingsUpdate {
            pace: Some(title_select_api::settings::Pace::Unthrottled),
            ..Default::default()
        };
        env.store
            .update_settings(&env.config, &unthrottled, "Operator")
            .unwrap();
        std::fs::remove_file(&held).unwrap();
        running.join().unwrap().unwrap();
    });

    let runs = runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs[0].contains("--numprocessors"),
        "the first CHD keeps the default pace's share: {runs:?}"
    );
    assert!(
        !runs[1].contains("--numprocessors"),
        "the next CHD is left every core, unthrottled: {runs:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_pace_changed_while_a_chds_inputs_are_prepared_reaches_the_chdman_it_then_starts() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("args");
    let fake = fake_chdman(root.path(), "records");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    // Its inputs are paused as they're read, until the pace has changed.
    let (reached, go) = (
        std::sync::Arc::new(AtomicBool::new(false)),
        std::sync::Arc::new(AtomicBool::new(false)),
    );
    let (at, until) = (reached.clone(), go.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if let title_select_store::Fault::ChdSourceRead(_) = fault {
                at.store(true, Ordering::SeqCst);
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                while !until.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        })));
    std::thread::scope(|scope| {
        let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !reached.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "its inputs were never read"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let unthrottled = title_select_api::settings::SettingsUpdate {
            pace: Some(title_select_api::settings::Pace::Unthrottled),
            ..Default::default()
        };
        env.store
            .update_settings(&env.config, &unthrottled, "Operator")
            .unwrap();
        go.store(true, Ordering::SeqCst);
        running.join().unwrap().unwrap();
    });
    env.store.hook_faults(None);

    let runs = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(runs.lines().count(), 1, "{runs}");
    assert!(
        !runs.contains("--numprocessors"),
        "chdman started after the pace became unthrottled is left every core: {runs}"
    );
}

#[test]
fn a_disc_zipped_in_its_source_set_is_read_once_for_all_its_tracks() {
    let Some(_) = chdman() else { return };
    let env = Env::setup(None, &[CD]);
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = reads.clone();
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if let title_select_store::Fault::ChdSourceRead(_) = at {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    })));
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let item = &plan.content.items[0];
    assert!(
        item.chd && item.sources.iter().all(|s| s.member.is_some()),
        "{item:?}"
    );
    let ended = env.build(&plan);

    assert_eq!(only_item(&env, &ended).state, ItemState::Applied);
    assert!(env.dir().join("Orbit (USA).chd").is_file());
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_chd_damaged_once_in_place_never_lets_its_original_be_deleted() {
    let Some(_) = chdman() else { return };
    /// How the CHD is damaged in place, once it is put in place and before its original is disposed of.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Damage {
        None,
        /// Cut to half its length.
        Truncated,
        /// Bytes past its header overwritten, at the same length, and its modification time put back.
        Overwritten,
    }
    for damage in [Damage::None, Damage::Truncated, Damage::Overwritten] {
        let damaged = damage != Damage::None;
        let env = Env::new(None);
        env.set_format(None, false);
        let (folder, selection) = env.folder(&[CD]);
        assert_eq!(
            env.build(&env.plan(&folder, selection)).state,
            JobState::Succeeded
        );
        assert!(env.dir().join("Orbit (USA).zip").is_file());

        env.set_format(Some(StorageFormat::Chd), true);
        let plan = env.plan(&folder, selection);
        assert!(
            plan.content
                .items
                .iter()
                .any(|i| i.chd && i.converts.is_some()),
            "{:?}",
            plan.content.items
        );
        // Cut short once it is in place, before its original is disposed of; its header still reads as it did.
        let cd = env.dir().join("Orbit (USA).chd");
        let path = cd.clone();
        let ended = env.build_hooked(&plan, move |fault| {
            if damaged
                && matches!(fault, title_select_store::Fault::ConversionKept(_))
                && path.exists()
            {
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                std::fs::set_permissions(&path, perms).unwrap();
                let mut file = std::fs::File::options().write(true).open(&path).unwrap();
                let meta = file.metadata().unwrap();
                let len = meta.len();
                match damage {
                    Damage::Truncated => file.set_len(len / 2).unwrap(),
                    _ => {
                        use std::io::{Seek, SeekFrom, Write};
                        file.seek(SeekFrom::Start(len / 2)).unwrap();
                        file.write_all(&[0x5a; 64]).unwrap();
                        file.sync_all().unwrap();
                        file.set_modified(meta.modified().unwrap()).unwrap();
                        let after = file.metadata().unwrap();
                        assert_eq!(
                            (after.len(), after.modified().unwrap()),
                            (len, meta.modified().unwrap())
                        );
                    }
                }
            }
        });

        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        assert!(cd.is_file());
        if !damaged {
            assert!(env.kept(&folder).is_empty(), "{:?}", env.kept(&folder));
            continue;
        }
        assert_eq!(env.kept(&folder), ["Orbit (USA).zip"], "{damage:?}");
        let build = env.store.build_of_job(ended.id).unwrap().unwrap();
        let detail = env.store.build(build.id).unwrap();
        let said: Vec<_> = detail
            .items
            .iter()
            .filter_map(|i| i.error.as_deref())
            .collect();
        assert!(
            said.iter().any(|e| e.contains("stays in the kept files")
                && e.contains("not the file that was checked")),
            "{damage:?}: {said:?}"
        );
    }
}

/// A chdman that runs `real`, linked beside it in `dir`, except as `name` in `tests/data/chdman` says for Disc 2 of
/// Saga.
#[cfg(unix)]
fn saga_chdman(dir: &Path, real: &Path, name: &str) -> PathBuf {
    std::os::unix::fs::symlink(real, dir.join("real")).unwrap();
    fake_chdman(dir, name)
}

/// An Env with both of Saga's discs built as TorrentZips, and then stored as CHD: the folder, the selection, and the
/// plan converting them.
fn saga_converting(env: &Env) -> (LibraryFolder, i64, Plan) {
    saga_converting_over(env, false)
}

/// As [`saga_converting`], with a CHD no build wrote at each disc's target when `foreign`: the plan keeps each aside.
fn saga_converting_over(env: &Env, foreign: bool) -> (LibraryFolder, i64, Plan) {
    env.set_format(None, false);
    let (folder, selection) = env.folder(&SAGA);
    assert_eq!(
        env.build(&env.plan(&folder, selection)).state,
        JobState::Succeeded
    );
    if foreign {
        std::fs::create_dir_all(env.dir().join("Saga (USA).m3u")).unwrap();
        for (n, chd) in SAGA_CHDS.iter().enumerate() {
            std::fs::write(env.dir().join(chd), noise(90 + n as u64, 4096)).unwrap();
        }
    }
    env.set_format(Some(StorageFormat::Chd), false);
    let choices = title_select_api::build::PlanChoices {
        resolved_only: false,
        adopt: vec![],
        keep_aside: match foreign {
            true => SAGA_CHDS.iter().map(|c| c.to_string()).collect(),
            false => vec![],
        },
        remove_all: false,
    };
    let plan = env
        .store
        .plan_build_with(
            &env.config,
            folder.id,
            &title_select_api::build::PlanBuild { selection, choices },
        )
        .unwrap();
    let converting = plan
        .content
        .items
        .iter()
        .filter(|i| i.chd && i.converts.is_some())
        .count();
    assert_eq!(converting, 2, "{:?}", plan.content.items);
    let keeping = plan
        .content
        .items
        .iter()
        .filter(|i| i.keep.is_some())
        .count();
    assert_eq!(
        keeping,
        if foreign { 2 } else { 0 },
        "{:?}",
        plan.content.items
    );
    (folder, selection, plan)
}

/// Which formats Saga's discs are in the folder as: in it, or in its release's `.m3u` folder.
fn saga_files(env: &Env) -> Vec<String> {
    let names = |dir: &Path, prefix: &str| -> Vec<String> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("Saga") && !n.ends_with(".m3u"))
            .map(|n| format!("{prefix}{n}"))
            .collect()
    };
    let mut found = names(&env.dir(), "");
    found.extend(names(&env.dir().join("Saga (USA).m3u"), "Saga (USA).m3u/"));
    found.sort();
    found
}

const SAGA_ZIPS: [&str; 2] = ["Saga (USA) (Disc 1).zip", "Saga (USA) (Disc 2).zip"];
/// A game of two discs goes in a folder named for its `.m3u`, which it holds beside them.
const SAGA_CHDS: [&str; 2] = [
    "Saga (USA).m3u/Saga (USA) (Disc 1).chd",
    "Saga (USA).m3u/Saga (USA) (Disc 2).chd",
];

#[cfg(unix)]
#[test]
fn when_one_disc_fails_to_verify_no_disc_of_its_game_changes_format() {
    let Some(real) = chdman() else { return };
    let root = tempfile::tempdir().unwrap();
    // Disc 2's CHD gives back other bytes.
    let fake = saga_chdman(root.path(), &real, "saga-disc-2-differs");
    let env = Env::new(Some(&fake));
    let (folder, _, plan) = saga_converting(&env);
    let ended = env.build(&plan);

    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert_eq!(saga_files(&env), SAGA_ZIPS);
    assert!(env.kept(&folder).is_empty(), "{:?}", env.kept(&folder));
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    let build = env.store.build_of_job(ended.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    for disc in SAGA_CHDS {
        let item = detail.items.iter().find(|i| i.target == disc).unwrap();
        assert_eq!(item.state, ItemState::Failed, "{item:?}");
    }
    let disc1 = detail
        .items
        .iter()
        .find(|i| i.target == SAGA_CHDS[0])
        .unwrap();
    assert!(
        disc1.error.as_deref().unwrap().contains(SAGA_CHDS[1]),
        "{disc1:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_build_you_cancel_while_chdman_makes_disc_2_changes_no_disc_of_its_game() {
    let Some(real) = chdman() else { return };
    let root = tempfile::tempdir().unwrap();
    let slow = root.path().join("slow");
    let fake = saga_chdman(root.path(), &real, "saga-disc-2-slow");
    let env = Env::new(Some(&fake));
    let (folder, _, plan) = saga_converting(&env);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = std::thread::scope(|scope| {
        let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !slow.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "chdman never made disc 2"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        env.store.cancel_job(job.id).unwrap();
        running.join().unwrap().unwrap()
    });

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    assert_eq!(saga_files(&env), SAGA_ZIPS);
    assert!(env.kept(&folder).is_empty(), "{:?}", env.kept(&folder));
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn a_build_that_stops_while_putting_a_games_discs_in_place_finishes_them_on_recovery() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting(&env);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    // Killed once the first disc is renamed into place, before the second is.
    env.store.hook_faults(Some(std::sync::Arc::new(|fault| {
        if let title_select_store::Fault::PublishedUnsynced(_) = fault {
            panic!("killed at {fault:?}");
        }
    })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    let published = saga_files(&env)
        .iter()
        .filter(|n| n.ends_with(".chd"))
        .count();
    assert_eq!(published, 1, "{:?}", saga_files(&env));

    let env = env.restart();
    let found = saga_files(&env);
    for chd in SAGA_CHDS {
        assert!(found.iter().any(|n| n == chd), "{found:?}");
    }
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

/// Counts faults that `is` matches, and is true at the `n`th.
fn nth(
    n: usize,
    is: impl Fn(&title_select_store::Fault) -> bool + Send + Sync,
) -> impl Fn(title_select_store::Fault) -> bool + Send + Sync {
    let seen = std::sync::atomic::AtomicUsize::new(0);
    move |fault| is(&fault) && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == n
}

#[test]
fn a_cancel_while_the_last_disc_is_copied_and_hashed_leaves_its_game_as_it_was() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting(&env);
    let ended = env.build_cancelled_at(
        &plan,
        nth(2, |f| matches!(f, title_select_store::Fault::ChdCopied(_))),
    );

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    assert_eq!(saga_files(&env), SAGA_ZIPS);
    assert!(env.kept(&folder).is_empty(), "{:?}", env.kept(&folder));
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
    for disc in SAGA_CHDS {
        let item = env
            .items(&ended)
            .into_iter()
            .find(|i| i.target == disc)
            .unwrap();
        assert_eq!(item.state, ItemState::Pending, "{item:?}");
    }
}

#[test]
fn a_cancel_once_a_games_discs_are_going_in_place_lets_them_all_finish() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting(&env);
    let ended = env.build_cancelled_at(&plan, |f| {
        matches!(f, title_select_store::Fault::BeforePublish(_))
    });

    assert_ne!(ended.state, JobState::Failed, "{ended:?}");
    let found = saga_files(&env);
    for chd in SAGA_CHDS {
        assert!(found.iter().any(|n| n == chd), "{found:?}");
    }
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
}

/// Asserts the Job recorded, as it ended, all its meter counted: what the build's recovery read and wrote as it
/// failed included.
fn assert_recorded_as_metered(env: &Env, job: &title_select_api::Job) {
    let counted = env.store.meter().so_far(job.id).expect("metered").counted;
    let recorded = job.throughput.expect("its throughput");
    assert_eq!(
        (recorded.bytes_read, recorded.bytes_written),
        (counted.read, counted.written),
        "recorded as it ended, against counted by its meter"
    );
    assert_eq!(
        env.store.job(job.id).unwrap(),
        *job,
        "nothing is recorded after it ends"
    );
}

/// Recovers the store's unfinished Builds, and says why each that is still unfinished is.
fn recover(env: &Env) -> Vec<String> {
    env.store
        .recover_builds(&env.config)
        .unwrap()
        .into_iter()
        .map(|(_, why)| why)
        .collect()
}

#[test]
fn a_disc_that_cannot_be_put_in_place_holds_its_game_until_recovery_finishes_it() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting(&env);
    // Something takes Disc 2's name just before it is renamed into place.
    let intruder = env.dir().join(SAGA_CHDS[1]);
    let path = intruder.clone();
    let second = nth(2, |f| {
        matches!(f, title_select_store::Fault::BeforePublish(_))
    });
    let ended = env.build_hooked(&plan, move |fault| {
        if second(fault) {
            std::fs::write(&path, b"not a CHD").unwrap();
        }
    });

    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    assert!(env.dir().join(SAGA_CHDS[0]).is_file());
    assert_eq!(std::fs::read(&intruder).unwrap(), b"not a CHD");
    // Disc 2 isn't failed, and its verified CHD stays staged, while the build waits to finish the game.
    let disc2 = env
        .items(&ended)
        .into_iter()
        .find(|i| i.target == SAGA_CHDS[1])
        .unwrap();
    assert_eq!(disc2.state, ItemState::Publishing, "{disc2:?}");
    assert!(env.dir().join(title_select_api::build::STAGING).is_dir());
    let blocked = recover(&env);
    assert!(
        blocked.len() == 1 && blocked[0].contains(SAGA_CHDS[1]),
        "{blocked:?}"
    );
    assert!(env.dir().join(title_select_api::build::STAGING).is_dir());

    std::fs::remove_file(&intruder).unwrap();
    assert!(recover(&env).is_empty());
    let found = saga_files(&env);
    for chd in SAGA_CHDS {
        assert!(found.iter().any(|n| n == chd), "{found:?}");
    }
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn a_keep_that_fails_on_disc_2_holds_its_game_until_recovery_finishes_it() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting_over(&env, true);
    // Once Disc 1's file is kept aside, Disc 2's changes, so that it can't be.
    let disc2 = env.dir().join(SAGA_CHDS[1]);
    let path = disc2.clone();
    let first = nth(1, |f| matches!(f, title_select_store::Fault::AfterKeep(_)));
    let ended = env.build_hooked(&plan, move |fault| {
        if first(fault) {
            std::fs::write(&path, b"changed").unwrap();
        }
    });

    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    assert_eq!(env.kept(&folder), [SAGA_CHDS[0]]);
    assert_eq!(std::fs::read(&disc2).unwrap(), b"changed");
    // Recovery, run as the build failed, put Disc 1 in place, and holds the folder at Disc 2, which it can't.
    assert!(env.dir().join(SAGA_CHDS[0]).is_file());
    let blocked = recover(&env);
    assert!(
        blocked.len() == 1 && blocked[0].contains(SAGA_CHDS[1]),
        "{blocked:?}"
    );
    assert!(env.dir().join(title_select_api::build::STAGING).is_dir());

    std::fs::remove_file(&disc2).unwrap();
    assert!(recover(&env).is_empty());
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(env.dir().join(chd).is_file());
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn a_build_that_stops_between_two_keeps_finishes_its_game_on_recovery() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting_over(&env, true);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store.hook_faults(Some(std::sync::Arc::new(|fault| {
        if let title_select_store::Fault::AfterKeep(_) = fault {
            panic!("killed at {fault:?}");
        }
    })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    assert_eq!(saga_files(&env), [SAGA_ZIPS[0], SAGA_ZIPS[1], SAGA_CHDS[1]]);

    let env = env.restart();
    let mut kept = env.kept(&folder);
    kept.retain(|k| k.ends_with(".chd"));
    assert_eq!(kept, SAGA_CHDS);
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(env.dir().join(chd).is_file());
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn a_disc_whose_keep_stopped_unsynced_is_never_forgotten_across_two_recoveries() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting_over(&env, true);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store.hook_faults(Some(std::sync::Arc::new(|fault| {
        if let title_select_store::Fault::KeptUnsynced(_) = fault {
            panic!("killed at {fault:?}");
        }
    })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
    // Disc 1's file was kept aside; something takes its place before the service starts again.
    let obstruction = env.dir().join(SAGA_CHDS[0]);
    assert!(!obstruction.exists());
    std::fs::write(&obstruction, b"in the way").unwrap();

    let (env, blocked) = env.reopen();
    assert!(
        blocked.len() == 1 && blocked[0].contains(SAGA_CHDS[0]),
        "{blocked:?}"
    );
    let disc1 = env
        .items(&env.store.job(job.id).unwrap())
        .into_iter()
        .find(|i| i.target == SAGA_CHDS[0])
        .unwrap();
    assert_eq!(disc1.state, ItemState::Publishing, "{disc1:?}");

    std::fs::remove_file(&obstruction).unwrap();
    assert!(recover(&env).is_empty());
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(env.dir().join(chd).is_file(), "{chd}");
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

#[test]
fn a_copy_of_a_discs_checked_file_at_its_target_is_not_taken_for_it() {
    let Some(_) = chdman() else { return };
    let env = Env::new(None);
    let (folder, _, plan) = saga_converting(&env);
    // Just before Disc 2 is renamed into place, a copy of its checked file, byte for byte, takes its name.
    let dir = env.dir();
    let copy = dir.join(SAGA_CHDS[1]);
    let second = nth(2, |f| {
        matches!(f, title_select_store::Fault::BeforePublish(_))
    });
    let at = copy.clone();
    let ended = env.build_hooked(&plan, move |fault| {
        if second(fault)
            && let title_select_store::Fault::BeforePublish(seq) = fault
        {
            let staged = dir
                .join(title_select_api::build::STAGING)
                .join(format!("{seq}.zip"));
            std::fs::copy(&staged, &at).unwrap();
        }
    });

    assert_eq!(ended.state, JobState::Failed, "{ended:?}");
    assert_recorded_as_metered(&env, &ended);
    let blocked = recover(&env);
    assert!(
        blocked.len() == 1 && blocked[0].contains(SAGA_CHDS[1]),
        "{blocked:?}"
    );
    assert!(env.dir().join(title_select_api::build::STAGING).is_dir());
    let built = env.store.built_files(folder.id).unwrap();
    assert!(!built.iter().any(|b| b.target == SAGA_CHDS[1]), "{built:?}");

    std::fs::remove_file(&copy).unwrap();
    assert!(recover(&env).is_empty());
    let built = env.store.built_files(folder.id).unwrap();
    for chd in SAGA_CHDS {
        assert!(env.dir().join(chd).is_file(), "{chd}");
        assert!(built.iter().any(|b| b.target == chd), "{built:?}");
    }
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

/// The single item of the Build that ended as `job`.
fn only_item(env: &Env, job: &title_select_api::Job) -> title_select_api::build::BuildItem {
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items.len(), 1, "{detail:?}");
    detail.items[0].clone()
}

#[cfg(unix)]
#[test]
fn a_cue_sheet_that_names_a_track_twice_is_refused_before_chdman_runs() {
    let root = tempfile::tempdir().unwrap();
    let args = root.path().join("args");
    let fake = fake_chdman(root.path(), "records");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[ECHO]);
    let ended = env.build(&env.plan(&folder, selection));

    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert!(!args.exists(), "chdman ran");
    let item = only_item(&env, &ended);
    assert_eq!(item.state, ItemState::Failed, "{item:?}");
    let error = item.error.unwrap();
    assert!(
        error.contains("leaves out Echo (USA) (Track 2).bin"),
        "{error}"
    );
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
}

#[cfg(unix)]
#[test]
fn chdman_that_outruns_its_deadline_is_stopped_and_fails_the_disc() {
    let root = tempfile::tempdir().unwrap();
    let fake = fake_chdman(root.path(), "slow");
    let env = Env::with(Some(&fake));
    env.store
        .update_settings(
            &env.config,
            &title_select_api::settings::SettingsUpdate {
                chd: Some(title_select_api::settings::ChdTimeouts {
                    seconds: 1,
                    seconds_per_gib: 0,
                }),
                ..Default::default()
            },
            "Operator",
        )
        .unwrap();
    let (folder, selection) = env.folder(&[CD]);
    let ended = env.build(&env.plan(&folder, selection));

    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let item = only_item(&env, &ended);
    assert_eq!(item.state, ItemState::Failed, "{item:?}");
    assert!(
        item.error
            .as_deref()
            .unwrap()
            .contains("took longer than 1 second,"),
        "{item:?}"
    );
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    assert!(!env.dir().join("Orbit (USA).chd").exists());
    assert!(!env.dir().join(title_select_api::build::STAGING).exists());
}

/// A source set keeps each file exactly as the DAT lists it, so the storage policy never lets one hold a CHD.
#[test]
fn a_chd_is_never_a_source_sets_format_nor_a_play_librarys_for_a_system_without_discs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("store")).unwrap();
    let own = |format| SystemStorage {
        format: Some(format),
        delete_originals: None,
        ..Default::default()
    };
    let refused = |change: &dyn Fn(&mut StoragePolicy)| {
        let mut policy = store.storage_policy().unwrap();
        change(&mut policy);
        store
            .set_storage_policy(&policy, "Operator")
            .unwrap_err()
            .to_string()
    };

    let error = refused(&|p| p.source_sets.format = StorageFormat::Chd);
    assert!(
        error.contains("source sets can't store games as CHD"),
        "{error}"
    );
    let error = refused(&|p| {
        p.source_sets
            .systems
            .insert(SYSTEM.into(), own(StorageFormat::Chd));
    });
    assert!(
        error.contains("source sets can't store games as CHD"),
        "{error}"
    );
    let error = refused(&|p| p.play_libraries.format = StorageFormat::Chd);
    assert!(error.contains("CHD"), "{error}");
    let error = refused(&|p| {
        p.play_libraries
            .systems
            .insert("No-Intro/Test - System".into(), own(StorageFormat::Chd));
    });
    assert!(error.contains("CHD"), "{error}");

    let mut policy = store.storage_policy().unwrap();
    policy
        .play_libraries
        .systems
        .insert(SYSTEM.into(), own(StorageFormat::Chd));
    store.set_storage_policy(&policy, "Operator").unwrap();
}

/// Under single copy, a game the play library stores as CHD is copied, its source set's files left as they are: a CHD
/// isn't the DAT's bytes, so it can't be the game's only copy.
#[test]
fn under_single_copy_a_game_stored_as_chd_is_copied_not_placed() {
    let env = Env::new(None);
    // The source set's folder writable, so nothing but the format keeps the game from being placed.
    let config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.tmp.path().join("roots/play").display().to_string(),
    ))
    .unwrap();
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = true;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let (folder, selection) = env.folder(&[CD]);

    let plan = env.store.plan_build(&config, folder.id, selection).unwrap();
    let item = &plan.content.items[0];
    assert!(item.chd, "{item:?}");
    assert_ne!(item.kind, title_select_api::build::ItemKind::Place);
    assert!(item.place.is_none() && !item.changes_source_set, "{item:?}");
    assert!(
        plan.content.notes.iter().any(|n| n
            .contains("1 game's files are copied rather than placed")
            && n.contains("CHD")),
        "{:?}",
        plan.content.notes
    );
}

/// Turns single copy on for `env`, with its source set's folder writable and the system's games stored in play
/// libraries as loose folders, so that each disc's files can be placed; with `chdman`, when one is given.
fn placing(env: &mut Env, chdman: Option<&Path>) {
    let mut text = format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.tmp.path().join("roots/play").display().to_string(),
    );
    if let Some(chdman) = chdman {
        text += &format!("\n[chd]\nchdman = {:?}\n", chdman.display().to_string());
    }
    env.config = Config::parse(&text).unwrap();
    env.set_format(Some(StorageFormat::LooseFolder), false);
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = true;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

/// Every file under `dir`, relative to it, but for the folder Title Select keeps for itself.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut walk = vec![dir.to_path_buf()];
    while let Some(at) = walk.pop() {
        for entry in std::fs::read_dir(&at).into_iter().flatten() {
            let entry = entry.unwrap();
            if entry.file_name() == ".title-select" {
                continue;
            }
            match entry.file_type().unwrap().is_dir() {
                true => walk.push(entry.path()),
                false => found.push(
                    entry
                        .path()
                        .strip_prefix(dir)
                        .unwrap()
                        .display()
                        .to_string(),
                ),
            }
        }
    }
    found.sort();
    found
}

/// The targets of `plan`'s items that place a file of `game`.
fn placing_targets(plan: &Plan, games: &[&str]) -> Vec<String> {
    let mut targets: Vec<String> = plan
        .content
        .items
        .iter()
        .filter(|i| games.contains(&i.game.as_str()))
        .filter(|i| i.kind == title_select_api::build::ItemKind::Place)
        .map(|i| i.target.clone())
        .collect();
    targets.sort();
    targets
}

/// Under single copy, every file of a disc, and of each disc of a multi-disc release, is placed, and the release's
/// playlist, which the source set doesn't hold, is written fresh beside them.
#[test]
fn under_single_copy_every_file_of_a_disc_and_of_a_multi_disc_release_is_placed() {
    let mut env = Env::new(None);
    placing(&mut env, None);
    let (folder, selection) = env.folder(&[CD, SAGA[0], SAGA[1]]);
    let plan = env.plan(&folder, selection);
    let orbit = placing_targets(&plan, &[CD]);
    let saga = placing_targets(&plan, &SAGA);
    assert_eq!(
        (orbit.len(), saga.len()),
        (3, 4),
        "{:?}",
        plan.content.items
    );
    for item in plan.content.items.iter().filter(|i| i.place.is_some()) {
        assert_eq!(
            item.place.as_ref().unwrap().how,
            title_select_api::build::PlaceHow::Rename,
            "{item:?}"
        );
    }
    let playlist: Vec<&str> = plan
        .content
        .items
        .iter()
        .filter(|i| i.generated.is_some())
        .map(|i| i.target.as_str())
        .collect();
    assert_eq!(playlist.len(), 1, "{:?}", plan.content.items);

    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert!(
        env.items(&job)
            .iter()
            .all(|i| i.state == ItemState::Applied),
        "{:?}",
        env.items(&job)
    );
    // Nothing of them is left in the source set, and each is placed, with the playlist beside them.
    assert_eq!(
        tree(&env.tmp.path().join("roots/sets").join(SET))
            .into_iter()
            .filter(|f| f.starts_with("Orbit") || f.starts_with("Saga"))
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    let mut placed: Vec<String> = orbit.iter().chain(&saga).cloned().collect();
    placed.push(playlist[0].to_owned());
    placed.sort();
    assert_eq!(tree(&env.dir()), placed);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 7);
}

/// A disc's files are placed together: after a stop between two of them, on the rename and the copy path, the disc
/// is recorded whole in one place, its source set or the play library folder, and the next build places it.
#[test]
fn a_disc_whose_placement_stops_between_its_files_is_recorded_whole_in_one_place() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use title_select_store::Fault;
    // Where the stop is, as the nth fault of a kind; whether the disc is placed once recovery settles it.
    type Stop = (&'static str, bool, fn(Fault) -> bool, usize, bool);
    let stops: [Stop; 5] = [
        (
            "renaming the second file",
            false,
            |f| matches!(f, Fault::PlaceRenaming(_)),
            2,
            false,
        ),
        (
            "moved the first file",
            false,
            |f| matches!(f, Fault::PlaceMoved(_)),
            1,
            false,
        ),
        (
            "moved every file",
            false,
            |f| matches!(f, Fault::PlaceMoved(_)),
            3,
            true,
        ),
        (
            "copied the first file",
            true,
            |f| matches!(f, Fault::PlaceCopied(_)),
            1,
            false,
        ),
        (
            "deleting the second original",
            true,
            |f| matches!(f, Fault::PlaceChecking(_)),
            2,
            true,
        ),
    ];
    for (name, across, stop, nth, placed) in stops {
        let mut env = Env::new(None);
        placing(&mut env, None);
        env.store.copy_across_filesystems(across);
        let (folder, selection) = env.folder(&[CD]);
        let plan = env.plan(&folder, selection);
        let targets = placing_targets(&plan, &[CD]);
        assert_eq!(targets.len(), 3, "{name}: {:?}", plan.content.items);

        let job = env
            .store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.claim_job().unwrap().unwrap();
        let seen = AtomicUsize::new(0);
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if stop(fault) && seen.fetch_add(1, Ordering::SeqCst) + 1 == nth {
                    panic!("killed at {fault:?}");
                }
            })));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.store.run_build_job(&env.config, job.id, None)
        }));
        env.store.hook_faults(None);
        assert!(ran.is_err(), "{name}: {ran:?}");

        let mut env = env.restart();
        let set = tree(&env.tmp.path().join("roots/sets").join(SET))
            .into_iter()
            .filter(|f| f.starts_with("Orbit"))
            .count();
        let (library, recorded) = (tree(&env.dir()), env.store.placed_files(SET).unwrap());
        match placed {
            false => {
                assert_eq!(
                    (set, library.len(), recorded.len()),
                    (3, 0, 0),
                    "{name}: {library:?}"
                )
            }
            true => {
                assert_eq!((set, recorded.len()), (0, 3), "{name}: {recorded:?}");
                assert_eq!(library, targets, "{name}");
            }
        }
        // The next build places whatever the stop left in the source set.
        placing(&mut env, None);
        env.store.copy_across_filesystems(across);
        env.store
            .audit_source_set(
                &env.config,
                SET,
                None,
                title_select_engine::AuditEvidence::Incremental,
            )
            .unwrap();
        let job = env.build(&env.plan(&folder, selection));
        assert_eq!(job.state, JobState::Succeeded, "{name}: {job:?}");
        assert_eq!(tree(&env.dir()), targets, "{name}");
        assert_eq!(env.store.placed_files(SET).unwrap().len(), 3, "{name}");
    }
}

/// Another play library folder builds a disc's CHD from its files where they are placed, in the folder holding them.
#[test]
fn another_folder_builds_a_chd_from_a_disc_placed_in_the_folder_holding_it() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing(&mut env, Some(&chdman));
    let (holding, selection) = env.folder(&[CD]);
    let job = env.build(&env.plan(&holding, selection));
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 3);

    // A second folder stores the system's games as CHD, and copies them from where they are.
    let other = env
        .store
        .add_library_folder(
            &env.config,
            &AddLibraryFolder {
                root: "play".into(),
                path: Some("chds".into()),
                template: None,
                source_set: SET.into(),
                profile: PROFILE.into(),
            },
        )
        .unwrap();
    env.store.set_holding_folder(SET, Some(holding.id)).unwrap();
    env.set_format(Some(StorageFormat::Chd), false);
    let plan = env.plan(&other, selection);
    let item = &plan.content.items[0];
    assert!(item.chd, "{item:?}");
    assert!(
        item.sources
            .iter()
            .all(|s| title_select_api::storage::split_placed(&s.path).is_some()),
        "{item:?}"
    );
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let items = env.items(&job);
    assert_eq!(items[0].state, ItemState::Applied, "{items:?}");
    assert!(
        env.tmp
            .path()
            .join("roots/play/chds/Orbit (USA).chd")
            .is_file()
    );
    // The placed disc is as it was.
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 3);
}

/// Flips the last byte of `path` in place, then sets its modification time back: the same size and stamp, other bytes.
fn corrupt_keeping_stamp(path: &Path) {
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(path, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// A disc zipped in its source set is converted to its loose tracks, each placed, all standing in for the one ZIP:
/// the ZIP is deleted only once every track is checked last, in place. With one track changed before that check, the
/// ZIP goes to the source set's kept files, on the same filesystem and across filesystems, with or without a stop.
#[test]
fn a_zip_converted_to_several_files_is_deleted_only_once_every_file_holds() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use title_select_store::Fault;
    // Whether a track is changed, where the build stops if it does (the track is then changed before the restart),
    // and whether across filesystems.
    type Case = (&'static str, bool, Option<fn(Fault) -> bool>, bool);
    let cases: [Case; 6] = [
        ("unchanged", false, None, false),
        ("unchanged, across", false, None, true),
        ("changed at the last check", true, None, false),
        ("changed at the last check, across", true, None, true),
        (
            "stopped at the last check",
            true,
            Some(|f| matches!(f, Fault::PlaceChecking(_))),
            false,
        ),
        (
            "stopped once recorded, across",
            true,
            Some(|f| matches!(f, Fault::PlaceRecorded(_))),
            true,
        ),
    ];
    for (name, change, stop, across) in cases {
        let mut env = Env::setup(None, &[CD]);
        placing(&mut env, None);
        env.store.copy_across_filesystems(across);
        let (folder, selection) = env.folder(&[CD]);
        let plan = env.plan(&folder, selection);
        let orbit: Vec<_> = plan.content.items.iter().filter(|i| i.game == CD).collect();
        assert_eq!(orbit.len(), 3, "{name}: {orbit:?}");
        // The ZIP is the original of one of them only.
        let owners: Vec<_> = orbit
            .iter()
            .filter(|i| !i.place.as_ref().unwrap().originals.is_empty())
            .collect();
        assert_eq!(owners.len(), 1, "{name}: {orbit:?}");
        assert_eq!(
            owners[0].place.as_ref().unwrap().how,
            title_select_api::build::PlaceHow::Convert
        );
        let track = orbit
            .iter()
            .find(|i| i.place.as_ref().unwrap().originals.is_empty())
            .unwrap();
        let track = env.dir().join(&track.target);
        let zip = env
            .tmp
            .path()
            .join("roots/sets")
            .join(SET)
            .join(format!("{CD}.zip"));
        let bytes = std::fs::read(&zip).unwrap();

        let job = env
            .store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        env.store.claim_job().unwrap().unwrap();
        let (at, done) = (track.clone(), AtomicBool::new(false));
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if !change || done.load(Ordering::SeqCst) {
                    return;
                }
                match stop {
                    Some(stop) if stop(fault) => {
                        done.store(true, Ordering::SeqCst);
                        panic!("killed at {fault:?}");
                    }
                    None if matches!(fault, Fault::PlaceChecking(_)) => {
                        done.store(true, Ordering::SeqCst);
                        corrupt_keeping_stamp(&at);
                    }
                    _ => {}
                }
            })));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.store.run_build_job(&env.config, job.id, None)
        }));
        env.store.hook_faults(None);
        let env = match stop {
            Some(_) => {
                assert!(ran.is_err(), "{name}: {ran:?}");
                corrupt_keeping_stamp(&track);
                env.restart()
            }
            None => {
                let job = ran.unwrap().unwrap();
                assert_eq!(job.state, JobState::Succeeded, "{name}: {job:?}");
                env
            }
        };

        // Placed either way: the three tracks are recorded where they are.
        assert_eq!(env.store.placed_files(SET).unwrap().len(), 3, "{name}");
        assert!(!zip.exists(), "{name}");
        let kept = env.store.source_kept_files(SET).unwrap();
        match change {
            false => assert!(kept.is_empty(), "{name}: {kept:?}"),
            true => {
                assert_eq!(kept.len(), 1, "{name}: {kept:?}");
                assert!(kept[0].build.is_some() && kept[0].plan.is_none(), "{name}");
                assert_eq!(kept[0].name, format!("{CD}.zip"), "{name}");
                let at = env
                    .tmp
                    .path()
                    .join("roots/sets")
                    .join(SET)
                    .join(".title-select/retained")
                    .join(kept[0].generation.to_string())
                    .join(&kept[0].kept_name);
                assert_eq!(std::fs::read(at).unwrap(), bytes, "{name}");
            }
        }
    }
}

// Place Converted Games (#216): under single copy, a disc the play library stores as CHD is placed there as its CHD,
// its cue sheet kept in the store, rather than copied.

/// Turns single copy on for `env`, with its source set's folder writable and the system's games stored in play
/// libraries as CHD, converted games placed, by `chdman`.
fn placing_chds(env: &mut Env, chdman: &Path) {
    placing(env, Some(chdman));
    let mut policy = env.store.storage_policy().unwrap();
    let system = policy
        .play_libraries
        .systems
        .entry(SYSTEM.into())
        .or_default();
    system.format = Some(StorageFormat::Chd);
    system.place_converted = Some(true);
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

impl Env {
    /// A Selection of `titles`, each the winner of its own group.
    fn select(&self, titles: &[&str]) -> i64 {
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
            .record_selection(job.id, &selection)
            .unwrap()
            .selection
            .unwrap()
    }

    fn audit(
        &self,
        evidence: title_select_engine::AuditEvidence,
    ) -> title_select_api::report::Report {
        self.store
            .audit_source_set(&self.config, SET, None, evidence)
            .unwrap()
            .report
    }

    /// Every cue sheet the store keeps.
    fn cues(&self) -> Vec<Vec<u8>> {
        let mut cues: Vec<Vec<u8>> = std::fs::read_dir(self.store.dir().join("cues"))
            .into_iter()
            .flatten()
            .map(|e| std::fs::read(e.unwrap().path()).unwrap())
            .collect();
        cues.sort();
        cues
    }

    /// The files of `game` in the source set's folder.
    fn in_set(&self, game: &str) -> Vec<String> {
        tree(&self.tmp.path().join("roots/sets").join(SET))
            .into_iter()
            .filter(|f| f.starts_with(game))
            .collect()
    }
}

/// The cue sheet the DAT lists for Orbit.
fn orbit_cue() -> Vec<u8> {
    cd()[0].payload.clone()
}

/// Places Orbit as a CHD in a new folder, and audits the source set again; returns the folder.
fn place_orbit(env: &mut Env, chdman: &Path) -> LibraryFolder {
    placing_chds(env, chdman);
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let item = &plan.content.items[0];
    assert!(item.chd && item.place.is_some(), "{:?}", plan.content);
    let job = env.build(&plan);
    assert_eq!(
        job.state,
        JobState::Succeeded,
        "{job:?} {:?}",
        env.items(&job)
    );
    folder
}

#[test]
fn a_disc_placed_as_chd_keeps_its_cue_sheet_in_the_store_and_its_tracks_go() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let _folder = place_orbit(&mut env, &chdman);
    assert_eq!(tree(&env.dir()), ["Orbit (USA).chd"]);
    assert_eq!(env.in_set(CD), Vec::<String>::new());
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1, "{placed:?}");
    assert!(placed[0].chd, "{placed:?}");
    assert_eq!(placed[0].path, "Orbit (USA).chd");
    assert_eq!(env.cues(), [orbit_cue()]);
    // The cue sheet is kept for as long as the disc is placed.
    env.store.collect_garbage().unwrap();
    assert_eq!(env.cues(), [orbit_cue()]);
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
}

#[test]
fn a_dvd_placed_as_chd_needs_no_cue_sheet() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    let (folder, selection) = env.folder(&[DVD]);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(
        job.state,
        JobState::Succeeded,
        "{job:?} {:?}",
        env.items(&job)
    );
    assert_eq!(tree(&env.dir()), ["Atlas (USA).chd"]);
    assert_eq!(env.in_set(DVD), Vec::<String>::new());
    assert!(env.cues().is_empty());
}

#[test]
fn placing_converted_games_is_off_by_default_and_a_choice_for_play_libraries_alone() {
    let env = Env::new(None);
    let policy = env.store.storage_policy().unwrap();
    assert!(!policy.play_libraries.place_converted);
    let mut refused = policy.clone();
    refused.source_sets.place_converted = true;
    assert!(env.store.set_storage_policy(&refused, "Operator").is_err());
    let mut refused = policy.clone();
    refused
        .source_sets
        .systems
        .entry(SYSTEM.into())
        .or_default()
        .place_converted = Some(true);
    assert!(env.store.set_storage_policy(&refused, "Operator").is_err());
    // Without single copy, placing converted games places nothing.
    let mut on = policy;
    on.play_libraries.place_converted = true;
    env.store.set_storage_policy(&on, "Operator").unwrap();
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let item = &plan.content.items[0];
    assert!(
        item.chd && item.place.is_none() && !item.changes_source_set,
        "{item:?}"
    );
}

/// A quick inventory takes a placed CHD's tracks on claims, by its header's SHA-1, never decompressing it; a full audit
/// extracts it and verifies each against the DAT; an incremental audit reuses that read while the CHD's stamp holds.
/// As for any file, a quick inventory also reuses a payload read of the CHD as it is: so it runs first here, before
/// any audit has read it where it is placed.
#[test]
fn a_quick_inventory_claims_a_placed_chd_a_full_audit_verifies_it_and_an_incremental_reuses_that() {
    use title_select_api::report::{EvidenceLevel, Finding, GameStatus};
    use title_select_engine::AuditEvidence;
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    let placed = title_select_api::storage::placed_path(folder.id, "Orbit (USA).chd");
    let of_orbit = |report: &title_select_api::report::Report| {
        let mut entries: Vec<_> = report
            .entries
            .iter()
            .filter(|e| e.path == placed)
            .cloned()
            .collect();
        entries.sort_by(|a, b| a.member.cmp(&b.member));
        let game = report.games.iter().find(|g| g.name == CD).unwrap().clone();
        (entries, game)
    };
    let members = [
        Some("Orbit (USA) (Track 1).bin"),
        Some("Orbit (USA) (Track 2).bin"),
        Some("Orbit (USA).cue"),
    ];

    let quick = env.audit(AuditEvidence::Quick);
    let (entries, game) = of_orbit(&quick);
    let named: Vec<Option<&str>> = entries.iter().map(|e| e.member.as_deref()).collect();
    assert_eq!(named, members, "{entries:?}");
    for e in &entries {
        assert_eq!(e.finding, Finding::Matched, "{e:?}");
        assert_eq!(
            e.evidence.as_ref().unwrap().level,
            EvidenceLevel::Claim,
            "{e:?}"
        );
    }
    assert_eq!(game.status, GameStatus::Complete);
    assert_eq!(game.claimed_roms.len(), 3, "{game:?}");

    let full = env.audit(AuditEvidence::Full);
    let (entries, game) = of_orbit(&full);
    let named: Vec<Option<&str>> = entries.iter().map(|e| e.member.as_deref()).collect();
    assert_eq!(named, members, "{entries:?}");
    for e in &entries {
        assert_eq!(e.finding, Finding::Matched, "{e:?}");
        let evidence = e.evidence.as_ref().unwrap();
        assert_eq!(evidence.level, EvidenceLevel::Payload, "{e:?}");
        assert!(evidence.reused.is_none(), "{e:?}");
    }
    assert_eq!(game.status, GameStatus::Complete, "{game:?}");
    assert!(game.claimed_roms.is_empty(), "{game:?}");

    let incremental = env.audit(AuditEvidence::Incremental);
    let (entries, game) = of_orbit(&incremental);
    assert_eq!(entries.len(), 3, "{entries:?}");
    for e in &entries {
        let evidence = e.evidence.as_ref().unwrap();
        assert_eq!(evidence.level, EvidenceLevel::Payload, "{e:?}");
        assert!(evidence.reused.is_some(), "{e:?}");
    }
    assert_eq!(game.status, GameStatus::Complete);
    assert!(game.claimed_roms.is_empty(), "{game:?}");
}

/// A placed CHD's cue sheet is checked from the store: one whose blob no longer holds the DAT's bytes is unreadable,
/// and its game not complete.
#[test]
fn a_placed_chds_cue_sheet_is_audited_from_the_store() {
    use title_select_api::report::{Finding, GameStatus};
    use title_select_engine::AuditEvidence;
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    let blob = std::fs::read_dir(env.store.dir().join("cues"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&blob).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&blob, &bytes).unwrap();
    let report = env.audit(AuditEvidence::Full);
    let placed = title_select_api::storage::placed_path(folder.id, "Orbit (USA).chd");
    let cue = report
        .entries
        .iter()
        .find(|e| e.path == placed && e.member.as_deref() == Some("Orbit (USA).cue"));
    assert!(
        cue.is_none_or(|e| e.finding != Finding::Matched),
        "{:?}",
        report.entries
    );
    let game = report.games.iter().find(|g| g.name == CD).unwrap();
    assert_ne!(game.status, GameStatus::Complete, "{game:?}");
}

/// What `job` did with each of its items, by target: its state and any error.
fn states(env: &Env, job: &title_select_api::Job) -> Vec<(String, ItemState, Option<String>)> {
    env.items(job)
        .into_iter()
        .map(|i| (i.target, i.state, i.error))
        .collect()
}

/// Every file of Orbit in the source set's folder, by name, as the bytes of each ROM it holds, loose or in a ZIP.
fn orbit_home(env: &Env) -> Vec<(String, Vec<u8>)> {
    let set = env.tmp.path().join("roots/sets").join(SET);
    let mut found = Vec::new();
    for f in env.in_set(CD) {
        let path = set.join(&f);
        match f.ends_with(".zip") {
            true => {
                let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
                for n in 0..zip.len() {
                    let mut member = zip.by_index(n).unwrap();
                    let mut bytes = Vec::new();
                    std::io::Read::read_to_end(&mut member, &mut bytes).unwrap();
                    found.push((member.name().unwrap().into_owned(), bytes));
                }
            }
            false => found.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )),
        }
    }
    found.sort();
    found
}

/// Orbit's ROMs as the DAT lists them, by name.
fn orbit_roms() -> Vec<(String, Vec<u8>)> {
    let mut roms: Vec<(String, Vec<u8>)> = cd()
        .into_iter()
        .map(|r| (r.name.to_owned(), r.payload))
        .collect();
    roms.sort();
    roms
}

/// Places Orbit as a CHD, audits the source set, then plans its return home, Orbit no longer selected.
fn returning_orbit(env: &mut Env, chdman: &Path) -> (LibraryFolder, Plan) {
    let (folder, plan, _) = returning_orbit_by(env, chdman);
    (folder, plan)
}

/// As [`returning_orbit`], with the Selection the plan was made from.
fn returning_orbit_by(env: &mut Env, chdman: &Path) -> (LibraryFolder, Plan, i64) {
    let folder = place_orbit(env, chdman);
    env.audit(title_select_engine::AuditEvidence::Full);
    let selection = env.select(&[DVD]);
    let plan = env.plan(&folder, selection);
    let r = plan
        .content
        .returns
        .iter()
        .find(|r| r.release == CD)
        .unwrap_or_else(|| panic!("{:?}", plan.content));
    assert_eq!(r.how, title_select_api::build::ReturnHow::Convert, "{r:?}");
    (folder, plan, selection)
}

#[test]
fn a_placed_chd_is_returned_home_byte_exact_its_cue_sheet_from_the_store() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (_, plan) = returning_orbit(&mut env, &chdman);
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(orbit_home(&env), orbit_roms());
    assert!(
        env.store
            .placed_files(SET)
            .unwrap()
            .iter()
            .all(|p| p.game != CD)
    );
    assert!(!env.dir().join("Orbit (USA).chd").exists());
    // Once home, the cue sheet the store kept for it goes.
    env.store.collect_garbage().unwrap();
    assert!(env.cues().is_empty());
    // What the return wrote home is what the next audit finds.
    let report = env.audit(title_select_engine::AuditEvidence::Full);
    let game = report.games.iter().find(|g| g.name == CD).unwrap();
    assert_eq!(game.status, title_select_api::report::GameStatus::Complete);
    let scratch: Vec<String> = std::fs::read_dir(env.store.dir().join("tmp"))
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(scratch.is_empty(), "{scratch:?}");
}

/// The latest finished audit of the source set, whole.
fn latest_report(env: &Env) -> (i64, title_select_api::report::Report) {
    let audit = env
        .store
        .latest_audit_rows(SET, |_, _| {}, |_| {})
        .unwrap()
        .unwrap();
    (audit.id, env.store.report(audit.id).unwrap())
}

/// Sets the configured chdman to `chdman`, keeping the source set's folder writable.
fn use_chdman(env: &mut Env, chdman: &Path) {
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
         [chd]\nchdman = {:?}\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.tmp.path().join("roots/play").display().to_string(),
        chdman.display().to_string(),
    ))
    .unwrap();
}

/// Gives chdman `seconds` for any disc.
fn chd_seconds(env: &Env, seconds: u64) {
    env.store
        .update_settings(
            &env.config,
            &title_select_api::settings::SettingsUpdate {
                chd: Some(title_select_api::settings::ChdTimeouts {
                    seconds,
                    seconds_per_gib: 0,
                }),
                ..Default::default()
            },
            "Operator",
        )
        .unwrap();
}

/// Whether Orbit is placed as it was before `job` tried to return it, as `before`, with nothing of it home, and the
/// item failed saying it stays placed.
fn orbit_stayed(env: &Env, job: &title_select_api::Job, before: &[u8]) -> String {
    let item = env.items(job).into_iter().find(|i| i.game == CD).unwrap();
    assert_eq!(item.state, ItemState::Failed, "{item:?}");
    let error = item.error.clone().unwrap();
    assert!(error.contains("stays placed"), "{error}");
    assert_eq!(
        std::fs::read(env.dir().join("Orbit (USA).chd")).unwrap(),
        before
    );
    assert!(
        env.store
            .placed_files(SET)
            .unwrap()
            .iter()
            .any(|p| p.game == CD)
    );
    assert!(env.in_set(CD).is_empty(), "{:?}", env.in_set(CD));
    assert_eq!(env.cues(), [orbit_cue()]);
    error
}

/// A return whose chdman runs out of time says nothing of what the placed CHD holds: the game stays placed, unflagged,
/// the latest audit's verdict as it was, and the next build tries again.
#[cfg(unix)]
#[test]
fn a_return_whose_chdman_times_out_leaves_the_game_placed_unflagged_and_the_audit_as_it_was() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    env.audit(title_select_engine::AuditEvidence::Full);
    let audit = latest_report(&env);
    let before = std::fs::read(env.dir().join("Orbit (USA).chd")).unwrap();
    let fakes = tempfile::tempdir().unwrap();
    use_chdman(&mut env, &fake_chdman(fakes.path(), "slow"));
    chd_seconds(&env, 1);
    let selection = env.select(&[]);
    let job = env.build(&env.plan(&folder, selection));
    let error = orbit_stayed(&env, &job, &before);
    assert!(error.contains("took longer than 1 second"), "{error}");
    assert!(env.store.review_flags(SET).unwrap().is_empty());
    assert_eq!(latest_report(&env), audit);

    // With chdman as it should be, the next build returns it.
    use_chdman(&mut env, &chdman);
    chd_seconds(&env, 600);
    let selection = env.select(&[]);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(orbit_home(&env), orbit_roms());
}

/// Builds, with `chdman`, Orbit's CHD from its cue sheet and a first track whose last byte differs from the DAT's.
fn orbit_with_another_track(chdman: &Path, dir: &Path) -> PathBuf {
    let roms = cd();
    for r in &roms {
        let mut bytes = r.payload.clone();
        if r.name.ends_with("(Track 1).bin") {
            *bytes.last_mut().unwrap() ^= 0xff;
        }
        std::fs::write(dir.join(r.name), bytes).unwrap();
    }
    let out = dir.join("other.chd");
    let made = std::process::Command::new(chdman)
        .arg("createcd")
        .arg("-i")
        .arg(dir.join("Orbit (USA).cue"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    out
}

/// A placed CHD that a return proves doesn't hold its disc as the DAT lists it, as a CHD chdman says is damaged or one
/// that gives back a track of other bytes, stays placed with nothing deleted or written home, and its game needs review
/// with the reason; no audit's verdict changes. A Full Audit that verifies the CHD clears the flag, and so does a
/// return that verifies it home.
#[test]
fn a_placed_chd_proven_damaged_stays_placed_and_its_game_is_flagged_until_verified() {
    let Some(chdman) = chdman() else { return };
    let made = tempfile::tempdir().unwrap();
    let other = orbit_with_another_track(&chdman, made.path());
    // How the copy chdman reads differs from what the DAT lists: its last byte flipped, as a CHD damaged where it is
    // placed would read but holding still, as it must to be returned at all; or a CHD of a track of other bytes.
    type Damage = (&'static str, &'static str, Box<dyn Fn(&Path) + Send + Sync>);
    let damages: [Damage; 2] = [
        (
            "damaged",
            "chdman extractcd failed",
            Box::new(|copy: &Path| {
                let mut bytes = std::fs::read(copy).unwrap();
                *bytes.last_mut().unwrap() ^= 0xff;
                std::fs::write(copy, &bytes).unwrap();
            }),
        ),
        (
            "another track",
            "does not hash to the DAT's ROM",
            Box::new(move |copy: &Path| {
                std::fs::copy(&other, copy).unwrap();
            }),
        ),
    ];
    for (label, said, damage) in damages {
        let mut env = Env::new(Some(&chdman));
        let (folder, plan, selection) = returning_orbit_by(&mut env, &chdman);
        // What the latest audit says of Orbit, and in all: the build places the DVD too, which moves its entry.
        let verdict = |env: &Env| {
            let (id, report) = latest_report(env);
            (id, report.summary.clone(), orbit_found(&report, &folder))
        };
        let audit = verdict(&env);
        let before = std::fs::read(env.dir().join("Orbit (USA).chd")).unwrap();
        let tmp = env.store.dir().join("tmp");
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if matches!(fault, title_select_store::Fault::ReturnChdCopied(_)) {
                    for e in std::fs::read_dir(&tmp).unwrap() {
                        let copy = e.unwrap().path().join("out.chd");
                        if copy.is_file() {
                            damage(&copy);
                        }
                    }
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        let error = orbit_stayed(&env, &job, &before);
        assert!(error.contains(said), "{label}: {error}");
        let flags = env.store.review_flags(SET).unwrap();
        assert_eq!(flags.len(), 1, "{label}: {flags:?}");
        assert_eq!(flags[0].game, CD);
        assert_eq!(
            flags[0].path,
            title_select_api::storage::placed_path(folder.id, "Orbit (USA).chd")
        );
        assert!(flags[0].reason.contains(said), "{label}: {flags:?}");
        assert_eq!(verdict(&env), audit, "{label}: the audit's verdict changed");

        match label {
            // A Full Audit that verifies the CHD itself clears the flag.
            "damaged" => {
                env.audit(title_select_engine::AuditEvidence::Full);
                assert!(env.store.review_flags(SET).unwrap().is_empty(), "{label}");
            }
            // A return that verifies the game home clears it.
            _ => {
                let job = env.build(&env.plan(&folder, selection));
                assert_eq!(
                    job.state,
                    JobState::Succeeded,
                    "{label}: {:?}",
                    states(&env, &job)
                );
                assert_eq!(orbit_home(&env), orbit_roms());
                assert!(env.store.review_flags(SET).unwrap().is_empty(), "{label}");
            }
        }
    }
}

/// Runs `plan`'s build until the `nth` fault `stop` matches, killed there as by a stop of the service, then restarts
/// it, which recovers what the stop left.
fn killed_at(
    env: Env,
    plan: &Plan,
    (stop, nth): (fn(title_select_store::Fault) -> bool, usize),
    label: &str,
) -> Env {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let seen = AtomicUsize::new(0);
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if stop(fault) && seen.fetch_add(1, Ordering::SeqCst) + 1 == nth {
                panic!("killed at {fault:?}");
            }
        })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{label}: never reached the stop: {ran:?}");
    env.restart()
}

/// Whether Orbit has exactly one recorded copy: placed in `folder` as its CHD, recorded so, its cue sheet kept in the
/// store even once garbage is collected, and nothing of it in the source set; or in the source set as the DAT lists it,
/// not placed, and no CHD of it in the folder. No CHD work space is left. Returns whether it is placed.
fn orbit_once(env: &Env, label: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    env.store.collect_garbage().unwrap();
    assert!(
        env.chd_scratch().is_empty(),
        "{label}: {:?}",
        env.chd_scratch()
    );
    let rows: Vec<_> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .filter(|p| p.game == CD)
        .collect();
    let chd = env.dir().join("Orbit (USA).chd");
    match rows.as_slice() {
        [] => {
            assert_eq!(orbit_home(env), orbit_roms(), "{label}: home");
            assert!(!chd.exists(), "{label}: home, yet in the folder");
            false
        }
        [row] => {
            assert!(row.chd, "{label}: {row:?}");
            assert!(
                env.in_set(CD).is_empty(),
                "{label}: placed, yet {:?}",
                env.in_set(CD)
            );
            let st = std::fs::metadata(&chd).unwrap();
            assert_eq!(
                (st.dev(), st.ino()),
                (row.dev, row.ino),
                "{label}: the file recorded"
            );
            assert!(
                env.cues().contains(&orbit_cue()),
                "{label}: its cue sheet is gone"
            );
            true
        }
        rows => panic!("{label}: {rows:?}"),
    }
}

type Kill = (&'static str, fn(title_select_store::Fault) -> bool, usize);

/// A stop at each step of placing a disc as CHD leaves it with exactly one recorded copy, and the next build places it.
#[test]
fn a_disc_whose_placement_as_chd_stops_at_any_step_has_one_recorded_copy_and_is_placed_next() {
    use title_select_store::Fault;
    let Some(chdman) = chdman() else { return };
    let kills: [Kill; 11] = [
        ("chd copied", |f| matches!(f, Fault::ChdCopied(_)), 1),
        ("cue sheet kept", |f| matches!(f, Fault::PlaceCueKept(_)), 1),
        ("staged", |f| matches!(f, Fault::Staged(_)), 1),
        (
            "before publish",
            |f| matches!(f, Fault::BeforePublish(_)),
            1,
        ),
        (
            "published unsynced",
            |f| matches!(f, Fault::PublishedUnsynced(_)),
            1,
        ),
        ("after publish", |f| matches!(f, Fault::AfterPublish(_)), 1),
        ("recorded", |f| matches!(f, Fault::PlaceRecorded(_)), 1),
        (
            "checking the first original",
            |f| matches!(f, Fault::PlaceChecking(_)),
            1,
        ),
        (
            "checking the last original",
            |f| matches!(f, Fault::PlaceChecking(_)),
            3,
        ),
        (
            "disposed of the first original",
            |f| matches!(f, Fault::PlaceDisposed(_)),
            1,
        ),
        (
            "disposed of the last original",
            |f| matches!(f, Fault::PlaceDisposed(_)),
            3,
        ),
    ];
    for (label, stop, nth) in kills {
        let mut env = Env::new(Some(&chdman));
        placing_chds(&mut env, &chdman);
        let (folder, selection) = env.folder(&[CD]);
        let plan = env.plan(&folder, selection);
        let env = killed_at(env, &plan, (stop, nth), label);
        orbit_once(&env, label);
        env.audit(title_select_engine::AuditEvidence::Incremental);
        let job = env.build(&env.plan(&folder, selection));
        assert_eq!(
            job.state,
            JobState::Succeeded,
            "{label}: {:?}",
            states(&env, &job)
        );
        assert!(
            orbit_once(&env, label),
            "{label}: the next build didn't place it"
        );
        assert_eq!(env.cues(), [orbit_cue()], "{label}");
    }
}

/// A stop at each step of returning a placed CHD home leaves its disc with exactly one recorded copy, and the next
/// build returns it.
#[test]
fn a_placed_chd_whose_return_stops_at_any_step_has_one_recorded_copy_and_is_returned_next() {
    use title_select_store::Fault;
    let Some(chdman) = chdman() else { return };
    let kills: [Kill; 9] = [
        (
            "copied for chdman",
            |f| matches!(f, Fault::ReturnChdCopied(_)),
            1,
        ),
        (
            "staged the first file",
            |f| matches!(f, Fault::ReturnStaged(_)),
            1,
        ),
        (
            "staged the last file",
            |f| matches!(f, Fault::ReturnStaged(_)),
            3,
        ),
        (
            "published the first file",
            |f| matches!(f, Fault::ReturnPublished(_)),
            1,
        ),
        (
            "published the last file",
            |f| matches!(f, Fault::ReturnPublished(_)),
            3,
        ),
        ("set aside", |f| matches!(f, Fault::ReturnSetAside(_)), 1),
        ("checking", |f| matches!(f, Fault::ReturnChecking(_)), 1),
        ("recorded", |f| matches!(f, Fault::ReturnRecorded(_)), 1),
        ("disposed", |f| matches!(f, Fault::ReturnDisposed(_)), 1),
    ];
    for (label, stop, nth) in kills {
        let mut env = Env::new(Some(&chdman));
        let (folder, plan, selection) = returning_orbit_by(&mut env, &chdman);
        let env = killed_at(env, &plan, (stop, nth), label);
        orbit_once(&env, label);
        env.audit(title_select_engine::AuditEvidence::Incremental);
        let job = env.build(&env.plan(&folder, selection));
        assert_eq!(
            job.state,
            JobState::Succeeded,
            "{label}: {:?}",
            states(&env, &job)
        );
        assert!(
            !orbit_once(&env, label),
            "{label}: the next build didn't return it"
        );
        assert!(env.cues().is_empty(), "{label}: {:?}", env.cues().len());
    }
}

/// Places Orbit as a CHD in its holding folder, audits the source set, and adds another folder, `more`.
fn orbit_and_another_folder(env: &mut Env, chdman: &Path) -> (LibraryFolder, LibraryFolder) {
    let holding = place_orbit(env, chdman);
    env.audit(title_select_engine::AuditEvidence::Full);
    env.store.set_holding_folder(SET, Some(holding.id)).unwrap();
    let other = env
        .store
        .add_library_folder(
            &env.config,
            &AddLibraryFolder {
                root: "play".into(),
                path: Some("more".into()),
                template: None,
                source_set: SET.into(),
                profile: PROFILE.into(),
            },
        )
        .unwrap();
    (holding, other)
}

/// Another folder that stores the game as CHD copies the placed CHD whole, read by descriptor where it is placed: the
/// very CHD verified when it was placed, which stays placed as it was.
#[test]
fn another_folder_storing_chds_copies_a_disc_placed_as_chd_whole() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (_, other) = orbit_and_another_folder(&mut env, &chdman);
    let selection = env.select(&[CD]);
    let plan = env.plan(&other, selection);
    let item = plan.content.items.iter().find(|i| i.game == CD).unwrap();
    assert!(item.chd, "{item:?}");
    assert!(
        item.sources
            .iter()
            .all(|s| s.path.ends_with("Orbit (USA).chd")
                && s.path.starts_with(".title-select/placed/")),
        "{item:?}"
    );
    let placed = env.store.placed_files(SET).unwrap();
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    let item = env.items(&job).into_iter().find(|i| i.game == CD).unwrap();
    assert_eq!(item.state, ItemState::Applied, "{item:?}");
    let copy = env.tmp.path().join("roots/play/more/Orbit (USA).chd");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(env.dir().join("Orbit (USA).chd")).unwrap()
    );
    assert_eq!(env.store.placed_files(SET).unwrap(), placed);
    // The copy passes the folder's check by its header, as any CHD built there does.
    let check = env.check(&env.config, &other);
    assert_eq!(
        outcome(&check, "Orbit (USA).chd").0,
        FileOutcome::Unchanged,
        "{check:?}"
    );
}

/// Another folder that stores the game otherwise writes it from what extracting the placed CHD gives back, its cue
/// sheet from the store, every file verified against the DAT, as a return writes it home; the CHD stays placed.
#[test]
fn another_folder_storing_loose_files_extracts_a_disc_placed_as_chd() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (_, other) = orbit_and_another_folder(&mut env, &chdman);
    env.set_format(Some(StorageFormat::LooseFolder), false);
    let selection = env.select(&[CD]);
    let plan = env.plan(&other, selection);
    let items: Vec<_> = plan.content.items.iter().filter(|i| i.game == CD).collect();
    assert!(
        !items.is_empty() && items.iter().all(|i| !i.chd),
        "{items:?}"
    );
    let placed = env.store.placed_files(SET).unwrap();
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert!(
        env.items(&job)
            .iter()
            .filter(|i| i.game == CD)
            .all(|i| i.state == ItemState::Applied),
        "{:?}",
        states(&env, &job)
    );
    let more = env.tmp.path().join("roots/play/more");
    let mut written: Vec<(String, Vec<u8>)> = tree(&more)
        .into_iter()
        .filter(|f| f.contains("Orbit"))
        .map(|f| {
            (
                Path::new(&f)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read(more.join(&f)).unwrap(),
            )
        })
        .collect();
    written.sort();
    assert_eq!(written, orbit_roms());
    assert_eq!(env.store.placed_files(SET).unwrap(), placed);
    assert!(env.store.review_flags(SET).unwrap().is_empty());
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
}

/// A disc a folder already holds as a CHD it built, verified then, is placed as that CHD once converted games are
/// placed: its cue sheet kept in the store from the source set, checked against the DAT, and its tracks gone.
#[test]
fn a_chd_built_earlier_is_placed_as_it_is_once_converted_games_are_placed() {
    use std::os::unix::fs::MetadataExt;
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, selection) = env.folder(&[CD]);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    let chd = env.dir().join("Orbit (USA).chd");
    let built = std::fs::metadata(&chd).unwrap();
    assert_eq!(env.in_set(CD).len(), 3);

    placing_chds(&mut env, &chdman);
    let plan = env.plan(&folder, selection);
    let item = plan.content.items.iter().find(|i| i.game == CD).unwrap();
    assert_eq!(
        item.place.as_ref().map(|p| p.how),
        Some(title_select_api::build::PlaceHow::Built),
        "{item:?}"
    );
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert!(env.in_set(CD).is_empty(), "{:?}", env.in_set(CD));
    let now = std::fs::metadata(&chd).unwrap();
    assert_eq!((now.dev(), now.ino()), (built.dev(), built.ino()));
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1, "{placed:?}");
    assert!(placed[0].chd);
    assert_eq!(env.cues(), [orbit_cue()]);
    // And it goes home as any placed CHD does.
    env.audit(title_select_engine::AuditEvidence::Full);
    let selection = env.select(&[DVD]);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(orbit_home(&env), orbit_roms());
}

/// A repair leaves a game placed as a CHD as it is, and says why: it doesn't change a CHD, as it would otherwise
/// repack it into the play library's files for its disc. A copy of its tracks elsewhere is only kept aside.
#[test]
fn a_repair_leaves_a_disc_placed_as_chd_as_it_is() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    let set = env.tmp.path().join("roots/sets").join(SET);
    for (n, rom) in cd().into_iter().enumerate() {
        std::fs::write(set.join(format!("stray {n}.bin")), &rom.payload).unwrap();
    }
    env.audit(title_select_engine::AuditEvidence::Full);
    let repair = env
        .store
        .plan_repair(&env.config, SET, &Default::default())
        .unwrap();
    let placed = title_select_api::storage::placed_path(folder.id, "");
    let touching: Vec<_> = repair
        .content
        .items
        .iter()
        .filter(|i| {
            i.game.as_deref() == Some(CD)
                || i.path.as_deref().is_some_and(|p| p.starts_with(&placed))
        })
        .collect();
    assert!(!touching.is_empty(), "{:?}", repair.content.items);
    for item in touching {
        assert_eq!(
            item.chosen,
            title_select_api::repair::RepairChoice::Leave,
            "{item:?}"
        );
        assert_eq!(
            item.choices,
            [title_select_api::repair::RepairChoice::Leave],
            "{item:?}"
        );
        assert!(
            item.note
                .as_deref()
                .is_some_and(|n| n.contains("placed as a CHD")),
            "{item:?}"
        );
    }
}

/// Garbage collected after a cue sheet is kept in the store but before its item records it takes the unreferenced
/// blob: the item then finds it gone in the transaction that would record it, and fails, its disc left in the source
/// set as it was, never placed without its cue sheet. The next build places it.
#[test]
fn a_cue_sheet_collected_before_its_placement_is_recorded_never_leaves_a_chd_placed_without_it() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let store = std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<String>>));
    let collected = store.clone();
    let dir = env.store.dir().to_path_buf();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, title_select_store::Fault::PlaceCueKept(_)) {
                // As a collection would, between the keep and the record: what no row holds goes.
                let cues = dir.join("cues");
                let gone: Vec<String> = std::fs::read_dir(&cues)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                for name in &gone {
                    std::fs::remove_file(cues.join(name)).unwrap();
                }
                *collected.lock().unwrap() = Some(gone);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(store.lock().unwrap().as_ref().map(Vec::len), Some(1));
    let item = env.items(&job).into_iter().find(|i| i.game == CD).unwrap();
    assert_eq!(item.state, ItemState::Failed, "{item:?}");
    assert!(!orbit_once(&env, "collected"));
    env.audit(title_select_engine::AuditEvidence::Incremental);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert!(orbit_once(&env, "placed next"));
}

/// Every disc of a release goes as a CHD together, or none does: placed whole, recovered whole after a stop between
/// its two discs going in place, and returned whole.
#[test]
fn a_release_of_two_discs_is_placed_as_chds_and_returned_whole() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    let (folder, selection) = env.folder(&SAGA);
    let plan = env.plan(&folder, selection);
    let placing: Vec<_> = plan
        .content
        .items
        .iter()
        .filter(|i| i.chd && i.place.is_some())
        .collect();
    assert_eq!(placing.len(), 2, "{:?}", plan.content.items);
    // Stopped once the first disc is in place: recovery puts the second in place too.
    let env = killed_at(
        env,
        &plan,
        (
            |f| matches!(f, title_select_store::Fault::AfterPublish(_)),
            1,
        ),
        "between the discs",
    );
    let saga_placed = |env: &Env| {
        env.store
            .placed_files(SET)
            .unwrap()
            .into_iter()
            .filter(|p| p.game.starts_with("Saga"))
            .count()
    };
    let in_set = |env: &Env| env.in_set("Saga").len();
    match saga_placed(&env) {
        0 => assert_eq!(in_set(&env), 4),
        2 => assert_eq!(in_set(&env), 0),
        n => panic!("{n} of Saga's discs are placed"),
    }
    env.audit(title_select_engine::AuditEvidence::Incremental);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!((saga_placed(&env), in_set(&env)), (2, 0));
    let mut chds: Vec<String> = tree(&env.dir())
        .into_iter()
        .filter(|f| f.ends_with(".chd"))
        .collect();
    chds.sort();
    assert_eq!(chds, SAGA_CHDS);
    assert_eq!(env.cues().len(), 2);

    env.audit(title_select_engine::AuditEvidence::Full);
    let selection = env.select(&[DVD]);
    let plan = env.plan(&folder, selection);
    assert!(
        plan.content.returns.iter().any(|r| r.games.len() == 2),
        "{:?}",
        plan.content.returns
    );
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!((saga_placed(&env), in_set(&env)), (0, 4));
    assert!(!tree(&env.dir()).iter().any(|f| f.starts_with("Saga")));
}

/// Orbit's entries at its placed CHD, by member, and its game, in `report`.
fn orbit_found(
    report: &title_select_api::report::Report,
    folder: &LibraryFolder,
) -> (
    Vec<title_select_api::report::Entry>,
    title_select_api::report::Game,
) {
    let placed = title_select_api::storage::placed_path(folder.id, "Orbit (USA).chd");
    let mut entries: Vec<_> = report
        .entries
        .iter()
        .filter(|e| e.path == placed)
        .cloned()
        .collect();
    entries.sort_by(|a, b| a.member.cmp(&b.member));
    let game = report.games.iter().find(|g| g.name == CD).unwrap().clone();
    (entries, game)
}

/// The kept cue sheet of the one CHD placed, in the store.
fn cue_blob(env: &Env) -> PathBuf {
    std::fs::read_dir(env.store.dir().join("cues"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

/// A quick inventory never takes a placed CHD's tracks on Payload Evidence, not even an earlier full audit's: it
/// claims them by its header, and never makes its game Verified.
#[test]
fn a_quick_inventory_after_a_full_audit_only_claims_a_placed_chd() {
    use title_select_api::report::{EvidenceLevel, Finding};
    use title_select_engine::AuditEvidence;
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    let (entries, _) = orbit_found(&env.audit(AuditEvidence::Full), &folder);
    assert!(
        entries
            .iter()
            .all(|e| e.evidence.as_ref().unwrap().level == EvidenceLevel::Payload),
        "{entries:?}"
    );
    let (entries, game) = orbit_found(&env.audit(AuditEvidence::Quick), &folder);
    assert_eq!(entries.len(), 3, "{entries:?}");
    for e in &entries {
        assert_eq!(e.finding, Finding::Matched, "{e:?}");
        let evidence = e.evidence.as_ref().unwrap();
        assert_eq!(evidence.level, EvidenceLevel::Claim, "{e:?}");
        assert!(evidence.reused.is_none(), "{e:?}");
    }
    assert_eq!(game.claimed_roms.len(), 3, "{game:?}");
}

/// A placed CHD's cue sheet is checked against what the store keeps on every audit, even while its tracks' read is
/// reused: a blob gone, or changed, since the full audit is found by the next incremental one, and by a quick one.
#[test]
fn a_cue_blob_gone_or_changed_is_found_while_the_tracks_read_is_reused() {
    use title_select_api::report::{EvidenceLevel, Finding, GameStatus};
    use title_select_engine::AuditEvidence;
    let Some(chdman) = chdman() else { return };
    for gone in [true, false] {
        let mut env = Env::new(Some(&chdman));
        let folder = place_orbit(&mut env, &chdman);
        env.audit(AuditEvidence::Full);
        let blob = cue_blob(&env);
        match gone {
            true => std::fs::remove_file(&blob).unwrap(),
            false => {
                let mut bytes = std::fs::read(&blob).unwrap();
                bytes[0] ^= 0xff;
                std::fs::write(&blob, &bytes).unwrap();
            }
        }
        for evidence in [AuditEvidence::Incremental, AuditEvidence::Quick] {
            let label = format!("gone {gone}, {evidence:?}");
            let (entries, game) = orbit_found(&env.audit(evidence), &folder);
            let cue = entries
                .iter()
                .find(|e| e.member.as_deref() == Some("Orbit (USA).cue"))
                .unwrap_or_else(|| panic!("{label}: {entries:?}"));
            assert_eq!(cue.finding, Finding::Unreadable, "{label}: {cue:?}");
            assert!(
                cue.reason
                    .as_deref()
                    .is_some_and(|r| r.contains("cue sheet")),
                "{label}: {cue:?}"
            );
            assert_ne!(game.status, GameStatus::Complete, "{label}: {game:?}");
            assert!(
                game.missing_roms.iter().any(|r| r == "Orbit (USA).cue"),
                "{label}: {game:?}"
            );
            let tracks: Vec<_> = entries
                .iter()
                .filter(|e| e.finding == Finding::Matched)
                .collect();
            assert_eq!(tracks.len(), 2, "{label}: {entries:?}");
            if evidence == AuditEvidence::Incremental {
                for t in tracks {
                    let ev = t.evidence.as_ref().unwrap();
                    assert!(
                        ev.level == EvidenceLevel::Payload && ev.reused.is_some(),
                        "{label}: {t:?}"
                    );
                }
            }
        }
    }
}

/// A placed CHD gone from the folder holding it is no healthy CHD a repair leaves: its game, every DAT-exact ROM of it
/// in the source set's folder, is filled there, where it is no longer placed, its review flag cleared, and its kept cue
/// sheet let go.
#[test]
fn a_repair_rebuilds_a_game_whose_placed_chd_is_gone_in_the_source_set() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    // Flagged for review first: a placement dropped needs no review as one.
    let (_, plan, _) = returning_orbit_by(&mut env, &chdman);
    flag_orbit(&env, &plan);
    std::fs::remove_file(env.dir().join("Orbit (USA).chd")).unwrap();
    let set = env.tmp.path().join("roots/sets").join(SET);
    for (n, rom) in cd().into_iter().enumerate() {
        std::fs::write(set.join(format!("stray {n}.bin")), &rom.payload).unwrap();
    }
    env.audit(title_select_engine::AuditEvidence::Full);
    let repair = env
        .store
        .plan_repair(&env.config, SET, &Default::default())
        .unwrap();
    let item = repair
        .content
        .items
        .iter()
        .find(|i| i.game.as_deref() == Some(CD))
        .unwrap_or_else(|| panic!("{:?}", repair.content.items));
    assert_ne!(
        item.chosen,
        title_select_api::repair::RepairChoice::Leave,
        "{item:?}"
    );
    assert!(
        item.note
            .as_deref()
            .is_none_or(|n| !n.contains("placed as a CHD")),
        "{item:?}"
    );
    let approved = env
        .store
        .approve_repair(&env.config, repair.id, &repair.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env
        .store
        .run_repair_job(&env.config, approved.job.unwrap(), None)
        .unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let done = env.store.repair_plan(repair.id).unwrap();
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    // Home in the source set, written there, and no longer placed; its cue sheet goes with the next collection.
    assert!(!env.in_set(CD).is_empty());
    assert!(
        env.store
            .placed_files(SET)
            .unwrap()
            .iter()
            .all(|p| p.game != CD),
        "{:?}",
        env.store.placed_files(SET).unwrap()
    );
    assert!(env.store.review_flags(SET).unwrap().is_empty());
    env.store.collect_garbage().unwrap();
    assert!(env.cues().is_empty());
    let report = env.audit(title_select_engine::AuditEvidence::Full);
    let game = report.games.iter().find(|g| g.name == CD).unwrap();
    assert_eq!(
        game.status,
        title_select_api::report::GameStatus::Complete,
        "{game:?}"
    );
}

/// Flags Orbit for review, placed as a CHD in `folder`, as a return proving its CHD damaged does: its copy chdman reads
/// flipped in its last byte. Its CHD where it is placed is left as it was.
fn flag_orbit(env: &Env, plan: &Plan) {
    let tmp = env.store.dir().join("tmp");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, title_select_store::Fault::ReturnChdCopied(_)) {
                for e in std::fs::read_dir(&tmp).unwrap() {
                    let copy = e.unwrap().path().join("out.chd");
                    if copy.is_file() {
                        let mut bytes = std::fs::read(&copy).unwrap();
                        *bytes.last_mut().unwrap() ^= 0xff;
                        std::fs::write(&copy, &bytes).unwrap();
                    }
                }
            }
        })));
    env.build(plan);
    env.store.hook_faults(None);
    assert_eq!(env.store.review_flags(SET).unwrap().len(), 1);
}

/// A placed CHD's review flag is about that file: a copy of its game found elsewhere, verified, never clears it, while
/// the CHD itself doesn't verify. A Full Audit that verifies the CHD clears it; an Incremental Audit, or an inventory,
/// doesn't.
#[test]
fn a_placed_chds_review_flag_stays_while_another_copy_of_its_game_verifies_until_the_chd_does() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, plan, _) = returning_orbit_by(&mut env, &chdman);
    flag_orbit(&env, &plan);
    // The CHD damaged where it is placed, and a DAT-exact copy of Orbit written in the source set's folder.
    let chd = env.dir().join("Orbit (USA).chd");
    let good = std::fs::read(&chd).unwrap();
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 0xff;
    std::fs::write(&chd, &bad).unwrap();
    let home = env.tmp.path().join("roots/sets").join(SET).join(CD);
    std::fs::create_dir_all(&home).unwrap();
    for rom in cd() {
        std::fs::write(home.join(rom.name), &rom.payload).unwrap();
    }
    let report = env.audit(title_select_engine::AuditEvidence::Full);
    let game = report.games.iter().find(|g| g.name == CD).unwrap();
    assert_eq!(
        game.status,
        title_select_api::report::GameStatus::Complete,
        "{game:?}"
    );
    let (entries, _) = orbit_found(&report, &folder);
    assert!(
        entries
            .iter()
            .any(|e| e.finding == title_select_api::report::Finding::Unreadable),
        "{entries:?}"
    );
    assert_eq!(
        env.store.review_flags(SET).unwrap().len(),
        1,
        "another copy cleared it"
    );

    // The CHD as it was: an Incremental Audit that reads it, and a Quick Inventory, leave the flag; a Full Audit
    // verifies it, and clears it.
    std::fs::write(&chd, &good).unwrap();
    env.audit(title_select_engine::AuditEvidence::Incremental);
    env.audit(title_select_engine::AuditEvidence::Quick);
    assert_eq!(env.store.review_flags(SET).unwrap().len(), 1);
    env.audit(title_select_engine::AuditEvidence::Full);
    assert!(env.store.review_flags(SET).unwrap().is_empty());
}

/// Adds an ingest folder to `env`'s configuration, chdman as `chdman`, and settles `bytes` there as `name`.
fn ingest(env: &mut Env, chdman: &Path, name: &str, bytes: &[u8]) {
    let dir = env.tmp.path().join("in");
    std::fs::create_dir_all(&dir).unwrap();
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
         [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\n[chd]\nchdman = {:?}\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.tmp.path().join("roots/play").display().to_string(),
        dir.display().to_string(),
        chdman.display().to_string(),
    ))
    .unwrap();
    env.store
        .update_settings(
            &env.config,
            &title_select_api::settings::SettingsUpdate {
                ingest_folders: Some(
                    [(
                        "in".to_owned(),
                        title_select_api::settings::IngestCadence {
                            settle_secs: 0,
                            scan_every_minutes: None,
                        },
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            "Operator",
        )
        .unwrap();
    env.store.register_ingest_folder(&env.config, "in").unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
    for _ in 0..2 {
        let job = env.store.enqueue_ingest_scan("in", "Operator").unwrap();
        env.store.claim_job().unwrap().unwrap();
        let ended = env.store.run_ingest_scan_job(&env.config, job.id).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    }
}

/// A disc placed as a CHD is never placed as files of two formats: a repair of one whose CHD is there but holds a track
/// of other bytes, only that track in an ingest folder, either leaves it as it is, nothing written beside it, or
/// releases it whole, the track left where it is: a part of the disc is never written beside or in place of its CHD.
#[test]
fn a_repair_never_completes_a_damaged_placed_chd_with_a_file_of_another_format() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    let made = tempfile::tempdir().unwrap();
    let other = orbit_with_another_track(&chdman, made.path());
    std::fs::copy(&other, env.dir().join("Orbit (USA).chd")).unwrap();
    let track = cd()
        .into_iter()
        .find(|r| r.name.ends_with("(Track 1).bin"))
        .unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    ingest(&mut env, &chdman, "track.bin", &track.payload);
    env.audit(title_select_engine::AuditEvidence::Full);
    let orbit_files = |env: &Env| -> Vec<String> {
        let mut files: Vec<String> = tree(&env.dir())
            .into_iter()
            .chain(tree(&env.tmp.path().join("roots/sets").join(SET)))
            .filter(|f| f.contains("Orbit"))
            .collect();
        files.sort();
        files.dedup();
        files
    };

    for choice in [
        title_select_api::repair::RepairChoice::Leave,
        title_select_api::repair::RepairChoice::KeepAside,
    ] {
        let request = title_select_api::repair::PlanRepair {
            choices: [(format!("game:{CD}"), choice)].into(),
            ..Default::default()
        };
        let repair = env.store.plan_repair(&env.config, SET, &request).unwrap();
        let item = repair
            .content
            .items
            .iter()
            .find(|i| i.key == format!("game:{CD}"))
            .unwrap_or_else(|| panic!("{:#?}", repair.content.items));
        assert_eq!(
            item.kind,
            title_select_api::repair::RepairKind::KeepAside,
            "only a part of the disc is there to fill it, so it is released alone: {item:?}"
        );
        assert!(
            repair
                .content
                .items
                .iter()
                .filter(|i| i.key != item.key)
                .all(|i| i.game.as_deref() != Some(CD)
                    && !i.path.as_deref().is_some_and(|p| p.contains("Orbit"))),
            "{:#?}",
            repair.content.items
        );
        if repair
            .content
            .items
            .iter()
            .any(|i| i.chosen != title_select_api::repair::RepairChoice::Leave)
        {
            let approved = env
                .store
                .approve_repair(&env.config, repair.id, &repair.digest, "Operator", WORKERS)
                .unwrap();
            env.store.claim_job().unwrap().unwrap();
            let ended = env
                .store
                .run_repair_job(&env.config, approved.job.unwrap(), None)
                .unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
        let placed: Vec<_> = env
            .store
            .placed_files(SET)
            .unwrap()
            .into_iter()
            .filter(|p| p.game == CD)
            .map(|p| (p.path, p.chd))
            .collect();
        match choice {
            title_select_api::repair::RepairChoice::Leave => {
                // Placed as its CHD alone, nothing beside it.
                assert_eq!(placed, [("Orbit (USA).chd".to_owned(), true)]);
                assert_eq!(orbit_files(&env), ["Orbit (USA).chd"]);
            }
            _ => {
                // Released whole: the CHD kept aside, the track left in the ingest folder, nothing of it written.
                assert!(placed.is_empty(), "{placed:?}");
                assert!(orbit_files(&env).is_empty(), "{:?}", orbit_files(&env));
                let kept: Vec<_> = env
                    .store
                    .source_kept_files(SET)
                    .unwrap()
                    .into_iter()
                    .map(|k| k.name)
                    .collect();
                assert_eq!(
                    kept,
                    [title_select_api::storage::placed_path(
                        folder.id,
                        "Orbit (USA).chd"
                    )]
                );
                assert!(env.tmp.path().join("in/track.bin").exists());
            }
        }
        let report = env.audit(title_select_engine::AuditEvidence::Full);
        let game = report.games.iter().find(|g| g.name == CD).unwrap();
        assert_ne!(
            game.status,
            title_select_api::report::GameStatus::Complete,
            "{game:?}"
        );
    }
}

mod release;
