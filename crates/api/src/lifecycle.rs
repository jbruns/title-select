//! The lifecycle of registrations: Unregistering a Source Set, Library Folder, Additions Set or Ingest Folder, setting
//! a whole set aside, moving a Library Folder, Retiring a Selection Profile, and forgetting what an Unregistered
//! registration left in the store.

use serde::{Deserialize, Serialize};

/// The kind of a registration that can be Unregistered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationKind {
    SourceSet,
    LibraryFolder,
    AdditionsSet,
    IngestFolder,
}

impl RegistrationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RegistrationKind::SourceSet => "source_set",
            RegistrationKind::LibraryFolder => "library_folder",
            RegistrationKind::AdditionsSet => "additions_set",
            RegistrationKind::IngestFolder => "ingest_folder",
        }
    }

    /// The registration's kind as you read it.
    pub fn label(self) -> &'static str {
        match self {
            RegistrationKind::SourceSet => "source set",
            RegistrationKind::LibraryFolder => "play library folder",
            RegistrationKind::AdditionsSet => "additions set",
            RegistrationKind::IngestFolder => "ingest folder",
        }
    }
}

/// What holds a registration, or a Root, registered: each reason it can't be Unregistered yet. Empty when nothing
/// does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holds {
    pub holds: Vec<String>,
}

/// What Unregistering did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unregistration {
    pub kind: RegistrationKind,
    pub id: i64,
    pub name: String,
    pub unregistered_at: String,
    /// What you may want to know: a marker left in place for kept files, or one that could not be removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// An Unregistered registration, whose history stays readable until you forget it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnregisteredRecord {
    pub kind: RegistrationKind,
    pub id: i64,
    pub name: String,
    pub root: String,
    /// Relative to the Root, with `/` separators; empty for an Ingest Folder, which is a whole Root.
    pub path: String,
    pub registered_at: String,
    pub unregistered_at: String,
    pub audits: u64,
    pub selections: u64,
    pub builds: u64,
    pub jobs: u64,
    /// Kept files still in its folder; it can't be forgotten while there are any.
    pub kept_files: u64,
}

/// The history an Unregistered registration left in the store, read-only: what it is, its finished Audits, its
/// Selections, its Builds and its Jobs, newest first. Each is empty for a kind that has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationHistory {
    pub record: UnregisteredRecord,
    pub audits: Vec<crate::AuditSummary>,
    pub selections: Vec<crate::SelectionRecord>,
    pub builds: Vec<crate::build::BuildSummary>,
    pub jobs: Vec<crate::Job>,
}

/// What forgetting an Unregistered registration removed from the store. No file was touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forgotten {
    pub kind: RegistrationKind,
    pub id: i64,
    pub name: String,
    pub audits: u64,
    pub selections: u64,
    pub builds: u64,
    pub jobs: u64,
    /// DAT revisions nothing else referred to, removed with their blobs.
    pub dat_revisions: u64,
}

/// The request to move or rename a Library Folder within its Root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFolderMove {
    /// The new path, relative to the same Root.
    pub path: String,
}

/// A plan to move or rename a Library Folder in place, which you approve by its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderMove {
    pub id: i64,
    pub folder: i64,
    pub root: String,
    pub from_path: String,
    pub to_path: String,
    pub digest: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// How it ended: `moved`, `failed: …` or `left: …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// A plan to set a whole Additions Set aside: every addition still in it is retired into kept files, then the set
/// is Unregistered. It is not stored; approving it checks its digest against the set as it is then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsSetAside {
    pub system: String,
    pub set: i64,
    /// The additions that will be retired, by file name.
    pub additions: Vec<String>,
    pub digest: String,
}
