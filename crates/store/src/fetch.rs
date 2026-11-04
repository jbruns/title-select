//! Fetching newer DAT Revisions for Following Source Sets from the DAT Providers that permit it (ADR 0012): Redump,
//! and MAME's GitHub releases. No-Intro is never fetched from: its DATs arrive only as files the Operator drops in.
//!
//! A fetch is not a Job, and never starts one: a DAT Revision it brings makes the Following Source Sets Stale, as an
//! import does, and the Operator decides when to audit. Each system's artifact is requested at most once a day,
//! whatever started the fetch and however the last request ended; requests to one provider are at least
//! [`MIN_GAP`] apart; and a fetch stops at its first error, keeping the last good revision.

mod http;

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use title_select_api::fetch::{
    FetchArtifact, FetchArtifactStatus, FetchAttempt, FetchOutcome, FetchProvider,
    FetchProviderStatus, FetchReport, FetchStatus, FetchSystem, FetchTrigger,
};
use title_select_api::{CuePackOrigin, ImportOutcome};
use title_select_engine::dat::Dat;

pub use http::{FetchError, Fetched, Http, USER_AGENT, refusal};

use crate::config::Config;
use crate::{Error, Result, Store, now};

/// The least time between two requests to one provider, redirects included.
pub const MIN_GAP: Duration = Duration::from_secs(1);
/// How long a system's artifact waits after a request before the next.
const ONCE_A_DAY: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

/// The largest download, and the largest DAT read out of one.
const MAX_DOWNLOAD: u64 = 512 << 20;
const MAX_DAT: u64 = 1 << 30;
/// The largest answer to a release check.
const MAX_RELEASE: u64 = 4 << 20;

/// Where each provider is reached. Only the official endpoints are ever used outside tests: there is no mirror.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub redump: String,
    pub github_api: String,
    pub github_web: String,
    pub github_raw: String,
}

impl Endpoints {
    pub fn official() -> Endpoints {
        Endpoints {
            redump: "http://redump.org".into(),
            github_api: "https://api.github.com".into(),
            github_web: "https://github.com".into(),
            github_raw: "https://raw.githubusercontent.com".into(),
        }
    }

    /// Every endpoint at `base`, such as a fake provider on a local port.
    #[doc(hidden)]
    pub fn at(base: &str) -> Endpoints {
        let base = base.trim_end_matches('/').to_owned();
        Endpoints {
            redump: base.clone(),
            github_api: base.clone(),
            github_web: base.clone(),
            github_raw: base,
        }
    }
}

/// The HTTP client and endpoints fetches use, and what paces them: when each provider was last sent a request, and
/// which providers a fetch is running for. One per Service.
pub struct Fetcher {
    http: Http,
    endpoints: Endpoints,
    last: Mutex<HashMap<FetchProvider, Instant>>,
    running: Mutex<HashSet<FetchProvider>>,
}

impl Fetcher {
    pub fn new(endpoints: Endpoints) -> Fetcher {
        Fetcher {
            http: Http::new(),
            endpoints,
            last: Mutex::default(),
            running: Mutex::default(),
        }
    }

    pub fn is_running(&self, provider: FetchProvider) -> bool {
        self.running.lock().unwrap().contains(&provider)
    }

    fn begin(&self, provider: FetchProvider) -> Result<Running<'_>> {
        if !self.running.lock().unwrap().insert(provider) {
            return Err(Error::Conflict(format!(
                "a fetch from {} is already running",
                provider.dat_provider()
            )));
        }
        Ok(Running {
            fetcher: self,
            provider,
        })
    }

    /// Gets `url` from `provider`, at least [`MIN_GAP`] after its last request, and each redirect as long after.
    fn get(
        &self,
        provider: FetchProvider,
        url: &str,
        accept: Option<&str>,
        limit: u64,
    ) -> Result<Fetched, FetchError> {
        self.http.get_paced(url, accept, limit, &mut || {
            let mut last = self.last.lock().unwrap();
            if let Some(at) = last.get(&provider) {
                let wait = MIN_GAP.saturating_sub(at.elapsed());
                std::thread::sleep(wait);
            }
            last.insert(provider, Instant::now());
        })
    }
}

