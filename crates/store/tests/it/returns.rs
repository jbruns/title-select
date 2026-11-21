//! Single Copy (ADR 0023): Builds that return a Library Folder's Placed Games to their Source Set's folder, by a rename,
//! a copy or a conversion back to the Source Set's format, and recovery from a stop at each step, from synthetic DATs,
//! Source Sets and files.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::common::{RomSpec, rom};
use crate::single_copy::*;
use title_select_api::build::{BlockerKind, ItemKind, ItemState, Plan, PlanReturn, ReturnHow};
use title_select_api::storage::StorageFormat;
use title_select_api::{AuditEvidence, Finding, JobState};
use title_select_store::Fault;

/// The plan's return of `release`.
fn returned<'a>(plan: &'a Plan, release: &str) -> &'a PlanReturn {
    plan.content
        .returns
        .iter()
        .find(|r| r.release == release)
        .unwrap_or_else(|| panic!("no return of {release}: {:?}", plan.content))
}

/// Places Pong in the folder, from a TorrentZip in the Source Set, the folder storing each game as `format`; then
/// audits the Source Set again, so that the latest Audit names it where it is placed.
fn place_pong(env: &Env, format: StorageFormat) -> Vec<u8> {
    let bytes = pong_zip(env);
    env.audit(AuditEvidence::Full);
    env.single_copy(format);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    env.audit(AuditEvidence::Full);
    bytes
}

/// Whether Pong is home, and only home: in the Source Set's folder as `bytes`, not in the folder, and no longer placed.
fn home(env: &Env, name: &str, bytes: &[u8]) {
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    let (set, library) = env.files();
    assert_eq!(set, [name], "{library:?}");
    assert!(
        !library.iter().any(|f| f.starts_with("Pong")),
        "{library:?}"
    );
    assert_eq!(std::fs::read(env.set().join(name)).unwrap(), bytes);
}

#[test]
fn a_deselected_placed_game_is_converted_back_to_torrentzip_verified_and_returned_home() {
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::Loose);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).a26".into()])
    );
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert_eq!(r.how, ReturnHow::Convert);
    assert_eq!(r.games, ["Pong (USA)"]);
    assert_eq!(r.files.len(), 1);
    assert_eq!(r.files[0].path, "Pong (USA).zip");
    assert!(r.needs >= bytes.len() as u64, "{r:?}");
    assert!(r.reason.contains("no longer selected"), "{}", r.reason);
    // Nothing of the folder keeps it aside.
    assert!(
        plan.content
            .removals
            .iter()
            .all(|r| r.target != "Pong (USA).a26"),
        "{:?}",
        plan.content.removals
    );
    assert_eq!(plan.summary.returned, 1);
    assert_eq!(plan.summary.return_needs, r.needs);

    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let it = env.item(job.id, "Pong (USA)");
    assert_eq!(
        (it.kind, it.state),
        (ItemKind::Return, ItemState::Applied),
        "{it:?}"
    );
    // TorrentZip is canonical: the very bytes the Source Set held before it was placed.
    home(&env, "Pong (USA).zip", &bytes);
    assert!(
        env.store.built_files(env.folder.id).unwrap().is_empty(),
        "nothing is left built in the folder"
    );
    // The Audit names it home, and so does the next.
    let next = env.audit(AuditEvidence::Incremental);
    let entry = next
        .entries
        .iter()
        .find(|e| e.path == "Pong (USA).zip")
        .unwrap_or_else(|| panic!("{:?}", next.entries));
    assert_eq!(entry.finding, Finding::Matched);
    assert!(
        next.entries
            .iter()
            .all(|e| !e.path.starts_with(".title-select")),
        "{:?}",
        next.entries
    );
    // The folder holds nothing placed, so it can be unregistered again.
    assert!(env.store.holding_folder(SET).unwrap().is_some());
}

#[test]
fn a_deselected_placed_game_on_one_filesystem_is_renamed_home_with_its_evidence() {
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::TorrentZip);
    let placed = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert_eq!((r.how, r.needs), (ReturnHow::Rename, 0));

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    home(&env, "Pong (USA).zip", &bytes);
    let after = std::fs::metadata(env.set().join("Pong (USA).zip")).unwrap();
    assert_eq!((after.dev(), after.ino()), (placed.dev(), placed.ino()));
    let next = env.audit(AuditEvidence::Incremental);
    let entry = next
        .entries
        .iter()
        .find(|e| e.path == "Pong (USA).zip")
        .unwrap();
    assert!(
        entry.evidence.as_ref().unwrap().reused.is_some(),
        "{entry:?}"
    );
}

#[test]
fn across_filesystems_a_deselected_placed_game_is_copied_home_verified_and_its_placed_file_deleted()
{
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let placed = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert_eq!(r.how, ReturnHow::Copy);
    assert!(r.needs >= bytes.len() as u64);

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    home(&env, "Pong (USA).zip", &bytes);
    let after = std::fs::metadata(env.set().join("Pong (USA).zip")).unwrap();
    assert_ne!(after.ino(), placed.ino(), "a copy");
    assert!(
        private_files(&env.library()).is_empty(),
        "{:?}",
        private_files(&env.library())
    );
    assert!(
        private_files(&env.set()).is_empty(),
        "{:?}",
        private_files(&env.set())
    );
}

