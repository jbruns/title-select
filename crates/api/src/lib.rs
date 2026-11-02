//! The types of Title Select's versioned JSON API (`/api/v1`), shared by the Service, the headless client and the UI
//! (ADR 0005). Every response body is one of these, or the `schema: 2` [`Report`].

pub mod build;
pub mod ingest;
pub mod paths;
pub mod repair;
pub mod report;
pub mod selection;
pub mod storage;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use report::*;

/// The path every API route is under.
pub const PREFIX: &str = "/api/v1";

/// How often the Service sends a comment on an idle event stream, so that a client can tell it from a stalled one.
pub const SSE_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// The body of the unauthenticated health path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
}

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    /// The Job a refusal names, such as the active Job of a Source Set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
}

/// A Root the Operator configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub path: String,
    pub writable: bool,
    /// For a read-only Root: the paths of the Source Sets in it that a repair may write to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_paths: Vec<String>,
    /// Whether it is an Ingest Folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ingest: bool,
}

/// A DAT, identified by its provider and header name, with its DAT Revisions, oldest import first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dat {
    /// `<provider>/<name>`.
    pub dat: String,
    pub provider: String,
    pub name: String,
    pub revisions: Vec<DatRevisionRecord>,
}

/// A DAT Revision as the store records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevisionRecord {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub provider: String,
    /// The DAT's header name.
    pub name: String,
    pub version: String,
    /// The original filename.
    pub file: String,
    pub imported_at: String,
    pub source_url: Option<String>,
    /// Whether the unmodified file is kept in the store.
    pub blob_present: bool,
}

impl DatRevisionRecord {
    /// The DAT this is a revision of, as `<provider>/<name>`.
    pub fn dat(&self) -> String {
        format!("{}/{}", self.provider, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    /// A new DAT Revision.
    Added,
    /// The same bytes were already imported; nothing changed.
    AlreadyImported,
    /// The same bytes were already imported and their dropped blob is restored.
    Restored,
}

/// The response to a DAT upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedDatRevision {
    pub outcome: ImportOutcome,
    pub dat_revision: DatRevisionRecord,
}

/// The DAT Revision a Source Set was last verified against: its latest Audit's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastVerified {
    pub sha256: String,
    pub version: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetRecord {
    /// The Source Set's folder name, which identifies it.
    pub name: String,
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub path: String,
    /// The bound DAT, as `<provider>/<name>`.
    pub dat: String,
    pub added_at: String,
    pub last_verified_against: Option<LastVerified>,
}

/// The request to register a folder in a Root as a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceSet {
    pub root: String,
    /// Relative to the Root. When absent, the path `template` proposes for the DAT is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The Path Template to propose the path from when `path` is absent; by default the configured one. Giving
    /// both is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The DAT to audit it against, as `<provider>/<name>`.
    pub dat: String,
}

/// The configured default Path Template of each kind of registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathTemplateDefaults {
    pub source_set: String,
    pub managed_folder: String,
}

/// One Audit in a Source Set's history. A pruned Audit keeps only this summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub id: i64,
    pub source_set: String,
    pub started_at: String,
    pub finished_at: String,
    pub dat_revision_sha256: String,
    pub dat_revision_version: String,
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
    pub pruned: bool,
    /// How the Audit got its Entries' checksums.
    #[serde(default = "full_evidence")]
    pub evidence: AuditEvidence,
}

fn full_evidence() -> AuditEvidence {
    AuditEvidence::Full
}

/// A named Selection Profile and its newest version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub name: String,
    pub created_at: String,
    /// The newest version, or none before the first save.
    pub latest: Option<ProfileVersion>,
}

/// One immutable version of a Selection Profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileVersion {
    pub name: String,
    pub version: u32,
    /// SHA-256 of the canonical JSON.
    pub sha256: String,
    pub saved_at: String,
    pub saved_by: String,
    pub profile: selection::SelectionProfile,
}

/// The request to create a named Selection Profile with no versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProfile {
    pub name: String,
}