/// Marks a provider's fetch running until dropped.
struct Running<'a> {
    fetcher: &'a Fetcher,
    provider: FetchProvider,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.fetcher.running.lock().unwrap().remove(&self.provider);
    }
}

/// A Redump system, as its downloads page lists it, and its DAT's header name, if Title Select knows it.
#[derive(Debug)]
struct RedumpSystem {
    code: &'static str,
    dat: Option<&'static str>,
    datfile: bool,
    cues: bool,
    sbi: bool,
}

fn redump_systems() -> Vec<RedumpSystem> {
    let rows = |tsv: &'static str| {
        tsv.lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split('\t').collect::<Vec<_>>())
    };
    let names: HashMap<&str, &str> = rows(include_str!("fetch/redump_dat_names.tsv"))
        .map(|cols| (cols[0], cols[1]))
        .collect();
    rows(include_str!("fetch/redump_systems.tsv"))
        .map(|cols| RedumpSystem {
            code: cols[0],
            dat: names.get(cols[0]).copied(),
            cues: cols[2] == "y",
            datfile: cols[3] == "y",
            sbi: cols[4] == "y",
        })
        .collect()
}

/// The Redump system whose DAT has the header name `dat`, matched exactly.
fn redump_system<'a>(systems: &'a [RedumpSystem], dat: &str) -> Option<&'a RedumpSystem> {
    systems.iter().find(|s| s.dat == Some(dat))
}

/// The artifacts fetched for a Redump system, in order: only what Redump publishes for it.
fn redump_artifacts(system: &RedumpSystem) -> Vec<FetchArtifact> {
    [
        (system.datfile, FetchArtifact::Dat),
        (system.cues, FetchArtifact::Cues),
        (system.sbi, FetchArtifact::Sbi),
    ]
    .into_iter()
    .filter_map(|(published, artifact)| published.then_some(artifact))
    .collect()
}

/// MAME's system for a DAT it publishes: `mame` for ListXML, or the software list's name.
fn mame_system(dat: &str) -> Option<String> {
    if dat == "MAME" {
        return Some("mame".into());
    }
    let valid = !dat.is_empty()
        && dat
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    (valid && dat != "mame").then(|| dat.to_owned())
}

/// A DAT of a provider that Following Source Sets are bound to.
struct Followed {
    /// The provider as the DAT was imported under, whatever its case.
    provider: String,
    /// The DAT's header name.
    dat: String,
    source_sets: Vec<String>,
}

/// What one request brought, to record with its outcome.
#[derive(Default)]
struct Brought {
    outcome: Option<FetchOutcome>,
    dat_revision_id: Option<i64>,
    pack: Option<String>,
    release: Option<String>,
    asset_url: Option<String>,
}

impl Brought {
    fn new(outcome: FetchOutcome) -> Brought {
        Brought {
            outcome: Some(outcome),
            ..Brought::default()
        }
    }
}

const ATTEMPT_COLUMNS: &str =
    "f.id, f.provider, f.system, f.artifact, f.trigger, f.url, f.started_at, f.finished_at,
     f.outcome, f.error, r.sha256, r.version, f.pack_sha256, f.release, f.asset_url";

fn attempt_from_row(r: &rusqlite::Row) -> rusqlite::Result<(FetchAttempt, Option<String>)> {
    let provider: String = r.get(1)?;
    let artifact: String = r.get(3)?;
    let trigger: String = r.get(4)?;
    let outcome: Option<String> = r.get(8)?;
    Ok((
        FetchAttempt {
            id: r.get(0)?,
            provider: FetchProvider::parse(&provider).unwrap_or(FetchProvider::Redump),
            system: r.get(2)?,
            artifact: FetchArtifact::parse(&artifact).unwrap_or(FetchArtifact::Dat),
            trigger: if trigger == "manual" {
                FetchTrigger::Manual
            } else {
                FetchTrigger::Schedule
            },
            url: r.get(5)?,
            started_at: r.get(6)?,
            finished_at: r.get(7)?,
            outcome: outcome.as_deref().and_then(FetchOutcome::parse),
            error: r.get(9)?,
            dat_revision: r.get(10)?,
            version: r.get(11)?,
            pack: r.get(12)?,
            release: r.get(13)?,
        },
        r.get(14)?,
    ))
}