#[test]
fn turning_single_copy_off_moves_nothing_and_the_next_plan_returns_every_placed_game_keeping_a_copy_here()
 {
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::TorrentZip);
    let before = env.files();
    let selection = env.select(&["Pong (USA)"]);

    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    // Saving the setting moves nothing.
    assert_eq!(env.files(), before);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    // Still selected here, so the folder keeps its file as a built copy, and the source set gets its own: a copy.
    assert_eq!(r.how, ReturnHow::Copy);
    assert!(r.placed.iter().all(|p| p.keeps), "{r:?}");
    assert!(r.reason.contains("single copy is off"), "{}", r.reason);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Unchanged);

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(
        env.files(),
        (vec!["Pong (USA).zip".into()], vec!["Pong (USA).zip".into()])
    );
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read(env.library().join("Pong (USA).zip")).unwrap(),
        bytes
    );
    let built = env.store.built_files(env.folder.id).unwrap();
    assert_eq!(built.len(), 1, "{built:?}");
    // No longer the only copy, but an ordinary copy built here.
    assert_eq!(built[0].kind, ItemKind::Copy, "{built:?}");
    // The next plan finds it built here and copied, nothing left to return.
    let again = env.plan(&env.folder, selection);
    assert!(
        again.content.returns.is_empty(),
        "{:?}",
        again.content.returns
    );
    assert_eq!(item(&again, "Pong (USA)").kind, ItemKind::Unchanged);
}

#[test]
fn a_game_still_selected_and_stored_loose_here_is_converted_home_and_kept_as_an_ordinary_built_file()
 {
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert_eq!(r.how, ReturnHow::Convert);
    assert!(r.placed.iter().all(|p| p.keeps), "{r:?}");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        bytes
    );
    // Written from its ROMs here, as a repack is.
    let built = env.store.built_files(env.folder.id).unwrap();
    assert_eq!(
        built
            .iter()
            .map(|b| (b.target.as_str(), b.kind))
            .collect::<Vec<_>>(),
        [("Pong (USA).a26", ItemKind::Repack)],
        "{built:?}"
    );
    let again = env.plan(&env.folder, selection);
    assert!(again.content.returns.is_empty());
    assert_eq!(item(&again, "Pong (USA)").kind, ItemKind::Unchanged);
}

#[test]
fn naming_another_holding_folder_returns_the_games_home_then_the_new_folder_places_them() {
    let env = Env::new();
    let second = add_folder(&env.store, &env.config, "atari-2");
    env.store
        .set_holding_folder(SET, Some(env.folder.id))
        .unwrap();
    let bytes = place_pong(&env, StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);

    env.store.set_holding_folder(SET, Some(second.id)).unwrap();
    // Naming it moves nothing.
    assert_eq!(
        env.store.placed_files(SET).unwrap()[0].library_folder,
        env.folder.id
    );

    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert!(r.reason.contains("no longer holds"), "{}", r.reason);
    assert_eq!(r.how, ReturnHow::Copy, "it stays built here, as a copy");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(names(&env.set()), ["Pong (USA).zip"]);
    let built = env.store.built_files(env.folder.id).unwrap();
    assert_eq!(
        built.iter().map(|b| b.kind).collect::<Vec<_>>(),
        [ItemKind::Copy],
        "{built:?}"
    );
    env.audit(AuditEvidence::Incremental);

    // There is no move between the folders: the new one places the game from the source set's folder.
    let placing = env.plan(&second, selection);
    assert_eq!(item(&placing, "Pong (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&placing).state, JobState::Succeeded);
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].library_folder, second.id);
    assert!(names(&env.set()).is_empty());
    assert_eq!(
        std::fs::read(env.tmp.path().join("roots/play/atari-2/Pong (USA).zip")).unwrap(),
        bytes
    );
    // The first folder builds by copying now, and keeps its copy.
    assert_eq!(names(&env.library()), ["Pong (USA).zip"]);
}

#[test]
fn a_return_that_wont_fit_in_the_source_sets_root_is_refused_before_it_runs() {
    let env = Env::new();
    place_pong(&env, StorageFormat::Loose);
    let selection = env.select(&["Combat (USA)"]);
    let before = env.files();

    env.store.pretend_free_space(Some(1000));
    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pong (USA)");
    assert!(r.needs > 1000, "{r:?}");
    let space = plan
        .content
        .blockers
        .iter()
        .find(|b| b.kind == BlockerKind::Space)
        .unwrap_or_else(|| panic!("{:?}", plan.content.blockers));
    assert!(
        space
            .message
            .contains(&format!("needs up to {} bytes in root sets", r.needs))
            && space.message.contains("it has 1000 bytes free"),
        "{}",
        space.message
    );
    assert!(
        env.store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .is_err()
    );
    assert_eq!(env.files(), before);

    // Space that runs out after the plan is approved refuses the build before it changes anything.
    env.store.pretend_free_space(None);
    let plan = env.plan(&env.folder, selection);
    assert!(
        plan.content.blockers.is_empty(),
        "{:?}",
        plan.content.blockers
    );
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.pretend_free_space(Some(1000));
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);
    let ran = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(ran.state, JobState::Failed, "{ran:?}");
    assert!(
        ran.error
            .as_deref()
            .is_some_and(|e| e.contains("the plan changed")),
        "{ran:?}"
    );
    assert_eq!(env.files(), before);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn once_every_placed_game_is_home_the_folder_and_the_source_set_can_be_unregistered() {
    let env = Env::new();
    place_pong(&env, StorageFormat::TorrentZip);
    assert!(
        env.store
            .unregister_library_folder(&env.config, env.folder.id)
            .is_err()
    );
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    env.store
        .unregister_library_folder(&env.config, env.folder.id)
        .unwrap();
    env.store.unregister_source_set(&env.config, SET).unwrap();
}

