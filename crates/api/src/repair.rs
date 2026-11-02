//! Repairing a Source Set: a Change Plan proposed from its latest Audit and the assessed items of Ingest Folders,
//! which you review, change and approve before anything in the Source Set changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::additions::NewAddition;
use crate::build::{Approval, ExpectedRom, FileIdentity, FileStat, KeptDeletionOutcome};
use crate::ingest::FolderEntry;
use crate::storage::Storage;

/// What a Source Set's marker says: that it is this store's Source Set `source_set`. Its fields differ from those of
/// a Managed Folder's or an Ingest Folder's marker, so none is ever read as another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetMarker {
    pub store: String,
    pub source_set: i64,
}

/// The kind of a repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    /// A Game whose ROMs are present under names other than the DAT's: written under the DAT's names.
    Rename,
    /// A Game whose ROMs are present but not stored as the format repairs store Games in: repacked.
    Repack,
    /// A Missing Game filled from an Ingest Folder.
    Fill,
    /// A Game with a Mismatched or Unreadable Entry, replaced from an Ingest Folder.
    Replace,
    /// An Entry whose payload is one ROM while its name gives another.
    Conflict,
    /// A file holding nothing a Game needs: Unmatched Entries or duplicates.
    KeepAside,
    /// A Game's file, exactly its ROMs, stored in the format the Storage Policy no longer gives it: rewritten in the
    /// one it does.
    Convert,
}

impl RepairKind {
    pub fn label(self) -> &'static str {
        match self {
            RepairKind::Rename => "rename",
            RepairKind::Repack => "repack",
            RepairKind::Fill => "fill from ingest",
            RepairKind::Replace => "replace from ingest",
            RepairKind::Conflict => "conflict",
            RepairKind::KeepAside => "keep aside",
            RepairKind::Convert => "convert",
        }
    }
}

/// What to do with one repair item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairChoice {
    /// Carry out the repair the item proposes; for a Conflicting Entry, take its payload as the ROM it matches.
    Apply,
    /// Change nothing.
    Leave,
    /// Keep the file aside in the Source Set's kept files.
    KeepAside,
    /// Keep the file aside, then add its one ROM as an Operator Addition.
    AddAddition,
}

impl RepairChoice {
    pub fn label(self) -> &'static str {
        match self {
            RepairChoice::Apply => "apply",
            RepairChoice::Leave => "leave",
            RepairChoice::KeepAside => "keep aside",
            RepairChoice::AddAddition => "add as operator addition",
        }
    }
}

/// One thing a repair plan proposes, with the choices you have and the one it carries out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairItem {
    /// Stable across plans of the same Audit: `game:<name>`, `entry:<path>` or `entry:<path>#<member>`, or
    /// `file:<path>`.
    pub key: String,
    pub kind: RepairKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// The file the item is about, relative to the Source Set's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub summary: String,
    pub choices: Vec<RepairChoice>,
    pub default: RepairChoice,
    pub chosen: RepairChoice,
    /// Why the chosen repair cannot be carried out, or anything else to know about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it is added as an Operator Addition: its system, name and parent Game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition: Option<NewAddition>,
    /// When it is added as an Operator Addition: the one ROM its file held, with its hashes, read through a
    /// descriptor while the file held still when the plan was made. It is published only while it still holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition_rom: Option<crate::build::ExpectedRom>,
}

/// A file as the plan saw it, which a step changes only while it is still so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenFile {
    pub identity: FileIdentity,
    pub file: FileStat,
    pub ctime_ns: i128,
}

/// A ROM a step writes, and the Entry it is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRom {
    pub rom: ExpectedRom,
    /// The file it is read from, relative to the Source Set's folder.
    pub from: String,
    /// The member of `from` it is read from; none for a loose file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

/// One change a repair makes, in the order it is made. Externally tagged: a buffered (internally tagged or flattened)
/// form cannot carry the 128-bit times a [`SeenFile`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStep {
    /// Writes `target` from `roms`, each read from a file the plan saw as in `sources`, under a staged name; it is
    /// published once every file it displaces is kept aside.
    Build {
        target: String,
        roms: Vec<RepairRom>,
        sources: BTreeMap<String, SeenFile>,
        /// Written as a loose file, its one ROM, rather than a TorrentZip.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        loose: bool,
    },
    /// Moves `path` into the kept files as `kept_name` in this repair's generation, without replacing anything; only
    /// once each Build step it `needs`, which reads it or replaces it, has staged its file. Besides the item it
    /// belongs to, it displaces `path` for each item it `affects`, whose steps a cancellation then never skips once it
    /// has run.
    Keep {
        path: String,
        kept_name: String,
        reason: String,
        seen: SeenFile,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        needs: Vec<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        affects: Vec<String>,
    },
    /// Renames `from` to `to` without replacing anything, once `from` is read again as exactly `roms`.
    Rename {
        from: String,
        to: String,
        seen: SeenFile,
        roms: Vec<ExpectedRom>,
        /// `from` is a loose file, its one ROM, rather than a TorrentZip.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        loose: bool,
    },
    /// Publishes the staged file of the Build step `build` as its target, without replacing anything.
    Publish { build: u32, target: String },
    /// Moves the Ingest Folder item `item` into the Source Set as `target`, written as a Game is stored; its original
    /// is deleted only when `exact` is set and what was written reads as exactly that ROM, and kept in the Ingest
    /// Folder's kept files otherwise.
    Fill {
        target: String,
        ingest_root: String,
        item: i64,
        name: String,
        seen: SeenFile,
        sha256: Option<String>,
        roms: Vec<RepairRom>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<ExpectedRom>,
        /// Written as a loose file, its one ROM, rather than a TorrentZip.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        loose: bool,
    },
    /// Deletes `path`, the original a conversion read, kept aside by the Keep step `keep`, once the file the Build
    /// step `build` wrote and the Publish step `publish` put in place is verified, and only while the original still
    /// reads as the conversion read it. Otherwise it stays in the kept files.
    Delete {
        path: String,
        build: u32,
        keep: u32,
        publish: u32,
    },
}

