//! Ingest Folders: writable folders where new files arrive, the items Title Select finds in them, what assessing each
//! one found, and the plans that move items out of them once you approve.

use serde::{Deserialize, Serialize};

use crate::build::{Approval, FileIdentity, FileStat};

/// What an Ingest Folder's marker says: that it is this store's Ingest Folder `ingest_folder`. Its fields differ from
/// a Managed Folder's marker, so neither is ever read as the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestMarker {
    pub store: String,
    pub ingest_folder: i64,
}

/// An Ingest Folder: a Root of kind `ingest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestFolder {
    /// The Root's name.
    pub root: String,
    pub path: String,
    /// Its id once you registered it; nothing is scanned or written before then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// How long an item's size and times must stay unchanged before it is assessed, in seconds.
    pub settle_secs: u64,
    /// How often it is scanned on its own, in minutes; none when only you start scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_every_minutes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<String>,
    /// How many items are in each state.
    #[serde(default)]
    pub counts: Vec<(ItemState, u64)>,
}

/// Registers the Ingest Folder of the Root `root`, writing its marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterIngestFolder {
    pub root: String,
}

/// Where an item is in its life in the Ingest Folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    /// Seen, but still changing, or not yet unchanged for the settle interval.
    Arriving,
    /// Read and matched; see its assessment.
    Assessed,
    /// It could not be read as what its name says, such as a damaged ZIP.
    Unreadable,
    /// Reading it failed; the next scan tries again.
    Failed,
    /// It changed after it was assessed; it is assessed again once it settles.
    Stale,
    /// It is no longer in the folder.
    Gone,
    /// Every Source Set it matches already holds it.
    AlreadyHeld,
    /// An archive Title Select does not open: 7z, RAR, a nested or whole-set archive, or a folder.
    Unsupported,
    /// You dismissed it; it stays dismissed until it changes.
    Dismissed,
    /// An approved plan moved it out of the folder.
    Moved,
}

impl ItemState {
    pub const ALL: [ItemState; 10] = [
        ItemState::Arriving,
        ItemState::Assessed,
        ItemState::Unreadable,
        ItemState::Failed,
        ItemState::Stale,
        ItemState::Gone,
        ItemState::AlreadyHeld,
        ItemState::Unsupported,
        ItemState::Dismissed,
        ItemState::Moved,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Arriving => "arriving",
            ItemState::Assessed => "assessed",
            ItemState::Unreadable => "unreadable",
            ItemState::Failed => "failed",
            ItemState::Stale => "stale",
            ItemState::Gone => "gone",
            ItemState::AlreadyHeld => "already_held",
            ItemState::Unsupported => "unsupported",
            ItemState::Dismissed => "dismissed",
            ItemState::Moved => "moved",
        }
    }

    pub fn parse(s: &str) -> Option<ItemState> {
        ItemState::ALL.into_iter().find(|state| state.as_str() == s)
    }

    /// The name the workbench shows.
    pub fn label(self) -> &'static str {
        match self {
            ItemState::AlreadyHeld => "already held",
            other => other.as_str(),
        }
    }
}

/// What a Source Set's latest Audit says of a Game an item matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// The Game is missing, or missing this ROM: the item fills it.
    Missing,
    /// An Entry for the Game is Mismatched: the item replaces it.
    Mismatched,
    /// An Entry for the Game is Unreadable: the item replaces it.
    Unreadable,
    /// The Source Set has no Audit with its current DAT Revision yet.
    Unaudited,
    /// The Source Set already holds the Game complete.
    Held,
}

impl Need {
    pub fn label(self) -> &'static str {
        match self {
            Need::Missing => "fills a missing game",
            Need::Mismatched => "replaces a mismatched entry",
            Need::Unreadable => "replaces an unreadable entry",
            Need::Unaudited => "matches; the source set has no audit yet",
            Need::Held => "already held",
        }
    }
}

/// A payload of an item: a ZIP member, or the whole file, with its checksums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub size: u64,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

