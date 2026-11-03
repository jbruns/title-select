//! The Activity screen's pure logic: the counters over its Jobs, where each Job links, and the kept files of every
//! folder as one list.

use title_select_api::build::{KeptFile, LibraryFolder};
use title_select_api::ingest::IngestKeptFile;
use title_select_api::repair::SourceKeptFile;
use title_select_api::{Job, JobState};

use crate::build_model::keep_reason_label;
use crate::parts::url_state::encode;

/// How many of the listed Jobs are Running, Queued and Failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub running: usize,
    pub queued: usize,
    pub failed: usize,
}

pub fn counts<'a>(jobs: impl IntoIterator<Item = &'a Job>) -> Counts {
    let mut counts = Counts::default();
    for job in jobs {
        match job.state {
            JobState::Running => counts.running += 1,
            JobState::Queued => counts.queued += 1,
            JobState::Failed => counts.failed += 1,
            JobState::Succeeded | JobState::Interrupted => {}
        }
    }
    counts
}

/// What a Job works on, which its row links to: the Ingest Folder it scans, the Library Folder it builds or checks,
/// the Additions Set it works on, or else its Source Set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    SourceSet(String),
    LibraryFolder(i64),
    IngestFolder(String),
    AdditionsSet(String),
}

impl Subject {
    pub fn of(job: &Job) -> Subject {
        if let Some(root) = &job.ingest_folder {
            Subject::IngestFolder(root.clone())
        } else if let Some(folder) = job.library_folder {
            Subject::LibraryFolder(folder)
        } else if let Some(system) = &job.additions_set {
            Subject::AdditionsSet(system.clone())
        } else {
            Subject::SourceSet(job.source_set.clone())
        }
    }

    pub fn href(&self) -> String {
        match self {
            Subject::SourceSet(name) => format!("/source-sets/{}", encode(name)),
            Subject::LibraryFolder(id) => format!("/libraries/{id}"),
            Subject::IngestFolder(root) => format!("/ingest/{}", encode(root)),
            Subject::AdditionsSet(_) => "/additions".to_string(),
        }
    }
}

/// What a Job recorded or carries out, which its row also links to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Audit(i64),
    Selection(i64),
    /// The repair a Repair Job carries out, on its Source Set's repair tab.
    Repair {
        source_set: String,
    },
}

impl Outcome {
    pub fn of(job: &Job) -> Option<Outcome> {
        if job.repair.is_some() {
            return Some(Outcome::Repair {
                source_set: job.source_set.clone(),
            });
        }
        match (job.audit, job.selection) {
            (Some(audit), _) => Some(Outcome::Audit(audit)),
            (None, Some(selection)) => Some(Outcome::Selection(selection)),
            (None, None) => None,
        }
    }

    pub fn href(&self) -> String {
        match self {
            Outcome::Audit(id) => format!("/audits/{id}"),
            Outcome::Selection(id) => format!("/selections/{id}"),
            Outcome::Repair { source_set } => {
                format!("/source-sets/{}?tab=repair", encode(source_set))
            }
        }
    }
}

/// The Failed Job the open row (`?row=<job id>`) shows, if it shows one: opening it marks it seen.
pub fn opened_failure<'a>(
    jobs: impl IntoIterator<Item = &'a Job>,
    row: Option<&str>,
) -> Option<i64> {
    let id: i64 = row?.parse().ok()?;
    jobs.into_iter()
        .find(|j| j.id == id && j.state == JobState::Failed)
        .map(|j| j.id)
}

/// Where a kept file is kept: each folder Title Select keeps files aside in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeptPlace {
    LibraryFolder {
        id: i64,
        path: String,
    },
    IngestFolder(String),
    SourceSet(String),
    /// By its system, `<provider>/<name>`.
    AdditionsSet(String),
}

impl KeptPlace {
    fn kind(&self) -> &'static str {
        match self {
            KeptPlace::LibraryFolder { .. } => "library-folder",
            KeptPlace::IngestFolder(_) => "ingest-folder",
            KeptPlace::SourceSet(_) => "source-set",
            KeptPlace::AdditionsSet(_) => "additions-set",
        }
    }

    /// The folder's name.
    pub fn name(&self) -> String {
        match self {
            KeptPlace::LibraryFolder { path, .. } => path.clone(),
            KeptPlace::IngestFolder(root) => root.clone(),
            KeptPlace::SourceSet(name) => name.clone(),
            KeptPlace::AdditionsSet(system) => system.clone(),
        }
    }

    /// The page where you review and delete its kept files.
    pub fn href(&self) -> String {
        match self {
            KeptPlace::LibraryFolder { id, .. } => format!("/libraries/{id}?tab=kept"),
            KeptPlace::IngestFolder(root) => format!("/ingest/{}", encode(root)),
            KeptPlace::SourceSet(name) => format!("/source-sets/{}?tab=kept", encode(name)),
            KeptPlace::AdditionsSet(_) => "/additions".to_string(),
        }
    }
}

/// One kept file of any folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRow {
    pub place: KeptPlace,
    /// Its ID among its kind of folder's kept files.
    pub id: i64,
    pub name: String,
    /// Its name in the retention area, in `<generation>/`.
    pub kept_name: String,
    pub generation: u64,
    pub reason: String,
    pub size: u64,
    pub kept_at: String,
}

impl KeptRow {
    /// Unique across every folder: kept files are numbered per kind of folder.
    pub fn key(&self) -> String {
        format!("{}-{}", self.place.kind(), self.id)
    }

