//! What notifications and metrics read from the store: each occurrence as its write commits
//! ([`Store::set_observer`]), and the gauges and counters `/metrics` serves ([`Store::metrics`]).
//!
//! A write notes what it changes as it goes ([`note`]). The notes are kept on the writing thread, which holds the
//! writer, and handed to the observer only once the write returns successfully, after the writer is released.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use title_select_api::SourceSetStale;
use title_select_api::build::CheckSummary;

use super::{Result, Store};

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

/// What receives each [`Observed`] once its write commits.
pub(crate) type Observer = Box<dyn Fn(Observed) + Send + Sync>;

thread_local! {
    /// What the write running on this thread has noted, or `None` when no one observes.
    static PENDING: RefCell<Option<Vec<Observed>>> = const { RefCell::new(None) };
}

/// Notes `o` for the write running on this thread, if anyone observes.
pub(crate) fn note(o: Observed) {
    PENDING.with_borrow_mut(|p| {
        if let Some(p) = p {
            p.push(o);
        }
    });
}

fn observing() -> bool {
    PENDING.with_borrow(Option::is_some)
}

/// Starts collecting notes for a write on this thread, when `on`.
pub(crate) fn begin(on: bool) {
    PENDING.set(on.then(Vec::new));
}

/// What the write on this thread noted, ending its collection.
pub(crate) fn end() -> Vec<Observed> {
    PENDING.take().unwrap_or_default()
}

/// The Source Sets Stale when a write began, to note those that it makes Stale.
pub(crate) struct StaleWatch(Option<BTreeSet<String>>);

impl StaleWatch {
    pub(crate) fn start(c: &Connection) -> Result<Self> {
        if !observing() {
            return Ok(Self(None));
        }
        let stale = crate::sets::stale_sets(c)?
            .into_iter()
            .map(|(name, _)| name);
        Ok(Self(Some(stale.collect())))
    }

    /// Notes each Source Set Stale now that was not when the watch started.
    pub(crate) fn finish(self, c: &Connection) -> Result<()> {
        let Some(before) = self.0 else {
            return Ok(());
        };
        for (source_set, reasons) in crate::sets::stale_sets(c)? {
            if !before.contains(&source_set) {
                note(Observed::Stale {
                    source_set,
                    reasons,
                });
            }
        }
        Ok(())
    }
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
    /// Hands `observer` each [`Observed`] as the write that makes it commits, on the writing thread. Set once; later
    /// calls are ignored. It must return quickly and never touch the store.
    pub fn set_observer(&self, observer: impl Fn(Observed) + Send + Sync + 'static) {
        let _ = self.observer.set(Box::new(observer));
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
