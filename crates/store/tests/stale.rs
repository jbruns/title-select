//! Stale Source Sets: what a stat walk finds against the latest Audit, and what a newer DAT Revision does to a
//! Source Set that Follows its DAT, from synthetic DATs and Source Sets.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::*;
use title_select_api::{AuditEvidence, JobKind, JobState, SourceSetStale};
use title_select_store::config::Config;
use title_select_store::{Error, Store};

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

/// A store with the Source Sets Alpha and Beta, both bound to one DAT, in the read-only Root `sets`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["sets/Alpha", "sets/Beta", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            tmp.path().join("sets").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let env = Env { tmp, store, config };
        env.import("1", &games());
        for set in ["Alpha", "Beta"] {
            env.store
                .add_source_set(&env.config, "sets", set, "Test/Test - System")
                .unwrap();
            write_file(&env.set(set).join("Pong (USA).a26"), b"pong");
            write_zip(
                &env.set(set).join("Combat (USA).zip"),
                &[("Combat (USA).a26", b"combat")],
            );
        }
        env
    }

    fn set(&self, name: &str) -> PathBuf {
        self.tmp.path().join("sets").join(name)
    }

    fn import(&self, version: &str, games: &[(&str, Vec<RomSpec>)]) {
        let dat = write_dat_version(
            &self.tmp.path().join("dats"),
            &format!("test-{version}.dat"),
            version,
            games,
        );
        self.store.import_dat(&dat, "Test", None).unwrap();
    }

    fn audit(&self, name: &str, evidence: AuditEvidence) {
        self.store
            .audit_source_set(&self.config, name, None, evidence)
            .unwrap();
    }

    /// Queues a stat walk of `name`, then claims and runs it.
    fn walk(&self, name: &str) -> title_select_api::Job {
        let job = self.store.enqueue_stat_walk(name, "Operator").unwrap();
        assert_eq!(job.kind, JobKind::StatWalk);
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        self.store.run_stat_walk_job(&self.config, job.id).unwrap()
    }

    fn stale(&self, name: &str) -> Vec<SourceSetStale> {
        self.store.source_set(name).unwrap().stale
    }

    /// The stat walk's counts of changed, added and removed files, if it made `name` Stale.
    fn files_changed(&self, name: &str) -> Option<(u64, u64, u64)> {
        self.stale(name).into_iter().find_map(|s| match s {
            SourceSetStale::FilesChanged {
                changed,
                added,
                removed,
                ..
            } => Some((changed, added, removed)),
            _ => None,
        })
    }
}

#[test]
fn a_changed_added_or_removed_file_makes_the_source_set_stale_and_an_audit_clears_it() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    let ended = env.walk("Alpha");
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert_eq!(env.stale("Alpha"), [], "nothing changed since the audit");

    write_file(&env.set("Alpha").join("Pong (USA).a26"), b"pong, changed");
    env.walk("Alpha");
    assert_eq!(env.files_changed("Alpha"), Some((1, 0, 0)));
    env.audit("Alpha", AuditEvidence::Incremental);
    assert_eq!(env.stale("Alpha"), [], "an audit clears it");

    write_file(&env.set("Alpha").join("new.bin"), b"new");
    env.walk("Alpha");
    assert_eq!(env.files_changed("Alpha"), Some((0, 1, 0)));
    env.audit("Alpha", AuditEvidence::Incremental);

    std::fs::remove_file(env.set("Alpha").join("new.bin")).unwrap();
    env.walk("Alpha");
    assert_eq!(env.files_changed("Alpha"), Some((0, 0, 1)));
    assert_eq!(env.stale("Beta"), [], "only the source set walked");
}

#[test]
fn a_stat_walk_reads_no_payload_and_writes_nothing_in_the_source_set() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    let before = listing(&env.set("Alpha"));
    // A file that cannot be opened is still statted, so the walk never opened it.
    let pong = env.set("Alpha").join("Pong (USA).a26");
    let mode = |m: u32| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pong, std::fs::Permissions::from_mode(m)).unwrap();
    };
    mode(0o000);
    let ended = env.walk("Alpha");
    mode(0o644);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert_eq!(listing(&env.set("Alpha")), before);
}

#[test]
fn a_zip_a_quick_inventory_only_claimed_is_compared_too() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Quick);
    env.walk("Alpha");
    assert_eq!(env.stale("Alpha"), []);

    write_zip(
        &env.set("Alpha").join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat, changed")],
    );
    env.walk("Alpha");
    assert_eq!(env.files_changed("Alpha"), Some((1, 0, 0)));
}

