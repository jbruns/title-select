//! Ingest Folders: writable Roots where new files arrive. A scan, run as a Job, lists the folder's top-level entries
//! and follows each as an item: Arriving until its size and times stay unchanged for the settle interval, then read
//! once, hashed, and matched against the current DAT Revision of every Source Set. Hashes are reused while the entry
//! is still exactly the file they were read from. Nothing in the folder changes until you approve a plan that lists
//! each item, where it goes and why; the plan binds the whole folder as it was seen, and every write first checks
//! the folder's marker (ADR 0008).

pub(crate) mod moves;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::{FileType, Stat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use title_select_api::build::{Approval, FileStat, MARKER, RETENTION, kept_name};
use title_select_api::ingest::{
    Assessment, FolderEntry, FoundDat, IngestAction, IngestFolder, IngestItem, IngestKeptFile,
    IngestMarker, IngestOutcome, IngestPlan, IngestPlanContent, IngestPlanItem, ItemMatch,
    ItemPayload, ItemState, MatchedRom, Need, PlanIngest, Proposal,
};
use title_select_api::{AuditEvidence, ImportOutcome, Job, JobKind, JobState, Workers};
use title_select_engine::FileStamp;
use title_select_engine::build::{Payload, Probe, Probed, ZipLimits, probe_within};
use title_select_engine::dat::Dat;

use self::moves::{Destination, Move};
use super::audits::choose_revision;
use super::checks::Seen;
use super::folder_dir::{FolderDir, Identity};
use super::folders::{MARKER_LIMIT, store_id, write_marker};
use super::jobs::{finish_running, job_by_id};
use super::kept::{Scope, settle_dir_temps};
use super::plans::{json, to_json};
use super::sets::set_by_name;
use super::{Error, Fault, Result, Store, now};
use crate::config::{Config, Root};

/// Endings of names a download is written under until it finishes.
const PARTIAL: [&str; 4] = [".part", ".!qB", ".crdownload", ".tmp"];

/// Archive formats Title Select never opens.
const UNSUPPORTED_ARCHIVES: [&str; 2] = ["7z", "rar"];

/// The largest file read as a possible DAT.
const DAT_LIMIT: u64 = 512 << 20;

/// Whether a scan passes over `name`: a dotfile, Title Select's own entries among them, or a download not yet
/// finished.
pub(super) fn is_ignored(name: &str) -> bool {
    name.starts_with('.') || PARTIAL.iter().any(|end| name.ends_with(end))
}

fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

pub(super) struct IngestRow {
    pub id: i64,
    pub root: String,
    pub generation: u64,
    pub last_scan_at: Option<String>,
}

const FOLDER_QUERY: &str = "SELECT id, root, generation, last_scan_at FROM ingest_folders";

fn folder_from_row(r: &Row) -> rusqlite::Result<IngestRow> {
    Ok(IngestRow {
        id: r.get(0)?,
        root: r.get(1)?,
        generation: r.get::<_, i64>(2)? as u64,
        last_scan_at: r.get(3)?,
    })
}

pub(super) fn ingest_by_id(c: &Connection, id: i64) -> Result<IngestRow> {
    c.query_row(
        &format!("{FOLDER_QUERY} WHERE id = ?1"),
        [id],
        folder_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no ingest folder {id}")))
}

fn ingest_by_root(c: &Connection, root: &str) -> Result<Option<IngestRow>> {
    Ok(c.query_row(
        &format!("{FOLDER_QUERY} WHERE root = ?1"),
        [root],
        folder_from_row,
    )
    .optional()?)
}

fn registered(c: &Connection, root: &str) -> Result<IngestRow> {
    ingest_by_root(c, root)?.ok_or_else(|| {
        Error::NotFound(format!(
            "ingest folder {root} is not registered; register it first"
        ))
    })
}

/// The Root `root`, refused unless it is an Ingest Folder.
fn ingest_root<'a>(config: &'a Config, root: &str) -> std::result::Result<&'a Root, String> {
    match config.root(root) {
        Some(r) if r.is_ingest() && r.writable => Ok(r),
        Some(_) => Err(format!(
            "root {root} is not an ingest folder; declare it with kind = \"ingest\" and writable = true, then \
             restart the service"
        )),
        None => Err(format!("there is no root {root} in the configuration")),
    }
}

/// Opens the Ingest Folder of Root `root`. Its configured path is followed; nothing in it is.
pub(super) fn open_ingest(config: &Config, root: &str) -> std::result::Result<FolderDir, String> {
    let root_config = ingest_root(config, root)?;
    FolderDir::open(&root_config.path, "").map_err(|e| e.to_string())
}

enum MarkerFound {
    Absent,
    Unreadable(String),
    Present(IngestMarker),
}

fn read_ingest_marker(dir: &FolderDir) -> MarkerFound {
    match dir.read(MARKER, MARKER_LIMIT) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MarkerFound::Absent,
        Err(e) => MarkerFound::Unreadable(e.to_string()),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(marker) => MarkerFound::Present(marker),
            Err(e) => MarkerFound::Unreadable(e.to_string()),
        },
    }
}

/// Checks, through `dir`, that the folder's marker shows it is `expected`; the error says why it does not.
pub(super) fn check_ingest_marker(
    dir: &FolderDir,
    expected: &IngestMarker,
) -> std::result::Result<(), String> {
    let path = dir.path().display();
    match read_ingest_marker(dir) {
        MarkerFound::Present(marker) if marker == *expected => Ok(()),
        MarkerFound::Present(marker) if marker.store != expected.store => Err(format!(
            "{path}: its .title-select marker is from another store; is this the right folder, or the right mount? \
             Nothing in it was changed"
        )),
        MarkerFound::Present(marker) => Err(format!(
            "{path}: its .title-select marker is for ingest folder {}, not {}; is this the right mount? Nothing in \
             it was changed",
            marker.ingest_folder, expected.ingest_folder
        )),
        MarkerFound::Absent => Err(format!(
            "{path}: its .title-select marker is gone; is its share mounted? Nothing in it was changed"
        )),
        MarkerFound::Unreadable(e) => Err(format!(
            "{path}: its .title-select marker cannot be read ({e}); nothing in it was changed"
        )),
    }
}

/// Opens the registered Ingest Folder `row` and checks its marker.
fn open_checked(
    store: &Store,
    config: &Config,
    row: &IngestRow,
) -> Result<std::result::Result<FolderDir, String>> {
    let dir = match open_ingest(config, &row.root) {
        Ok(dir) => dir,
        Err(why) => return Ok(Err(why)),
    };
    let expected = IngestMarker {
        store: store.read(store_id)?,
        ingest_folder: row.id,
    };
    Ok(check_ingest_marker(&dir, &expected).map(|()| dir))
}