#[test]
fn a_source_set_is_locked_while_a_build_returning_its_games_is_queued() {
    let env = Env::new();
    place_pong(&env, StorageFormat::TorrentZip);
    torrentzip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.audit(AuditEvidence::Full);
    let second = add_folder(&env.store, &env.config, "atari-2");
    env.store.set_holding_folder(SET, Some(second.id)).unwrap();
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(plan.content.returns.len(), 1);
    // The new holding folder places Combat, which is in the source set's folder.
    let placing = env.plan(&second, selection);
    assert_eq!(item(&placing, "Combat (USA)").kind, ItemKind::Place);
    env.store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    let audit = env
        .store
        .audit_source_set(&env.config, SET, None, AuditEvidence::Full);
    assert!(
        matches!(&audit, Err(title_select_store::Error::Conflict(why)) if why.contains("returns them")),
        "{:?}",
        audit.map(|_| ())
    );
    let holding = env.store.set_holding_folder(SET, Some(env.folder.id));
    assert!(
        matches!(&holding, Err(title_select_store::Error::Conflict(_))),
        "{holding:?}"
    );
    // That plan waits for the return queued: it can't be approved while it is.
    let approved = env.store.approve_plan(
        &env.config,
        placing.id,
        &placing.digest,
        "Operator",
        WORKERS,
    );
    assert!(
        matches!(&approved, Err(title_select_store::Error::Conflict(why)) if why.contains("changes source set")),
        "{:?}",
        approved.map(|_| ())
    );
}

/// Runs the build of `plan` until `stop` says to stop it, as a kill would, then restarts the service, which recovers
/// what the stop left: Pong then has exactly one recorded copy, placed here as `placed` or home as `home`, and nothing
/// of it is left set aside or staged. Then the next build finishes returning it. Returns the restarted store.
fn stop_and_recover(
    env: Env,
    plan: &Plan,
    stop: impl Fn(Fault) -> bool + Send + Sync + 'static,
    (placed, home_name, bytes): (&str, &str, &[u8]),
    label: &str,
) -> Env {
    env.build_until(plan, stop);
    let how = plan.content.returns[0].how;
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{label}: {blocked:?}");
    env.store.copy_across_filesystems(how == ReturnHow::Copy);
    one_copy(&env, (placed, home_name), label);
    env.audit(AuditEvidence::Incremental);
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    assert!(
        again.content.blockers.is_empty(),
        "{label}: {:?}",
        again.content.blockers
    );
    if !env.store.placed_files(SET).unwrap().is_empty() {
        let r = returned(&again, "Pong (USA)");
        assert_eq!(r.how, how, "{label}: {:?}", again.content.notes);
    }
    let job = env.build(&again);
    assert_eq!(job.state, JobState::Succeeded, "{label}");
    home(&env, home_name, bytes);
    env
}

/// Whether Pong has exactly one recorded copy: placed here as `placed`, its file there, and nothing of it home; or
/// home as `home_name`, nothing of it here, and no longer placed. Nothing is left in either folder's private area.
fn one_copy(env: &Env, (placed, home_name): (&str, &str), label: &str) {
    let rows = env.store.placed_files(SET).unwrap();
    let (set, library) = env.files();
    match rows.as_slice() {
        [] => {
            assert_eq!(set, [home_name], "{label}: home, {library:?}");
            assert!(
                !library.iter().any(|f| f.starts_with("Pong")),
                "{label}: {library:?}"
            );
        }
        [row] => {
            assert_eq!(row.path, placed, "{label}");
            assert!(set.is_empty(), "{label}: still placed, {set:?}");
            let st = std::fs::metadata(env.library().join(placed)).unwrap();
            assert_eq!(
                (st.dev(), st.ino()),
                (row.dev, row.ino),
                "{label}: the file recorded"
            );
        }
        rows => panic!("{label}: {rows:?}"),
    }
    assert!(
        private_files(&env.library()).is_empty(),
        "{label}: {:?}",
        private_files(&env.library())
    );
    assert!(
        private_files(&env.set()).is_empty(),
        "{label}: {:?}",
        private_files(&env.set())
    );
}

