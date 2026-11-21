//! A repair releases a damaged placed CHD (ADR 0023): the CHD is set aside in the source set's kept files, its
//! placement dropped, and its game left missing, for the next repair to fill from an ingest folder and the next build
//! to place again. A placed multi-disc game is released whole: every disc's CHD is set aside, healthy ones included,
//! with the files laid out beside them. Nothing is extracted, put home, filled or deleted.

use super::*;
use title_select_api::build::FileOutcome;
use title_select_api::repair::{RepairChoice, RepairItem, RepairKind, RepairPlan, RepairStep};
use title_select_api::report::GameStatus;
use title_select_api::storage::placed_path;
use title_select_store::Fault;

impl Env {
    /// Plans a repair of the source set with `choices` in place of the defaults.
    fn repair_plan(&self, choices: &[(&str, RepairChoice)]) -> RepairPlan {
        let request = title_select_api::repair::PlanRepair {
            choices: choices.iter().map(|(k, c)| (k.to_string(), *c)).collect(),
            ..Default::default()
        };
        self.store.plan_repair(&self.config, SET, &request).unwrap()
    }

    /// Approves and runs `plan`, then runs the audit it owes; returns the plan as it ended.
    fn repair(&self, plan: &RepairPlan) -> RepairPlan {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        let ended = self.store.run_repair_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.follow_up(plan.id)
    }

    /// Runs the audit repair `plan` owes; returns the plan.
    fn follow_up(&self, plan: i64) -> RepairPlan {
        self.store.fulfil_repair_audits().unwrap();
        let done = self.store.repair_plan(plan).unwrap();
        let audit = done.audit_job.unwrap_or_else(|| {
            panic!(
                "a repair queues an audit: {:?} {:?}",
                done.recovery_error,
                self.store.repair_plan(plan).unwrap().outcomes
            )
        });
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, audit);
        let audited = self.store.run_audit_job(&self.config, audit, None).unwrap();
        assert_eq!(audited.state, JobState::Succeeded, "{audited:?}");
        done
    }

    /// The status the latest audit gives `game`.
    fn status(&self, game: &str) -> GameStatus {
        let (_, report) = latest_report(self);
        report
            .games
            .iter()
            .find(|g| g.name == game)
            .map(|g| g.status)
            .unwrap_or_else(|| panic!("{game}: {:?}", report.games))
    }

    /// The paths of the source set's placed files in the play library folder, in order.
    fn placed_paths(&self) -> Vec<String> {
        let mut placed: Vec<String> = self
            .store
            .placed_files(SET)
            .unwrap()
            .into_iter()
            .map(|p| p.path)
            .collect();
        placed.sort();
        placed
    }

    /// The source set's kept files, by where each was.
    fn set_kept(&self) -> Vec<String> {
        let mut kept: Vec<String> = self
            .store
            .source_kept_files(SET)
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        kept.sort();
        kept
    }

    /// Reopens the store as after a stop of the service, carrying placed files by a copy when `across`, and, as the
    /// service does when it next starts, recovers the Jobs and repairs the stop left and runs each Repair Job to its
    /// end.
    fn restarted(self, across: bool) -> Env {
        let Env {
            tmp,
            store,
            config,
            sha256,
        } = self;
        let path = store.dir().to_path_buf();
        drop(store);
        let store = Store::open(&path).unwrap();
        store.copy_across_filesystems(across);
        let env = Env {
            tmp,
            store,
            config,
            sha256,
        };
        let mut jobs: Vec<i64> = env
            .store
            .recover_jobs(WORKERS)
            .unwrap()
            .into_iter()
            .filter_map(|r| r.requeued.map(|j| j.id))
            .collect();
        assert!(env.store.recover_builds(&env.config).unwrap().is_empty());
        jobs.extend(
            env.store
                .recover_repairs()
                .unwrap()
                .into_iter()
                .map(|j| j.id),
        );
        for id in jobs {
            assert_eq!(env.store.claim_job().unwrap().unwrap().id, id);
            env.store.run_repair_job(&env.config, id, None).unwrap();
        }
        env
    }
}

/// The item of `plan` for `game`.
fn item_of<'p>(plan: &'p RepairPlan, game: &str) -> &'p RepairItem {
    plan.content
        .items
        .iter()
        .find(|i| i.key == format!("game:{game}"))
        .unwrap_or_else(|| panic!("{game}: {:#?}", plan.content.items))
}

/// Whether `plan` only sets files aside: a repair that releases a placed release never fills.
fn only_keeps(plan: &RepairPlan) -> bool {
    plan.content
        .steps
        .iter()
        .all(|s| matches!(s.step, RepairStep::Keep { .. }))
}

/// The sequence number of `plan`'s step keeping `path`, a file of the play library folder, aside.
fn keeping(plan: &RepairPlan, path: &str) -> u32 {
    plan.content
        .steps
        .iter()
        .position(|s| matches!(&s.step, RepairStep::Keep { path: p, .. } if p.ends_with(path)))
        .unwrap_or_else(|| panic!("{path}: {:#?}", plan.content.steps)) as u32
}

/// Places Orbit as a CHD, flags it for review as a return proving it damaged does, then damages it where it is placed,
/// a track of other bytes, and audits it fully. Returns the folder and the selection that places it.
fn damaged_orbit(env: &mut Env, chdman: &Path) -> (LibraryFolder, i64) {
    let folder = place_orbit(env, chdman);
    env.audit(title_select_engine::AuditEvidence::Full);
    let placing = env.select(&[CD]);
    let returning = env.select(&[DVD]);
    flag_orbit(env, &env.plan(&folder, returning));
    let made = tempfile::tempdir().unwrap();
    let other = orbit_with_another_track(chdman, made.path());
    std::fs::copy(&other, env.dir().join("Orbit (USA).chd")).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    assert_ne!(env.status(CD), GameStatus::Complete);
    (folder, placing)
}

/// Where the source set keeps `kept`, one of its kept files.
fn kept_at(env: &Env, kept: &title_select_api::repair::SourceKeptFile) -> PathBuf {
    env.tmp
        .path()
        .join("roots/sets")
        .join(SET)
        .join(".title-select/retained")
        .join(kept.generation.to_string())
        .join(&kept.kept_name)
}

/// A placed CHD with a wrong track: the repair defaults to releasing it. The CHD is set aside in the source set's kept
/// files, it is no longer placed, its review flag is cleared and its cue sheet let go, nothing of it is left in the
/// folder, and the audit the repair owes finds the game missing. The kept CHD is then yours to delete, as any kept
/// file is.
#[test]
fn a_repair_releases_a_damaged_placed_chd_into_the_source_sets_kept_files() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_orbit(&mut env, &chdman);
    assert_eq!(env.store.review_flags(SET).unwrap().len(), 1);
    let bytes = std::fs::read(env.dir().join("Orbit (USA).chd")).unwrap();

    let plan = env.repair_plan(&[]);
    let item = item_of(&plan, CD);
    assert_eq!(item.default, RepairChoice::KeepAside, "{item:?}");
    assert_eq!(item.chosen, RepairChoice::KeepAside, "{item:?}");
    assert_eq!(
        item.choices,
        [RepairChoice::KeepAside, RepairChoice::Leave],
        "{item:?}"
    );
    let note = item.note.as_deref().unwrap_or_default();
    for said in [
        "is the only copy of Orbit (USA)",
        "sets it aside in your source set's kept files, where you can delete it",
        "This repair only sets files aside",
        "with a good copy of it in an ingest folder, the next repair after the audit this one queues rebuilds it",
    ] {
        assert!(note.contains(said), "{said}: {note}");
    }
    assert!(plan.content.needs.is_empty(), "a rename takes no space");
    assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
    env.repair(&plan);

    assert_eq!(
        env.set_kept(),
        [placed_path(folder.id, "Orbit (USA).chd")],
        "the damaged CHD is a kept file of the source set"
    );
    assert!(
        env.store
            .placed_files(SET)
            .unwrap()
            .iter()
            .all(|p| p.game != CD)
    );
    assert!(env.store.review_flags(SET).unwrap().is_empty());
    assert!(!tree(&env.dir()).iter().any(|f| f.contains("Orbit")));
    env.store.collect_garbage().unwrap();
    assert!(env.cues().is_empty(), "the cue sheet is let go");
    assert_eq!(env.status(CD), GameStatus::Missing);
    assert!(env.in_set(CD).is_empty());

    let kept = env.store.source_kept_files(SET).unwrap();
    let copy = kept_at(&env, &kept[0]);
    assert_eq!(std::fs::read(&copy).unwrap(), bytes, "set aside as it was");
    let pick = title_select_api::build::PlanKeptDeletion {
        ids: vec![kept[0].id],
        all: false,
    };
    let deletion = env
        .store
        .plan_source_kept_deletion(&env.config, SET, &pick)
        .unwrap();
    let done = env
        .store
        .approve_source_kept_deletion(&env.config, deletion.id, &deletion.digest, "Operator")
        .unwrap();
    assert!(done.outcomes.iter().all(|o| o.deleted), "{done:#?}");
    assert!(!copy.exists());
    assert!(env.set_kept().is_empty());
}

