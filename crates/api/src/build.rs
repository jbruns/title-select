//! Library Folders, the Change Plans that build a Selection into one, and the Builds that carry them out.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::report::{EvidenceLevel, Finding};
use crate::selection::Role;

/// The marker file every Library Folder holds: its [`Marker`] as JSON.
pub const MARKER: &str = ".title-select";
/// The folder, inside a Library Folder, that a Build stages its files in, so that renames never cross mounts.
pub const STAGING: &str = ".title-select-staging";
/// The directory, inside a Library Folder, where Kept Files are set aside under `<generation>/`, each renamed with
/// [`kept_name`]. A check never reports it, or what is in it, as Drift.
pub const RETENTION: &str = ".title-select-retained";
/// The suffix every Kept File's name ends with: no tool that scans a folder for games matches it.
pub const KEPT_SUFFIX: &str = ".kept";

/// The name a file `name` is kept under in its generation's retention folder: the name with [`KEPT_SUFFIX`]
/// appended, and, from the second file of that name, a number before the suffix.
pub fn kept_name(name: &str, nth: u32) -> String {
    if nth <= 1 {
        format!("{name}{KEPT_SUFFIX}")
    } else {
        format!("{name}.{nth}{KEPT_SUFFIX}")
    }
}

/// What a Library Folder's marker records. The store stays the source of truth; the marker lets a plan tell its
/// folder from a folder another store manages, a folder restored from a backup, or a wrong mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The id of the store that manages the folder.
    pub store: String,
    /// The Library Folder's id in that store.
    pub folder: i64,
    /// The folder's build generation: 0 when registered, and one more after each successful Build.
    pub generation: u64,
}

/// A folder in a writable Root that Title Select builds a Source Set's Selections into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFolder {
    pub id: i64,
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub path: String,
    /// The system: the bound Source Set's DAT, as `<provider>/<name>`.
    pub system: String,
    pub source_set: String,
    /// The Selection Profile whose Selections are built here.
    pub profile: String,
    pub generation: u64,
    pub created_at: String,
    /// Whether the Root is writable now.
    pub writable: bool,
    /// The current build: the latest that succeeded.
    pub build: Option<BuildSummary>,
    /// Files the store records as built into the folder.
    pub files: u64,
    /// The Build Job Queued or Running for the folder.
    pub active_job: Option<i64>,
    /// The newest plan made for the folder.
    pub latest_plan: Option<i64>,
    /// What the folder lacks of its current build's selection, by reason; absent when it lacks nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<IncompleteCounts>,
    /// The latest build, when it stopped before it finished and no build has succeeded since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished: Option<UnfinishedBuild>,
    /// The latest check of the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckSummary>,
    /// The newest plan waits on approval: it is not approved, and was made at the folder's current generation.
    #[serde(default)]
    pub plan_open: bool,
    /// What changed since the current build's Selection was made, so that building again would build something
    /// else; empty when nothing did, and before the first build.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<Stale>,
}

/// One thing that changed since a Library Folder's current Build: its Source Set or its Selection Profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stale {
    /// The folder was rebound to another Source Set; `built` is the one its current Build was made from.
    SourceSetRebound { built: String },
    /// The Source Set's latest Audit found something other than the Audit the current Build used: `latest` is that
    /// Audit, `built` the Build's.
    SourceSetChanged { built: i64, latest: i64 },
    /// The folder was rebound to another Profile; `built` is the one its current Build's Selection used.
    ProfileRebound { built: String },
    /// The Profile was saved since: `latest` is its newest version, `built` the one the current Build's Selection
    /// used.
    ProfileSaved { built: u32, latest: u32 },
}

/// A build that stopped before it finished, as the store recorded it: shown straight after a restart, without a
/// rescan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfinishedBuild {
    pub build: i64,
    /// `interrupted` when the service stopped, `failed` when the build stopped itself.
    pub state: BuildState,
    /// Files renamed into place.
    pub applied: u64,
    /// Files whose target holds something other than what the build wrote.
    pub uncertain: u64,
    /// Files the build never reached, or that failed.
    pub not_written: u64,
    /// Files it kept aside before it stopped.
    #[serde(default)]
    pub kept: u64,
}

/// The request to register a Library Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddLibraryFolder {
    /// A writable Root.
    pub root: String,
    /// Relative to the Root. When absent, the path `template` proposes for the Source Set's DAT is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The Path Template to propose the path from when `path` is absent; by default the configured one. Giving
    /// both is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    pub source_set: String,
    pub profile: String,
}

/// The request to rebind a Library Folder to another Source Set of the same DAT, or to another Profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLibraryFolder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// The request to plan building a Selection into a Library Folder, with your choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBuild {
    pub selection: i64,
    #[serde(flatten)]
    pub choices: PlanChoices,
}

impl PlanBuild {
    pub fn new(selection: i64) -> PlanBuild {
        PlanBuild {
            selection,
            choices: PlanChoices::default(),
        }
    }
}