/// A Fault point a test stops the service at, by name.
type Stop = (&'static str, fn(Fault) -> bool);

#[test]
fn a_rename_home_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    let stops: [Stop; 3] = [
        ("renaming", |f| matches!(f, Fault::ReturnRenaming(_))),
        ("moved", |f| matches!(f, Fault::ReturnMoved(_))),
        ("recorded", |f| matches!(f, Fault::ReturnRecorded(_))),
    ];
    for (label, stop) in stops {
        let env = Env::new();
        let bytes = place_pong(&env, StorageFormat::TorrentZip);
        let selection = env.select(&["Combat (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Rename);
        stop_and_recover(
            env,
            &plan,
            stop,
            ("Pong (USA).zip", "Pong (USA).zip", &bytes),
            label,
        );
    }
}

const WRITE_STOPS: [Stop; 6] = [
    ("staged", |f| matches!(f, Fault::ReturnStaged(_))),
    ("published", |f| matches!(f, Fault::ReturnPublished(_))),
    ("set aside", |f| matches!(f, Fault::ReturnSetAside(_))),
    ("checking", |f| matches!(f, Fault::ReturnChecking(_))),
    ("recorded", |f| matches!(f, Fault::ReturnRecorded(_))),
    ("disposed", |f| matches!(f, Fault::ReturnDisposed(_))),
];

#[test]
fn a_copy_home_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    for (label, stop) in WRITE_STOPS {
        let env = Env::new();
        let bytes = place_pong(&env, StorageFormat::TorrentZip);
        env.store.copy_across_filesystems(true);
        let selection = env.select(&["Combat (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Copy);
        stop_and_recover(
            env,
            &plan,
            stop,
            ("Pong (USA).zip", "Pong (USA).zip", &bytes),
            label,
        );
    }
}

#[test]
fn a_conversion_home_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    for (label, stop) in WRITE_STOPS {
        let env = Env::new();
        let bytes = place_pong(&env, StorageFormat::Loose);
        let selection = env.select(&["Combat (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Convert);
        stop_and_recover(
            env,
            &plan,
            stop,
            ("Pong (USA).a26", "Pong (USA).zip", &bytes),
            label,
        );
    }
}

/// Approves `plan` and runs its Build Job, running `meddle` once, at the first Fault point `at` says to.
fn build_meddling(
    env: &Env,
    plan: &Plan,
    at: fn(Fault) -> bool,
    meddle: impl FnOnce() + Send + 'static,
) -> title_select_api::Job {
    let meddle = std::sync::Mutex::new(Some(meddle));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if at(fault)
                && let Some(meddle) = meddle.lock().unwrap().take()
            {
                meddle();
            }
        })));
    let job = env.build(plan);
    env.store.hook_faults(None);
    job
}

/// Places Pong as a TorrentZip, and plans its return home by a copy, Pong no longer selected.
fn copying_pong_home(env: &Env) -> (Vec<u8>, Plan) {
    let bytes = place_pong(env, StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Copy);
    (bytes, plan)
}

/// Whether Pong is still placed, as `bytes`, as it was before `job` tried to return it, nothing of it home, and its
/// item failed saying why, with `why` in it.
fn still_placed(env: &Env, job: &title_select_api::Job, bytes: &[u8], why: &str) {
    let it = env.item(job.id, "Pong (USA)");
    assert_eq!(it.state, ItemState::Failed, "{it:?}");
    let error = it.error.clone().unwrap_or_default();
    assert!(
        error.contains(why) && error.contains("stays placed"),
        "{error}"
    );
    let rows = env.store.placed_files(SET).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (set, library) = env.files();
    assert!(set.is_empty(), "{set:?}");
    assert_eq!(library, ["Pong (USA).zip"]);
    let placed = env.library().join("Pong (USA).zip");
    assert_eq!(std::fs::read(&placed).unwrap(), bytes);
    let st = std::fs::metadata(&placed).unwrap();
    assert_eq!((st.dev(), st.ino()), (rows[0].dev, rows[0].ino));
    assert!(
        private_files(&env.library()).is_empty(),
        "{:?}",
        private_files(&env.library())
    );
    assert!(
        private_files(&env.set()).is_empty(),
        "{:?}",
        private_files(&env.set())
    );
}

#[test]
fn a_file_written_home_that_changes_before_the_last_check_leaves_the_game_placed() {
    let env = Env::new();
    let (bytes, plan) = copying_pong_home(&env);
    let written = env.set().join("Pong (USA).zip");
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnChecking(_)),
        move || overwrite_keeping_stamp(&written),
    );
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    still_placed(&env, &job, &bytes, "changed");
    // A check of the folder still trusts the placed file: put back, it was read again and recorded as found.
    env.audit(AuditEvidence::Incremental);
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    assert_eq!(returned(&again, "Pong (USA)").how, ReturnHow::Copy);
    assert_eq!(env.build(&again).state, JobState::Succeeded);
    home(&env, "Pong (USA).zip", &bytes);
}

#[test]
fn a_source_set_folder_replaced_before_the_last_check_leaves_the_game_placed() {
    let env = Env::new();
    let (bytes, plan) = copying_pong_home(&env);
    let set = env.set();
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnChecking(_)),
        move || replace_with_marked_copy(&set, &set),
    );
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    still_placed(&env, &job, &bytes, "source set");
    // What was written home went with the folder replaced, and was removed from it.
    let moved = PathBuf::from(format!("{}.moved", env.set().display()));
    assert!(names(&moved).is_empty(), "{:?}", names(&moved));
}

#[test]
fn a_placed_file_that_changes_while_set_aside_is_put_back_and_never_deleted() {
    let env = Env::new();
    let (bytes, plan) = copying_pong_home(&env);
    let library = env.library();
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnChecking(_)),
        move || {
            let aside = private_files(&library);
            assert_eq!(aside.len(), 1, "{aside:?}");
            assert!(aside[0].contains(".deleting/"), "{aside:?}");
            overwrite_keeping_stamp(&library.join(&aside[0]));
        },
    );
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let it = env.item(job.id, "Pong (USA)");
    assert_eq!(it.state, ItemState::Failed, "{it:?}");
    // Put back as it is now, and recorded as it was: a check flags it, and nothing deleted it.
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    let (set, library) = env.files();
    assert!(set.is_empty(), "{set:?}");
    assert_eq!(library, ["Pong (USA).zip"]);
    let now = std::fs::read(env.library().join("Pong (USA).zip")).unwrap();
    assert_eq!(now.len(), bytes.len());
    assert_ne!(now, bytes);
    assert!(private_files(&env.library()).is_empty());
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    assert!(
        again.content.returns.is_empty(),
        "{:?}",
        again.content.returns
    );
    assert!(
        again
            .content
            .notes
            .iter()
            .any(|n| n.contains("Pong (USA) stays placed here")),
        "{:?}",
        again.content.notes
    );
}

