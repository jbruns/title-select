//! Follow-up scans of Ingest Folders. A scan that leaves items arriving is followed by one more scan of that folder
//! once the earliest of them could have settled, so an item dropped in is assessed without the Operator starting
//! another scan. Each folder has at most one follow-up pending; the scan that ends last decides it. Nothing is kept
//! across restarts: the store says what is arriving, so the Service works the follow-ups out again as it starts.

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

/// The follow-up scan pending for each Ingest Folder, by Root, and when it is due.
#[derive(Default)]
pub struct FollowUps {
    due: Mutex<BTreeMap<String, Timestamp>>,
    wake: Notify,
}

impl FollowUps {
    /// Replaces the follow-up pending for the folder of Root `root` with one at `at`, but no sooner than
    /// [`LEAST_DELAY`] after `now`; with none when `at` is none.
    pub fn set(&self, root: &str, at: Option<Timestamp>, now: Timestamp) {
        {
            let mut due = self.due.lock().unwrap();
            match at {
                Some(at) => {
                    due.insert(root.to_owned(), at.max(now + LEAST_DELAY));
                }
                None => {
                    due.remove(root);
                }
            }
        }
        self.wake.notify_one();
    }

    /// When the follow-up pending for the folder of Root `root` is due, if one is.
    pub fn due_at(&self, root: &str) -> Option<Timestamp> {
        self.due.lock().unwrap().get(root).copied()
    }

    /// Takes every follow-up due by `now`, by Root, and says when the next one left is due.
    fn take_due(&self, now: Timestamp) -> (Vec<String>, Option<Timestamp>) {
        let mut due = self.due.lock().unwrap();
        let ready: Vec<String> = due
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(root, _)| root.clone())
            .collect();
        for root in &ready {
            due.remove(root);
        }
        (ready, due.values().min().copied())
    }
}

/// Sets the follow-up of the folder a scan `job` scanned, now that it has ended: when its earliest arriving item
/// could have settled, or none. A scan that did not succeed leaves it as it was.
pub fn scan_ended(store: &Store, config: &Config, follow_ups: &FollowUps, job: &Job) {
    if job.kind != JobKind::IngestScan || job.state != JobState::Succeeded {
        return;
    }
    let Some(root) = job.ingest_folder.as_deref() else {
        return;
    };
    follow_up(store, config, follow_ups, root);
}

fn follow_up(store: &Store, config: &Config, follow_ups: &FollowUps, root: &str) {
    match store.ingest_follow_up_at(config, root) {
        Ok(at) => follow_ups.set(root, at, Timestamp::now()),
        Err(err) => {
            eprintln!("title-select: cannot tell when to scan ingest folder {root} again: {err}")
        }
    }
}

/// Queues each follow-up scan as it falls due, from the follow-ups the store's arriving items call for now. A
/// follow-up that meets a scan already queued or running is dropped: that scan sets the next one as it ends.
pub async fn run(store: Arc<Store>, config: Arc<Config>, jobs: Arc<Jobs>) {
    let seeded = {
        let (store, config, jobs) = (store.clone(), config.clone(), jobs.clone());
        tokio::task::spawn_blocking(move || -> title_select_store::Result<()> {
            for folder in store.ingest_folders(&config)? {
                if folder.id.is_some() {
                    follow_up(&store, &config, &jobs.follow_ups, &folder.root);
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
        let now = Timestamp::now();
        let (ready, next) = jobs.follow_ups.take_due(now);
        for root in ready {
            let store = store.clone();
            let queued =
                tokio::task::spawn_blocking(move || store.enqueue_ingest_scan(&root, ACTOR)).await;
            match queued {
                Ok(Ok(job)) => jobs.changed(job),
                Ok(Err(title_select_store::Error::Conflict(_)))
                | Ok(Err(title_select_store::Error::JobActive { .. })) => {}
                Ok(Err(err)) => eprintln!("title-select: cannot queue a follow-up scan: {err}"),
                Err(err) => eprintln!("title-select: cannot queue a follow-up scan: {err}"),
            }
        }
        match next {
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

    #[test]
    fn each_folder_has_one_follow_up_and_the_latest_scan_decides_it() {
        let follow_ups = FollowUps::default();
        follow_ups.set("drop", Some(at(60)), at(0));
        follow_ups.set("other", Some(at(90)), at(0));
        follow_ups.set("drop", Some(at(75)), at(10));
        assert_eq!(
            follow_ups.due_at("drop"),
            Some(at(75)),
            "replaced, not added"
        );

        assert_eq!(follow_ups.take_due(at(74)), (vec![], Some(at(75))));
        assert_eq!(
            follow_ups.take_due(at(80)),
            (vec!["drop".to_owned()], Some(at(90)))
        );
        assert_eq!(follow_ups.due_at("drop"), None, "taken once");

        follow_ups.set("other", None, at(81));
        assert_eq!(follow_ups.take_due(at(100)), (vec![], None));
    }

    #[test]
    fn a_follow_up_already_due_waits_the_least_delay() {
        let follow_ups = FollowUps::default();
        follow_ups.set("drop", Some(at(5)), at(10));
        assert_eq!(follow_ups.due_at("drop"), Some(at(12)));
    }
}
