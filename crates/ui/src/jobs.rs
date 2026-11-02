//! What the workbench knows of the Service's Jobs, kept current by the `/events` stream.

use std::collections::BTreeMap;

use title_select_api::{Job, JobEvent, JobProgress, JobSnapshot, JobState};

/// Parses one server-sent event by its SSE event name, since the JSON alone does not say which it is.
pub fn parse_event(name: &str, data: &str) -> Result<JobEvent, String> {
    let parsed = match name {
        "state" => serde_json::from_str::<Job>(data).map(|job| JobEvent::State(Box::new(job))),
        "progress" => serde_json::from_str::<JobProgress>(data).map(JobEvent::Progress),
        "resync" => serde_json::from_str::<JobSnapshot>(data).map(JobEvent::Resync),
        other => return Err(format!("unknown event `{other}`")),
    };
    parsed.map_err(|e| format!("`{name}` event: {e}"))
}

/// A Job and, while it is Running, how far it has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJob {
    pub job: Job,
    pub progress: Option<JobProgress>,
}

/// What applying an event changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Stale, or about a Job the book no longer holds: nothing changed.
    Ignored,
    Changed,
    /// A Job reached a final state, so screens showing Audits should fetch again.
    Finished,
    /// The book was replaced by a snapshot.
    Resynced,
}

/// The Jobs, by ID, as the latest events left them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobBook {
    jobs: BTreeMap<i64, LiveJob>,
}

impl JobBook {
    /// Applies one event by the README's rule: a `state` is ignored when its `version` is at or below the Job's, a
    /// `progress` when its `version` is below it; a `resync` replaces everything.
    pub fn apply(&mut self, event: JobEvent) -> Applied {
        match event {
            JobEvent::State(job) => {
                let job = *job;
                if let Some(held) = self.jobs.get(&job.id)
                    && held.job.version >= job.version
                {
                    return Applied::Ignored;
                }
                let finished = job.state.is_final();
                self.jobs.insert(
                    job.id,
                    LiveJob {
                        job,
                        progress: None,
                    },
                );
                if finished {
                    Applied::Finished
                } else {
                    Applied::Changed
                }
            }
            JobEvent::Progress(progress) => match self.jobs.get_mut(&progress.job) {
                Some(held) if progress.version >= held.job.version => {
                    held.progress = Some(progress);
                    Applied::Changed
                }
                _ => Applied::Ignored,
            },
            JobEvent::Resync(snapshot) => {
                self.replace(snapshot.jobs);
                Applied::Resynced
            }
        }
    }

    /// Replaces the book with these Jobs, keeping the progress of any Running Job whose version is unchanged.
    pub fn replace(&mut self, jobs: Vec<Job>) {
        let mut previous = std::mem::take(&mut self.jobs);
        for job in jobs {
            let progress = previous.remove(&job.id).and_then(|held| {
                (held.job.version == job.version && job.state == JobState::Running)
                    .then_some(held.progress)
                    .flatten()
            });
            self.jobs.insert(job.id, LiveJob { job, progress });
        }
    }

    /// The Jobs a fetch of `/jobs` listed, each replaced by the book's when the book holds a newer or equal
    /// version, together with the book's other Jobs; newest first.
    pub fn overlay(&self, history: &[Job]) -> Vec<LiveJob> {
        let mut all: BTreeMap<i64, LiveJob> = history
            .iter()
            .map(|job| {
                (
                    job.id,
                    LiveJob {
                        job: job.clone(),
                        progress: None,
                    },
                )
            })
            .collect();
        for (id, held) in &self.jobs {
            match all.get(id) {
                Some(fetched) if fetched.job.version > held.job.version => {}
                _ => {
                    all.insert(*id, held.clone());
                }
            }
        }
        all.into_values().rev().collect()
    }

    pub fn get(&self, id: i64) -> Option<&LiveJob> {
        self.jobs.get(&id)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// The Jobs, newest first.
    pub fn newest_first(&self) -> Vec<LiveJob> {
        self.jobs.values().rev().cloned().collect()
    }

    /// How many Jobs are Queued or Running.
    pub fn active(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| !j.job.state.is_final())
            .count()
    }
}