/// The latest request for one system's artifact, and the latest that did not fail.
fn latest(
    c: &Connection,
    provider: FetchProvider,
    system: &str,
    artifact: FetchArtifact,
    success: bool,
) -> Result<Option<(FetchAttempt, Option<String>)>> {
    let filter = if success {
        "AND f.outcome IN ('added', 'unchanged')"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {ATTEMPT_COLUMNS} FROM fetches f LEFT JOIN dat_revisions r ON r.id = f.dat_revision_id
         WHERE f.provider = ?1 AND f.system = ?2 AND f.artifact = ?3 {filter} ORDER BY f.id DESC LIMIT 1"
    );
    Ok(c.query_row(
        &sql,
        params![provider.as_str(), system, artifact.as_str()],
        attempt_from_row,
    )
    .optional()?)
}

/// When the next request for one system's artifact may be sent, if not now.
fn next_at(
    c: &Connection,
    provider: FetchProvider,
    system: &str,
    artifact: FetchArtifact,
) -> Result<Option<jiff::Timestamp>> {
    let Some((last, _)) = latest(c, provider, system, artifact, false)? else {
        return Ok(None);
    };
    let started: jiff::Timestamp = last
        .started_at
        .parse()
        .map_err(|e| Error::Io(format!("fetch {}: {e}", last.id)))?;
    let next = started + ONCE_A_DAY;
    Ok((next > jiff::Timestamp::now()).then_some(next))
}

fn artifact_status(
    c: &Connection,
    provider: FetchProvider,
    system: &str,
    artifact: FetchArtifact,
) -> Result<FetchArtifactStatus> {
    Ok(FetchArtifactStatus {
        artifact,
        last_attempt: latest(c, provider, system, artifact, false)?.map(|(a, _)| a),
        last_success: latest(c, provider, system, artifact, true)?.map(|(a, _)| a),
        next_at: next_at(c, provider, system, artifact)?.map(|t| t.to_string()),
    })
}

/// The DATs of `provider` that a registered Source Set Follows, by header name, with those Source Sets.
fn followed(c: &Connection, provider: FetchProvider) -> Result<Vec<Followed>> {
    let mut stmt = c.prepare(
        "SELECT d.provider, d.name, s.name FROM source_sets s JOIN dats d ON d.id = s.dat_id
         WHERE s.follow AND s.unregistered_at IS NULL AND d.provider = ?1 COLLATE NOCASE
         ORDER BY d.name, d.provider, s.name",
    )?;
    let rows = stmt.query_map([provider.dat_provider()], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    let mut out: Vec<Followed> = Vec::new();
    for row in rows {
        let (provider, dat, set): (String, String, String) = row?;
        match out.last_mut() {
            Some(last) if last.dat == dat && last.provider == provider => {
                last.source_sets.push(set)
            }
            _ => out.push(Followed {
                provider,
                dat,
                source_sets: vec![set],
            }),
        }
    }
    Ok(out)
}

/// The one member of the ZIP `bytes` whose name ends with `ext`, read whole up to `limit` bytes.
fn zip_member(
    bytes: &[u8],
    ext: &str,
    limit: u64,
) -> std::result::Result<(String, Vec<u8>), String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a ZIP: {e}"))?;
    let matching: Vec<usize> = (0..archive.len())
        .filter(|&i| {
            archive.by_index_raw(i).is_ok_and(|m| {
                m.is_file()
                    && m.name()
                        .is_ok_and(|n| n.to_ascii_lowercase().ends_with(ext))
            })
        })
        .collect();
    let [index] = matching[..] else {
        return Err(format!(
            "the ZIP holds {} {ext} files, not one",
            matching.len()
        ));
    };
    let member = archive.by_index(index).map_err(|e| e.to_string())?;
    let name = member
        .name()
        .map_err(|e| e.to_string())?
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    if member.size() > limit {
        return Err(format!("{name} is larger than {limit} bytes"));
    }
    let mut out = Vec::new();
    member
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("{name}: {e}"))?;
    if out.len() as u64 > limit {
        return Err(format!("{name} is larger than {limit} bytes"));
    }
    Ok((name, out))
}

/// The steps of a fetch: each system's artifact to request, with its URL.
struct Step {
    system: String,
    artifact: FetchArtifact,
    url: String,
    /// The provider the DAT was imported under.
    provider: String,
    /// The header name the DAT must carry, and the packs are for.
    dat: String,
}

