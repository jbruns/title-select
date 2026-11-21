//! The `schema: 6` Audit report (see GLOSSARY.md and ADR 0002).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// The report shape this version writes.
pub const SCHEMA: u32 = 6;

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
    /// The Game lists no ROM that must be present and can match: none at all, or only nodump or optional ones (ADR
    /// 0025). It is in none of the other counts and no Fixdat.
    NothingToCheck,
}

impl GameStatus {
    pub const ALL: [GameStatus; 4] = [
        GameStatus::Complete,
        GameStatus::Incomplete,
        GameStatus::Missing,
        GameStatus::NothingToCheck,
    ];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GameStatus::Complete => "complete",
            GameStatus::Incomplete => "incomplete",
            GameStatus::Missing => "missing",
            GameStatus::NothingToCheck => "nothing_to_check",
        }
    }

    /// Whether a Game of this status satisfies a Game it is a Dependency of (ADR 0025): it is Complete, or has
    /// nothing to check.
    pub fn satisfies_dependency(self) -> bool {
        matches!(self, GameStatus::Complete | GameStatus::NothingToCheck)
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

/// A built-in Header Rule: how to recognise the Header a dump of one system may carry before its ROM data, which the
/// DAT's checksums leave out (ADR 0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderRule {
    /// iNES: `NES\x1a`, 16 bytes.
    Nes,
    /// Famicom Disk System: `FDS\x1a`, 16 bytes.
    Fds,
    /// Atari 7800: `ATARI7800` at offset 1, 128 bytes.
    Atari7800,
    /// Atari Lynx: `LYNX`, 64 bytes.
    Lynx,
}

impl HeaderRule {
    pub const ALL: [HeaderRule; 4] = [
        HeaderRule::Nes,
        HeaderRule::Fds,
        HeaderRule::Atari7800,
        HeaderRule::Lynx,
    ];

    /// The most bytes [`HeaderRule::starts`] looks at, for any rule.
    pub const MAX_SIGNATURE_END: usize = 10;

    /// The name the report and the store use.
    pub fn as_str(self) -> &'static str {
        match self {
            HeaderRule::Nes => "nes",
            HeaderRule::Fds => "fds",
            HeaderRule::Atari7800 => "atari7800",
            HeaderRule::Lynx => "lynx",
        }
    }

    pub fn parse(s: &str) -> Option<HeaderRule> {
        HeaderRule::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The rule a DAT names in `<clrmamepro header="…"/>`, by No-Intro's detector file names, ignoring case; none for
    /// a name Title Select does not know.
    pub fn named(detector: &str) -> Option<HeaderRule> {
        let file = detector.rsplit(['/', '\\']).next().unwrap_or(detector);
        match file.to_ascii_lowercase().as_str() {
            "no-intro_nes.xml" => Some(HeaderRule::Nes),
            "no-intro_fds.xml" => Some(HeaderRule::Fds),
            "no-intro_a7800.xml" => Some(HeaderRule::Atari7800),
            "no-intro_lnx.xml" => Some(HeaderRule::Lynx),
            _ => None,
        }
    }

    /// The Header's size in bytes.
    pub fn size(self) -> u64 {
        match self {
            HeaderRule::Nes | HeaderRule::Fds => 16,
            HeaderRule::Atari7800 => 128,
            HeaderRule::Lynx => 64,
        }
    }

    /// The signature a Header starts with, and its offset.
    fn signature(self) -> (usize, &'static [u8]) {
        match self {
            HeaderRule::Nes => (0, b"NES\x1a"),
            HeaderRule::Fds => (0, b"FDS\x1a"),
            HeaderRule::Atari7800 => (1, b"ATARI7800"),
            HeaderRule::Lynx => (0, b"LYNX"),
        }
    }

    /// Whether a payload of `len` bytes that begins with `start` (at least its first
    /// [`HeaderRule::MAX_SIGNATURE_END`] bytes, or all of it if shorter) carries this Header: it has the signature,
    /// and more bytes than the Header.
    pub fn starts(self, start: &[u8], len: u64) -> bool {
        let (offset, signature) = self.signature();
        len > self.size() && start.get(offset..offset + signature.len()) == Some(signature)
    }

    /// Whether emulators generally need this Header to play a game, so that a dump without it may not play.
    pub fn needed_to_play(self) -> bool {
        matches!(self, HeaderRule::Nes | HeaderRule::Atari7800)
    }
}