impl RepairStep {
    /// What the step does, as one line.
    pub fn describe(&self) -> String {
        match self {
            RepairStep::Build { target, roms, .. } => {
                let mut from: Vec<&str> = roms.iter().map(|r| r.from.as_str()).collect();
                from.dedup();
                format!("write {target} from {}", from.join(", "))
            }
            RepairStep::Keep {
                path, kept_name, ..
            } => format!("keep {path} aside as {kept_name}"),
            RepairStep::Rename { from, to, .. } => format!("rename {from} to {to}"),
            RepairStep::Publish { target, .. } => format!("put the new {target} in place"),
            RepairStep::Fill {
                target,
                ingest_root,
                name,
                ..
            } => format!("write {target} from {name} in ingest folder {ingest_root}"),
            RepairStep::Delete { path, .. } => {
                format!("delete the original {path} once its converted file is verified")
            }
        }
    }
}

/// A planned step, with the item it carries out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedStep {
    pub item: String,
    pub step: RepairStep,
}

/// What a repair plan covers, in the order its digest is computed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPlanContent {
    pub source_set: String,
    pub set_id: i64,
    pub root: String,
    pub path: String,
    /// The Audit the plan is proposed from; it must still be the Source Set's latest when you approve.
    pub audit: i64,
    pub dat_revision: String,
    /// The generation this repair keeps files under.
    pub generation: u64,
    /// Whether this repair writes the Source Set's marker, as the first repair of a Source Set does.
    pub creates_marker: bool,
    pub folder: FileIdentity,
    pub items: Vec<RepairItem>,
    pub steps: Vec<PlannedStep>,
    /// Every entry in the Source Set but Title Select's own, by path relative to its folder, as it was when the plan
    /// was made. Anything changed since refuses the approval.
    pub fingerprint: Vec<FolderEntry>,
    /// What the plan leaves as it is, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The Storage Policy the plan follows for the Source Set's system; it must still be the policy when you
    /// approve.
    #[serde(default, skip_serializing_if = "crate::storage::is_default")]
    pub storage: Storage,
}

/// What a repair of one item did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairOutcome {
    pub item: String,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A repair Change Plan for a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPlan {
    pub id: i64,
    /// The SHA-256 of the canonical JSON of `content`, which an approval binds to.
    pub digest: String,
    pub created_at: String,
    pub content: RepairPlanContent,
    pub approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<RepairOutcome>,
    /// Why recovery could not finish it when the service last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
    /// The incremental Audit the repair queued once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_job: Option<i64>,
    /// Set while that Audit waits for another Job of the Source Set to end.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audit_owed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_error: Option<String>,
    /// The latest Repair Job that carries it out, once it is approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
}

/// The body of a request for a repair plan: a choice for each item whose default you change, by its key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRepair {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, RepairChoice>,
    /// For each item you add as an Operator Addition: its system, name and parent Game, by its key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub additions: BTreeMap<String, NewAddition>,
}

/// A file a repair kept aside in a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceKeptFile {
    pub id: i64,
    /// The Source Set it was kept in; for a kept file of an Additions Set, that Additions Set.
    pub source_set: i64,
    /// The repair that kept it; for a kept file of an Additions Set, the Operator Addition retired.
    pub plan: i64,
    pub generation: u64,
    /// Where it was, relative to the Source Set's folder.
    pub name: String,
    pub kept_name: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub file: FileStat,
    pub identity: FileIdentity,
    pub kept_at: String,
}

/// What deleting kept files of a Source Set covers: each file as the store recorded it, and as it was read when
/// the deletion was planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceKeptDeletionContent {
    pub source_set: i64,
    pub files: Vec<SourceKeptFile>,
    #[serde(default)]
    pub reviewed: Vec<KeptFileReviewed>,
}

/// A kept file as it was read, through a descriptor held open, when its deletion was planned. It is deleted only
/// while it still reads the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptFileReviewed {
    pub id: i64,
    /// Nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
    pub sha256: String,
}

/// A plan to delete kept files of a Source Set, carried out once you approve it with its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceKeptDeletion {
    pub id: i64,
    pub digest: String,
    pub created_at: String,
    pub content: SourceKeptDeletionContent,
    pub approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<KeptDeletionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
}