/// Orbit, whole, as one ZIP's bytes.
fn orbit_zip(env: &Env) -> Vec<u8> {
    let roms = cd();
    let members: Vec<(&str, &[u8])> = roms
        .iter()
        .map(|r| (r.name, r.payload.as_slice()))
        .collect();
    let zip = env.tmp.path().join("orbit.zip");
    write_zip(&zip, &members);
    std::fs::read(&zip).unwrap()
}

/// With a good copy in an ingest folder, the repair that releases the damaged CHD only sets it aside, and the game is
/// missing. The next repair, after the audit it owes, fills the game in the source set from that copy; the audit after
/// it finds the game complete there, and the next build places it again as a CHD a full audit verifies.
#[test]
fn a_released_chd_is_rebuilt_by_the_next_repair_for_the_next_build_to_place() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, placing) = damaged_orbit(&mut env, &chdman);
    let zip = orbit_zip(&env);
    ingest(&mut env, &chdman, "Orbit (USA).zip", &zip);

    let plan = env.repair_plan(&[]);
    let item = item_of(&plan, CD);
    assert_eq!(item.kind, RepairKind::KeepAside, "{item:?}");
    assert_eq!(item.chosen, RepairChoice::KeepAside, "{item:?}");
    assert!(
        only_keeps(&plan),
        "nothing filled: {:#?}",
        plan.content.steps
    );
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(env.set_kept(), [placed_path(folder.id, "Orbit (USA).chd")]);
    assert_eq!(env.status(CD), GameStatus::Missing);
    assert!(env.in_set(CD).is_empty(), "{:?}", env.in_set(CD));
    assert!(env.tmp.path().join("in/Orbit (USA).zip").is_file());

    let plan = env.repair_plan(&[]);
    assert_ne!(item_of(&plan, CD).chosen, RepairChoice::Leave, "{plan:#?}");
    assert!(
        plan.content
            .steps
            .iter()
            .any(|s| matches!(s.step, RepairStep::Fill { .. })),
        "{:#?}",
        plan.content.steps
    );
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(env.status(CD), GameStatus::Complete);
    assert_eq!(orbit_home(&env), orbit_roms());
    assert!(!tree(&env.dir()).iter().any(|f| f.contains("Orbit")));

    let job = env.build(&env.plan(&folder, placing));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    let placed: Vec<(String, bool)> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .filter(|p| p.game == CD)
        .map(|p| (p.path, p.chd))
        .collect();
    assert_eq!(placed, [("Orbit (USA).chd".to_owned(), true)]);
    assert!(env.in_set(CD).is_empty(), "{:?}", env.in_set(CD));
    // Placed again as a CHD a full audit reads as the DAT's.
    let report = env.audit(title_select_engine::AuditEvidence::Full);
    let game = report.games.iter().find(|g| g.name == CD).unwrap();
    assert_eq!(game.status, GameStatus::Complete, "{game:?}");
}

/// Across filesystems, the damaged CHD is copied into the source set's kept files, the copy charged to the plan, and
/// deleted where it was placed only while it is still exactly what was copied: one written over after the copy stays
/// placed, and its copy stays kept.
#[test]
fn a_damaged_placed_chd_released_across_filesystems_is_deleted_only_with_a_trusted_stamp() {
    let Some(chdman) = chdman() else { return };
    for spoiled in [false, true] {
        let mut env = Env::new(Some(&chdman));
        let (folder, _) = damaged_orbit(&mut env, &chdman);
        env.store.copy_across_filesystems(true);
        let chd = env.dir().join("Orbit (USA).chd");
        let bytes = std::fs::read(&chd).unwrap();
        let plan = env.repair_plan(&[]);
        assert!(
            plan.content.needs["sets"] >= bytes.len() as u64,
            "{:?}",
            plan.content.needs
        );
        let seq = keeping(&plan, "Orbit (USA).chd");
        let written = chd.clone();
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
                if spoiled && at == Fault::RepairMoved(seq) {
                    let mtime = std::fs::metadata(&written).unwrap().modified().unwrap();
                    let mut bytes = std::fs::read(&written).unwrap();
                    *bytes.last_mut().unwrap() ^= 1;
                    let mut file = std::fs::File::options().write(true).open(&written).unwrap();
                    std::io::Write::write_all(&mut file, &bytes).unwrap();
                    file.set_modified(mtime).unwrap();
                }
            })));
        env.repair(&plan);
        env.store.hook_faults(None);

        let kept = env.store.source_kept_files(SET).unwrap();
        assert_eq!(kept.len(), 1, "{spoiled}: {kept:?}");
        assert_eq!(kept[0].name, placed_path(folder.id, "Orbit (USA).chd"));
        assert_eq!(
            std::fs::read(kept_at(&env, &kept[0])).unwrap(),
            bytes,
            "{spoiled}: an exact copy"
        );
        let placed: Vec<String> = env
            .store
            .placed_files(SET)
            .unwrap()
            .into_iter()
            .filter(|p| p.game == CD)
            .map(|p| p.path)
            .collect();
        match spoiled {
            false => {
                assert!(!chd.exists(), "the original is deleted");
                assert!(placed.is_empty(), "{placed:?}");
                assert_eq!(env.status(CD), GameStatus::Missing);
            }
            true => {
                assert!(
                    chd.is_file(),
                    "the changed original stays where it was placed"
                );
                assert_eq!(placed, ["Orbit (USA).chd"]);
            }
        }
    }
}

/// Across filesystems, a file copied into the source set's kept files whose original can't then be moved out of its
/// folder to be deleted stays placed, and its copy stays kept: nothing is deleted, and no copy is deleted to leave one.
/// The .m3u is the first file kept, so the CHDs, which need it kept first, are left placed and recorded. Once the
/// folder can be written again, the next repair releases the whole release, the .m3u set aside a second time.
#[test]
fn a_copy_across_filesystems_whose_original_cant_be_deleted_stays_kept_beside_it() {
    use std::os::unix::fs::PermissionsExt;
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    env.store.copy_across_filesystems(true);
    let m3u = "Saga (USA).m3u/Saga (USA).m3u";
    let bytes = std::fs::read(env.dir().join(m3u)).unwrap();
    let saga = env.dir().join("Saga (USA).m3u");
    std::fs::set_permissions(&saga, std::fs::Permissions::from_mode(0o555)).unwrap();
    let done = env.repair(&env.repair_plan(&[]));
    std::fs::set_permissions(&saga, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        done.outcomes.iter().all(|o| !o.done),
        "{:#?}",
        done.outcomes
    );
    assert!(
        done.outcomes
            .iter()
            .flat_map(|o| o.detail.iter().chain(&o.error))
            .any(|d| d.contains("it stays placed")),
        "{:#?}",
        done.outcomes
    );
    assert_eq!(env.set_kept(), [placed_path(folder.id, m3u)]);
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(std::fs::read(kept_at(&env, &kept[0])).unwrap(), bytes);
    assert_eq!(std::fs::read(env.dir().join(m3u)).unwrap(), bytes);
    each_disc_once(&env, &folder, "its folder can't be written", Some(m3u));

    env.audit(title_select_engine::AuditEvidence::Full);
    env.repair(&env.repair_plan(&[]));
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(env.set_kept(), saga_released_with(&folder, Some(m3u)));
}

/// A release the source set's root hasn't the space for is refused before it runs, nothing changed; with the space,
/// the same plan runs.
#[test]
fn a_release_its_root_has_no_space_for_is_refused_before_it_runs() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    damaged_orbit(&mut env, &chdman);
    env.store.copy_across_filesystems(true);
    let size = std::fs::metadata(env.dir().join("Orbit (USA).chd"))
        .unwrap()
        .len();
    let plan = env.repair_plan(&[]);
    assert_eq!(
        plan.content.needs.keys().collect::<Vec<_>>(),
        ["sets"],
        "only the source set's root is written"
    );
    let needs = plan.content.needs["sets"];
    assert!(needs >= size, "{:?}", plan.content.needs);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains(&format!("needs up to {needs} bytes free in root sets"))),
        "{:?}",
        plan.content.notes
    );

    env.store.pretend_free_space(Some(needs - 1));
    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap_err();
    assert!(
        matches!(&refused, title_select_store::Error::Refused(why) if why.contains("needs up to")),
        "{refused:?}"
    );
    assert!(env.store.repair_plan(plan.id).unwrap().approval.is_none());
    assert!(env.set_kept().is_empty());
    assert!(env.dir().join("Orbit (USA).chd").is_file());

    env.store.pretend_free_space(Some(needs));
    env.repair(&plan);
    env.store.pretend_free_space(None);
    assert_eq!(env.set_kept().len(), 1);
    assert_eq!(env.status(CD), GameStatus::Missing);
}

