//! Follow-up scans of Ingest Folders. A scan that leaves items arriving is followed by one more scan of that folder
//! once the earliest of them could have settled, so an item dropped in is assessed without the Operator starting
//! another scan. Each folder has at most one follow-up pending. It stays pending until a scan of the folder succeeds,
//! which replaces it or, with nothing left arriving, clears it; a scan that fails turns it into a retry that waits
//! longer each time. Nothing is kept across restarts: the store says what is arriving, so the Service works the
//! follow-ups out again as it starts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use title_select_api::{Job, JobKind, JobState};
use title_select_store::Store;
use title_select_store::config::Config;
use tokio::sync::Notify;

use crate::jobs::Jobs;

/// Who a follow-up scan's Job says started it.
pub const ACTOR: &str = "follow-up";

/// The least time from a scan's end to its follow-up, so that a folder whose items settle at once, or never leave
/// arriving, is not scanned over and over.
const LEAST_DELAY: SignedDuration = SignedDuration::from_secs(2);

/// How long a follow-up taken to be queued waits before it is due again. The scan it queued, or the one already
/// queued or running that it met, replaces it as it ends; this only matters when that scan never ends here, such as
/// one cancelled before it ran.
const RECHECK: SignedDuration = SignedDuration::from_mins(1);

/// The longest wait before retrying a folder whose scans keep failing.
const LONGEST_RETRY: SignedDuration = SignedDuration::from_mins(15);

/// How a scan of an Ingest Folder ended, as far as its follow-up goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// It succeeded; the earliest item still arriving could have settled at `settles_at`, if any is.
    Succeeded { settles_at: Option<Timestamp> },
    /// It did not succeed. `arriving` says whether the store still has items arriving in the folder.
    Failed { arriving: bool, settle_secs: u64 },
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Timestamp,
    /// The scans that failed since the last that succeeded.
    failures: u32,
}

/// The follow-up scan pending for each Ingest Folder, by Root, and when it is due.
#[derive(Default)]
pub struct FollowUps {
    pending: Mutex<BTreeMap<String, Pending>>,
    wake: Notify,
}

impl FollowUps {
    /// Sets the follow-up of the folder of Root `root` from how a scan of it ended at `now`. None is ever due sooner
    /// than [`LEAST_DELAY`] after `now`.
    pub fn after_scan(&self, root: &str, scan: Scan, now: Timestamp) {
        {
            let mut pending = self.pending.lock().unwrap();
            let next = match scan {
                Scan::Succeeded { settles_at } => settles_at.map(|at| Pending { at, failures: 0 }),
                Scan::Failed {
                    arriving,
                    settle_secs,
                } => {
                    let failures = pending.get(root).map_or(0, |p| p.failures) + 1;
                    arriving.then(|| Pending {
                        at: now + retry_after(settle_secs, failures),
                        failures,
                    })
                }
            };
            match next {
                Some(next) => {
                    pending.insert(
                        root.to_owned(),
                        Pending {
                            at: next.at.max(now + LEAST_DELAY),
                            ..next
                        },
                    );
                }
                None => {
                    pending.remove(root);
                }
            }
        }
        self.wake.notify_one();
    }

    /// When the follow-up pending for the folder of Root `root` is due, if one is.
    pub fn due_at(&self, root: &str) -> Option<Timestamp> {
        self.pending.lock().unwrap().get(root).map(|p| p.at)
    }

    /// Takes every follow-up due by `now`, by Root, to be queued. Each stays pending, due again [`RECHECK`] from
    /// now, until a scan of its folder ends.
    fn take_due(&self, now: Timestamp) -> Vec<String> {
        let mut pending = self.pending.lock().unwrap();
        let mut due = Vec::new();
        for (root, p) in pending.iter_mut() {
            if p.at <= now {
                p.at = now + RECHECK;
                due.push(root.clone());
            }
        }
        due
    }

    /// Drops the follow-up of the folder of Root `root`, and its failures, as the folder is no longer registered.
    pub fn forget(&self, root: &str) {
        self.pending.lock().unwrap().remove(root);
    }

    /// The follow-up scan of the folder of Root `root` that queuing returned, if one was queued. A folder no longer
    /// registered loses its follow-up; one that met a scan already queued or running keeps it.
    fn queued(&self, root: &str, queued: title_select_store::Result<Job>) -> Option<Job> {
        use title_select_store::Error;
        match queued {
            Ok(job) => Some(job),
            Err(Error::NotFound(_)) => {
                self.forget(root);
                None
            }
            Err(Error::Conflict(_) | Error::JobActive { .. }) => None,
            Err(err) => {
                eprintln!(
                    "title-select: cannot queue a follow-up scan of ingest folder {root}: {err}"
                );
                None
            }
        }
    }

