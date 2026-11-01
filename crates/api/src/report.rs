//! The `schema: 3` Audit report (see GLOSSARY.md and ADR 0002).

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The report shape this version writes.
pub const SCHEMA: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    Matched,
    Misnamed,
    Conflicting,
    Mismatched,
    Unmatched,
    Unreadable,
}

impl Finding {
    pub const ALL: [Finding; 6] = [
        Finding::Matched,
        Finding::Misnamed,
        Finding::Conflicting,
        Finding::Mismatched,
        Finding::Unmatched,
        Finding::Unreadable,
    ];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Finding::Matched => "matched",
            Finding::Misnamed => "misnamed",
            Finding::Conflicting => "conflicting",
            Finding::Mismatched => "mismatched",
            Finding::Unmatched => "unmatched",
            Finding::Unreadable => "unreadable",
        }
    }

    pub fn parse(s: &str) -> Option<Finding> {
        Finding::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Complete,
    Incomplete,
    Missing,
}

impl GameStatus {
    pub const ALL: [GameStatus; 3] = [
        GameStatus::Complete,
        GameStatus::Incomplete,
        GameStatus::Missing,
    ];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GameStatus::Complete => "complete",
            GameStatus::Incomplete => "incomplete",
            GameStatus::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Option<GameStatus> {
        GameStatus::ALL.into_iter().find(|g| g.as_str() == s)
    }
}

/// How an Audit knows an Entry's checksums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// Taken from an archive's directory (size and CRC32) without reading the payload. Never makes an Entry Verified.
    Claim,
    /// Computed by reading every byte of the payload.
    Payload,
}

impl EvidenceLevel {
    pub const ALL: [EvidenceLevel; 2] = [EvidenceLevel::Claim, EvidenceLevel::Payload];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceLevel::Claim => "claim",
            EvidenceLevel::Payload => "payload",
        }
    }

    pub fn parse(s: &str) -> Option<EvidenceLevel> {
        EvidenceLevel::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// How an Audit gets its Entries' checksums, chosen when it starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvidence {
    /// Claim Evidence from each ZIP archive's directory, without reading its payloads, unless an earlier read of
    /// an unchanged archive can be reused. A loose file has no directory, so it is treated as in an incremental
    /// Audit.
    Quick,
    /// Reuses the Payload Evidence of each file that is unchanged since its payload was last read, by the change
    /// test (a heuristic), and reads every other payload.
    #[default]
    Incremental,
    /// Reads every payload again, reusing nothing, except what an interrupted attempt of the same Audit Job read.
    Full,
}

impl AuditEvidence {
    pub const ALL: [AuditEvidence; 3] = [
        AuditEvidence::Quick,
        AuditEvidence::Incremental,
        AuditEvidence::Full,
    ];

    /// The name the report and the API use.
    pub fn as_str(self) -> &'static str {
        match self {
            AuditEvidence::Quick => "quick",
            AuditEvidence::Incremental => "incremental",
            AuditEvidence::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<AuditEvidence> {
        AuditEvidence::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// Why an Audit reused an earlier read of a payload instead of reading it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reuse {
    /// The file's device, inode, size, modification time and status change time were all as recorded when its
    /// payload was read. This change test is a heuristic: a file rewritten in place with its times restored would
    /// pass it.
    UnchangedFileHeuristic,
}

/// Every algorithm an Evidence can list, in report order.
pub const ALGORITHMS: [&str; 5] = ["size", "crc32", "md5", "sha1", "sha256"];

/// What the Audit actually checked for an Entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub level: EvidenceLevel,
    /// The size and checksum algorithms the Finding rests on, in the order size, crc32, md5, sha1, sha256.
    pub algorithms: Vec<Cow<'static, str>>,
    /// For Payload Evidence, when the payload was last read: by this Audit, or by an earlier one when `reused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<jiff::Timestamp>,
    /// Set when the payload was not read by this Audit, and why its earlier read was trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused: Option<Reuse>,
}

/// The DAT Revision an Audit is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevision {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub name: String,
    pub version: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
}

/// A ROM of a DAT Revision, by its Game and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RomRef {
    pub game: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the Source Set's root.
    pub path: String,
    /// The member's name, when the Entry is a member of a ZIP archive at `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub finding: Finding,
    /// The ROMs this Entry's payload matches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roms: Vec<RomRef>,
    /// For a Conflicting or Mismatched Entry, the ROM its name gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_rom: Option<RomRef>,
    /// Absent for an Unreadable Entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Why an Unreadable Entry could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub name: String,
    pub status: GameStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_roms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Version of this report's shape.
    pub schema: u32,
    pub dat_revision: DatRevision,
    pub source_set: String,
    /// How the Audit got its Entries' checksums. A report from before this was recorded read every payload.
    #[serde(default = "full")]
    pub evidence: AuditEvidence,
    pub started_at: jiff::Timestamp,
    pub finished_at: jiff::Timestamp,
    pub summary: Summary,
    pub entries: Vec<Entry>,
    pub games: Vec<Game>,
}

fn full() -> AuditEvidence {
    AuditEvidence::Full
}