/// What you chose for a plan. They are part of its content, and so of the digest you approve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChoices {
    /// Build the resolved groups only, leaving unresolved groups untouched, instead of blocking on them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolved_only: bool,
    /// The targets in conflict whose file you adopt: recorded as built, without writing anything. Any other conflict
    /// is left as it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopt: Vec<String>,
    /// The targets in conflict whose file you keep aside: moved into the folder's retention area as a kept file, so
    /// that the game is built there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep_aside: Vec<String>,
}

/// The request to approve a plan: the digest of the plan you reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovePlan {
    pub digest: String,
}

/// What a plan does with one selected release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Copy a source archive that is already the Game's TorrentZip.
    Copy,
    /// Write a TorrentZip from the verified payloads.
    Repack,
    /// The folder already holds this file, built from the same ROMs.
    Unchanged,
    /// No Entry in the Audit holds every ROM; skipped.
    NoSource,
    /// A file Title Select did not build is at the target, or one it built was changed there; it is never
    /// overwritten, so the item is skipped.
    Conflict,
    /// Cannot be built as planned, such as an invalid name or an unmanaged file at the target; skipped.
    Blocked,
    /// A file in conflict at the target that you chose to adopt: its ROMs are the ones the DAT expects there, so it
    /// is recorded as built and nothing is written.
    Adopt,
    /// Its group is unresolved and you chose to build the resolved groups only; skipped, and whatever is at its
    /// target is left untouched.
    Unresolved,
    /// Never a plan item: a Build's item that removes a built file whose game is no longer selected, one of the
    /// plan's removals.
    Remove,
    /// Never a plan item: a cleanup's item that keeps aside an entry Title Select did not build.
    KeepAside,
    /// Never a plan item: a cleanup's item that acknowledges an entry Title Select did not build, leaving it.
    Acknowledge,
    /// Never a plan item: a cleanup's item that puts a kept file back in the folder.
    Restore,
}

impl ItemKind {
    pub const ALL: [ItemKind; 12] = [
        ItemKind::Copy,
        ItemKind::Repack,
        ItemKind::Adopt,
        ItemKind::Unchanged,
        ItemKind::NoSource,
        ItemKind::Conflict,
        ItemKind::Unresolved,
        ItemKind::Blocked,
        ItemKind::Remove,
        ItemKind::KeepAside,
        ItemKind::Acknowledge,
        ItemKind::Restore,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Copy => "copy",
            ItemKind::Repack => "repack",
            ItemKind::Unchanged => "unchanged",
            ItemKind::NoSource => "no_source",
            ItemKind::Conflict => "conflict",
            ItemKind::Blocked => "blocked",
            ItemKind::Adopt => "adopt",
            ItemKind::Unresolved => "unresolved",
            ItemKind::Remove => "remove",
            ItemKind::KeepAside => "keep_aside",
            ItemKind::Acknowledge => "acknowledge",
            ItemKind::Restore => "restore",
        }
    }

    pub fn parse(s: &str) -> Option<ItemKind> {
        ItemKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether a Build writes a file for the item.
    pub fn writes(self) -> bool {
        matches!(self, ItemKind::Copy | ItemKind::Repack)
    }
}

/// A ROM a built file must hold, as the DAT Revision lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedRom {
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// What a file is, as opposed to the name it is found at: its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

/// Why a file is kept aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    /// A built file whose game is no longer selected.
    Removal,
    /// A built file whose game the selection or DAT revision now wants built from other ROMs.
    Replacement,
    /// A file in conflict at a selected game's target that you chose to keep aside and rebuild.
    Conflict,
    /// An entry Title Select did not build that you chose to keep aside in a cleanup.
    Drift,
    /// A built file that was not TorrentZip, kept as it was when a cleanup repacked it.
    Repack,
}

impl KeepReason {
    pub fn as_str(self) -> &'static str {
        match self {
            KeepReason::Removal => "removal",
            KeepReason::Replacement => "replacement",
            KeepReason::Conflict => "conflict",
            KeepReason::Drift => "drift",
            KeepReason::Repack => "repack",
        }
    }

    pub fn parse(s: &str) -> Option<KeepReason> {
        [
            KeepReason::Removal,
            KeepReason::Replacement,
            KeepReason::Conflict,
            KeepReason::Drift,
            KeepReason::Repack,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// A file a plan keeps aside: moved, as it is, into the folder's retention area. It is moved only while it is still
/// this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepAside {
    pub reason: KeepReason,
    /// Its name in the folder, which may differ from the target in case.
    pub name: String,
    pub file: FileStat,
    pub identity: FileIdentity,
    /// Its SHA-256, when the store recorded it or the plan's check read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Whether it is a directory, moved whole and never read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub directory: bool,
}

/// What a plan does with a built file whose game is no longer selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    /// Keep it aside in the retention area.
    Keep,
    /// It is gone already; its record is dropped.
    Forget,
    /// It changed since it was built, so it is not the file Title Select built; it is left as it is.
    Leave,
}

