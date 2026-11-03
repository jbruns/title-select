//! The app shell's pure logic: where old workbench paths moved, the navigation badges' counts, and which failed Jobs
//! the Operator has seen.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use title_select_api::ingest::{IngestFolder, ItemState};
use title_select_api::{Job, JobState};

/// The workbench paths from before #71, each with where it moved. A path under an old one moves with it.
const MOVED: &[(&str, &str)] = &[
    ("/ingest-folders", "/ingest"),
    ("/dat-revisions", "/dats"),
    ("/jobs", "/activity"),
    ("/storage-policy", "/settings/storage"),
    ("/about", "/settings/about"),
];

/// Where a workbench path from before #71 moved, keeping its query and fragment; `None` when it didn't move.
pub fn moved(path: &str) -> Option<String> {
    let split = path.find(['?', '#']).unwrap_or(path.len());
    let (route, rest) = path.split_at(split);
    MOVED.iter().find_map(|(old, new)| {
        let tail = route.strip_prefix(old)?;
        (tail.is_empty() || tail.starts_with('/')).then(|| format!("{new}{tail}{rest}"))
    })
}

/// How many items across the Ingest Folders wait on a decision: to plan them out of the folder or dismiss them.
pub fn waiting_on_decision(folders: &[IngestFolder]) -> u64 {
    folders
        .iter()
        .flat_map(|f| &f.counts)
        .filter(|(state, _)| {
            matches!(
                state,
                ItemState::Assessed
                    | ItemState::AlreadyHeld
                    | ItemState::Unreadable
                    | ItemState::Unsupported
            )
        })
        .map(|(_, n)| n)
        .sum()
}

/// How many Jobs are Running, plus those that Failed and the Operator hasn't seen.
pub fn needing_attention<'a>(jobs: impl IntoIterator<Item = &'a Job>, seen: &SeenJobs) -> u64 {
    jobs.into_iter()
        .filter(|job| match job.state {
            JobState::Running => true,
            JobState::Failed => !seen.is_seen(job.id),
            _ => false,
        })
        .count() as u64
}

/// The failed Jobs the Operator opened, kept in local storage. Bounded: when it holds more than [`SeenJobs::CAP`]
/// IDs it forgets the oldest and counts every Job up to the newest forgotten one as seen, since Job IDs only grow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeenJobs {
    /// Every Job at or below this was seen.
    floor: i64,
    ids: BTreeSet<i64>,
}

impl SeenJobs {
    pub const CAP: usize = 200;

    pub fn is_seen(&self, id: i64) -> bool {
        id <= self.floor || self.ids.contains(&id)
    }

    pub fn mark(&mut self, id: i64) {
        if self.is_seen(id) {
            return;
        }
        self.ids.insert(id);
        while self.ids.len() > Self::CAP {
            if let Some(oldest) = self.ids.pop_first() {
                self.floor = oldest;
            }
        }
    }
}

/// Stored as `<floor>;<id>,<id>,…`.
impl fmt::Display for SeenJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{};", self.floor)?;
        let ids: Vec<String> = self.ids.iter().map(i64::to_string).collect();
        f.write_str(&ids.join(","))
    }
}

