//! The types of Title Select's versioned JSON API (`/api/v1`), shared by the Service, the headless client and the UI
//! (ADR 0005). Every response body is one of these, or the `schema: 5` [`Report`].

pub mod additions;
pub mod arcade;
pub mod build;
pub mod calibration;
pub mod coverage;
pub mod dat;
pub mod discs;
pub mod fetch;
pub mod ingest;
pub mod lifecycle;
pub mod paths;
pub mod repair;
pub mod report;
pub mod romm;
pub mod selection;
pub mod settings;
pub mod storage;
pub mod support;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use dat::DatFormat;
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
    /// What stops a DAT Revision from being deleted, when that is why the request was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blockers: Option<Vec<DeletionBlocker>>,
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

/// A DAT, identified by its recognised provider and native name, or by its DAT Format and name when its provider is
/// unknown, with its DAT Revisions, oldest first by header version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dat {
    /// `<provider>/<name>`; the provider part of a DAT whose provider is unknown is reserved for its format, such as
    /// `unknown-logiqx`.
    pub dat: String,
    /// Its newest revision's DAT Format.
    pub format: DatFormat,
    /// Its DAT Provider; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub name: String,
    pub revisions: Vec<DatRevisionRecord>,
}

/// A DAT Revision as the store records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevisionRecord {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    /// The DAT this is a revision of, as `<provider>/<name>`.
    pub dat: String,
    pub format: DatFormat,
    /// Its DAT Provider: the one a fetch took it from, else the one its header signals; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The DAT's native name: the header's, a software list's, or `MAME` for ListXML.
    pub name: String,
    /// What the header claims, each absent when it doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// The header's `url`, apart from the [`source_url`](Self::source_url) it was downloaded from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_url: Option<String>,
    /// The header's version, or ListXML's build; empty when it gives none.
    pub version: String,
    /// The header's date, as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The original filename.
    pub file: String,
    pub imported_at: String,
    /// Where it was downloaded from, as you or a fetch said: provenance, not a claim of the header.
    pub source_url: Option<String>,
    /// Whether the unmodified file is kept in the store.
    pub blob_present: bool,
    /// What stops you deleting it, as the catalogue lists it (`/dats`, `/dat-revisions`); empty when you can delete
    /// it. Absent where a record names a revision for another purpose, such as an audit's report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blockers: Option<Vec<DeletionBlocker>>,
}

/// What still depends on a DAT Revision, so that it can't be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionBlockerKind {
    /// A registered Source Set uses its DAT: it blocks every revision of the DAT.
    SourceSet,
    /// A registered Additions Set belongs to its DAT.
    AdditionsSet,
    /// An Audit that isn't pruned, finished or running, uses it.
    Audit,
    /// A Selection that isn't pruned was made against it.
    Selection,
    /// A Change Plan not yet carried out or superseded rests on an Audit or Selection of it.
    ChangePlan,
    /// A Build of it is running, needs recovery, or is a Library Folder's current Build.
    Build,
    /// A file in a registered Library Folder was built from it.
    BuiltFile,
    /// A kept file, set aside from a registered Library Folder and not yet settled, was built from it.
    KeptFile,
    /// A Job against it is queued or running.
    Job,
    /// A DAT import from an Ingest Folder still owes an Audit against it.
    IngestReaudit,
}

/// One kind of thing that still depends on a DAT Revision: how many, and what names them where that helps you act
/// (source sets, folders).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionBlocker {
    pub kind: DeletionBlockerKind,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

impl DeletionBlocker {
    /// Why it blocks, in one clause for you to read.
    pub fn describe(&self) -> String {
        let n = self.count;
        let plural = |one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let of = if self.names.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.names.join(", "))
        };
        match self.kind {
            DeletionBlockerKind::SourceSet => format!(
                "{} {} its DAT{of}",
                plural("source set", "source sets"),
                if n == 1 { "uses" } else { "use" }
            ),
            DeletionBlockerKind::AdditionsSet => {
                format!("its DAT has an additions set{of}")
            }
            DeletionBlockerKind::Audit => format!(
                "{} that {} pruned {} it{of}",
                plural("audit", "audits"),
                if n == 1 { "isn't" } else { "aren't" },
                if n == 1 { "uses" } else { "use" }
            ),
            DeletionBlockerKind::Selection => format!(
                "{} that {} pruned {} made against it{of}",
                plural("selection", "selections"),
                if n == 1 { "isn't" } else { "aren't" },
                if n == 1 { "was" } else { "were" }
            ),
            DeletionBlockerKind::ChangePlan => format!(
                "{} {} on it{of}",
                plural("open plan", "open plans"),
                if n == 1 { "rests" } else { "rest" }
            ),
            DeletionBlockerKind::Build => format!(
                "{} from it {}{of}",
                plural("build", "builds"),
                if n == 1 {
                    "is running, awaits recovery or is current"
                } else {
                    "are running, await recovery or are current"
                }
            ),
            DeletionBlockerKind::BuiltFile => format!(
                "{} in your play library {} built from it{of}",
                plural("file", "files"),
                if n == 1 { "was" } else { "were" }
            ),
            DeletionBlockerKind::KeptFile => format!(
                "{} built from it {} still set aside{of}",
                plural("kept file", "kept files"),
                if n == 1 { "is" } else { "are" }
            ),
            DeletionBlockerKind::Job => format!(
                "{} against it {} queued or running",
                plural("job", "jobs"),
                if n == 1 { "is" } else { "are" }
            ),
            DeletionBlockerKind::IngestReaudit => format!(
                "a DAT import from an ingest folder still owes {}{of}",
                plural("audit", "audits")
            ),
        }
    }
}

