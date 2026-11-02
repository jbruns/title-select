//! Change Plans: what building a Selection into a Managed Folder would do, computed from the store and a stat of the
//! folder and of the sources, and stored with the SHA-256 of its canonical JSON, which an approval binds to.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::Metadata;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};
use title_select_api::build::{
    Approval, Blocker, BlockerKind, Cause, CheckSummary, ExpectedRom, FileIdentity, FileOutcome,
    FileStat, FolderCheck, ItemKind, KeepAside, KeepReason, Plan, PlanBuild, PlanChange,
    PlanChoices, PlanContent, PlanItem, PlanRemoval, PlanSource, RETENTION, RemovalKind, STAGING,
    invalid_name,
};
use title_select_api::report::{Entry, EvidenceLevel, Finding};
use title_select_api::selection::{ReleaseRow, Role, Selection, diff_rows};
use title_select_api::storage::{Storage, StorageScope, target_of as storage_target};
use title_select_api::{DatRevisionRecord, Job, Workers};
use title_select_engine::build::{Probe, holds_as, is_torrentzip};
use title_select_engine::dat::Dat;

use super::audits::{ENTRY_COLUMNS, entry_from_row};
use super::checks::{Ack, Scan, ScanInput, Seen, read_acks, read_probes, scan, store_check};
use super::dats::revision_by_id;
use super::folder_dir::{FolderDir, Identity, file_stat};
use super::folders::{
    FolderRow, MARKER_NEW, folder_by_id, is_own, marker_problem, open_folder, store_id,
};
use super::jobs::{build_hold, job_by_id};
use super::kept::{deletion_active, folder_dirs, is_dir_temp};
use super::selections::{read_rows, read_selection, record_by_id};
use super::sets::{resolve, set_by_id};
use super::{Error, Result, Store, now};

mod cleanup;
use crate::config::Config;

/// A file a Build wrote into a Managed Folder, as the store recorded it.
#[derive(Debug, Clone)]
pub(super) struct Built {
    pub game: String,
    pub roms: Vec<ExpectedRom>,
    pub file: FileStat,
    /// The Build that recorded it.
    pub build: i64,
    pub sha256: Option<String>,
    pub torrentzip: bool,
    /// Whether it was built loose, its one ROM under its DAT name.
    pub loose: bool,
}

