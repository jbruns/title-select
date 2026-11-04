//! Library Folders: folders in writable Roots that Builds write a Source Set's Selections into, each holding one
//! marker that ties it to this store.

use std::cmp::Ordering;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use title_select_api::build::{
    AddLibraryFolder, BuildState, BuildSummary, ChangeLibraryFolder, FileOutcome, Incomplete,
    IncompleteItem, IncompleteReason, LibraryFolder, MARKER, Marker, PlanContent, RETENTION,
    STAGING, Stale, UnfinishedBuild,
};
use title_select_api::discs;
use title_select_api::paths::{PathTemplate, TokenValues, relative_path};

use super::audits::recorded_content_sha256;
use super::checks::{latest_check, latest_summary, selected_targets};
use super::dats::split_dat;
use super::folder_dir::{FolderDir, Identity};
use super::plans::json;
use super::profiles::active_profile_id;
use super::selections::{read_selection, record_by_id};
use super::sets::{resolve, set_by_id, set_by_name};
use super::{Error, Result, Store, io_error, now};
use crate::config::Config;
use rustix::fs::FileType;

/// A Library Folder's row and what its bindings name.
#[derive(Debug, Clone)]
pub(super) struct FolderRow {
    pub id: i64,
    pub root: String,
    pub path: String,
    pub source_set_id: i64,
    pub source_set: String,
    pub dat_id: i64,
    pub system: String,
    pub profile_id: i64,
    pub profile: String,
    pub generation: u64,
    pub current_build: Option<i64>,
    pub created_at: String,
    pub files: u64,
    pub active_job: Option<i64>,
    pub latest_plan: Option<i64>,
    /// When it was Unregistered: its history stays readable, but nothing new is done to it.
    pub unregistered_at: Option<String>,
}

impl FolderRow {
    /// Refuses new work on an Unregistered Library Folder.
    pub(super) fn check_registered(&self) -> Result<()> {
        match &self.unregistered_at {
            Some(_) => Err(Error::Refused(format!(
                "play library folder {}/{} is unregistered; register the folder again to use it",
                self.root, self.path
            ))),
            None => Ok(()),
        }
    }
}

const FOLDER_QUERY: &str =
    "SELECT f.id, f.root, f.path, s.id, s.name, d.id, d.provider || '/' || d.name, p.id,
            p.name, f.generation, f.current_build_id, f.created_at,
            (SELECT count(*) FROM built_files b WHERE b.folder_id = f.id),
            (SELECT max(j.id) FROM jobs j
             WHERE j.library_folder_id = f.id AND j.kind IN ('build', 'check')
               AND j.state IN ('queued', 'running')),
            (SELECT max(pl.id) FROM plans pl WHERE pl.folder_id = f.id), f.unregistered_at
     FROM library_folders f
     JOIN source_sets s ON s.id = f.source_set_id
     JOIN dats d ON d.id = s.dat_id
     JOIN profiles p ON p.id = f.profile_id";

fn folder_from_row(r: &Row) -> rusqlite::Result<FolderRow> {
    Ok(FolderRow {
        id: r.get(0)?,
        root: r.get(1)?,
        path: r.get(2)?,
        source_set_id: r.get(3)?,
        source_set: r.get(4)?,
        dat_id: r.get(5)?,
        system: r.get(6)?,
        profile_id: r.get(7)?,
        profile: r.get(8)?,
        generation: r.get::<_, i64>(9)? as u64,
        current_build: r.get(10)?,
        created_at: r.get(11)?,
        files: r.get::<_, i64>(12)? as u64,
        active_job: r.get(13)?,
        latest_plan: r.get(14)?,
        unregistered_at: r.get(15)?,
    })
}