/// A built file whose game is no longer selected, and what the plan does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRemoval {
    pub target: String,
    pub game: String,
    pub kind: RemovalKind,
    pub reason: String,
    /// The file kept aside, for a removal that keeps one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<KeepAside>,
    /// For the original of a conversion that stopped before it was disposed of: its game's file, in place already
    /// in the format the storage policy says. With delete originals after conversion on, the original is deleted
    /// once that file verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converts_to: Option<String>,
}

/// A file's size and modification time, as a plan saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
}

/// The Entry a ROM is taken from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSource {
    /// The ROM's name in the DAT.
    pub rom: String,
    /// Relative to the Source Set's folder.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// What the Audit found the Entry to be; none for an Addition, which is never a match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceLevel>,
    /// The file at `path`.
    pub file: FileStat,
    /// Which file the plan found at `path`: a loose file is written from it only while it is still that file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<FileIdentity>,
    /// For a loose file's source, its status change time when the plan found it, in nanoseconds since the Unix epoch:
    /// with `identity`, it tells the file apart from another that took its inode number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctime_ns: Option<i128>,
}

/// The Addition a plan item builds, from its system's Additions Set rather than the Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAddition {
    pub id: i64,
    pub additions_set: i64,
    /// Its file in the Additions Set, by identity, as recorded when it was published.
    pub identity: FileIdentity,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Whether its file in the Additions Set is loose, its ROM itself, rather than a TorrentZip.
    #[serde(default)]
    pub loose: bool,
}

/// One selected release in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub group: String,
    /// The DAT's Game name.
    pub game: String,
    /// The file name in the Library Folder: `<game>.zip`, or, stored loose, its one ROM's name.
    pub target: String,
    pub kind: ItemKind,
    /// Whether it is written as a loose file, its one ROM, rather than a TorrentZip, as the storage policy says.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loose: bool,
    pub roms: Vec<ExpectedRom>,
    /// One per ROM, in ROM order; empty unless the item is built or unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PlanSource>,
    /// Why the item has no source, conflicts or is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What is at the target now, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<FileStat>,
    /// For a conflict: the file at the target holds the ROMs the DAT expects there, so you may adopt it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adoptable: bool,
    /// For a conflict: the entry at the target is a regular file, so you may keep it aside and rebuild.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keepable: bool,
    /// The file at the target that is kept aside before this one is written: a replacement, or a conflict you
    /// chose to keep aside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<KeepAside>,
    /// The game's file built in the other format, under its other name, that this one converts: kept aside once
    /// this one is in place, and, when the storage policy says so, deleted once this one is verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converts: Option<KeepAside>,
    /// For an Addition: which, read from its Additions Set. Its `game` is the addition's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition: Option<PlanAddition>,
}

/// Why a plan cannot be approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    /// The Source Set has no Audit to take sources from.
    NoAudit,
    /// The latest Audit is against another DAT Revision than the Selection.
    RevisionMismatch,
    /// A group is unresolved and you have not confirmed a choice.
    Unresolved,
    /// The Root is not writable.
    ReadOnlyRoot,
    /// The folder is missing.
    FolderMissing,
    /// The folder's marker does not match the store.
    Marker,
    /// No longer raised: a replacement keeps the old file aside. Kept so that older plans still read.
    Replacement,
    /// No longer raised: a removal keeps the file aside. Kept so that older plans still read.
    Removal,
    /// An earlier build stopped and its folder has not been cleaned up since, or the folder holds leftovers no
    /// build of this store accounts for.
    Recovery,
    /// A cleanup action you chose cannot be carried out as the folder is now.
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub kind: BlockerKind,
    pub message: String,
    /// The group to confirm, for an unresolved group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The file in the folder it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// What a plan covers, in the order its digest is computed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanContent {
    pub folder: i64,
    /// The folder's build generation the plan starts from.
    pub generation: u64,
    pub selection: i64,
    /// The Audit the sources come from.
    pub audit: Option<i64>,
    /// The Selection's DAT Revision, by SHA-256.
    pub dat_revision: String,
    /// Your choices, as you made them when planning.
    #[serde(default, skip_serializing_if = "PlanChoices::is_default")]
    pub choices: PlanChoices,
    pub items: Vec<PlanItem>,
    /// The built files whose games are no longer selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removals: Vec<PlanRemoval>,
    pub blockers: Vec<Blocker>,
    /// For a cleanup of the folder's drift, rather than a build: what it does. A cleanup has no items or removals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<Cleanup>,
    /// What the storage policy says for the folder's system when the plan was made.
    #[serde(default, skip_serializing_if = "crate::storage::is_default")]
    pub storage: crate::storage::Storage,
    /// What the plan does not do as the storage policy says, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl PlanChoices {
    pub fn is_default(&self) -> bool {
        *self == PlanChoices::default()
    }
}

/// What caused a release to change from the folder's current build to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Your Profile's settings: another Profile version.
    Settings,
    /// Your choices.
    Choices,
    /// A newer DAT Revision.
    DatRevision,
}

