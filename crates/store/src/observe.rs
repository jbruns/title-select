//! What notifications and metrics read from the store: what happened since the last look ([`Store::observe`]), and the
//! gauges and counters `/metrics` serves ([`Store::metrics`]). Both only read.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use title_select_api::SourceSetStale;
use title_select_api::build::CheckSummary;

use super::{Result, Store};

/// How far [`Store::observe`] has looked: each occurrence it reports once moves these past it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watermarks {
    /// The newest DAT Revision seen, by id.
    dat_revision: i64,
    /// Every fetch at or below this id has finished and been seen.
    fetch: i64,
    /// Fetches above `fetch` that have finished and been seen.
    fetches_seen: BTreeSet<i64>,
    /// The Source Sets Stale when last seen, by name.
    stale: BTreeSet<String>,
    /// The drift each Library Folder's latest check found when last seen, by id.
    drift: BTreeMap<i64, u64>,
}

/// One thing that happened, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// A DAT Revision was imported, by upload, drop-in or fetch.
    DatRevision {
        /// The DAT, as `<provider>/<name>`.
        dat: String,
        version: String,
    },
    /// A request to a DAT Provider failed.
    FetchFailed {
        provider: String,
        system: String,
        artifact: String,
        error: String,
    },
    /// A Source Set went Stale.
    Stale {
        source_set: String,
        reasons: Vec<SourceSetStale>,
    },
    /// A check found more drift in a Library Folder than the one before.
    Drift {
        folder: i64,
        root: String,
        path: String,
        drift: u64,
    },
}