/// A plan that counted on a rename, which then falls back to a copy across mounts as it runs, copies only what fits:
/// a CHD whose copy won't fit stays placed and recorded, nothing is kept, and the step says why.
#[test]
fn a_release_whose_copy_across_mounts_wont_fit_as_it_runs_leaves_the_chd_placed() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_orbit(&mut env, &chdman);
    let chd = env.dir().join("Orbit (USA).chd");
    let bytes = std::fs::read(&chd).unwrap();
    let plan = env.repair_plan(&[]);
    assert!(plan.content.needs.is_empty(), "planned as a rename");

    // The rename turns out to cross mounts, and there isn't room for the copy.
    env.store.copy_across_filesystems(true);
    env.store.pretend_free_space(Some(bytes.len() as u64 / 2));
    let done = env.repair(&plan);
    env.store.pretend_free_space(None);
    env.store.copy_across_filesystems(false);

    let outcome = done
        .outcomes
        .iter()
        .find(|o| o.item == format!("game:{CD}"))
        .unwrap_or_else(|| panic!("{:#?}", done.outcomes));
    assert!(!outcome.done, "{outcome:?}");
    assert!(
        outcome
            .error
            .iter()
            .chain(&outcome.detail)
            .any(|e| e.contains("free in root sets")),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read(&chd).unwrap(), bytes, "left as it is");
    assert!(env.set_kept().is_empty(), "{:?}", env.set_kept());
    let placed: Vec<String> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .filter(|p| p.game == CD)
        .map(|p| placed_path(p.library_folder, &p.path))
        .collect();
    assert_eq!(placed, [placed_path(folder.id, "Orbit (USA).chd")]);
}

/// A CHD of disc `n` of Saga whose data track holds other bytes, made in `dir`.
fn saga_with_another_track(chdman: &Path, dir: &Path, n: usize) -> PathBuf {
    for r in saga(n) {
        let mut bytes = r.payload.clone();
        if r.name.ends_with(".bin") {
            *bytes.last_mut().unwrap() ^= 0xff;
        }
        std::fs::write(dir.join(r.name), bytes).unwrap();
    }
    let out = dir.join("other.chd");
    let made = std::process::Command::new(chdman)
        .arg("createcd")
        .arg("-i")
        .arg(dir.join(format!("{}.cue", SAGA[n - 1])))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    out
}

/// Places both discs of Saga as CHDs, audits fully, then damages disc `n`'s CHD where it is placed, its data track of
/// other bytes, and audits fully again. Returns the folder and the selection placing it.
fn damaged_saga(env: &mut Env, chdman: &Path, n: usize) -> (LibraryFolder, i64) {
    placing_chds(env, chdman);
    let (folder, selection) = env.folder(&SAGA);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(env, &job));
    env.audit(title_select_engine::AuditEvidence::Full);
    let made = tempfile::tempdir().unwrap();
    let other = saga_with_another_track(chdman, made.path(), n);
    std::fs::copy(&other, env.dir().join(SAGA_CHDS[n - 1])).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    (folder, selection)
}

/// Disc `n` of Saga as its files in the source set's folder, by name, with their bytes.
fn saga_home(env: &Env, n: usize) -> Vec<(String, Vec<u8>)> {
    let set = env.tmp.path().join("roots/sets").join(SET);
    let mut found: Vec<(String, Vec<u8>)> = env
        .in_set(SAGA[n - 1])
        .into_iter()
        .filter(|f| !f.ends_with('/'))
        .filter(|f| set.join(f).is_file())
        .map(|f| {
            let bytes = std::fs::read(set.join(&f)).unwrap();
            (f.rsplit('/').next().unwrap().to_owned(), bytes)
        })
        .collect();
    found.sort();
    found
}

/// Disc `n` of Saga as the DAT lists it, by name.
fn saga_roms(n: usize) -> Vec<(String, Vec<u8>)> {
    let mut roms: Vec<(String, Vec<u8>)> = saga(n)
        .into_iter()
        .map(|r| (r.name.to_owned(), r.payload))
        .collect();
    roms.sort();
    roms
}

/// Disc `n` of Saga, whole, as one ZIP's bytes.
fn saga_zip(env: &Env, n: usize) -> Vec<u8> {
    let roms = saga(n);
    let members: Vec<(&str, &[u8])> = roms
        .iter()
        .map(|r| (r.name, r.payload.as_slice()))
        .collect();
    let zip = env.tmp.path().join(format!("saga {n}.zip"));
    write_zip(&zip, &members);
    std::fs::read(&zip).unwrap()
}

/// Whether anything of Saga is in the play library folder, or recorded placed.
fn saga_anywhere_placed(env: &Env) -> bool {
    tree(&env.dir()).iter().any(|f| f.starts_with("Saga"))
        || env
            .store
            .placed_files(SET)
            .unwrap()
            .iter()
            .any(|p| p.game.starts_with("Saga"))
}

/// What Saga's release leaves in the source set's kept files once it is released whole: both CHDs and the .m3u.
fn saga_released(folder: &LibraryFolder) -> Vec<String> {
    saga_released_with(folder, None)
}

/// As [`saga_released`], with `twice`, one of its files, kept a second time: a stop across mounts left a copy of it
/// kept while it stayed placed, and the next repair set it aside again.
fn saga_released_with(folder: &LibraryFolder, twice: Option<&str>) -> Vec<String> {
    let mut kept: Vec<String> = [SAGA_CHDS[0], SAGA_CHDS[1], "Saga (USA).m3u/Saga (USA).m3u"]
        .into_iter()
        .chain(twice)
        .map(|f| placed_path(folder.id, f))
        .collect();
    kept.sort();
    kept
}

/// A two-disc release whose disc 2 is damaged is released whole: both CHDs, the healthy one included, and the
/// release's .m3u are set aside in the source set's kept files, nothing of it is left in the folder, and nothing is
/// written home. Once a good copy of both discs is in an ingest folder, the next repair fills them, the audit after it
/// finds both complete, and the next build places both again as CHDs a full audit verifies.
#[test]
fn a_two_disc_release_with_a_damaged_disc_sets_every_disc_aside() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, selection) = damaged_saga(&mut env, &chdman, 2);
    let healthy = std::fs::read(env.dir().join(SAGA_CHDS[0])).unwrap();

    let plan = env.repair_plan(&[]);
    for game in SAGA {
        let item = item_of(&plan, game);
        assert_eq!(item.chosen, RepairChoice::KeepAside, "{item:?}");
        assert!(
            item.note.as_deref().is_some_and(|n| n.contains(
                "a repair never sets one disc of a game aside alone, so this sets every disc of Saga (USA) aside \
                 in your source set's kept files"
            )),
            "{item:?}"
        );
    }
    assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);

    assert_eq!(env.set_kept(), saga_released(&folder));
    let kept = env.store.source_kept_files(SET).unwrap();
    let disc1 = kept
        .iter()
        .find(|k| k.name == placed_path(folder.id, SAGA_CHDS[0]))
        .unwrap();
    assert_eq!(std::fs::read(kept_at(&env, disc1)).unwrap(), healthy);
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert!(env.in_set("Saga").is_empty(), "{:?}", env.in_set("Saga"));
    assert_eq!(env.status(SAGA[0]), GameStatus::Missing);
    assert_eq!(env.status(SAGA[1]), GameStatus::Missing);
    env.store.collect_garbage().unwrap();
    assert!(env.cues().is_empty(), "the cue sheets are let go");

    for n in [1, 2] {
        let zip = saga_zip(&env, n);
        ingest(&mut env, &chdman, &format!("Saga {n}.zip"), &zip);
    }
    let done = env.repair(&env.repair_plan(&[]));
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(saga_home(&env, 1), saga_roms(1));
    assert_eq!(saga_home(&env, 2), saga_roms(2));
    assert_eq!(env.status(SAGA[0]), GameStatus::Complete);
    assert_eq!(env.status(SAGA[1]), GameStatus::Complete);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(env.placed_paths(), SAGA_CHDS);
    assert!(env.store.placed_files(SET).unwrap().iter().all(|p| p.chd));
    assert!(env.in_set("Saga").is_empty(), "{:?}", env.in_set("Saga"));
    // Placed again as CHDs a full audit reads as the DAT's.
    env.audit(title_select_engine::AuditEvidence::Full);
    assert_eq!(env.status(SAGA[0]), GameStatus::Complete);
    assert_eq!(env.status(SAGA[1]), GameStatus::Complete);
}

/// A two-disc release whose disc 2's CHD is gone is released whole: disc 1's CHD and the .m3u are set aside, nothing
/// is kept for disc 2, and neither disc is placed any more.
#[test]
fn a_two_disc_release_with_a_gone_disc_sets_the_other_aside_and_forgets_it() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    let (folder, selection) = env.folder(&SAGA);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    env.audit(title_select_engine::AuditEvidence::Full);
    std::fs::remove_file(env.dir().join(SAGA_CHDS[1])).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);

    let plan = env.repair_plan(&[]);
    for game in SAGA {
        assert_eq!(item_of(&plan, game).chosen, RepairChoice::KeepAside);
    }
    assert!(
        item_of(&plan, SAGA[1]).summary.contains("gone"),
        "{:?}",
        item_of(&plan, SAGA[1])
    );
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);

    assert_eq!(
        env.set_kept(),
        [
            placed_path(folder.id, SAGA_CHDS[0]),
            placed_path(folder.id, "Saga (USA).m3u/Saga (USA).m3u"),
        ]
    );
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(env.status(SAGA[0]), GameStatus::Missing);
    assert_eq!(env.status(SAGA[1]), GameStatus::Missing);
}