/// One release that changes from the folder's current build to the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChange {
    pub group: String,
    pub title: String,
    /// Absent when the release is new.
    pub before: Option<Role>,
    /// Absent when the release is gone.
    pub after: Option<Role>,
    pub cause: Cause,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub copy: u64,
    pub repack: u64,
    pub unchanged: u64,
    pub no_source: u64,
    pub conflict: u64,
    pub blocked: u64,
    #[serde(default)]
    pub adopt: u64,
    #[serde(default)]
    pub unresolved: u64,
    /// Files kept aside: replacements, conflicts you chose to keep aside, and removals.
    #[serde(default)]
    pub keep: u64,
    /// Built files no longer selected, whether kept aside, forgotten or left.
    #[serde(default)]
    pub remove: u64,
    /// A cleanup's entries acknowledged.
    #[serde(default)]
    pub acknowledge: u64,
    /// A cleanup's kept files put back.
    #[serde(default)]
    pub restore: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub digest: String,
    pub actor: String,
    pub at: String,
}

/// A Change Plan that builds a Selection into a Library Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: i64,
    /// The SHA-256 of the canonical JSON of `content`, which an approval binds to.
    pub digest: String,
    pub created_at: String,
    pub content: PlanContent,
    pub summary: PlanSummary,
    /// The Selection of the folder's current build, which `changes` compares against.
    pub previous_selection: Option<i64>,
    pub changes: Vec<PlanChange>,
    pub approval: Option<Approval>,
    /// The Build Job that carries the plan out, once approved.
    pub job: Option<i64>,
    /// The check of the folder made with the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<i64>,
}

impl Plan {
    pub fn approvable(&self) -> bool {
        self.content.blockers.is_empty()
    }
}