/// The request to save a Selection Profile as its next version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveProfile {
    pub profile: selection::SelectionProfile,
    /// The version the edit started from; a save is refused if a newer version was saved since. None for the
    /// first version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<u32>,
    /// Source Sets to queue a Selection for with the saved version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<StartSelectionFor>,
}

/// A Source Set to queue a Selection for, at a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSelectionFor {
    pub source_set: String,
    /// A prefix of the DAT Revision's SHA-256; defaults as for an Audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// The response to a save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedProfile {
    pub version: ProfileVersion,
    /// False when nothing changed, so the newest version was returned and none was created.
    pub created: bool,
    /// The Selection Jobs queued.
    pub jobs: Vec<Job>,
}

/// The names the Profile editor offers and `check` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vocabulary {
    pub regions: Vec<String>,
    pub languages: Vec<String>,
    pub videos: Vec<String>,
    pub categories: Vec<selection::Category>,
    /// The region order a Selection uses when a Profile or system leaves it empty.
    #[serde(default)]
    pub default_region_order: Vec<String>,
    /// The video order a Selection uses when a Profile or system leaves it empty.
    #[serde(default)]
    pub default_video_order: Vec<String>,
}

/// The result of checking a Selection Profile: empty when the engine can read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCheck {
    pub problems: Vec<String>,
}

/// The request for a live preview: the Selection a Profile, saved or not, would make for a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewRequest {
    pub source_set: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub profile: selection::SelectionProfile,
}

/// A live preview, with what changed from the latest saved Selection for the same Source Set and DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub dat_revision: DatRevisionRecord,
    pub selection: selection::Selection,
    /// The Selection the diff is against.
    pub against: Option<i64>,
    pub diff: Vec<selection::ReleaseChange>,
    pub elapsed_ms: u64,
}

/// The request to queue a Selection for a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSelection {
    pub source_set: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub profile: String,
    /// Defaults to the newest version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

/// A recorded Selection. A pruned Selection keeps only this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRecord {
    pub id: i64,
    pub source_set: String,
    pub dat_revision_sha256: String,
    pub dat_revision_version: String,
    pub profile: ProfileVersionRef,
    /// The Profile's newest version, so that a client can offer it when it is newer.
    pub latest_profile_version: u32,
    pub job: Option<i64>,
    pub system: String,
    pub system_settings: bool,
    pub pins: selection::SelectionPins,
    pub summary: selection::SelectionSummary,
    pub created_at: String,
    pub pruned: bool,
}

/// A release of a Selection with its position, and the availability of the release in the Source Set's latest
/// Audit against the same DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionReleaseRow {
    pub seq: u64,
    #[serde(flatten)]
    pub release: selection::ReleaseRow,
    /// None when there is no Audit against the Selection's DAT Revision.
    pub availability: Option<GameStatus>,
    /// The availability rests, for at least one ROM, on Claim Evidence alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claimed: bool,
}

/// The availability of a Selection's releases: the Game Statuses of the Source Set's latest Audit against the
/// Selection's DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    /// None when the Source Set has no Audit against that DAT Revision.
    pub audit: Option<i64>,
    pub games: BTreeMap<String, GameStatus>,
    /// The Games whose status rests, for at least one ROM, on Claim Evidence alone.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub claimed: BTreeSet<String>,
}

/// Release by release, what changed between two Selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionDiff {
    pub from: i64,
    pub to: i64,
    pub changes: Vec<selection::ReleaseChange>,
}

/// The request to start an Audit of a registered Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartAudit {
    pub source_set: String,
    /// A prefix of the DAT Revision's SHA-256. Defaults to the latest Audit's DAT Revision, else the newest imported
    /// revision of the Source Set's DAT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// How the Audit gets its checksums; incremental by default.
    #[serde(default)]
    pub evidence: AuditEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl JobState {
    pub const ALL: [JobState; 5] = [
        JobState::Queued,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed,
        JobState::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<JobState> {
        JobState::ALL.into_iter().find(|j| j.as_str() == s)
    }

    /// Whether the Job will not change state again.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        )
    }
}