/// After a release, the play library folder's check reports each released disc the current build still selects, and
/// the .m3u laid out beside them, as missing: the folder is checked against what its build wrote there, not against
/// what is placed. Once the game is filled, the next build places it again, and the check finds nothing missing.
#[test]
fn after_a_release_the_folder_check_reports_the_released_discs_missing() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, selection) = damaged_saga(&mut env, &chdman, 2);
    env.repair(&env.repair_plan(&[]));

    let check = env.check(&env.config, &folder);
    let mut missing: Vec<&str> = check
        .files
        .iter()
        .filter(|f| f.outcome == FileOutcome::Missing)
        .map(|f| f.target.as_str())
        .collect();
    missing.sort();
    assert_eq!(
        missing,
        [SAGA_CHDS[0], SAGA_CHDS[1], "Saga (USA).m3u/Saga (USA).m3u"],
        "{:#?}",
        check.files
    );
    assert!(
        check
            .files
            .iter()
            .filter(|f| f.outcome == FileOutcome::Missing)
            .all(|f| f.selected && !f.placed),
        "{:#?}",
        check.files
    );
    assert_eq!(check.summary.missing, 3, "{:?}", check.summary);

    // Filled from an ingest folder, the next build places both again, and the folder is whole.
    for n in [1, 2] {
        let zip = saga_zip(&env, n);
        ingest(&mut env, &chdman, &format!("Saga {n}.zip"), &zip);
    }
    env.repair(&env.repair_plan(&[]));
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    let check = env.check(&env.config, &folder);
    assert_eq!(check.summary.missing, 0, "{:#?}", check.files);
}

/// A release left part way, disc 1's CHD changed after the plan was made so that it can't be set aside, keeps disc 2's
/// CHD and the .m3u aside and disc 1 placed, never both and never neither. Audited again, the next repair releases the
/// rest and fills nothing, though a good copy of disc 2 is in an ingest folder: no disc of a release is filled by the
/// repair that releases it. The repair after that fills disc 2.
#[test]
fn a_release_left_part_way_is_released_whole_by_the_next_repair() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    let zip = saga_zip(&env, 2);
    ingest(&mut env, &chdman, "Saga 2.zip", &zip);
    let plan = env.repair_plan(&[]);
    assert_eq!(item_of(&plan, SAGA[1]).kind, RepairKind::KeepAside);
    assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
    // Disc 1 changes once the plan is approved, before the repair runs.
    let disc1 = env.dir().join(SAGA_CHDS[0]);
    let mut bytes = std::fs::read(&disc1).unwrap();
    bytes.push(0);
    let (at, changed) = (disc1.clone(), bytes.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault: Fault| {
            if fault == Fault::RepairApproving {
                std::fs::write(&at, &changed).unwrap();
            }
        })));
    let done = env.repair(&plan);
    env.store.hook_faults(None);
    assert!(
        done.outcomes
            .iter()
            .any(|o| o.item == format!("game:{}", SAGA[0]) && !o.done),
        "{:#?}",
        done.outcomes
    );
    let placed = env.placed_paths();
    assert_eq!(placed, [SAGA_CHDS[0]], "disc 1 stays placed");
    assert_eq!(std::fs::read(&disc1).unwrap(), bytes);
    assert_eq!(
        env.set_kept(),
        [
            placed_path(folder.id, SAGA_CHDS[1]),
            placed_path(folder.id, "Saga (USA).m3u/Saga (USA).m3u"),
        ]
    );
    each_disc_once(&env, &folder, "disc 1 changed", None);

    env.audit(title_select_engine::AuditEvidence::Full);
    let plan = env.repair_plan(&[]);
    assert_eq!(item_of(&plan, SAGA[0]).chosen, RepairChoice::KeepAside);
    assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(env.set_kept(), saga_released(&folder));
    assert!(env.in_set("Saga").is_empty(), "{:?}", env.in_set("Saga"));
    assert_eq!(env.status(SAGA[0]), GameStatus::Missing);
    assert_eq!(env.status(SAGA[1]), GameStatus::Missing);

    let done = env.repair(&env.repair_plan(&[]));
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(saga_home(&env, 2), saga_roms(2));
    assert_eq!(env.status(SAGA[1]), GameStatus::Complete);
}

/// How Saga is kept: each disc either placed, recorded so with its CHD where it was placed, or its CHD a kept file of
/// the source set and no longer placed, never both and never neither; the .m3u in the folder or kept, once; nothing
/// of it in the source set. The one exception is `twice`, a file a stop across mounts, or an original that couldn't be
/// deleted, left both copied into the kept files and still in place, recorded placed if it is a CHD: nothing is lost,
/// and the copy is never deleted to leave one.
fn each_disc_once(env: &Env, folder: &LibraryFolder, label: &str, twice: Option<&str>) {
    let placed = env.placed_paths();
    let kept = env.set_kept();
    for (chd, recorded) in [
        (SAGA_CHDS[0], true),
        (SAGA_CHDS[1], true),
        ("Saga (USA).m3u/Saga (USA).m3u", false),
    ] {
        let there = env.dir().join(chd).is_file();
        let aside = kept.contains(&placed_path(folder.id, chd));
        match twice == Some(chd) {
            true => assert!(
                there && aside,
                "{label}: {chd} is in place and its copy kept; {:?}, {kept:?}",
                tree(&env.dir())
            ),
            false => assert_ne!(
                there,
                aside,
                "{label}: {chd} is in place or kept, once; {:?}, {kept:?}",
                tree(&env.dir())
            ),
        }
        if recorded {
            assert_eq!(
                placed.contains(&chd.to_owned()),
                there,
                "{label}: {chd} is recorded placed only while it is there: {placed:?}"
            );
        }
    }
    assert!(
        env.in_set("Saga").is_empty(),
        "{label}: {:?}",
        env.in_set("Saga")
    );
}

/// Stopped at each fault point of releasing a two-disc release, on one filesystem and across two, and started again,
/// the repair leaves each disc either placed and recorded or in the source set's kept files, never both and never
/// neither. The one accepted exception is a stop across mounts once a file is copied and moved to be deleted, before
/// its change time there is recorded: no stamp of it is trusted, so it is put back, placed and recorded, and its copy
/// stays kept. An audit and the next repair then release whatever is still placed: both CHDs and the .m3u set aside,
/// nothing of Saga placed.
#[test]
fn a_release_stopped_and_started_again_leaves_each_disc_placed_or_kept() {
    let Some(chdman) = chdman() else { return };
    for across in [false, true] {
        // Every fault point a release that isn't stopped passes, in order.
        let points: Vec<Fault> = {
            let mut env = Env::new(Some(&chdman));
            damaged_saga(&mut env, &chdman, 2);
            env.store.copy_across_filesystems(across);
            let plan = env.repair_plan(&[]);
            let approved = env
                .store
                .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
                .unwrap();
            env.store.claim_job().unwrap().unwrap();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let record = seen.clone();
            env.store
                .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
                    let mut seen = record.lock().unwrap();
                    if !seen.contains(&at) {
                        seen.push(at);
                    }
                })));
            let ended = env
                .store
                .run_repair_job(&env.config, approved.job.unwrap(), None)
                .unwrap();
            env.store.hook_faults(None);
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
            seen.lock().unwrap().clone()
        };
        for at in [
            Fault::RepairMoved(0),
            Fault::RepairStepDone(1),
            Fault::RepairFinished,
        ] {
            assert!(points.contains(&at), "{across}: {at:?} in {points:?}");
        }
        assert_eq!(
            points.contains(&Fault::RepairCarryMoved(1)),
            across,
            "{points:?}"
        );
        for stop in points {
            let label = format!("across {across}, stopped at {stop:?}");
            let mut env = Env::new(Some(&chdman));
            let (folder, _) = damaged_saga(&mut env, &chdman, 2);
            env.store.copy_across_filesystems(across);
            let plan = env.repair_plan(&[]);
            // The file whose copy a stop before its delete stamp leaves kept, while it stays placed.
            let twice = match stop {
                Fault::RepairCarryMoved(seq) => Some(
                    [SAGA_CHDS[0], SAGA_CHDS[1], "Saga (USA).m3u/Saga (USA).m3u"]
                        .into_iter()
                        .find(|f| keeping(&plan, f) == seq)
                        .unwrap(),
                ),
                _ => None,
            };
            let approved = env
                .store
                .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
                .unwrap();
            env.store.claim_job().unwrap().unwrap();
            env.store
                .hook_faults(Some(std::sync::Arc::new(move |fault: Fault| {
                    if fault == stop {
                        panic!("stopped at {fault:?}");
                    }
                })));
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.store
                    .run_repair_job(&env.config, approved.job.unwrap(), None)
            }));
            env.store.hook_faults(None);
            assert!(ran.is_err(), "{label}: never reached the stop: {ran:?}");
            let env = env.restarted(across);
            each_disc_once(&env, &folder, &label, twice);

            env.follow_up(plan.id);
            env.audit(title_select_engine::AuditEvidence::Full);
            if saga_anywhere_placed(&env) {
                env.repair(&env.repair_plan(&[]));
            }
            each_disc_once(&env, &folder, &label, None);
            assert!(!saga_anywhere_placed(&env), "{label}: released");
            assert_eq!(
                env.set_kept(),
                saga_released_with(&folder, twice),
                "{label}"
            );
            assert_eq!(env.status(SAGA[0]), GameStatus::Missing, "{label}");
            assert_eq!(env.status(SAGA[1]), GameStatus::Missing, "{label}");
        }
    }
}