impl PlanContent {
    pub fn summary(&self) -> PlanSummary {
        let mut s = PlanSummary::default();
        for item in &self.items {
            *match item.kind {
                ItemKind::Copy => &mut s.copy,
                ItemKind::Repack => &mut s.repack,
                ItemKind::Unchanged => &mut s.unchanged,
                ItemKind::NoSource => &mut s.no_source,
                ItemKind::Conflict => &mut s.conflict,
                ItemKind::Blocked => &mut s.blocked,
                ItemKind::Adopt => &mut s.adopt,
                ItemKind::Unresolved => &mut s.unresolved,
                ItemKind::Remove => &mut s.remove,
                ItemKind::KeepAside => &mut s.keep,
                ItemKind::Acknowledge => &mut s.acknowledge,
                ItemKind::Restore => &mut s.restore,
            } += 1;
            if item.keep.is_some() {
                s.keep += 1;
            }
        }
        s.remove = self.removals.len() as u64;
        s.keep += self
            .removals
            .iter()
            .filter(|r| r.kind == RemovalKind::Keep)
            .count() as u64;
        if let Some(cleanup) = &self.cleanup {
            for entry in &cleanup.entries {
                match entry.action {
                    Some(CleanupAction::KeepAside | CleanupAction::AddAddition) => s.keep += 1,
                    Some(CleanupAction::Adopt) => s.adopt += 1,
                    Some(CleanupAction::Acknowledge) => s.acknowledge += 1,
                    None => {}
                }
            }
            for restore in &cleanup.restores {
                match restore.kind {
                    RestoreKind::Adopt => s.adopt += 1,
                    RestoreKind::Restore | RestoreKind::PutBack => s.restore += 1,
                }
            }
            s.repack += cleanup.repacks.len() as u64;
            s.keep += cleanup.repacks.len() as u64;
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl BuildState {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildState::Running => "running",
            BuildState::Succeeded => "succeeded",
            BuildState::Failed => "failed",
            BuildState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<BuildState> {
        [
            BuildState::Running,
            BuildState::Succeeded,
            BuildState::Failed,
            BuildState::Interrupted,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// A Build: one approved plan carried out by a Build Job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSummary {
    pub id: i64,
    pub folder: i64,
    pub plan: i64,
    pub job: i64,
    pub state: BuildState,
    /// The folder's generation once the Build succeeded.
    pub generation: u64,
    pub selection: i64,
    pub audit: i64,
    pub dat_revision: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Files renamed into place, files that failed, and files not reached.
    pub applied: u64,
    pub failed: u64,
    pub pending: u64,
    /// Files whose target, once the build stopped, held something other than what it wrote.
    #[serde(default)]
    pub uncertain: u64,
    /// What the plan held, including the items it skipped.
    pub planned: PlanSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    /// Not written yet; after an interruption, never written.
    Pending,
    /// Written and verified, and being renamed into place.
    Publishing,
    /// Renamed into place.
    Applied,
    /// Its re-hash or validation failed, or it could not be written; nothing was put in place.
    Failed,
    /// The build stopped while renaming it into place, and what is at its target now is not the file it wrote; that
    /// file is left as it is.
    Uncertain,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Publishing => "publishing",
            ItemState::Applied => "applied",
            ItemState::Failed => "failed",
            ItemState::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<ItemState> {
        [
            ItemState::Pending,
            ItemState::Publishing,
            ItemState::Applied,
            ItemState::Failed,
            ItemState::Uncertain,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// One file a Build writes, adopts or removes, and how it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildItem {
    pub target: String,
    pub game: String,
    pub kind: ItemKind,
    pub state: ItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildDetail {
    pub build: BuildSummary,
    pub items: Vec<BuildItem>,
}

/// A file in a Library Folder and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltFile {
    pub target: String,
    pub game: String,
    /// `copy` or `repack`.
    pub kind: ItemKind,
    pub build: i64,
    pub selection: i64,
    pub audit: i64,
    pub dat_revision: String,
    pub sources: Vec<PlanSource>,
    pub roms: Vec<ExpectedRom>,
    pub file: FileStat,
    /// The SHA-256 of the whole file, when the store recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// False once a check found the file holds the right ROMs in a ZIP that is not TorrentZip, such as one re-zipped
    /// by another tool.
    #[serde(default = "yes")]
    pub torrentzip: bool,
}

fn yes() -> bool {
    true
}

/// Why a Library Folder is Incomplete: why its current build lacks a selected game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    /// Its group was unresolved and you built the resolved groups only.
    Unresolved,
    /// No audited file holds every ROM.
    NoSource,
    /// Its file failed to build.
    Failed,
    /// Something Title Select did not build, or changed since, is at its target.
    Conflict,
    /// It cannot be built as planned, such as for an invalid name.
    Blocked,
    /// A check found its built file gone.
    Missing,
}

impl IncompleteReason {
    pub const ALL: [IncompleteReason; 6] = [
        IncompleteReason::Unresolved,
        IncompleteReason::NoSource,
        IncompleteReason::Failed,
        IncompleteReason::Conflict,
        IncompleteReason::Blocked,
        IncompleteReason::Missing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IncompleteReason::Unresolved => "unresolved",
            IncompleteReason::NoSource => "no_source",
            IncompleteReason::Failed => "failed",
            IncompleteReason::Conflict => "conflict",
            IncompleteReason::Blocked => "blocked",
            IncompleteReason::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Option<IncompleteReason> {
        IncompleteReason::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// How many selected games a Library Folder lacks, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteCounts {
    pub unresolved: u64,
    pub no_source: u64,
    pub failed: u64,
    pub conflict: u64,
    pub blocked: u64,
    pub missing: u64,
}

impl IncompleteCounts {
    pub fn get(&self, reason: IncompleteReason) -> u64 {
        match reason {
            IncompleteReason::Unresolved => self.unresolved,
            IncompleteReason::NoSource => self.no_source,
            IncompleteReason::Failed => self.failed,
            IncompleteReason::Conflict => self.conflict,
            IncompleteReason::Blocked => self.blocked,
            IncompleteReason::Missing => self.missing,
        }
    }

    pub fn add(&mut self, reason: IncompleteReason) {
        *match reason {
            IncompleteReason::Unresolved => &mut self.unresolved,
            IncompleteReason::NoSource => &mut self.no_source,
            IncompleteReason::Failed => &mut self.failed,
            IncompleteReason::Conflict => &mut self.conflict,
            IncompleteReason::Blocked => &mut self.blocked,
            IncompleteReason::Missing => &mut self.missing,
        } += 1;
    }

    pub fn total(&self) -> u64 {
        IncompleteReason::ALL.into_iter().map(|r| self.get(r)).sum()
    }
}

/// One selected game a Library Folder lacks, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteItem {
    pub reason: IncompleteReason,
    pub group: String,
    pub game: String,
    pub target: String,
    /// The error of a failed file, or why the item was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What a Library Folder lacks: what its current build recorded, and the conflicts and missing files its latest
/// check found since. Never written to the marker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incomplete {
    /// The current build.
    pub build: Option<i64>,
    pub counts: IncompleteCounts,
    pub items: Vec<IncompleteItem>,
}

/// What a check found for one built file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcome {
    /// Its size and modification time are as recorded; it was not read.
    Unchanged,
    /// Its size or modification time changed, but it is still the file the build wrote; the record was refreshed.
    Refreshed,
    /// It holds the ROMs the DAT expects there in another ZIP, such as one re-zipped by another tool; recorded as
    /// built, and flagged when it is not TorrentZip.
    Repacked,
    /// It changed and no longer holds the ROMs the DAT expects there, or is no longer a regular file.
    Conflict,
    /// It is gone.
    Missing,
    /// It could not be read, or changed while it was read.
    Unreadable,
}

impl FileOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            FileOutcome::Unchanged => "unchanged",
            FileOutcome::Refreshed => "refreshed",
            FileOutcome::Repacked => "repacked",
            FileOutcome::Conflict => "conflict",
            FileOutcome::Missing => "missing",
            FileOutcome::Unreadable => "unreadable",
        }
    }
}

/// One built file, as a check found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedFile {
    pub target: String,
    pub game: String,
    pub outcome: FileOutcome,
    /// Whether the check read the file. A file whose size and modification time are as recorded is never read.
    pub hashed: bool,
    /// False when the file holds the right ROMs in a ZIP that is not TorrentZip; true for a loose file.
    pub torrentzip: bool,
    /// Whether the selection the check was made against selects it.
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What an entry Title Select did not build is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// A game the DAT lists, under its own name, that the selection does not select.
    Unselected,
    /// A selected game, under its own name, that Title Select did not build. A plan can adopt it.
    Selected,
    /// A game the DAT lists, under another name.
    Renamed,
    /// Nothing the DAT lists: a hack, homebrew or junk, or a file that cannot be read as one.
    Unknown,
    /// A directory, which is never opened.
    Directory,
}

impl DriftKind {
    pub const ALL: [DriftKind; 5] = [
        DriftKind::Unselected,
        DriftKind::Selected,
        DriftKind::Renamed,
        DriftKind::Unknown,
        DriftKind::Directory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DriftKind::Unselected => "unselected",
            DriftKind::Selected => "selected",
            DriftKind::Renamed => "renamed",
            DriftKind::Unknown => "unknown",
            DriftKind::Directory => "directory",
        }
    }
}

/// One entry in a Library Folder that Title Select did not build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftEntry {
    pub name: String,
    pub kind: DriftKind,
    /// The DAT's game it holds, for a known title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// Whether it sits at a selected game's target, which makes the folder Incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict: bool,
    /// Whether the check read it; one unchanged since an earlier check is not read again.
    pub hashed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The counts of a check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSummary {
    pub id: i64,
    pub at: String,
    /// The Check Job that made it, or none when a plan made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
    /// Why the folder's marker is not this store's and this folder's; absent when it is. The rest of the check is
    /// skipped when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    pub unchanged: u64,
    pub refreshed: u64,
    pub repacked: u64,
    /// Built files in conflict, and entries in conflict at a selected game's target.
    pub conflict: u64,
    pub missing: u64,
    pub unreadable: u64,
    /// Files the check read.
    pub hashed: u64,
    /// Entries Title Select did not build.
    pub drift: u64,
    /// Entries Title Select did not build that you acknowledged, unchanged since; not counted as drift.
    #[serde(default)]
    pub acknowledged: u64,
}

/// A read-only check of a Library Folder against the store's record of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCheck {
    pub folder: i64,
    pub summary: CheckSummary,
    /// The folder's generation when checked.
    pub generation: u64,
    /// The Selection checked against: the plan's, or the current build's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<i64>,
    /// The DAT Revision entries were hashed against, by SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dat_revision: Option<String>,
    pub files: Vec<CheckedFile>,
    pub drift: Vec<DriftEntry>,
    /// The entries you acknowledged, unchanged since, which are not drift.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged: Vec<DriftEntry>,
}