/// The Evidence that an item's payloads are a Game's ROMs in a Source Set's current DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMatch {
    pub source_set: String,
    /// The DAT Revision matched against, by SHA-256.
    pub dat_revision: String,
    pub game: String,
    /// The ROMs matched, by name, each with the member that matched it, if a ZIP member did.
    pub roms: Vec<MatchedRom>,
    /// Whether every ROM of the Game is among the payloads.
    pub complete: bool,
    /// Whether the item holds payloads that are not the Game's ROMs.
    pub extra: bool,
    pub need: Need,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedRom {
    pub rom: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

/// A DAT file found in an Ingest Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundDat {
    /// The DAT's header name and version.
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// The providers of registered DATs with this name.
    pub providers: Vec<String>,
    /// Whether this exact DAT Revision is already imported.
    pub imported: bool,
}

/// What an item is to Title Select once assessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Proposal {
    /// It belongs in a Source Set: it fills a Missing Game, or replaces a Mismatched or Unreadable Entry.
    SourceSet {
        source_set: String,
        game: String,
        need: Need,
    },
    /// No Source Set's DAT knows it: a candidate Operator Addition, with the system its extension suggests.
    OperatorAddition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
    },
    /// A DAT file you may import.
    DatImport,
}

/// What assessing an item found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Assessment {
    /// The SHA-256 of the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub zip: bool,
    #[serde(default)]
    pub payloads: Vec<ItemPayload>,
    #[serde(default)]
    pub matches: Vec<ItemMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dat: Option<FoundDat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Proposal>,
}

/// An entry in an Ingest Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestItem {
    pub id: i64,
    pub name: String,
    pub state: ItemState,
    pub file: FileStat,
    pub identity: FileIdentity,
    pub first_seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessed_at: Option<String>,
    #[serde(default)]
    pub assessment: Assessment,
    /// Why it is Unreadable, Failed or Unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What a plan does with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestAction {
    /// Keep it aside in the Ingest Folder's kept files.
    Discard,
    /// Import it as a DAT Revision, then keep it aside.
    DatImport,
}

impl IngestAction {
    pub fn as_str(self) -> &'static str {
        match self {
            IngestAction::Discard => "discard",
            IngestAction::DatImport => "dat_import",
        }
    }

    pub fn parse(s: &str) -> Option<IngestAction> {
        [IngestAction::Discard, IngestAction::DatImport]
            .into_iter()
            .find(|a| a.as_str() == s)
    }
}

/// Plans `action` for `items` of an Ingest Folder; nothing changes until you approve the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIngest {
    pub action: IngestAction,
    pub items: Vec<i64>,
    /// For a DAT import: the provider to import under, when no registered DAT names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// An entry of the Ingest Folder as the plan saw it: what approval checks the folder still holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    pub name: String,
    pub identity: FileIdentity,
    pub file: FileStat,
    /// Its status change time, in nanoseconds since the Unix epoch: changed by any write, any change of its times,
    /// and any link made to it or removed.
    pub ctime_ns: i128,
    /// How many names link to it.
    pub nlink: u64,
}

/// One item a plan moves: where it goes and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestPlanItem {
    pub item: i64,
    pub name: String,
    pub identity: FileIdentity,
    pub file: FileStat,
    /// Its status change time, in nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
    pub sha256: Option<String>,
    /// Where it goes, relative to the Ingest Folder.
    pub destination: String,
    pub reason: String,
    /// For a DAT import: the provider it is imported under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// What an approval binds: the items, where each goes, and the whole folder as it was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestPlanContent {
    pub ingest_folder: i64,
    pub root: String,
    pub action: IngestAction,
    /// The kept files generation the plan's items are kept under.
    pub generation: u64,
    pub items: Vec<IngestPlanItem>,
    /// Every entry of the folder but Title Select's own, by name.
    pub fingerprint: Vec<FolderEntry>,
}

/// What happened to one item of an approved plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestOutcome {
    pub item: i64,
    pub name: String,
    pub moved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A plan for items of an Ingest Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestPlan {
    pub id: i64,
    /// The SHA-256 of the JSON of `content`, which an approval binds to.
    pub digest: String,
    pub created_at: String,
    pub content: IngestPlanContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub outcomes: Vec<IngestOutcome>,
}

/// A file a plan kept aside in an Ingest Folder's kept files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestKeptFile {
    pub id: i64,
    pub generation: u64,
    pub name: String,
    pub kept_name: String,
    pub reason: String,
    /// The plan that kept it, if a plan did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub file: FileStat,
    pub kept_at: String,
}