/// What deleting a DAT Revision did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedDatRevision {
    pub sha256: String,
    /// Its DAT, as `<provider>/<name>`.
    pub dat: String,
    /// Whether its file was still kept, and so was deleted with it.
    pub blob_deleted: bool,
    /// Whether it was its DAT's last catalogued revision, so the DAT left the catalogue.
    pub dat_removed: bool,
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

/// A Redump cue pack as the store records it: a ZIP of `.cue` sheets for one DAT, kept unmodified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuePackRecord {
    /// SHA-256 of the unmodified cue pack; its identity.
    pub sha256: String,
    /// The DAT its sheets are for, as `<provider>/<name>`.
    pub dat: String,
    /// The original filename.
    pub file: String,
    pub source_url: Option<String>,
    /// How it came into the store.
    pub origin: CuePackOrigin,
    /// How many `.cue` sheets it holds.
    pub cues: u64,
    pub imported_at: String,
    /// Whether its file is still kept: an older pack for the same DAT that no repair plan refers to is deleted from
    /// the store, and importing it again restores it.
    pub blob_present: bool,
}

/// How a cue pack came into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuePackOrigin {
    /// You imported it.
    Import,
    /// Title Select fetched it.
    Fetch,
}

impl CuePackOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            CuePackOrigin::Import => "import",
            CuePackOrigin::Fetch => "fetch",
        }
    }
}

/// The response to a cue pack upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuePackImport {
    pub pack: CuePackRecord,
    pub outcome: ImportOutcome,
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
    /// Whether it Follows its DAT: a newer DAT Revision then makes it Stale.
    #[serde(default)]
    pub follow: bool,
    /// Why it is Stale, if it is: each cause, files first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<SourceSetStale>,
    /// Its Coverage: the engine region names it covers. Absent when it covers every Game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<String>>,
}

/// One reason a Source Set is Stale. Looking changed is a heuristic, never a claim of damage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSetStale {
    /// The latest stat walk since its latest Audit found files changed, added or removed.
    FilesChanged {
        walk: i64,
        /// When a stat walk first found these changes.
        found_at: String,
        changed: u64,
        added: u64,
        removed: u64,
    },
    /// It Follows its DAT, and a DAT Revision newer than its latest Audit's was imported: the newest.
    NewerRevision {
        sha256: String,
        version: String,
        imported_at: String,
    },
}

/// What a stat walk of a Source Set found, compared with the Audit it names: files whose stamp changed, files the
/// Audit did not see, and files it saw that are gone. Walks that keep finding the same are one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatWalk {
    pub id: i64,
    pub source_set: String,
    /// The Audit it compared with: the latest when it ran.
    pub audit: i64,
    /// When a walk first found this.
    pub found_at: String,
    /// When a walk last found this.
    pub checked_at: String,
    pub changed: u64,
    pub added: u64,
    pub removed: u64,
}

impl StatWalk {
    /// Whether it found anything changed, added or removed.
    pub fn looks_changed(&self) -> bool {
        self.changed + self.added + self.removed > 0
    }
}

/// A DAT Revision that made a Source Set Stale because it Follows its DAT: newer than its latest Audit's when it was
/// imported, or when you started following. Kept in its history after an Audit, or after you stop following.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowedRevision {
    pub source_set: String,
    pub sha256: String,
    pub version: String,
    pub imported_at: String,
    /// When it made the Source Set Stale.
    pub found_at: String,
}

/// The request to Follow a Source Set's DAT, or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetFollow {
    pub follow: bool,
}