pub(super) fn folder_by_id(c: &Connection, id: i64) -> Result<FolderRow> {
    c.query_row(
        &format!("{FOLDER_QUERY} WHERE f.id = ?1"),
        [id],
        folder_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no library folder {id}")))
}

/// This store's id, which every marker it writes records.
pub(super) fn store_id(c: &Connection) -> Result<String> {
    Ok(
        c.query_row("SELECT value FROM meta WHERE key = 'store_id'", [], |r| {
            r.get(0)
        })?,
    )
}

const BUILD_QUERY: &str =
    "SELECT b.id, b.folder_id, b.plan_id, b.job_id, b.state, b.generation, b.selection_id,
            b.audit_id, r.sha256, b.started_at, b.finished_at,
            (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'applied'),
            (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'failed'),
            (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'pending'),
            p.content,
            (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'uncertain')
     FROM builds b
     JOIN dat_revisions r ON r.id = b.dat_revision_id
     JOIN plans p ON p.id = b.plan_id";

fn build_from_row(r: &Row) -> rusqlite::Result<BuildSummary> {
    let state: String = r.get(4)?;
    let content: String = r.get(14)?;
    let content: PlanContent = serde_json::from_str(&content).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(BuildSummary {
        id: r.get(0)?,
        folder: r.get(1)?,
        plan: r.get(2)?,
        job: r.get(3)?,
        state: BuildState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown build state {state}").into(),
            )
        })?,
        generation: r.get::<_, i64>(5)? as u64,
        selection: r.get(6)?,
        audit: r.get(7)?,
        dat_revision: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
        applied: r.get::<_, i64>(11)? as u64,
        failed: r.get::<_, i64>(12)? as u64,
        pending: r.get::<_, i64>(13)? as u64,
        uncertain: r.get::<_, i64>(15)? as u64,
        planned: content.summary(),
    })
}

