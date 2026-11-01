//! The one way an entry leaves an Ingest Folder: a move an approved plan lists, recorded as a durable intent before
//! anything changes and advanced before each step, so that recovery rolls it forward from where it stopped.
//!
//! Others write to an Ingest Folder too, so every step allows for an entry being changed or swapped at any moment,
//! and each doubt ends with the data kept:
//!
//! - Within one filesystem and mount, it is one rename that never replaces anything, made only while the entry is
//!   still the file planned: the same identity, size, modification and change times, and no other links. What was
//!   moved is then read back; anything other than the file planned is put back, or kept aside when its name was
//!   taken meanwhile.
//! - Otherwise it is copied to a staged name recorded first, the copy is read back and its hash checked, and the copy
//!   is renamed into place without replacing anything. The original is deleted only when the published copy reads as
//!   exactly the ROM the move names: a file that already matches a ROM exactly is complete, so a write to it after
//!   the last check could only spoil it, never add something wanted. It is then moved into the folder's private
//!   `.deleting` area, read back there, and deleted only when it is still exactly what was copied. Every other
//!   original is kept aside in the folder's kept files, and the move says so.
//! - Into the folder's own kept files it is always a rename: kept files are never copied.
//! - A Game's ROMs, from a ZIP or a loose file, are repacked as TorrentZip, read and hashed in one pass, and the repack
//!   checked. The original is deleted, as a copy's is, only when it is a loose file whose repack holds exactly the one
//!   ROM the move names, the whole file and nothing else; every other original is kept aside in the folder's kept
//!   files.
//! - Into a Source Set, only one a repair may write to, with this store's marker.

use std::io::{self, ErrorKind, Seek, SeekFrom};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use title_select_api::build::{ExpectedRom, FileStat, kept_name};
use title_select_api::ingest::IngestMarker;
use title_select_engine::build::{FromSource, repack_sources, verify};
use title_select_engine::payload::{Checksums, Hasher};

use super::{check_ingest_marker, ingest_by_id, item_unmovable, open_ingest};
use crate::builds::sha256_of;
use crate::checks::Seen;
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, file_stat};
use crate::folders::store_id;
use crate::kept::{Scope, open_area_deleting, open_retention, unique};
use crate::plans::{json, to_json};
use crate::repairs::{MarkerRule, open_repairable};
use crate::{Error, Fault, Result, Store, now};

/// Where a move puts an entry.
#[derive(Debug, Clone)]
pub(crate) enum Destination {
    /// The folder's own kept files: `.title-select-retained/<generation>/<kept_name>`.
    Kept {
        generation: u64,
        kept_name: String,
        reason: String,
    },
    /// The file `name` in the folder `rel` of the Root `root`: a writable Root, or a Source Set a repair may write to.
    Path {
        root: String,
        rel: String,
        name: String,
        /// The kept files generation an original is kept under: of a repack, or one that changed.
        generation: u64,
    },
}

/// A move of the entry `name` of Ingest Folder `folder`, which the plan saw as `seen`.
pub(crate) struct Move<'a> {
    pub folder: i64,
    pub plan: Option<i64>,
    pub item: Option<i64>,
    pub name: &'a str,
    pub seen: Seen,
    pub sha256: Option<String>,
    pub dest: Destination,
    /// For an entry repacked as TorrentZip: each ROM and the member it is read from, or none to read the whole entry,
    /// a loose file.
    pub repack: Option<Vec<(ExpectedRom, Option<String>)>>,
    /// The ROM a copy must read as, exactly, for its original to be deleted.
    pub exact: Option<ExpectedRom>,
}

/// How a move ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Moved {
    pub kind: String,
    /// The kept file it made: the entry itself, or the original of a repack.
    pub kept: Option<i64>,
    pub dest_sha256: Option<String>,
    /// What it did besides what was planned, such as keeping an original that changed rather than deleting it.
    pub note: Option<String>,
}