/// The payload-reader and hash worker counts a Job runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workers {
    pub readers: usize,
    pub hashers: usize,
}

/// What a Job does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[default]
    Audit,
    Selection,
    /// Carries out an approved Change Plan into a Managed Folder.
    Build,
    /// Checks a Managed Folder against the store's record of it, read-only.
    Check,
    /// Scans an Ingest Folder and assesses what settled in it, read-only.
    IngestScan,
    /// Carries out an approved repair of a Source Set.
    Repair,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Audit => "audit",
            JobKind::Selection => "selection",
            JobKind::Build => "build",
            JobKind::Check => "check",
            JobKind::IngestScan => "ingest_scan",
            JobKind::Repair => "repair",
        }
    }

    pub fn parse(s: &str) -> Option<JobKind> {
        [
            JobKind::Audit,
            JobKind::Selection,
            JobKind::Build,
            JobKind::Check,
            JobKind::IngestScan,
            JobKind::Repair,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// A Selection Profile version, by Profile name and version number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileVersionRef {
    pub name: String,
    pub version: u32,
}

/// A durable unit of work: an Audit of a Source Set, or a Selection made for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    #[serde(default)]
    pub kind: JobKind,
    pub state: JobState,
    pub source_set: String,
    /// The DAT Revision the Job is pinned to, by SHA-256.
    pub dat_revision: String,
    /// The Selection Profile version a Selection Job runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileVersionRef>,
    /// Who started it: the identity the trusted proxy handed over, or the Operator token's.
    pub actor: String,
    /// 1, or 2 for the Job queued again after a restart interrupted the first.
    pub attempt: u32,
    /// The interrupted Job this one was queued again for.
    pub requeued_from: Option<i64>,
    /// The Audit a Succeeded Audit Job recorded.
    pub audit: Option<i64>,
    /// The Selection a Succeeded Selection Job recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<i64>,
    /// The Managed Folder a Build Job builds into, or a Check Job checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_folder: Option<i64>,
    /// The approved plan a Build Job carries out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<i64>,
    /// How an Audit Job's Audit gets its checksums.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<AuditEvidence>,
    /// The Ingest Folder an Ingest Scan Job scans, by its Root's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_folder: Option<String>,
    /// The approved repair plan a Repair Job carries out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<i64>,
    /// When you asked the Job to stop at its next safe point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at: Option<String>,
    /// Why a Job Failed or was Interrupted.
    pub error: Option<String>,
    pub workers: Workers,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Starts at 1 and goes up with each change of state, so that a client can tell a stale `state` from a newer one.
    pub version: u64,
}

/// How far a Running Audit has got through its Source Set's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub job: i64,
    /// The `version` of the Running Job.
    pub version: u64,
    pub files_done: usize,
    pub files_total: usize,
}

/// The Jobs a client of `/events` should know about: every Queued or Running Job and the most recently finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub jobs: Vec<Job>,
}

/// A server-sent event: the SSE event name is `state`, `progress` or `resync`, and its data is this, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JobEvent {
    State(Box<Job>),
    Progress(JobProgress),
    /// Sent by `/events` when a client connects and whenever it fell behind and missed events: the Jobs now, to
    /// replace what the client knew.
    Resync(JobSnapshot),
}

impl JobEvent {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            JobEvent::State(_) => "state",
            JobEvent::Progress(_) => "progress",
            JobEvent::Resync(_) => "resync",
        }
    }

    /// The Job the event is about; none for a `resync`.
    pub fn job_id(&self) -> Option<i64> {
        match self {
            JobEvent::State(job) => Some(job.id),
            JobEvent::Progress(p) => Some(p.job),
            JobEvent::Resync(_) => None,
        }
    }
}

/// One page of a list with cursor paging. Pass `next_cursor` as `cursor` for the next page; it is absent on the last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// An Audit's Entry with its position in the report's `entries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRow {
    pub seq: u64,
    #[serde(flatten)]
    pub entry: Entry,
}

/// An Audit's Game Status with its position in the report's `games`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRow {
    pub seq: u64,
    #[serde(flatten)]
    pub game: Game,
}