/// A kept file: a file a Build moved out of its Library Folder into the folder's retention area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptFile {
    pub id: i64,
    pub folder: i64,
    /// The generation of the Build that kept it: the folder in the retention area it is in.
    pub generation: u64,
    /// Its name in the folder.
    pub name: String,
    /// Its name in the retention area, in `<generation>/`.
    pub kept_name: String,
    pub reason: KeepReason,
    /// The Build that kept it.
    pub build: i64,
    /// The Build that wrote it, when Title Select built it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_by: Option<i64>,
    /// The DAT's game it was built for, when Title Select built it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// Its SHA-256; none for a directory, which is never read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub file: FileStat,
    pub identity: FileIdentity,
    pub kept_at: String,
    /// Whether it is a directory, kept aside whole.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub directory: bool,
}

/// What a cleanup does with an entry Title Select did not build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupAction {
    /// Move it, whole, into the folder's retention area as a kept file.
    KeepAside,
    /// Record it as built: only a file that holds exactly a selected game's ROMs, renamed to that game's file name
    /// when it is not already, and never over anything.
    Adopt,
    /// Leave it, and stop reporting it as drift until it changes.
    Acknowledge,
    /// Keep it aside, then add it as an Addition from there: only a file that holds one ROM.
    AddAddition,
}

impl CleanupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanupAction::KeepAside => "keep_aside",
            CleanupAction::Adopt => "adopt",
            CleanupAction::Acknowledge => "acknowledge",
            CleanupAction::AddAddition => "add_addition",
        }
    }
}

/// The request to plan a cleanup of a Library Folder's drift: an action for each entry you chose, the kept files to
/// put back, and the built files to repack as TorrentZip. Anything not named is left as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCleanup {
    /// Entries Title Select did not build, by name, and what to do with each.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, CleanupAction>,
    /// Kept files to put back, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restore: Vec<i64>,
    /// Built files that are not TorrentZip to repack, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repack: Vec<String>,
    /// For each entry added as an Addition, by name: what to add it as.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub additions: BTreeMap<String, crate::additions::NewAddition>,
}

impl PlanCleanup {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.restore.is_empty() && self.repack.is_empty()
    }
}