#[test]
fn a_file_put_in_place_of_the_placed_file_before_its_rename_home_is_put_back_and_the_game_stays_placed()
 {
    let env = Env::new();
    let bytes = place_pong(&env, StorageFormat::TorrentZip);
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Rename);
    let placed = env.library().join("Pong (USA).zip");
    let elsewhere = env.tmp.path().join("Pong (USA).zip.elsewhere");
    let (from, to) = (placed.clone(), elsewhere.clone());
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnRenaming(_)),
        move || {
            std::fs::rename(&from, &to).unwrap();
            std::fs::write(&from, b"another file").unwrap();
        },
    );
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let it = env.item(job.id, "Pong (USA)");
    assert_eq!(it.state, ItemState::Failed, "{it:?}");
    // What took its name was put back, and nothing went home.
    let (set, library) = env.files();
    assert!(set.is_empty(), "{set:?}");
    assert_eq!(library, ["Pong (USA).zip"]);
    assert_eq!(std::fs::read(&placed).unwrap(), b"another file");
    assert_eq!(std::fs::read(&elsewhere).unwrap(), bytes);
    // Still recorded as the file placed, which a check now finds gone.
    let rows = env.store.placed_files(SET).unwrap();
    let st = std::fs::metadata(&elsewhere).unwrap();
    assert_eq!((rows[0].dev, rows[0].ino), (st.dev(), st.ino()));
}

#[test]
fn a_rollback_that_cant_put_the_placed_file_back_leaves_the_return_outstanding_until_it_can() {
    let env = Env::new();
    let (bytes, plan) = copying_pong_home(&env);
    let (home, placed) = (
        env.set().join("Pong (USA).zip"),
        env.library().join("Pong (USA).zip"),
    );
    let occupant = placed.clone();
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnChecking(_)),
        move || {
            overwrite_keeping_stamp(&home);
            std::fs::write(&occupant, b"in the way").unwrap();
        },
    );
    assert_ne!(job.state, JobState::Succeeded, "{job:?}");
    assert!(
        job.error.as_deref().is_some_and(|e| e.contains("put back")),
        "{job:?}"
    );
    let it = env.item(job.id, "Pong (USA)");
    assert!(
        !matches!(it.state, ItemState::Failed | ItemState::Applied),
        "{it:?}"
    );
    // Still recorded where it was placed, and set aside, not deleted.
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    let aside = private_files(&env.library());
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(std::fs::read(env.library().join(&aside[0])).unwrap(), bytes);
    // Nothing else changes the source set's folder until it is finished.
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    let refused = env
        .store
        .approve_plan(&env.config, again.id, &again.digest, "Operator", WORKERS);
    assert!(refused.is_err(), "{refused:?}");

    std::fs::remove_file(&placed).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    env.store.copy_across_filesystems(true);
    one_copy(&env, ("Pong (USA).zip", "Pong (USA).zip"), "recovered");
    assert_eq!(
        std::fs::read(env.library().join("Pong (USA).zip")).unwrap(),
        bytes
    );
}

/// A DAT whose Pitfall has two ROMs, which a Source Set storing games as loose folders holds as two files.
fn two_roms() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (
            "Pitfall (USA)",
            vec![
                rom("Pitfall (USA).a26", b"pitfall"),
                rom("Pitfall (USA).bin", b"harry"),
            ],
        ),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

const PITFALL: [&str; 2] = [
    "Pitfall (USA)/Pitfall (USA).a26",
    "Pitfall (USA)/Pitfall (USA).bin",
];

