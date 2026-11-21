//! Fetching newer DAT Revisions for Following Source Sets from the DAT Providers that permit it (ADR 0012): Redump,
//! and MAME's GitHub releases. No-Intro is never fetched from: its DATs arrive only as files the Operator drops in.
//!
//! A fetch is not a Job, and never starts one: a DAT Revision it brings makes the Following Source Sets Stale, as an
//! import does, and the Operator decides when to audit. Each system's artifact is requested at most once a day,
//! whatever started the fetch and however the last request ended; requests to one provider are at least
//! [`MIN_GAP`] apart; and a fetch stops at its first error, keeping the last good revision.
//!
//! A MAME fetch also brings progetto-SNAPS's Support Files (ADR 0012's amendment), from its GitHub mirror at the commit
//! the GitHub API names, once for each newest ListXML DAT Revision a registered Source Set is bound to: after the
//! DATs, under the same courtesy rules.

mod http;

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use title_select_api::fetch::{
    FetchArtifact, FetchArtifactStatus, FetchAttempt, FetchOutcome, FetchProvider,
    FetchProviderStatus, FetchReport, FetchStatus, FetchSupport, FetchSystem, FetchTrigger,
    SUPPORT_SYSTEM,
};
use title_select_api::support::{SupportFileKind, SupportFileOrigin};
use title_select_api::{CuePackOrigin, ImportOutcome};
use title_select_engine::cap::{Clock, Monotonic};
use title_select_engine::dat::Dat;

pub use http::{Allowed, FetchError, Fetched, Http, Method, USER_AGENT, refusal, refusal_unnamed};

use crate::{Error, Result, Store, now};

/// The least time between two requests to one provider, redirects included.
pub const MIN_GAP: Duration = Duration::from_secs(1);
/// How long a system's artifact waits after a request before the next.
const ONCE_A_DAY: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

/// The largest download, and the largest DAT read out of one.
const MAX_DOWNLOAD: u64 = 512 << 20;
const MAX_DAT: u64 = 1 << 30;
/// The largest answer to a release or commit check.
const MAX_RELEASE: u64 = 4 << 20;

/// progetto-SNAPS's GitHub mirror of its Support Files, and the branch whose commit they are fetched at.
const SUPPORT_MIRROR: &str = "AntoPISA/MAME_SupportFiles";
const SUPPORT_BRANCH: &str = "main";

/// Redump's own hosts.
const REDUMP_HOSTS: &[&str] = &["redump.org", "www.redump.org"];
/// GitHub's hosts that MAME's releases are found and fetched from: its API, its web, the raw files software lists
/// and the Support Files are read from, and the hosts GitHub's documentation names for downloading release assets.
const GITHUB_HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "raw.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

/// Where each provider is reached, and the hosts its requests and redirects may address. Only the official endpoints
/// are ever used outside tests: there is no mirror.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub redump: String,
    pub github_api: String,
    pub github_web: String,
    pub github_raw: String,
    redump_allowed: Allowed,
    mame_allowed: Allowed,
}

impl Endpoints {
    pub fn official() -> Endpoints {
        Endpoints {
            redump: "http://redump.org".into(),
            github_api: "https://api.github.com".into(),
            github_web: "https://github.com".into(),
            github_raw: "https://raw.githubusercontent.com".into(),
            redump_allowed: Allowed::hosts(REDUMP_HOSTS),
            mame_allowed: Allowed::hosts(GITHUB_HOSTS),
        }
    }

    /// Every endpoint at `base`, such as a fake provider on a local port, which is the only origin either provider
    /// may then address.
    #[doc(hidden)]
    pub fn at(base: &str) -> Endpoints {
        let base = base.trim_end_matches('/').to_owned();
        Endpoints {
            redump: base.clone(),
            github_api: base.clone(),
            github_web: base.clone(),
            redump_allowed: Allowed::origin(&base),
            mame_allowed: Allowed::origin(&base),
            github_raw: base,
        }
    }

    /// The hosts `provider`'s requests and redirects may address.
    pub fn allowed(&self, provider: FetchProvider) -> &Allowed {
        match provider {
            FetchProvider::Redump => &self.redump_allowed,
            FetchProvider::Mame => &self.mame_allowed,
        }
    }
}

/// Where a [`Fetcher`] reads the time and waits: the real clocks, or a fake in tests, so they needn't wait seconds.
pub trait FetchClock: Clock + Send + Sync {
    /// The time now, which requests are recorded at.
    fn wall(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }
}