/// A placed CHD the audit couldn't read, chdman not there to extract it, isn't proven damaged: the repair offers to set
/// it aside but leaves it placed unless you choose to, and says why.
#[test]
fn a_placed_chd_the_audit_couldnt_read_is_set_aside_only_by_your_choice() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = place_orbit(&mut env, &chdman);
    env.audit(title_select_engine::AuditEvidence::Full);
    without_chdman(&mut env);
    env.audit(title_select_engine::AuditEvidence::Full);

    let plan = env.repair_plan(&[]);
    let item = item_of(&plan, CD);
    assert_eq!(item.default, RepairChoice::Leave, "{item:?}");
    assert_eq!(item.chosen, RepairChoice::Leave, "{item:?}");
    assert_eq!(
        item.choices,
        [RepairChoice::KeepAside, RepairChoice::Leave],
        "{item:?}"
    );
    assert!(
        item.note
            .as_deref()
            .is_some_and(|n| n.contains("couldn't read it") && n.contains("only copy")),
        "{item:?}"
    );

    let chosen = env.repair_plan(&[(&format!("game:{CD}"), RepairChoice::KeepAside)]);
    env.repair(&chosen);
    assert_eq!(env.set_kept(), [placed_path(folder.id, "Orbit (USA).chd")]);
    assert!(
        env.store
            .placed_files(SET)
            .unwrap()
            .iter()
            .all(|p| p.game != CD)
    );
}

/// Names chdman where none is, so that a full audit can't read a CHD; the source set stays writable.
fn without_chdman(env: &mut Env) {
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
         [chd]\nchdman = {:?}\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.tmp.path().join("roots/play").display().to_string(),
        env.tmp.path().join("no-chdman").display().to_string(),
    ))
    .unwrap();
}

/// A two-disc release the audit couldn't read is left by default, and choosing to set one disc aside sets every disc
/// aside: the release is never split.
#[test]
fn choosing_to_set_aside_one_disc_the_audit_couldnt_read_sets_its_release_aside() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    without_chdman(&mut env);
    env.audit(title_select_engine::AuditEvidence::Full);

    let plan = env.repair_plan(&[]);
    for game in SAGA {
        assert_eq!(item_of(&plan, game).chosen, RepairChoice::Leave);
    }

    let chosen = env.repair_plan(&[(&format!("game:{}", SAGA[1]), RepairChoice::KeepAside)]);
    for game in SAGA {
        let item = item_of(&chosen, game);
        assert_eq!(item.chosen, RepairChoice::KeepAside, "{item:?}");
    }
    let done = env.repair(&chosen);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(env.set_kept(), saga_released(&folder));
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
}

/// Choosing to leave one disc of a release set aside by default leaves every disc of it placed, and choosing to set one
/// aside and leave the other is refused: the release is never split.
#[test]
fn choosing_to_leave_one_disc_of_a_release_leaves_every_disc_placed() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    damaged_saga(&mut env, &chdman, 2);

    let plan = env.repair_plan(&[(&format!("game:{}", SAGA[0]), RepairChoice::Leave)]);
    for game in SAGA {
        let item = item_of(&plan, game);
        assert_eq!(item.chosen, RepairChoice::Leave, "{item:?}");
    }
    assert!(plan.content.steps.is_empty(), "{:#?}", plan.content.steps);

    let request = title_select_api::repair::PlanRepair {
        choices: [
            (format!("game:{}", SAGA[0]), RepairChoice::Leave),
            (format!("game:{}", SAGA[1]), RepairChoice::KeepAside),
        ]
        .into(),
        ..Default::default()
    };
    let refused = env.store.plan_repair(&env.config, SET, &request);
    assert!(
        matches!(&refused, Err(title_select_store::Error::Refused(why)) if why.contains("choose the same")),
        "{refused:?}"
    );
}

/// Turns SBI files on for the system's play libraries, from an SBI pack of `members`, by name with their bytes.
fn sbi_on(env: &Env, members: &[(&str, &[u8])]) {
    let zip = env.tmp.path().join("sbi.zip");
    write_zip(&zip, members);
    env.store
        .keep_sbi_pack(&std::fs::read(zip).unwrap(), SYSTEM)
        .unwrap();
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.systems.get_mut(SYSTEM).unwrap().sbi = Some(true);
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

/// What `folder` records as built there, by path.
fn built_in(env: &Env, folder: &LibraryFolder) -> Vec<String> {
    let mut built: Vec<String> = env
        .store
        .built_files(folder.id)
        .unwrap()
        .into_iter()
        .map(|b| b.target)
        .collect();
    built.sort();
    built
}

/// A one-disc CHD placed in the system folder itself, its SBI file beside it, is released with its SBI file: both are
/// set aside in the source set's kept files. The folder still records both as its build wrote them, for its check to
/// report missing until the next build.
#[test]
fn a_released_disc_in_the_system_folder_takes_its_sbi_file_with_it() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    sbi_on(&env, &[("Orbit (USA).sbi", b"SBI\0orbit")]);
    let (folder, selection) = env.folder(&[CD]);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(tree(&env.dir()), ["Orbit (USA).chd", "Orbit (USA).sbi"]);
    env.audit(title_select_engine::AuditEvidence::Full);
    let made = tempfile::tempdir().unwrap();
    let other = orbit_with_another_track(&chdman, made.path());
    std::fs::copy(other, env.dir().join("Orbit (USA).chd")).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);

    let done = env.repair(&env.repair_plan(&[]));
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert!(tree(&env.dir()).is_empty(), "{:?}", tree(&env.dir()));
    assert_eq!(
        env.set_kept(),
        [
            placed_path(folder.id, "Orbit (USA).chd"),
            placed_path(folder.id, "Orbit (USA).sbi"),
        ]
    );
    assert_eq!(
        built_in(&env, &folder),
        ["Orbit (USA).chd", "Orbit (USA).sbi"]
    );
}

/// Places both discs of Saga as CHDs laid out flat, each with its SBI file beside it in the system folder, and audits
/// fully. Returns the folder.
fn flat_saga(env: &mut Env, chdman: &Path) -> LibraryFolder {
    placing_chds(env, chdman);
    let mut policy = env.store.storage_policy().unwrap();
    policy
        .play_libraries
        .systems
        .get_mut(SYSTEM)
        .unwrap()
        .layout = Some(title_select_api::storage::DiscLayout::Flat);
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    sbi_on(
        env,
        &[
            ("Saga (USA) (Disc 1).sbi", b"SBI\0saga 1"),
            ("Saga (USA) (Disc 2).sbi", b"SBI\0saga 2"),
        ],
    );
    let (folder, selection) = env.folder(&SAGA);
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(env, &job));
    assert_eq!(
        tree(&env.dir()),
        [
            "Saga (USA) (Disc 1).chd",
            "Saga (USA) (Disc 1).sbi",
            "Saga (USA) (Disc 2).chd",
            "Saga (USA) (Disc 2).sbi",
        ]
    );
    env.audit(title_select_engine::AuditEvidence::Full);
    folder
}

/// A two-disc release laid out flat, each disc's CHD and SBI file in the system folder, is released whole with its
/// SBI files: both CHDs and both SBI files set aside, nothing of it left in the folder.
#[test]
fn a_release_laid_out_flat_takes_its_sbi_files_with_it() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = flat_saga(&mut env, &chdman);
    let made = tempfile::tempdir().unwrap();
    let other = saga_with_another_track(&chdman, made.path(), 2);
    std::fs::copy(other, env.dir().join("Saga (USA) (Disc 2).chd")).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);

    let done = env.repair(&env.repair_plan(&[]));
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(
        env.set_kept(),
        [
            placed_path(folder.id, "Saga (USA) (Disc 1).chd"),
            placed_path(folder.id, "Saga (USA) (Disc 1).sbi"),
            placed_path(folder.id, "Saga (USA) (Disc 2).chd"),
            placed_path(folder.id, "Saga (USA) (Disc 2).sbi"),
        ]
    );
}

