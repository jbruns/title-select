//! The app shell's pure logic: where old workbench paths moved, the navigation badges' counts, and which failed Jobs
//! the Operator has seen.

use std::collections::{BTreeMap, BTreeSet};
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

/// How many Jobs are Running, plus those that Failed and the Operator hasn't seen. `jobs` are the live Jobs, whose
/// failures may be newer than `failed`, the server's list of every Failed Job's ID.
pub fn needing_attention<'a>(
    jobs: impl IntoIterator<Item = &'a Job>,
    failed: impl IntoIterator<Item = i64>,
    seen: &SeenJobs,
) -> u64 {
    let jobs: Vec<&Job> = jobs.into_iter().collect();
    let running = jobs.iter().filter(|j| j.state == JobState::Running).count();
    (running + unseen_failures(jobs, failed, seen).len()) as u64
}

/// The IDs of the Failed Jobs the Operator hasn't seen: among the live `jobs` and `failed`, the server's list.
pub fn unseen_failures<'a>(
    jobs: impl IntoIterator<Item = &'a Job>,
    failed: impl IntoIterator<Item = i64>,
    seen: &SeenJobs,
) -> BTreeSet<i64> {
    let live = jobs
        .into_iter()
        .filter(|j| j.state == JobState::Failed)
        .map(|j| j.id);
    failed
        .into_iter()
        .chain(live)
        .filter(|id| !seen.is_seen(*id))
        .collect()
}

/// The `recent` Jobs with every Job in `failed` they lack, so that each failure the Activity badge counts has a row;
/// a Job in both keeps its newer version. Newest first.
pub fn with_failures(recent: Vec<Job>, failed: Vec<Job>) -> Vec<Job> {
    let mut all: BTreeMap<i64, Job> = BTreeMap::new();
    for job in recent.into_iter().chain(failed) {
        match all.get(&job.id) {
            Some(held) if held.version >= job.version => {}
            _ => {
                all.insert(job.id, job);
            }
        }
    }
    all.into_values().rev().collect()
}

/// The failed Jobs the Operator opened, kept in local storage. It stays bounded by forgetting Jobs that no longer
/// failed (see [`SeenJobs::keep_only`]), never by guessing that an unopened Job was seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeenJobs {
    ids: BTreeSet<i64>,
}

impl SeenJobs {
    pub fn is_seen(&self, id: i64) -> bool {
        self.ids.contains(&id)
    }

    pub fn mark(&mut self, id: i64) {
        self.ids.insert(id);
    }

    /// Forgets every seen Job not in `failed`, the server's list of every Failed Job's ID. Whether it changed.
    pub fn keep_only(&mut self, failed: impl IntoIterator<Item = i64>) -> bool {
        let failed: BTreeSet<i64> = failed.into_iter().collect();
        let before = self.ids.len();
        self.ids.retain(|id| failed.contains(id));
        self.ids.len() != before
    }
}

/// Stored as `<id>,<id>,…`.
impl fmt::Display for SeenJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<String> = self.ids.iter().map(i64::to_string).collect();
        f.write_str(&ids.join(","))
    }
}

impl FromStr for SeenJobs {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ids = s
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| format!("not a list of seen jobs: {s}"))?;
        Ok(SeenJobs { ids })
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
            next_check_at: None,
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
            job(3, JobState::Running),
            job(4, JobState::Queued),
            job(5, JobState::Succeeded),
            job(6, JobState::Interrupted),
            job(8, JobState::Failed),
        ];
        let mut seen = SeenJobs::default();
        assert_eq!(needing_attention(&jobs, [1, 2], &seen), 4);
        seen.mark(2);
        assert_eq!(needing_attention(&jobs, [1, 2], &seen), 3);
    }

    #[test]
    fn every_unseen_failure_is_listed_once_whether_live_or_fetched() {
        let jobs = [job(3, JobState::Running), job(8, JobState::Failed)];
        let mut seen = SeenJobs::default();
        seen.mark(2);
        let unseen = unseen_failures(&jobs, [1, 2, 8], &seen);
        assert_eq!(unseen.into_iter().collect::<Vec<_>>(), vec![1, 8]);
    }

    #[test]
    fn failures_older_than_the_recent_jobs_join_the_history() {
        let mut newer = job(5, JobState::Failed);
        newer.version = 2;
        let history = with_failures(
            vec![job(9, JobState::Succeeded), job(5, JobState::Failed)],
            vec![newer.clone(), job(1, JobState::Failed)],
        );
        let ids: Vec<i64> = history.iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![9, 5, 1]);
        assert_eq!(history[1], newer);
    }

    #[test]
    fn a_failure_in_both_the_list_and_the_live_jobs_counts_once() {
        let jobs = [job(8, JobState::Failed)];
        assert_eq!(needing_attention(&jobs, [8], &SeenJobs::default()), 1);
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
        assert!("not,ids".parse::<SeenJobs>().is_err());
    }

    #[test]
    fn many_marks_never_make_an_unopened_failure_seen() {
        let mut seen = SeenJobs::default();
        for id in 2..=1000 {
            seen.mark(id);
        }
        assert!(!seen.is_seen(1));
        assert!(!seen.is_seen(1001));
    }

    #[test]
    fn it_forgets_only_seen_jobs_that_no_longer_failed() {
        let mut seen = SeenJobs::default();
        for id in [1, 2, 3] {
            seen.mark(id);
        }
        assert!(seen.keep_only([2, 3, 9]));
        assert!(!seen.is_seen(1));
        assert!(seen.is_seen(2) && seen.is_seen(3));
        assert!(!seen.is_seen(9));
        assert!(!seen.keep_only([2, 3]));
    }
}