/// The request to set a Source Set's Coverage, as engine region names, or to clear it with `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetCoverage {
    pub coverage: Option<Vec<String>>,
}

/// Where a Source Set keeps its Games under Single Copy (ADR 0023): the Library Folder that holds its Placed Games,
/// how many are placed in each folder, and the Games a Build found need review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placements {
    /// The Library Folder holding its Placed Games: the one it names, or else its only bound folder. None when several
    /// are bound and it names none, or none is bound.
    pub holding_folder: Option<i64>,
    /// Each Library Folder its Placed Games are in now, by id, with how many: usually the holding folder alone, but a
    /// folder no longer holding them keeps them until its next Build returns them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placed: Vec<PlacedIn>,
    /// Its Games a Build found need review, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_flags: Vec<report::ReviewFlag>,
}

/// How many of a Source Set's Games are placed in one Library Folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedIn {
    pub library_folder: i64,
    pub games: u64,
}

/// The request to name the Library Folder that holds a Source Set's Placed Games, or to name none with `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHoldingFolder {
    pub folder: Option<i64>,
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
    /// Its Coverage, as engine region names; by default it covers every Game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<String>>,
}

/// The configured default Path Template of each kind of registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathTemplateDefaults {
    pub source_set: String,
    pub library_folder: String,
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
    /// How many of its Entries rest on Claim Evidence rather than Payload Evidence; unknown once it is pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed: Option<usize>,
    /// How many of its Games carry each Game Warning; those of none are left out.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warnings: BTreeMap<GameWarning, usize>,
    /// The Header Rule its DAT Revision names, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_rule: Option<NamedHeaderRule>,
}

/// A Source Set with what the list of Source Sets shows of it: its latest finished Audit and its open Repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetSummary {
    pub set: SourceSetRecord,
    /// Its latest finished Audit. When the Source Set has a Coverage, `games` counts only the Games it covers
    /// (ADR 0021); the rest are in `outside_coverage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<AuditSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<OpenRepair>,
    /// The latest Audit's Games outside the Source Set's Coverage; absent without a Coverage or an Audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_coverage: Option<OutsideCoverage>,
    /// The latest Audit's Entries that its status ranks on under the Source Set's Coverage: those of covered Games
    /// and those of no Game at all. `latest.findings` and `latest.claimed` still count every Entry. Absent without a
    /// Coverage or an Audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_entries: Option<CoveredEntries>,
    /// Set when the Source Set has a Coverage that couldn't be applied: Games' regions can't be read without the
    /// Selection engine's clone lists, or its latest Audit couldn't be read. Its latest Audit's counts are then whole,
    /// as if it had none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coverage_unavailable: bool,
}

/// Entries of an Audit counted under a Coverage: those whose ROMs, matched or named, belong to a covered Game, and
/// those that belong to no Game, which need review whatever the Coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveredEntries {
    /// How many have each Finding; those of none are left out.
    pub findings: BTreeMap<Finding, usize>,
    /// How many of them rest on Claim Evidence.
    pub claimed: usize,
}

/// An Audit's Games outside a Coverage: they keep their Game Status, and present ones still count and are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideCoverage {
    /// How many have each Game Status; those of none are left out.
    pub games: BTreeMap<GameStatus, usize>,
    /// How many of them have no region the engine recognises (the region Unknown), whatever their Game Status.
    pub unknown_region: usize,
}

/// A Source Set's latest Audit counted by region: a Game counts under each region it carries, so the rows overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionCounts {
    pub audit: i64,
    /// Every Game once, whatever its regions.
    pub total: BTreeMap<GameStatus, usize>,
    /// One row per region at least one Game carries, World and Unknown included, in the engine's region order.
    pub regions: Vec<RegionCount>,
}

/// The Games carrying one region, by Game Status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionCount {
    pub region: String,
    /// Whether the Source Set's Coverage covers the region; true for every region without a Coverage.
    pub covered: bool,
    /// How many have each Game Status; those of none are left out.
    pub games: BTreeMap<GameStatus, usize>,
}

/// What setting a Source Set's Coverage to `coverage` would change, without saving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveragePreview {
    /// The proposed Coverage as it would be stored; null covers every Game.
    pub coverage: Option<Vec<String>>,
    /// The latest Audit the counts read; absent before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<i64>,
    /// Missing Games the current Coverage covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_before: Option<usize>,
    /// Missing Games the proposed Coverage would cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_after: Option<usize>,
    /// The Library Folders the change would make Stale.
    pub stale_library_folders: Vec<CoverageStaleFolder>,
}

/// A Library Folder a Coverage change would make Stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageStaleFolder {
    pub id: i64,
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub path: String,
}