#[cfg(test)]
thread_local! {
    /// Makes moves on this thread copy, as across mounts.
    pub(crate) static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn force_copy() -> bool {
    #[cfg(test)]
    {
        FORCE_COPY.with(|f| f.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

struct MoveRow {
    id: i64,
    folder: i64,
    plan: Option<i64>,
    item: Option<i64>,
    kind: String,
    state: String,
    name: String,
    seen: Seen,
    sha256: Option<String>,
    dest: Destination,
    roms: Option<Vec<(ExpectedRom, Option<String>)>>,
    exact: Option<ExpectedRom>,
    staged_name: Option<String>,
    staged: Option<Identity>,
    dest_sha256: Option<String>,
    found: Option<Identity>,
    aside_name: Option<String>,
    error: Option<String>,
}

const MOVE_QUERY: &str = "SELECT id, folder_id, plan_id, item_id, kind, state, name, dev, ino, size, mtime_ns, \
     sha256, dest_kind, dest_root, dest_rel, generation, dest_name, reason, roms, staged_name, staged_dev, \
     staged_ino, dest_sha256, ctime_ns, found_dev, found_ino, aside_name, error, exact_rom FROM ingest_moves";

fn parse_ns(r: &Row, at: usize) -> rusqlite::Result<i128> {
    r.get::<_, String>(at)?.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(at, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn identity_at(r: &Row, at: usize) -> rusqlite::Result<Option<Identity>> {
    Ok(
        match (
            r.get::<_, Option<i64>>(at)?,
            r.get::<_, Option<i64>>(at + 1)?,
        ) {
            (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
            _ => None,
        },
    )
}

fn move_from_row(r: &Row) -> rusqlite::Result<MoveRow> {
    let dest_kind: String = r.get(12)?;
    let generation = r.get::<_, Option<i64>>(15)?.unwrap_or_default() as u64;
    let dest = if dest_kind == "kept" {
        Destination::Kept {
            generation,
            kept_name: r.get(16)?,
            reason: r.get::<_, Option<String>>(17)?.unwrap_or_default(),
        }
    } else {
        Destination::Path {
            root: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
            rel: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
            name: r.get(16)?,
            generation,
        }
    };
    let roms: Option<String> = r.get(18)?;
    let exact: Option<String> = r.get(28)?;
    Ok(MoveRow {
        id: r.get(0)?,
        folder: r.get(1)?,
        plan: r.get(2)?,
        item: r.get(3)?,
        kind: r.get(4)?,
        state: r.get(5)?,
        name: r.get(6)?,
        seen: Seen {
            identity: Identity::from_sql(r.get(7)?, r.get(8)?),
            file: FileStat {
                size: r.get::<_, i64>(9)? as u64,
                mtime_ns: parse_ns(r, 10)?,
            },
            ctime_ns: parse_ns(r, 23)?,
        },
        sha256: r.get(11)?,
        dest,
        roms: match roms {
            Some(_) => Some(json(r, 18)?),
            None => None,
        },
        exact: match exact {
            Some(_) => Some(json(r, 28)?),
            None => None,
        },
        staged_name: r.get(19)?,
        staged: identity_at(r, 20)?,
        dest_sha256: r.get(22)?,
        found: identity_at(r, 24)?,
        aside_name: r.get(26)?,
        error: r.get(27)?,
    })
}

fn move_by_id(c: &Connection, id: i64) -> Result<MoveRow> {
    c.query_row(&format!("{MOVE_QUERY} WHERE id = ?1"), [id], move_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no ingest move {id}")))
}

impl MoveRow {
    fn dest_name(&self) -> &str {
        match &self.dest {
            Destination::Kept { kept_name, .. } => kept_name,
            Destination::Path { name, .. } => name,
        }
    }

    fn generation(&self) -> u64 {
        match &self.dest {
            Destination::Kept { generation, .. } | Destination::Path { generation, .. } => {
                *generation
            }
        }
    }
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `id`, with `file`'s size and
/// modification time when given.
fn is_file(dir: &FolderDir, name: &str, id: Identity, file: Option<&FileStat>) -> io::Result<bool> {
    Ok(match dir.stat(name)? {
        Some(st) => {
            FolderDir::kind(&st) == FileType::RegularFile
                && Identity::of(&st) == id
                && file.is_none_or(|f| file_stat(&st) == *f)
        }
        None => false,
    })
}

/// Whether `name` in `dir` is, without following a symlink, still exactly the regular file seen as `seen`: the same
/// identity, size, modification and change times, with no other link that could reach it.
fn is_seen(dir: &FolderDir, name: &str, seen: &Seen) -> io::Result<bool> {
    Ok(match dir.stat(name)? {
        Some(st) => {
            FolderDir::kind(&st) == FileType::RegularFile
                && st.st_nlink == 1
                && Seen::of(&st) == *seen
        }
        None => false,
    })
}

/// What an entry Title Select has just moved turned out to be.
enum Moving {
    /// The file planned, as it now is, and its SHA-256.
    Planned { file: FileStat, sha256: String },
    /// Something else: gone, or the entry `identity`.
    Other {
        identity: Option<Identity>,
        why: String,
    },
}

/// Reads back the entry `name` of `dir`, which Title Select has just moved there, to tell whether it is still the
/// file `seen` planned: the same regular file, size and modification time, with no other link, reading as `expect`
/// when given, and unchanged while it was read. Its change time is not compared with the plan's, since a rename
/// changes it on most filesystems; reading it whole, with its change time steady meanwhile, stands in.
fn read_back(dir: &FolderDir, name: &str, seen: &Seen, expect: Option<&str>) -> io::Result<Moving> {
    let Some(st) = dir.stat(name)? else {
        return Ok(Moving::Other {
            identity: None,
            why: "it is gone".into(),
        });
    };
    let identity = Identity::of(&st);
    let other = |why: &str| {
        Ok(Moving::Other {
            identity: Some(identity),
            why: why.to_owned(),
        })
    };
    if FolderDir::kind(&st) != FileType::RegularFile || identity != seen.identity {
        return other("it is not the entry the plan was made for");
    }
    if file_stat(&st) != seen.file || st.st_nlink != 1 {
        return other("it changed after the plan was made");
    }
    let file = dir.open_file(name)?;
    let before = rustix::fs::fstat(&file)?;
    let (sha256, _) = sha256_of(&file)?;
    let after = rustix::fs::fstat(&file)?;
    if Identity::of(&before) != identity
        || Seen::of(&before) != Seen::of(&after)
        || file_stat(&after) != seen.file
    {
        return other("it changed while it was read back");
    }
    if expect.is_some_and(|expect| expect != sha256) {
        return other("its content changed after the plan was made");
    }
    Ok(Moving::Planned {
        file: file_stat(&after),
        sha256,
    })
}

fn io(e: io::Error) -> Error {
    Error::Io(e.to_string())
}

type Outcome = Result<std::result::Result<Moved, String>>;

/// A kept file to record.
struct KeptRow {
    generation: u64,
    kept_name: String,
    reason: String,
    identity: Identity,
    file: FileStat,
    sha256: Option<String>,
}

fn insert_kept(tx: &Transaction, m: &MoveRow, k: &KeptRow) -> Result<i64> {
    let (dev, ino) = k.identity.to_sql();
    tx.execute(
        "INSERT INTO ingest_kept (folder_id, plan_id, item_id, generation, name, kept_name, reason, sha256, size,
                                  mtime_ns, dev, ino, kept_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            m.folder,
            m.plan,
            m.item,
            k.generation as i64,
            m.name,
            k.kept_name,
            k.reason,
            k.sha256,
            k.file.size as i64,
            k.file.mtime_ns.to_string(),
            dev,
            ino,
            now()
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// What became of a repack's original: the kept file it became, if it did, and a note on why not.
type KeptOriginal = (Option<KeptRow>, Option<String>);

impl Store {
    /// Records the intent to make `mv`, then makes it. `Err` says why it was not made, or not finished; one not
    /// finished is rolled forward when the service next starts.
    pub(crate) fn move_out(&self, config: &Config, mv: Move) -> Outcome {
        match self.record_move(config, mv, &mut |_, _| Ok(()))? {
            Ok(id) => self.drive_move(config, id),
            Err(why) => Ok(Err(why)),
        }
    }

    /// Records the intent to make `mv`, and runs `also` with its id in the same transaction, without making it.
    /// Returns its id, or why it cannot be made.
    pub(crate) fn record_move(
        &self,
        config: &Config,
        mv: Move,
        also: &mut dyn FnMut(&Transaction, i64) -> Result<()>,
    ) -> Result<std::result::Result<i64, String>> {
        let row = self.read(|c| ingest_by_id(c, mv.folder))?;
        let src = match open_ingest(config, &row.root) {
            Ok(dir) => dir,
            Err(why) => return Ok(Err(why)),
        };
        let kind = match (&mv.repack, &mv.dest) {
            (Some(_), _) => "repack",
            (None, Destination::Kept { .. }) => "rename",
            (None, Destination::Path { root, rel, .. }) => {
                let dest = match self.open_dest_folder(config, root, rel)? {
                    Ok(dest) => dest,
                    Err(why) => return Ok(Err(why)),
                };
                let same = src.identity().map_err(io)?.dev == dest.identity().map_err(io)?.dev;
                if same && !force_copy() {
                    "rename"
                } else {
                    "copy"
                }
            }
        };
        let (dev, ino) = mv.seen.identity.to_sql();
        let (dest_kind, dest_root, dest_rel, generation, dest_name, reason) = match &mv.dest {
            Destination::Kept {
                generation,
                kept_name,
                reason,
            } => ("kept", None, None, *generation, kept_name, Some(reason)),
            Destination::Path {
                root,
                rel,
                name,
                generation,
            } => ("path", Some(root), Some(rel), *generation, name, None),
        };
        let id = self.write(|tx| {
            tx.execute(
                "INSERT INTO ingest_moves (folder_id, plan_id, item_id, kind, state, name, dev, ino, size, mtime_ns,
                                           ctime_ns, sha256, dest_kind, dest_root, dest_rel, generation, dest_name,
                                           reason, roms, exact_rom, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                         ?19, ?20)",
                params![
                    mv.folder,
                    mv.plan,
                    mv.item,
                    kind,
                    mv.name,
                    dev,
                    ino,
                    mv.seen.file.size as i64,
                    mv.seen.file.mtime_ns.to_string(),
                    mv.seen.ctime_ns.to_string(),
                    mv.sha256,
                    dest_kind,
                    dest_root,
                    dest_rel,
                    generation as i64,
                    dest_name,
                    reason,
                    mv.repack.as_ref().map(to_json),
                    mv.exact.as_ref().map(to_json),
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            also(tx, id)?;
            Ok(id)
        })?;
        Ok(Ok(id))
    }

    /// Advances move `id` from the step it was recorded at to its end, after checking that its folder is open, in a
    /// writable Root, with this store's marker.
    pub(crate) fn drive_move(&self, config: &Config, id: i64) -> Outcome {
        let m = self.read(|c| move_by_id(c, id))?;
        if m.state == "done" {
            return Ok(Ok(Moved {
                kind: m.kind,
                kept: None,
                dest_sha256: m.dest_sha256,
                note: None,
            }));
        }
        let row = self.read(|c| ingest_by_id(c, m.folder))?;
        let precondition = |why: String| -> Outcome {
            // Nothing was done yet: the move is abandoned rather than left for recovery.
            if m.state == "pending" {
                return self.fail_move(m.id, why);
            }
            Ok(Err(why))
        };
        if m.state == "pending"
            && let Some(item) = m.item
            && let Some(why) =
                self.read(|c| item_unmovable(c, item, matches!(m.dest, Destination::Kept { .. })))?
        {
            return precondition(format!("{why}; it was left as it is"));
        }
        let src = match open_ingest(config, &row.root) {
            Ok(dir) => dir,
            Err(why) => return precondition(why),
        };
        let expected = IngestMarker {
            store: self.read(store_id)?,
            ingest_folder: row.id,
        };
        if let Err(why) = check_ingest_marker(&src, &expected) {
            return precondition(why);
        }
        let dest = match &m.dest {
            Destination::Kept { generation, .. } => {
                match open_retention(self, Scope::Ingest(m.folder), &src, *generation, true)? {
                    Ok(Some(retention)) => retention.generation,
                    Ok(None) => return precondition("the kept files folder is missing".into()),
                    Err(why) => return precondition(why),
                }
            }
            Destination::Path { root, rel, .. } => {
                match self.open_dest_folder(config, root, rel)? {
                    Ok(dest) => dest,
                    Err(why) => return precondition(why),
                }
            }
        };
        match m.kind.as_str() {
            "rename" => self.drive_rename(config, &m, &src, &dest),
            _ => self.drive_copy(&m, &src, &dest),
        }
    }

    fn set_state(&self, id: i64, state: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_moves SET state = ?2 WHERE id = ?1",
                params![id, state],
            )?;
            Ok(())
        })
    }

    fn fail_move(&self, id: i64, why: String) -> Outcome {
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_moves SET state = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1",
                params![id, why, now()],
            )?;
            Ok(())
        })?;
        Ok(Err(why))
    }

    /// Records move `m` done, with the kept file it made, and its item moved, in one transaction.
    fn finish_move(
        &self,
        m: &MoveRow,
        kept: Option<KeptRow>,
        dest_sha256: Option<String>,
        note: Option<String>,
    ) -> Outcome {
        let kept_id = self.write(|tx| {
            let kept_id = match &kept {
                Some(k) => Some(insert_kept(tx, m, k)?),
                None => None,
            };
            tx.execute(
                "UPDATE ingest_moves SET state = 'done', kept_id = ?2, dest_sha256 = ?3, note = ?4,
                                         finished_at = ?5
                 WHERE id = ?1",
                params![m.id, kept_id, dest_sha256, note, now()],
            )?;
            if let Some(item) = m.item {
                tx.execute(
                    "UPDATE ingest_items SET state = 'moved', changed_at = ?2 WHERE id = ?1",
                    params![item, now()],
                )?;
            }
            Ok(kept_id)
        })?;
        Ok(Ok(Moved {
            kind: m.kind.clone(),
            kept: kept_id,
            dest_sha256,
            note,
        }))
    }

    fn kept_entry(
        &self,
        m: &MoveRow,
        identity: Identity,
        file: FileStat,
        sha256: Option<String>,
    ) -> Option<KeptRow> {
        match &m.dest {
            Destination::Kept {
                generation,
                kept_name,
                reason,
            } => Some(KeptRow {
                generation: *generation,
                kept_name: kept_name.clone(),
                reason: reason.clone(),
                identity,
                file,
                sha256,
            }),
            Destination::Path { .. } => None,
        }
    }

    fn drive_rename(
        &self,
        config: &Config,
        m: &MoveRow,
        src: &FolderDir,
        dest: &FolderDir,
    ) -> Outcome {
        let dest_name = m.dest_name();
        if m.state == "restoring" {
            return self.restore(m, src, dest);
        }
        match dest.stat(dest_name).map_err(io)? {
            None => {
                if !is_seen(src, &m.name, &m.seen).map_err(io)? {
                    return self.fail_move(
                        m.id,
                        format!(
                            "{} is no longer the file the plan was made for; it was left as it is",
                            m.name
                        ),
                    );
                }
                self.set_state(m.id, "moving")?;
                self.fault_point(Fault::IngestRenaming);
                match src.rename_new(&m.name, dest, dest_name) {
                    Ok(()) => {}
                    Err(e)
                        if e.kind() == ErrorKind::CrossesDevices
                            && matches!(m.dest, Destination::Kept { .. }) =>
                    {
                        return self.fail_move(
                            m.id,
                            format!(
                                "the kept files folder is on another filesystem from {}, and kept files are never \
                                 copied; it was left as it is",
                                m.name
                            ),
                        );
                    }
                    Err(e) if e.kind() == ErrorKind::CrossesDevices => {
                        self.write(|tx| {
                            tx.execute(
                                "UPDATE ingest_moves SET kind = 'copy', state = 'pending' WHERE id = ?1",
                                [m.id],
                            )?;
                            Ok(())
                        })?;
                        return self.drive_move(config, m.id);
                    }
                    Err(e) => return self.fail_move(m.id, format!("{e}; it was left as it is")),
                }
                self.fault_point(Fault::IngestMoved);
            }
            // Something took the destination before anything was moved there.
            Some(_) if m.state == "pending" || is_seen(src, &m.name, &m.seen).map_err(io)? => {
                return self.fail_move(
                    m.id,
                    format!(
                        "{}/{dest_name} already exists; {} was left as it is",
                        dest.path().display(),
                        m.name
                    ),
                );
            }
            Some(_) => {}
        }
        dest.sync().map_err(io)?;
        src.sync().map_err(io)?;
        match read_back(dest, dest_name, &m.seen, m.sha256.as_deref()).map_err(io)? {
            Moving::Planned { file, sha256 } => {
                let kept = self.kept_entry(m, m.seen.identity, file, Some(sha256.clone()));
                self.finish_move(m, kept, Some(sha256), None)
            }
            Moving::Other {
                identity: Some(found),
                why,
            } => {
                let why = format!(
                    "what was moved for {} was not the file planned: {why}",
                    m.name
                );
                self.write(|tx| {
                    let (dev, ino) = found.to_sql();
                    tx.execute(
                        "UPDATE ingest_moves SET state = 'restoring', found_dev = ?2, found_ino = ?3, error = ?4
                         WHERE id = ?1",
                        params![m.id, dev, ino, why],
                    )?;
                    Ok(())
                })?;
                let m = self.read(|c| move_by_id(c, m.id))?;
                self.restore(&m, src, dest)
            }
            Moving::Other {
                identity: None,
                why,
            } => self.fail_move(
                m.id,
                format!(
                    "{}/{dest_name}: {why}; nothing else was changed",
                    dest.path().display()
                ),
            ),
        }
    }

    /// Puts back what a rename moved in place of the file planned: under its name in the Ingest Folder, or, when
    /// that was taken meanwhile, aside in the folder's kept files. Ends the move failed, saying where it is.
    fn restore(&self, m: &MoveRow, src: &FolderDir, dest: &FolderDir) -> Outcome {
        let why = m
            .error
            .clone()
            .unwrap_or_else(|| format!("what was moved for {} was not the file planned", m.name));
        let Some(found) = m.found else {
            return self.fail_move(m.id, why);
        };
        let holds = |dir: &FolderDir, name: &str| -> Result<bool> {
            Ok(dir
                .stat(name)
                .map_err(io)?
                .is_some_and(|st| Identity::of(&st) == found))
        };
        let dest_name = m.dest_name();
        if m.aside_name.is_none() && holds(dest, dest_name)? {
            match dest.rename_new(dest_name, src, &m.name) {
                Ok(()) => self.fault_point(Fault::IngestRestored),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return self.fail_move(
                        m.id,
                        format!(
                            "{why}; it could not be put back ({e}) and was left at {}/{dest_name}",
                            dest.path().display()
                        ),
                    );
                }
            }
        }
        // Whether this call or one before a stop renamed it, both folders are synced before the move ends.
        src.sync().map_err(io)?;
        dest.sync().map_err(io)?;
        if m.aside_name.is_none() && holds(src, &m.name)? {
            return self.fail_move(m.id, format!("{why}; it was put back as {}", m.name));
        }
        let reason = format!("found in place of {} as it was moved", m.name);
        match self.move_aside(m, src, dest, dest_name, found, &reason)? {
            Ok(aside) => self.fail_move(
                m.id,
                format!(
                    "{why}; {} was taken meanwhile, so it was kept as {aside}",
                    m.name
                ),
            ),
            Err(left) => self.fail_move(m.id, format!("{why}; {left}")),
        }
    }

    /// Sets the entry `name` of `dir`, found as `found` where the file planned should have been, aside in the
    /// folder's kept files under a free name recorded first, and records it kept. Returns the name it is kept as,
    /// or where it was left.
    fn move_aside(
        &self,
        m: &MoveRow,
        src: &FolderDir,
        dir: &FolderDir,
        name: &str,
        found: Identity,
        reason: &str,
    ) -> Result<std::result::Result<String, String>> {
        let left = || format!("it was left at {}/{name}", dir.path().display());
        let generation = m.generation();
        let retention = match open_retention(self, Scope::Ingest(m.folder), src, generation, true)?
        {
            Ok(Some(retention)) => retention.generation,
            Ok(None) | Err(_) => return Ok(Err(left())),
        };
        let aside = match &m.aside_name {
            Some(aside) => aside.clone(),
            None => {
                let mut nth = 1;
                let aside = loop {
                    let candidate = kept_name(&m.name, nth);
                    if retention.stat(&candidate).map_err(io)?.is_none() {
                        break candidate;
                    }
                    nth += 1;
                };
                self.write(|tx| {
                    tx.execute(
                        "UPDATE ingest_moves SET aside_name = ?2 WHERE id = ?1",
                        params![m.id, aside],
                    )?;
                    Ok(())
                })?;
                aside
            }
        };
        let holds = |d: &FolderDir, n: &str| -> Result<Option<rustix::fs::Stat>> {
            Ok(d.stat(n)
                .map_err(io)?
                .filter(|st| Identity::of(st) == found))
        };
        if holds(dir, name)?.is_some() {
            if let Err(e) = dir.rename_new(name, &retention, &aside) {
                return Ok(Err(format!("it could not be kept aside ({e}); {}", left())));
            }
            self.fault_point(Fault::IngestSetAside);
        }
        // Whether this call or one before a stop renamed it, both folders are synced before it is recorded kept.
        dir.sync().map_err(io)?;
        retention.sync().map_err(io)?;
        let Some(st) = holds(&retention, &aside)? else {
            return Ok(Err(left()));
        };
        let recorded = self.read(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM ingest_kept WHERE folder_id = ?1 AND generation = ?2 AND kept_name = ?3",
                params![m.folder, generation as i64, aside],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })?;
        if !recorded {
            let kept = KeptRow {
                generation,
                kept_name: aside.clone(),
                reason: reason.to_owned(),
                identity: found,
                file: file_stat(&st),
                sha256: None,
            };
            self.write(|tx| insert_kept(tx, m, &kept).map(|_| ()))?;
        }
        Ok(Ok(aside))
    }

    fn drive_copy(&self, m: &MoveRow, src: &FolderDir, dest: &FolderDir) -> Outcome {
        let dest_name = m.dest_name().to_owned();
        let mut dest_sha256 = m.dest_sha256.clone();
        if matches!(m.state.as_str(), "pending" | "moving" | "staging") {
            match self.stage_and_publish(m, src, dest, &dest_name)? {
                Ok(sha) => dest_sha256 = Some(sha),
                Err(why) => return Ok(Err(why)),
            }
            self.fault_point(Fault::IngestPublished);
        }
        let m = &self.read(|c| move_by_id(c, m.id))?;
        let Some(staged) = m.staged else {
            return Ok(Err("the published copy is not recorded".into()));
        };
        let Some(published_sha) = dest_sha256.clone() else {
            return Ok(Err("the published copy's hash is not recorded".into()));
        };
        let published = match dest.stat(&dest_name).map_err(io)? {
            Some(st) if Identity::of(&st) == staged => file_stat(&st),
            _ => {
                return Ok(Err(format!(
                    "{}/{dest_name} is no longer the copy that was published; {} was left as it is",
                    dest.path().display(),
                    m.name
                )));
            }
        };
        let delete = match (m.kind.as_str(), m.state.as_str()) {
            (_, "retaining") => false,
            (_, "disposing" | "keeping") => true,
            (kind, _) => match &m.exact {
                Some(rom) => {
                    let file = dest.open_file(&dest_name).map_err(io)?;
                    if Identity::of_fd(&file).map_err(io)? != staged {
                        return Ok(Err(format!(
                            "{}/{dest_name} is no longer the copy that was published; {} was left as it is",
                            dest.path().display(),
                            m.name
                        )));
                    }
                    if kind == "repack" {
                        exactly_repacked(m, rom, &file)
                    } else {
                        exactly(rom, &checksums_of(&file).map_err(io)?)
                    }
                }
                None => false,
            },
        };
        // What the original must still hash as to be deleted: what was copied, or, for a repack, what was read.
        let original_sha = match m.kind.as_str() {
            "repack" => m.sha256.clone(),
            _ => Some(published_sha.clone()),
        };
        let disposed = match (delete, original_sha) {
            (true, Some(original_sha)) => self
                .delete_original(m, src, &original_sha)?
                .map(|note| (None, note)),
            _ => self.keep_original(m, src)?,
        };
        let (original, note) = match disposed {
            Ok(disposed) => disposed,
            Err(why) => return Ok(Err(why)),
        };
        let kept = original.or_else(|| self.kept_entry(m, staged, published, dest_sha256.clone()));
        self.finish_move(m, kept, dest_sha256, note)
    }

    /// Writes the copy or repack of `m` under a staged name recorded first, checks it, and renames it to `dest_name`
    /// without replacing anything. Resumes from a staged file or a publish a stop interrupted. Returns the SHA-256
    /// of what was published.
    fn stage_and_publish(
        &self,
        m: &MoveRow,
        src: &FolderDir,
        dest: &FolderDir,
        dest_name: &str,
    ) -> Result<std::result::Result<String, String>> {
        if let Some(staged) = m.staged {
            if is_file(dest, dest_name, staged, None).map_err(io)? {
                dest.sync().map_err(io)?;
                let file = dest.open_file(dest_name).map_err(io)?;
                let (sha, _) = sha256_of(&file).map_err(io)?;
                self.published(m.id, &sha)?;
                return Ok(Ok(sha));
            }
            let staged_name = m.staged_name.clone().unwrap_or_default();
            if is_file(dest, &staged_name, staged, None).map_err(io)? {
                return match self.check_staged(m, src, dest, &staged_name)? {
                    Ok(sha) => self.publish(m, dest, &staged_name, dest_name, sha),
                    Err(why) => self.fail_move(m.id, why).map(|r| r.map(|_| String::new())),
                };
            }
        }
        let fail = |why: String| -> Result<std::result::Result<String, String>> {
            self.fail_move(m.id, why).map(|r| r.map(|_| String::new()))
        };
        if let Some(staged_name) = &m.staged_name
            && dest.stat(staged_name).map_err(io)?.is_some()
        {
            return fail(format!(
                "{}/{staged_name} is not the copy Title Select was writing; it and {} were left as they are",
                dest.path().display(),
                m.name
            ));
        }
        if !is_seen(src, &m.name, &m.seen).map_err(io)? {
            return fail(format!(
                "{} is no longer the file the plan was made for; it was left as it is",
                m.name
            ));
        }
        let staged_name = format!(".{dest_name}.title-select-new-{}", unique());
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_moves SET state = 'staging', staged_name = ?2, staged_dev = NULL, staged_ino = NULL
                 WHERE id = ?1",
                params![m.id, staged_name],
            )?;
            Ok(())
        })?;
        let file = match dest.create_new(&staged_name) {
            Ok(file) => file,
            Err(e) => return fail(format!("{e}; {} was left as it is", m.name)),
        };
        let (dev, ino) = Identity::of_fd(&file).map_err(io)?.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_moves SET staged_dev = ?2, staged_ino = ?3 WHERE id = ?1",
                params![m.id, dev, ino],
            )?;
            Ok(())
        })?;
        let source = match open_source(src, m) {
            Ok(source) => source,
            Err(why) => return fail(why),
        };
        let written = match &m.roms {
            Some(roms) => {
                let named: Vec<FromSource> = roms
                    .iter()
                    .map(|(rom, member)| (rom, 0, member.as_deref()))
                    .collect();
                repack_sources(&file, &named, &[&source]).map(|mut read| read.remove(0).0)
            }
            None => copy_hashing(&source, &file).map_err(|e| e.to_string()),
        };
        let source_sha = match written {
            Ok(sha) => sha,
            Err(why) => {
                return fail(format!(
                    "{why}; the partial copy and {} were left as they are",
                    m.name
                ));
            }
        };
        if m.sha256
            .as_deref()
            .is_some_and(|planned| planned != source_sha)
        {
            return fail(format!(
                "{} no longer hashes as it did when the plan was made; it was left as it is",
                m.name
            ));
        }
        let sha = match check_written(m, &file, &source_sha) {
            Ok(sha) => sha,
            Err(why) => return fail(why),
        };
        drop(file);
        if !is_seen(src, &m.name, &m.seen).map_err(io)? {
            return fail(format!(
                "{} changed while it was copied; it was left as it is",
                m.name
            ));
        }
        self.fault_point(Fault::IngestStaged);
        self.publish(m, dest, &staged_name, dest_name, sha)
    }

    /// Checks a staged file a stop left, reading the source again.
    fn check_staged(
        &self,
        m: &MoveRow,
        src: &FolderDir,
        dest: &FolderDir,
        staged_name: &str,
    ) -> Result<std::result::Result<String, String>> {
        let source = match open_source(src, m) {
            Ok(source) => source,
            Err(why) => return Ok(Err(why)),
        };
        let (source_sha, _) = sha256_of(&source).map_err(io)?;
        if m.sha256
            .as_deref()
            .is_some_and(|planned| planned != source_sha)
        {
            return Ok(Err(format!(
                "{} no longer hashes as it did when the plan was made; it was left as it is",
                m.name
            )));
        }
        let file = dest.open_file(staged_name).map_err(io)?;
        Ok(check_written(m, &file, &source_sha))
    }

    fn publish(
        &self,
        m: &MoveRow,
        dest: &FolderDir,
        staged_name: &str,
        dest_name: &str,
        sha: String,
    ) -> Result<std::result::Result<String, String>> {
        if let Err(e) = dest.rename_new(staged_name, dest, dest_name) {
            let why = format!(
                "{e}; the copy was left at {staged_name} and {} as it is",
                m.name
            );
            return self.fail_move(m.id, why).map(|r| r.map(|_| String::new()));
        }
        dest.sync().map_err(io)?;
        self.published(m.id, &sha)?;
        Ok(Ok(sha))
    }

    fn published(&self, id: i64, sha: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE ingest_moves SET state = 'published', dest_sha256 = ?2 WHERE id = ?1",
                params![id, sha],
            )?;
            Ok(())
        })
    }

    /// Opens the folder `rel` of the Root `root`, a move's destination: a Source Set there only when a repair may write
    /// to it and its marker is this store's; any other folder only in a writable Root.
    fn open_dest_folder(
        &self,
        config: &Config,
        root: &str,
        rel: &str,
    ) -> Result<std::result::Result<FolderDir, String>> {
        let set: Option<i64> = self.read(|c| {
            Ok(c.query_row(
                "SELECT id FROM source_sets WHERE root = ?1 AND path = ?2",
                params![root, rel],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        if let Some(set) = set {
            return open_repairable(self, config, set, MarkerRule::Required);
        }
        Ok(match config.root(root) {
            Some(r) if r.writable => FolderDir::open(&r.path, rel).map_err(|e| e.to_string()),
            Some(_) => Err(format!(
                "root {root} is not writable now; declare it with writable = true, then restart the service"
            )),
            None => Err(format!("there is no root {root} in the configuration")),
        })
    }

    /// Whether a scan found the item of `m` changing since the plan was made.
    fn unsettled(&self, m: &MoveRow) -> Result<bool> {
        let Some(item) = m.item else {
            return Ok(false);
        };
        let state: Option<String> = self.read(|c| {
            Ok(c.query_row(
                "SELECT state FROM ingest_items WHERE id = ?1",
                [item],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        Ok(matches!(state.as_deref(), Some("arriving" | "stale")))
    }

    /// Deletes the original of a published copy whose SHA-256 is `copied`: moved, without replacing anything, into
    /// the folder's private `.deleting` area, read back there, and deleted only if it is still exactly the file that
    /// was copied. An original that changed is kept aside instead. Returns a note of anything done besides deleting.
    fn delete_original(
        &self,
        m: &MoveRow,
        src: &FolderDir,
        copied: &str,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let deleting = match open_area_deleting(self, Scope::Ingest(m.folder), src, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => return Ok(Err("the private .deleting folder is missing".into())),
            Err(why) => return Ok(Err(why)),
        };
        let slot = format!("move-{}", m.id);
        if m.state != "keeping" {
            if is_seen(src, &m.name, &m.seen).map_err(io)? {
                if self.unsettled(m)? {
                    return Ok(Ok(Some(format!(
                        "{} changed while it was copied, so it was left in the folder",
                        m.name
                    ))));
                }
                self.set_state(m.id, "disposing")?;
                if let Err(e) = src.rename_new(&m.name, &deleting, &slot) {
                    return Ok(Err(format!(
                        "the copy is in place, but {} could not be set aside to delete: {e}; it was left as it is",
                        m.name
                    )));
                }
                self.fault_point(Fault::IngestIsolated);
            } else if m.state != "disposing" {
                return Ok(Ok(Some(format!(
                    "{} changed after it was copied, so it was left in the folder as it is",
                    m.name
                ))));
            }
            // Whether this call or one before a stop set it aside, both folders are synced before it goes on.
            src.sync().map_err(io)?;
            deleting.sync().map_err(io)?;
            let found = match read_back(&deleting, &slot, &m.seen, Some(copied)).map_err(io)? {
                Moving::Planned { .. } if !self.unsettled(m)? => {
                    deleting.unlink(&slot).map_err(io)?;
                    deleting.sync().map_err(io)?;
                    return Ok(Ok(None));
                }
                Moving::Planned { .. } => (m.seen.identity, "a scan found it changing".to_owned()),
                // Gone: deleted before a stop, which may have come before the deletion was synced.
                Moving::Other { identity: None, .. } => {
                    deleting.sync().map_err(io)?;
                    return Ok(Ok(None));
                }
                Moving::Other {
                    identity: Some(found),
                    why,
                } => (found, why),
            };
            self.write(|tx| {
                let (dev, ino) = found.0.to_sql();
                tx.execute(
                    "UPDATE ingest_moves SET state = 'keeping', found_dev = ?2, found_ino = ?3, error = ?4
                     WHERE id = ?1",
                    params![m.id, dev, ino, found.1],
                )?;
                Ok(())
            })?;
        }
        let m = self.read(|c| move_by_id(c, m.id))?;
        let (Some(found), why) = (m.found, m.error.clone().unwrap_or_default()) else {
            return Ok(Err("what was set aside to delete is not recorded".into()));
        };
        let reason = format!(
            "{} changed after it was copied; kept instead of deleted",
            m.name
        );
        Ok(
            match self.move_aside(&m, src, &deleting, &slot, found, &reason)? {
                Ok(aside) => Ok(Some(format!(
                    "{} changed after it was copied ({why}), so it was kept as {aside} instead of deleted",
                    m.name
                ))),
                Err(left) => Err(format!(
                    "{} changed after it was copied ({why}); {left}",
                    m.name
                )),
            },
        )
    }

    /// Keeps the original of a published repack, or of a published copy that may not delete it, in the folder's
    /// kept files. Returns the kept file to record and a note on anything done besides what was planned.
    fn keep_original(
        &self,
        m: &MoveRow,
        src: &FolderDir,
    ) -> Result<std::result::Result<KeptOriginal, String>> {
        let generation = m.generation();
        let retention = match open_retention(self, Scope::Ingest(m.folder), src, generation, true)?
        {
            Ok(Some(retention)) => retention,
            Ok(None) => return Ok(Err("the kept files folder is missing".into())),
            Err(why) => return Ok(Err(why)),
        };
        let (made, reason, note) = match m.kind.as_str() {
            "repack" if m.exact.is_some() => (
                "repack",
                "the original of a repack into a game's ZIP that is not exactly the ROM the plan names, so it is not \
                 deleted"
                    .to_owned(),
                Some(format!(
                    "{} was repacked, and kept rather than deleted, since only an original whose repack is exactly \
                     a ROM is deleted",
                    m.name
                )),
            ),
            "repack" => (
                "repack",
                "the original of a repack into a game's ZIP".to_owned(),
                None,
            ),
            _ => (
                "copy",
"the original of a copy that is not exactly a ROM the plan names, so it is not deleted"
                    .to_owned(),
                Some(format!(
                    "{} was copied, and kept rather than deleted, since only an original whose copy is exactly a \
                     ROM is deleted",
                    m.name
                )),
            ),
        };
        let kept = kept_name(&m.name, 1);
        if is_seen(src, &m.name, &m.seen).map_err(io)? {
            self.set_state(m.id, "retaining")?;
            if let Err(e) = src.rename_new(&m.name, &retention.generation, &kept) {
                return Ok(Err(format!(
                    "the {made} is in place, but {} could not be kept aside: {e}; it was left as it is",
                    m.name
                )));
            }
            self.fault_point(Fault::IngestSetAside);
        } else if m.state != "retaining" {
            return Ok(Ok((
                None,
                Some(format!(
                    "{} changed after it was {}, so it was left in the folder as it is",
                    m.name,
                    if made == "repack" {
                        "repacked"
                    } else {
                        "copied"
                    }
                )),
            )));
        }
        // Whether this call or one before a stop renamed it, both folders are synced before it is recorded kept.
        src.sync().map_err(io)?;
        retention.generation.sync().map_err(io)?;
        let Some(st) = retention.generation.stat(&kept).map_err(io)? else {
            return Ok(Err(format!(
                "{} is neither in the folder nor kept; the {made} is in place",
                m.name
            )));
        };
        if Identity::of(&st) != m.seen.identity {
            return Ok(Err(format!(
                "{kept} in the kept files is not {}; the {made} is in place",
                m.name
            )));
        }
        Ok(Ok((
            Some(KeptRow {
                generation,
                kept_name: kept,
                reason,
                identity: m.seen.identity,
                file: file_stat(&st),
                sha256: m.sha256.clone(),
            }),
            note,
        )))
    }
}

/// Whether `sums` are exactly those of `rom`: the same size, and every checksum the ROM lists, of which it lists one at
/// least.
fn exactly(rom: &ExpectedRom, sums: &Checksums) -> bool {
    let same = |want: &Option<String>, have: &str| {
        want.as_deref()
            .is_none_or(|want| want.eq_ignore_ascii_case(have))
    };
    rom.size == sums.size
        && (rom.crc.is_some() || rom.md5.is_some() || rom.sha1.is_some() || rom.sha256.is_some())
        && same(&rom.crc, &sums.crc)
        && same(&rom.md5, &sums.md5)
        && same(&rom.sha1, &sums.sha1)
        && same(&rom.sha256, &sums.sha256)
}

/// Whether the published repack `file` of `m` shows that its original is exactly `rom`: the original is one loose
/// file of `rom`'s size that the repack read whole, and the repack is valid TorrentZip holding exactly `rom`, its
/// payload hashed again. The original is then deleted only while it still hashes as what was read.
fn exactly_repacked(m: &MoveRow, rom: &ExpectedRom, file: &std::fs::File) -> bool {
    let loose_whole = matches!(m.roms.as_deref(), Some([(only, None)]) if only == rom);
    loose_whole
        && m.sha256.is_some()
        && m.seen.file.size == rom.size
        && (rom.crc.is_some() || rom.md5.is_some() || rom.sha1.is_some() || rom.sha256.is_some())
        && verify(file, std::slice::from_ref(rom)).is_ok()
}

fn checksums_of(mut file: &std::fs::File) -> io::Result<Checksums> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Checks the staged file `file` of `m`: a copy reads back as `source_sha`; a repack is valid TorrentZip of its ROMs.
/// Returns its SHA-256.
fn check_written(
    m: &MoveRow,
    file: &std::fs::File,
    source_sha: &str,
) -> std::result::Result<String, String> {
    file.sync_all().map_err(|e| e.to_string())?;
    let (sha, _) = sha256_of(file).map_err(|e| e.to_string())?;
    match &m.roms {
        Some(roms) => {
            let roms: Vec<ExpectedRom> = roms.iter().map(|(rom, _)| rom.clone()).collect();
            verify(file, &roms).map_err(|why| {
                format!(
                    "the repack of {} did not check: {why}; it was left as it is",
                    m.name
                )
            })?;
        }
        None if sha != source_sha => {
            return Err(format!(
                "the copy of {} did not read back the same; it was left as it is",
                m.name
            ));
        }
        None => {}
    }
    Ok(sha)
}

/// Opens the original of `m` without following a symlink, only while it is the file planned.
fn open_source(src: &FolderDir, m: &MoveRow) -> std::result::Result<std::fs::File, String> {
    let file = src.open_file(&m.name).map_err(|e| e.to_string())?;
    let st = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&st) != m.seen || st.st_nlink != 1 {
        return Err(format!(
            "{} is no longer the file the plan was made for; it was left as it is",
            m.name
        ));
    }
    Ok(file)
}

/// Copies `source`, from its start, into `dest` with `io::copy`, which on Linux uses `copy_file_range` so that the
/// filesystem may share or offload the copy. Returns the SHA-256 of `source`, read separately.
fn copy_hashing(mut source: &std::fs::File, mut dest: &std::fs::File) -> io::Result<String> {
    source.seek(SeekFrom::Start(0))?;
    io::copy(&mut source, &mut dest)?;
    Ok(sha256_of(source)?.0)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use sha2::{Digest, Sha256};

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    struct Env {
        tmp: tempfile::TempDir,
        store: Store,
        config: Config,
        folder: i64,
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["in", "lib/Atari"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\n[roots.lib]\npath = {:?}\nwritable = true\n",
            tmp.path().join("in").display().to_string(),
            tmp.path().join("lib").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let folder = store
            .register_ingest_folder(&config, "in")
            .unwrap()
            .id
            .unwrap();
        Env {
            tmp,
            store,
            config,
            folder,
        }
    }

    impl Env {
        fn put(&self, name: &str, bytes: &[u8]) {
            std::fs::write(self.tmp.path().join("in").join(name), bytes).unwrap();
        }

        fn mv<'a>(
            &self,
            name: &'a str,
            dest: Destination,
            repack: Option<Vec<(ExpectedRom, Option<String>)>>,
        ) -> Move<'a> {
            let path = self.tmp.path().join("in").join(name);
            let st = rustix::fs::lstat(&path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            Move {
                folder: self.folder,
                plan: None,
                item: None,
                name,
                seen: Seen::of(&st),
                sha256: Some(hex(&Sha256::digest(&bytes))),
                dest,
                repack,
                exact: None,
            }
        }

        /// Makes the move, stopping as a kill would at `fault`, then recovers it.
        fn interrupted(&self, mv: Move, fault: Fault) {
            self.store.inject_fault(Some(fault));
            let ran = catch_unwind(AssertUnwindSafe(|| self.store.move_out(&self.config, mv)));
            assert!(ran.is_err(), "the fault was not reached");
            self.store.inject_fault(None);
            let left = self.store.recover_ingest_moves(&self.config).unwrap();
            assert!(left.is_empty(), "{left:?}");
        }

        fn listing(&self) -> Vec<String> {
            let base = self.tmp.path();
            let mut found: Vec<String> = walkdir::WalkDir::new(base)
                .min_depth(1)
                .into_iter()
                .map(|e| e.unwrap())
                .filter(|e| e.file_type().is_file())
                .map(|e| {
                    e.path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .filter(|p| !p.starts_with("store") && !p.ends_with(".title-select"))
                .collect();
            found.sort();
            found
        }

        fn unfinished(&self) -> i64 {
            self.store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM ingest_moves WHERE state NOT IN ('done', 'failed')",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        }
    }

    fn kept(name: &str) -> Destination {
        Destination::Kept {
            generation: 1,
            kept_name: kept_name(name, 1),
            reason: "you discarded it".into(),
        }
    }

    /// The ROM `name` whose payload is `bytes`.
    fn rom_of(name: &str, bytes: &[u8]) -> ExpectedRom {
        let mut hasher = title_select_engine::payload::Hasher::new();
        hasher.update(bytes);
        let sums = hasher.finish();
        ExpectedRom {
            name: name.into(),
            size: sums.size,
            crc: Some(sums.crc),
            md5: None,
            sha1: Some(sums.sha1),
            sha256: Some(sums.sha256),
        }
    }

    fn atari(name: &str) -> Destination {
        Destination::Path {
            root: "lib".into(),
            rel: "Atari".into(),
            name: name.into(),
            generation: 1,
        }
    }

    #[test]
    fn a_rename_interrupted_after_it_moved_is_finished_by_recovery() {
        let env = env();
        env.put("pong.a26", b"pong");

        env.interrupted(
            env.mv("pong.a26", kept("pong.a26"), None),
            Fault::IngestMoved,
        );

        assert_eq!(env.listing(), ["in/.title-select-retained/1/pong.a26.kept"]);
        assert_eq!(env.unfinished(), 0);
        let kept = env.store.ingest_kept_files("in").unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kept_name, "pong.a26.kept");
    }

    #[test]
    fn a_copy_across_mounts_is_verified_published_and_its_original_deleted_even_when_interrupted() {
        FORCE_COPY.with(|f| f.set(true));
        for fault in [Fault::IngestStaged, Fault::IngestPublished] {
            let env = env();
            env.put("pong.a26", b"pong");

            let mut mv = env.mv("pong.a26", atari("Pong (USA).a26"), None);
            mv.exact = Some(rom_of("Pong (USA).a26", b"pong"));
            env.interrupted(mv, fault);

            assert_eq!(env.listing(), ["lib/Atari/Pong (USA).a26"], "{fault:?}");
            assert_eq!(
                std::fs::read(env.tmp.path().join("lib/Atari/Pong (USA).a26")).unwrap(),
                b"pong"
            );
            assert_eq!(env.unfinished(), 0);
        }
        FORCE_COPY.with(|f| f.set(false));
    }

    #[test]
    fn a_copy_of_a_file_changed_since_the_plan_is_refused_and_nothing_is_deleted() {
        FORCE_COPY.with(|f| f.set(true));
        let env = env();
        env.put("pong.a26", b"pong");
        let mut mv = env.mv("pong.a26", atari("Pong (USA).a26"), None);
        mv.sha256 = Some("0".repeat(64));

        let moved = env.store.move_out(&env.config, mv).unwrap();

        assert!(moved.is_err(), "{moved:?}");
        let listing = env.listing();
        assert!(listing.contains(&"in/pong.a26".to_owned()), "{listing:?}");
        assert!(
            !listing.contains(&"lib/Atari/Pong (USA).a26".to_owned()),
            "{listing:?}"
        );
        FORCE_COPY.with(|f| f.set(false));
    }

    #[test]
    fn a_zip_with_extra_members_is_repacked_as_torrentzip_and_its_original_kept() {
        let env = env();
        let path = env.tmp.path().join("in/pong.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("readme.txt", options).unwrap();
            zip.write_all(b"from the scene").unwrap();
            zip.start_file("pong.bin", options).unwrap();
            zip.write_all(b"pong").unwrap();
            zip.finish().unwrap();
        }
        let rom = rom_of("Pong (USA).a26", b"pong");

        env.interrupted(
            env.mv(
                "pong.zip",
                atari("Pong (USA).zip"),
                Some(vec![(rom.clone(), Some("pong.bin".into()))]),
            ),
            Fault::IngestPublished,
        );

        assert_eq!(
            env.listing(),
            [
                "in/.title-select-retained/1/pong.zip.kept",
                "lib/Atari/Pong (USA).zip"
            ]
        );
        let built = std::fs::File::open(env.tmp.path().join("lib/Atari/Pong (USA).zip")).unwrap();
        verify(&built, &[rom]).unwrap();
        let kept = env.store.ingest_kept_files("in").unwrap();
        assert_eq!(kept[0].name, "pong.zip");
    }

    /// Runs `change` on the path of the folder `in` once the move reaches `at`.
    fn when(env: &Env, at: Fault, change: impl Fn(&std::path::Path) + Send + Sync + 'static) {
        let base = env.tmp.path().join("in");
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault: Fault| {
                if fault == at {
                    change(&base);
                }
            })));
    }

    /// Overwrites the file at `path` with as many other bytes and puts its mtime back, as a writer that hides its
    /// change would.
    fn overwrite_in_place(path: &std::path::Path) {
        let before = std::fs::metadata(path).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        (&file)
            .write_all(&vec![b'x'; before.len() as usize])
            .unwrap();
        file.set_modified(before.modified().unwrap()).unwrap();
    }

    #[test]
    fn an_original_written_to_after_its_copy_was_verified_is_never_deleted() {
        FORCE_COPY.with(|f| f.set(true));
        type Change = fn(&std::path::Path);
        let appended: Change = |base| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(base.join("pong.a26"))
                .unwrap();
            file.write_all(b" and more").unwrap();
        };
        let overwritten: Change = |base| overwrite_in_place(&base.join("pong.a26"));
        let isolated_overwritten: Change = |base| {
            let slot = walkdir::WalkDir::new(base)
                .into_iter()
                .map(|e| e.unwrap())
                .find(|e| e.file_name().to_string_lossy().starts_with("move-"))
                .unwrap();
            overwrite_in_place(slot.path());
        };
        // Changed before it is set aside, it is still in the folder, and stays there; changed after, it is kept.
        for (at, change, left, kept) in [
            (
                Fault::IngestPublished,
                appended,
                &b"pong and more"[..],
                "in/pong.a26",
            ),
            (Fault::IngestPublished, overwritten, b"xxxx", "in/pong.a26"),
            (
                Fault::IngestIsolated,
                isolated_overwritten,
                b"xxxx",
                "in/.title-select-retained/1/pong.a26.kept",
            ),
        ] {
            let env = env();
            env.put("pong.a26", b"pong");
            when(&env, at, change);

            let mut mv = env.mv("pong.a26", atari("Pong (USA).a26"), None);
            mv.exact = Some(rom_of("Pong (USA).a26", b"pong"));
            let moved = env.store.move_out(&env.config, mv).unwrap().unwrap();

            assert_eq!(env.listing(), [kept, "lib/Atari/Pong (USA).a26"], "{at:?}");
            assert_eq!(std::fs::read(env.tmp.path().join(kept)).unwrap(), left);
            assert_eq!(
                std::fs::read(env.tmp.path().join("lib/Atari/Pong (USA).a26")).unwrap(),
                b"pong"
            );
            assert!(moved.note.is_some_and(|n| n.contains("changed")));
        }
        FORCE_COPY.with(|f| f.set(false));
    }

    #[test]
    fn an_entry_swapped_in_just_before_a_rename_is_put_back_and_the_move_fails() {
        for dir in [false, true] {
            let env = env();
            env.put("pong.a26", b"pong");
            when(&env, Fault::IngestRenaming, move |base| {
                std::fs::rename(base.join("pong.a26"), base.join("pong.a26.orig")).unwrap();
                match dir {
                    true => std::fs::create_dir(base.join("pong.a26")).unwrap(),
                    false => std::fs::write(base.join("pong.a26"), b"gnop").unwrap(),
                }
            });

            let moved = env
                .store
                .move_out(&env.config, env.mv("pong.a26", kept("pong.a26"), None))
                .unwrap();

            assert!(moved.is_err_and(|why| why.contains("put back")), "{dir}");
            assert_eq!(env.tmp.path().join("in/pong.a26").is_dir(), dir);
            assert!(env.tmp.path().join("in/pong.a26.orig").exists());
            assert!(env.store.ingest_kept_files("in").unwrap().is_empty());
            assert_eq!(env.unfinished(), 0);
        }
    }

    #[test]
    fn an_entry_swapped_in_whose_name_was_taken_again_is_kept_aside() {
        let env = env();
        env.put("pong.a26", b"pong");
        let moving = env.mv("pong.a26", kept("pong.a26"), None);
        let base = env.tmp.path().join("in");
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault: Fault| match fault {
                Fault::IngestRenaming => {
                    std::fs::rename(base.join("pong.a26"), base.join("pong.a26.orig")).unwrap();
                    std::fs::write(base.join("pong.a26"), b"gnop").unwrap();
                }
                Fault::IngestMoved => std::fs::write(base.join("pong.a26"), b"newer").unwrap(),
                _ => {}
            })));

        let moved = env.store.move_out(&env.config, moving).unwrap();

        assert!(moved.is_err_and(|why| why.contains("kept as")));
        let listing = env.listing();
        assert_eq!(
            listing,
            [
                "in/.title-select-retained/1/pong.a26.2.kept",
                "in/pong.a26",
                "in/pong.a26.orig"
            ],
            "{listing:?}"
        );
        assert_eq!(
            std::fs::read(env.tmp.path().join("in/pong.a26")).unwrap(),
            b"newer"
        );
    }

    #[test]
    fn a_copy_that_is_not_exactly_a_rom_keeps_its_original_and_never_deletes_it() {
        FORCE_COPY.with(|f| f.set(true));
        let mut other = rom_of("Pong (USA).a26", b"gnop");
        other.sha256 = None;
        for exact in [None, Some(other)] {
            let env = env();
            env.put("pong.a26", b"pong");
            let mut mv = env.mv("pong.a26", atari("Pong (USA).a26"), None);
            mv.exact = exact.clone();

            let moved = env.store.move_out(&env.config, mv).unwrap().unwrap();

            assert_eq!(
                env.listing(),
                [
                    "in/.title-select-retained/1/pong.a26.kept",
                    "lib/Atari/Pong (USA).a26"
                ],
                "{exact:?}"
            );
            assert!(
                moved
                    .note
                    .is_some_and(|n| n.contains("kept rather than deleted")),
                "{exact:?}"
            );
            let kept = env.store.ingest_kept_files("in").unwrap();
            assert_eq!(kept.len(), 1, "{exact:?}");
            assert_eq!(kept[0].name, "pong.a26");
        }
        FORCE_COPY.with(|f| f.set(false));
    }

    /// The state, error and note of the last move.
    fn last_move(env: &Env) -> (String, Option<String>, Option<String>) {
        env.store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT state, error, note FROM ingest_moves ORDER BY id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap()
    }

    /// Makes `mv` with `hook` run at each fault point, stopping as a kill would at `stop`, then recovers it. Returns
    /// each directory recovery synced, as the path below the test's directory.
    fn synced_on_recovery(
        env: &Env,
        mv: Move,
        stop: Fault,
        hook: fn(&std::path::Path, Fault),
    ) -> Vec<String> {
        let base = env.tmp.path().join("in");
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault: Fault| {
                hook(&base, fault);
                if fault == stop {
                    panic!("stopped at {fault:?}");
                }
            })));
        let ran = catch_unwind(AssertUnwindSafe(|| env.store.move_out(&env.config, mv)));
        assert!(ran.is_err(), "{stop:?} was not reached");
        env.store.hook_faults(None);
        crate::folder_dir::SYNCED.with(|synced| synced.borrow_mut().clear());
        env.store.recover_ingest_moves(&env.config).unwrap();
        assert_eq!(env.unfinished(), 0, "{stop:?}");
        let tmp = env.tmp.path().to_string_lossy().into_owned();
        crate::folder_dir::SYNCED.with(|synced| {
            synced
                .borrow()
                .iter()
                .map(|p| {
                    let p = p.to_string_lossy();
                    p.strip_prefix(&tmp)
                        .unwrap_or(&p)
                        .trim_start_matches('/')
                        .to_owned()
                })
                .collect()
        })
    }

    fn swap(base: &std::path::Path, fault: Fault) {
        if fault == Fault::IngestRenaming {
            std::fs::rename(base.join("pong.a26"), base.join("pong.a26.orig")).unwrap();
            std::fs::write(base.join("pong.a26"), b"gnop").unwrap();
        }
    }

    #[test]
    fn a_stop_after_any_rename_ends_the_move_only_once_both_folders_are_synced() {
        const KEPT: &str = "in/.title-select-retained/1";
        const DELETING: &str = "in/.title-select-retained/.deleting";
        type Case = (
            &'static str,
            Fault,
            fn(&std::path::Path, Fault),
            &'static [&'static str],
            &'static str,
        );
        let cases: [Case; 7] = [
            (
                "rename",
                Fault::IngestMoved,
                |_, _| {},
                &["in", KEPT],
                "done",
            ),
            (
                "rename put back",
                Fault::IngestRestored,
                swap,
                &["in", KEPT],
                "failed",
            ),
            (
                "rename kept aside",
                Fault::IngestSetAside,
                |base, fault| {
                    swap(base, fault);
                    if fault == Fault::IngestMoved {
                        std::fs::write(base.join("pong.a26"), b"newer").unwrap();
                    }
                },
                &[KEPT],
                "failed",
            ),
            (
                "copy deleted",
                Fault::IngestIsolated,
                |_, _| {},
                &["in", DELETING],
                "done",
            ),
            (
                "copy kept when changed",
                Fault::IngestSetAside,
                |base, fault| {
                    if fault == Fault::IngestIsolated {
                        let slot = base.join(".title-select-retained/.deleting/move-1");
                        overwrite_in_place(&slot);
                    }
                },
                &[DELETING, KEPT],
                "done",
            ),
            (
                "copy not exactly a rom",
                Fault::IngestSetAside,
                |_, _| {},
                &["in", KEPT],
                "done",
            ),
            (
                "repack",
                Fault::IngestSetAside,
                |_, _| {},
                &["in", KEPT],
                "done",
            ),
        ];
        for (case, stop, hook, dirs, state) in cases {
            let env = env();
            let mut mv = match case {
                "repack" => {
                    let path = env.tmp.path().join("in/pong.zip");
                    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
                    let options = zip::write::SimpleFileOptions::default();
                    zip.start_file("readme.txt", options).unwrap();
                    zip.write_all(b"from the scene").unwrap();
                    zip.start_file("pong.bin", options).unwrap();
                    zip.write_all(b"pong").unwrap();
                    zip.finish().unwrap();
                    let roms = vec![(rom_of("Pong (USA).a26", b"pong"), "pong.bin".into())];
                    env.mv("pong.zip", atari("Pong (USA).zip"), Some(roms))
                }
                _ if case.starts_with("rename") => {
                    env.put("pong.a26", b"pong");
                    env.mv("pong.a26", kept("pong.a26"), None)
                }
                _ => {
                    env.put("pong.a26", b"pong");
                    env.mv("pong.a26", atari("Pong (USA).a26"), None)
                }
            };
            if matches!(case, "copy deleted" | "copy kept when changed") {
                mv.exact = Some(rom_of("Pong (USA).a26", b"pong"));
            }
            FORCE_COPY.with(|f| f.set(case.starts_with("copy")));

            let synced = synced_on_recovery(&env, mv, stop, hook);

            FORCE_COPY.with(|f| f.set(false));
            for dir in dirs {
                assert!(
                    synced.iter().any(|s| s == dir),
                    "{case}: {dir} not in {synced:?}"
                );
            }
            let (ended, error, note) = last_move(&env);
            assert_eq!(ended, state, "{case}: {error:?} {note:?}");
            let listing = env.listing();
            match case {
                "copy deleted" => assert_eq!(listing, ["lib/Atari/Pong (USA).a26"], "{case}"),
                "rename" => assert_eq!(listing, [format!("{KEPT}/pong.a26.kept")], "{case}"),
                _ => assert!(
                    listing
                        .iter()
                        .any(|p| p.starts_with(KEPT) || p == "in/pong.a26"),
                    "{case}: {listing:?}"
                ),
            }
        }
    }
}