pub(super) fn build_by_id(c: &Connection, id: i64) -> Result<BuildSummary> {
    c.query_row(
        &format!("{BUILD_QUERY} WHERE b.id = ?1"),
        [id],
        build_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no build {id}")))
}

pub(super) fn builds_where(
    c: &Connection,
    condition: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<BuildSummary>> {
    let mut stmt = c.prepare(&format!(
        "{BUILD_QUERY} WHERE {condition} ORDER BY b.id DESC"
    ))?;
    let rows = stmt.query_map(params, build_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn record(c: &Connection, config: &Config, row: FolderRow) -> Result<LibraryFolder> {
    let build = match row.current_build {
        Some(id) => Some(build_by_id(c, id)?),
        None => None,
    };
    let incomplete = Some(incomplete(c, &row)?.counts).filter(|counts| counts.total() > 0);
    let unfinished = unfinished(c, &row)?;
    let plan_open = plan_open(c, &row)?;
    let stale = stale(c, &row)?;
    Ok(LibraryFolder {
        id: row.id,
        writable: config.root(&row.root).is_some_and(|r| r.writable),
        root: row.root,
        path: row.path,
        system: row.system,
        source_set: row.source_set,
        profile: row.profile,
        generation: row.generation,
        created_at: row.created_at,
        build,
        files: row.files,
        active_job: row.active_job,
        latest_plan: row.latest_plan,
        incomplete,
        unfinished,
        check: latest_summary(c, row.id)?,
        plan_open,
        stale,
    })
}

/// Whether the folder's newest plan waits on approval: unapproved, and made at the folder's current generation, so
/// no Build has carried out another plan since.
fn plan_open(c: &Connection, row: &FolderRow) -> Result<bool> {
    let Some(plan) = row.latest_plan else {
        return Ok(false);
    };
    Ok(c.query_row(
        "SELECT p.generation = ?2 AND NOT EXISTS (SELECT 1 FROM plan_approvals a WHERE a.plan_id = p.id)
         FROM plans p WHERE p.id = ?1",
        params![plan, row.generation as i64],
        |r| r.get(0),
    )?)
}

/// What changed since the folder's current Build: a rebinding to another Source Set or Profile, or else a newer
/// Audit of its Source Set that found something other than the Build's did, or a version of its Profile newer than the
/// Build's Selection's.
fn stale(c: &Connection, row: &FolderRow) -> Result<Vec<Stale>> {
    let Some(build) = row.current_build else {
        return Ok(Vec::new());
    };
    let (set_id, set, profile_id, profile, version, audit): (i64, String, i64, String, u32, i64) =
        c.query_row(
            "SELECT s.source_set_id, ss.name, v.profile_id, p.name, v.version, b.audit_id
             FROM builds b
             JOIN selections s ON s.id = b.selection_id
             JOIN source_sets ss ON ss.id = s.source_set_id
             JOIN profile_versions v ON v.id = s.profile_version_id
             JOIN profiles p ON p.id = v.profile_id
             WHERE b.id = ?1",
            [build],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
    let mut stale = Vec::new();
    if set_id != row.source_set_id {
        stale.push(Stale::SourceSetRebound { built: set });
    } else if let Some(latest) = c
        .query_row(
            "SELECT a.id FROM audits a
             WHERE a.source_set_id = ?1 AND a.status = 'finished' AND a.pruned_at IS NULL
               AND a.finish_seq > (SELECT finish_seq FROM audits WHERE id = ?2)
             ORDER BY a.finish_seq DESC LIMIT 1",
            params![set_id, audit],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        && recorded_content_sha256(c, latest)? != recorded_content_sha256(c, audit)?
    {
        stale.push(Stale::SourceSetChanged {
            built: audit,
            latest,
        });
    }
    if profile_id != row.profile_id {
        stale.push(Stale::ProfileRebound { built: profile });
    } else {
        let latest: u32 = c.query_row(
            "SELECT max(version) FROM profile_versions WHERE profile_id = ?1",
            [profile_id],
            |r| r.get(0),
        )?;
        if latest > version {
            stale.push(Stale::ProfileSaved {
                built: version,
                latest,
            });
        }
    }
    Ok(stale)
}

/// The folder's latest Build that stopped before it finished, unless a Build has succeeded since. Whether it is the
/// folder's current Build does not matter: one stopped after it advanced the generation still did not finish. A
/// cleanup that succeeded since does not settle it: it built nothing the Build left undone.
fn unfinished(c: &Connection, row: &FolderRow) -> Result<Option<UnfinishedBuild>> {
    Ok(c.query_row(
        "SELECT b.id, b.state,
                (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'applied'),
                (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state = 'uncertain'),
                (SELECT count(*) FROM build_items i WHERE i.build_id = b.id AND i.state IN ('pending', 'failed')),
                (SELECT count(*) FROM kept_files k WHERE k.build_id = b.id AND k.state IN ('kept', 'deleting', 'quarantined', 'deleted', 'gone', 'restoring', 'restored'))
         FROM builds b
         WHERE b.folder_id = ?1 AND b.state IN ('failed', 'interrupted')
           AND NOT EXISTS (SELECT 1 FROM builds s WHERE s.folder_id = ?1 AND s.id > b.id AND s.state = 'succeeded'
                             AND s.kind = 'build')
         ORDER BY b.id DESC LIMIT 1",
        params![row.id],
        |r| {
            let state: String = r.get(1)?;
            Ok(UnfinishedBuild {
                build: r.get(0)?,
                state: BuildState::parse(&state).unwrap_or(BuildState::Interrupted),
                applied: r.get::<_, i64>(2)? as u64,
                uncertain: r.get::<_, i64>(3)? as u64,
                not_written: r.get::<_, i64>(4)? as u64,
                kept: r.get::<_, i64>(5)? as u64,
            })
        },
    )
    .optional()?)
}

/// The plan of Build `build` that built its files: its own, or, for a cleanup, that of the Build it cleaned up after.
pub(super) fn build_plan(c: &Connection, mut build: i64) -> Result<PlanContent> {
    loop {
        let content: PlanContent = c.query_row(
            "SELECT p.content FROM builds b JOIN plans p ON p.id = b.plan_id WHERE b.id = ?1",
            [build],
            |r| json(r, 0),
        )?;
        match &content.cleanup {
            Some(cleanup) => build = cleanup.build,
            None => return Ok(content),
        }
    }
}

/// What the folder lacks of its current Build's Selection: what the Build recorded, and the conflicts and missing
/// files a check found since at that Selection's targets, each target once. Which targets are selected comes from
/// the Build's Selection, never from the check, which may have been made for another Selection's plan.
pub(super) fn incomplete(c: &Connection, row: &FolderRow) -> Result<Incomplete> {
    let mut out = Incomplete {
        build: row.current_build,
        ..Incomplete::default()
    };
    let Some(build) = row.current_build else {
        return Ok(out);
    };
    let mut stmt = c.prepare(
        "SELECT reason, grp, game, target, detail FROM build_incomplete WHERE build_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([build], |r| {
        let reason: String = r.get(0)?;
        Ok(IncompleteItem {
            reason: IncompleteReason::parse(&reason).unwrap_or(IncompleteReason::Failed),
            group: r.get(1)?,
            game: r.get(2)?,
            target: r.get(3)?,
            detail: r.get(4)?,
        })
    })?;
    out.items = rows.collect::<Result<_, _>>()?;
    let selection: i64 = c.query_row(
        "SELECT selection_id FROM builds WHERE id = ?1",
        [build],
        |r| r.get(0),
    )?;
    if let Some(check) = latest_check(c, row.id)?
        .filter(|check| check.generation == row.generation && check.summary.marker.is_none())
    {
        let content = build_plan(c, build)?;
        let group_of = |target: &str| {
            content
                .items
                .iter()
                .find(|i| i.target.eq_ignore_ascii_case(target))
                .map(|i| (i.group.clone(), i.game.clone()))
        };
        let selected = selected_targets(c, selection)?;
        let is_selected = |target: &str| selected.contains_key(&target.to_lowercase());
        let mut found = Vec::new();
        for f in check.files.iter().filter(|f| is_selected(&f.target)) {
            let reason = match f.outcome {
                FileOutcome::Conflict | FileOutcome::Unreadable => IncompleteReason::Conflict,
                FileOutcome::Missing => IncompleteReason::Missing,
                _ => continue,
            };
            found.push((reason, f.target.clone(), f.detail.clone()));
        }
        for d in check.drift.iter().filter(|d| is_selected(&d.name)) {
            found.push((
                IncompleteReason::Conflict,
                d.name.clone(),
                Some(format!(
                    "{} is in the folder and was not built by Title Select",
                    d.name
                )),
            ));
        }
        for (reason, target, detail) in found {
            if out
                .items
                .iter()
                .any(|i| i.target.eq_ignore_ascii_case(&target))
            {
                continue;
            }
            let Some((group, game)) = group_of(&target) else {
                continue;
            };
            out.items.push(IncompleteItem {
                reason,
                group,
                game,
                target,
                detail,
            });
        }
    }
    if !record_by_id(c, selection)?.pruned {
        for group in read_selection(c, selection)?.groups {
            for (number, titles) in discs::missing(&group) {
                let Some(title) = titles.first() else {
                    continue;
                };
                if out.items.iter().any(|i| i.game == *title) {
                    continue;
                }
                out.items.push(IncompleteItem {
                    reason: IncompleteReason::MissingDisc,
                    group: group.group.clone(),
                    game: title.clone(),
                    target: title.clone(),
                    detail: Some(format!(
                        "disc {number} of the multi-disc game {} is not selected, so the game cannot be played \
                         through; choose the discs you want for the group {} in the selection",
                        discs::disc(title).map_or_else(|| title.clone(), |d| d.release),
                        group.group
                    )),
                });
            }
        }
    }
    for item in &mut out.items {
        if item.reason == IncompleteReason::MissingDisc {
            continue;
        }
        if let Some(d) = discs::disc(&item.game) {
            let note = format!(
                "it is disc {} of {}, so that multi-disc game is incomplete",
                d.number, d.release
            );
            item.detail = Some(match item.detail.take() {
                Some(detail) => format!("{detail}; {note}"),
                None => note,
            });
        }
    }
    for item in &out.items {
        out.counts.add(item.reason);
    }
    Ok(out)
}

/// Opens the Library Folder `rel` in the Root `root`, no component of `rel` followed if it is a symlink. The error
/// is a sentence for a plan's blocker.
pub(super) fn open_folder(
    config: &Config,
    root: &str,
    rel: &str,
) -> std::result::Result<FolderDir, String> {
    let root_config = config
        .root(root)
        .ok_or_else(|| format!("no root named {root} is configured any more"))?;
    FolderDir::open(&root_config.path, rel).map_err(|e| {
        format!("{e} (no part of a library folder's path inside its root may be a symlink)")
    })
}

/// Opens the Library Folder `rel` in the Root `root` as [`open_folder`] does, for a change: refused unless the
/// Root is configured `writable = true` now.
pub(super) fn open_writable_folder(
    config: &Config,
    root: &str,
    rel: &str,
) -> std::result::Result<FolderDir, String> {
    if !config.root(root).is_some_and(|r| r.writable) {
        return Err(format!(
            "root {root} is not writable now; declare it with writable = true, then restart the service. Nothing \
             in the folder was changed"
        ));
    }
    open_folder(config, root, rel)
}

/// The temp file a marker is written to before it is renamed over the marker.
pub(super) const MARKER_NEW: &str = ".title-select.new";

/// The largest marker read.
pub(super) const MARKER_LIMIT: u64 = 64 << 10;

/// Whether `name` is one of the files Title Select keeps in a Library Folder for itself.
pub(super) fn is_own(name: &str) -> bool {
    name == MARKER
        || name == STAGING
        || name == MARKER_NEW
        || name == RETENTION
        || super::kept::is_dir_temp(name)
}

/// Replaces the folder's marker atomically and durably. The new marker is written to a file created exclusively and
/// without following a symlink, so anything already at [`MARKER_NEW`] makes it fail. `record` is given the temp
/// file's identity as soon as it exists, before anything relies on it; the temp file is renamed over the marker
/// only while it is still that file, and the marker must then be that file.
pub(super) fn write_marker(
    dir: &FolderDir,
    marker: &impl serde::Serialize,
    record: &mut dyn FnMut(Identity) -> Result<()>,
) -> Result<()> {
    let mut json = serde_json::to_vec_pretty(marker).expect("a marker always serializes");
    json.push(b'\n');
    let tmp = dir.path().join(MARKER_NEW);
    let mut file = dir
        .create_new(MARKER_NEW)
        .map_err(|e| Error::Io(e.to_string()))?;
    let id = Identity::of_fd(&file).map_err(|e| io_error(&tmp, e))?;
    record(id)?;
    file.write_all(&json)
        .and_then(|()| file.sync_all())
        .map_err(|e| io_error(&tmp, e))?;
    let is_ours = |name: &str| -> Result<()> {
        match dir.stat(name).map_err(|e| Error::Io(e.to_string()))? {
            Some(st)
                if FolderDir::kind(&st) == FileType::RegularFile && Identity::of(&st) == id =>
            {
                Ok(())
            }
            _ => Err(Error::Refused(format!(
                "{}: {name} was replaced while the marker was written; the folder was left as it is",
                dir.path().display()
            ))),
        }
    };
    is_ours(MARKER_NEW)?;
    dir.replace(MARKER_NEW, MARKER)
        .and_then(|()| dir.sync())
        .map_err(|e| Error::Io(e.to_string()))?;
    is_ours(MARKER)
}

/// What a Library Folder's marker says.
pub(super) enum MarkerState {
    Absent,
    Unreadable(String),
    Present(Marker),
}

pub(super) fn read_marker(dir: &FolderDir) -> MarkerState {
    match dir.read(MARKER, MARKER_LIMIT) {
        Err(e) if e.kind() == ErrorKind::NotFound => MarkerState::Absent,
        Err(e) => MarkerState::Unreadable(e.to_string()),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(marker) => MarkerState::Present(marker),
            Err(e) => MarkerState::Unreadable(e.to_string()),
        },
    }
}

/// Why the folder's marker, read through `dir`, does not show it is Library Folder `folder` of store `store` at
/// `generation`, as a sentence; `None` when it does. `empty` is whether the folder holds nothing but Title Select's
/// own entries.
pub(super) fn marker_problem(
    dir: &FolderDir,
    store: &str,
    folder: i64,
    generation: u64,
    empty: bool,
) -> Option<String> {
    match read_marker(dir) {
        MarkerState::Absent if empty && generation == 0 => None,
        MarkerState::Absent if empty => Some(format!(
            "the folder is empty and has no marker, though {generation} builds went into it; is its share mounted?"
        )),
        MarkerState::Absent => Some(
            "the folder has files but no .title-select marker; is this the right folder, or the right mount?"
                .to_owned(),
        ),
        MarkerState::Unreadable(e) => Some(format!("its .title-select marker cannot be read: {e}")),
        MarkerState::Present(marker) if marker.store != store => Some(
            "its .title-select marker is from another store; is this a restored store, or the wrong mount?"
                .to_owned(),
        ),
        MarkerState::Present(marker) if marker.folder != folder => Some(format!(
            "its .title-select marker is for library folder {}, not {folder}; is this the right mount?",
            marker.folder
        )),
        MarkerState::Present(marker) => match marker.generation.cmp(&generation) {
            Ordering::Equal => None,
            Ordering::Greater => Some(format!(
                "its marker is at build generation {}, newer than the store's {generation}; was the store restored \
                 from a backup?",
                marker.generation
            )),
            Ordering::Less => Some(format!(
                "its marker is at build generation {}, older than the store's {generation}; was the folder restored \
                 from a backup, or is this the wrong mount?",
                marker.generation
            )),
        },
    }
}

/// Checks, through `dir`, that the folder's marker is exactly `expected`; the error says what it is instead.
pub(super) fn check_marker(dir: &FolderDir, expected: &Marker) -> std::result::Result<(), String> {
    match read_marker(dir) {
        MarkerState::Present(marker) if marker == *expected => Ok(()),
        MarkerState::Present(marker) => Err(format!(
            "{}: the marker is for store {}, library folder {}, generation {}, where this build expected store {}, \
             library folder {}, generation {}; the folder was left as it is",
            dir.path().display(),
            marker.store,
            marker.folder,
            marker.generation,
            expected.store,
            expected.folder,
            expected.generation
        )),
        MarkerState::Absent => Err(format!(
            "{}: the marker is gone; the folder was left as it is",
            dir.path().display()
        )),
        MarkerState::Unreadable(e) => Err(format!(
            "{}: the marker cannot be read ({e}); the folder was left as it is",
            dir.path().display()
        )),
    }
}

/// Opens the folder `rel` in the Root at `root_path`, creating each missing component, and checks that it is
/// empty. No existing component may be a symlink.
fn create_or_check_empty(root_path: &Path, rel: &str) -> Result<FolderDir> {
    let refused = |e: std::io::Error| Error::Refused(e.to_string());
    let mut dir = FolderDir::open(root_path, "").map_err(refused)?;
    for part in rel.split('/') {
        dir = match dir.sub(part) {
            Ok(sub) => sub,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                dir.mkdir(part).map_err(refused)?;
                dir.sub(part).map_err(refused)?
            }
            Err(e) => {
                return Err(Error::Refused(format!(
                    "{e} (no part of a library folder's path inside its root may be a symlink)"
                )));
            }
        };
    }
    let entries = dir.entries().map_err(refused)?;
    if let Some((name, _)) = entries.first() {
        return Err(Error::Refused(format!(
            "{} is not empty (it holds {name}); a library folder starts empty, and adopting a folder's files comes \
             later",
            dir.path().display(),
        )));
    }
    Ok(dir)
}

/// Whether one of two canonical folders lies inside the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Checks that `rel` in the Root `root_name` may be a Library Folder: a writable Root that is not an Ingest Folder, a
/// relative path, not a registered Library Folder's, and overlapping no folder Title Select reads or writes nor the
/// destination of a move under way. Returns it normalized.
pub(super) fn check_library_path(
    c: &Connection,
    config: &Config,
    root_name: &str,
    rel: &str,
) -> Result<String> {
    let root = config
        .root(root_name)
        .ok_or_else(|| Error::Refused(format!("no root named {} is configured", root_name)))?;
    if !root.writable {
        return Err(Error::Refused(format!(
            "root {} is read-only; a library folder needs a root with writable = true",
            root_name
        )));
    }
    if root.is_ingest() {
        return Err(Error::Refused(format!(
            "root {} is an ingest folder; a library folder needs another writable root",
            root_name
        )));
    }
    let rel = relative_path(rel)
        .map_err(|e| Error::Refused(format!("{rel}: a library folder's path {e}")))?;
    if !Path::new(&rel)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::Refused(format!(
            "{rel}: a library folder's path must be relative to its root"
        )));
    }
    let canonical_root = root.path.canonicalize().map_err(|e| {
        Error::Refused(format!("root {} ({}): {e}", root_name, root.path.display()))
    })?;
    let clash: Option<i64> = c
        .query_row(
            "SELECT id FROM library_folders WHERE root = ?1 AND path = ?2 AND unregistered_at IS NULL",
            params![root_name, rel],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(clash) = clash {
        return Err(Error::Refused(format!(
            "library folder {clash} already uses {rel} in root {}",
            root_name
        )));
    }
    // The folder's own path need not exist yet; the folder it would be is checked against every folder Title
    // Select reads or writes.
    let would_be = resolved(&canonical_root, &rel).unwrap_or_else(|| {
        rel.split('/')
            .fold(canonical_root.clone(), |p, part| p.join(part))
    });
    if let Some((what, other)) = overlapping(c, config, &would_be, Registering::LibraryFolder)? {
        return Err(Error::Refused(format!(
            "{rel} in root {} would overlap {what} at {}",
            root_name,
            other.display()
        )));
    }
    Ok(rel)
}

/// What is being registered, which overlaps nothing else Title Select reads or writes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Registering {
    /// May not overlap another one, nor a Source Set or an Ingest Folder.
    LibraryFolder,
    /// May not overlap a Library Folder or an Ingest Folder; Source Sets are checked by their own rules.
    SourceSet,
    /// May not overlap a Source Set or a Library Folder.
    IngestFolder,
}

/// The first registered folder, or destination reserved by an approved move, that `would_be` overlaps but
/// `registering` may not: what it is, and where.
pub(super) fn overlapping(
    c: &Connection,
    config: &Config,
    would_be: &Path,
    registering: Registering,
) -> Result<Option<(String, PathBuf)>> {
    // An approved move that hasn't finished holds its destination, as though the folder were there already.
    let mut stmt = c.prepare(
        "SELECT root, path, 'a source set' FROM source_sets WHERE unregistered_at IS NULL AND ?1
         UNION ALL SELECT root, path, ?2 FROM library_folders WHERE unregistered_at IS NULL
         UNION ALL SELECT root, to_path, 'a library folder being moved there' FROM library_folder_moves
         WHERE approved_at IS NOT NULL AND finished_at IS NULL",
    )?;
    let others = stmt
        .query_map(
            params![
                registering != Registering::SourceSet,
                match registering {
                    Registering::LibraryFolder => "another library folder",
                    _ => "a library folder",
                }
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let ingest = config
        .roots
        .iter()
        .filter(|(_, r)| r.is_ingest() && registering != Registering::IngestFolder)
        .map(|(name, _)| (name.clone(), String::new(), "an ingest folder".to_owned()));
    for (root, path, what) in others.into_iter().chain(ingest) {
        // A destination being moved to may not exist yet.
        let Some(other) = config.root(&root).and_then(|r| resolved(&r.path, &path)) else {
            continue;
        };
        if overlaps(would_be, &other) {
            return Ok(Some((what, other)));
        }
    }
    Ok(None)
}

/// `rel` under `base`, through whatever of it exists: its longest existing ancestor resolved, with the rest, each a
/// plain name, joined on. `None` when a part is not a plain name, or an existing part can't be resolved.
pub(super) fn resolved(base: &Path, rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| matches!(*part, "." | "..")) {
        return None;
    }
    for exists in (0..=parts.len()).rev() {
        let path = parts[..exists]
            .iter()
            .fold(base.to_path_buf(), |p, part| p.join(part));
        match path.canonicalize() {
            Ok(found) => {
                return Some(parts[exists..].iter().fold(found, |p, part| p.join(part)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}

impl Store {
    /// The path a Path Template proposes for a Library Folder of the Source Set `source_set`'s system: `template` if
    /// given, else the configured default.
    pub fn propose_library_folder_path(
        &self,
        config: &Config,
        source_set: &str,
        template: Option<&str>,
    ) -> Result<String> {
        let template = match template {
            Some(text) => PathTemplate::parse(text).map_err(|e| Error::Refused(e.to_string()))?,
            None => config.path_templates.library_folder.clone(),
        };
        let set = self.read(|c| set_by_name(c, source_set))?;
        let (provider, dat_name) = split_dat(&set.record.dat)?;
        template
            .render(&TokenValues::for_dat(provider, dat_name))
            .map_err(|e| Error::Refused(e.to_string()))
    }

    /// Checks that `rel` in the Root `root_name` may be a Library Folder: a writable Root that is not an Ingest
    /// Folder, a relative path, not a registered Library Folder's and overlapping no folder Title Select reads or
    /// writes. Returns it normalized.
    pub(super) fn check_library_path(
        &self,
        config: &Config,
        root_name: &str,
        rel: &str,
    ) -> Result<String> {
        self.read(|c| check_library_path(c, config, root_name, rel))
    }

    /// Registers a Library Folder: `add.path` (or the path its template proposes) in the writable Root `add.root`,
    /// bound to a Source Set and a Selection Profile. The folder is created if absent and must be empty if present;
    /// its marker is written before the registration commits.
    pub fn add_library_folder(
        &self,
        config: &Config,
        add: &AddLibraryFolder,
    ) -> Result<LibraryFolder> {
        let rel = match (&add.path, &add.template) {
            (Some(_), Some(_)) => {
                return Err(Error::Refused("give a path or a template, not both".into()));
            }
            (Some(path), None) => path.clone(),
            (None, template) => {
                self.propose_library_folder_path(config, &add.source_set, template.as_deref())?
            }
        };
        // Held from the check of its path until it is recorded, so that nothing registered or reserved meanwhile
        // overlaps the folders it creates.
        let _lifecycle = self.lifecycle_lock();
        let rel = self.check_library_path(config, &add.root, &rel)?;
        let root = config.root(&add.root).expect("a checked root");
        self.read(|c| {
            set_by_name(c, &add.source_set)?;
            active_profile_id(c, &add.profile)?;
            Ok(())
        })?;
        let folder = create_or_check_empty(&root.path, &rel)?;
        self.fault_point(crate::Fault::FolderRegistering);
        self.write(|tx| {
            // Again where it is recorded, so that nothing registered or reserved since is overlapped, and it is bound
            // only to a Source Set still registered and a Profile not Retired.
            check_library_path(tx, config, &add.root, &rel)?;
            let set = set_by_name(tx, &add.source_set)?;
            let profile = active_profile_id(tx, &add.profile)?;
            let store = store_id(tx)?;
            tx.execute(
                "INSERT INTO library_folders (root, path, source_set_id, profile_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![add.root, rel, set.id, profile, now()],
            )?;
            let id = tx.last_insert_rowid();
            write_marker(
                &folder,
                &Marker {
                    store,
                    folder: id,
                    generation: 0,
                },
                // A folder whose registration stopped is not registered, so nothing is ever removed from it.
                &mut |_| Ok(()),
            )?;
            record(tx, config, folder_by_id(tx, id)?)
        })
    }

    /// Rebinds a Library Folder to another Source Set of the same system, or to another Profile. Refused while a
    /// Build Job is Queued or Running for it.
    pub fn change_library_folder(
        &self,
        config: &Config,
        id: i64,
        change: &ChangeLibraryFolder,
    ) -> Result<LibraryFolder> {
        self.write(|tx| {
            let folder = folder_by_id(tx, id)?;
            folder.check_registered()?;
            if let Some(job) = folder.active_job {
                return Err(Error::Conflict(format!(
                    "library folder {id} has a build or check queued or running: job {job}"
                )));
            }
            if let Some(name) = &change.source_set {
                let set = set_by_name(tx, name)?;
                if set.dat_id != folder.dat_id {
                    return Err(Error::Refused(format!(
                        "source set {name} is for {}, but library folder {id} is for {}",
                        set.record.dat, folder.system
                    )));
                }
                tx.execute(
                    "UPDATE library_folders SET source_set_id = ?2 WHERE id = ?1",
                    params![id, set.id],
                )?;
            }
            if let Some(name) = &change.profile {
                let profile = active_profile_id(tx, name)?;
                tx.execute(
                    "UPDATE library_folders SET profile_id = ?2 WHERE id = ?1",
                    params![id, profile],
                )?;
            }
            record(tx, config, folder_by_id(tx, id)?)
        })
    }

    /// Every Library Folder, by root and path.
    pub fn library_folders(&self, config: &Config) -> Result<Vec<LibraryFolder>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{FOLDER_QUERY} WHERE f.unregistered_at IS NULL ORDER BY f.root, f.path"
            ))?;
            let rows = stmt
                .query_map([], folder_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter().map(|row| record(c, config, row)).collect()
        })
    }

    pub fn library_folder(&self, config: &Config, id: i64) -> Result<LibraryFolder> {
        self.read(|c| record(c, config, folder_by_id(c, id)?))
    }

    /// What Library Folder `id` lacks of its current Build's Selection, item by item.
    pub fn incomplete(&self, id: i64) -> Result<Incomplete> {
        self.read(|c| {
            let tx = c.unchecked_transaction()?;
            incomplete(&tx, &folder_by_id(&tx, id)?)
        })
    }

    /// Every Build of the Library Folder, newest first.
    pub fn builds(&self, folder: i64) -> Result<Vec<BuildSummary>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            builds_where(c, "b.folder_id = ?1", [folder])
        })
    }

    /// A Source Set's folder, opened through its Root without following a symlink below it; or why it cannot be.
    pub(super) fn set_folder_dir(
        &self,
        config: &Config,
        set_id: i64,
    ) -> Result<std::result::Result<FolderDir, String>> {
        let set = self.read(|c| set_by_id(c, set_id))?;
        let Some(root) = config.root(&set.record.root) else {
            return Ok(Err(format!(
                "there is no root {} in the configuration",
                set.record.root
            )));
        };
        Ok(FolderDir::open(&root.path, &set.record.path).map_err(|e| e.to_string()))
    }

    /// The configured path of a Source Set's folder.
    pub(super) fn set_folder(&self, config: &Config, set_id: i64) -> Result<(PathBuf, PathBuf)> {
        let set = self.read(|c| set_by_id(c, set_id))?;
        let resolved = resolve(config, &set.record.root, &set.record.path)?;
        Ok((resolved.path, resolved.canonical_root))
    }
}