/// What `/metrics` serves, read at once.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    /// How many Jobs ended in each final state, by kind and state.
    pub jobs_ended: BTreeMap<(String, String), u64>,
    pub jobs_running: u64,
    pub source_sets: Vec<SourceSetMetrics>,
    /// The latest fetch of each provider's system.
    pub fetches: Vec<FetchMetrics>,
    /// Each checked Library Folder's latest check.
    pub library_folders: Vec<FolderMetrics>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSetMetrics {
    pub name: String,
    pub stale: bool,
    /// When its latest Audit finished, in seconds since the Unix epoch.
    pub last_audit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FetchMetrics {
    pub provider: String,
    pub system: String,
    /// When it finished, in seconds since the Unix epoch.
    pub finished_at: i64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FolderMetrics {
    pub id: i64,
    /// Its Root's name.
    pub root: String,
    pub source_set: String,
    pub drift: u64,
}

/// Seconds since the Unix epoch of a stored timestamp.
fn seconds(at: &str) -> Option<i64> {
    at.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

impl Store {
    /// What has happened since `marks`, which it moves past each: new DAT Revisions, failed fetches, Source Sets newly
    /// Stale and Library Folders whose latest check found more drift than they had. Start from
    /// `Watermarks::default()` and discard the first answer to report only what happens afterwards.
    pub fn observe(&self, marks: &mut Watermarks) -> Result<Vec<Observed>> {
        let mut out = Vec::new();
        let (revisions, fetches, folders) = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT r.id, d.provider || '/' || d.name, r.version FROM dat_revisions r
                 JOIN dats d ON d.id = r.dat_id WHERE r.id > ?1 ORDER BY r.id",
            )?;
            let revisions = stmt
                .query_map([marks.dat_revision], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<(i64, String, String)>>>()?;
            let mut stmt = c.prepare(
                "SELECT id, provider, system, artifact, finished_at IS NOT NULL, outcome, COALESCE(error, '')
                 FROM fetches WHERE id > ?1 ORDER BY id",
            )?;
            let fetches = stmt
                .query_map([marks.fetch], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let folders = latest_checks(c)?;
            Ok((revisions, fetches, folders))
        })?;

        for (id, dat, version) in revisions {
            marks.dat_revision = marks.dat_revision.max(id);
            out.push(Observed::DatRevision { dat, version });
        }

        let mut unfinished = None;
        for (id, provider, system, artifact, finished, outcome, error) in fetches {
            if !finished {
                unfinished.get_or_insert(id);
                continue;
            }
            if !marks.fetches_seen.insert(id) {
                continue;
            }
            if outcome.as_deref() == Some("failed") {
                out.push(Observed::FetchFailed {
                    provider,
                    system,
                    artifact,
                    error,
                });
            }
        }
        // Move the mark up to the first fetch still running, forgetting what lies at or below it.
        let mark = match unfinished {
            Some(first) => first - 1,
            None => marks.fetches_seen.last().copied().unwrap_or(marks.fetch),
        };
        marks.fetch = marks.fetch.max(mark);
        let below = marks.fetch;
        marks.fetches_seen.retain(|id| *id > below);

        let mut stale = BTreeSet::new();
        for set in self.source_sets()? {
            if set.stale.is_empty() {
                continue;
            }
            if !marks.stale.contains(&set.name) {
                out.push(Observed::Stale {
                    source_set: set.name.clone(),
                    reasons: set.stale,
                });
            }
            stale.insert(set.name);
        }
        marks.stale = stale;

        let mut drift = BTreeMap::new();
        for (folder, root, path, _, summary) in folders {
            let found = summary.drift;
            if found > marks.drift.get(&folder).copied().unwrap_or(0) {
                out.push(Observed::Drift {
                    folder,
                    root,
                    path,
                    drift: found,
                });
            }
            drift.insert(folder, found);
        }
        marks.drift = drift;
        Ok(out)
    }

    /// What `/metrics` serves: Job counts, and each Source Set's, fetch's and Library Folder's latest state.
    pub fn metrics(&self) -> Result<Metrics> {
        let mut metrics = self.read(|c| {
            let mut metrics = Metrics::default();
            let mut stmt = c.prepare("SELECT kind, state, COUNT(*) FROM jobs GROUP BY kind, state")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (kind, state, count) = row?;
                match state.as_str() {
                    "running" => metrics.jobs_running += count as u64,
                    "queued" => {}
                    _ => {
                        metrics.jobs_ended.insert((kind, state), count as u64);
                    }
                }
            }
            let mut stmt = c.prepare(
                "SELECT provider, system, finished_at, outcome FROM fetches
                 WHERE id IN (SELECT MAX(id) FROM fetches WHERE finished_at IS NOT NULL GROUP BY provider, system)
                 ORDER BY provider, system",
            )?;
            let rows = stmt.query_map(params![], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (provider, system, finished_at, outcome) = row?;
                let Some(finished_at) = seconds(&finished_at) else {
                    continue;
                };
                metrics.fetches.push(FetchMetrics {
                    provider,
                    system,
                    finished_at,
                    outcome: outcome.unwrap_or_else(|| "failed".into()),
                });
            }
            metrics.library_folders = latest_checks(c)?
                .into_iter()
                .map(|(id, root, _, source_set, summary)| FolderMetrics {
                    id,
                    root,
                    source_set,
                    drift: summary.drift,
                })
                .collect();
            Ok(metrics)
        })?;
        metrics.source_sets = self
            .source_sets()?
            .into_iter()
            .map(|set| SourceSetMetrics {
                stale: !set.stale.is_empty(),
                last_audit: set
                    .last_verified_against
                    .and_then(|l| seconds(&l.finished_at)),
                name: set.name,
            })
            .collect();
        Ok(metrics)
    }
}

/// Each registered Library Folder that was checked, with its root, path, Source Set and latest check's summary.
#[allow(clippy::type_complexity)]
fn latest_checks(
    c: &rusqlite::Connection,
) -> Result<Vec<(i64, String, String, String, CheckSummary)>> {
    let mut stmt = c.prepare(
        "SELECT f.id, f.root, f.path, s.name,
                (SELECT summary FROM folder_checks WHERE folder_id = f.id ORDER BY id DESC LIMIT 1)
         FROM library_folders f JOIN source_sets s ON s.id = f.source_set_id
         WHERE f.unregistered_at IS NULL ORDER BY f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, root, path, set, summary) = row?;
        let Some(summary) = summary.and_then(|s| serde_json::from_str(&s).ok()) else {
            continue;
        };
        out.push((id, root, path, set, summary));
    }
    Ok(out)
}