    /// When the next follow-up is due, if any is pending.
    fn next_due(&self) -> Option<Timestamp> {
        self.pending.lock().unwrap().values().map(|p| p.at).min()
    }
}

/// How long to wait before retrying a folder after its `failures`-th failed scan in a row: the settle interval,
/// doubling with each failure, up to [`LONGEST_RETRY`].
fn retry_after(settle_secs: u64, failures: u32) -> SignedDuration {
    let base = SignedDuration::from_secs(settle_secs.min(LONGEST_RETRY.as_secs() as u64) as i64)
        .max(LEAST_DELAY);
    let doubled = base
        .checked_mul(
            1i32.checked_shl(failures.saturating_sub(1))
                .unwrap_or(i32::MAX),
        )
        .unwrap_or(LONGEST_RETRY);
    doubled.min(LONGEST_RETRY)
}

/// Sets the follow-up of the folder a scan `job` scanned, now that it has ended.
pub fn scan_ended(store: &Store, config: &Config, follow_ups: &FollowUps, job: &Job) {
    if job.kind != JobKind::IngestScan || !job.state.is_final() {
        return;
    }
    let Some(root) = job.ingest_folder.as_deref() else {
        return;
    };
    follow_up(
        store,
        config,
        follow_ups,
        root,
        job.state == JobState::Succeeded,
    );
}

/// Sets the follow-up of the folder of Root `root` after a scan that `succeeded` or not, from what the store says is
/// arriving in it.
fn follow_up(store: &Store, config: &Config, follow_ups: &FollowUps, root: &str, succeeded: bool) {
    let settles_at = match store.ingest_follow_up_at(config, root) {
        Ok(at) => at,
        Err(err) => {
            eprintln!("title-select: cannot tell when to scan ingest folder {root} again: {err}");
            return;
        }
    };
    let scan = if succeeded {
        Scan::Succeeded { settles_at }
    } else {
        Scan::Failed {
            arriving: settles_at.is_some(),
            settle_secs: config.roots.get(root).map_or(0, |r| r.settle_secs()),
        }
    };
    follow_ups.after_scan(root, scan, Timestamp::now());
}