impl Store {
    /// Fetches, from `provider`, what is due for every system a registered Source Set Follows: a newer DAT
    /// Revision, and Redump's cue and SBI packs. Refused while `provider` is off in `config`; a manual fetch is also
    /// refused when nothing is due, naming when something will be. Returns each request in order; the fetch stops at
    /// the first that fails.
    pub fn fetch(
        &self,
        config: &Config,
        fetcher: &Fetcher,
        provider: FetchProvider,
        trigger: FetchTrigger,
        actor: &str,
    ) -> Result<FetchReport> {
        let name = provider.dat_provider();
        if !config.fetch.enabled(provider) {
            return Err(Error::Refused(format!(
                "fetching from {name} is off; to turn it on, set `{} = true` under `[fetch]` in your configuration",
                provider.as_str()
            )));
        }
        let _running = fetcher.begin(provider)?;
        self.close_interrupted(provider)?;
        let followed = self.read(|c| followed(c, provider))?;
        let mut report = FetchReport {
            provider,
            attempts: Vec::new(),
            stopped: None,
        };
        let refuse_or_nothing = |why: String| match trigger {
            FetchTrigger::Manual => Err(Error::Refused(why)),
            FetchTrigger::Schedule => Ok(None),
        };
        let mut release_next = None;
        let plan = match provider {
            FetchProvider::Redump => {
                let systems = redump_systems();
                let known: Vec<_> = followed
                    .iter()
                    .filter_map(|f| redump_system(&systems, &f.dat).map(|s| (f, s)))
                    .collect();
                if known.is_empty() {
                    return refuse_or_nothing(no_followed(name, &followed))
                        .map(|_: Option<()>| report);
                }
                let mut steps = Vec::new();
                for (followed, system) in known {
                    for artifact in redump_artifacts(system) {
                        let path = match artifact {
                            FetchArtifact::Dat => "datfile",
                            FetchArtifact::Cues => "cues",
                            _ => "sbi",
                        };
                        steps.push(Step {
                            system: system.code.into(),
                            artifact,
                            url: format!("{}/{path}/{}/", fetcher.endpoints.redump, system.code),
                            provider: followed.provider.clone(),
                            dat: followed.dat.clone(),
                        });
                    }
                }
                Some((steps, None))
            }
            FetchProvider::Mame => {
                let systems: Vec<(String, &Followed)> = followed
                    .iter()
                    .filter_map(|f| mame_system(&f.dat).map(|s| (s, f)))
                    .collect();
                if systems.is_empty() {
                    return refuse_or_nothing(no_followed(name, &followed))
                        .map(|_: Option<()>| report);
                }
                let mut release = None;
                release_next =
                    self.read(|c| next_at(c, provider, "mame", FetchArtifact::Release))?;
                if release_next.is_none() {
                    let url = format!(
                        "{}/repos/mamedev/mame/releases/latest",
                        fetcher.endpoints.github_api
                    );
                    let web = &fetcher.endpoints.github_web;
                    let attempt = self.attempt(
                        fetcher,
                        provider,
                        "mame",
                        FetchArtifact::Release,
                        trigger,
                        actor,
                        &url,
                        |s, f| s.check_mame_release(f, &url, web),
                    )?;
                    let failed = attempt.outcome == Some(FetchOutcome::Failed);
                    report.attempts.push(attempt);
                    if failed {
                        report.stopped = report.attempts.last().and_then(|a| a.error.clone());
                        return Ok(report);
                    }
                }
                if let Some((last, asset)) =
                    self.read(|c| latest(c, provider, "mame", FetchArtifact::Release, true))?
                {
                    release = last.release.zip(asset);
                }
                match release {
                    None => None,
                    Some((tag, asset)) => {
                        let mut steps = Vec::new();
                        for (system, followed) in systems {
                            let current = self
                                .read(|c| latest(c, provider, &system, FetchArtifact::Dat, true))?
                                .is_some_and(|(a, _)| a.release.as_deref() == Some(tag.as_str()));
                            if current {
                                continue;
                            }
                            let url = if system == "mame" {
                                asset.clone()
                            } else {
                                format!(
                                    "{}/mamedev/mame/{tag}/hash/{system}.xml",
                                    fetcher.endpoints.github_raw
                                )
                            };
                            steps.push(Step {
                                system,
                                artifact: FetchArtifact::Dat,
                                url,
                                provider: followed.provider.clone(),
                                dat: followed.dat.clone(),
                            });
                        }
                        Some((steps, Some(tag)))
                    }
                }
            }
        };
        let (steps, tag) = plan.unwrap_or_default();
        let mut due = Vec::new();
        let mut next = release_next;
        for step in steps {
            match self.read(|c| next_at(c, provider, &step.system, step.artifact))? {
                None => due.push(step),
                Some(at) => next = Some(next.map_or(at, |n| n.min(at))),
            }
        }
        if due.is_empty() && report.attempts.is_empty() {
            let when =
                next.map_or_else(String::new, |n| format!("; the next may be sent after {n}"));
            return refuse_or_nothing(format!(
                "nothing is due from {name}: each system is fetched at most once a day{when}"
            ))
            .map(|_: Option<()>| report);
        }
        for step in due {
            // A Source Set unregistered or no longer Following since the fetch began leaves its DAT unfetched.
            let still = self
                .read(|c| self::followed(c, provider))?
                .iter()
                .any(|f| f.provider == step.provider && f.dat == step.dat);
            if !still {
                continue;
            }
            let tag = tag.clone();
            let attempt = self.attempt(
                fetcher,
                provider,
                &step.system,
                step.artifact,
                trigger,
                actor,
                &step.url,
                |s, f| {
                    let mut brought = match step.artifact {
                        FetchArtifact::Dat
                            if provider == FetchProvider::Mame && step.system == "mame" =>
                        {
                            let (file, bytes) = zip_member(&f.bytes, ".xml", MAX_DAT)?;
                            s.import_fetched(&step, &file, &bytes, &f.url, None)?
                        }
                        FetchArtifact::Dat if provider == FetchProvider::Mame => {
                            // A software list's header has no version: it is its release's.
                            let file = format!("{}.xml", step.system);
                            s.import_fetched(&step, &file, &f.bytes, &f.url, tag.as_deref())?
                        }
                        FetchArtifact::Dat => {
                            let (file, bytes) = zip_member(&f.bytes, ".dat", MAX_DAT)?;
                            s.import_fetched(&step, &file, &bytes, &f.url, None)?
                        }
                        artifact => s.keep_pack(&step, artifact, f)?,
                    };
                    brought.release = tag;
                    Ok(brought)
                },
            )?;
            let failed = attempt.outcome == Some(FetchOutcome::Failed);
            if failed {
                report.stopped = attempt.error.clone();
            }
            report.attempts.push(attempt);
            if failed {
                break;
            }
        }
        Ok(report)
    }