/// Places Pitfall, held in the Source Set as a loose folder of two files, in the folder as one TorrentZip; then plans
/// its return, no longer selected: converted back to the two files.
fn placing_pitfall(env: &Env) -> Plan {
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = StorageFormat::LooseFolder;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    std::fs::create_dir_all(env.set().join("Pitfall (USA)")).unwrap();
    std::fs::write(env.set().join(PITFALL[0]), b"pitfall").unwrap();
    std::fs::write(env.set().join(PITFALL[1]), b"harry").unwrap();
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pitfall (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pitfall (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    assert!(pitfall_away(env));
    env.audit(AuditEvidence::Full);
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pitfall (USA)");
    assert_eq!(r.how, ReturnHow::Convert);
    assert_eq!(
        r.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        PITFALL
    );
    plan
}

/// Whether nothing of Pitfall is home, the folder its files were in empty if its placement left it, and only its
/// TorrentZip is here.
fn pitfall_away(env: &Env) -> bool {
    let (set, library) = env.files();
    let folder = env.set().join("Pitfall (USA)");
    library == ["Pitfall (USA).zip"]
        && set.iter().all(|f| f == "Pitfall (USA)")
        && (!folder.exists() || names(&folder).is_empty())
}

/// Whether Pitfall is home whole, as its two files, and nothing of it is left here.
fn pitfall_home(env: &Env) {
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(
        std::fs::read(env.set().join(PITFALL[0])).unwrap(),
        b"pitfall"
    );
    assert_eq!(std::fs::read(env.set().join(PITFALL[1])).unwrap(), b"harry");
    assert!(env.files().1.is_empty(), "{:?}", env.files());
    assert!(private_files(&env.library()).is_empty());
    assert!(private_files(&env.set()).is_empty());
}

#[test]
fn a_release_of_several_files_is_returned_whole() {
    let env = Env::with(&two_roms());
    let plan = placing_pitfall(&env);
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.item(job.id, "Pitfall (USA)").state, ItemState::Applied);
    pitfall_home(&env);
    // The audit names both files home.
    let report = env.audit(AuditEvidence::Incremental);
    for path in PITFALL {
        let entry = report.entries.iter().find(|e| e.path == path);
        assert_eq!(
            entry.map(|e| &e.finding),
            Some(&Finding::Matched),
            "{:?}",
            report.entries
        );
    }
}

#[test]
fn a_release_of_several_files_stopped_between_them_is_returned_whole_or_not_at_all() {
    let stops: [Stop; 2] = [
        ("staged", |f| matches!(f, Fault::ReturnStaged(_))),
        ("published", |f| matches!(f, Fault::ReturnPublished(_))),
    ];
    for (label, at) in stops {
        let env = Env::with(&two_roms());
        let plan = placing_pitfall(&env);
        // Stopped after the first of its two files, before the second.
        let seen = std::sync::atomic::AtomicUsize::new(0);
        env.build_until(&plan, move |f| {
            at(f) && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
        });
        let (env, blocked) = env.restart();
        assert!(blocked.is_empty(), "{label}: {blocked:?}");
        assert_eq!(env.store.placed_files(SET).unwrap().len(), 1, "{label}");
        assert!(pitfall_away(&env), "{label}: {:?}", env.files());
        assert!(private_files(&env.library()).is_empty(), "{label}");
        assert!(private_files(&env.set()).is_empty(), "{label}");
        env.audit(AuditEvidence::Incremental);
        let selection = env.select(&["Combat (USA)"]);
        let again = env.plan(&env.folder, selection);
        assert_eq!(env.build(&again).state, JobState::Succeeded, "{label}");
        pitfall_home(&env);
    }
}

/// Whether Pong is still placed, recorded as the file placed, nothing of it home, and the Source Set's latest Audit
/// names nothing of it home either: no Evidence was carried there for it.
fn pong_left_placed(env: &Env, label: &str) {
    let rows = env.store.placed_files(SET).unwrap();
    assert_eq!(rows.len(), 1, "{label}: {rows:?}");
    let (set, library) = env.files();
    assert!(set.is_empty(), "{label}: {set:?}");
    assert_eq!(library, ["Pong (USA).zip"], "{label}");
    let st = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    assert_eq!((st.dev(), st.ino()), (rows[0].dev, rows[0].ino), "{label}");
    let report = env.audit(AuditEvidence::Incremental);
    assert!(
        report.entries.iter().all(|e| e.path != "Pong (USA).zip"),
        "{label}: {:?}",
        report.entries
    );
    // Changed, it is flagged, and stays placed.
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    assert!(
        again.content.returns.is_empty(),
        "{label}: {:?}",
        again.content.returns
    );
    assert!(
        again
            .content
            .notes
            .iter()
            .any(|n| n.contains("Pong (USA) stays placed here")),
        "{label}: {:?}",
        again.content.notes
    );
}

#[test]
fn a_file_changed_just_after_its_rename_home_is_never_taken_as_home_with_its_evidence() {
    for stop in [false, true] {
        let label = if stop { "recovered" } else { "checked" };
        let env = Env::new();
        place_pong(&env, StorageFormat::TorrentZip);
        let selection = env.select(&["Combat (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Rename);
        // Changed home, the same size and modification time, right after its rename.
        let home = env.set().join("Pong (USA).zip");
        let env = match stop {
            false => {
                let job = build_meddling(
                    &env,
                    &plan,
                    |f| matches!(f, Fault::ReturnMoved(_)),
                    move || overwrite_keeping_stamp(&home),
                );
                assert_eq!(job.state, JobState::Succeeded, "{job:?}");
                let it = env.item(job.id, "Pong (USA)");
                assert_eq!(it.state, ItemState::Failed, "{it:?}");
                env
            }
            true => {
                env.build_until(&plan, move |f| {
                    let at = matches!(f, Fault::ReturnMoved(_));
                    if at {
                        overwrite_keeping_stamp(&home);
                    }
                    at
                });
                let (env, blocked) = env.restart();
                assert!(blocked.is_empty(), "{blocked:?}");
                env
            }
        };
        pong_left_placed(&env, label);
    }
}

/// A DAT whose Pitfall is a game of two discs, one release, which the Source Set holds as two TorrentZips.
fn two_discs() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        (
            "Pitfall (USA) (Disc 1)",
            vec![rom("Pitfall (USA) (Disc 1).a26", b"pitfall")],
        ),
        (
            "Pitfall (USA) (Disc 2)",
            vec![rom("Pitfall (USA) (Disc 2).a26", b"harry")],
        ),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

const DISCS: [&str; 2] = ["Pitfall (USA) (Disc 1).zip", "Pitfall (USA) (Disc 2).zip"];

/// Places both discs of Pitfall in the folder, by renames; then plans their return, no longer selected: renamed
/// back, file by file. Returns the plan and each disc's bytes.
fn renaming_pitfall(env: &Env) -> (Plan, Vec<Vec<u8>>) {
    let mut bytes = Vec::new();
    for (zip, payload) in DISCS.iter().zip([&b"pitfall"[..], b"harry"]) {
        let member = zip.replace(".zip", ".a26");
        torrentzip(&env.set().join(zip), &[(&member, payload)]);
        bytes.push(std::fs::read(env.set().join(zip)).unwrap());
    }
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pitfall (USA) (Disc 1)", "Pitfall (USA) (Disc 2)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pitfall (USA) (Disc 1)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 2);
    env.audit(AuditEvidence::Full);
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let r = returned(&plan, "Pitfall (USA)");
    assert_eq!(r.how, ReturnHow::Rename, "{r:?}");
    assert_eq!(r.placed.len(), 2);
    (plan, bytes)
}

/// Whether Pitfall is still placed whole, both its discs here and none home, and the next plan returns it, by
/// renames, and that build puts it home whole.
fn pitfall_returns_next(env: &Env, bytes: &[Vec<u8>], label: &str) {
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 2, "{label}");
    assert!(env.files().0.is_empty(), "{label}: {:?}", env.files());
    assert_eq!(env.files().1, DISCS, "{label}");
    assert!(private_files(&env.library()).is_empty(), "{label}");
    env.audit(AuditEvidence::Incremental);
    let selection = env.select(&["Combat (USA)"]);
    let again = env.plan(&env.folder, selection);
    let r = again
        .content
        .returns
        .iter()
        .find(|r| r.release == "Pitfall (USA)")
        .unwrap_or_else(|| panic!("{label}: {:?}", again.content.notes));
    assert_eq!(r.how, ReturnHow::Rename, "{label}");
    assert_eq!(env.build(&again).state, JobState::Succeeded, "{label}");
    assert!(env.store.placed_files(SET).unwrap().is_empty(), "{label}");
    for (zip, bytes) in DISCS.iter().zip(bytes) {
        assert_eq!(
            &std::fs::read(env.set().join(zip)).unwrap(),
            bytes,
            "{label}"
        );
    }
}

#[test]
fn a_release_partly_renamed_home_when_the_service_stopped_is_put_back_and_returned_by_the_next_build()
 {
    let env = Env::with(&two_discs());
    let (plan, bytes) = renaming_pitfall(&env);
    // Stopped after the first of its two discs was renamed home.
    env.build_until(&plan, |f| matches!(f, Fault::ReturnMoved(_)));
    assert_eq!(env.files().0.len(), 1, "{:?}", env.files());
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    pitfall_returns_next(&env, &bytes, "recovered");
}

#[test]
fn a_rename_home_rolled_back_by_its_last_check_leaves_the_release_to_return_next() {
    let env = Env::with(&two_discs());
    let (plan, bytes) = renaming_pitfall(&env);
    // The source set's folder replaced as the last file is renamed: the check finds it no longer in place, and puts
    // every file back.
    let set = env.set();
    let seen = std::sync::atomic::AtomicUsize::new(0);
    let meddle = std::sync::Mutex::new(Some(move || replace_with_marked_copy(&set, &set)));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::ReturnMoved(_))
                && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
                && let Some(meddle) = meddle.lock().unwrap().take()
            {
                meddle();
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let items = env.store.build(build.id).unwrap().items;
    let it = items.iter().find(|i| i.kind == ItemKind::Return).unwrap();
    assert_eq!(it.state, ItemState::Failed, "{it:?}");
    // The folder that was replaced goes, and the source set's folder is the one there now.
    std::fs::remove_dir_all(format!("{}.moved", env.set().display())).unwrap();
    pitfall_returns_next(&env, &bytes, "checked");
}

#[test]
fn a_placed_file_changed_just_before_its_rename_home_stays_placed() {
    let env = Env::new();
    place_pong(&env, StorageFormat::TorrentZip);
    let selection = env.select(&["Combat (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(returned(&plan, "Pong (USA)").how, ReturnHow::Rename);
    // Changed where it is placed, the same size and modification time, just before its rename.
    let placed = env.library().join("Pong (USA).zip");
    let job = build_meddling(
        &env,
        &plan,
        |f| matches!(f, Fault::ReturnRenaming(_)),
        move || overwrite_keeping_stamp(&placed),
    );
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let it = env.item(job.id, "Pong (USA)");
    assert_eq!(it.state, ItemState::Failed, "{it:?}");
    pong_left_placed(&env, "changed before");
}

/// Reopens the store, as [`Env::restart`] does, but stops its recovery when `stop` says to, as a kill would; then
/// restarts it again, which recovers what that stop left.
fn restart_stopping(
    env: Env,
    stop: impl Fn(Fault) -> bool + Send + Sync + 'static,
) -> (Env, Vec<(i64, String)>) {
    let Env {
        tmp,
        store,
        config,
        folder,
        sha256,
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let store = title_select_store::Store::open(&path).unwrap();
    store.hook_faults(Some(std::sync::Arc::new(move |fault| {
        if stop(fault) {
            panic!("killed at {fault:?}");
        }
    })));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.recover_jobs(WORKERS).unwrap();
        store.recover_builds(&config).unwrap();
    }));
    assert!(ran.is_err(), "recovery never reached the stop");
    store.hook_faults(None);
    Env {
        tmp,
        store,
        config,
        folder,
        sha256,
    }
    .restart()
}

#[test]
fn a_stop_between_putting_a_renamed_file_back_in_a_build_and_recording_it_is_finished_on_restart() {
    // Rolled back by the last check, in the build: the source set's folder replaced as the last disc is renamed,
    // and the build stopped once that disc is put back, before it is verified and recorded.
    let env = Env::with(&two_discs());
    let (plan, bytes) = renaming_pitfall(&env);
    let set = env.set();
    let (moved, put_back) = (
        std::sync::atomic::AtomicUsize::new(0),
        std::sync::atomic::AtomicUsize::new(0),
    );
    let meddle = std::sync::Mutex::new(Some(move || replace_with_marked_copy(&set, &set)));
    env.build_until(&plan, move |fault| {
        if matches!(fault, Fault::ReturnMoved(_))
            && moved.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            && let Some(meddle) = meddle.lock().unwrap().take()
        {
            meddle();
        }
        matches!(fault, Fault::ReturnPutBack(_))
            && put_back.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
    });
    std::fs::remove_dir_all(format!("{}.moved", env.set().display())).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    pitfall_returns_next(&env, &bytes, "rolled back by the build");
}

#[test]
fn a_stop_between_putting_a_renamed_file_back_in_recovery_and_recording_it_is_finished_on_restart()
{
    // Rolled back by recovery: the build stopped after the first disc was renamed home, and its recovery stopped
    // once that disc is put back.
    let env = Env::with(&two_discs());
    let (plan, bytes) = renaming_pitfall(&env);
    env.build_until(&plan, |f| matches!(f, Fault::ReturnMoved(_)));
    let (env, blocked) = restart_stopping(env, |f| matches!(f, Fault::ReturnPutBack(_)));
    assert!(blocked.is_empty(), "{blocked:?}");
    pitfall_returns_next(&env, &bytes, "rolled back by recovery");
}

#[test]
fn a_game_listing_more_than_one_rom_goes_home_as_torrentzip_though_one_was_placed() {
    let mut games = crate::single_copy::games();
    games.push((
        "Manual (USA)",
        vec![
            rom("Manual (USA).a26", b"manual game"),
            rom("Manual (USA).txt", b"manual text"),
        ],
    ));
    let mut env = Env::with(&games);
    // Revision 2 makes Manual's text an optional ROM, which no file holds.
    let dat = env.tmp.path().join("dats/alpha-2.dat");
    let xml = std::fs::read_to_string(env.tmp.path().join("dats/alpha.dat"))
        .unwrap()
        .replace(r#"version>1<"#, r#"version>2<"#)
        .replace(
            r#"<rom name="Manual (USA).txt""#,
            r#"<rom optional="yes" name="Manual (USA).txt""#,
        );
    std::fs::write(&dat, xml).unwrap();
    env.sha256 = env.store.import_dat(&dat, None).unwrap().0.sha256;
    torrentzip(
        &env.set().join("Manual (USA).zip"),
        &[("Manual (USA).a26", b"manual game")],
    );
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Manual (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Manual (USA)").kind,
        ItemKind::Place,
        "{:?}",
        plan.content.notes
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    env.audit(AuditEvidence::Full);
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = StorageFormat::Loose;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);

    let r = returned(&plan, "Manual (USA)");
    let paths: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["Manual (USA).zip"], "{r:?}");
}

#[test]
fn a_game_placed_with_more_roms_than_its_dat_now_lists_goes_home_as_torrentzip() {
    let mut games = crate::single_copy::games();
    games.push((
        "Manual (USA)",
        vec![
            rom("Manual (USA).a26", b"manual game"),
            rom("Manual (USA).txt", b"manual text"),
        ],
    ));
    let mut env = Env::with(&games);
    let both = [
        ("Manual (USA).a26", &b"manual game"[..]),
        ("Manual (USA).txt", &b"manual text"[..]),
    ];
    torrentzip(&env.set().join("Manual (USA).zip"), &both);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Manual (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Manual (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    // Revision 2 lists only Manual's game ROM, though both were placed.
    let dat = env.tmp.path().join("dats/alpha-2.dat");
    let xml = std::fs::read_to_string(env.tmp.path().join("dats/alpha.dat")).unwrap();
    let text = xml.find(r#"<rom name="Manual (USA).txt""#).unwrap();
    let end = text + xml[text..].find("/>").unwrap() + 2;
    let xml = format!("{}{}", &xml[..text], &xml[end..]).replace("version>1<", "version>2<");
    std::fs::write(&dat, xml).unwrap();
    env.sha256 = env.store.import_dat(&dat, None).unwrap().0.sha256;
    env.store
        .audit_source_set(&env.config, SET, Some(&env.sha256), AuditEvidence::Full)
        .unwrap();
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = StorageFormat::Loose;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);

    assert!(
        plan.content.blockers.is_empty(),
        "{:?}",
        plan.content.blockers
    );
    let r = returned(&plan, "Manual (USA)");
    let paths: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["Manual (USA).zip"], "{r:?}");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert!(env.files().0.contains(&"Manual (USA).zip".to_owned()));
}