#[test]
fn walks_that_keep_finding_the_same_are_one_record_and_one_finding_nothing_clears_it() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    write_file(&env.set("Alpha").join("new.bin"), b"new");
    env.walk("Alpha");
    env.walk("Alpha");
    let walks = env.store.stat_walks("Alpha").unwrap();
    assert_eq!(walks.len(), 1, "{walks:?}");
    assert!(walks[0].checked_at >= walks[0].found_at);

    std::fs::remove_file(env.set("Alpha").join("new.bin")).unwrap();
    env.walk("Alpha");
    assert_eq!(
        env.stale("Alpha"),
        [],
        "the files are as the audit saw them again"
    );
    assert_eq!(
        env.store.stat_walks("Alpha").unwrap().len(),
        1,
        "history keeps what was found"
    );
}

#[test]
fn a_stat_walk_needs_an_audit_to_compare_with_and_only_one_waits_per_source_set() {
    let env = Env::new();
    let refused = env.store.enqueue_stat_walk("Alpha", "Operator");
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");

    env.audit("Alpha", AuditEvidence::Incremental);
    let job = env.store.enqueue_stat_walk("Alpha", "Operator").unwrap();
    let again = env.store.enqueue_stat_walk("Alpha", "Operator");
    assert!(
        matches!(again, Err(Error::JobActive { job: j, kind: JobKind::StatWalk, .. }) if j == job.id),
        "{again:?}"
    );
}

#[test]
fn a_stat_walk_waits_behind_every_other_queued_job() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    let walk = env.store.enqueue_stat_walk("Alpha", "Operator").unwrap();
    let audit = env
        .store
        .enqueue_audit(
            "Beta",
            None,
            "Operator",
            title_select_store::Workers {
                readers: 1,
                hashers: 1,
            },
            AuditEvidence::Incremental,
        )
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, audit.id);
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, walk.id);
}