/// An entry Title Select did not build, as a cleanup found it, and what it does with it. A cleanup lists every such
/// entry, chosen or not, so that its digest binds the folder as it was planned against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupEntry {
    pub name: String,
    pub kind: DriftKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    pub file: FileStat,
    pub identity: FileIdentity,
    /// Its SHA-256, when a check read it; never for a directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<CleanupAction>,
    /// For an adoption: the name it is recorded under, the game's file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// For an adoption: the ROMs the DAT expects in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roms: Vec<ExpectedRom>,
    /// For an adoption: the group the game is selected in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Whether the check found it was not TorrentZip, for an adoption.
    #[serde(default = "yes")]
    pub torrentzip: bool,
    /// For an Addition: what it is added as. Its one ROM, as the check read it, is in `roms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition: Option<crate::additions::NewAddition>,
    /// For an Addition: where it is published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition_target: Option<crate::additions::AdditionTarget>,
}

/// How a cleanup puts a kept file back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreKind {
    /// Back under its own name, which is a selected game's file name, recorded as built.
    Restore,
    /// Under the file name of the selected game it holds, recorded as built: its own name is taken, or is not that
    /// game's.
    Adopt,
    /// Back under its own name, as an entry Title Select did not build: it holds no selected game.
    PutBack,
}

/// A kept file a cleanup puts back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupRestore {
    pub kept: KeptFile,
    pub kind: RestoreKind,
    /// The name it is put back under.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roms: Vec<ExpectedRom>,
    /// The group the game is selected in, when it holds a selected game.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default = "yes")]
    pub torrentzip: bool,
}

/// A built file a cleanup rewrites as TorrentZip from its own contents, keeping the file as it was aside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupRepack {
    pub target: String,
    pub game: String,
    /// The group the game is selected in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    pub roms: Vec<ExpectedRom>,
    /// Where the file was built from, which its record keeps once it is repacked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PlanSource>,
    pub file: FileStat,
    pub identity: FileIdentity,
    pub sha256: String,
}

/// What a cleanup of a Library Folder's drift does, against the folder's current Build `build`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cleanup {
    pub build: i64,
    pub choices: PlanCleanup,
    pub entries: Vec<CleanupEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restores: Vec<CleanupRestore>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repacks: Vec<CleanupRepack>,
    /// The SHA-256 of what the folder held as the cleanup was planned: each entry in it, each built file's record
    /// and what is at its name, each acknowledged entry and each kept file, by identity, size and modification time.
    /// The plan is carried out only while the folder still holds the same.
    #[serde(default)]
    pub fingerprint: String,
}

impl Cleanup {
    /// Whether it moves or writes anything in the folder, rather than only acknowledging entries.
    pub fn changes_folder(&self) -> bool {
        !self.restores.is_empty()
            || !self.repacks.is_empty()
            || self.entries.iter().any(|e| {
                matches!(
                    e.action,
                    Some(
                        CleanupAction::KeepAside
                            | CleanupAction::Adopt
                            | CleanupAction::AddAddition
                    )
                )
            })
    }
}

/// The request to plan deleting kept files of a Library Folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanKeptDeletion {
    /// The kept files to delete, by id; every kept file of the folder when `all` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

/// What deleting kept files covers, in the order its digest is computed over: each file as the store recorded it, and
/// as it was read, ctime and SHA-256, when the deletion was planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptDeletionContent {
    pub folder: i64,
    pub files: Vec<KeptFile>,
    #[serde(default)]
    pub reviewed: Vec<crate::repair::KeptFileReviewed>,
}

/// What deleting one kept file did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptDeletionOutcome {
    pub id: i64,
    pub kept_name: String,
    pub generation: u64,
    pub deleted: bool,
    /// Why it was left, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A plan to delete kept files, carried out once you approve it with its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptDeletion {
    pub id: i64,
    /// The SHA-256 of the canonical JSON of `content`, which an approval binds to.
    pub digest: String,
    pub created_at: String,
    pub content: KeptDeletionContent,
    pub approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// What each deletion did, once carried out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<KeptDeletionOutcome>,
    /// Why recovery could not finish it when the service last started: its folder could not be shown to be this
    /// store's. Its kept files stay recorded until it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
}