/// The glossary's name for a Job's state.
pub fn state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "Queued",
        JobState::Running => "Running",
        JobState::Succeeded => "Succeeded",
        JobState::Failed => "Failed",
        JobState::Interrupted => "Interrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::Workers;

    fn job(id: i64, state: JobState, version: u64) -> Job {
        Job {
            id,
            kind: Default::default(),
            state,
            source_set: "Atari 2600".into(),
            dat_revision: "ab".repeat(32),
            actor: "operator".into(),
            attempt: 1,
            requeued_from: None,
            audit: (state == JobState::Succeeded).then_some(7),
            profile: None,
            selection: None,
            managed_folder: None,
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
            version,
        }
    }

    fn state(id: i64, s: JobState, version: u64) -> JobEvent {
        JobEvent::State(Box::new(job(id, s, version)))
    }

    fn progress(id: i64, version: u64, files_done: usize) -> JobEvent {
        JobEvent::Progress(JobProgress {
            job: id,
            version,
            files_done,
            files_total: 10,
        })
    }

    #[test]
    fn a_newer_state_replaces_and_an_older_or_equal_one_is_ignored() {
        let mut book = JobBook::default();
        assert_eq!(book.apply(state(1, JobState::Running, 2)), Applied::Changed);
        assert_eq!(book.apply(state(1, JobState::Queued, 1)), Applied::Ignored);
        assert_eq!(book.apply(state(1, JobState::Queued, 2)), Applied::Ignored);
        assert_eq!(book.get(1).unwrap().job.state, JobState::Running);
        assert_eq!(
            book.apply(state(1, JobState::Succeeded, 3)),
            Applied::Finished
        );
        assert_eq!(book.get(1).unwrap().job.state, JobState::Succeeded);
    }

    #[test]
    fn progress_below_the_held_version_is_ignored() {
        let mut book = JobBook::default();
        assert_eq!(
            book.apply(progress(1, 2, 3)),
            Applied::Ignored,
            "unknown Job"
        );
        book.apply(state(1, JobState::Running, 2));
        assert_eq!(
            book.apply(progress(1, 1, 3)),
            Applied::Ignored,
            "an older version"
        );
        assert_eq!(book.apply(progress(1, 2, 4)), Applied::Changed);
        assert_eq!(book.get(1).unwrap().progress.unwrap().files_done, 4);
        book.apply(state(1, JobState::Succeeded, 3));
        assert_eq!(
            book.get(1).unwrap().progress,
            None,
            "a new state clears progress"
        );
        assert_eq!(
            book.apply(progress(1, 2, 9)),
            Applied::Ignored,
            "late progress after it finished"
        );
    }

    #[test]
    fn resync_replaces_everything_and_keeps_current_progress() {
        let mut book = JobBook::default();
        book.apply(state(1, JobState::Running, 2));
        book.apply(progress(1, 2, 5));
        book.apply(state(2, JobState::Queued, 1));
        let snapshot = JobEvent::Resync(JobSnapshot {
            jobs: vec![job(1, JobState::Running, 2), job(3, JobState::Queued, 1)],
        });
        assert_eq!(book.apply(snapshot), Applied::Resynced);
        assert_eq!(book.len(), 2);
        assert!(
            book.get(2).is_none(),
            "a Job missing from the snapshot is dropped"
        );
        assert_eq!(book.get(1).unwrap().progress.unwrap().files_done, 5);
        assert_eq!(
            book.newest_first()
                .iter()
                .map(|j| j.job.id)
                .collect::<Vec<_>>(),
            [3, 1]
        );
        assert_eq!(book.active(), 2);
    }

    #[test]
    fn resync_may_move_a_job_back_to_an_older_version() {
        let mut book = JobBook::default();
        book.apply(state(1, JobState::Succeeded, 3));
        book.apply(JobEvent::Resync(JobSnapshot {
            jobs: vec![job(1, JobState::Running, 2)],
        }));
        assert_eq!(
            book.get(1).unwrap().job.version,
            2,
            "the snapshot is the truth"
        );
        assert_eq!(book.apply(state(1, JobState::Running, 2)), Applied::Ignored);
    }

    #[test]
    fn overlay_keeps_the_newer_of_each_job_with_live_progress() {
        let mut book = JobBook::default();
        book.apply(state(1, JobState::Succeeded, 3));
        book.apply(state(3, JobState::Running, 2));
        book.apply(progress(3, 2, 6));
        book.apply(state(4, JobState::Running, 2));
        let history = [
            job(4, JobState::Failed, 3),
            job(3, JobState::Running, 2),
            job(2, JobState::Failed, 3),
            job(1, JobState::Running, 2),
        ];
        let shown = book.overlay(&history);
        let summary: Vec<_> = shown
            .iter()
            .map(|j| (j.job.id, j.job.state, j.progress.map(|p| p.files_done)))
            .collect();
        assert_eq!(
            summary,
            [
                (4, JobState::Failed, None),
                (3, JobState::Running, Some(6)),
                (2, JobState::Failed, None),
                (1, JobState::Succeeded, None),
            ]
        );
    }

    #[test]
    fn events_parse_by_their_sse_name() {
        let job = job(4, JobState::Queued, 1);
        let data = serde_json::to_string(&job).unwrap();
        assert_eq!(
            parse_event("state", &data).unwrap(),
            JobEvent::State(Box::new(job.clone()))
        );
        let p = r#"{"job":4,"version":2,"files_done":1,"files_total":3}"#;
        assert!(matches!(
            parse_event("progress", p).unwrap(),
            JobEvent::Progress(_)
        ));
        let r = format!(r#"{{"jobs":[{data}]}}"#);
        assert!(
            matches!(parse_event("resync", &r).unwrap(), JobEvent::Resync(s) if s.jobs == [job])
        );
        assert!(parse_event("progress", &data).is_err());
        assert!(parse_event("other", "{}").is_err());
    }
}