impl FromStr for SeenJobs {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(SeenJobs::default());
        }
        let bad = || format!("not a list of seen jobs: {s}");
        let (floor, ids) = s.split_once(';').ok_or_else(bad)?;
        let mut seen = SeenJobs {
            floor: floor.parse().map_err(|_| bad())?,
            ids: BTreeSet::new(),
        };
        for id in ids.split(',').filter(|id| !id.is_empty()) {
            seen.mark(id.parse().map_err(|_| bad())?);
        }
        Ok(seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::Workers;

    #[test]
    fn old_paths_move_to_their_new_place() {
        assert_eq!(moved("/ingest-folders").as_deref(), Some("/ingest"));
        assert_eq!(
            moved("/ingest-folders/downloads").as_deref(),
            Some("/ingest/downloads")
        );
        assert_eq!(moved("/dat-revisions").as_deref(), Some("/dats"));
        assert_eq!(moved("/jobs").as_deref(), Some("/activity"));
        assert_eq!(
            moved("/storage-policy").as_deref(),
            Some("/settings/storage")
        );
        assert_eq!(moved("/about").as_deref(), Some("/settings/about"));
    }

    #[test]
    fn a_moved_path_keeps_its_query_and_fragment() {
        assert_eq!(
            moved("/ingest-folders/My%20Drop?row=3#items").as_deref(),
            Some("/ingest/My%20Drop?row=3#items")
        );
        assert_eq!(
            moved("/about#default-orders").as_deref(),
            Some("/settings/about#default-orders")
        );
    }

    #[test]
    fn paths_that_did_not_move_stay() {
        assert_eq!(moved("/ingest"), None);
        assert_eq!(moved("/profiles"), None);
        assert_eq!(moved("/jobsite"), None);
        assert_eq!(moved("/additions"), None);
    }

    fn folder(counts: Vec<(ItemState, u64)>) -> IngestFolder {
        IngestFolder {
            root: "downloads".into(),
            path: "/srv/downloads".into(),
            id: Some(1),
            settle_secs: 0,
            scan_every_minutes: None,
            last_scan_at: None,
            counts,
        }
    }

    #[test]
    fn items_you_can_plan_or_dismiss_wait_on_a_decision_across_folders() {
        let folders = [
            folder(vec![
                (ItemState::Assessed, 3),
                (ItemState::Arriving, 4),
                (ItemState::Moved, 9),
                (ItemState::Dismissed, 2),
            ]),
            folder(vec![
                (ItemState::AlreadyHeld, 1),
                (ItemState::Unreadable, 1),
                (ItemState::Unsupported, 1),
                (ItemState::Failed, 5),
                (ItemState::Stale, 6),
                (ItemState::Gone, 7),
            ]),
        ];
        assert_eq!(waiting_on_decision(&folders), 6);
        assert_eq!(waiting_on_decision(&[]), 0);
    }

    fn job(id: i64, state: JobState) -> Job {
        Job {
            id,
            kind: Default::default(),
            state,
            source_set: "Atari 2600".into(),
            dat_revision: "ab".repeat(32),
            actor: "operator".into(),
            attempt: 1,
            requeued_from: None,
            audit: None,
            profile: None,
            selection: None,
            library_folder: None,
            ingest_folder: None,
            repair: None,
            additions_set: None,
            cancel_requested_at: None,
            plan: None,
            evidence: None,
            error: None,
            workers: Workers {
                readers: 1,
                hashers: 1,
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            version: 1,
        }
    }

    #[test]
    fn running_and_unseen_failed_jobs_need_attention() {
        let jobs = [
            job(1, JobState::Failed),
            job(2, JobState::Failed),
            job(3, JobState::Running),
            job(4, JobState::Queued),
            job(5, JobState::Succeeded),
            job(6, JobState::Interrupted),
        ];
        let mut seen = SeenJobs::default();
        assert_eq!(needing_attention(&jobs, &seen), 3);
        seen.mark(2);
        assert_eq!(needing_attention(&jobs, &seen), 2);
    }

    #[test]
    fn a_seen_job_stays_seen_after_a_round_trip_through_storage() {
        let mut seen = SeenJobs::default();
        seen.mark(7);
        seen.mark(3);
        let stored: SeenJobs = seen.to_string().parse().unwrap();
        assert!(stored.is_seen(7));
        assert!(stored.is_seen(3));
        assert!(!stored.is_seen(5));
        assert_eq!(stored, seen);
    }

    #[test]
    fn nothing_is_seen_when_storage_is_empty_or_garbled() {
        assert_eq!("".parse::<SeenJobs>(), Ok(SeenJobs::default()));
        assert!("not;ids".parse::<SeenJobs>().is_err());
    }

    #[test]
    fn past_its_cap_it_forgets_the_oldest_but_still_counts_them_seen() {
        let mut seen = SeenJobs::default();
        let cap = SeenJobs::CAP as i64;
        for id in (1..=cap + 10).map(|n| n * 2) {
            seen.mark(id);
        }
        assert_eq!(seen.ids.len(), SeenJobs::CAP);
        // The oldest marked IDs fell under the floor, and so did the unmarked ones between them.
        assert!(seen.is_seen(2));
        assert!(seen.is_seen(3));
        assert!(seen.is_seen((cap + 10) * 2));
        assert!(!seen.is_seen((cap + 10) * 2 - 1));
        assert!(!seen.is_seen((cap + 10) * 2 + 1));
    }
}