    /// Records a request before it is sent, sends it and runs `then` on its body, and records how it ended.
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &self,
        fetcher: &Fetcher,
        provider: FetchProvider,
        system: &str,
        artifact: FetchArtifact,
        trigger: FetchTrigger,
        actor: &str,
        url: &str,
        then: impl FnOnce(&Store, Fetched) -> std::result::Result<Brought, String>,
    ) -> Result<FetchAttempt> {
        let id = self.write(|tx| {
            tx.execute(
                "INSERT INTO fetches (provider, system, artifact, trigger, actor, url, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    provider.as_str(),
                    system,
                    artifact.as_str(),
                    trigger.as_str(),
                    actor,
                    url,
                    now()
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })?;
        let accept = (artifact == FetchArtifact::Release).then_some("application/vnd.github+json");
        let limit = if artifact == FetchArtifact::Release {
            MAX_RELEASE
        } else {
            MAX_DOWNLOAD
        };
        let result = fetcher
            .get(provider, url, accept, limit)
            .map_err(|e| e.to_string())
            .and_then(|f| then(self, f));
        let (brought, error) = match result {
            Ok(brought) => (brought, None),
            Err(e) => (Brought::new(FetchOutcome::Failed), Some(e)),
        };
        self.write(|tx| {
            tx.execute(
                "UPDATE fetches SET finished_at = ?2, outcome = ?3, error = ?4, dat_revision_id = ?5,
                 pack_sha256 = ?6, release = ?7, asset_url = ?8 WHERE id = ?1",
                params![
                    id,
                    now(),
                    brought.outcome.map(FetchOutcome::as_str),
                    error,
                    brought.dat_revision_id,
                    brought.pack,
                    brought.release,
                    brought.asset_url
                ],
            )?;
            let sql = format!(
                "SELECT {ATTEMPT_COLUMNS} FROM fetches f LEFT JOIN dat_revisions r ON r.id = f.dat_revision_id
                 WHERE f.id = ?1"
            );
            Ok(tx.query_row(&sql, [id], attempt_from_row)?.0)
        })
    }

    /// Imports a fetched DAT, once it is found to be a revision of the DAT `expected`.
    fn import_fetched(
        &self,
        step: &Step,
        file: &str,
        bytes: &[u8],
        url: &str,
        version: Option<&str>,
    ) -> std::result::Result<Brought, String> {
        let expected = &step.dat;
        let mut dat = Dat::parse(bytes, Path::new(file)).map_err(|e| e.to_string())?;
        if let Some(version) = version {
            dat.version = version.to_owned();
        }
        if dat.name != *expected {
            return Err(format!(
                "{file} is a DAT of {:?}, not of {expected:?}",
                dat.name
            ));
        }
        let (record, outcome) = self
            .import_parsed_dat(&dat, bytes, file, &step.provider, Some(url))
            .map_err(|e| e.to_string())?;
        let id = self
            .read(|c| {
                Ok(c.query_row(
                    "SELECT id FROM dat_revisions WHERE sha256 = ?1",
                    [&record.sha256],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .map_err(|e| e.to_string())?;
        Ok(Brought {
            dat_revision_id: Some(id),
            ..Brought::new(if outcome == ImportOutcome::Added {
                FetchOutcome::Added
            } else {
                FetchOutcome::Unchanged
            })
        })
    }

    /// Keeps a fetched cue pack in the cue pack store, or an SBI pack in the SBI pack store, for the DAT it is for.
    fn keep_pack(
        &self,
        step: &Step,
        artifact: FetchArtifact,
        fetched: Fetched,
    ) -> std::result::Result<Brought, String> {
        let dat = format!("{}/{}", step.provider, step.dat);
        let file = fetched
            .file_name
            .clone()
            .unwrap_or_else(|| format!("{} ({}).zip", step.dat, artifact.as_str()));
        let (sha, outcome) = match artifact {
            FetchArtifact::Cues => self
                .import_cue_pack_bytes(
                    &fetched.bytes,
                    &file,
                    &dat,
                    Some(&fetched.url),
                    CuePackOrigin::Fetch,
                )
                .map(|(record, outcome)| (record.sha256, outcome)),
            _ => self.import_sbi_pack_bytes(&fetched.bytes, &file, &dat, &fetched.url),
        }
        .map_err(|e| e.to_string())?;
        Ok(Brought {
            pack: Some(sha),
            ..Brought::new(if outcome == ImportOutcome::Added {
                FetchOutcome::Added
            } else {
                FetchOutcome::Unchanged
            })
        })
    }

    /// Reads MAME's latest release: its tag, and the URL of its ListXML ZIP, which must be one of its official
    /// release downloads.
    fn check_mame_release(
        &self,
        fetched: Fetched,
        url: &str,
        web: &str,
    ) -> std::result::Result<Brought, String> {
        #[derive(serde::Deserialize)]
        struct Release {
            tag_name: String,
            assets: Vec<Asset>,
        }
        #[derive(serde::Deserialize)]
        struct Asset {
            name: String,
            browser_download_url: String,
        }
        let release: Release = serde_json::from_slice(&fetched.bytes)
            .map_err(|e| format!("{url}: not a release: {e}"))?;
        let tag = release.tag_name;
        if mame_system(&tag).is_none() {
            return Err(format!("{url}: {tag:?} is not a MAME release tag"));
        }
        let is_listxml = |name: &str| {
            name.strip_prefix("mame")
                .and_then(|n| n.strip_suffix("lx.zip"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        };
        let asset = release
            .assets
            .into_iter()
            .find(|a| is_listxml(&a.name))
            .ok_or_else(|| format!("release {tag} has no ListXML ZIP"))?;
        let official = format!("{web}/mamedev/mame/releases/download/{tag}/");
        if !asset.browser_download_url.starts_with(&official) {
            return Err(format!(
                "release {tag}'s ListXML is at {}, outside its official downloads",
                asset.browser_download_url
            ));
        }
        let previous = self
            .read(|c| latest(c, FetchProvider::Mame, "mame", FetchArtifact::Release, true))
            .map_err(|e| e.to_string())?;
        let same = previous.is_some_and(|(a, _)| a.release.as_deref() == Some(tag.as_str()));
        Ok(Brought {
            release: Some(tag),
            asset_url: Some(asset.browser_download_url),
            ..Brought::new(if same {
                FetchOutcome::Unchanged
            } else {
                FetchOutcome::Added
            })
        })
    }

    /// Records any request a stopped Service left unfinished as failed.
    fn close_interrupted(&self, provider: FetchProvider) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE fetches SET finished_at = ?2, outcome = 'failed',
                 error = 'Title Select stopped before this request finished'
                 WHERE provider = ?1 AND finished_at IS NULL",
                params![provider.as_str(), now()],
            )?;
            Ok(())
        })
    }

    /// Where fetching from each provider stands, for the systems a registered Source Set Follows.
    pub fn fetch_status(&self, config: &Config, fetcher: &Fetcher) -> Result<FetchStatus> {
        let systems = redump_systems();
        self.read(|c| {
            let mut providers = Vec::new();
            for provider in FetchProvider::ALL {
                let mut out = Vec::new();
                for f in followed(c, provider)? {
                    let (system, artifacts) = match provider {
                        FetchProvider::Redump => match redump_system(&systems, &f.dat) {
                            Some(s) => (Some(s.code.to_owned()), redump_artifacts(s)),
                            None => (None, Vec::new()),
                        },
                        FetchProvider::Mame => match mame_system(&f.dat) {
                            Some(s) => (Some(s), vec![FetchArtifact::Dat]),
                            None => (None, Vec::new()),
                        },
                    };
                    let artifacts = match &system {
                        Some(s) => artifacts
                            .into_iter()
                            .map(|a| artifact_status(c, provider, s, a))
                            .collect::<Result<_>>()?,
                        None => Vec::new(),
                    };
                    out.push(FetchSystem {
                        system,
                        dat: format!("{}/{}", provider.dat_provider(), f.dat),
                        source_sets: f.source_sets,
                        artifacts,
                    });
                }
                let release = match provider {
                    FetchProvider::Mame => Some(artifact_status(
                        c,
                        provider,
                        "mame",
                        FetchArtifact::Release,
                    )?),
                    FetchProvider::Redump => None,
                };
                providers.push(FetchProviderStatus {
                    provider,
                    enabled: config.fetch.enabled(provider),
                    running: fetcher.is_running(provider),
                    release,
                    systems: out,
                });
            }
            Ok(FetchStatus { providers })
        })
    }
}

/// Why nothing of `provider` can be fetched: no registered Source Set Follows a DAT of it that Title Select knows.
fn no_followed(provider: &str, followed: &[Followed]) -> String {
    if followed.is_empty() {
        format!(
            "no source set follows a {provider} DAT, so there is nothing to fetch from {provider}"
        )
    } else {
        let dats: Vec<_> = followed.iter().map(|f| f.dat.as_str()).collect();
        format!(
            "Title Select can't fetch the {provider} DATs your source sets follow: {}",
            dats.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_redump_dat_is_a_system_redump_publishes_a_dat_for() {
        let systems = redump_systems();
        let names = include_str!("fetch/redump_dat_names.tsv")
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'));
        for line in names {
            let (code, dat) = line.split_once('\t').unwrap();
            let system = systems.iter().find(|s| s.code == code);
            assert!(
                system.is_some_and(|s| s.datfile && s.dat == Some(dat)),
                "{line}"
            );
        }
        let psx = redump_system(&systems, "Sony - PlayStation").unwrap();
        assert_eq!(
            redump_artifacts(psx),
            [FetchArtifact::Dat, FetchArtifact::Cues, FetchArtifact::Sbi]
        );
        let gc = redump_system(&systems, "Nintendo - GameCube").unwrap();
        assert_eq!(redump_artifacts(gc), [FetchArtifact::Dat]);
        assert!(
            redump_system(&systems, "Sony PlayStation").is_none(),
            "the site's name is not the DAT's"
        );
        let kt = systems.iter().find(|s| s.code == "kt").unwrap();
        assert_eq!(redump_artifacts(kt), []);
    }
}