/// A release laid out flat whose disc 2 CHD is gone, and whose disc 2 SBI file changed after the plan was made so that
/// it can't be set aside, keeps disc 1's SBI file aside and leaves disc 1's CHD placed: no CHD is set aside without
/// every file beside it, and the gone disc stays recorded until one is, so the release is still found by its CHDs.
/// Audited again, the next repair releases the rest.
#[test]
fn a_release_one_of_whose_sbi_files_cant_be_set_aside_stays_recorded_by_its_chds() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let folder = flat_saga(&mut env, &chdman);
    std::fs::remove_file(env.dir().join("Saga (USA) (Disc 2).chd")).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    let plan = env.repair_plan(&[]);
    let sbi = env.dir().join("Saga (USA) (Disc 2).sbi");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::RepairApproving {
                std::fs::write(&sbi, b"another SBI file").unwrap();
            }
        })));
    env.repair(&plan);
    env.store.hook_faults(None);
    assert_eq!(
        env.placed_paths(),
        ["Saga (USA) (Disc 1).chd", "Saga (USA) (Disc 2).chd"],
        "both discs stay recorded"
    );
    assert_eq!(
        env.set_kept(),
        [placed_path(folder.id, "Saga (USA) (Disc 1).sbi")]
    );

    env.audit(title_select_engine::AuditEvidence::Full);
    env.repair(&env.repair_plan(&[]));
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(
        env.set_kept(),
        [
            placed_path(folder.id, "Saga (USA) (Disc 1).chd"),
            placed_path(folder.id, "Saga (USA) (Disc 1).sbi"),
            placed_path(folder.id, "Saga (USA) (Disc 2).sbi"),
        ]
    );
}

/// A two-disc release both of whose CHDs are gone is still released, for what is laid out beside them: the .m3u is
/// set aside in the source set's kept files, and neither disc is recorded placed any more.
#[test]
fn a_release_whose_every_chd_is_gone_sets_its_m3u_aside_and_forgets_its_discs() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    placing_chds(&mut env, &chdman);
    let (folder, selection) = env.folder(&SAGA);
    env.build(&env.plan(&folder, selection));
    for chd in SAGA_CHDS {
        std::fs::remove_file(env.dir().join(chd)).unwrap();
    }
    env.audit(title_select_engine::AuditEvidence::Full);
    let plan = env.repair_plan(&[]);
    for game in SAGA {
        assert_eq!(item_of(&plan, game).chosen, RepairChoice::KeepAside);
    }
    assert!(
        plan.content.steps.iter().any(
            |s| matches!(&s.step, RepairStep::Keep { path, .. } if path.ends_with("Saga (USA).m3u"))
        ),
        "the .m3u is set aside: {plan:#?}"
    );
    env.repair(&plan);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(
        env.set_kept(),
        [placed_path(folder.id, "Saga (USA).m3u/Saga (USA).m3u")]
    );
}

/// A file laid out beside a release's CHDs that can't be set aside, the .m3u changed after the plan was made, leaves
/// the whole release placed and recorded: no CHD is set aside without it. Audited again, the next repair releases it
/// whole.
#[test]
fn a_release_whose_m3u_cant_be_set_aside_stays_placed_whole() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    let plan = env.repair_plan(&[]);
    let m3u = env.dir().join("Saga (USA).m3u/Saga (USA).m3u");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::RepairApproving {
                std::fs::write(&m3u, b"a changed playlist\n").unwrap();
            }
        })));
    let done = env.repair(&plan);
    env.store.hook_faults(None);
    assert!(
        done.outcomes.iter().all(|o| !o.done),
        "{:#?}",
        done.outcomes
    );
    let placed = env.placed_paths();
    assert_eq!(placed, SAGA_CHDS, "both discs stay placed");
    assert!(env.set_kept().is_empty(), "{:?}", env.set_kept());
    each_disc_once(&env, &folder, "the .m3u changed", None);

    env.audit(title_select_engine::AuditEvidence::Full);
    env.repair(&env.repair_plan(&[]));
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(env.set_kept(), saga_released(&folder));
}

/// Stopped across filesystems once disc 1's CHD is copied into the kept files and moved to be deleted, before its
/// change time there is recorded, and started again, the repair trusts no stamp it didn't record: the CHD is put back,
/// placed and recorded as it was, its copy stays kept, and nothing is deleted. The other files of the release are set
/// aside as planned. Audited again, the next repair releases disc 1 once more.
#[test]
fn a_release_stopped_before_its_delete_stamp_is_recorded_keeps_the_copy_and_the_placed_chd() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    env.store.copy_across_filesystems(true);
    let bytes = std::fs::read(env.dir().join(SAGA_CHDS[0])).unwrap();
    let plan = env.repair_plan(&[]);
    let seq = keeping(&plan, SAGA_CHDS[0]);
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::RepairCarryMoved(seq) {
                panic!("stopped at {at:?}");
            }
        })));
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .run_repair_job(&env.config, approved.job.unwrap(), None)
    }));
    env.store.hook_faults(None);
    assert!(stopped.is_err());
    let env = env.restarted(true);

    let label = "stopped before the stamp was recorded";
    each_disc_once(&env, &folder, label, Some(SAGA_CHDS[0]));
    assert_eq!(env.placed_paths(), [SAGA_CHDS[0]]);
    assert_eq!(std::fs::read(env.dir().join(SAGA_CHDS[0])).unwrap(), bytes);
    let kept = env.store.source_kept_files(SET).unwrap();
    let copy = kept
        .iter()
        .find(|k| k.name == placed_path(folder.id, SAGA_CHDS[0]))
        .unwrap();
    assert_eq!(std::fs::read(kept_at(&env, copy)).unwrap(), bytes);
    assert_eq!(env.set_kept(), saga_released(&folder), "nothing lost");

    env.follow_up(plan.id);
    env.audit(title_select_engine::AuditEvidence::Full);
    let plan = env.repair_plan(&[]);
    assert_eq!(item_of(&plan, SAGA[0]).chosen, RepairChoice::KeepAside);
    env.repair(&plan);
    assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
    assert_eq!(
        env.set_kept(),
        saga_released_with(&folder, Some(SAGA_CHDS[0]))
    );
}

/// A gone disc's CHD put back as its release is released, while the .m3u or the other disc's CHD is being set aside,
/// is looked for again before its placement is forgotten: it stays placed and recorded, and the release is left
/// parted, for the next repair to release the rest of.
#[test]
fn a_gone_disc_put_back_as_its_release_is_released_stays_placed() {
    let Some(chdman) = chdman() else { return };
    for put_back in ["Saga (USA).m3u/Saga (USA).m3u", SAGA_CHDS[0]] {
        let mut env = Env::new(Some(&chdman));
        let (folder, _) = damaged_saga(&mut env, &chdman, 2);
        let path = env.dir().join(SAGA_CHDS[1]);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        env.audit(title_select_engine::AuditEvidence::Full);
        let plan = env.repair_plan(&[]);
        let seq = keeping(&plan, put_back);
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
                if at == Fault::RepairMoved(seq) {
                    std::fs::write(&path, &bytes).unwrap();
                }
            })));
        env.repair(&plan);
        env.store.hook_faults(None);
        assert_eq!(
            env.placed_paths(),
            [SAGA_CHDS[1]],
            "{put_back}: the CHD put back stays placed"
        );
        each_disc_once(&env, &folder, put_back, None);
    }
}

/// A release laid out flat both of whose CHDs are gone, and one of whose SBI files changed after the plan was made so
/// that it can't be set aside, leaves both discs recorded: the gone placements are forgotten only with the last file
/// beside them, which needs every one before it. With disc 2's SBI file changed, disc 1's is kept aside; with disc
/// 1's changed, disc 2's is skipped. Audited again, the next repair finds the release by its records and sets the rest
/// aside.
#[test]
fn a_release_whose_every_chd_is_gone_stays_recorded_until_its_last_side_file_is_kept() {
    let Some(chdman) = chdman() else { return };
    for changed in [2, 1] {
        let mut env = Env::new(Some(&chdman));
        let folder = flat_saga(&mut env, &chdman);
        for n in 1..=2 {
            std::fs::remove_file(env.dir().join(format!("Saga (USA) (Disc {n}).chd"))).unwrap();
        }
        env.audit(title_select_engine::AuditEvidence::Full);
        let plan = env.repair_plan(&[]);
        assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
        let sbi = env.dir().join(format!("Saga (USA) (Disc {changed}).sbi"));
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
                if at == Fault::RepairApproving {
                    std::fs::write(&sbi, b"another SBI file").unwrap();
                }
            })));
        env.repair(&plan);
        env.store.hook_faults(None);
        let kept: Vec<String> = match changed {
            2 => vec![placed_path(folder.id, "Saga (USA) (Disc 1).sbi")],
            _ => Vec::new(),
        };
        assert_eq!(env.set_kept(), kept, "disc {changed}'s SBI file changed");
        assert_eq!(
            env.placed_paths(),
            ["Saga (USA) (Disc 1).chd", "Saga (USA) (Disc 2).chd"],
            "disc {changed}'s SBI file changed: both discs stay recorded"
        );
        assert!(
            env.dir()
                .join(format!("Saga (USA) (Disc {changed}).sbi"))
                .exists()
        );

        env.audit(title_select_engine::AuditEvidence::Full);
        env.repair(&env.repair_plan(&[]));
        assert!(!saga_anywhere_placed(&env), "{:?}", tree(&env.dir()));
        assert_eq!(
            env.set_kept(),
            [
                placed_path(folder.id, "Saga (USA) (Disc 1).sbi"),
                placed_path(folder.id, "Saga (USA) (Disc 2).sbi"),
            ],
            "disc {changed}'s SBI file changed"
        );
    }
}

