//! Fetching newer DAT Revisions for Following Source Sets from the DAT Providers that permit it (ADR 0012): Redump
//! and MAME. No-Intro is never fetched from; its DATs arrive only as files the Operator drops in.

use serde::{Deserialize, Serialize};

/// A DAT Provider Title Select may fetch from, once the Operator turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchProvider {
    Redump,
    Mame,
}

impl FetchProvider {
    pub const ALL: [FetchProvider; 2] = [FetchProvider::Redump, FetchProvider::Mame];

    pub fn as_str(self) -> &'static str {
        match self {
            FetchProvider::Redump => "redump",
            FetchProvider::Mame => "mame",
        }
    }

    pub fn parse(s: &str) -> Option<FetchProvider> {
        FetchProvider::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// The provider its DATs are imported under, the first half of `<provider>/<name>`.
    pub fn dat_provider(self) -> &'static str {
        match self {
            FetchProvider::Redump => "Redump",
            FetchProvider::Mame => "MAME",
        }
    }
}

/// What one request fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchArtifact {
    /// MAME's latest release, from the GitHub Releases API.
    Release,
    /// A DAT: Redump's DAT of a system, MAME's ListXML or one of its software lists.
    Dat,
    /// Redump's cue pack of a system: a ZIP of its `.cue` sheets.
    Cues,
    /// Redump's SBI pack of a system: a ZIP of its subchannel files.
    Sbi,
}

impl FetchArtifact {
    pub fn as_str(self) -> &'static str {
        match self {
            FetchArtifact::Release => "release",
            FetchArtifact::Dat => "dat",
            FetchArtifact::Cues => "cues",
            FetchArtifact::Sbi => "sbi",
        }
    }

    pub fn parse(s: &str) -> Option<FetchArtifact> {
        [
            FetchArtifact::Release,
            FetchArtifact::Dat,
            FetchArtifact::Cues,
            FetchArtifact::Sbi,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }
}

/// What started a fetch: the Service's daily schedule, or the Operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchTrigger {
    Schedule,
    Manual,
}

impl FetchTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            FetchTrigger::Schedule => "schedule",
            FetchTrigger::Manual => "manual",
        }
    }
}

/// How one request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchOutcome {
    /// It brought something the store did not hold: a new DAT Revision, pack or release.
    Added,
    /// It brought only what the store already held.
    Unchanged,
    /// It failed; the last good revision stays, and the fetch stopped there.
    Failed,
}

impl FetchOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            FetchOutcome::Added => "added",
            FetchOutcome::Unchanged => "unchanged",
            FetchOutcome::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<FetchOutcome> {
        [
            FetchOutcome::Added,
            FetchOutcome::Unchanged,
            FetchOutcome::Failed,
        ]
        .into_iter()
        .find(|o| o.as_str() == s)
    }
}

/// One request to a DAT Provider, recorded before it is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchAttempt {
    pub id: i64,
    pub provider: FetchProvider,
    /// The provider's system: Redump's system code, such as `psx`, or for MAME `mame` (ListXML) or a software list's
    /// name. A release check is of the system `mame`.
    pub system: String,
    pub artifact: FetchArtifact,
    pub trigger: FetchTrigger,
    pub url: String,
    pub started_at: String,
    /// Absent while it runs, or if the Service stopped during it.
    pub finished_at: Option<String>,
    pub outcome: Option<FetchOutcome>,
    pub error: Option<String>,
    /// The DAT Revision a DAT fetch brought, by its SHA-256, with its header version.
    pub dat_revision: Option<String>,
    pub version: Option<String>,
    /// The SHA-256 of the pack a cue or SBI fetch brought, kept in the store.
    pub pack: Option<String>,
    /// For MAME, the release it found or fetched from, such as `mame0262`.
    pub release: Option<String>,
}

/// Where one artifact of a system stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchArtifactStatus {
    pub artifact: FetchArtifact,
    /// The latest request, whatever its outcome.
    pub last_attempt: Option<FetchAttempt>,
    /// The latest request that did not fail: what the store holds now.
    pub last_success: Option<FetchAttempt>,
    /// When the next request may be sent; absent when one may be sent now.
    pub next_at: Option<String>,
}

/// A system of a DAT Provider that a Following Source Set is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchSystem {
    /// The provider's system, as in [`FetchAttempt::system`]; absent when Title Select doesn't know one for the DAT.
    pub system: Option<String>,
    /// The DAT, as `<provider>/<name>`.
    pub dat: String,
    /// The Following Source Sets bound to it.
    pub source_sets: Vec<String>,
    pub artifacts: Vec<FetchArtifactStatus>,
}

/// Where fetching from one DAT Provider stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchProviderStatus {
    pub provider: FetchProvider,
    /// Whether the Operator turned fetching from it on. Off by default.
    pub enabled: bool,
    /// Whether a fetch from it is running now.
    pub running: bool,
    /// MAME's release check; absent for Redump.
    pub release: Option<FetchArtifactStatus>,
    pub systems: Vec<FetchSystem>,
}

/// The body of `GET /fetch`: every fetchable DAT Provider, with its Following systems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchStatus {
    pub providers: Vec<FetchProviderStatus>,
}

/// What one fetch did: its requests, in order, and why it stopped early if it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchReport {
    pub provider: FetchProvider,
    pub attempts: Vec<FetchAttempt>,
    pub stopped: Option<String>,
}