    pub fn library(folder: &LibraryFolder, f: KeptFile) -> KeptRow {
        KeptRow {
            place: KeptPlace::LibraryFolder {
                id: folder.id,
                path: folder.path.clone(),
            },
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: keep_reason_label(f.reason).to_string(),
            size: f.file.size,
            kept_at: f.kept_at,
        }
    }

    pub fn ingest(root: &str, f: IngestKeptFile) -> KeptRow {
        KeptRow {
            place: KeptPlace::IngestFolder(root.to_string()),
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: f.reason,
            size: f.file.size,
            kept_at: f.kept_at,
        }
    }

    /// A kept file of a Source Set, or of an Additions Set when `place` says so.
    pub fn source(place: KeptPlace, f: SourceKeptFile) -> KeptRow {
        KeptRow {
            place,
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: f.reason,
            size: f.file.size,
            kept_at: f.kept_at,
        }
    }
}

/// The kept files, the most recently kept first.
pub fn newest_first(mut rows: Vec<KeptRow>) -> Vec<KeptRow> {
    rows.sort_by(|a, b| b.kept_at.cmp(&a.kept_at));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::Workers;

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
    fn counters_count_running_queued_and_failed_jobs() {
        let jobs = [
            job(1, JobState::Running),
            job(2, JobState::Queued),
            job(3, JobState::Queued),
            job(4, JobState::Failed),
            job(5, JobState::Succeeded),
            job(6, JobState::Interrupted),
        ];
        assert_eq!(
            counts(&jobs),
            Counts {
                running: 1,
                queued: 2,
                failed: 1
            }
        );
    }

    #[test]
    fn a_job_links_to_the_folder_it_works_on_before_its_source_set() {
        let mut scan = job(1, JobState::Running);
        scan.ingest_folder = Some("My Drop".into());
        assert_eq!(Subject::of(&scan), Subject::IngestFolder("My Drop".into()));
        assert_eq!(Subject::of(&scan).href(), "/ingest/My%20Drop");

        let mut build = job(2, JobState::Queued);
        build.library_folder = Some(7);
        assert_eq!(Subject::of(&build).href(), "/libraries/7");

        let mut additions = job(3, JobState::Queued);
        additions.additions_set = Some("No-Intro/Atari - 2600".into());
        assert_eq!(Subject::of(&additions).href(), "/additions");

        let audit = job(4, JobState::Succeeded);
        assert_eq!(Subject::of(&audit).href(), "/source-sets/Atari%202600");
    }

    #[test]
    fn a_finished_job_links_to_what_it_recorded() {
        let mut audit = job(1, JobState::Succeeded);
        audit.audit = Some(12);
        assert_eq!(Outcome::of(&audit), Some(Outcome::Audit(12)));
        assert_eq!(Outcome::of(&audit).unwrap().href(), "/audits/12");

        let mut selection = job(2, JobState::Succeeded);
        selection.selection = Some(5);
        assert_eq!(Outcome::of(&selection).unwrap().href(), "/selections/5");

        let mut repair = job(3, JobState::Running);
        repair.repair = Some(9);
        assert_eq!(
            Outcome::of(&repair).unwrap().href(),
            "/source-sets/Atari%202600?tab=repair"
        );

        assert_eq!(Outcome::of(&job(4, JobState::Queued)), None);
    }

    #[test]
    fn opening_a_failed_job_names_it_and_opening_any_other_does_not() {
        let jobs = [job(3, JobState::Failed), job(4, JobState::Succeeded)];
        assert_eq!(opened_failure(&jobs, Some("3")), Some(3));
        assert_eq!(opened_failure(&jobs, Some("4")), None);
        assert_eq!(opened_failure(&jobs, Some("8")), None);
        assert_eq!(opened_failure(&jobs, Some("x")), None);
        assert_eq!(opened_failure(&jobs, None), None);
    }

    fn kept(place: KeptPlace, id: i64, kept_at: &str) -> KeptRow {
        KeptRow {
            place,
            id,
            name: format!("file {id}.zip"),
            kept_name: format!("file {id}.zip.kept"),
            generation: 1,
            reason: "replaced".into(),
            size: 10,
            kept_at: kept_at.into(),
        }
    }

    #[test]
    fn kept_files_of_every_folder_are_one_list_newest_first_each_linking_to_its_folder() {
        let rows = newest_first(vec![
            kept(
                KeptPlace::LibraryFolder {
                    id: 3,
                    path: "roms/Atari".into(),
                },
                1,
                "2026-01-02T00:00:00Z",
            ),
            kept(
                KeptPlace::SourceSet("Atari 2600".into()),
                1,
                "2026-01-04T00:00:00Z",
            ),
            kept(
                KeptPlace::IngestFolder("My Drop".into()),
                1,
                "2026-01-01T00:00:00Z",
            ),
            kept(
                KeptPlace::AdditionsSet("No-Intro/Atari - 2600".into()),
                1,
                "2026-01-03T00:00:00Z",
            ),
        ]);
        let shown: Vec<(String, String)> = rows.iter().map(|r| (r.key(), r.place.href())).collect();
        assert_eq!(
            shown,
            [
                (
                    "source-set-1".to_string(),
                    "/source-sets/Atari%202600?tab=kept".to_string()
                ),
                ("additions-set-1".to_string(), "/additions".to_string()),
                (
                    "library-folder-1".to_string(),
                    "/libraries/3?tab=kept".to_string()
                ),
                (
                    "ingest-folder-1".to_string(),
                    "/ingest/My%20Drop".to_string()
                ),
            ]
        );
    }
}