/// A Source Set's open Repair: one approved and not finished, or else one not yet approved that was proposed from
/// its latest Audit since its last approved Repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRepair {
    /// The repair plan.
    pub id: i64,
    /// The Audit it was proposed from.
    pub audit: i64,
    pub approved: bool,
    /// Its Repair Job, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
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
    /// When it was Retired: hidden from new choices, its versions kept for the Selections made with them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
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
    /// The presets a Coverage can be built from, with their regions.
    #[serde(default)]
    pub coverage_presets: Vec<CoveragePreset>,
}

/// A Coverage preset and the regions it expands to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveragePreset {
    pub name: String,
    pub regions: Vec<String>,
}

impl CoveragePreset {
    /// Every preset, in the order the workbench offers them.
    pub fn all() -> Vec<CoveragePreset> {
        coverage::PRESETS
            .iter()
            .map(|p| CoveragePreset {
                name: p.name.to_owned(),
                regions: p.regions.iter().map(|r| (*r).to_owned()).collect(),
            })
            .collect()
    }
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
    /// The Library Folders whose plans or builds use the Selection, so that a client can tell a what-if
    /// Selection, which none uses.
    #[serde(default)]
    pub library_folders: Vec<i64>,
}

/// The system a Source Set's latest DAT Revision is for, named as a Profile files its system settings and
/// choices under. None when the engine can't read the DAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetSystem {
    pub source_set: String,
    pub system: Option<String>,
    /// Whether the DAT is an Arcade DAT, which the arcade selector selects under the Profile's `arcade` settings
    /// rather than its Retool settings (ADR 0027).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub arcade: bool,
}

/// The system a DAT is for, as a Profile files its system settings and choices under, and whether it is an Arcade
/// DAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatSystem {
    pub name: String,
    pub arcade: bool,
}

impl DatSystem {
    /// The system of a DAT the Retool port selects.
    pub fn retool(name: impl Into<String>) -> DatSystem {
        DatSystem {
            name: name.into(),
            arcade: false,
        }
    }
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
    /// Carries out an approved Change Plan into a Library Folder.
    Build,
    /// Checks a Library Folder against the store's record of it, read-only.
    Check,
    /// Scans an Ingest Folder and assesses what settled in it, read-only.
    IngestScan,
    /// Carries out an approved repair of a Source Set.
    Repair,
    /// Publishes, retires and restores the Additions of one Additions Set.
    Additions,
    /// Stats a Source Set's files against its latest Audit, read-only and at low priority.
    StatWalk,
    /// Measures how many hash workers the Host and payload readers each Root sustain, read-only and alone.
    Calibration,
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
            JobKind::Additions => "additions",
            JobKind::StatWalk => "stat_walk",
            JobKind::Calibration => "calibration",
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
            JobKind::Additions,
            JobKind::StatWalk,
            JobKind::Calibration,
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
    /// The Library Folder a Build Job builds into, or a Check Job checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_folder: Option<i64>,
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
    /// The system whose Additions Set an Additions Job works on, as `<provider>/<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions_set: Option<String>,
    /// When you asked the Job to stop at its next safe point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at: Option<String>,
    /// Why a Job Failed or was Interrupted.
    pub error: Option<String>,
    /// The RomM scan a Build Job requested once it ended, having changed files in a mapped Library Folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romm_scan: Option<romm::RommScan>,
    pub workers: Workers,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Starts at 1 and goes up with each change of state, so that a client can tell a stale `state` from a newer one.
    pub version: u64,
    /// What a finished Job read and wrote, and for how long it ran; absent for a Job that hasn't finished, or that
    /// finished before the Service metered Jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput: Option<JobThroughput>,
}

/// The payload bytes a finished Job read and the bytes it wrote, and how long it ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobThroughput {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub duration_ms: u64,
}

impl JobThroughput {
    /// The bytes read and written per second, on average over the Job; none for a Job that took no time.
    pub fn bytes_per_s(&self) -> Option<f64> {
        (self.duration_ms > 0).then(|| {
            (self.bytes_read + self.bytes_written) as f64 * 1000.0 / self.duration_ms as f64
        })
    }
}

/// How fast bytes and files move, over the last few seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub read_bytes_per_s: f64,
    pub write_bytes_per_s: f64,
    /// Files read and written per second.
    pub files_per_s: f64,
}

// Rates are counts over a positive span, never NaN.
impl Eq for Rates {}

/// The byte-rate caps in force, in bytes a second: none where nothing caps that way, as under `unthrottled`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_bytes_per_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_bytes_per_s: Option<u64>,
}