impl FetchClock for Monotonic {}

/// The HTTP client and endpoints fetches use, and what paces them: when each provider was last sent a request, and
/// which providers a fetch is running for. One per Service.
pub struct Fetcher {
    http: Http,
    endpoints: Endpoints,
    pace: Arc<dyn FetchClock>,
    /// When each provider was last sent a request, in nanoseconds by `pace`'s clock: before its start, if a request
    /// recorded before then paces it.
    last: Mutex<HashMap<FetchProvider, i128>>,
    running: Mutex<HashSet<FetchProvider>>,
}

impl Fetcher {
    pub fn new(endpoints: Endpoints) -> Fetcher {
        Fetcher::paced(endpoints, Arc::new(Monotonic))
    }

    /// A Fetcher whose requests `pace` spaces, so tests can stand in a fake clock.
    #[doc(hidden)]
    pub fn paced(endpoints: Endpoints, pace: Arc<dyn FetchClock>) -> Fetcher {
        Fetcher {
            http: Http::new(),
            endpoints,
            pace,
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

    /// Paces `provider` from `last`, the time of the latest request to it any Service recorded, unless this fetcher
    /// has sent it one already: its next request waits until [`MIN_GAP`] after `last`, or, with none recorded, for
    /// [`MIN_GAP`].
    fn seed(&self, provider: FetchProvider, last: Option<jiff::Timestamp>) {
        let mut paced = self.last.lock().unwrap();
        paced.entry(provider).or_insert_with(|| {
            let ago = last
                .and_then(|at| Duration::try_from(self.pace.wall().duration_since(at)).ok())
                .unwrap_or_default()
                .min(MIN_GAP);
            i128::from(self.pace.now()) - ago.as_nanos() as i128
        });
    }

    /// Gets `url` from `provider`, at least [`MIN_GAP`] after its last request, and each redirect as long after.
    fn get(
        &self,
        provider: FetchProvider,
        url: &str,
        accept: Option<&str>,
        limit: u64,
    ) -> Result<Fetched, FetchError> {
        let allowed = self.endpoints.allowed(provider);
        // The gap runs from when the last request was answered, so the provider sees at least it between requests.
        self.http
            .get_paced(url, allowed, accept, limit, &mut |answered| {
                let mut last = self.last.lock().unwrap();
                if !answered && let Some(&at) = last.get(&provider) {
                    let since = u64::try_from(i128::from(self.pace.now()) - at).unwrap_or(0);
                    self.pace
                        .sleep(MIN_GAP.saturating_sub(Duration::from_nanos(since)));
                }
                last.insert(provider, i128::from(self.pace.now()));
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
    commit: Option<String>,
    support: Option<String>,
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
     f.outcome, f.error, r.sha256, coalesce(r.version, sf.mame_version), f.pack_sha256, f.release, f.asset_url,
     f.commit_sha, f.support_sha256";
/// The fetches [`ATTEMPT_COLUMNS`] are read from, as `f`.
const ATTEMPTS: &str = "fetches f LEFT JOIN dat_revisions r ON r.id = f.dat_revision_id
     LEFT JOIN support_files sf ON sf.sha256 = f.support_sha256";

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
            commit: r.get(15)?,
            support_file: r.get(16)?,
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
        "SELECT {ATTEMPT_COLUMNS} FROM {ATTEMPTS}
         WHERE f.provider = ?1 AND f.system = ?2 AND f.artifact = ?3 {filter} ORDER BY f.id DESC LIMIT 1"
    );
    Ok(c.query_row(
        &sql,
        params![provider.as_str(), system, artifact.as_str()],
        attempt_from_row,
    )
    .optional()?)
}

/// The release the latest successful fetch of one system's DAT brought, while the DAT Revision it brought is still
/// catalogued: once that revision is deleted, its release no longer counts as fetched (ADR 0003), though the fetch
/// stays in the history and still paces the next request.
fn release_catalogued(
    c: &Connection,
    provider: FetchProvider,
    system: &str,
) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT f.release, r.deleted_at IS NULL FROM fetches f
         LEFT JOIN dat_revisions r ON r.id = f.dat_revision_id
         WHERE f.provider = ?1 AND f.system = ?2 AND f.artifact = ?3 AND f.outcome IN ('added', 'unchanged')
         ORDER BY f.id DESC LIMIT 1",
        params![provider.as_str(), system, FetchArtifact::Dat.as_str()],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<bool>>(1)?)),
    )
    .optional()?
    .and_then(|(release, catalogued)| release.filter(|_| catalogued == Some(true))))
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