/// Scans the ingest folder again, as you do once the audit after a release has run.
fn rescan(env: &Env) {
    let job = env.store.enqueue_ingest_scan("in", "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env.store.run_ingest_scan_job(&env.config, job.id).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
}

/// Has the source set store Saga's system as TorrentZip.
fn store_as_torrentzip(env: &Env) {
    let mut policy = env.store.storage_policy().unwrap();
    policy
        .source_sets
        .systems
        .entry(SYSTEM.into())
        .or_default()
        .format = Some(StorageFormat::TorrentZip);
    env.store.set_storage_policy(&policy, "Operator").unwrap();
}

/// Whether `plan` has no step for either disc of Saga.
fn plans_nothing_for_saga(plan: &RepairPlan) -> bool {
    !plan
        .content
        .steps
        .iter()
        .any(|step| step.item.starts_with("game:Saga"))
}

/// With a disc's CHD gone and the other disc's CHD placed but read by a quick inventory only, by its claim, the release
/// isn't released: nothing is filled for either disc, not even the gone one from an ingest folder, as that would split
/// the game between the source set and the play library.
#[test]
fn a_gone_disc_beside_a_placed_chd_read_only_by_its_claim_is_never_filled() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (_folder, _) = damaged_saga(&mut env, &chdman, 2);
    store_as_torrentzip(&env);
    std::fs::remove_file(env.dir().join(SAGA_CHDS[1])).unwrap();
    env.audit(title_select_engine::AuditEvidence::Quick);
    let zip = saga_zip(&env, 2);
    ingest(&mut env, &chdman, "Saga 2.zip", &zip);
    let plan = env.repair_plan(&[]);
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|outcome| outcome.done));
    assert!(env.dir().join(SAGA_CHDS[0]).is_file());
    assert!(env.placed_paths().contains(&SAGA_CHDS[0].to_owned()));
    assert!(
        env.in_set(SAGA[1]).is_empty(),
        "disc 1 stays placed but disc 2 was filled: {:?}",
        env.in_set(SAGA[1])
    );
}

/// An incremental audit reuses a placed CHD's payload evidence only while the CHD is as that evidence read it: one
/// rewritten since under the same size and modification time is neither released nor repaired.
#[test]
fn a_placed_chd_rewritten_under_its_stamp_since_an_incremental_audit_is_left_as_it_is() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    damaged_saga(&mut env, &chdman, 2);
    env.audit(title_select_engine::AuditEvidence::Quick);
    env.audit(title_select_engine::AuditEvidence::Incremental);
    let plan = env.repair_plan(&[]);
    assert!(only_keeps(&plan));
    assert_eq!(item_of(&plan, SAGA[0]).chosen, RepairChoice::KeepAside);
    assert_eq!(item_of(&plan, SAGA[1]).chosen, RepairChoice::KeepAside);
    let changed = env.dir().join(SAGA_CHDS[0]);
    corrupt_keeping_stamp(&changed);
    let plan = env.repair_plan(&[]);
    assert!(plans_nothing_for_saga(&plan), "{:#?}", plan.content);
}

/// With a disc's CHD gone and the other disc's CHD changed since the full audit, the release isn't released, and the
/// gone disc isn't filled from an ingest folder while the other is still placed.
#[test]
fn a_gone_disc_beside_a_placed_chd_changed_since_the_full_audit_is_never_filled() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    store_as_torrentzip(&env);
    std::fs::remove_file(env.dir().join(SAGA_CHDS[1])).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    corrupt_keeping_stamp(&env.dir().join(SAGA_CHDS[0]));
    let zip = saga_zip(&env, 2);
    ingest(&mut env, &chdman, "Saga 2.zip", &zip);
    let plan = env.repair_plan(&[]);
    let gone = item_of(&plan, SAGA[1]);
    assert_eq!(gone.chosen, RepairChoice::Leave);
    let note = gone.note.as_deref().unwrap_or_default();
    assert!(
        note.contains(&format!(
            "{}, a disc of Saga (USA), is still placed as a CHD",
            placed_path(folder.id, SAGA_CHDS[0])
        )) && note.contains("Run a full audit"),
        "{note}"
    );
    assert!(plans_nothing_for_saga(&plan), "{:#?}", plan.content);
}

/// A repair stopped after disc 1's CHD is moved into the kept files and before that is recorded; a CHD is put back
/// where it was before the restart. Recovery records the kept copy, but the CHD put back stays placed and recorded, so
/// the next repair sees it: the release is parted, never forgotten while a CHD of it is in the play library folder.
#[test]
fn a_chd_put_back_before_its_keep_is_recorded_stays_placed() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    let plan = env.repair_plan(&[]);
    let seq = keeping(&plan, SAGA_CHDS[0]);
    let path = env.dir().join(SAGA_CHDS[0]);
    let bytes = std::fs::read(&path).unwrap();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairMoved(seq) {
                panic!("stopped before recording the kept CHD");
            }
        })));
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store
            .run_repair_job(&env.config, approved.job.unwrap(), None)
    }));
    env.store.hook_faults(None);
    assert!(stopped.is_err());
    std::fs::write(&path, &bytes).unwrap();
    let env = env.restarted(false);
    env.follow_up(plan.id);
    assert!(path.is_file());
    assert!(
        env.set_kept()
            .contains(&placed_path(folder.id, SAGA_CHDS[0]))
    );
    assert!(
        env.placed_paths().contains(&SAGA_CHDS[0].to_owned()),
        "a restored CHD is orphaned: {:?}",
        env.placed_paths(),
    );
    env.audit(title_select_engine::AuditEvidence::Full);
    let next = env.repair_plan(&[]);
    assert!(only_keeps(&next));
    keeping(&next, SAGA_CHDS[0]);
}

/// With disc 1's CHD damaged, disc 2's gone and a quick inventory since, nothing is planned for either disc: not a
/// fill of the gone disc from an ingest folder.
#[test]
fn a_gone_disc_beside_a_damaged_chd_read_only_by_its_claim_is_never_filled() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    damaged_saga(&mut env, &chdman, 1);
    std::fs::remove_file(env.dir().join(SAGA_CHDS[1])).unwrap();
    env.audit(title_select_engine::AuditEvidence::Quick);
    let zip = saga_zip(&env, 2);
    ingest(&mut env, &chdman, "Saga 2.zip", &zip);
    let plan = env.repair_plan(&[]);
    assert!(plans_nothing_for_saga(&plan), "{:#?}", plan.content);
}

/// A release left part way, disc 1 kept and disc 2 still placed, with a copy of disc 1 ingested since: after a full
/// audit or a quick inventory, the next repair only sets disc 2 aside, or leaves it, and fills neither disc.
#[test]
fn a_parted_release_with_an_ingest_copy_is_never_filled() {
    let Some(chdman) = chdman() else { return };
    for evidence in [
        title_select_engine::AuditEvidence::Full,
        title_select_engine::AuditEvidence::Quick,
    ] {
        let mut env = Env::new(Some(&chdman));
        let (folder, _) = damaged_saga(&mut env, &chdman, 1);
        let first = env.repair_plan(&[]);
        let sibling = env.dir().join(SAGA_CHDS[1]);
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::RepairApproving {
                    let mut bytes = std::fs::read(&sibling).unwrap();
                    bytes.push(0);
                    std::fs::write(&sibling, bytes).unwrap();
                }
            })));
        env.repair(&first);
        env.store.hook_faults(None);
        assert_eq!(env.placed_paths(), [SAGA_CHDS[1]]);
        assert!(
            env.set_kept()
                .contains(&placed_path(folder.id, SAGA_CHDS[0]))
        );
        store_as_torrentzip(&env);
        env.audit(evidence);
        let zip = saga_zip(&env, 1);
        ingest(&mut env, &chdman, "Saga 1.zip", &zip);
        let next = env.repair_plan(&[]);
        assert!(
            !next
                .content
                .steps
                .iter()
                .any(|step| step.item.starts_with("game:Saga")
                    && !matches!(step.step, RepairStep::Keep { .. })),
            "{evidence:?}: {:#?}",
            next.content.steps
        );
    }
}