impl ByteCaps {
    /// The caps of several Roots moving bytes at once: each way's caps added up, or none if any Root moving that way
    /// is uncapped, or no Root moves that way. Each Root comes with whether it reads and whether it writes.
    pub fn sum<'a>(roots: impl IntoIterator<Item = (&'a ByteCaps, bool, bool)>) -> ByteCaps {
        let (mut read, mut write) = (Some(Vec::new()), Some(Vec::new()));
        for (caps, reads, writes) in roots {
            for (on, cap, total) in [
                (reads, caps.read_bytes_per_s, &mut read),
                (writes, caps.write_bytes_per_s, &mut write),
            ] {
                if on {
                    match (cap, total.as_mut()) {
                        (Some(cap), Some(caps)) => caps.push(cap),
                        _ => *total = None,
                    }
                }
            }
        }
        let add = |caps: Option<Vec<u64>>| {
            caps.filter(|c| !c.is_empty())
                .map(|c| c.into_iter().fold(0u64, u64::saturating_add))
        };
        ByteCaps {
            read_bytes_per_s: add(read),
            write_bytes_per_s: add(write),
        }
    }
}

/// How far a Running Audit has got through its Source Set's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub job: i64,
    /// The `version` of the Running Job.
    pub version: u64,
    pub files_done: usize,
    pub files_total: usize,
    /// How fast the Job reads and writes now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rates: Option<Rates>,
}

/// Sent by `/events` while Jobs run, and once when the last stops: how fast the Service reads and writes in total, in
/// each Root and for each Running Job, and how hard it works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Throughput {
    pub total: Rates,
    /// The byte-rate caps on the Roots moving bytes now, added up.
    #[serde(default)]
    pub caps: ByteCaps,
    /// Each Root bytes moved in or out of since the Service started, by name.
    pub roots: Vec<RootRates>,
    /// Each Running Job the Service meters.
    pub jobs: Vec<JobLoad>,
    /// The pace Running Jobs follow.
    pub pace: settings::Pace,
    /// The Service's CPU use as a share of the CPUs it may use, from 0 up; absent where the host can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f64>,
}

// CPU use is a share over a positive span, never NaN.
impl Eq for Throughput {}

/// How fast bytes move in and out of one Root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootRates {
    pub root: String,
    pub rates: Rates,
    /// The Root's byte-rate caps in force now.
    #[serde(default)]
    pub caps: ByteCaps,
}

/// How fast one Running Job reads and writes, and its workers at work against what the pace allows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLoad {
    pub job: i64,
    pub rates: Rates,
    /// The byte-rate caps on the Roots the Job moves bytes in, added up.
    #[serde(default)]
    pub caps: ByteCaps,
    /// Payload readers at work now.
    pub readers: usize,
    /// Hash workers at work now.
    pub hashers: usize,
    /// The payload readers and hash workers the pace allows the Job now.
    pub allowance: Workers,
    /// Whether chdman, whose bytes aren't capped, runs for the Job now.
    pub chdman: bool,
}

/// The Jobs a client of `/events` should know about: every Queued or Running Job and the most recently finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub jobs: Vec<Job>,
}

/// Sent by `/events` when the settings change, so a client can fetch them again. It carries nothing secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChanged {
    /// The pace running Jobs follow now.
    pub pace: settings::Pace,
}

/// A server-sent event: the SSE event name is `state`, `progress`, `throughput`, `resync`, `settings` or
/// `romm_reconciled`, and its data is this, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JobEvent {
    State(Box<Job>),
    Progress(JobProgress),
    // Before `Resync`, whose `jobs` an empty Throughput's would match.
    Throughput(Box<Throughput>),
    /// Sent by `/events` when a client connects and whenever it fell behind and missed events: the Jobs now, to
    /// replace what the client knew.
    Resync(JobSnapshot),
    Settings(SettingsChanged),
    RommReconciled(romm::RommReconciled),
}

impl JobEvent {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            JobEvent::State(_) => "state",
            JobEvent::Progress(_) => "progress",
            JobEvent::Throughput(_) => "throughput",
            JobEvent::Resync(_) => "resync",
            JobEvent::Settings(_) => "settings",
            JobEvent::RommReconciled(_) => "romm_reconciled",
        }
    }

    /// The Job the event is about; none for a `throughput`, `resync`, `settings` or `romm_reconciled`.
    pub fn job_id(&self) -> Option<i64> {
        match self {
            JobEvent::State(job) => Some(job.id),
            JobEvent::Progress(p) => Some(p.job),
            JobEvent::Throughput(_)
            | JobEvent::Resync(_)
            | JobEvent::Settings(_)
            | JobEvent::RommReconciled(_) => None,
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