/// Records the unfinished fetch `fetch` as bringing the pack `sha256`: while it runs, that holds the pack's file
/// against retention. Run under the writer that keeps the pack, so that no retention comes between.
pub(crate) fn hold_pack(c: &Connection, fetch: i64, sha256: &str) -> Result<()> {
    c.execute(
        "UPDATE fetches SET pack_sha256 = ?2 WHERE id = ?1 AND finished_at IS NULL",
        params![fetch, sha256],
    )?;
    Ok(())
}

/// The condition, on a pack `pack`, that no running fetch holds it.
pub(crate) const NOT_HELD: &str =
    "NOT EXISTS (SELECT 1 FROM fetches h WHERE h.pack_sha256 = {pack} AND h.finished_at IS NULL)";

/// When the latest request to `provider` that any Service recorded was sent, or finished if it did.
fn last_request(c: &Connection, provider: FetchProvider) -> Result<Option<jiff::Timestamp>> {
    let at: Option<String> = c
        .query_row(
            "SELECT coalesce(finished_at, started_at) FROM fetches WHERE provider = ?1
             ORDER BY id DESC LIMIT 1",
            [provider.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(at.and_then(|at| at.parse().ok()))
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

/// The DATs of `provider`, under the store's spelling of it, that a registered Source Set Follows, by header name,
/// with those Source Sets.
fn followed(c: &Connection, provider: FetchProvider) -> Result<Vec<Followed>> {
    let spelling = crate::dats::canonical_provider(c, provider.dat_provider())?;
    let mut stmt = c.prepare(
        "SELECT d.name, s.name FROM source_sets s JOIN dats d ON d.id = s.dat_id
         WHERE s.follow AND s.unregistered_at IS NULL AND d.provider = ?1
         ORDER BY d.name, s.name",
    )?;
    let rows = stmt.query_map([&spelling], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out: Vec<Followed> = Vec::new();
    for row in rows {
        let (dat, set): (String, String) = row?;
        match out.last_mut() {
            Some(last) if last.dat == dat => last.source_sets.push(set),
            _ => out.push(Followed {
                provider: spelling.clone(),
                dat,
                source_sets: vec![set],
            }),
        }
    }
    for f in &mut out {
        f.source_sets.sort();
        f.source_sets.dedup();
    }
    Ok(out)
}

/// A ListXML DAT Revision the Support Files are fetched for.
#[derive(Clone)]
struct ListxmlTarget {
    id: i64,
    sha256: String,
    /// Its import_seq, read with the rest: a revision deleted and imported again takes a new one, which a fetch
    /// chosen for it before mustn't record.
    import_seq: i64,
}

/// The newest revision of each ListXML DAT that a registered Source Set is bound to, Following or not, in import
/// order. MAME's Support Files are fetched for each.
// The DAT Format is what marks a DAT as a ListXML, the same sign the Selection routes on to the arcade selector.
// header_order is numbered within one DAT, so each DAT's newest is found on its own.
fn support_targets(c: &Connection) -> Result<Vec<ListxmlTarget>> {
    let mut stmt = c.prepare(
        "SELECT r.id, r.sha256, r.import_seq FROM dat_revisions r
         WHERE r.id = (SELECT n.id FROM dat_revisions n
                       WHERE n.dat_id = r.dat_id AND n.deleted_at IS NULL AND n.format = 'listxml'
                       ORDER BY n.header_order DESC, n.id DESC LIMIT 1)
           AND EXISTS (SELECT 1 FROM source_sets s WHERE s.dat_id = r.dat_id AND s.unregistered_at IS NULL)
         ORDER BY r.import_seq, r.id",
    )?;
    Ok(stmt
        .query_map([], |r| {
            Ok(ListxmlTarget {
                id: r.get(0)?,
                sha256: r.get(1)?,
                import_seq: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The registered Source Sets bound to a ListXML DAT.
fn listxml_source_sets(c: &Connection) -> Result<Vec<String>> {
    let mut stmt = c.prepare(
        "SELECT s.name FROM source_sets s WHERE s.unregistered_at IS NULL
           AND EXISTS (SELECT 1 FROM dat_revisions r
                       WHERE r.dat_id = s.dat_id AND r.deleted_at IS NULL AND r.format = 'listxml')
         ORDER BY s.name",
    )?;
    Ok(stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The requests that fetch MAME's Support Files, in order: the commit check, then each file.
fn support_artifacts() -> [FetchArtifact; 4] {
    [
        FetchArtifact::SupportCommit,
        FetchArtifact::Catver,
        FetchArtifact::Languages,
        FetchArtifact::Bestgames,
    ]
}

/// Whether every Support File has been fetched since the ListXML DAT Revision imported at `import_seq` was imported:
/// for it, or for a revision imported at or after it, which a fetch is sent for only once it is imported.
// Each fetch keeps the import_seq its revision had when the fetch chose it: a revision deleted and imported again
// takes a new one, which its earlier or in-flight fetches must not take with them.
fn support_fetched_for(c: &Connection, import_seq: i64) -> Result<bool> {
    for kind in SupportFileKind::ALL {
        let fetched: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM fetches f
                            WHERE f.provider = ?1 AND f.system = ?2 AND f.artifact = ?3
                              AND f.outcome IN ('added', 'unchanged')
                              AND f.for_import_seq >= ?4)",
            params![
                FetchProvider::Mame.as_str(),
                SUPPORT_SYSTEM,
                kind.artifact().as_str(),
                import_seq
            ],
            |r| r.get(0),
        )?;
        if !fetched {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The ListXML DAT Revision the Support Files are owed for: the last imported of the targets they haven't all been
/// fetched for, so that a fetch for it serves every other target too.
fn support_owed(c: &Connection) -> Result<Option<ListxmlTarget>> {
    owed_among(c, &support_targets(c)?)
}

/// As [`support_owed`], among `targets`, in import order.
fn owed_among(c: &Connection, targets: &[ListxmlTarget]) -> Result<Option<ListxmlTarget>> {
    let mut owed = None;
    for target in targets {
        if !support_fetched_for(c, target.import_seq)? {
            owed = Some(target.clone());
        }
    }
    Ok(owed)
}

/// When every request of the Support Files may be sent, if not now: the commit check is sent only when every file
/// may follow it.
fn support_next_at(c: &Connection) -> Result<Option<jiff::Timestamp>> {
    let mut latest = None;
    for artifact in support_artifacts() {
        latest = latest.max(next_at(c, FetchProvider::Mame, SUPPORT_SYSTEM, artifact)?);
    }
    Ok(latest)
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
        fetcher: &Fetcher,
        provider: FetchProvider,
        trigger: FetchTrigger,
        actor: &str,
    ) -> Result<FetchReport> {
        let name = provider.dat_provider();
        if !self.settings()?.fetch.enabled(provider) {
            return Err(Error::Refused(format!(
                "fetching from {name} is off; to turn it on, go to Settings, then Operations, and turn on fetching from {name}"
            )));
        }
        let _running = fetcher.begin(provider)?;
        self.close_interrupted(provider)?;
        fetcher.seed(provider, self.read(|c| last_request(c, provider))?);
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
                // A Source Set bound to a ListXML has Support Files to fetch, whether or not anything Follows.
                let listxml = !self.read(support_targets)?.is_empty();
                if systems.is_empty() && !listxml {
                    return refuse_or_nothing(no_followed(name, &followed))
                        .map(|_: Option<()>| report);
                }
                let mut release = None;
                if !systems.is_empty() {
                    release_next =
                        self.read(|c| next_at(c, provider, "mame", FetchArtifact::Release))?;
                }
                if !systems.is_empty() && release_next.is_none() {
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
                        None,
                        |s, _, f| s.check_mame_release(f, &url, web),
                    )?;
                    if let Some(attempt) = attempt {
                        let failed = attempt.outcome == Some(FetchOutcome::Failed);
                        report.attempts.push(attempt);
                        if failed {
                            report.stopped = report.attempts.last().and_then(|a| a.error.clone());
                            return Ok(report);
                        }
                    }
                }
                if let Some((last, asset)) = self
                    .read(|c| latest(c, provider, "mame", FetchArtifact::Release, true))?
                    .filter(|_| !systems.is_empty())
                {
                    release = last.release.zip(asset);
                }
                match release {
                    None => None,
                    Some((tag, asset)) => {
                        let mut steps = Vec::new();
                        for (system, followed) in systems {
                            let current = self.read(|c| {
                                Ok(release_catalogued(c, provider, &system)?.as_deref()
                                    == Some(tag.as_str()))
                            })?;
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
        let mut planned = HashSet::new();
        for step in steps {
            if !planned.insert((step.system.clone(), step.artifact)) {
                continue;
            }
            match self.read(|c| next_at(c, provider, &step.system, step.artifact))? {
                None => due.push(step),
                Some(at) => next = Some(next.map_or(at, |n| n.min(at))),
            }
        }
        let mut support_due = false;
        if provider == FetchProvider::Mame && self.read(support_owed)?.is_some() {
            match self.read(support_next_at)? {
                None => support_due = true,
                Some(at) => next = Some(next.map_or(at, |n| n.min(at))),
            }
        }
        if due.is_empty() && report.attempts.is_empty() && !support_due {
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
                None,
                |s, fetch, f| {
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
                        artifact => s.keep_pack(&step, artifact, fetch, f)?,
                    };
                    brought.release = tag;
                    Ok(brought)
                },
            )?;
            // Another fetch reserved this request first.
            let Some(attempt) = attempt else { continue };
            let failed = attempt.outcome == Some(FetchOutcome::Failed);
            if failed {
                report.stopped = attempt.error.clone();
            }
            report.attempts.push(attempt);
            if failed {
                break;
            }
        }
        if provider == FetchProvider::Mame && report.stopped.is_none() {
            self.fetch_support(fetcher, trigger, actor, &mut report)?;
        }
        Ok(report)
    }

    /// Fetches MAME's Support Files, if they are owed for the newest ListXML DAT Revision and every request is due:
    /// the mirror's commit, then each file at it. Stops at the first request that fails, keeping the last good files.
    fn fetch_support(
        &self,
        fetcher: &Fetcher,
        trigger: FetchTrigger,
        actor: &str,
        report: &mut FetchReport,
    ) -> Result<()> {
        let provider = FetchProvider::Mame;
        // The import_seq comes with the revision, from the query that chose it; each request records both as it is
        // reserved.
        let Some(owed) = self.read(support_owed)? else {
            return Ok(());
        };
        if self.read(support_next_at)?.is_some() {
            return Ok(());
        }
        let url = format!(
            "{}/repos/{SUPPORT_MIRROR}/commits/{SUPPORT_BRANCH}",
            fetcher.endpoints.github_api
        );
        let attempt = self.attempt(
            fetcher,
            provider,
            SUPPORT_SYSTEM,
            FetchArtifact::SupportCommit,
            trigger,
            actor,
            &url,
            Some(&owed),
            |s, _, f| s.check_support_commit(f, &url),
        )?;
        // Not due, or the ListXML it was for can't be shown to still be the revision chosen (see `attempt`).
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let commit = attempt.commit.clone();
        if !push(report, attempt) {
            return Ok(());
        }
        let Some(commit) = commit else {
            return Ok(());
        };
        for kind in SupportFileKind::ALL {
            let file = kind.file_name();
            let url = format!(
                "{}/{SUPPORT_MIRROR}/{commit}/{file}/{file}",
                fetcher.endpoints.github_raw
            );
            let attempt = self.attempt(
                fetcher,
                provider,
                SUPPORT_SYSTEM,
                kind.artifact(),
                trigger,
                actor,
                &url,
                Some(&owed),
                |s, _, f| s.keep_fetched_support(kind, f, &commit),
            )?;
            // Another fetch reserved this request first.
            let Some(attempt) = attempt else { continue };
            if !push(report, attempt) {
                break;
            }
        }
        Ok(())
    }

    /// Reads the commit the Support Files' mirror is at, which the GitHub API answers with alone.
    fn check_support_commit(
        &self,
        fetched: Fetched,
        url: &str,
    ) -> std::result::Result<Brought, String> {
        let commit = String::from_utf8_lossy(&fetched.bytes).trim().to_owned();
        if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "{url} answered with something other than a commit, so no support file was fetched"
            ));
        }
        let previous = self
            .read(|c| {
                latest(
                    c,
                    FetchProvider::Mame,
                    SUPPORT_SYSTEM,
                    FetchArtifact::SupportCommit,
                    true,
                )
            })
            .map_err(|e| e.to_string())?;
        let same = previous.is_some_and(|(a, _)| a.commit.as_deref() == Some(commit.as_str()));
        Ok(Brought {
            commit: Some(commit),
            ..Brought::new(if same {
                FetchOutcome::Unchanged
            } else {
                FetchOutcome::Added
            })
        })
    }

    /// Keeps a fetched Support File, once its header is found to be the file `kind`'s.
    fn keep_fetched_support(
        &self,
        kind: SupportFileKind,
        fetched: Fetched,
        commit: &str,
    ) -> std::result::Result<Brought, String> {
        let file = kind.file_name();
        let (named, _) = title_select_api::support::read_header(&fetched.bytes, &fetched.url)?;
        if named != kind {
            return Err(format!(
                "{} holds {}, not {file}, so it wasn't kept",
                fetched.url,
                named.file_name()
            ));
        }
        let (record, outcome) = self
            .keep_support_file(
                &fetched.bytes,
                file,
                Some(&fetched.url),
                SupportFileOrigin::Fetch,
                Some(commit),
            )
            .map_err(|e| e.to_string())?;
        Ok(Brought {
            commit: Some(commit.to_owned()),
            support: Some(record.sha256),
            ..Brought::new(if outcome == ImportOutcome::Added {
                FetchOutcome::Added
            } else {
                FetchOutcome::Unchanged
            })
        })
    }

    /// Reserves a request by recording it, unless one for the same system's artifact was recorded within a day, sends
    /// it and runs `then` on its body, and records how it ended. Returns `None`, sending nothing, if it was not due:
    /// the check and the record are one transaction, so no two fetches can both send it.
    ///
    /// `for_listxml`, a Support File request's ListXML DAT Revision with the import_seq it was chosen at, is recorded
    /// with the reservation, so that while the request is in flight the revision's row is named by history: deleting
    /// it keeps the row, whose id no later import can take. `None`, sending nothing, if that id can't be shown to still
    /// be that revision: it no longer has that import_seq, and no fetch has named it at that import_seq. It may have
    /// been imported again since it was chosen, or deleted outright and its id taken; either way it is owed again. A
    /// row a fetch has named at that import_seq is still that revision, though imported again since or deleted: the
    /// rest of a fetch sends its files for it.
    // One tiny window remains, accepted: a revision deleted outright before its first reservation, then replaced by
    // an import that takes back both its id (SQLite reuses the highest rowid) and its import_seq (max + 1), passes as
    // the revision chosen. The replacement is then owed nothing until a newer ListXML arrives.
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
        for_listxml: Option<&ListxmlTarget>,
        then: impl FnOnce(&Store, i64, Fetched) -> std::result::Result<Brought, String>,
    ) -> Result<Option<FetchAttempt>> {
        let id = self.write(|tx| {
            if next_at(tx, provider, system, artifact)?.is_some() {
                return Ok(None);
            }
            if let Some(target) = for_listxml {
                let same: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM dat_revisions WHERE id = ?1 AND import_seq = ?2)
                         OR EXISTS (SELECT 1 FROM fetches WHERE for_revision_id = ?1 AND for_import_seq = ?2)",
                    [target.id, target.import_seq],
                    |r| r.get(0),
                )?;
                if !same {
                    return Ok(None);
                }
            }
            let (for_revision, for_import_seq) = for_listxml.map(|t| (t.id, t.import_seq)).unzip();
            tx.execute(
                "INSERT INTO fetches (provider, system, artifact, trigger, actor, url, started_at, for_revision_id,
                                      for_import_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    provider.as_str(),
                    system,
                    artifact.as_str(),
                    trigger.as_str(),
                    actor,
                    url,
                    now(),
                    for_revision,
                    for_import_seq
                ],
            )?;
            Ok(Some(tx.last_insert_rowid()))
        })?;
        let Some(id) = id else { return Ok(None) };
        let (accept, limit) = match artifact {
            FetchArtifact::Release => (Some("application/vnd.github+json"), MAX_RELEASE),
            // The commit's SHA alone, rather than the commit.
            FetchArtifact::SupportCommit => (Some("application/vnd.github.sha"), MAX_RELEASE),
            FetchArtifact::Catver | FetchArtifact::Languages | FetchArtifact::Bestgames => {
                (None, crate::support_files::SUPPORT_FILE_LIMIT)
            }
            _ => (None, MAX_DOWNLOAD),
        };
        let result = fetcher
            .get(provider, url, accept, limit)
            .map_err(|e| e.to_string())
            .and_then(|f| then(self, id, f));
        let (brought, error) = match result {
            Ok(brought) => (brought, None),
            Err(e) => (Brought::new(FetchOutcome::Failed), Some(e)),
        };
        let finished = self.write(|tx| {
            tx.execute(
                "UPDATE fetches SET finished_at = ?2, outcome = ?3, error = ?4, dat_revision_id = ?5,
                 pack_sha256 = ?6, release = ?7, asset_url = ?8, commit_sha = ?9, support_sha256 = ?10
                 WHERE id = ?1",
                params![
                    id,
                    now(),
                    brought.outcome.map(FetchOutcome::as_str),
                    error,
                    brought.dat_revision_id,
                    brought.pack,
                    brought.release,
                    brought.asset_url,
                    brought.commit,
                    brought.support
                ],
            )?;
            if let Some(error) = &error {
                let (provider, system, artifact) = tx.query_row(
                    "SELECT provider, system, artifact FROM fetches WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                crate::observe::note(crate::observe::Observed::FetchFailed {
                    provider,
                    system,
                    artifact,
                    error: error.clone(),
                });
            }
            let sql = format!(
                "SELECT {ATTEMPT_COLUMNS} FROM {ATTEMPTS} WHERE f.id = ?1"
            );
            Ok(Some(tx.query_row(&sql, [id], attempt_from_row)?.0))
        })?;
        match artifact {
            FetchArtifact::Sbi => {
                self.retain_sbi_packs()?;
            }
            FetchArtifact::Cues => {
                self.retain_cue_packs()?;
            }
            _ => {}
        }
        Ok(finished)
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
            .import_parsed_dat(&dat, bytes, file, Some(&step.provider), Some(url))
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
        fetch: i64,
        fetched: Fetched,
    ) -> std::result::Result<Brought, String> {
        let dat = format!("{}/{}", step.provider, step.dat);
        let file = fetched
            .file_name
            .clone()
            .unwrap_or_else(|| format!("{} ({}).zip", step.dat, artifact.as_str()));
        let (sha, outcome) = match artifact {
            FetchArtifact::Cues => self
                .import_cue_pack_held(
                    &fetched.bytes,
                    &file,
                    &dat,
                    Some(&fetched.url),
                    CuePackOrigin::Fetch,
                    Some(fetch),
                )
                .map(|(record, outcome)| (record.sha256, outcome)),
            _ => self.import_sbi_pack_bytes(&fetched.bytes, &file, &dat, &fetched.url, fetch),
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
    pub fn fetch_status(&self, fetcher: &Fetcher) -> Result<FetchStatus> {
        let systems = redump_systems();
        let fetch = self.settings()?.fetch;
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
                        dat: format!("{}/{}", f.provider, f.dat),
                        source_sets: f.source_sets,
                        artifacts,
                    });
                }
                let (release, support) = match provider {
                    FetchProvider::Mame => (
                        Some(artifact_status(
                            c,
                            provider,
                            "mame",
                            FetchArtifact::Release,
                        )?),
                        support_status(c)?,
                    ),
                    FetchProvider::Redump => (None, None),
                };
                providers.push(FetchProviderStatus {
                    provider,
                    enabled: fetch.enabled(provider),
                    running: fetcher.is_running(provider),
                    release,
                    systems: out,
                    support,
                });
            }
            Ok(FetchStatus { providers })
        })
    }
}