/// Every top-level entry of the folder a scan does not pass over, as it is now, by name: identity, size, modification
/// and change times, and link count. A write that puts the size and modification time back still changes the change
/// time, as does a link made to an entry under a name a scan passes over.
// `st_nlink` is narrower than u64 on some platforms.
#[allow(clippy::unnecessary_cast)]
fn fingerprint(dir: &FolderDir) -> std::io::Result<Vec<FolderEntry>> {
    let mut found = Vec::new();
    for (name, _) in dir.entries()? {
        if is_ignored(&name) {
            continue;
        }
        if let Some(st) = dir.stat(&name)? {
            let seen = Seen::of(&st);
            found.push(FolderEntry {
                name,
                identity: seen.identity.into(),
                file: seen.file,
                ctime_ns: seen.ctime_ns,
                nlink: st.st_nlink as u64,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// What a scan read from an item: its hashes and payloads, and, for a DAT file, its header name and version.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ItemRead {
    probe: Probe,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dat: Option<(String, String)>,
}

struct ItemRow {
    item: IngestItem,
    seen: Seen,
    stable_since: String,
    /// Its stat stayed as recorded for the settle interval.
    settled: bool,
    hashed: Option<Seen>,
    read: Option<ItemRead>,
}

const ITEM_QUERY: &str = "SELECT id, name, state, dev, ino, size, mtime_ns, ctime_ns, stable_since, hashed_dev, \
     hashed_ino, hashed_size, hashed_mtime_ns, hashed_ctime_ns, probe, assessment, reason, first_seen_at, assessed_at, \
     settled FROM ingest_items";

fn seen_at(r: &Row, at: usize) -> rusqlite::Result<Option<Seen>> {
    let dev: Option<i64> = r.get(at)?;
    let ino: Option<i64> = r.get(at + 1)?;
    let size: Option<i64> = r.get(at + 2)?;
    let mtime: Option<String> = r.get(at + 3)?;
    let ctime: Option<String> = r.get(at + 4)?;
    let parse = |i: usize, text: &str| {
        text.parse::<i128>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    Ok(match (dev, ino, size, mtime, ctime) {
        (Some(dev), Some(ino), Some(size), Some(mtime), Some(ctime)) => Some(Seen {
            file: FileStat {
                size: size as u64,
                mtime_ns: parse(at + 3, &mtime)?,
            },
            identity: Identity::from_sql(dev, ino),
            ctime_ns: parse(at + 4, &ctime)?,
        }),
        _ => None,
    })
}

fn item_from_row(r: &Row) -> rusqlite::Result<ItemRow> {
    let state: String = r.get(2)?;
    let seen = seen_at(r, 3)?.expect("an item always has its stat");
    let read: Option<String> = r.get(14)?;
    let assessment: Option<String> = r.get(15)?;
    let corrupt = |i: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    };
    let read = read
        .map(|text| serde_json::from_str::<ItemRead>(&text).map_err(|e| corrupt(14, e)))
        .transpose()?;
    let assessment = assessment
        .map(|text| serde_json::from_str::<Assessment>(&text).map_err(|e| corrupt(15, e)))
        .transpose()?
        .unwrap_or_default();
    Ok(ItemRow {
        item: IngestItem {
            id: r.get(0)?,
            name: r.get(1)?,
            state: ItemState::parse(&state).unwrap_or(ItemState::Failed),
            file: seen.file.clone(),
            identity: seen.identity.into(),
            first_seen_at: r.get(17)?,
            assessed_at: r.get(18)?,
            assessment,
            reason: r.get(16)?,
        },
        seen,
        stable_since: r.get(8)?,
        settled: r.get(19)?,
        hashed: seen_at(r, 9)?,
        read,
    })
}

fn item_by_id(c: &Connection, id: i64) -> Result<ItemRow> {
    c.query_row(&format!("{ITEM_QUERY} WHERE id = ?1"), [id], item_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no ingest item {id}")))
}

/// The items still in the folder, by name.
fn present_items(c: &Connection, folder: i64) -> Result<HashMap<String, ItemRow>> {
    let mut stmt = c.prepare(&format!(
        "{ITEM_QUERY} WHERE folder_id = ?1 AND state NOT IN ('gone', 'moved')"
    ))?;
    let rows = stmt
        .query_map([folder], item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(|r| (r.item.name.clone(), r)).collect())
}

/// The change test's view of `seen`: an item's hashes are reused under exactly the test an incremental Audit reuses
/// a payload read under.
fn stamp(seen: &Seen) -> FileStamp {
    FileStamp {
        dev: seen.identity.dev,
        ino: seen.identity.ino,
        size: seen.file.size,
        mtime_ns: seen.file.mtime_ns,
        ctime_ns: seen.ctime_ns,
    }
}

fn stat_params(seen: &Seen) -> (i64, i64, i64, String, String) {
    let (dev, ino) = seen.identity.to_sql();
    (
        dev,
        ino,
        seen.file.size as i64,
        seen.file.mtime_ns.to_string(),
        seen.ctime_ns.to_string(),
    )
}

/// What [`Store::fulfil_ingest_reaudits`] did: the Audit Jobs it queued, and each owed Audit it could not queue,
/// with why.
#[derive(Debug, Default)]
pub struct OwedAudits {
    pub queued: Vec<Job>,
    pub failed: Vec<String>,
}

/// What assessing an item found, to be stored.
struct Found {
    state: ItemState,
    assessment: Assessment,
    reason: Option<String>,
    read: Option<(Seen, ItemRead)>,
}

impl Found {
    fn unsupported(reason: String) -> Found {
        Found {
            state: ItemState::Unsupported,
            assessment: Assessment::default(),
            reason: Some(reason),
            read: None,
        }
    }
}

/// Why an entry is Unsupported from its type and name alone, without opening it.
fn unsupported_at_sight(name: &str, st: &Stat) -> Option<String> {
    match FolderDir::kind(st) {
        FileType::RegularFile => {}
        FileType::Directory => {
            return Some(
                "it is a folder; only loose files and per-game ZIP archives are read".to_owned(),
            );
        }
        _ => return Some("it is not a regular file".to_owned()),
    }
    let ext = extension(name)?;
    UNSUPPORTED_ARCHIVES.contains(&ext.as_str()).then(|| {
        format!(
            "{ext} archives are not opened; only loose files and per-game ZIP archives are read"
        )
    })
}

/// One Source Set's current DAT Revision and what its latest Audit with that revision says of each Game.
struct SetIndex {
    name: String,
    /// The DAT, as `<provider>/<name>`.
    dat: String,
    revision: String,
    /// Index into [`Matcher::dats`].
    index: usize,
    audit: Option<AuditIndex>,
}

struct AuditIndex {
    /// Each Game's status, missing ROMs, and ROMs counted present on Claim Evidence alone.
    games: HashMap<String, (String, Vec<String>, Vec<String>)>,
    /// The Games with a Mismatched or Unreadable Entry.
    bad: HashMap<String, Need>,
}

struct DatIndex {
    dat: Dat,
    /// How many ROMs each Game lists.
    rom_counts: Vec<usize>,
    extensions: HashSet<String>,
}

/// Every Source Set's current DAT Revision, parsed once per scan.
struct Matcher {
    sets: Vec<SetIndex>,
    dats: Vec<DatIndex>,
}

impl SetIndex {
    fn need(&self, game: &str, roms: &[MatchedRom]) -> Need {
        let Some(audit) = &self.audit else {
            return Need::Unaudited;
        };
        match audit.games.get(game) {
            None => Need::Missing,
            Some((status, _, _)) if status == "missing" => Need::Missing,
            Some((status, missing, _))
                if status == "incomplete" && roms.iter().any(|r| missing.contains(&r.rom)) =>
            {
                Need::Missing
            }
            Some((_, _, claimed)) => audit.bad.get(game).copied().unwrap_or(
                match roms.iter().any(|r| claimed.contains(&r.rom)) {
                    true => Need::Claimed,
                    false => Need::Held,
                },
            ),
        }
    }
}

impl Matcher {
    fn load(store: &Store) -> Result<Matcher> {
        let chosen = store.read(|c| {
            let mut stmt = c.prepare("SELECT name FROM source_sets ORDER BY name")?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut chosen = Vec::new();
            for name in names {
                let set = set_by_name(c, &name)?;
                let (revision_id, revision) = choose_revision(c, &set, None)?;
                let audit = audit_index(c, set.id, revision_id)?;
                chosen.push((set.record, revision, audit));
            }
            Ok(chosen)
        })?;
        let mut matcher = Matcher {
            sets: Vec::new(),
            dats: Vec::new(),
        };
        let mut parsed: HashMap<String, usize> = HashMap::new();
        for (set, revision, audit) in chosen {
            if !revision.blob_present {
                continue;
            }
            let index = match parsed.get(&revision.sha256) {
                Some(index) => *index,
                None => {
                    let path = store.blob_path(&revision.sha256);
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    let Ok(dat) = Dat::parse(&bytes, &path) else {
                        continue;
                    };
                    let mut rom_counts = vec![0; dat.games.len()];
                    for rom in &dat.roms {
                        rom_counts[rom.game] += 1;
                    }
                    let extensions = dat.roms.iter().filter_map(|r| extension(&r.name)).collect();
                    matcher.dats.push(DatIndex {
                        dat,
                        rom_counts,
                        extensions,
                    });
                    parsed.insert(revision.sha256.clone(), matcher.dats.len() - 1);
                    matcher.dats.len() - 1
                }
            };
            matcher.sets.push(SetIndex {
                name: set.name,
                dat: set.dat,
                revision: revision.sha256,
                index,
                audit,
            });
        }
        Ok(matcher)
    }

    /// Each Source Set Game the payloads hold ROMs of. In a ZIP, ROMs one Game shares with others match those too;
    /// where one Game accounts for every payload that matches a Source Set, only the Games that do are kept, and a
    /// ZIP no single Game accounts for is a whole-set archive, refused with why.
    fn matches(
        &self,
        payloads: &[Payload],
        zip: bool,
    ) -> std::result::Result<Vec<ItemMatch>, String> {
        let mut found = Vec::new();
        for set in &self.sets {
            let index = &self.dats[set.index];
            let mut games: BTreeMap<usize, (Vec<MatchedRom>, BTreeSet<usize>)> = BTreeMap::new();
            for (at, payload) in payloads.iter().enumerate() {
                let sums = payload.checksums();
                for rom in index.dat.candidates(payload.size, &payload.crc) {
                    let rom = &index.dat.roms[rom];
                    if !sums.matches(rom) {
                        continue;
                    }
                    let (roms, used) = games.entry(rom.game).or_default();
                    if roms.iter().all(|r| r.rom != rom.name) {
                        roms.push(MatchedRom {
                            rom: rom.name.clone(),
                            member: payload.name.clone(),
                        });
                    }
                    used.insert(at);
                }
            }
            if zip && games.len() > 1 {
                let matched: BTreeSet<usize> =
                    games.values().flat_map(|(_, used)| used).copied().collect();
                games.retain(|_, (_, used)| *used == matched);
                if games.is_empty() {
                    return Err(format!(
                        "it holds ROMs of more than one game of {} and no one game accounts for them; whole-set \
                         archives are not opened, only one game's ZIP",
                        set.name
                    ));
                }
            }
            for (game, (roms, used)) in games {
                let name = index.dat.games[game].clone();
                let need = set.need(&name, &roms);
                found.push(ItemMatch {
                    source_set: set.name.clone(),
                    dat_revision: set.revision.clone(),
                    complete: roms.len() == index.rom_counts[game],
                    extra: used.len() < payloads.len(),
                    game: name,
                    roms,
                    need,
                });
            }
        }
        Ok(found)
    }

    /// The one DAT, as `<provider>/<name>`, of a registered Source Set with a ROM ending `.ext`, if exactly one has.
    fn system_for(&self, ext: &str) -> Option<String> {
        let systems: BTreeSet<&str> = self
            .sets
            .iter()
            .filter(|set| self.dats[set.index].extensions.contains(ext))
            .map(|set| set.dat.as_str())
            .collect();
        match systems.len() {
            1 => systems.into_iter().next().map(str::to_owned),
            _ => None,
        }
    }
}

/// What the latest finished Audit of Source Set `set` with DAT Revision `revision` says, if there is one.
fn audit_index(c: &Connection, set: i64, revision: i64) -> Result<Option<AuditIndex>> {
    let audit: Option<i64> = c
        .query_row(
            "SELECT id FROM audits WHERE source_set_id = ?1 AND dat_revision_id = ?2 AND status = 'finished'
             AND pruned_at IS NULL ORDER BY finish_seq DESC LIMIT 1",
            params![set, revision],
            |r| r.get(0),
        )
        .optional()?;
    let Some(audit) = audit else {
        return Ok(None);
    };
    let mut stmt = c.prepare(
        "SELECT name, status, missing_roms, claimed_roms FROM audit_games WHERE audit_id = ?1",
    )?;
    let games = stmt
        .query_map([audit], |r| {
            let missing: Vec<String> = json(r, 2)?;
            let claimed: Vec<String> = json(r, 3)?;
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, String>(1)?, missing, claimed),
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let mut stmt = c.prepare(
        "SELECT named_game, finding FROM audit_entries
         WHERE audit_id = ?1 AND finding IN ('mismatched', 'unreadable') AND named_game IS NOT NULL",
    )?;
    let mut bad = HashMap::new();
    for row in stmt.query_map([audit], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (game, finding) = row?;
        let need = if finding == "mismatched" {
            Need::Mismatched
        } else {
            Need::Unreadable
        };
        bad.entry(game)
            .and_modify(|n: &mut Need| *n = (*n).min(need))
            .or_insert(need);
    }
    Ok(Some(AuditIndex { games, bad }))
}

fn found_dat(c: &Connection, name: &str, version: &str, sha256: &str) -> Result<FoundDat> {
    let mut stmt = c.prepare("SELECT provider FROM dats WHERE name = ?1 ORDER BY provider")?;
    let providers = stmt
        .query_map([name], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let imported = c
        .query_row(
            "SELECT 1 FROM dat_revisions WHERE sha256 = ?1",
            [sha256],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(FoundDat {
        name: name.to_owned(),
        version: version.to_owned(),
        sha256: sha256.to_owned(),
        providers,
        imported,
    })
}

/// Assesses what was read from the item `name`: a DAT file, a ZIP of one Game's ROMs, or a loose file.
fn assess(store: &Store, matcher: &Matcher, name: &str, read: &ItemRead) -> Result<Found> {
    let probe = &read.probe;
    let mut assessment = Assessment {
        sha256: Some(probe.sha256.clone()),
        zip: probe.zip,
        ..Assessment::default()
    };
    let found = |state, assessment, reason: Option<String>| Found {
        state,
        assessment,
        reason,
        read: None,
    };
    if !probe.zip && extension(name).as_deref() == Some("zip") {
        return Ok(found(
            ItemState::Unreadable,
            assessment,
            Some("it is not a readable ZIP archive".into()),
        ));
    }
    if probe.zip
        && let Some(error) = &probe.error
    {
        return Ok(found(
            ItemState::Unreadable,
            assessment,
            Some(format!("a member cannot be read: {error}")),
        ));
    }
    if let Some((dat_name, version)) = &read.dat {
        let dat = store.read(|c| found_dat(c, dat_name, version, &probe.sha256))?;
        let imported = dat.imported;
        assessment.dat = Some(dat);
        return Ok(if imported {
            found(ItemState::AlreadyHeld, assessment, None)
        } else {
            assessment.proposal = Some(Proposal::DatImport);
            found(ItemState::Assessed, assessment, None)
        });
    }
    let matches = match matcher.matches(&probe.payloads, probe.zip) {
        Ok(matches) => matches,
        Err(why) => return Ok(Found::unsupported(why)),
    };
    assessment.payloads = probe
        .payloads
        .iter()
        .map(|p| ItemPayload {
            member: p.name.clone(),
            size: p.size,
            crc: p.crc.clone(),
            md5: p.md5.clone(),
            sha1: p.sha1.clone(),
            sha256: p.sha256.clone(),
        })
        .collect();
    let best = matches
        .iter()
        .min_by(|a, b| (a.need, &a.source_set).cmp(&(b.need, &b.source_set)))
        .cloned();
    assessment.matches = matches;
    let state = match best {
        Some(best) if best.need == Need::Held => ItemState::AlreadyHeld,
        Some(best) => {
            assessment.proposal = Some(Proposal::SourceSet {
                source_set: best.source_set,
                game: best.game,
                need: best.need,
            });
            ItemState::Assessed
        }
        None => {
            let exts: BTreeSet<String> = if probe.zip {
                probe
                    .payloads
                    .iter()
                    .filter_map(|p| p.name.as_deref().and_then(extension))
                    .collect()
            } else {
                extension(name).into_iter().collect()
            };
            let system = match exts.len() {
                1 => exts.first().and_then(|ext| matcher.system_for(ext)),
                _ => None,
            };
            assessment.proposal = Some(Proposal::OperatorAddition { system });
            ItemState::Assessed
        }
    };
    Ok(found(state, assessment, None))
}

/// Reads the regular file `name`, found by `lstat` as `seen`, through `dir`: hashed whole and, for a ZIP, by
/// member; parsed as a DAT when its name says it may be one. Trusted only if it is the same file, unchanged, before
/// and after. A ZIP is judged from its directory before any member is decompressed, and its reading stops at the
/// limits of a game's ZIP; one refused is `Ok(Err(why))`.
fn read_item(
    dir: &FolderDir,
    name: &str,
    seen: &Seen,
) -> std::result::Result<std::result::Result<ItemRead, String>, String> {
    let changed = || "it changed while it was read".to_owned();
    let mut file = dir.open_file(name).map_err(|e| e.to_string())?;
    let before = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&before) != *seen {
        return Err(changed());
    }
    let probe = match probe_within(&file, &ZipLimits::default()).map_err(|e| e.to_string())? {
        Probed::Read(probe) => probe,
        Probed::Refused(why) => {
            let after = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
            return match Seen::of(&after) == *seen {
                true => Ok(Err(why)),
                false => Err(changed()),
            };
        }
    };
    let mut dat = None;
    if !probe.zip
        && probe.size <= DAT_LIMIT
        && matches!(extension(name).as_deref(), Some("dat" | "xml"))
    {
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0))
            .and_then(|_| (&file).take(DAT_LIMIT + 1).read_to_end(&mut bytes))
            .map_err(|e| e.to_string())?;
        if let Ok(parsed) = Dat::parse(&bytes, Path::new(name))
            && !parsed.name.is_empty()
        {
            dat = Some((parsed.name, parsed.version));
        }
    }
    let after = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    match dir.stat(name) {
        Ok(Some(st)) if Seen::of(&after) == *seen && Seen::of(&st) == *seen => {
            Ok(Ok(ItemRead { probe, dat }))
        }
        _ => Err(changed()),
    }
}

fn store_found(tx: &Transaction, id: i64, found: &Found) -> Result<()> {
    let now = now();
    tx.execute(
        "UPDATE ingest_items SET state = ?2, assessment = ?3, reason = ?4, assessed_at = ?5, changed_at = ?5
         WHERE id = ?1",
        params![
            id,
            found.state.as_str(),
            to_json(&found.assessment),
            found.reason,
            now
        ],
    )?;
    if let Some((seen, read)) = &found.read {
        let (dev, ino, size, mtime, ctime) = stat_params(seen);
        tx.execute(
            "UPDATE ingest_items SET hashed_dev = ?2, hashed_ino = ?3, hashed_size = ?4, hashed_mtime_ns = ?5,
                                     hashed_ctime_ns = ?6, probe = ?7
             WHERE id = ?1",
            params![id, dev, ino, size, mtime, ctime, to_json(read)],
        )?;
    }
    Ok(())
}

fn ingest_plan_from_row(r: &Row) -> rusqlite::Result<IngestPlan> {
    let approved_digest: Option<String> = r.get(4)?;
    let approval = match approved_digest {
        Some(digest) => Some(Approval {
            digest,
            actor: r.get(5)?,
            at: r.get(6)?,
        }),
        None => None,
    };
    let outcomes: Option<String> = r.get(8)?;
    Ok(IngestPlan {
        id: r.get(0)?,
        content: json(r, 1)?,
        digest: r.get(2)?,
        created_at: r.get(3)?,
        approval,
        finished_at: r.get(7)?,
        outcomes: match outcomes {
            Some(_) => json(r, 8)?,
            None => Vec::new(),
        },
    })
}

const PLAN_QUERY: &str = "SELECT id, content, digest, created_at, approved_digest, approved_by, approved_at, \
                          finished_at, outcomes FROM ingest_plans";

fn plan_by_id(c: &Connection, id: i64) -> Result<IngestPlan> {
    c.query_row(
        &format!("{PLAN_QUERY} WHERE id = ?1"),
        [id],
        ingest_plan_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no ingest plan {id}")))
}

fn digest(content: &IngestPlanContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a plan always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn kept_from_row(r: &Row) -> rusqlite::Result<IngestKeptFile> {
    Ok(IngestKeptFile {
        id: r.get(0)?,
        generation: r.get::<_, i64>(1)? as u64,
        name: r.get(2)?,
        kept_name: r.get(3)?,
        reason: r.get(4)?,
        plan: r.get(5)?,
        sha256: r.get(6)?,
        file: FileStat {
            size: r.get::<_, i64>(7)? as u64,
            mtime_ns: r.get::<_, String>(8)?.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        },
        kept_at: r.get(9)?,
    })
}

/// What each item of an approved plan came to, from the moves recorded for it.
fn outcomes_from_moves(c: &Connection, plan: &IngestPlan) -> Result<Vec<IngestOutcome>> {
    let mut outcomes = Vec::new();
    for item in &plan.content.items {
        let found: Option<(String, Option<String>, Option<String>)> = c
            .query_row(
                "SELECT state, error, note FROM ingest_moves WHERE plan_id = ?1 AND item_id = ?2
                 ORDER BY id DESC LIMIT 1",
                params![plan.id, item.item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        outcomes.push(match found {
            Some((state, _, note)) if state == "done" => IngestOutcome {
                item: item.item,
                name: item.name.clone(),
                moved: true,
                detail: Some(
                    std::iter::once(format!("moved to {}", item.destination))
                        .chain(note)
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                error: None,
            },
            Some((_, error, _)) => IngestOutcome {
                item: item.item,
                name: item.name.clone(),
                moved: false,
                detail: None,
                error: Some(error.unwrap_or_else(|| "it was not moved".into())),
            },
            None => IngestOutcome {
                item: item.item,
                name: item.name.clone(),
                moved: false,
                detail: None,
                error: Some(
                    "the service stopped before it was moved; it was left as it is. Plan it again"
                        .into(),
                ),
            },
        });
    }
    Ok(outcomes)
}

impl Store {
    /// Every Ingest Folder the configuration declares, with its id once registered.
    pub fn ingest_folders(&self, config: &Config) -> Result<Vec<IngestFolder>> {
        self.read(|c| {
            let mut found = Vec::new();
            for (name, root) in &config.roots {
                if !root.is_ingest() {
                    continue;
                }
                found.push(ingest_record(c, name, root)?);
            }
            Ok(found)
        })
    }

    /// The Ingest Folder of Root `root`.
    pub fn ingest_folder(&self, config: &Config, root: &str) -> Result<IngestFolder> {
        let root_config = ingest_root(config, root).map_err(Error::NotFound)?;
        self.read(|c| ingest_record(c, root, root_config))
    }

    /// Registers the Ingest Folder of Root `root` and writes its marker; every scan and every change then requires
    /// it. Registering again changes nothing. Refused when the folder has a marker that is not this store's for it.
    pub fn register_ingest_folder(&self, config: &Config, root: &str) -> Result<IngestFolder> {
        let dir = open_ingest(config, root).map_err(Error::Refused)?;
        let store = self.read(store_id)?;
        let existing = self.read(|c| ingest_by_root(c, root))?;
        let row = match existing {
            Some(row) => row,
            None => {
                if let MarkerFound::Present(_) | MarkerFound::Unreadable(_) =
                    read_ingest_marker(&dir)
                {
                    return Err(Error::Refused(format!(
                        "{}: the folder already has a .title-select marker that is not this store's ingest \
                         folder; is this the right folder? Nothing in it was changed",
                        dir.path().display()
                    )));
                }
                self.write(|tx| {
                    tx.execute(
                        "INSERT INTO ingest_folders (root, created_at) VALUES (?1, ?2)",
                        params![root, now()],
                    )?;
                    ingest_by_id(tx, tx.last_insert_rowid())
                })?
            }
        };
        let marker = IngestMarker {
            store,
            ingest_folder: row.id,
        };
        match read_ingest_marker(&dir) {
            MarkerFound::Present(found) if found == marker => {}
            // Before anything was moved out, a marker that never got written is written now.
            MarkerFound::Absent if row.generation == 0 => {
                write_marker(&dir, &marker, &mut |_| Ok(()))?;
            }
            _ => {
                return Err(Error::Refused(
                    check_ingest_marker(&dir, &marker)
                        .err()
                        .unwrap_or_else(|| "its marker is not this store's".into()),
                ));
            }
        }
        self.ingest_folder(config, root)
    }

    /// Queues a scan of the Ingest Folder of Root `root`, started by `actor`. Refused while one is queued or
    /// running.
    pub fn enqueue_ingest_scan(&self, root: &str, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let row = registered(tx, root)?;
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE ingest_folder_id = ?1 AND state IN ('queued', 'running')",
                    [row.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "ingest folder {root} already has a scan queued or running: job {job}"
                )));
            }
            tx.execute(
                "INSERT INTO jobs (state, kind, actor, readers, hashers, created_at, ingest_folder_id)
                 VALUES ('queued', 'ingest_scan', ?1, 1, 1, ?2, ?3)",
                params![actor, now(), row.id],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Scans the Ingest Folder of Root `root` now, as a Job started by `actor` and run on this thread. Refused while
    /// a scan is queued or running. Returns the Job as it ends.
    pub fn scan_ingest_folder(&self, config: &Config, root: &str, actor: &str) -> Result<Job> {
        let job = self.enqueue_ingest_scan(root, actor)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE jobs SET state = 'running', started_at = ?2, version = version + 1
                 WHERE id = ?1 AND state = 'queued'",
                params![job.id, now()],
            )?;
            Ok(())
        })?;
        self.run_ingest_scan_job(config, job.id)
    }

    /// Runs the Running scan Job `id`. A folder that cannot be opened, or whose marker is not this store's, fails
    /// the Job and leaves every item as it was. Returns the Job as it ends.
    pub fn run_ingest_scan_job(&self, config: &Config, id: i64) -> Result<Job> {
        let job = self.job(id)?;
        if job.kind != JobKind::IngestScan {
            return Err(Error::Refused(format!("job {id} is not an ingest scan")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let folder: i64 = self.read(|c| {
            Ok(c.query_row(
                "SELECT ingest_folder_id FROM jobs WHERE id = ?1",
                [id],
                |r| r.get(0),
            )?)
        })?;
        let outcome = self.scan_ingest(config, folder);
        self.write(|tx| {
            let (state, error) = match &outcome {
                Ok(()) => (JobState::Succeeded, None),
                Err(err) => (JobState::Failed, Some(err.to_string())),
            };
            match finish_running(tx, id, state, error.as_deref()) {
                Err(Error::Io(_)) | Ok(()) => Ok(()),
                Err(err) => Err(err),
            }
        })?;
        self.job(id)
    }

    fn scan_ingest(&self, config: &Config, folder: i64) -> Result<()> {
        let row = self.read(|c| ingest_by_id(c, folder))?;
        let settle = ingest_root(config, &row.root)
            .map_err(Error::Refused)?
            .settle_secs();
        let dir = open_checked(self, config, &row)?.map_err(Error::Refused)?;
        let entries = dir.entries().map_err(|e| Error::Io(e.to_string()))?;
        let mut items = self.read(|c| present_items(c, folder))?;
        let mut matcher: Option<Matcher> = None;
        let started = jiff::Timestamp::now();
        for (name, _) in entries {
            if is_ignored(&name) {
                continue;
            }
            let Some(st) = dir.stat(&name).map_err(|e| Error::Io(e.to_string()))? else {
                continue;
            };
            let seen = Seen::of(&st);
            let existing = items.remove(&name);
            // Settled once its stat has stayed as recorded for the interval, whatever state it is shown in.
            let settled = match &existing {
                Some(existing) if existing.seen == seen => {
                    existing.settled || {
                        let since: jiff::Timestamp = existing
                            .stable_since
                            .parse()
                            .map_err(|e: jiff::Error| Error::Io(e.to_string()))?;
                        started.duration_since(since).as_secs() >= settle as i64
                    }
                }
                _ => false,
            };
            if let Some(existing) = &existing
                && settled
                && !existing.settled
            {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE ingest_items SET settled = 1 WHERE id = ?1",
                        [existing.item.id],
                    )?;
                    Ok(())
                })?;
            }
            if let Some(reason) = unsupported_at_sight(&name, &st) {
                self.record_seen(
                    folder,
                    &name,
                    existing.as_ref(),
                    &seen,
                    ItemState::Unsupported,
                )?;
                let id = self.read(|c| current_id(c, folder, &name))?;
                self.write(|tx| store_found(tx, id, &Found::unsupported(reason)))?;
                continue;
            }
            let Some(existing) = existing else {
                self.record_seen(folder, &name, None, &seen, ItemState::Arriving)?;
                continue;
            };
            if existing.seen != seen {
                let state = match existing.item.state {
                    ItemState::Arriving => ItemState::Arriving,
                    _ => ItemState::Stale,
                };
                self.record_seen(folder, &name, Some(&existing), &seen, state)?;
                continue;
            }
            match existing.item.state {
                ItemState::Dismissed => continue,
                // Refused unread, as an archive within the archive or past the limits: reading it again would only
                // refuse it again.
                ItemState::Unsupported if existing.read.is_none() => continue,
                _ if !settled => continue,
                _ => {}
            }
            let matcher = match &mut matcher {
                Some(matcher) => matcher,
                None => matcher.insert(Matcher::load(self)?),
            };
            let reused = existing
                .read
                .clone()
                .filter(|_| existing.hashed.as_ref().map(stamp) == Some(stamp(&seen)));
            let (read, fresh) = match reused {
                Some(read) => (read, false),
                None => {
                    self.fault_point(Fault::IngestRead);
                    match read_item(&dir, &name, &seen) {
                        Ok(Ok(read)) => (read, true),
                        Ok(Err(why)) => {
                            self.write(|tx| {
                                store_found(tx, existing.item.id, &Found::unsupported(why))
                            })?;
                            continue;
                        }
                        Err(why) => {
                            let failed = Found {
                                state: ItemState::Failed,
                                assessment: Assessment::default(),
                                reason: Some(why),
                                read: None,
                            };
                            self.write(|tx| store_found(tx, existing.item.id, &failed))?;
                            continue;
                        }
                    }
                }
            };
            let mut found = assess(self, matcher, &name, &read)?;
            if fresh {
                found.read = Some((seen.clone(), read));
            }
            if found.state == existing.item.state
                && found.assessment == existing.item.assessment
                && found.reason == existing.item.reason
                && found.read.is_none()
            {
                continue;
            }
            self.write(|tx| store_found(tx, existing.item.id, &found))?;
        }
        self.write(|tx| {
            let now = now();
            for gone in items.values() {
                tx.execute(
                    "UPDATE ingest_items SET state = 'gone', changed_at = ?2 WHERE id = ?1",
                    params![gone.item.id, now],
                )?;
            }
            tx.execute(
                "UPDATE ingest_folders SET last_scan_at = ?2 WHERE id = ?1",
                params![folder, now],
            )?;
            Ok(())
        })
    }

    /// Records the entry `name` as seen now in `state`: a new item, or `existing` with its new stat. Its stable time
    /// restarts whenever its stat differs from the one recorded.
    fn record_seen(
        &self,
        folder: i64,
        name: &str,
        existing: Option<&ItemRow>,
        seen: &Seen,
        state: ItemState,
    ) -> Result<()> {
        let (dev, ino, size, mtime, ctime) = stat_params(seen);
        let now = now();
        self.write(|tx| {
            match existing {
                None => {
                    tx.execute(
                        "INSERT INTO ingest_items (folder_id, name, state, dev, ino, size, mtime_ns, ctime_ns,
                                                   stable_since, first_seen_at, changed_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)",
                        params![folder, name, state.as_str(), dev, ino, size, mtime, ctime, now],
                    )?;
                }
                Some(existing) => {
                    let unchanged = existing.seen == *seen;
                    let stable_since = if unchanged {
                        existing.stable_since.clone()
                    } else {
                        now.clone()
                    };
                    tx.execute(
                        "UPDATE ingest_items SET state = ?2, dev = ?3, ino = ?4, size = ?5, mtime_ns = ?6,
                                                 ctime_ns = ?7, stable_since = ?8, changed_at = ?9,
                                                 settled = settled AND ?10
                         WHERE id = ?1",
                        params![
                            existing.item.id,
                            state.as_str(),
                            dev,
                            ino,
                            size,
                            mtime,
                            ctime,
                            stable_since,
                            now,
                            unchanged
                        ],
                    )?;
                }
            }
            Ok(())
        })
    }

    /// Every item of the Ingest Folder of Root `root`, those gone or moved out included, by name.
    pub fn ingest_items(&self, root: &str) -> Result<Vec<IngestItem>> {
        self.read(|c| {
            let row = registered(c, root)?;
            let mut stmt = c.prepare(&format!(
                "{ITEM_QUERY} WHERE folder_id = ?1 ORDER BY name, id"
            ))?;
            let items = stmt
                .query_map([row.id], item_from_row)?
                .map(|r| r.map(|r| r.item))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
    }

    pub fn ingest_item(&self, id: i64) -> Result<IngestItem> {
        self.read(|c| item_by_id(c, id).map(|r| r.item))
    }

    /// Dismisses the item `id`: scans leave it be until it changes. Nothing in the folder changes.
    pub fn dismiss_ingest_item(&self, id: i64) -> Result<IngestItem> {
        self.write(|tx| {
            let row = item_by_id(tx, id)?;
            if matches!(row.item.state, ItemState::Gone | ItemState::Moved) {
                return Err(Error::Conflict(format!(
                    "{} is {}; only an item still in the folder can be dismissed",
                    row.item.name,
                    row.item.state.label()
                )));
            }
            tx.execute(
                "UPDATE ingest_items SET state = 'dismissed', changed_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            Ok(item_by_id(tx, id)?.item)
        })
    }

    /// Plans `request` for items of the Ingest Folder of Root `root`. Nothing changes until you approve it; the plan
    /// binds each item, where it goes and why, and every entry of the folder as it is now.
    pub fn plan_ingest(
        &self,
        config: &Config,
        root: &str,
        request: &PlanIngest,
    ) -> Result<IngestPlan> {
        let row = self.read(|c| registered(c, root))?;
        let dir = open_checked(self, config, &row)?.map_err(Error::Refused)?;
        if request.items.is_empty() {
            return Err(Error::Refused("choose at least one item".into()));
        }
        let generation = row.generation + 1;
        let mut items = Vec::new();
        let mut names = HashSet::new();
        for id in &request.items {
            let found = self.read(|c| item_by_id(c, *id))?;
            let in_folder: i64 = self.read(|c| {
                Ok(c.query_row(
                    "SELECT folder_id FROM ingest_items WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?)
            })?;
            let item = &found.item;
            if in_folder != row.id {
                return Err(Error::Refused(format!(
                    "item {id} is not in ingest folder {root}"
                )));
            }
            if let Some(why) = unmovable(&found, request.action == IngestAction::Discard) {
                return Err(Error::Conflict(why));
            }
            if !names.insert(item.name.clone()) {
                return Err(Error::Refused(format!("{} is listed twice", item.name)));
            }
            match dir.stat(&item.name).map_err(|e| Error::Io(e.to_string()))? {
                Some(st)
                    if FolderDir::kind(&st) == FileType::RegularFile
                        && Seen::of(&st) == found.seen
                        && st.st_nlink != 1 =>
                {
                    return Err(Error::Refused(format!(
                        "{} has other links to it, so moving it would not move all of it; remove the other links, \
                         then scan again",
                        item.name
                    )));
                }
                Some(st)
                    if FolderDir::kind(&st) == FileType::RegularFile
                        && Seen::of(&st) == found.seen => {}
                Some(st) if FolderDir::kind(&st) != FileType::RegularFile => {
                    return Err(Error::Refused(format!(
                        "{} is not a file; only files are moved out of an ingest folder",
                        item.name
                    )));
                }
                _ => {
                    return Err(Error::Conflict(format!(
                        "{} changed since the folder was last scanned; scan it again",
                        item.name
                    )));
                }
            }
            let kept = kept_name(&item.name, 1);
            let (reason, provider) = match request.action {
                IngestAction::Discard => ("you discarded it".to_owned(), None),
                IngestAction::DatImport => {
                    let Some(dat) = &item.assessment.dat else {
                        return Err(Error::Refused(format!(
                            "{} is not a DAT file that can be imported",
                            item.name
                        )));
                    };
                    let provider = match (&request.provider, dat.providers.as_slice()) {
                        (Some(provider), _) => provider.clone(),
                        (None, [only]) => only.clone(),
                        (None, []) => {
                            return Err(Error::Refused(format!(
                                "no registered DAT is named {}; choose the provider to import it under",
                                dat.name
                            )));
                        }
                        (None, _) => {
                            return Err(Error::Refused(format!(
                                "more than one provider has a DAT named {}; choose the provider to import it \
                                 under",
                                dat.name
                            )));
                        }
                    };
                    if provider.is_empty() || provider.contains('/') {
                        return Err(Error::Refused(format!(
                            "{provider:?}: a provider is a non-empty name without `/`"
                        )));
                    }
                    (
                        format!(
                            "imported as a DAT revision of {provider}/{} ({}), then kept",
                            dat.name, dat.version
                        ),
                        Some(provider),
                    )
                }
            };
            items.push(IngestPlanItem {
                item: item.id,
                name: item.name.clone(),
                identity: item.identity,
                file: item.file.clone(),
                ctime_ns: found.seen.ctime_ns,
                sha256: item.assessment.sha256.clone(),
                destination: format!("{RETENTION}/{generation}/{kept}"),
                reason,
                provider,
            });
        }
        let content = IngestPlanContent {
            ingest_folder: row.id,
            root: root.to_owned(),
            action: request.action,
            generation,
            items,
            fingerprint: fingerprint(&dir).map_err(|e| Error::Io(e.to_string()))?,
        };
        let digest = digest(&content);
        self.write(|tx| {
            tx.execute(
                "INSERT INTO ingest_plans (folder_id, content, digest, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![row.id, to_json(&content), digest, now()],
            )?;
            plan_by_id(tx, tx.last_insert_rowid())
        })
    }

    pub fn ingest_plan(&self, id: i64) -> Result<IngestPlan> {
        self.read(|c| plan_by_id(c, id))
    }

    /// Every plan made for the Ingest Folder of Root `root`, newest first.
    pub fn ingest_plans(&self, root: &str) -> Result<Vec<IngestPlan>> {
        self.read(|c| {
            let row = registered(c, root)?;
            let mut stmt = c.prepare(&format!(
                "{PLAN_QUERY} WHERE folder_id = ?1 ORDER BY id DESC LIMIT 50"
            ))?;
            Ok(stmt
                .query_map([row.id], ingest_plan_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Approves plan `id`, reviewed as `digest`, as `actor`, and carries it out: each item is moved only while the
    /// folder's marker is this store's and every entry of the folder is still as the plan saw it. A DAT import
    /// imports the file, queues an Audit with the new revision of each Source Set bound to its DAT, then keeps the
    /// file. Returns the plan with what happened to each item.
    pub fn approve_ingest_plan(
        &self,
        config: &Config,
        id: i64,
        digest: &str,
        actor: &str,
        workers: Workers,
    ) -> Result<IngestPlan> {
        let plan = self.ingest_plan(id)?;
        if plan.approval.is_some() {
            return Err(Error::Conflict(format!("plan {id} was already approved")));
        }
        if plan.digest != digest {
            return Err(Error::Conflict(format!(
                "plan {id} is not the plan you reviewed (its digest is {}); review it again",
                plan.digest
            )));
        }
        let content = &plan.content;
        let row = self.read(|c| ingest_by_id(c, content.ingest_folder))?;
        let dir = open_checked(self, config, &row)?.map_err(Error::Refused)?;
        if row.generation + 1 != content.generation {
            return Err(Error::Conflict(format!(
                "another plan changed ingest folder {} since this one was made; plan it again",
                row.root
            )));
        }
        if fingerprint(&dir).map_err(|e| Error::Io(e.to_string()))? != content.fingerprint {
            return Err(Error::Conflict(format!(
                "ingest folder {} changed since the plan was made; nothing was moved. Scan it, plan again and \
                 review the new plan",
                row.root
            )));
        }
        for item in &content.items {
            let keep_aside = content.action == IngestAction::Discard;
            if let Some(why) = self.read(|c| item_unmovable(c, item.item, keep_aside))? {
                return Err(Error::Conflict(format!(
                    "{why}; nothing was moved. Scan the folder, plan again and review the new plan"
                )));
            }
        }
        if let Err(why) = settle_dir_temps(self, Scope::Ingest(row.id), &dir)? {
            return Err(Error::Refused(why));
        }
        self.write(|tx| {
            let advanced = tx.execute(
                "UPDATE ingest_folders SET generation = ?2 WHERE id = ?1 AND generation = ?3",
                params![row.id, content.generation as i64, row.generation as i64],
            )?;
            let approved = tx.execute(
                "UPDATE ingest_plans SET approved_digest = ?2, approved_by = ?3, approved_at = ?4
                 WHERE id = ?1 AND approved_at IS NULL",
                params![id, digest, actor, now()],
            )?;
            if advanced != 1 || approved != 1 {
                return Err(Error::Conflict(format!(
                    "plan {id} was approved, or another plan applied, meanwhile; review it again"
                )));
            }
            Ok(())
        })?;
        let mut outcomes = Vec::new();
        for item in &content.items {
            let mut detail = Vec::new();
            if content.action == IngestAction::DatImport {
                match self.import_from_ingest(&dir, id, item, actor, workers) {
                    Ok(done) => detail.extend(done),
                    Err(err) => {
                        outcomes.push(IngestOutcome {
                            item: item.item,
                            name: item.name.clone(),
                            moved: false,
                            detail: None,
                            error: Some(format!("{err}; it was left as it is")),
                        });
                        continue;
                    }
                }
            }
            let moved = self.move_out(
                config,
                Move {
                    folder: row.id,
                    plan: Some(id),
                    item: Some(item.item),
                    name: &item.name,
                    seen: Seen {
                        identity: item.identity.into(),
                        file: item.file.clone(),
                        ctime_ns: item.ctime_ns,
                    },
                    sha256: item.sha256.clone(),
                    dest: Destination::Kept {
                        generation: content.generation,
                        kept_name: kept_name(&item.name, 1),
                        reason: item.reason.clone(),
                    },
                    repack: None,
                    exact: None,
                },
            )?;
            outcomes.push(match moved {
                Ok(moved) => {
                    detail.push(format!("kept as {}", item.destination));
                    detail.extend(moved.note);
                    IngestOutcome {
                        item: item.item,
                        name: item.name.clone(),
                        moved: true,
                        detail: Some(detail.join("; ")),
                        error: None,
                    }
                }
                Err(why) => IngestOutcome {
                    item: item.item,
                    name: item.name.clone(),
                    moved: false,
                    detail: (!detail.is_empty()).then(|| detail.join("; ")),
                    error: Some(why),
                },
            });
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_plans SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                params![id, now(), to_json(&outcomes)],
            )?;
            plan_by_id(tx, id)
        })
    }

    /// Imports the DAT file `item` of plan `plan`, read through `dir` while it is still the file planned. A new
    /// revision of a DAT already imported owes each Source Set bound to it an incremental Audit with it; that is
    /// recorded before the import and queued as soon as no other Job is, here, once a Job ends, or on recovery.
    /// Returns what it did.
    fn import_from_ingest(
        &self,
        dir: &FolderDir,
        plan: i64,
        item: &IngestPlanItem,
        actor: &str,
        workers: Workers,
    ) -> Result<Vec<String>> {
        let changed = || Error::Conflict(format!("{} changed since the plan was made", item.name));
        let file = dir
            .open_file(&item.name)
            .map_err(|e| Error::Io(e.to_string()))?;
        let planned = Seen {
            identity: item.identity.into(),
            file: item.file.clone(),
            ctime_ns: item.ctime_ns,
        };
        let st = rustix::fs::fstat(&file).map_err(|e| Error::Io(e.to_string()))?;
        if Seen::of(&st) != planned || st.st_nlink != 1 {
            return Err(changed());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(DAT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::Io(e.to_string()))?;
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let after = rustix::fs::fstat(&file).map_err(|e| Error::Io(e.to_string()))?;
        if item.sha256.as_deref() != Some(sha.as_str()) || Seen::of(&after) != planned {
            return Err(changed());
        }
        let provider = item.provider.as_deref().ok_or_else(|| {
            Error::Refused(format!("{} has no provider to import it under", item.name))
        })?;
        let name = Dat::parse(&bytes, Path::new(&item.name))?.name;
        self.write(|tx| {
            let newer: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM dats d JOIN dat_revisions r ON r.dat_id = d.id
                                WHERE d.provider = ?1 AND d.name = ?2)
                 AND NOT EXISTS (SELECT 1 FROM dat_revisions WHERE sha256 = ?3)",
                params![provider, name, sha],
                |r| r.get(0),
            )?;
            if newer {
                tx.execute(
                    "INSERT OR IGNORE INTO ingest_reaudits
                         (plan_id, item_id, source_set_id, sha256, actor, readers, hashers, created_at)
                     SELECT ?1, ?2, s.id, ?3, ?4, ?5, ?6, ?7
                     FROM source_sets s JOIN dats d ON d.id = s.dat_id
                     WHERE d.provider = ?8 AND d.name = ?9",
                    params![
                        plan,
                        item.item,
                        sha,
                        actor,
                        workers.readers as i64,
                        workers.hashers as i64,
                        now(),
                        provider,
                        name
                    ],
                )?;
            }
            Ok(())
        })?;
        self.fault_point(crate::Fault::IngestDatOwed);
        let imported = self.import_dat_bytes(&bytes, &item.name, provider, None);
        if let Err(err) = &imported {
            let why = format!("the import failed: {err}");
            self.write(|tx| {
                tx.execute(
                    "UPDATE ingest_reaudits SET fulfilled_at = ?3, error = ?4
                     WHERE plan_id = ?1 AND item_id = ?2 AND fulfilled_at IS NULL",
                    params![plan, item.item, now(), why],
                )?;
                Ok(())
            })?;
        }
        let (revision, outcome) = imported?;
        let mut done = vec![match outcome {
            ImportOutcome::Added => {
                format!("imported {} revision {}", revision.dat(), revision.version)
            }
            ImportOutcome::AlreadyImported => format!(
                "{} revision {} was already imported",
                revision.dat(),
                revision.version
            ),
            ImportOutcome::Restored => format!(
                "{} revision {} was imported before; its file is kept again",
                revision.dat(),
                revision.version
            ),
        }];
        self.fulfil_ingest_reaudits()?;
        let owed = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT s.name, a.job_id, a.fulfilled_at IS NOT NULL, a.error
                 FROM ingest_reaudits a JOIN source_sets s ON s.id = a.source_set_id
                 WHERE a.plan_id = ?1 AND a.item_id = ?2 ORDER BY s.name",
            )?;
            Ok(stmt
                .query_map(params![plan, item.item], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        for (set, job, ended, error) in owed {
            done.push(match (job, ended, error) {
                (Some(job), _, _) => format!("queued an incremental audit of {set} with it: job {job}"),
                (None, true, error) => format!(
                    "cannot queue an incremental audit of {set} with it: {}",
                    error.unwrap_or_default()
                ),
                (None, false, Some(error)) => format!(
                    "an incremental audit of {set} with it is owed and tried again once a job ends: {error}"
                ),
                (None, false, None) => format!(
                    "an incremental audit of {set} with it is queued as soon as the job running now ends"
                ),
            });
        }
        Ok(done)
    }

    /// Queues each incremental Audit a DAT imported from an Ingest Folder owes, once its revision is imported and
    /// no other Job stands in the way. One whose revision is not imported yet stays owed: its import may still be
    /// under way. One that cannot be queued at all ends with why; one that failed only this time stays owed with
    /// why. Neither stops the others.
    pub fn fulfil_ingest_reaudits(&self) -> Result<OwedAudits> {
        let owed = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT a.id, s.name, a.sha256, a.actor, a.readers, a.hashers
                 FROM ingest_reaudits a JOIN source_sets s ON s.id = a.source_set_id
                 WHERE a.fulfilled_at IS NULL
                   AND EXISTS (SELECT 1 FROM dat_revisions r WHERE r.sha256 = a.sha256)
                 ORDER BY a.id",
            )?;
            Ok(stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        let mut result = OwedAudits::default();
        for (id, set, sha, actor, readers, hashers) in owed {
            let workers = Workers {
                readers: readers as usize,
                hashers: hashers as usize,
            };
            // The Job holds the revision's blob from when it is queued, before the obligation stops holding it.
            let (job, ended, error) = match self.enqueue_audit(
                &set,
                Some(&sha),
                &actor,
                workers,
                AuditEvidence::Incremental,
            ) {
                Ok(job) => (Some(job), true, None),
                Err(Error::JobActive { .. }) => continue,
                Err(err @ (Error::NotFound(_) | Error::Refused(_) | Error::Conflict(_))) => {
                    (None, true, Some(err.to_string()))
                }
                Err(err) => (None, false, Some(err.to_string())),
            };
            if let Some(why) = &error {
                result
                    .failed
                    .push(format!("an incremental audit of {set}: {why}"));
            }
            let recorded = self.write(|tx| {
                tx.execute(
                    "UPDATE ingest_reaudits SET job_id = ?2, fulfilled_at = ?3, error = ?4 WHERE id = ?1",
                    params![id, job.as_ref().map(|j| j.id), ended.then(now), error],
                )?;
                Ok(())
            });
            if let Err(err) = recorded {
                result
                    .failed
                    .push(format!("an incremental audit of {set}: {err}"));
            }
            result.queued.extend(job);
        }
        Ok(result)
    }

    /// Ends, with why, each Audit owed by a DAT import whose plan finished without importing it.
    fn end_unimported_reaudits(&self) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_reaudits SET fulfilled_at = ?1,
                     error = 'the service stopped before the import, and it was not imported'
                 WHERE fulfilled_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM dat_revisions r WHERE r.sha256 = ingest_reaudits.sha256)
                   AND EXISTS (SELECT 1 FROM ingest_plans p
                               WHERE p.id = ingest_reaudits.plan_id AND p.finished_at IS NOT NULL)",
                [now()],
            )?;
            Ok(())
        })
    }

    /// The files plans kept aside in the Ingest Folder of Root `root`.
    pub fn ingest_kept_files(&self, root: &str) -> Result<Vec<IngestKeptFile>> {
        self.read(|c| {
            let row = registered(c, root)?;
            let mut stmt = c.prepare(
                "SELECT id, generation, name, kept_name, reason, plan_id, sha256, size, mtime_ns, kept_at
                 FROM ingest_kept WHERE folder_id = ?1 ORDER BY generation, kept_name",
            )?;
            Ok(stmt
                .query_map([row.id], kept_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Rolls forward every move out of an Ingest Folder a stop interrupted, each only once its folder is open, in a
    /// writable Root, with this store's marker; then finishes each approved plan whose moves have all ended. Returns
    /// each move left unfinished, with why.
    pub fn recover_ingest_moves(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let mut left = Vec::new();
        let folders = self.read(|c| {
            let mut stmt = c.prepare(FOLDER_QUERY)?;
            Ok(stmt
                .query_map([], folder_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        for row in &folders {
            if let Ok(dir) = open_checked(self, config, row)?
                && let Err(why) = settle_dir_temps(self, Scope::Ingest(row.id), &dir)?
            {
                left.push((0, why));
            }
        }
        let unfinished = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM ingest_moves WHERE state NOT IN ('done', 'failed') ORDER BY id",
            )?;
            Ok(stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        for id in unfinished {
            if let Err(why) = self.drive_move(config, id)? {
                left.push((id, why));
            }
        }
        let plans = self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{PLAN_QUERY} WHERE approved_at IS NOT NULL AND finished_at IS NULL
                 AND NOT EXISTS (SELECT 1 FROM ingest_moves m WHERE m.plan_id = ingest_plans.id
                                 AND m.state NOT IN ('done', 'failed'))"
            ))?;
            Ok(stmt
                .query_map([], ingest_plan_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        for plan in plans {
            self.write(|tx| {
                let outcomes = outcomes_from_moves(tx, &plan)?;
                tx.execute(
                    "UPDATE ingest_plans SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                    params![plan.id, now(), to_json(&outcomes)],
                )?;
                Ok(())
            })?;
        }
        // An owed Audit that cannot be queued is reported, never a reason not to start.
        let owed = self
            .end_unimported_reaudits()
            .and_then(|()| self.fulfil_ingest_reaudits());
        match owed {
            Ok(owed) => left.extend(owed.failed.into_iter().map(|why| (0, why))),
            Err(err) => left.push((
                0,
                format!("cannot queue the audits ingest imports owe: {err}"),
            )),
        }
        Ok(left)
    }
}

/// Why the item `row` cannot be moved now, if it cannot. Whatever state it is shown in, it must have settled: its
/// stat stayed as recorded for the folder's settle interval. Then, unless it is only kept aside (`keep_aside`) and is
/// Unsupported or Dismissed, its payloads must have been read and assessed as it is now.
fn unmovable(row: &ItemRow, keep_aside: bool) -> Option<String> {
    let item = &row.item;
    match item.state {
        ItemState::Arriving | ItemState::Stale => {
            return Some(format!(
                "{} is {}; it is moved only once it has stopped changing and been assessed",
                item.name,
                item.state.label()
            ));
        }
        ItemState::Assessed
        | ItemState::AlreadyHeld
        | ItemState::Unreadable
        | ItemState::Unsupported
        | ItemState::Dismissed => {}
        _ => {
            return Some(format!(
                "{} is {}; scan the folder again",
                item.name,
                item.state.label()
            ));
        }
    }
    if !row.settled {
        return Some(format!(
            "{} has not stopped changing for long enough yet; it is moved only once it has. Scan the folder again \
             later",
            item.name
        ));
    }
    let unread = matches!(item.state, ItemState::Unsupported | ItemState::Dismissed);
    let verified = row.read.is_some() && row.hashed.as_ref() == Some(&row.seen);
    if !verified && !(keep_aside && unread) {
        return Some(format!(
            "{} was not read and assessed as it is now; scan the folder again",
            item.name
        ));
    }
    None
}

/// An item of an Ingest Folder, assessed as it is now, that holds a Game of a Source Set whole: what a repair may fill
/// the Game from.
pub(crate) struct FillCandidate {
    pub root: String,
    pub item: IngestItem,
    pub seen: Seen,
    pub matched: ItemMatch,
}

/// Every item that may fill a Game of Source Set `set` as DAT Revision `dat_revision` lists it, in the order the
/// items were first seen.
pub(crate) fn fill_candidates(
    c: &Connection,
    set: &str,
    dat_revision: &str,
) -> Result<Vec<FillCandidate>> {
    let mut stmt = c.prepare(&format!(
        "{ITEM_QUERY} WHERE folder_id = ?1 AND state = 'assessed' ORDER BY id"
    ))?;
    let mut found = Vec::new();
    for folder in c
        .prepare(FOLDER_QUERY)?
        .query_map([], folder_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        for row in stmt
            .query_map([folder.id], item_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            if unmovable(&row, false).is_some() {
                continue;
            }
            for matched in &row.item.assessment.matches {
                if matched.source_set == set
                    && matched.dat_revision == dat_revision
                    && matched.complete
                {
                    found.push(FillCandidate {
                        root: folder.root.clone(),
                        item: row.item.clone(),
                        seen: row.seen.clone(),
                        matched: matched.clone(),
                    });
                }
            }
        }
    }
    Ok(found)
}

/// As [`unmovable`], for the item `id` as recorded now.
pub(super) fn item_unmovable(c: &Connection, id: i64, keep_aside: bool) -> Result<Option<String>> {
    Ok(unmovable(&item_by_id(c, id)?, keep_aside))
}

fn current_id(c: &Connection, folder: i64, name: &str) -> Result<i64> {
    Ok(c.query_row(
        "SELECT id FROM ingest_items WHERE folder_id = ?1 AND name = ?2 AND state NOT IN ('gone', 'moved')",
        params![folder, name],
        |r| r.get(0),
    )?)
}

fn ingest_record(c: &Connection, name: &str, root: &Root) -> Result<IngestFolder> {
    let row = ingest_by_root(c, name)?;
    let mut counts = Vec::new();
    if let Some(row) = &row {
        let mut stmt = c.prepare(
            "SELECT state, count(*) FROM ingest_items WHERE folder_id = ?1 GROUP BY state",
        )?;
        let found = stmt
            .query_map([row.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        for state in ItemState::ALL {
            if let Some(n) = found.get(state.as_str()) {
                counts.push((state, *n as u64));
            }
        }
    }
    Ok(IngestFolder {
        root: name.to_owned(),
        path: root.path.display().to_string(),
        id: row.as_ref().map(|r| r.id),
        settle_secs: root.settle_secs(),
        scan_every_minutes: root.scan_every_minutes,
        last_scan_at: row.and_then(|r| r.last_scan_at),
        counts,
    })
}
