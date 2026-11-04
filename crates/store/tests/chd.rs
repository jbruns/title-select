//! CHD in Play Libraries: a disc Game built to CHD with chdman, verified once by extraction against the DAT, and
//! checked afterwards by its header alone. CHD is never a Source Set's format.
//!
//! The tests that run chdman find it as the store does: `TITLE_SELECT_CHDMAN`, else `chdman` on the `PATH`. Where
//! it is missing they are skipped, except in CI, where they fail.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
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
        Env::with(chdman, "")
    }

    /// As [`Env::new`], with `chd` added to the configuration's `[chd]` table.
    fn with(chdman: Option<&Path>, chd: &str) -> Env {
        Env::setup(chdman, chd, &[])
    }

    /// As [`Env::with`], with the games `zipped` in the source set as ZIPs rather than loose folders.
    fn setup(chdman: Option<&Path>, chd: &str, zipped: &[&str]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        let play = tmp.path().join("roots/play");
        std::fs::create_dir_all(sets.join(SET)).unwrap();
        std::fs::create_dir_all(&play).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = config_with(tmp.path(), chdman, chd);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(&tmp.path().join("in"), "test.dat", "1", &games());
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
            .record_selection(&self.config, job.id, &selection)
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
    config_with(root, chdman, "")
}

fn config_with(root: &Path, chdman: Option<&Path>, chd: &str) -> Config {
    let mut text = format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n",
        root.join("roots/sets").display().to_string(),
        root.join("roots/play").display().to_string(),
    );
    text += &format!("\n[chd]\n{chd}\n");
    if let Some(chdman) = chdman {
        text += &format!("chdman = {:?}\n", chdman.display().to_string());
    }
    Config::parse(&text).unwrap()
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

fn outcome(check: &FolderCheck, target: &str) -> (FileOutcome, bool) {
    let file = check.files.iter().find(|f| f.target == target).unwrap();
    (file.outcome, file.hashed)
}

/// A shell script standing in for chdman: `body` runs with its arguments.
#[cfg(unix)]
fn fake_chdman(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-chdman");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    let fake = fake_chdman(
        root.path(),
        &format!(
            "case \"$1\" in\n\
             createcd) printf 'MComprHD' > out.chd ;;\n\
             extractcd) : > x/x.cue; head -c {} /dev/zero > x/t1.bin; head -c {} /dev/zero > x/t2.bin ;;\n\
             *) exit 2 ;;\n\
             esac",
            2352 * 300,
            2352 * 200
        ),
    );
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
    let fake = fake_chdman(root.path(), "exec sleep 30");
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[CD]);
    let plan = env.plan(&folder, selection);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let started = std::time::Instant::now();
    let ended = std::thread::scope(|scope| {
        let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
        while env.chd_scratch().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        env.store.cancel_job(job.id).unwrap();
        running.join().unwrap().unwrap()
    });

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "chdman was left to run"
    );
    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
    assert!(env.chd_scratch().is_empty(), "{:?}", env.chd_scratch());
    assert!(!env.dir().join("Orbit (USA).chd").exists());
    let build = env.store.library_folder(&env.config, folder.id).unwrap();
    let detail = env.store.build(build.unfinished.unwrap().build).unwrap();
    assert_eq!(detail.items[0].state, ItemState::Pending, "{detail:?}");
}

#[test]
fn a_disc_zipped_in_its_source_set_is_read_once_for_all_its_tracks() {
    let Some(_) = chdman() else { return };
    let env = Env::setup(None, "", &[CD]);
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

/// A chdman that runs `real`, except as `disc2` says for Disc 2 of Saga: its shell commands, run in place of
/// chdman's createcd, or after its extractcd, in the scratch directory.
#[cfg(unix)]
fn saga_chdman(dir: &Path, real: &Path, create2: &str, extract2: &str) -> PathBuf {
    fake_chdman(
        dir,
        &format!(
            "real={real:?}\n\
             case \"$1:$3\" in\n\
             createcd:*\"Disc 2\"*) touch disc2; {create2} ;;\n\
             extractcd:*) \"$real\" \"$@\" || exit $?; if [ -f disc2 ]; then {extract2}; fi ;;\n\
             *) exec \"$real\" \"$@\" ;;\n\
             esac",
            real = real.display().to_string()
        ),
    )
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

/// Which formats Saga's discs are in the folder as.
fn saga_files(env: &Env) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(env.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("Saga"))
        .collect();
    found.sort();
    found
}

const SAGA_ZIPS: [&str; 2] = ["Saga (USA) (Disc 1).zip", "Saga (USA) (Disc 2).zip"];
const SAGA_CHDS: [&str; 2] = ["Saga (USA) (Disc 1).chd", "Saga (USA) (Disc 2).chd"];

#[cfg(unix)]
#[test]
fn when_one_disc_fails_to_verify_no_disc_of_its_game_changes_format() {
    let Some(real) = chdman() else { return };
    let root = tempfile::tempdir().unwrap();
    // Disc 2's CHD gives back other bytes.
    let fake = saga_chdman(
        root.path(),
        &real,
        "exec \"$real\" \"$@\"",
        "printf XXXX | dd of=x/t1.bin conv=notrunc 2>/dev/null",
    );
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
    let fake = saga_chdman(
        root.path(),
        &real,
        &format!("touch {:?}; exec sleep 30", slow.display().to_string()),
        "true",
    );
    let env = Env::new(Some(&fake));
    let (folder, _, plan) = saga_converting(&env);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = std::thread::scope(|scope| {
        let running = scope.spawn(|| env.store.run_build_job(&env.config, job.id, None));
        while !slow.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        env.store.cancel_job(job.id).unwrap();
        running.join().unwrap().unwrap()
    });

    assert_eq!(ended.state, JobState::Interrupted, "{ended:?}");
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
    assert_eq!(saga_files(&env), [SAGA_ZIPS[0], SAGA_CHDS[1], SAGA_ZIPS[1]]);

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
    let ran = root.path().join("ran");
    let fake = fake_chdman(
        root.path(),
        &format!("touch {:?}; exit 1", ran.display().to_string()),
    );
    let env = Env::new(Some(&fake));
    let (folder, selection) = env.folder(&[ECHO]);
    let ended = env.build(&env.plan(&folder, selection));

    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert!(!ran.exists(), "chdman ran");
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
    let fake = fake_chdman(root.path(), "exec sleep 30");
    let env = Env::with(
        Some(&fake),
        "timeout_seconds = 1\ntimeout_seconds_per_gib = 0",
    );
    let (folder, selection) = env.folder(&[CD]);
    let started = std::time::Instant::now();
    let ended = env.build(&env.plan(&folder, selection));

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "chdman was left to run"
    );
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