#[test]
fn a_newer_revision_makes_only_the_source_sets_that_follow_their_dat_stale() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    env.audit("Beta", AuditEvidence::Incremental);
    let followed = env.store.set_follow("Alpha", true).unwrap();
    assert!(followed.follow);
    assert_eq!(followed.stale, [], "no newer revision yet");

    let mut newer = games();
    newer.push(("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]));
    env.import("2", &newer);

    assert!(
        matches!(&env.stale("Alpha")[..], [SourceSetStale::NewerRevision { version, .. }] if version == "2"),
        "{:?}",
        env.stale("Alpha")
    );
    assert_eq!(env.stale("Beta"), [], "beta doesn't follow its DAT");
    assert!(
        env.store.job_snapshot(10).unwrap().is_empty(),
        "following never starts an audit"
    );

    let revision = env.store.source_set("Alpha").unwrap();
    assert!(revision.follow);
    env.store
        .audit_source_set(
            &env.config,
            "Alpha",
            Some(&newest_sha(&env)),
            AuditEvidence::Incremental,
        )
        .unwrap();
    assert_eq!(env.stale("Alpha"), [], "an audit against it clears it");

    env.store.set_follow("Beta", true).unwrap();
    assert_eq!(
        env.stale("Beta").len(),
        1,
        "following later shows what waits"
    );
    env.store.set_follow("Beta", false).unwrap();
    assert_eq!(env.stale("Beta"), []);
}

fn newest_sha(env: &Env) -> String {
    env.store
        .dat_revisions()
        .unwrap()
        .into_iter()
        .find(|r| r.version == "2")
        .unwrap()
        .sha256
}

fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
    let mut found: Vec<(String, u64)> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .map(|e| e.unwrap())
        .map(|e| {
            (
                e.path().to_string_lossy().into_owned(),
                e.metadata().unwrap().len(),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_stat_walk_the_service_stopped_in_is_never_queued_again() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    let job = env.store.enqueue_stat_walk("Alpha", "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();

    let interrupted = env
        .store
        .recover_jobs(title_select_store::Workers {
            readers: 1,
            hashers: 1,
        })
        .unwrap();

    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].job.id, job.id);
    assert_eq!(interrupted[0].job.state, JobState::Interrupted);
    assert_eq!(interrupted[0].requeued, None);
}

#[test]
fn a_stamped_file_that_turns_unreadable_looks_changed() {
    use std::os::unix::fs::symlink;
    let env = Env::new();
    // Pong is a symlink to a file in the root but outside the source set, which the audit reads and stamps.
    let target = env.tmp.path().join("sets/pong.bin");
    write_file(&target, b"pong");
    let pong = env.set("Alpha").join("Pong (USA).a26");
    std::fs::remove_file(&pong).unwrap();
    symlink(&target, &pong).unwrap();
    env.audit("Alpha", AuditEvidence::Incremental);
    env.walk("Alpha");
    assert_eq!(env.stale("Alpha"), [], "nothing changed since the audit");

    std::fs::remove_file(&target).unwrap();
    env.walk("Alpha");
    assert_eq!(
        env.files_changed("Alpha"),
        Some((1, 0, 0)),
        "its target was deleted"
    );

    let outside = env.tmp.path().join("pong-outside.bin");
    write_file(&outside, b"pong");
    std::fs::remove_file(&pong).unwrap();
    symlink(&outside, &pong).unwrap();
    env.walk("Alpha");
    assert_eq!(
        env.files_changed("Alpha"),
        Some((1, 0, 0)),
        "it now leaves the root"
    );
}

#[test]
fn each_newer_revision_that_made_a_followed_source_set_stale_stays_in_its_history() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    env.store.set_follow("Alpha", true).unwrap();
    let versions = |name: &str| -> Vec<String> {
        env.store
            .followed_revisions(name)
            .unwrap()
            .into_iter()
            .map(|r| r.version)
            .collect()
    };
    env.import("2", &games());
    env.import("3", &games());
    assert_eq!(versions("Alpha"), ["3", "2"], "newest first");

    let three = env
        .store
        .dat_revisions()
        .unwrap()
        .into_iter()
        .find(|r| r.version == "3")
        .unwrap()
        .sha256;
    env.store
        .audit_source_set(
            &env.config,
            "Alpha",
            Some(&three),
            AuditEvidence::Incremental,
        )
        .unwrap();
    assert_eq!(env.stale("Alpha"), [], "an audit clears it");
    assert_eq!(versions("Alpha"), ["3", "2"], "but not its history");
    env.import("4", &games());
    assert_eq!(versions("Alpha"), ["4", "3", "2"]);

    env.store.set_follow("Alpha", false).unwrap();
    assert_eq!(versions("Alpha"), ["4", "3", "2"], "unfollowing keeps it");
    env.import("5", &games());
    assert_eq!(versions("Alpha"), ["4", "3", "2"], "nor makes it stale");
    env.store.set_follow("Alpha", true).unwrap();
    assert_eq!(
        versions("Alpha"),
        ["5", "4", "3", "2"],
        "following again finds the newer revision waiting"
    );
    assert_eq!(
        versions("Beta"),
        Vec::<String>::new(),
        "beta never followed"
    );
}

/// Moves the Source Set `name`'s audits and check jobs back in time, `hours` before now.
fn backdate(env: &Env, name: &str, audits: i64, created: i64, finished: i64) {
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    let ago =
        |hours: i64| (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(hours)).to_string();
    let set = "(SELECT id FROM source_sets WHERE name = ?1)";
    db.execute(
        &format!("UPDATE audits SET finished_at = ?2 WHERE source_set_id = {set}"),
        rusqlite::params![name, ago(audits)],
    )
    .unwrap();
    db.execute(
        &format!(
            "UPDATE jobs SET created_at = ?2, finished_at = ?3
             WHERE kind = 'stat_walk' AND source_set_id = {set}"
        ),
        rusqlite::params![name, ago(created), ago(finished)],
    )
    .unwrap();
    db.execute(
        &format!(
            "UPDATE stat_walks SET found_at = ?2, checked_at = ?2 WHERE source_set_id = {set}"
        ),
        rusqlite::params![name, ago(finished)],
    )
    .unwrap();
}

/// The Source Sets the schedule queues a check of, checking each every hour.
fn due(env: &Env) -> Vec<String> {
    env.store
        .enqueue_stat_walks("schedule", std::time::Duration::from_secs(3600))
        .unwrap()
        .into_iter()
        .map(|j| j.source_set)
        .collect()
}

#[test]
fn the_schedule_checks_only_the_source_sets_whose_last_check_or_else_audit_is_older_than_its_interval()
 {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    env.audit("Beta", AuditEvidence::Incremental);
    assert_eq!(
        due(&env),
        Vec::<String>::new(),
        "both audited within the hour"
    );

    env.walk("Alpha");
    backdate(&env, "Alpha", 3, 0, 0);
    backdate(&env, "Beta", 3, 0, 0);
    assert_eq!(due(&env), ["Beta"], "alpha was checked within the hour");
}

#[test]
fn a_check_that_waited_long_to_run_is_due_again_an_interval_after_it_finished() {
    let env = Env::new();
    env.audit("Alpha", AuditEvidence::Incremental);
    env.walk("Alpha");
    // Queued 3 hours ago, it waited behind other jobs and finished a minute ago.
    backdate(&env, "Alpha", 4, 3, 0);
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    let minute_ago = (jiff::Timestamp::now() - jiff::SignedDuration::from_mins(1)).to_string();
    db.execute(
        "UPDATE jobs SET finished_at = ?1 WHERE kind = 'stat_walk'",
        [&minute_ago],
    )
    .unwrap();
    assert_eq!(
        due(&env),
        Vec::<String>::new(),
        "not due until an hour after it finished"
    );

    backdate(&env, "Alpha", 4, 3, 2);
    assert_eq!(due(&env), ["Alpha"], "due an hour after it finished");
}