/// The Header an Audit left out of an Entry's checksums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub bytes: u64,
}

/// The Header Rule a DAT Revision's DAT names: `rule` is absent when Title Select does not know `name`, and then no
/// Header is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedHeaderRule {
    /// The name as the DAT gives it.
    pub name: String,
    pub rule: Option<HeaderRule>,
}

impl NamedHeaderRule {
    pub fn new(name: &str) -> NamedHeaderRule {
        NamedHeaderRule {
            name: name.to_owned(),
            rule: HeaderRule::named(name),
        }
    }
}

/// A note on a Game that is not a Game Status and never stops a Build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameWarning {
    /// A ROM matched without the Header its emulators generally need, so the Game may not play.
    HeaderMissing,
}

impl GameWarning {
    pub const ALL: [GameWarning; 1] = [GameWarning::HeaderMissing];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GameWarning::HeaderMissing => "header_missing",
        }
    }

    pub fn parse(s: &str) -> Option<GameWarning> {
        GameWarning::ALL.into_iter().find(|w| w.as_str() == s)
    }
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
    /// The Header the Audit left out of the checksums it compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Header>,
}

/// The DAT Revision an Audit is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevision {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub name: String,
    pub version: String,
    pub file: String,
    /// The Header Rule its DAT names, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_rule: Option<NamedHeaderRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
    /// How many Games carry each warning; a warning no Game carries is left out.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warnings: BTreeMap<GameWarning, usize>,
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
    /// The ROMs counted present only on Claim Evidence: no Entry's payload was read and matched them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claimed_roms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<GameWarning>,
    /// Its missing ROMs that are Available Elsewhere (ADR 0017), in DAT order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_elsewhere: Vec<AvailableElsewhere>,
}

/// The Games of `games` an Audit found any of their own files of, among `entries`: each with a ROM that an Entry holds
/// and that isn't among its missing ROMs. A ROM only inside another Game's file stays missing, so it doesn't count.
pub fn games_found<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    games: impl IntoIterator<Item = &'a Game>,
) -> HashSet<&'a str> {
    let missing: HashMap<&str, &[String]> = games
        .into_iter()
        .map(|g| (g.name.as_str(), g.missing_roms.as_slice()))
        .collect();
    let mut found = HashSet::new();
    for rom in entries.into_iter().flat_map(|entry| &entry.roms) {
        if let Some((&game, m)) = missing.get_key_value(rom.game.as_str())
            && !m.contains(&rom.name)
        {
            found.insert(game);
        }
    }
    found
}

/// A Game's ROM present only inside other Games' ZIPs or folders: it doesn't count towards the Game's status, but a
/// Repair may fill the Game from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableElsewhere {
    pub rom: String,
    /// The Games whose ZIP or folder holds it, in DAT order.
    pub games: Vec<String>,
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

/// A Game that needs review because a Build proved its placed CHD doesn't hold its disc as the DAT lists it: a
/// current condition of the Game, apart from any Audit's verdict, until an Audit or a return verifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFlag {
    pub game: String,
    /// Its placed file, as an Audit names it.
    pub path: String,
    pub reason: String,
    /// The Build that raised it.
    pub build: i64,
    pub raised_at: String,
}

#[cfg(test)]
mod tests {
    use super::GameStatus;

    #[test]
    fn a_dependency_is_satisfied_when_complete_or_with_nothing_to_check() {
        let satisfying: Vec<GameStatus> = GameStatus::ALL
            .into_iter()
            .filter(|s| s.satisfies_dependency())
            .collect();
        assert_eq!(
            satisfying,
            [GameStatus::Complete, GameStatus::NothingToCheck]
        );
    }
}