fn read_originals(c: &Connection, folder: i64) -> Result<HashSet<(String, String, Identity)>> {
    let mut stmt = c.prepare(
        "SELECT target, converts_to, dev, ino FROM conversion_originals WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            Identity::from_sql(r.get(2)?, r.get(3)?),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Whether `a` and `b` list the same ROMs: the same names, sizes and every hash. Only a file of the same ROMs in the
/// other format is a conversion; anything else replaces it.
fn same_roms(a: &[ExpectedRom], b: &[ExpectedRom]) -> bool {
    let sorted = |roms: &[ExpectedRom]| {
        let mut roms = roms.to_vec();
        roms.sort_by(|x, y| x.name.cmp(&y.name));
        roms
    };
    a.len() == b.len() && sorted(a) == sorted(b)
}

pub(super) fn read_built(c: &Connection, folder: i64) -> Result<BTreeMap<String, Built>> {
    let mut stmt = c.prepare(
        "SELECT target, game, roms, size, mtime_ns, build_id, sha256, torrentzip, loose FROM built_files
         WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Built {
                game: r.get(1)?,
                roms: json(r, 2)?,
                file: FileStat {
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                },
                build: r.get(5)?,
                sha256: r.get(6)?,
                torrentzip: r.get(7)?,
                loose: r.get(8)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn json<T: serde::de::DeserializeOwned>(row: &Row, i: usize) -> rusqlite::Result<T> {
    let text: String = row.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("store values always serialize")
}

/// The SHA-256 of the plan's canonical JSON: compact, with fields in declaration order and items in plan order.
pub(super) fn digest(content: &PlanContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a plan always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Nanoseconds since the Unix epoch.
pub(super) fn mtime_ns(meta: &Metadata) -> i128 {
    match meta.modified() {
        Ok(at) => match at.duration_since(UNIX_EPOCH) {
            Ok(after) => after.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        },
        Err(_) => 0,
    }
}

pub(super) fn stat(meta: &Metadata) -> FileStat {
    FileStat {
        size: meta.len(),
        mtime_ns: mtime_ns(meta),
    }
}

/// What the folder's current Build was made from.
struct Previous {
    selection: i64,
    rows: Vec<ReleaseRow>,
    profile_version: i64,
    revision: i64,
}

/// Everything a plan reads from the store, read in one transaction.
struct Inputs {
    store: String,
    folder: FolderRow,
    set_root: String,
    set_path: String,
    selection_id: i64,
    selection: Selection,
    rows: Vec<ReleaseRow>,
    profile_version: i64,
    revision_id: i64,
    revision: DatRevisionRecord,
    /// The Source Set's latest Audit and its DAT Revision.
    audit: Option<(i64, i64, String)>,
    /// That Audit's Entries, when it is against the Selection's DAT Revision.
    entries: Vec<Entry>,
    built: BTreeMap<String, Built>,
    previous: Option<Previous>,
    /// The folder's earliest Build that ended without its folder being cleaned up, and why recovery left it.
    unrecovered: Option<(i64, Option<String>)>,
    choices: PlanChoices,
    /// The selected Games, by their target's lowercased name.
    selected: HashMap<String, String>,
    /// What earlier checks of the folder read, by name.
    cached: HashMap<String, (Seen, Probe)>,
    /// The directories Builds created in the folder that outlive them, such as the retention area, by name.
    dirs: HashMap<String, Identity>,
    /// The entries Title Select did not build that you acknowledged, by name.
    acks: HashMap<String, Ack>,
    /// What the storage policy says for the folder's system.
    storage: Storage,
    /// The originals conversions recorded before they put their converted files in place: by name, the name each
    /// was converted to, and the original's identity.
    originals: HashSet<(String, String, Identity)>,
}

fn load(
    c: &Connection,
    folder_id: i64,
    selection_id: i64,
    choices: &PlanChoices,
) -> Result<Inputs> {
    let folder = folder_by_id(c, folder_id)?;
    let (set_id, revision_id, profile_version, profile_id, pruned): (i64, i64, i64, i64, bool) = c
        .query_row(
            "SELECT sel.source_set_id, sel.dat_revision_id, sel.profile_version_id, v.profile_id,
                    sel.pruned_at IS NOT NULL
             FROM selections sel JOIN profile_versions v ON v.id = sel.profile_version_id
             WHERE sel.id = ?1",
            [selection_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no selection {selection_id}")))?;
    if set_id != folder.source_set_id || profile_id != folder.profile_id {
        let record = record_by_id(c, selection_id)?;
        return Err(Error::Refused(format!(
            "selection {selection_id} was made for source set {} with profile {}, but managed folder {} builds \
             source set {} with profile {}",
            record.source_set, record.profile.name, folder.id, folder.source_set, folder.profile
        )));
    }
    if pruned {
        return Err(Error::Refused(format!(
            "selection {selection_id} was pruned and only its summary is kept; make a new selection to build"
        )));
    }
    let selection = read_selection(c, selection_id)?;
    let rows = read_rows(c, selection_id)?;
    let (_, revision) = revision_by_id(c, revision_id)?;
    let set = set_by_id(c, set_id)?;
    let audit: Option<(i64, i64, String)> = c
        .query_row(
            "SELECT a.id, a.dat_revision_id, r.sha256 FROM audits a JOIN dat_revisions r ON r.id = a.dat_revision_id
             WHERE a.source_set_id = ?1 AND a.status = 'finished' AND a.pruned_at IS NULL
             ORDER BY a.finish_seq DESC LIMIT 1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let entries = match &audit {
        Some((id, audit_revision, _)) if *audit_revision == revision_id => {
            let mut stmt = c.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM audit_entries WHERE audit_id = ?1 ORDER BY seq"
            ))?;
            let mut rows = stmt.query([id])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(entry_from_row(row)?);
            }
            entries
        }
        _ => Vec::new(),
    };
    let previous = match folder.current_build {
        Some(build) => {
            let (selection, profile_version, revision): (i64, i64, i64) = c.query_row(
                "SELECT b.selection_id, sel.profile_version_id, sel.dat_revision_id
                 FROM builds b JOIN selections sel ON sel.id = b.selection_id WHERE b.id = ?1",
                [build],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            Some(Previous {
                selection,
                rows: read_rows(c, selection)?,
                profile_version,
                revision,
            })
        }
        None => None,
    };
    let unrecovered = c
        .query_row(
            "SELECT id, recovery_error FROM builds
             WHERE folder_id = ?1 AND cleaned_at IS NULL AND state IN ('failed', 'interrupted')
             ORDER BY id LIMIT 1",
            [folder.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut selected = HashMap::new();
    for group in &selection.groups {
        for winner in &group.winners {
            selected.insert(
                format!("{}.zip", winner.title).to_lowercase(),
                winner.title.clone(),
            );
        }
    }
    let storage = crate::storage::policy(c, StorageScope::PlayLibraries, &folder.system)?;
    Ok(Inputs {
        storage,
        unrecovered,
        choices: choices.clone(),
        selected,
        cached: read_probes(c, folder.id)?,
        acks: read_acks(c, folder.id)?,
        dirs: folder_dirs(c, folder.id)?,
        store: store_id(c)?,
        built: read_built(c, folder.id)?,
        originals: read_originals(c, folder.id)?,
        folder,
        set_root: set.record.root,
        set_path: set.record.path,
        selection_id,
        selection,
        rows,
        profile_version,
        revision_id,
        revision,
        audit,
        entries,
        previous,
    })
}

/// A plan before it is stored.
pub(super) struct Planned {
    pub content: PlanContent,
    pub previous: Option<i64>,
    pub changes: Vec<PlanChange>,
    /// The check of the folder the plan was made with, when the folder could be opened.
    pub check: Option<FolderCheck>,
    /// The targets of unresolved items whose earlier built file the check found still holds the ROMs the DAT
    /// revision now expects for the item's game.
    pub retained: HashSet<String>,
    pub scan: Option<Scan>,
}

fn blocker(kind: BlockerKind, message: impl Into<String>) -> Blocker {
    Blocker {
        kind,
        message: message.into(),
        group: None,
        target: None,
    }
}

/// An entry in a Managed Folder, as `lstat` found it.
struct Found {
    /// Its name as it is.
    name: String,
    file: FileStat,
    identity: Identity,
    regular: bool,
}

/// The entries in a Managed Folder, other than its own, by lowercased name.
type Files = HashMap<String, Found>;

/// The folder as a plan found it.
struct Folder {
    files: Files,
    /// The check of its entries, made when its marker shows it is this store's folder.
    scan: Option<Scan>,
    /// Why its marker does not.
    marker: Option<String>,
}

/// The folder's state, from a stat of it and its marker and a check of its entries: its files by lowercased name, or
/// why it cannot be built into.
fn check_folder(
    config: &Config,
    inputs: &Inputs,
    dat: Option<&Dat>,
    blockers: &mut Vec<Blocker>,
) -> Option<Folder> {
    let folder = &inputs.folder;
    if !config.root(&folder.root).is_some_and(|r| r.writable) {
        blockers.push(blocker(
            BlockerKind::ReadOnlyRoot,
            format!(
                "root {} is not writable; declare it with writable = true to build into it",
                folder.root
            ),
        ));
    }
    let dir = match open_folder(config, &folder.root, &folder.path) {
        Ok(dir) => dir,
        Err(reason) => {
            blockers.push(blocker(
                BlockerKind::FolderMissing,
                format!("the managed folder cannot be opened: {reason}. Is its share mounted?"),
            ));
            return None;
        }
    };
    let listing = match dir.entries() {
        Ok(listing) => listing,
        Err(e) => {
            blockers.push(blocker(BlockerKind::FolderMissing, e.to_string()));
            return None;
        }
    };
    let mut files = HashMap::new();
    let mut leftovers = Vec::new();
    for (name, _) in &listing {
        if is_own(name) {
            // A folder Title Select was creating that was never renamed into place is recorded, and left.
            let recorded_leftover = || matches!(dir.stat(name), Ok(Some(st)) if inputs.dirs.get(name) == Some(&Identity::of(&st)));
            if name == STAGING || name == MARKER_NEW || (is_dir_temp(name) && !recorded_leftover())
            {
                leftovers.push(name.clone());
            }
            continue;
        }
        let Ok(Some(st)) = dir.stat(name) else {
            continue;
        };
        files.insert(
            name.to_lowercase(),
            Found {
                name: name.clone(),
                file: file_stat(&st),
                identity: Identity::of(&st),
                regular: FolderDir::kind(&st) == rustix::fs::FileType::RegularFile,
            },
        );
    }
    check_recovery(inputs, &dir, &leftovers, blockers);
    check_retention(inputs, &dir, blockers);
    let problem = marker_problem(
        &dir,
        &inputs.store,
        folder.id,
        folder.generation,
        files.is_empty(),
    );
    let scan = match &problem {
        Some(problem) => {
            blockers.push(blocker(BlockerKind::Marker, problem.clone()));
            None
        }
        None => {
            let mut entries: Vec<String> = listing
                .into_iter()
                .map(|(name, _)| name)
                .filter(|name| !is_own(name))
                .collect();
            entries.sort();
            Some(scan(
                &dir,
                &entries,
                &ScanInput {
                    built: &inputs.built,
                    selected: &inputs.selected,
                    dat,
                    storage: inputs.storage,
                    cached: &inputs.cached,
                    acks: &inputs.acks,
                },
                None,
            ))
        }
    };
    Some(Folder {
        files,
        scan,
        marker: problem,
    })
}

/// Blocks on an earlier Build whose folder recovery has not cleaned up, or on a staging directory or half-written
/// marker that no Build of this store accounts for.
fn check_recovery(
    inputs: &Inputs,
    dir: &FolderDir,
    leftovers: &[String],
    blockers: &mut Vec<Blocker>,
) {
    if let Some((build, reason)) = &inputs.unrecovered {
        blockers.push(blocker(
            BlockerKind::Recovery,
            format!(
                "build {build} stopped, and its folder has not been cleaned up since{}. The service cleans it up \
                 when it starts; restart it once the folder is as build {build} left it",
                reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
        ));
    } else if !leftovers.is_empty() && inputs.folder.active_job.is_none() {
        blockers.push(blocker(
            BlockerKind::Recovery,
            format!(
                "{} holds {}, which no build of this store left there; remove {} to build here",
                dir.path().display(),
                leftovers.join(" and "),
                if leftovers.len() == 1 { "it" } else { "them" }
            ),
        ));
    }
}

/// Blocks on a retention area that no Build of this store created: anything at its name that is not the directory
/// a Build recorded creating there.
fn check_retention(inputs: &Inputs, dir: &FolderDir, blockers: &mut Vec<Blocker>) {
    let found = match dir.stat(RETENTION) {
        Ok(found) => found,
        Err(e) => {
            blockers.push(blocker(BlockerKind::Recovery, e.to_string()));
            return;
        }
    };
    let Some(st) = found else {
        return;
    };
    let ours = FolderDir::kind(&st) == rustix::fs::FileType::Directory
        && inputs.dirs.get(RETENTION) == Some(&Identity::of(&st));
    if !ours {
        blockers.push(blocker(
            BlockerKind::Recovery,
            format!(
                "{} holds {RETENTION}, which no build of this store created; move it out of the folder to build here",
                dir.path().display()
            ),
        ));
    }
}

/// The file `found` to keep aside for `reason`, with its SHA-256: what the plan's check read of it while it is the
/// same file, or else `recorded`, what the store recorded of the file a Build wrote there.
fn keep_of(
    reason: KeepReason,
    found: &Found,
    scan: Option<&Scan>,
    recorded: Option<&String>,
) -> KeepAside {
    let probed = scan
        .and_then(|s| s.probes.get(&found.name))
        .filter(|(seen, _)| seen.identity == found.identity && seen.file == found.file)
        .map(|(_, probe)| probe.sha256.clone());
    KeepAside {
        reason,
        name: found.name.clone(),
        file: found.file.clone(),
        identity: found.identity.into(),
        sha256: probed.or_else(|| recorded.cloned()),
        directory: false,
    }
}

/// What the plan does with each file built into the folder whose game is no longer selected: one the check found
/// as built is kept aside, one gone is forgotten, and one changed since is left as it is. Files of `untouched`
/// releases are skipped.
fn removals(
    inputs: &Inputs,
    folder: &Folder,
    items: &[PlanItem],
    untouched: &BTreeSet<String>,
) -> Vec<PlanRemoval> {
    let Some(scan) = &folder.scan else {
        return Vec::new();
    };
    let targets: HashSet<&str> = items.iter().map(|i| i.target.as_str()).collect();
    let lowered: HashSet<String> = items.iter().map(|i| i.target.to_lowercase()).collect();
    // Files a selected game's item converts, kept aside by that item once its new file is written.
    let converted: HashSet<&str> = items
        .iter()
        .filter_map(|i| i.converts.as_ref().map(|k| k.name.as_str()))
        .collect();
    let mut out = Vec::new();
    for (target, built) in &inputs.built {
        if targets.contains(target.as_str())
            || untouched.contains(target)
            || converted.contains(target.as_str())
        {
            continue;
        }
        if let Some(item) = items.iter().find(|i| i.game == built.game) {
            let outcome = scan
                .files
                .iter()
                .find(|f| f.target == *target)
                .map(|c| c.outcome);
            let mut removal = PlanRemoval {
                target: target.clone(),
                game: built.game.clone(),
                kind: RemovalKind::Leave,
                reason: format!(
                    "{target} stays until {} can be written as the storage policy says",
                    item.target
                ),
                keep: None,
                converts_to: None,
            };
            let intact = folder.files.get(&target.to_lowercase()).filter(|f| {
                f.regular
                    && f.name == *target
                    && matches!(
                        outcome,
                        Some(
                            FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
                        )
                    )
            });
            // A conversion put the game's file in place, then stopped before it disposed of the original: only the
            // same ROMs, and only the very file the conversion recorded before it did.
            let recorded = |found: &Found| {
                same_roms(&built.roms, &item.roms)
                    && inputs.originals.contains(&(
                        target.clone(),
                        item.target.clone(),
                        found.identity,
                    ))
            };
            if outcome == Some(FileOutcome::Missing) {
                removal.kind = RemovalKind::Forget;
                removal.reason = format!("{target} is gone already; its record is dropped");
            } else if let Some(found) = intact.filter(|f| {
                item.kind == ItemKind::Unchanged && built.loose != item.loose && recorded(f)
            }) {
                removal.kind = RemovalKind::Keep;
                removal.reason = format!(
                    "{target} was converted to {}, which is in place; it is kept aside{}",
                    item.target,
                    match inputs.storage.delete_originals {
                        true => format!(", then deleted once {} is verified", item.target),
                        false => String::new(),
                    }
                );
                removal.keep = Some(keep_of(
                    KeepReason::Replacement,
                    found,
                    Some(scan),
                    built.sha256.as_ref(),
                ));
                removal.converts_to = Some(item.target.clone());
            } else if let Some(found) = intact.filter(|_| {
                built.loose != item.loose
                    && (item.kind == ItemKind::Unchanged || item.kind.writes())
            }) {
                // Another file for the game, not a conversion of this one: it is replaced, and only ever kept.
                removal.kind = RemovalKind::Keep;
                removal.reason = format!(
                    "{target} is replaced by {}; it is kept aside once {} is in place",
                    item.target, item.target
                );
                removal.keep = Some(keep_of(
                    KeepReason::Replacement,
                    found,
                    Some(scan),
                    built.sha256.as_ref(),
                ));
            }
            out.push(removal);
            continue;
        }
        let lower = target.to_lowercase();
        let checked = scan.files.iter().find(|f| f.target == *target);
        let found = folder.files.get(&lower);
        let mut removal = PlanRemoval {
            target: target.clone(),
            game: built.game.clone(),
            kind: RemovalKind::Leave,
            reason: String::new(),
            keep: None,
            converts_to: None,
        };
        let outcome = checked.map(|c| c.outcome);
        if lowered.contains(&lower) {
            removal.reason = format!(
                "{target} differs from a selected game's file only in case; it is left as it is"
            );
        } else if outcome == Some(FileOutcome::Missing) {
            removal.kind = RemovalKind::Forget;
            removal.reason = format!(
                "{} is no longer selected, and {target} is gone already; its record is dropped",
                built.game
            );
        } else if let Some(found) = found.filter(|f| {
            f.regular
                && f.name == *target
                && matches!(
                    outcome,
                    Some(FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked)
                )
        }) {
            removal.kind = RemovalKind::Keep;
            removal.reason = format!(
                "{} is no longer selected; {target} is kept aside in the folder's retention area",
                built.game
            );
            removal.keep = Some(keep_of(
                KeepReason::Removal,
                found,
                Some(scan),
                built.sha256.as_ref(),
            ));
        } else {
            removal.reason = format!(
                "{} is no longer selected, but {target} changed since it was built{}; it is left as it is",
                built.game,
                checked
                    .and_then(|c| c.detail.as_deref())
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            );
        }
        out.push(removal);
    }
    out
}

/// One Entry that holds a ROM's payload.
struct Candidate<'a> {
    entry: &'a Entry,
    file: FileStat,
    identity: FileIdentity,
    ctime_ns: i128,
    copyable: bool,
}

/// Finds and ranks the Entries that hold each ROM, reading each source's metadata once and each archive's central
/// directory at most once per Game. Only an Entry with Payload Evidence is a source: a Claim says nothing of whether
/// its payload is intact.
struct Sources<'a> {
    set_dir: Option<(PathBuf, PathBuf)>,
    by_rom: HashMap<(String, String), Vec<&'a Entry>>,
    /// The ROMs only Claim Evidence holds.
    claimed: HashSet<(String, String)>,
    by_path: HashMap<&'a str, Vec<&'a Entry>>,
    stats: HashMap<&'a str, Option<(FileStat, FileIdentity, i128)>>,
    copyable: HashMap<(&'a str, String), bool>,
}

impl<'a> Sources<'a> {
    fn new(config: &Config, inputs: &'a Inputs) -> Sources<'a> {
        let set_dir = resolve(config, &inputs.set_root, &inputs.set_path)
            .ok()
            .map(|r| (r.path, r.canonical_root));
        let mut by_rom: HashMap<(String, String), Vec<&Entry>> = HashMap::new();
        let mut claimed = HashSet::new();
        let mut by_path: HashMap<&str, Vec<&Entry>> = HashMap::new();
        for entry in &inputs.entries {
            by_path.entry(&entry.path).or_default().push(entry);
            if !matches!(
                entry.finding,
                Finding::Matched | Finding::Misnamed | Finding::Conflicting
            ) {
                continue;
            }
            let payload = entry
                .evidence
                .as_ref()
                .is_some_and(|e| e.level == EvidenceLevel::Payload);
            for rom in &entry.roms {
                let key = (rom.game.clone(), rom.name.clone());
                if payload {
                    by_rom.entry(key).or_default().push(entry);
                } else {
                    claimed.insert(key);
                }
            }
        }
        Sources {
            set_dir,
            by_rom,
            claimed,
            by_path,
            stats: HashMap::new(),
            copyable: HashMap::new(),
        }
    }

    /// The source file at `path`, confined to the Source Set's Root, its identity and its status change time.
    fn file(&mut self, path: &'a str) -> Option<(FileStat, FileIdentity, i128)> {
        let set_dir = &self.set_dir;
        self.stats
            .entry(path)
            .or_insert_with(|| {
                let (dir, root) = set_dir.as_ref()?;
                let full = dir.join(path);
                let real = full.canonicalize().ok()?;
                if !real.starts_with(root) {
                    return None;
                }
                std::fs::metadata(&real)
                    .ok()
                    .filter(Metadata::is_file)
                    .map(|m| {
                        use std::os::unix::fs::MetadataExt;
                        (
                            stat(&m),
                            FileIdentity {
                                dev: m.dev(),
                                ino: m.ino(),
                            },
                            i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec()),
                        )
                    })
            })
            .clone()
    }

    /// Whether the archive at `path` can be copied as `game`'s file as it is: its members are exactly the Game's
    /// ROMs under their names, each Matched on its payload, and it is already TorrentZip.
    fn copyable(&mut self, path: &'a str, game: &str, roms: &[ExpectedRom]) -> bool {
        if let Some(&known) = self.copyable.get(&(path, game.to_owned())) {
            return known;
        }
        let members = self.by_path.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let names: BTreeSet<&str> = roms.iter().map(|r| r.name.as_str()).collect();
        let exact = members.len() == roms.len()
            && names.len() == roms.len()
            && members.iter().all(|e| {
                e.finding == Finding::Matched
                    && e.evidence
                        .as_ref()
                        .is_some_and(|ev| ev.level == EvidenceLevel::Payload)
                    && e.member.as_deref().is_some_and(|m| {
                        names.contains(m) && e.roms.iter().any(|r| r.game == game && r.name == m)
                    })
            })
            && members
                .iter()
                .filter_map(|e| e.member.as_deref())
                .collect::<BTreeSet<_>>()
                == names;
        let copyable = exact
            && self.file(path).is_some()
            && self
                .set_dir
                .as_ref()
                .is_some_and(|(dir, _)| is_torrentzip(&dir.join(path)));
        self.copyable.insert((path, game.to_owned()), copyable);
        copyable
    }

    /// The best Entry for each ROM, in ROM order, or the ROMs no Entry holds.
    fn choose(
        &mut self,
        game: &str,
        roms: &[ExpectedRom],
    ) -> std::result::Result<Vec<Candidate<'a>>, Vec<String>> {
        let mut chosen = Vec::new();
        let mut missing = Vec::new();
        for rom in roms {
            let entries = self
                .by_rom
                .get(&(game.to_owned(), rom.name.clone()))
                .cloned()
                .unwrap_or_default();
            let mut candidates: Vec<Candidate> = entries
                .into_iter()
                .filter_map(|entry| {
                    let (file, identity, ctime_ns) = self.file(&entry.path)?;
                    let copyable = entry.member.is_some() && self.copyable(&entry.path, game, roms);
                    Some(Candidate {
                        entry,
                        file,
                        identity,
                        ctime_ns,
                        copyable,
                    })
                })
                .collect();
            candidates.sort_by(|a, b| rank(a).cmp(&rank(b)));
            match candidates.into_iter().next() {
                Some(best) => chosen.push(best),
                None => missing.push(rom.name.clone()),
            }
        }
        if missing.is_empty() {
            Ok(chosen)
        } else {
            Err(missing)
        }
    }
}

/// Matched over Misnamed or Conflicting; a source that can be copied as it is; then path order.
fn rank<'a>(c: &Candidate<'a>) -> (bool, bool, &'a str, Option<&'a str>) {
    (
        c.entry.finding != Finding::Matched,
        !c.copyable,
        &c.entry.path,
        c.entry.member.as_deref(),
    )
}

fn compute(config: &Config, inputs: &Inputs, dat: Option<&Dat>) -> Planned {
    let mut blockers = Vec::new();
    let usable_audit = match &inputs.audit {
        None => {
            blockers.push(blocker(
                BlockerKind::NoAudit,
                format!(
                    "source set {} has no audit to take sources from; audit it first",
                    inputs.folder.source_set
                ),
            ));
            None
        }
        Some((_, revision, sha256)) if *revision != inputs.revision_id => {
            blockers.push(blocker(
                BlockerKind::RevisionMismatch,
                format!(
                    "the latest audit of source set {} is against DAT revision {}, but the selection is against {}; \
                     audit it against {} first",
                    inputs.folder.source_set,
                    short(sha256),
                    short(&inputs.revision.sha256),
                    short(&inputs.revision.sha256)
                ),
            ));
            None
        }
        Some((id, _, _)) => Some(*id),
    };
    // Building the resolved groups only leaves every release of an unresolved group untouched: its winner is not
    // built, and a file built for any of its releases is neither replaced nor removed.
    let mut unresolved = BTreeSet::new();
    let mut untouched = BTreeSet::new();
    for group in &inputs.selection.groups {
        if !group.unresolved {
            continue;
        }
        if inputs.choices.resolved_only {
            unresolved.insert(group.group.as_str());
            let titles = group
                .winners
                .iter()
                .map(|w| &w.title)
                .chain(group.alternatives.iter().map(|a| &a.title))
                .chain(group.exclusions.iter().map(|e| &e.title));
            let titles: HashSet<&String> = titles.collect();
            untouched.extend(titles.iter().map(|t| format!("{t}.zip")));
            // A file built for one of them in either format, under its own name.
            untouched.extend(
                inputs
                    .built
                    .iter()
                    .filter(|(_, b)| titles.contains(&b.game))
                    .map(|(target, _)| target.clone()),
            );
        } else {
            blockers.push(Blocker {
                kind: BlockerKind::Unresolved,
                message: format!(
                    "the group {} is unresolved; confirm a choice for it in the selection, or build the resolved \
                     groups only",
                    group.group
                ),
                group: Some(group.group.clone()),
                target: None,
            });
        }
    }
    let folder = check_folder(config, inputs, dat, &mut blockers);

    let mut items = Vec::new();
    if let (Some(dat), Some(_)) = (dat, usable_audit) {
        items = plan_items(config, inputs, dat, folder.as_ref(), &unresolved);
    }
    let removals = match (&folder, dat, usable_audit) {
        (Some(folder), Some(_), Some(_)) => removals(inputs, folder, &items, &untouched),
        _ => Vec::new(),
    };
    let (previous, changes) = match &inputs.previous {
        Some(previous) => (Some(previous.selection), changes(inputs, previous)),
        None => (None, Vec::new()),
    };
    let (check, scan) = match folder {
        Some(folder) => {
            let mut check = FolderCheck {
                folder: inputs.folder.id,
                summary: CheckSummary {
                    marker: folder.marker,
                    ..CheckSummary::default()
                },
                generation: inputs.folder.generation,
                selection: Some(inputs.selection_id),
                dat_revision: Some(inputs.revision.sha256.clone()),
                files: Vec::new(),
                drift: Vec::new(),
                acknowledged: Vec::new(),
            };
            if let Some(scan) = &folder.scan {
                check.summary = scan.summary();
                check.files = scan.files.clone();
                check.drift = scan.drift.clone();
                check.acknowledged = scan.acknowledged.clone();
            }
            (Some(check), folder.scan)
        }
        None => (None, None),
    };
    let retained = retained(inputs, &items, check.as_ref());
    let notes = storage_notes(inputs.storage, &items);
    Planned {
        content: PlanContent {
            folder: inputs.folder.id,
            generation: inputs.folder.generation,
            selection: inputs.selection_id,
            audit: usable_audit,
            dat_revision: inputs.revision.sha256.clone(),
            choices: inputs.choices.clone(),
            items,
            removals,
            blockers,
            cleanup: None,
            storage: inputs.storage,
            notes,
        },
        previous,
        changes,
        check,
        retained,
        scan,
    }
}

/// The targets of the unresolved `items` the folder still satisfies: `check` found the file built there unchanged,
/// or holding the same ROMs, and it was built for the item's game with the ROMs the item now expects.
fn retained(inputs: &Inputs, items: &[PlanItem], check: Option<&FolderCheck>) -> HashSet<String> {
    let intact: HashSet<&str> = check
        .iter()
        .flat_map(|c| &c.files)
        .filter(|f| {
            matches!(
                f.outcome,
                FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
            )
        })
        .map(|f| f.target.as_str())
        .collect();
    items
        .iter()
        .filter(|item| item.kind == ItemKind::Unresolved && !item.roms.is_empty())
        .filter(|item| intact.contains(item.target.as_str()))
        .filter(|item| {
            inputs
                .built
                .get(&item.target)
                .is_some_and(|b| b.game == item.game && b.roms == item.roms)
        })
        .map(|item| item.target.clone())
        .collect()
}

fn short(sha256: &str) -> &str {
    &sha256[..sha256.len().min(12)]
}

fn plan_items(
    config: &Config,
    inputs: &Inputs,
    dat: &Dat,
    folder: Option<&Folder>,
    unresolved: &BTreeSet<&str>,
) -> Vec<PlanItem> {
    let files = folder.map(|f| &f.files);
    let scan = folder.and_then(|f| f.scan.as_ref());
    let games: HashMap<&str, usize> = dat
        .games
        .iter()
        .enumerate()
        .map(|(i, g)| (g.as_str(), i))
        .collect();
    let mut roms_of: Vec<Vec<ExpectedRom>> = vec![Vec::new(); dat.games.len()];
    for rom in &dat.roms {
        roms_of[rom.game].push(ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
        });
    }
    let mut winners: Vec<(&str, &str)> = Vec::new();
    let mut seen = BTreeSet::new();
    for group in &inputs.selection.groups {
        for winner in &group.winners {
            if seen.insert(winner.title.as_str()) {
                winners.push((&group.group, &winner.title));
            }
        }
    }
    // Where the storage policy stores each game, and whether loose.
    let target_of = |game: &str| -> (String, bool) {
        let names: Vec<&str> = games
            .get(game)
            .map(|&i| roms_of[i].iter().map(|r| r.name.as_str()).collect())
            .unwrap_or_default();
        let (target, loose, _) = storage_target(inputs.storage, game, &names);
        (target, loose)
    };
    let mut lowered: HashMap<String, usize> = HashMap::new();
    for (_, title) in &winners {
        *lowered
            .entry(target_of(title).0.to_lowercase())
            .or_default() += 1;
    }
    let mut sources = Sources::new(config, inputs);
    let mut items = Vec::new();
    for (group, game) in winners {
        let (target, loose) = target_of(game);
        let roms = games
            .get(game)
            .map(|&i| roms_of[i].clone())
            .unwrap_or_default();
        let existing = files.and_then(|f| f.get(&target.to_lowercase()));
        let mut item = PlanItem {
            group: group.to_owned(),
            game: game.to_owned(),
            target: target.clone(),
            kind: ItemKind::Blocked,
            loose,
            roms,
            sources: Vec::new(),
            reason: None,
            existing: existing.map(|found| found.file.clone()),
            adoptable: false,
            keepable: false,
            keep: None,
            converts: None,
        };
        if unresolved.contains(group) {
            item.kind = ItemKind::Unresolved;
            item.reason = Some(format!(
                "the group {group} is unresolved, and you build the resolved groups only; whatever is at {target} \
                 is left untouched"
            ));
            items.push(item);
            continue;
        }
        let duplicate = {
            let mut names = BTreeSet::new();
            item.roms
                .iter()
                .find(|r| !names.insert(r.name.as_str()))
                .map(|r| r.name.clone())
        };
        let built = inputs.built.get(&target);
        let blocked = if !games.contains_key(game) {
            Some(format!("the DAT revision lists no game named {game}"))
        } else if let Some(reason) = invalid_name(&target) {
            Some(reason)
        } else if let Some(reason) = item
            .roms
            .iter()
            .find_map(|r| invalid_name(&r.name).map(|why| format!("a ROM's name: {why}")))
        {
            Some(reason)
        } else if lowered[&target.to_lowercase()] > 1 {
            Some(format!(
                "another selected game's file differs from {target} only in case, which Windows and SMB cannot tell \
                 apart"
            ))
        } else {
            duplicate.map(|name| format!("the DAT lists two ROMs named {name} in it"))
        };
        if let Some(reason) = blocked {
            item.reason = Some(reason);
            items.push(item);
            continue;
        }
        let keep_chosen = inputs.choices.keep_aside.contains(&target);
        if let Some(built) = built {
            // The check read a file whose size or modification time changed: it is as built while it is still the
            // file the build wrote, or holds the same ROMs.
            let as_built = |file: &FileStat| match scan
                .and_then(|s| s.files.iter().find(|f| f.target == target))
            {
                Some(checked) => matches!(
                    checked.outcome,
                    FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
                ),
                None => *file == built.file,
            };
            let replacing =
                built.game != item.game || built.roms != item.roms || built.loose != item.loose;
            match existing {
                Some(found) if found.name == target && as_built(&found.file) => {
                    if !replacing {
                        item.kind = ItemKind::Unchanged;
                        items.push(item);
                        continue;
                    }
                    // Built from other ROMs: kept aside, then written again.
                    item.keep = Some(keep_of(
                        KeepReason::Replacement,
                        found,
                        scan,
                        built.sha256.as_ref(),
                    ));
                }
                Some(found) => {
                    let changed = format!("{target} changed since it was built");
                    if !conflict(&mut item, found, keep_chosen, scan, &changed) {
                        items.push(item);
                        continue;
                    }
                }
                // A built file that is gone is written again.
                None => {}
            }
        } else if let Some(found) = existing {
            item.adoptable = found.name == target
                && !item.roms.is_empty()
                && scan
                    .and_then(|s| s.probes.get(&found.name))
                    .is_some_and(|(at, probe)| {
                        Some(&at.file) == item.existing.as_ref()
                            && holds_as(probe, &item.roms, item.loose)
                    });
            let name = &found.name;
            let chosen = inputs.choices.adopt.contains(&target);
            if item.adoptable && chosen {
                item.kind = ItemKind::Adopt;
                item.reason = Some(format!(
                    "you adopt {name}: it holds the ROMs the DAT expects there, so it is recorded as built and \
                     nothing is written"
                ));
                items.push(item);
                continue;
            }
            let why = if chosen {
                format!(
                    "you chose to adopt {name}, but it does not hold exactly the ROMs the DAT expects there"
                )
            } else if item.adoptable {
                format!(
                    "{name} is in the folder and was not built by Title Select, but holds the ROMs the DAT expects \
                     there; leave it, adopt it as built, or keep it aside and rebuild"
                )
            } else {
                format!("{name} is in the folder and was not built by Title Select")
            };
            if !conflict(&mut item, found, keep_chosen && !chosen, scan, &why) {
                items.push(item);
                continue;
            }
        }
        if item.roms.is_empty() {
            item.kind = ItemKind::NoSource;
            item.reason = Some("the DAT lists no ROMs for it".into());
            unkeep(&mut item);
            items.push(item);
            continue;
        }
        match sources.choose(game, &item.roms) {
            Err(missing) => {
                item.kind = ItemKind::NoSource;
                let claimed = missing
                    .iter()
                    .any(|rom| sources.claimed.contains(&(game.to_owned(), rom.clone())));
                item.reason = Some(if claimed {
                    format!(
                        "the latest audit found no payload-verified source for {}: a quick inventory has only \
                         claim evidence for it, so audit the source set incrementally or in full first",
                        missing.join(", ")
                    )
                } else {
                    format!(
                        "the latest audit found no source for {}",
                        missing.join(", ")
                    )
                });
                unkeep(&mut item);
            }
            Ok(chosen) => {
                let first = &chosen[0];
                let copy = !item.loose
                    && first.copyable
                    && chosen
                        .iter()
                        .all(|c| c.copyable && c.entry.path == first.entry.path);
                item.kind = if copy {
                    ItemKind::Copy
                } else {
                    ItemKind::Repack
                };
                item.sources = chosen
                    .iter()
                    .zip(&item.roms)
                    .map(|(c, rom)| PlanSource {
                        rom: rom.name.clone(),
                        path: c.entry.path.clone(),
                        member: c.entry.member.clone(),
                        finding: c.entry.finding,
                        evidence: c.entry.evidence.as_ref().map(|e| e.level),
                        file: c.file.clone(),
                        identity: Some(c.identity),
                        // Only a loose file is written from the source the plan found, by descriptor.
                        ctime_ns: item.loose.then_some(c.ctime_ns),
                    })
                    .collect();
                item.converts = converts(inputs, files, scan, &item);
            }
        }
        items.push(item);
    }
    items
}

/// The file built for `item`'s game in the other format, under its other name, that writing `item` converts: only
/// while the check found it as it was built, so that it can be kept aside.
fn converts(
    inputs: &Inputs,
    files: Option<&Files>,
    scan: Option<&Scan>,
    item: &PlanItem,
) -> Option<KeepAside> {
    let (target, built) = inputs.built.iter().find(|(t, b)| {
        **t != item.target
            && b.game == item.game
            && b.loose != item.loose
            && same_roms(&b.roms, &item.roms)
    })?;
    let found = files?
        .get(&target.to_lowercase())
        .filter(|f| f.regular && f.name == *target)?;
    let intact = scan?.files.iter().any(|c| {
        c.target == *target
            && matches!(
                c.outcome,
                FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
            )
    });
    intact.then(|| keep_of(KeepReason::Replacement, found, scan, built.sha256.as_ref()))
}

/// Why games the storage policy would store loose stay TorrentZip, with how many each reason covers.
fn storage_notes(storage: Storage, items: &[PlanItem]) -> Vec<String> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for item in items.iter().filter(|i| !i.loose && !i.roms.is_empty()) {
        let names: Vec<&str> = item.roms.iter().map(|r| r.name.as_str()).collect();
        if let (_, _, Some(why)) = storage_target(storage, &item.game, &names) {
            *counts.entry(why).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|(why, n)| match n {
            1 => format!("1 game is not stored loose: {why}"),
            n => format!("{n} games are not stored loose, each because {why}"),
        })
        .collect()
}

/// Makes `item` a conflict with `found` at its target, `why`, unless you chose to `keep` it aside and it can be:
/// then the item is built with `found` kept aside first, and this returns true.
fn conflict(
    item: &mut PlanItem,
    found: &Found,
    keep: bool,
    scan: Option<&Scan>,
    why: &str,
) -> bool {
    item.kind = ItemKind::Conflict;
    item.keepable = found.regular;
    if keep && found.regular {
        item.keep = Some(keep_of(KeepReason::Conflict, found, scan, None));
        return true;
    }
    item.reason = Some(if keep {
        format!(
            "{why}; you chose to keep it aside, but it is not a regular file, so it is never moved. It is left as it is"
        )
    } else {
        format!("{why}; it is never overwritten")
    });
    false
}

/// Undoes keeping aside the file at `item`'s target when there is nothing to build in its place: a replacement's
/// file stays as it is, and a conflict you chose to keep aside stays a conflict.
fn unkeep(item: &mut PlanItem) {
    let Some(keep) = item.keep.take() else {
        return;
    };
    let why = item.reason.take().unwrap_or_default();
    match keep.reason {
        KeepReason::Conflict => {
            item.kind = ItemKind::Conflict;
            item.reason = Some(format!(
                "you chose to keep {} aside and rebuild, but there is nothing to build it from ({why}); it is left \
                 as it is",
                keep.name
            ));
        }
        _ => {
            item.reason = Some(format!(
                "{why}; the file built there from other ROMs is left as it is"
            ));
        }
    }
}

/// What changes from the folder's current Build, and why: releases that become or stop being winners.
fn changes(inputs: &Inputs, previous: &Previous) -> Vec<PlanChange> {
    diff_rows(&previous.rows, &inputs.rows)
        .into_iter()
        .filter(|c| c.before == Some(Role::Winner) || c.after == Some(Role::Winner))
        .map(|c| {
            let rule_of = |rows: &[ReleaseRow]| {
                rows.iter()
                    .find(|r| r.group == c.group && r.title == c.title)
                    .map(|r| r.rule.is_choice())
                    .unwrap_or(false)
            };
            let cause = if c.before.is_none() || c.after.is_none() {
                Cause::DatRevision
            } else if rule_of(&previous.rows) || rule_of(&inputs.rows) {
                Cause::Choices
            } else if previous.profile_version != inputs.profile_version {
                Cause::Settings
            } else if previous.revision != inputs.revision_id {
                Cause::DatRevision
            } else {
                Cause::Settings
            };
            PlanChange {
                group: c.group,
                title: c.title,
                before: c.before,
                after: c.after,
                cause,
            }
        })
        .collect()
}

const PLAN_QUERY: &str =
    "SELECT p.id, p.digest, p.created_at, p.content, p.previous_selection_id, p.changes,
            a.digest, a.actor, a.approved_at, (SELECT max(j.id) FROM jobs j WHERE j.plan_id = p.id), p.check_id
     FROM plans p LEFT JOIN plan_approvals a ON a.plan_id = p.id";

fn plan_from_row(r: &Row) -> rusqlite::Result<Plan> {
    let content: PlanContent = json(r, 3)?;
    let approval = match r.get::<_, Option<String>>(6)? {
        Some(digest) => Some(Approval {
            digest,
            actor: r.get(7)?,
            at: r.get(8)?,
        }),
        None => None,
    };
    Ok(Plan {
        id: r.get(0)?,
        digest: r.get(1)?,
        created_at: r.get(2)?,
        summary: content.summary(),
        content,
        previous_selection: r.get(4)?,
        changes: json(r, 5)?,
        approval,
        job: r.get(9)?,
        check: r.get(10)?,
    })
}

pub(super) fn plan_by_id(c: &Connection, id: i64) -> Result<Plan> {
    c.query_row(
        &format!("{PLAN_QUERY} WHERE p.id = ?1"),
        [id],
        plan_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no plan {id}")))
}

pub(super) fn insert_plan(tx: &Transaction, planned: &Planned, check: Option<i64>) -> Result<Plan> {
    let content = &planned.content;
    tx.execute(
        "INSERT INTO plans (folder_id, selection_id, audit_id, generation, content, digest, previous_selection_id,
                            changes, created_at, check_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            content.folder,
            content.selection,
            content.audit,
            content.generation as i64,
            to_json(content),
            digest(content),
            planned.previous,
            to_json(&planned.changes),
            now(),
            check
        ],
    )?;
    plan_by_id(tx, tx.last_insert_rowid())
}

impl Store {
    /// Computes what building `selection` into the Managed Folder would do, with your `choices`.
    pub(super) fn compute_plan(
        &self,
        config: &Config,
        folder: i64,
        selection: i64,
        choices: &PlanChoices,
    ) -> Result<Planned> {
        let inputs = self.read(|c| {
            let tx = c.unchecked_transaction()?;
            load(&tx, folder, selection, choices)
        })?;
        let dat = if inputs.revision.blob_present {
            let blob = self.blob_path(&inputs.revision.sha256);
            Some(Dat::read(&blob)?)
        } else {
            None
        };
        let mut planned = compute(config, &inputs, dat.as_ref());
        if dat.is_none() {
            planned.content.blockers.push(blocker(
                BlockerKind::RevisionMismatch,
                format!(
                    "DAT revision {} is no longer kept; import its file ({}) again to build from it",
                    short(&inputs.revision.sha256),
                    inputs.revision.file
                ),
            ));
        }
        Ok(planned)
    }

    /// Computes again the plan whose content is `content`, a cleanup or a build, as it would be now.
    pub(super) fn recompute(&self, config: &Config, content: &PlanContent) -> Result<Planned> {
        match &content.cleanup {
            Some(cleanup) => self.compute_cleanup(config, content.folder, &cleanup.choices),
            None => self.compute_plan(config, content.folder, content.selection, &content.choices),
        }
    }

    /// Plans building `selection` into Managed Folder `folder`, and stores the plan for review.
    pub fn plan_build(&self, config: &Config, folder: i64, selection: i64) -> Result<Plan> {
        self.plan_build_with(config, folder, &PlanBuild::new(selection))
    }

    /// Plans building `request.selection` into Managed Folder `folder` with your choices, and stores the plan for
    /// review with the check of the folder it was made with. While a Build or Check of the folder is queued or
    /// running, the check is not stored and nothing it found is recorded.
    pub fn plan_build_with(
        &self,
        config: &Config,
        folder: i64,
        request: &PlanBuild,
    ) -> Result<Plan> {
        let mut choices = request.choices.clone();
        choices.adopt.sort();
        choices.adopt.dedup();
        choices.keep_aside.sort();
        choices.keep_aside.dedup();
        if let Some(both) = choices
            .keep_aside
            .iter()
            .find(|t| choices.adopt.contains(t))
        {
            return Err(Error::Refused(format!(
                "you chose both to adopt {both} and to keep it aside; choose one"
            )));
        }
        let planned = self.compute_plan(config, folder, request.selection, &choices)?;
        self.write(|tx| {
            let active = folder_by_id(tx, folder)?.active_job;
            let check = match (&planned.check, active) {
                (Some(check), None) => Some(
                    store_check(tx, folder, None, planned.scan.as_ref(), check.clone())?
                        .summary
                        .id,
                ),
                _ => None,
            };
            insert_plan(tx, &planned, check)
        })
    }

    pub fn plan(&self, id: i64) -> Result<Plan> {
        self.read(|c| plan_by_id(c, id))
    }

    /// Approves plan `id` as reviewed with `digest`, and queues the Build Job that carries it out. The plan is
    /// computed again first: when the store or the folder changed since, the new plan is stored and the approval
    /// refused, naming it.
    pub fn approve_plan(
        &self,
        config: &Config,
        id: i64,
        digest_reviewed: &str,
        actor: &str,
        workers: Workers,
    ) -> Result<Job> {
        let plan = self.plan(id)?;
        if plan.approval.is_some() {
            return Err(Error::Refused(format!("plan {id} is already approved")));
        }
        if digest_reviewed != plan.digest {
            return Err(Error::Conflict(format!(
                "you reviewed a different version of plan {id}; review it again"
            )));
        }
        if !plan.approvable() {
            return Err(Error::Refused(format!(
                "plan {id} has blockers; resolve them and plan again"
            )));
        }
        let content = &plan.content;
        let again = self.recompute(config, content)?;
        if digest(&again.content) != plan.digest {
            let newer = self.write(|tx| insert_plan(tx, &again, None))?;
            return Err(Error::Conflict(format!(
                "the plan changed since you reviewed it; review plan {}",
                newer.id
            )));
        }
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE managed_folder_id = ?1 AND kind IN ('build', 'check')
                     AND state IN ('queued', 'running')",
                    [content.folder],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "managed folder {} already has a build or check queued or running: job {job}",
                    content.folder
                )));
            }
            if let Some(busy) = deletion_active(tx, content.folder)? {
                return Err(Error::Conflict(busy));
            }
            let folder = folder_by_id(tx, content.folder)?;
            let set = crate::sets::set_by_id(tx, folder.source_set_id)?;
            crate::repairs::check_no_repair(tx, set.id, &set.record.name)?;
            let revision_id: i64 = tx.query_row(
                "SELECT dat_revision_id FROM selections WHERE id = ?1",
                [content.selection],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO plan_approvals (plan_id, digest, actor, approved_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, plan.digest, actor, now()],
            )?;
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers, created_at,
                                   managed_folder_id, plan_id)
                 VALUES ('queued', 'build', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    folder.source_set_id,
                    revision_id,
                    actor,
                    workers.readers.max(1) as i64,
                    workers.hashers.max(1) as i64,
                    now(),
                    folder.id,
                    id
                ],
            )?;
            let job = tx.last_insert_rowid();
            // The Selection and Audit are held from pruning until the Job ends, in the transaction that queues it.
            let pruned: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM selections WHERE id = ?1 AND pruned_at IS NOT NULL)
                     OR EXISTS (SELECT 1 FROM audits WHERE id = ?2 AND pruned_at IS NOT NULL)",
                params![content.selection, content.audit],
                |r| r.get(0),
            )?;
            if pruned {
                return Err(Error::Refused(format!(
                    "plan {id}'s selection or audit was pruned; plan again"
                )));
            }
            let hold = build_hold(job);
            tx.execute(
                "INSERT INTO selection_citations (selection_id, cited_by) VALUES (?1, ?2)",
                params![content.selection, hold],
            )?;
            if let Some(audit) = content.audit {
                tx.execute(
                    "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, ?2)",
                    params![audit, hold],
                )?;
            }
            // So are those of each built file the plan keeps aside, which its kept file's record cites once kept.
            let kept = content
                .items
                .iter()
                .filter_map(|i| i.keep.as_ref().map(|k| &k.name))
                .chain(content.removals.iter().filter_map(|r| r.keep.as_ref().map(|k| &k.name)))
                .chain(content.cleanup.iter().flat_map(|c| c.repacks.iter().map(|r| &r.target)));
            for name in kept {
                tx.execute(
                    "INSERT OR IGNORE INTO selection_citations (selection_id, cited_by)
                     SELECT f.selection_id, ?3 FROM built_files f JOIN selections s ON s.id = f.selection_id
                     WHERE f.folder_id = ?1 AND f.target = ?2 AND s.pruned_at IS NULL",
                    params![content.folder, name, hold],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO audit_citations (audit_id, cited_by)
                     SELECT f.audit_id, ?3 FROM built_files f JOIN audits a ON a.id = f.audit_id
                     WHERE f.folder_id = ?1 AND f.target = ?2 AND a.pruned_at IS NULL",
                    params![content.folder, name, hold],
                )?;
            }
            job_by_id(tx, job)
        })
    }
}