/// Why a name cannot be a file name on every filesystem a Play Library may be shared from, or `None` when it can.
pub fn invalid_name(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return Some(format!("{name:?} is not a file name"));
    }
    if let Some(ch) = name.chars().find(|c| "<>:\"/\\|?*".contains(*c)) {
        return Some(format!(
            "{name} contains {ch}, which Windows and SMB refuse"
        ));
    }
    if name.chars().any(char::is_control) {
        return Some(format!("{name:?} contains a control character"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(format!(
            "{name:?} ends with a dot or a space, which Windows and SMB drop"
        ));
    }
    if reserved_device(name) {
        return Some(format!(
            "{name} is named for a device, which Windows and SMB reserve"
        ));
    }
    None
}

/// Whether `name`, up to its first dot, is a Windows device name, whatever its case and extension.
fn reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => match upper.as_bytes() {
            [b'C', b'O', b'M', digit] | [b'L', b'P', b'T', digit] => (b'1'..=b'9').contains(digit),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_name_never_ends_in_an_es_de_system_extension() {
        // Every extension ES-DE's bundled systems match, across consoles, computers and arcade.
        const ES_DE: &[&str] = &[
            ".7z",
            ".zip",
            ".a26",
            ".a52",
            ".a78",
            ".bin",
            ".j64",
            ".jag",
            ".lnx",
            ".nes",
            ".fds",
            ".unf",
            ".unif",
            ".sfc",
            ".smc",
            ".fig",
            ".swc",
            ".bs",
            ".st",
            ".n64",
            ".z64",
            ".v64",
            ".ndd",
            ".gb",
            ".gbc",
            ".gba",
            ".agb",
            ".cgb",
            ".dmg",
            ".sgb",
            ".nds",
            ".3ds",
            ".cia",
            ".gcm",
            ".iso",
            ".wbfs",
            ".rvz",
            ".ciso",
            ".gcz",
            ".wad",
            ".wia",
            ".dol",
            ".elf",
            ".nsp",
            ".xci",
            ".md",
            ".gen",
            ".smd",
            ".sms",
            ".gg",
            ".sg",
            ".32x",
            ".cue",
            ".chd",
            ".gdi",
            ".cdi",
            ".m3u",
            ".pbp",
            ".cso",
            ".img",
            ".mdf",
            ".toc",
            ".ccd",
            ".pce",
            ".sgx",
            ".ngp",
            ".ngc",
            ".ws",
            ".wsc",
            ".vb",
            ".vboy",
            ".col",
            ".int",
            ".min",
            ".vec",
            ".o2",
            ".tap",
            ".tzx",
            ".z80",
            ".sna",
            ".dsk",
            ".adf",
            ".ipf",
            ".hdf",
            ".lha",
            ".d64",
            ".t64",
            ".prg",
            ".crt",
            ".g64",
            ".atr",
            ".xex",
            ".xfd",
            ".cas",
            ".rom",
            ".mx1",
            ".mx2",
            ".exe",
            ".com",
            ".bat",
            ".dosz",
            ".kat",
            ".lua",
            ".p8",
            ".png",
            ".tic",
            ".wasm",
            ".sh",
            ".desktop",
            ".app",
            ".lnk",
            ".url",
            ".rp",
            ".pak",
            ".scummvm",
            ".svm",
            ".dat",
            ".hdi",
            ".fdi",
            ".d88",
            ".88d",
            ".cmd",
            ".ldb",
            ".daphne",
            ".singe",
            ".mgw",
            ".rar",
            ".tar",
            ".gz",
            ".xz",
            ".zst",
            ".squashfs",
            ".ps3",
            ".psvita",
            ".vpk",
            ".xbe",
            ".xiso",
            ".bml",
            ".nca",
            ".nro",
            ".nso",
            ".kip",
            ".ecm",
            ".mds",
            ".vcd",
            ".cbn",
            ".nrg",
            ".pdi",
        ];
        for name in [
            "Pong (USA).zip",
            "Game.7z",
            "Disc.chd",
            "Game.zip.kept",
            "noext",
            "UPPER.ZIP",
        ] {
            for nth in [1, 2, 10] {
                let kept = kept_name(name, nth);
                assert!(kept.starts_with(name), "{kept}");
                let extension = &kept[kept.rfind('.').unwrap()..];
                assert_eq!(extension, KEPT_SUFFIX, "{kept}");
                assert!(
                    !ES_DE.iter().any(|e| e.eq_ignore_ascii_case(extension)),
                    "{kept} would be matched as a game"
                );
            }
        }
        assert_eq!(kept_name("Game.zip", 1), "Game.zip.kept");
        assert_eq!(kept_name("Game.zip", 2), "Game.zip.2.kept");
    }

    #[test]
    fn names_invalid_on_windows_or_smb_are_refused_and_dat_names_are_not() {
        for ok in [
            "Pong (USA).zip",
            "Combat (USA, Europe) (Rev 1).zip",
            "Café ★.zip",
            "Console (USA).zip",
            "COM10.zip",
            "LPT0.bin",
            "Auxiliary.a26",
        ] {
            assert_eq!(invalid_name(ok), None, "{ok}");
        }
        for bad in [
            "a<b.zip",
            "a>b.zip",
            "a:b.zip",
            "a\"b.zip",
            "a/b.zip",
            "a\\b.zip",
            "a|b.zip",
            "a?b.zip",
            "a*b.zip",
            "a\u{1}b.zip",
            "name.",
            "name ",
            "",
            "..",
            "CON.zip",
            "con",
            "Prn.a26",
            "aux.tar.gz",
            "NUL .zip",
            "com1.zip",
            "LPT9",
        ] {
            assert!(invalid_name(bad).is_some(), "{bad:?}");
        }
    }
}
