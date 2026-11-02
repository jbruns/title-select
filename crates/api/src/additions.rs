//! Operator Additions: files no DAT knows that the Operator chose to keep, each identified by its own hashes, in an
//! Additions Set per system.

use serde::{Deserialize, Serialize};

use crate::build::{ExpectedRom, FileIdentity};
use crate::repair::SourceKeptFile;

/// What an Additions Set's marker records: the store that manages it and the set's id there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsMarker {
    pub store: String,
    pub additions_set: i64,
}

/// Where an Operator Addition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdditionSource {
    /// An item of an Ingest Folder that matched nothing.
    Ingest,
    /// Drift in a Managed Folder.
    Drift,
    /// A Source Set Entry no DAT knows.
    SourceSet,
}

impl AdditionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AdditionSource::Ingest => "ingest",
            AdditionSource::Drift => "drift",
            AdditionSource::SourceSet => "source_set",
        }
    }

    pub fn parse(s: &str) -> Option<AdditionSource> {
        [
            AdditionSource::Ingest,
            AdditionSource::Drift,
            AdditionSource::SourceSet,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            AdditionSource::Ingest => "ingest folder",
            AdditionSource::Drift => "managed folder drift",
            AdditionSource::SourceSet => "source set",
        }
    }
}

/// Where an Operator Addition is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdditionState {
    /// Approved; its file is being written into the Additions Set.
    Pending,
    /// Its file is in the Additions Set, and builds that opt in read it.
    Active,
    /// You retired it; its file is being moved into the Additions Set's kept files.
    Retiring,
    /// You retired it; builds no longer read it. Its file is among the Additions Set's kept files, which you may
    /// delete or restore it from.
    Retired,
    /// You restored it; its file is being moved back from the Additions Set's kept files.
    Restoring,
    /// Its file could not be written into the Additions Set; the original was left as it was.
    Failed,
}

impl AdditionState {
    pub fn as_str(self) -> &'static str {
        match self {
            AdditionState::Pending => "pending",
            AdditionState::Active => "active",
            AdditionState::Retiring => "retiring",
            AdditionState::Retired => "retired",
            AdditionState::Restoring => "restoring",
            AdditionState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<AdditionState> {
        [
            AdditionState::Pending,
            AdditionState::Active,
            AdditionState::Retiring,
            AdditionState::Retired,
            AdditionState::Restoring,
            AdditionState::Failed,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }
}

/// An Operator Addition: a file identified by its own hashes, never labelled a match of any DAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addition {
    pub id: i64,
    /// The system: the DAT its Additions Set belongs to, as `<provider>/<name>`.
    pub system: String,
    /// The editable name a build writes it as: `<name>.zip`, or, loose, `<name>` with its ROM's extension.
    pub name: String,
    /// Its file in the Additions Set, chosen when it was approved.
    pub file_name: String,
    /// Its ROM: the member of its file, with its own hashes, read when it was approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rom: Option<ExpectedRom>,
    /// The Game it is a version of, as metadata only: it never changes a Selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub source: AdditionSource,
    /// Where it came from: the ingest item, Source Set Entry or Managed Folder entry, by name.
    pub origin: String,
    pub state: AdditionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub approved_by: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    /// Whether its file is loose, its ROM itself, rather than a TorrentZip, as the storage policy said for its
    /// system's source sets when it was approved.
    #[serde(default)]
    pub loose: bool,
}

/// An Additions Set: the folder of one system's Operator Additions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsSet {
    pub id: i64,
    /// The DAT it belongs to, as `<provider>/<name>`.
    pub system: String,
    pub root: String,
    pub path: String,
    pub created_at: String,
    pub additions: Vec<Addition>,
    /// Retired additions' files, kept in the Additions Set.
    #[serde(default)]
    pub kept: Vec<SourceKeptFile>,
    /// Temporary folders attempts to create its folder left behind.
    #[serde(default)]
    pub leftovers: Vec<AdditionsLeftover>,
}

/// A temporary folder an attempt to create an Additions Set's folder left in the Root `additions`. Title Select never
/// deletes it; you may remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsLeftover {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<FileIdentity>,
    pub found_at: String,
}

/// What to make an Operator Addition as.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NewAddition {
    /// The system, as `<provider>/<name>` of a registered DAT.
    pub system: String,
    /// The name builds write it as: `<name>.zip`, or, loose, `<name>` with its ROM's extension.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// A change to an Operator Addition's name or parent Game. The name is applied at the next build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditAddition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `Some(None)` clears the parent; absent leaves it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub parent: Option<Option<String>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Option<String>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().expect("skipped when absent").serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(d).map(Some)
    }
}

/// What a check of an Additions Set found at one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdditionsFinding {
    /// An active addition's file, as recorded.
    Intact,
    /// An active addition's file is gone.
    Missing,
    /// An active addition's file no longer holds what was recorded.
    Changed,
    /// A retired addition's file, still in the folder while it is moved into the kept files.
    Retired,
    /// A file no addition records: drift.
    Unknown,
    /// A folder, link or other entry no addition records: drift.
    Other,
}

/// One entry of a check of an Additions Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsEntry {
    pub name: String,
    pub finding: AdditionsFinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addition: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A read-only check of an Additions Set against its additions' records, which act as its DAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionsCheck {
    pub system: String,
    pub checked_at: String,
    pub entries: Vec<AdditionsEntry>,
}

impl AdditionsCheck {
    /// Whether the check found drift: anything but intact files and retired additions' files.
    pub fn drifted(&self) -> bool {
        self.entries.iter().any(|e| {
            !matches!(
                e.finding,
                AdditionsFinding::Intact | AdditionsFinding::Retired
            )
        })
    }
}

/// The query for one system's Additions Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemQuery {
    pub system: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_tells_clearing_the_parent_from_leaving_it() {
        let leave: EditAddition = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert_eq!(leave.parent, None);
        let clear: EditAddition = serde_json::from_str(r#"{"parent":null}"#).unwrap();
        assert_eq!(clear.parent, Some(None));
        let set: EditAddition = serde_json::from_str(r#"{"parent":"Game"}"#).unwrap();
        assert_eq!(set.parent, Some(Some("Game".into())));
    }
}