/// A CHD holding a conflicting entry, as one disc copied over the other makes it, is set aside once, by its release,
/// never by a second Keep of its conflicting entry.
#[test]
fn a_conflicting_entry_in_a_released_chd_is_set_aside_once() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 1);
    std::fs::copy(env.dir().join(SAGA_CHDS[1]), env.dir().join(SAGA_CHDS[0])).unwrap();
    let report = env.audit(title_select_engine::AuditEvidence::Full);
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.finding == title_select_api::Finding::Conflicting)
    );
    let plan = env.repair_plan(&[]);
    assert!(
        plan.content
            .items
            .iter()
            .any(|item| item.kind == RepairKind::Conflict)
    );
    let mut paths = std::collections::HashSet::new();
    for step in &plan.content.steps {
        if let RepairStep::Keep { path, .. } = &step.step {
            assert!(paths.insert(path));
        }
    }
    env.repair(&plan);
    each_disc_once(&env, &folder, "conflicting entry released", None);
    assert!(!saga_anywhere_placed(&env));
}

/// With a good copy of both discs in an ingest folder before the release, the repair that releases the release only
/// sets it aside. The healthy disc's copy was assessed as already held while its CHD was placed, which its note says:
/// once the audit has run and the ingest folder is scanned again, the next repair fills both discs, and the next build
/// places both again as CHDs a full audit verifies.
#[test]
fn a_two_disc_release_with_copies_ingested_before_it_is_released_is_rebuilt_by_the_next_repair() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, selection) = damaged_saga(&mut env, &chdman, 2);
    for n in [1, 2] {
        let zip = saga_zip(&env, n);
        ingest(&mut env, &chdman, &format!("Saga {n}.zip"), &zip);
    }
    let plan = env.repair_plan(&[]);
    assert!(only_keeps(&plan), "{:#?}", plan.content.steps);
    let note = item_of(&plan, SAGA[0]).note.clone().unwrap_or_default();
    assert!(
        note.contains("scan that folder again after the audit"),
        "{note}"
    );
    let done = env.repair(&plan);
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(env.set_kept(), saga_released(&folder));
    assert!(env.in_set("Saga").is_empty(), "{:?}", env.in_set("Saga"));

    rescan(&env);
    let done = env.repair(&env.repair_plan(&[]));
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
    assert_eq!(saga_home(&env, 1), saga_roms(1));
    assert_eq!(saga_home(&env, 2), saga_roms(2));
    let job = env.build(&env.plan(&folder, selection));
    assert_eq!(job.state, JobState::Succeeded, "{:?}", states(&env, &job));
    assert_eq!(env.placed_paths(), SAGA_CHDS);
    assert!(env.in_set("Saga").is_empty(), "{:?}", env.in_set("Saga"));
    env.audit(title_select_engine::AuditEvidence::Full);
    assert_eq!(env.status(SAGA[0]), GameStatus::Complete);
    assert_eq!(env.status(SAGA[1]), GameStatus::Complete);
}

/// Disc 1's folder moved away and made again, with disc 1's CHD put back at its path, as its Keep is recorded, on one
/// filesystem and across two: the CHD stays placed and the copy stays kept, so the release is parted, and the next
/// repair after a full audit sets disc 1 aside again.
#[test]
fn a_chd_put_back_in_a_replaced_folder_as_its_keep_is_recorded_stays_placed() {
    a_chd_put_back_in_a_replaced_folder(false);
}

#[test]
fn a_chd_put_back_in_a_replaced_folder_as_its_keep_is_recorded_across_mounts_stays_placed() {
    a_chd_put_back_in_a_replaced_folder(true);
}

fn a_chd_put_back_in_a_replaced_folder(across: bool) {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    env.store.copy_across_filesystems(across);
    let plan = env.repair_plan(&[]);
    let seq = keeping(&plan, SAGA_CHDS[0]);
    let path = env.dir().join(SAGA_CHDS[0]);
    let bytes = std::fs::read(&path).unwrap();
    let parent = path.parent().unwrap().to_owned();
    let away = env.dir().join("moved-release");
    let restored = path.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            let at = match across {
                false => fault == Fault::RepairMoved(seq),
                true => fault == Fault::RepairCarryChecking(seq),
            };
            if at {
                std::fs::rename(&parent, &away).unwrap();
                std::fs::create_dir(&parent).unwrap();
                std::fs::write(&restored, &bytes).unwrap();
            }
        })));
    env.repair(&plan);
    env.store.hook_faults(None);
    assert!(path.is_file());
    assert!(
        env.set_kept()
            .contains(&placed_path(folder.id, SAGA_CHDS[0]))
    );
    assert!(
        env.placed_paths().contains(&SAGA_CHDS[0].to_owned()),
        "across={across}: a restored CHD is orphaned: {:?}",
        env.placed_paths()
    );
    env.audit(title_select_engine::AuditEvidence::Full);
    let next = env.repair_plan(&[]);
    assert!(only_keeps(&next), "{:#?}", next.content);
    keeping(&next, SAGA_CHDS[0]);
    env.repair(&next);
    assert!(!path.exists());
    assert!(env.placed_paths().is_empty(), "{:?}", env.placed_paths());
    assert_eq!(
        env.set_kept()
            .iter()
            .filter(|k| **k == placed_path(folder.id, SAGA_CHDS[0]))
            .count(),
        2,
        "{:?}",
        env.set_kept()
    );
}

/// The play library folder moved away and made again, with everything in it moved into the new one and disc 2's CHD
/// put back, as disc 2's Keep is recorded: the folder held is no longer the one at its path, so disc 2 stays placed and
/// its copy stays kept, and the next repair after a full audit sets disc 2 aside again.
#[test]
fn a_chd_put_back_in_a_replaced_library_folder_as_its_keep_is_recorded_stays_placed() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    let (folder, _) = damaged_saga(&mut env, &chdman, 2);
    let plan = env.repair_plan(&[]);
    let seq = keeping(&plan, SAGA_CHDS[1]);
    let path = env.dir().join(SAGA_CHDS[1]);
    let bytes = std::fs::read(&path).unwrap();
    let lib = env.dir();
    let away = env.tmp.path().join("moved-library");
    let restored = path.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::RepairMoved(seq) {
                std::fs::rename(&lib, &away).unwrap();
                std::fs::create_dir(&lib).unwrap();
                for entry in std::fs::read_dir(&away).unwrap() {
                    let entry = entry.unwrap();
                    std::fs::rename(entry.path(), lib.join(entry.file_name())).unwrap();
                }
                std::fs::write(&restored, &bytes).unwrap();
            }
        })));
    env.repair(&plan);
    env.store.hook_faults(None);
    assert!(path.is_file());
    assert!(
        env.set_kept()
            .contains(&placed_path(folder.id, SAGA_CHDS[1]))
    );
    assert!(
        env.placed_paths().contains(&SAGA_CHDS[1].to_owned()),
        "a restored CHD is orphaned: {:?}",
        env.placed_paths()
    );
    env.audit(title_select_engine::AuditEvidence::Full);
    let next = env.repair_plan(&[]);
    assert!(only_keeps(&next), "{:#?}", next.content);
    keeping(&next, SAGA_CHDS[1]);
    env.repair(&next);
    assert!(!path.exists());
    assert!(env.placed_paths().is_empty(), "{:?}", env.placed_paths());
}

/// Both CHDs and the .m3u gone, disc 2 filled in the source set by a repair approved before disc 1's CHD was put back
/// by hand: the release is split, a limit the README states. The next full audit finds disc 1's CHD placed, and the
/// next repair writes nothing more for either disc.
#[test]
fn a_release_split_by_a_chd_put_back_after_its_repair_was_approved_is_written_no_further() {
    let Some(chdman) = chdman() else { return };
    let mut env = Env::new(Some(&chdman));
    damaged_saga(&mut env, &chdman, 2);
    store_as_torrentzip(&env);
    let path = env.dir().join(SAGA_CHDS[0]);
    let bytes = std::fs::read(&path).unwrap();
    for chd in SAGA_CHDS {
        std::fs::remove_file(env.dir().join(chd)).unwrap();
    }
    std::fs::remove_file(env.dir().join("Saga (USA).m3u/Saga (USA).m3u")).unwrap();
    env.audit(title_select_engine::AuditEvidence::Full);
    let zip = saga_zip(&env, 2);
    ingest(&mut env, &chdman, "Saga 2.zip", &zip);
    let plan = env.repair_plan(&[]);
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    std::fs::write(&path, &bytes).unwrap();
    env.store.claim_job().unwrap().unwrap();
    env.store
        .run_repair_job(&env.config, approved.job.unwrap(), None)
        .unwrap();
    env.follow_up(plan.id);
    assert!(env.placed_paths().contains(&SAGA_CHDS[0].to_owned()));
    assert!(
        !env.in_set(SAGA[1]).is_empty(),
        "the split this test starts from"
    );
    env.audit(title_select_engine::AuditEvidence::Full);
    let next = env.repair_plan(&[]);
    assert!(plans_nothing_for_saga(&next), "{:#?}", next.content);
}