/// Queues each follow-up scan as it falls due, from the follow-ups the store's arriving items call for now. A
/// follow-up that meets a scan already queued or running stays pending; that scan sets it as it ends.
pub async fn run(store: Arc<Store>, config: Arc<Config>, jobs: Arc<Jobs>) {
    let seeded = {
        let (store, config, jobs) = (store.clone(), config.clone(), jobs.clone());
        tokio::task::spawn_blocking(move || -> title_select_store::Result<()> {
            for folder in store.ingest_folders(&config)? {
                if folder.id.is_some() {
                    follow_up(&store, &config, &jobs.follow_ups, &folder.root, true);
                }
            }
            Ok(())
        })
        .await
    };
    match seeded {
        Ok(Ok(())) => {}
        Ok(Err(err)) => eprintln!("title-select: cannot find ingest folders to scan again: {err}"),
        Err(err) => eprintln!("title-select: cannot find ingest folders to scan again: {err}"),
    }
    loop {
        for root in jobs.follow_ups.take_due(Timestamp::now()) {
            let store = store.clone();
            let queued = {
                let root = root.clone();
                tokio::task::spawn_blocking(move || store.enqueue_ingest_scan(&root, ACTOR)).await
            };
            match queued {
                Ok(queued) => {
                    if let Some(job) = jobs.follow_ups.queued(&root, queued) {
                        jobs.changed(job);
                    }
                }
                Err(err) => eprintln!("title-select: cannot queue a follow-up scan: {err}"),
            }
        }
        match jobs.follow_ups.next_due() {
            Some(next) => {
                let wait = Duration::try_from(next.duration_since(Timestamp::now()))
                    .unwrap_or(Duration::ZERO);
                let _ = tokio::time::timeout(wait, jobs.follow_ups.wake.notified()).await;
            }
            None => jobs.follow_ups.wake.notified().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    fn settles(secs: i64) -> Scan {
        Scan::Succeeded {
            settles_at: Some(at(secs)),
        }
    }

    const NOTHING_ARRIVING: Scan = Scan::Succeeded { settles_at: None };

    fn failed(settle_secs: u64) -> Scan {
        Scan::Failed {
            arriving: true,
            settle_secs,
        }
    }

    #[test]
    fn each_folder_has_one_follow_up_and_the_latest_successful_scan_decides_it() {
        let follow_ups = FollowUps::default();
        follow_ups.after_scan("drop", settles(60), at(0));
        follow_ups.after_scan("other", settles(90), at(0));
        follow_ups.after_scan("drop", settles(75), at(10));
        assert_eq!(
            follow_ups.due_at("drop"),
            Some(at(75)),
            "replaced, not added"
        );

        assert_eq!(follow_ups.take_due(at(74)), Vec::<String>::new());
        assert_eq!(follow_ups.take_due(at(80)), ["drop"]);
        assert_eq!(
            follow_ups.take_due(at(80)),
            Vec::<String>::new(),
            "taken once"
        );

        follow_ups.after_scan("other", NOTHING_ARRIVING, at(81));
        assert_eq!(follow_ups.due_at("other"), None);
    }

    #[test]
    fn a_follow_up_already_due_waits_the_least_delay() {
        let follow_ups = FollowUps::default();
        follow_ups.after_scan("drop", settles(5), at(10));
        assert_eq!(follow_ups.due_at("drop"), Some(at(12)));
    }

    #[test]
    fn a_follow_up_taken_while_another_scan_runs_is_kept_until_that_scan_ends() {
        let follow_ups = FollowUps::default();
        follow_ups.after_scan("drop", settles(60), at(0));
        // Due, but queuing it meets a scan already running: it stays pending, checked again later.
        assert_eq!(follow_ups.take_due(at(60)), ["drop"]);
        assert_eq!(follow_ups.due_at("drop"), Some(at(60) + RECHECK));

        // That scan fails: the follow-up becomes a retry, the settle interval from now.
        follow_ups.after_scan("drop", failed(30), at(70));
        assert_eq!(follow_ups.due_at("drop"), Some(at(100)));
        assert_eq!(follow_ups.take_due(at(100)), ["drop"]);
    }

    #[test]
    fn an_unregistered_folder_has_no_follow_up_and_starts_its_retries_afresh() {
        let follow_ups = FollowUps::default();
        follow_ups.after_scan("drop", failed(60), at(0));
        follow_ups.after_scan("drop", failed(60), at(0));
        follow_ups.forget("drop");
        assert_eq!(follow_ups.due_at("drop"), None);
        assert_eq!(follow_ups.take_due(at(10_000)), Vec::<String>::new());

        follow_ups.after_scan("drop", failed(60), at(0));
        assert_eq!(
            follow_ups.due_at("drop"),
            Some(at(60)),
            "no failures carried over"
        );
    }

    #[test]
    fn a_follow_up_whose_folder_is_no_longer_registered_is_dropped_and_one_meeting_a_scan_is_kept()
    {
        let follow_ups = FollowUps::default();
        follow_ups.after_scan("gone", settles(60), at(0));
        follow_ups.after_scan("busy", settles(60), at(0));
        assert_eq!(follow_ups.take_due(at(60)), ["busy", "gone"]);

        let gone = Err(title_select_store::Error::NotFound(
            "ingest folder gone is not registered".into(),
        ));
        assert!(follow_ups.queued("gone", gone).is_none());
        assert_eq!(follow_ups.due_at("gone"), None);

        let busy = Err(title_select_store::Error::Conflict(
            "a scan is already queued".into(),
        ));
        assert!(follow_ups.queued("busy", busy).is_none());
        assert_eq!(follow_ups.due_at("busy"), Some(at(60) + RECHECK));
    }

    #[test]
    fn a_failed_scan_retries_doubling_from_the_settle_interval_up_to_fifteen_minutes() {
        let follow_ups = FollowUps::default();
        let mut retries = Vec::new();
        for _ in 0..7 {
            follow_ups.after_scan("drop", failed(60), at(0));
            retries.push(
                follow_ups
                    .due_at("drop")
                    .unwrap()
                    .duration_since(at(0))
                    .as_secs(),
            );
        }
        assert_eq!(retries, [60, 120, 240, 480, 900, 900, 900]);

        // A success starts the doubling again.
        follow_ups.after_scan("drop", settles(10), at(0));
        follow_ups.after_scan("drop", failed(60), at(0));
        assert_eq!(follow_ups.due_at("drop"), Some(at(60)));

        // Settling at once still waits the least delay between retries.
        follow_ups.after_scan("now", failed(0), at(0));
        assert_eq!(follow_ups.due_at("now"), Some(at(2)));

        // With nothing arriving, there is nothing to retry for.
        follow_ups.after_scan(
            "drop",
            Scan::Failed {
                arriving: false,
                settle_secs: 60,
            },
            at(0),
        );
        assert_eq!(follow_ups.due_at("drop"), None);
    }
}