/// Where fetching MAME's Support Files stands, while a registered Source Set is bound to a ListXML DAT.
fn support_status(c: &Connection) -> Result<Option<FetchSupport>> {
    let mut targets = support_targets(c)?;
    let owed = owed_among(c, &targets)?;
    let Some(ListxmlTarget {
        sha256: listxml, ..
    }) = owed.clone().or_else(|| targets.pop())
    else {
        return Ok(None);
    };
    let next_at = match owed {
        Some(_) => support_next_at(c)?,
        None => None,
    };
    Ok(Some(FetchSupport {
        source_sets: listxml_source_sets(c)?,
        listxml,
        fetched_for_listxml: owed.is_none(),
        artifacts: support_artifacts()
            .into_iter()
            .map(|a| artifact_status(c, FetchProvider::Mame, SUPPORT_SYSTEM, a))
            .collect::<Result<_>>()?,
        next_at: next_at.map(|t| t.to_string()),
    }))
}

/// Adds `attempt` to `report`, stopping it there if it failed. Returns whether the fetch goes on.
fn push(report: &mut FetchReport, attempt: FetchAttempt) -> bool {
    let failed = attempt.outcome == Some(FetchOutcome::Failed);
    if failed {
        report.stopped = attempt.error.clone();
    }
    report.attempts.push(attempt);
    !failed
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

    fn zip_of(member: &str, bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(member, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Records a request for `artifact` of `psx`, finished as bringing `pack` if `pack` is given.
    fn record(store: &Store, artifact: FetchArtifact, pack: Option<&str>) -> i64 {
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO fetches (provider, system, artifact, trigger, actor, url, started_at, finished_at,
                                          outcome, pack_sha256)
                     VALUES ('redump', 'psx', ?1, 'manual', 'Operator', 'x', ?2, ?3, ?4, ?5)",
                    params![
                        artifact.as_str(),
                        now(),
                        pack.map(|_| now()),
                        pack.map(|_| "added"),
                        pack
                    ],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .unwrap()
    }

    #[test]
    fn a_pack_a_fetch_is_restoring_survives_retention_before_the_fetch_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let dat = br#"<?xml version="1.0"?><datafile><header><name>Sony - PlayStation</name>
            <version>1</version><homepage>redump.org</homepage></header></datafile>"#;
        store.import_dat_bytes(dat, "psx.dat", None).unwrap();
        let keep = |artifact, version: &str, fetch| -> String {
            let dat = "Redump/Sony - PlayStation";
            match artifact {
                FetchArtifact::Cues => {
                    let cue = format!("FILE \"Game.bin\" BINARY {version}");
                    let bytes = zip_of("Game.cue", cue.as_bytes());
                    store
                        .import_cue_pack_held(
                            &bytes,
                            "c.zip",
                            dat,
                            None,
                            CuePackOrigin::Fetch,
                            Some(fetch),
                        )
                        .unwrap()
                        .0
                        .sha256
                }
                _ => {
                    let bytes = zip_of("Game.sbi", version.as_bytes());
                    store
                        .import_sbi_pack_bytes(&bytes, "s.zip", dat, "x", fetch)
                        .unwrap()
                        .0
                }
            }
        };
        for (artifact, packs) in [
            (FetchArtifact::Cues, crate::cue_packs::CUE_PACKS),
            (FetchArtifact::Sbi, crate::sbi_packs::SBI_PACKS),
        ] {
            let first = record(&store, artifact, None);
            let a = keep(artifact, "a", first);
            store.write(|tx| {
                tx.execute(
                    "UPDATE fetches SET finished_at = ?2, outcome = 'added', pack_sha256 = ?3 WHERE id = ?1",
                    params![first, now(), a],
                )?;
                Ok(())
            }).unwrap();
            let second = record(&store, artifact, None);
            let b = keep(artifact, "b", second);
            record(&store, artifact, Some(&b));
            store.retain_cue_packs().unwrap();
            store.retain_sbi_packs().unwrap();
            assert!(
                !dir.path().join(packs).join(&a).exists(),
                "{artifact:?}: a was let go"
            );

            let third = record(&store, artifact, None);
            assert_eq!(keep(artifact, "a", third), a);
            // A retention another import or fetch runs before this one finishes.
            store.retain_cue_packs().unwrap();
            store.retain_sbi_packs().unwrap();
            assert!(
                dir.path().join(packs).join(&a).is_file(),
                "{artifact:?}: a is held"
            );
        }
    }

    #[test]
    fn an_imported_cue_pack_a_fetch_brings_again_is_held_while_that_fetch_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let dat = br#"<?xml version="1.0"?><datafile><header><name>Sony - PlayStation</name>
            <version>1</version><homepage>redump.org</homepage></header></datafile>"#;
        store.import_dat_bytes(dat, "psx.dat", None).unwrap();
        let dat = "Redump/Sony - PlayStation";
        let a = zip_of("A.cue", b"FILE \"A.bin\" BINARY");
        let (imported, _) = store
            .import_cue_pack_bytes(&a, "a.zip", dat, None, CuePackOrigin::Import)
            .unwrap();

        // A fetch brings the same bytes, and before it finishes another pack is imported, which runs retention.
        let fetch = record(&store, FetchArtifact::Cues, None);
        let (again, _) = store
            .import_cue_pack_held(&a, "a.zip", dat, None, CuePackOrigin::Fetch, Some(fetch))
            .unwrap();
        assert_eq!(again.origin, CuePackOrigin::Import);
        let b = zip_of("B.cue", b"FILE \"B.bin\" BINARY");
        store
            .import_cue_pack_bytes(&b, "b.zip", dat, None, CuePackOrigin::Import)
            .unwrap();
        store.retain_cue_packs().unwrap();

        assert!(
            dir.path()
                .join(crate::cue_packs::CUE_PACKS)
                .join(&imported.sha256)
                .is_file()
        );
    }
}
