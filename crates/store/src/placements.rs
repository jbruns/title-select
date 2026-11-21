//! Placements (ADR 0023): where a Source Set keeps each of its Games, in its own folder or in the Library Folder that
//! holds its Placed Games. A Placed Game stays its Source Set's: its Audits read its recorded files where they are
//! placed, a Repair renames, repacks or replaces them there or keeps them aside into the Source Set's kept files, and
//! while any Game is placed neither the Library Folder nor the Source Set can be unregistered. A Build under Single
//! Copy places them (see `builds::place`); [`Store::record_placement`] records one for tests.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rustix::fs::Stat;
use title_select_api::build::{FileStat, PlacedFile};
use title_select_api::paths::relative_path;
use title_select_api::storage::is_reserved_name;
use title_select_engine::meter::Tally;

use crate::builds::sha256_of;
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use crate::folders::{folder_by_id, open_folder};
use crate::sets::{Resolved, resolve, set_by_id, set_by_name};
use crate::{Error, Result, Store, now};

/// The Library Folder holding Source Set `set`'s Placed Games: the one it names, while that is registered and bound
/// to it, or else the only registered Library Folder bound to it. None when it names none and several, or none, are
/// bound to it.
pub(crate) fn holding_folder(c: &Connection, set: i64) -> Result<Option<i64>> {
    let named: Option<i64> = c
        .query_row(
            "SELECT f.id FROM source_sets s JOIN library_folders f ON f.id = s.holding_folder_id
             WHERE s.id = ?1 AND f.source_set_id = s.id AND f.unregistered_at IS NULL",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if named.is_some() {
        return Ok(named);
    }
    let bound: Vec<i64> = c
        .prepare(
            "SELECT id FROM library_folders WHERE source_set_id = ?1 AND unregistered_at IS NULL
             ORDER BY id LIMIT 2",
        )?
        .query_map([set], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(match bound.as_slice() {
        [only] => Some(*only),
        _ => None,
    })
}

const PLACED_QUERY: &str =
    "SELECT game, folder_id, path, size, mtime_ns, dev, ino, ctime_ns, sha256,
                                   chd_roms IS NOT NULL FROM placed_files";

fn placed_from_row(r: &rusqlite::Row) -> rusqlite::Result<PlacedFile> {
    Ok(PlacedFile {
        game: r.get(0)?,
        library_folder: r.get(1)?,
        path: r.get(2)?,
        file: FileStat {
            size: r.get::<_, i64>(3)? as u64,
            mtime_ns: r.get::<_, i64>(4)? as i128,
        },
        dev: r.get::<_, i64>(5)? as u64,
        ino: r.get::<_, i64>(6)? as u64,
        ctime_ns: r.get::<_, i64>(7)? as i128,
        sha256: r.get(8)?,
        chd: r.get(9)?,
    })
}

/// Whether the file statted as `st` is the Placed Game's file `placed` as recorded: the same size and modification
/// time, and the same file, unchanged since, by its device, inode and status change time. Only then is it trusted
/// without being read again.
pub(crate) fn unchanged(placed: &PlacedFile, st: &Stat) -> bool {
    let identity = Identity::of(st);
    file_stat(st) == placed.file
        && (identity.dev, identity.ino) == (placed.dev, placed.ino)
        && ctime_ns(st) == placed.ctime_ns
}

/// Records the Placed Game's file `path` in Library Folder `folder` as statted as `now`, while it is still recorded
/// as `was`: what a check that read it again, and found it holding what it did, records.
pub(crate) fn restamp(
    tx: &Transaction,
    folder: i64,
    path: &str,
    was: &PlacedFile,
    now: &Stat,
) -> Result<()> {
    let (file, identity) = (file_stat(now), Identity::of(now));
    tx.execute(
        "UPDATE placed_files SET size = ?8, mtime_ns = ?9, dev = ?10, ino = ?11, ctime_ns = ?12
         WHERE folder_id = ?1 AND path = ?2
         AND size = ?3 AND mtime_ns = ?4 AND dev = ?5 AND ino = ?6 AND ctime_ns = ?7",
        params![
            folder,
            path,
            was.file.size as i64,
            was.file.mtime_ns as i64,
            was.dev as i64,
            was.ino as i64,
            was.ctime_ns as i64,
            file.size as i64,
            file.mtime_ns as i64,
            identity.dev as i64,
            identity.ino as i64,
            ctime_ns(now) as i64
        ],
    )?;
    Ok(())
}

/// Records the Placed Game's file `path` in Library Folder `folder`, recorded as `identity` and `file`, as statted as
/// `now` and read as `sha256`: what a return that put the file back where it was placed records, once it has read it
/// again whole there, holding what it did as it was set aside, or verified against the DAT after a rename home. Moving
/// it changed its change time, which alone stood for it while it was never read.
pub(crate) fn reread(
    tx: &Transaction,
    folder: i64,
    path: &str,
    (identity, file): (Identity, &FileStat),
    now: &Stat,
    sha256: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE placed_files SET ctime_ns = ?7, sha256 = ?8
         WHERE folder_id = ?1 AND path = ?2 AND dev = ?3 AND ino = ?4 AND size = ?5 AND mtime_ns = ?6",
        params![
            folder,
            path,
            identity.dev as i64,
            identity.ino as i64,
            file.size as i64,
            file.mtime_ns as i64,
            ctime_ns(now) as i64,
            sha256
        ],
    )?;
    Ok(())
}

/// The files of Source Set `set`'s Placed Games, by Library Folder and path.
pub(crate) fn placed_files_of(c: &Connection, set: i64) -> Result<Vec<PlacedFile>> {
    let mut stmt = c.prepare(&format!(
        "{PLACED_QUERY} WHERE source_set_id = ?1 ORDER BY folder_id, path"
    ))?;
    let rows = stmt.query_map([set], placed_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The files of the Placed Games that Library Folder `folder` holds, by path.
pub(crate) fn placed_files_in(c: &Connection, folder: i64) -> Result<Vec<PlacedFile>> {
    let mut stmt = c.prepare(&format!(
        "{PLACED_QUERY} WHERE folder_id = ?1 ORDER BY path"
    ))?;
    let rows = stmt.query_map([folder], placed_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Why Source Set `set` can't be unregistered while Games of it are placed, saying how many; none while none is.
pub(crate) fn set_hold(c: &Connection, set: i64) -> Result<Option<String>> {
    let placed: i64 = c.query_row(
        "SELECT count(*) FROM placed_games WHERE source_set_id = ?1",
        [set],
        |r| r.get(0),
    )?;
    Ok((placed > 0).then(|| {
        format!(
            "{} placed in a play library folder; they must be back in the source set's folder first",
            games(placed)
        )
    }))
}

/// Why Library Folder `folder` can't be unregistered, or bound to another Source Set, while it holds Placed Games,
/// saying how many; none while it holds none.
pub(crate) fn folder_hold(c: &Connection, folder: i64) -> Result<Option<String>> {
    let held: Option<(i64, String)> = c
        .query_row(
            "SELECT count(*), s.name FROM placed_games p JOIN source_sets s ON s.id = p.source_set_id
             WHERE p.folder_id = ?1 GROUP BY s.id",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(held.map(|(placed, set)| {
        format!(
            "it holds {} of source set {set}, placed there as their only copy; they must be back in the source \
             set's folder first",
            games(placed)
        )
    }))
}

fn games(n: i64) -> String {
    match n {
        1 => "1 game".to_owned(),
        n => format!("{n} games"),
    }
}

/// Records that the Placed Game's file `from` in Library Folder `folder` is now `to` there, statted as `now`: as a
/// Repair renamed it. It stays a file of the Game it was placed as.
pub(crate) fn move_placed(
    tx: &Transaction,
    folder: i64,
    from: &str,
    to: &str,
    now: &Stat,
) -> Result<()> {
    let (file, identity) = (file_stat(now), Identity::of(now));
    tx.execute(
        "UPDATE placed_files SET path = ?3, size = ?4, mtime_ns = ?5, dev = ?6, ino = ?7, ctime_ns = ?8
         WHERE folder_id = ?1 AND path = ?2",
        params![
            folder,
            from,
            to,
            file.size as i64,
            file.mtime_ns as i64,
            identity.dev as i64,
            identity.ino as i64,
            ctime_ns(now) as i64
        ],
    )?;
    tx.execute(
        "UPDATE review_flags SET path = ?2 WHERE path = ?1",
        params![
            title_select_api::storage::placed_path(folder, from),
            title_select_api::storage::placed_path(folder, to)
        ],
    )?;
    Ok(())
}

/// Records the file `path` in Library Folder `folder`, statted as `st` and read as `sha256`, as a file of Source Set
/// `set`'s Placed Game `game`: one a Repair wrote there.
pub(crate) fn place_file(
    tx: &Transaction,
    set: i64,
    folder: i64,
    path: &str,
    game: &str,
    st: &Stat,
    sha256: &str,
) -> Result<()> {
    place_file_as(tx, set, folder, path, game, st, Some(sha256))
}

/// As [`place_file`], for a file that may not have been read: one a Build placed by a rename, whose Audit's payload
/// Evidence stands for it.
pub(crate) fn place_file_as(
    tx: &Transaction,
    set: i64,
    folder: i64,
    path: &str,
    game: &str,
    st: &Stat,
    sha256: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO placed_games (source_set_id, game, folder_id, placed_at) VALUES (?1, ?2, ?3, ?4)",
        params![set, game, folder, now()],
    )?;
    let (file, identity) = (file_stat(st), Identity::of(st));
    tx.execute(
        "INSERT OR REPLACE INTO placed_files
         (folder_id, path, source_set_id, game, size, mtime_ns, dev, ino, ctime_ns, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            folder,
            path,
            set,
            game,
            file.size as i64,
            file.mtime_ns as i64,
            identity.dev as i64,
            identity.ino as i64,
            ctime_ns(st) as i64,
            sha256
        ],
    )?;
    Ok(())
}

/// Records the Placed Game's file `path` in Library Folder `folder`, recorded placed in this transaction, as a CHD of
/// the disc whose ROMs are `roms`: one whose header held `sha1` when extracting it gave back every track exactly, and
/// for a CD whose DAT-exact cue sheet is kept in the store as `cue`, which the blob must already be, written and synced.
pub(crate) fn place_chd(
    tx: &Transaction,
    folder: i64,
    path: &str,
    sha1: &str,
    roms: &[title_select_api::build::ExpectedRom],
    cue: Option<&str>,
) -> Result<()> {
    let roms = serde_json::to_string(roms).map_err(|e| Error::Io(e.to_string()))?;
    tx.execute(
        "UPDATE placed_files SET chd_sha1 = ?3, chd_roms = ?4, cue_sha256 = ?5 WHERE folder_id = ?1 AND path = ?2",
        params![folder, path, sha1, roms, cue],
    )?;
    Ok(())
}

/// A Placed Game's file that is a CHD, as [`place_chd`] recorded it.
#[derive(Debug, Clone)]
pub(crate) struct PlacedChd {
    pub folder: i64,
    pub path: String,
    pub game: String,
    pub sha1: String,
    pub roms: Vec<title_select_api::build::ExpectedRom>,
    pub cue: Option<String>,
    /// The SHA-256 of the file, as placed or last read whole and checked.
    pub sha256: Option<String>,
}

/// The files of Source Set `set`'s Placed Games that are CHDs, by Library Folder and path.
pub(crate) fn placed_chds_of(c: &Connection, set: i64) -> Result<Vec<PlacedChd>> {
    let mut stmt = c.prepare(
        "SELECT folder_id, path, game, chd_sha1, chd_roms, cue_sha256, sha256 FROM placed_files
         WHERE source_set_id = ?1 AND chd_roms IS NOT NULL ORDER BY folder_id, path",
    )?;
    let rows = stmt
        .query_map([set], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(folder, path, game, sha1, roms, cue, sha256)| {
            Ok(PlacedChd {
                folder,
                path,
                game,
                sha1: sha1.unwrap_or_default(),
                roms: serde_json::from_str(&roms).map_err(|e| Error::Io(e.to_string()))?,
                cue,
                sha256,
            })
        })
        .collect()
}

/// The files a Build laid out beside Source Set `set`'s placed CHDs, such as a release's `.m3u` or a disc's SBI file,
/// that are no placed file: each by its Library Folder and path there, with the release of the CHDs it is laid out
/// for. They are found by what each is recorded built for, the release or one of its discs, wherever the layout put
/// them, the system folder itself included.
pub(crate) fn laid_beside(c: &Connection, set: i64) -> Result<Vec<(i64, String, String)>> {
    use title_select_api::storage::release_of;
    let placed: std::collections::HashSet<(i64, String)> = placed_files_of(c, set)?
        .into_iter()
        .map(|p| (p.library_folder, p.path))
        .collect();
    let mut releases: std::collections::BTreeMap<i64, std::collections::BTreeSet<String>> =
        Default::default();
    for chd in placed_chds_of(c, set)? {
        releases
            .entry(chd.folder)
            .or_default()
            .insert(release_of(&chd.game));
    }
    let mut found = Vec::new();
    let mut stmt = c.prepare(
        "SELECT target, game FROM built_files WHERE folder_id = ?1 AND chd = 0 ORDER BY target",
    )?;
    for (folder, releases) in releases {
        let built = stmt
            .query_map([folder], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (target, owner) in built {
            // An `.m3u` is built for its release, an SBI file for its disc.
            let release = release_of(&owner);
            if releases.contains(&release) && !placed.contains(&(folder, target.clone())) {
                found.push((folder, target, release));
            }
        }
    }
    Ok(found)
}

/// The Game the Placed Game's file `path` in Library Folder `folder` is a file of; none when it is no such file.
pub(crate) fn placed_game_of(c: &Connection, folder: i64, path: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT game FROM placed_files WHERE folder_id = ?1 AND path = ?2",
        params![folder, path],
        |r| r.get(0),
    )
    .optional()?)
}

/// Forgets the Placed Game's file `path` in Library Folder `folder`, which a Repair moved into its Source Set's kept
/// files. The Game is no longer placed once it has no file left.
pub(crate) fn forget_placed(tx: &Transaction, folder: i64, path: &str) -> Result<()> {
    let found: Option<(i64, String)> = tx
        .query_row(
            "SELECT source_set_id, game FROM placed_files WHERE folder_id = ?1 AND path = ?2",
            params![folder, path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((set, game)) = found else {
        return Ok(());
    };
    tx.execute(
        "DELETE FROM placed_files WHERE folder_id = ?1 AND path = ?2",
        params![folder, path],
    )?;
    // A file no longer placed needs no review as one.
    clear_review_at(
        tx,
        set,
        &title_select_api::storage::placed_path(folder, path),
    )?;
    drop_unplaced(tx, set, &game)
}

/// Forgets that Source Set `set`'s Game `game` is placed once no file of it is.
fn drop_unplaced(tx: &Transaction, set: i64, game: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM placed_games WHERE source_set_id = ?1 AND game = ?2
         AND NOT EXISTS (SELECT 1 FROM placed_files f WHERE f.source_set_id = ?1 AND f.game = ?2)",
        params![set, game],
    )?;
    Ok(())
}

impl Store {
    /// The Library Folders holding Source Set `set`'s Placed Games, each with its folder resolved in its Root and the
    /// paths of the Placed Games' files in it.
    pub(crate) fn placed_folders(
        &self,
        config: &Config,
        set: i64,
    ) -> Result<Vec<(i64, Resolved, Vec<String>)>> {
        let files = self.read(|c| placed_files_of(c, set))?;
        let mut folders: Vec<(i64, Resolved, Vec<String>)> = Vec::new();
        for file in files {
            match folders.last_mut() {
                Some((folder, _, paths)) if *folder == file.library_folder => paths.push(file.path),
                _ => {
                    let row = self.read(|c| folder_by_id(c, file.library_folder))?;
                    let resolved = resolve(config, &row.root, &row.path)?;
                    folders.push((file.library_folder, resolved, vec![file.path]));
                }
            }
        }
        Ok(folders)
    }

    /// The Library Folder holding the Placed Games of Source Set `name`, if any: see [`holding_folder`].
    pub fn holding_folder(&self, name: &str) -> Result<Option<i64>> {
        self.read(|c| holding_folder(c, set_by_name(c, name)?.id))
    }

    /// Names Library Folder `folder`, which must be registered and bound to Source Set `name`, as the one holding its
    /// Placed Games; `None` names none. Refused while a Build placing its Games, or returning them, is queued or
    /// running. Changes no file: Games placed in another folder stay there until its next Build returns them, and the
    /// next Build of the folder named places them once they are home.
    pub fn set_holding_folder(&self, name: &str, folder: Option<i64>) -> Result<()> {
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            if let Some(folder) = folder {
                let row = folder_by_id(tx, folder)?;
                row.check_registered()?;
                if row.source_set_id != set.id {
                    return Err(Error::Refused(format!(
                        "play library folder {}/{} is bound to source set {}, not {name}; only a folder bound to \
                         it can hold its games",
                        row.root, row.path, row.source_set
                    )));
                }
            }
            // A Build placing its Games goes on into the folder it was approved for: the choice waits for it.
            if let Some(why) = placing_build(tx, set.id)? {
                return Err(Error::Conflict(format!(
                    "the folder holding source set {name}'s games can't change now: {why}"
                )));
            }
            tx.execute(
                "UPDATE source_sets SET holding_folder_id = ?2 WHERE id = ?1",
                params![set.id, folder],
            )?;
            Ok(())
        })
    }

    /// The files of the Placed Games of Source Set `name`, by Library Folder and path.
    pub fn placed_files(&self, name: &str) -> Result<Vec<PlacedFile>> {
        self.read(|c| placed_files_of(c, set_by_name(c, name)?.id))
    }

    /// The files of the Placed Games that Library Folder `folder` holds now, by path: each its Game's only copy.
    pub fn folder_placed_files(&self, folder: i64) -> Result<Vec<PlacedFile>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            placed_files_in(c, folder)
        })
    }

    /// The Games of Source Set `name` that a Build found need review, by name.
    pub fn review_flags(&self, name: &str) -> Result<Vec<title_select_api::report::ReviewFlag>> {
        self.read(|c| {
            let set = set_by_name(c, name)?.id;
            let mut stmt = c.prepare(
                "SELECT game, path, reason, build_id, raised_at FROM review_flags WHERE source_set_id = ?1
                 ORDER BY game",
            )?;
            let rows = stmt.query_map([set], |r| {
                Ok(title_select_api::report::ReviewFlag {
                    game: r.get(0)?,
                    path: r.get(1)?,
                    reason: r.get(2)?,
                    build: r.get(3)?,
                    raised_at: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Records Source Set `name`'s Game `game` as placed in Library Folder `folder`, the one holding its Placed Games,
    /// as the files at `paths` there, each relative to it; each is read whole to record its SHA-256. Moves no file:
    /// the Game's files must already be there. What a Build that places a Game records once it has moved it; for
    /// tests until one does.
    #[doc(hidden)]
    pub fn record_placement(
        &self,
        config: &Config,
        name: &str,
        game: &str,
        folder: i64,
        paths: &[&str],
    ) -> Result<()> {
        let (set, row) = self.read(|c| Ok((set_by_name(c, name)?, folder_by_id(c, folder)?)))?;
        row.check_registered()?;
        if paths.is_empty() {
            return Err(Error::Refused(format!(
                "{game}: a placed game has at least one file"
            )));
        }
        let dir = open_folder(config, &row.root, &row.path).map_err(Error::Refused)?;
        let mut files = Vec::new();
        for path in paths {
            let rel = relative_path(path).map_err(|e| Error::Refused(format!("{path}: {e}")))?;
            if rel.split('/').any(is_reserved_name) {
                return Err(Error::Refused(format!(
                    "{path} is under a name Title Select keeps its own work under"
                )));
            }
            files.push((rel.clone(), read(&dir, &rel)?));
        }
        self.write(|tx| {
            if holding_folder(tx, set.id)? != Some(folder) {
                return Err(Error::Refused(format!(
                    "play library folder {}/{} does not hold source set {name}'s placed games",
                    row.root, row.path
                )));
            }
            set_by_id(tx, set.id)?;
            tx.execute(
                "INSERT INTO placed_games (source_set_id, game, folder_id, placed_at) VALUES (?1, ?2, ?3, ?4)",
                params![set.id, game, folder, now()],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    Error::Conflict(format!("{game} of source set {name} is already placed"))
                }
                e => e.into(),
            })?;
            for (path, (st, sha256)) in &files {
                let (file, identity) = (file_stat(st), Identity::of(st));
                tx.execute(
                    "INSERT INTO placed_files
                     (folder_id, path, source_set_id, game, size, mtime_ns, dev, ino, ctime_ns, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        folder,
                        path,
                        set.id,
                        game,
                        file.size as i64,
                        file.mtime_ns as i64,
                        identity.dev as i64,
                        identity.ino as i64,
                        ctime_ns(st) as i64,
                        sha256
                    ],
                )
                .map_err(|e| match e {
                    rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                        Error::Conflict(format!("{path} is already a file of a placed game"))
                    }
                    e => e.into(),
                })?;
            }
            Ok(())
        })
    }
}

/// The regular file `rel` in `dir`, statted as it is, with the SHA-256 of all of it.
fn read(dir: &FolderDir, rel: &str) -> Result<(Stat, String)> {
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", dir.path().join(rel).display()));
    let file = dir.open_file(rel).map_err(io)?;
    let st = rustix::fs::fstat(&file).map_err(|e| io(e.into()))?;
    // Recorded for you, by no Job: no Job's meter counts it.
    let (sha256, _) = sha256_of(&file, &Tally::default()).map_err(io)?;
    Ok((st, sha256))
}

/// The Library Folders an Audit of Source Set `set` reads besides its own folder, or is to: the one holding its
/// Placed Games, whether or not it holds one yet, and any other that holds one of them.
fn read_folders(c: &Connection, set: i64) -> Result<Vec<i64>> {
    let mut folders: Vec<i64> = c
        .prepare("SELECT DISTINCT folder_id FROM placed_games WHERE source_set_id = ?1")?
        .query_map([set], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if let Some(holding) = holding_folder(c, set)?
        && !folders.contains(&holding)
    {
        folders.push(holding);
    }
    Ok(folders)
}

/// The Build or cleanup Job of a Library Folder holding Source Set `set`'s Placed Games, or to hold them, that is
/// queued or running: it may place them, which changes the Source Set's folder, or change the files placed there.
fn holding_build(c: &Connection, set: i64) -> Result<Option<i64>> {
    for folder in read_folders(c, set)? {
        let job: Option<i64> = c
            .query_row(
                "SELECT id FROM jobs WHERE library_folder_id = ?1 AND kind = 'build'
                 AND state IN ('queued', 'running')",
                [folder],
                |r| r.get(0),
            )
            .optional()?;
        if job.is_some() {
            return Ok(job);
        }
    }
    Ok(None)
}

/// Refuses to start an Audit of Source Set `set_name` (`set`) while a Build placing its Games, or a Build or cleanup of
/// a Library Folder holding its Placed Games, is queued or running: it would change files the Audit reads. Called in the transaction that
/// queues or starts the Audit.
pub(crate) fn check_no_holding_build(c: &Connection, set: i64, set_name: &str) -> Result<()> {
    // A Build placing its Games changes the Source Set's folder, whichever folder holds its Games now.
    if let Some(why) = placing_build(c, set)? {
        return Err(Error::Conflict(format!(
            "source set {set_name} can't be audited now: {why}"
        )));
    }
    match holding_build(c, set)? {
        Some(job) => Err(Error::Conflict(format!(
            "build job {job} of the play library folder holding source set {set_name}'s placed games is queued \
             or running; audit the source set once it ends"
        ))),
        None => Ok(()),
    }
}

/// Whether the plan `p` places Games or returns them to their Source Set's folder, so that carrying it out changes
/// the Source Set.
const CHANGES_SET: &str = "(EXISTS (SELECT 1 FROM json_each(p.content, '$.items')
                                WHERE json_extract(value, '$.kind') = 'place')
          OR coalesce(json_array_length(p.content, '$.returns'), 0) > 0)";

/// Why nothing else may change Source Set `set`'s folder now, when a Build whose plan places its Games, or returns
/// them home, is queued, running, or stopped and not yet recovered: the Source Set is locked as for a repair.
pub(crate) fn placing_build(c: &Connection, set: i64) -> Result<Option<String>> {
    let job: Option<i64> = c
        .query_row(
            &format!(
                "SELECT j.id FROM jobs j
             JOIN library_folders f ON f.id = j.library_folder_id
             JOIN plans p ON p.id = j.plan_id
             WHERE f.source_set_id = ?1 AND j.kind = 'build' AND j.state IN ('queued', 'running')
               AND {CHANGES_SET}"
            ),
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(job) = job {
        return Ok(Some(format!(
            "build job {job} places the source set's games, or returns them, and is queued or running; try again \
             once it ends"
        )));
    }
    // One that stopped part way, until recovery finishes it: its Games may be part moved.
    let build: Option<(i64, Option<String>)> = c
        .query_row(
            &format!(
                "SELECT b.id, b.recovery_error FROM builds b
             JOIN library_folders f ON f.id = b.folder_id
             JOIN plans p ON p.id = b.plan_id
             WHERE f.source_set_id = ?1 AND b.cleaned_at IS NULL
               AND {CHANGES_SET}"
            ),
            [set],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(build.map(|(build, why)| match why {
        Some(why) => format!(
            "build {build}, which places the source set's games or returns them, is unfinished: {why}"
        ),
        None => format!(
            "build {build}, which places the source set's games or returns them, is unfinished; the service \
             finishes it when it next starts"
        ),
    }))
}

/// Refuses to queue a Build or cleanup of Library Folder `folder` while it holds, or is to hold, the Placed Games of
/// a Source Set an Audit is queued for or reading: it would change files the Audit reads. Called in the transaction
/// that queues the Build.
pub(crate) fn check_no_audit_reading(c: &Connection, folder: i64) -> Result<()> {
    let set: i64 = c.query_row(
        "SELECT source_set_id FROM library_folders WHERE id = ?1",
        [folder],
        |r| r.get(0),
    )?;
    if !read_folders(c, set)?.contains(&folder) {
        return Ok(());
    }
    let audit: Option<String> = c
        .query_row(
            "SELECT 'audit job ' || id || ' of source set ' || (SELECT name FROM source_sets WHERE id = ?1)
             FROM jobs WHERE source_set_id = ?1 AND kind = 'audit' AND state IN ('queued', 'running')
             UNION ALL
             SELECT 'audit ' || id || ' of source set ' || (SELECT name FROM source_sets WHERE id = ?1)
             FROM audits WHERE source_set_id = ?1 AND status = 'running'
             LIMIT 1",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    match audit {
        Some(audit) => Err(Error::Conflict(format!(
            "{audit}, whose placed games this folder holds, is queued or running; build once it ends"
        ))),
        None => Ok(()),
    }
}

/// Records, in Source Set `set`'s Audit `audit`, that its file `from`, found as `was`, is now its Placed Game's file
/// `to`, as Audits name it ([`title_select_api::storage::placed_path`]), statted as `now`: moved by a Build that
/// didn't change it, renamed or copied and read back. Its Entries follow it with their Evidence, and so does the
/// payload read an Incremental Audit reuses, but only while it was of the file as `was` found it.
pub(crate) fn moved_entries(
    tx: &Transaction,
    (set, audit): (i64, i64),
    from: &str,
    to: &str,
    was: &title_select_api::build::PlacedOriginal,
    now: &Stat,
) -> Result<()> {
    tx.execute(
        "UPDATE audit_entries SET path = ?3 WHERE audit_id = ?1 AND path = ?2",
        params![audit, from, to],
    )?;
    let (file, identity) = (file_stat(now), Identity::of(now));
    tx.execute(
        "UPDATE OR REPLACE payload_reads SET path = ?8, dev = ?9, ino = ?10, size = ?11, mtime_ns = ?12, ctime_ns = ?13
         WHERE source_set_id = ?1 AND path = ?2 AND dev = ?3 AND ino = ?4 AND size = ?5 AND mtime_ns = ?6
           AND ctime_ns = ?7",
        params![
            set,
            from,
            was.identity.dev as i64,
            was.identity.ino as i64,
            was.file.size as i64,
            was.file.mtime_ns as i64,
            was.ctime_ns as i64,
            to,
            identity.dev as i64,
            identity.ino as i64,
            file.size as i64,
            file.mtime_ns as i64,
            ctime_ns(now) as i64
        ],
    )?;
    tx.execute(
        "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
        params![set, from],
    )?;
    Ok(())
}

/// A placed file whose Entries [`rewritten_entries`] writes: its path as an Audit names it, and the ROMs of `game` it
/// holds, as members of an archive or, `loose`, as the file itself.
pub(crate) struct Rewritten<'a> {
    pub path: &'a str,
    pub game: &'a str,
    pub roms: &'a [title_select_api::build::ExpectedRom],
    pub loose: bool,
}

/// Records, in Source Set `set`'s Audit `audit`, that its Game `game`'s files `from` were converted into, or are
/// stood in for by, its Placed Game's file `to`, holding `roms` loose or as a TorrentZip and checked to hold exactly
/// them: each ROM's Entry is now of `to`, with the Evidence its payload was matched by, and the payload reads of
/// `from` are forgotten, so that an Incremental Audit reads `to`.
pub(crate) fn rewritten_entries(
    tx: &Transaction,
    (set, audit): (i64, i64),
    from: &[&str],
    to: &[Rewritten],
) -> Result<()> {
    use title_select_api::report::{Finding, RomRef};
    let mut was = Vec::new();
    for path in from {
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM audit_entries WHERE audit_id = ?1 AND path = ?2 ORDER BY seq",
            crate::audits::ENTRY_COLUMNS
        ))?;
        let mut rows = stmt.query(params![audit, path])?;
        while let Some(row) = rows.next()? {
            was.push(crate::audits::entry_from_row(row)?);
        }
        tx.execute(
            "DELETE FROM audit_entries WHERE audit_id = ?1 AND path = ?2",
            params![audit, path],
        )?;
        tx.execute(
            "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
            params![set, path],
        )?;
    }
    let mut next: i64 = tx.query_row(
        "SELECT coalesce(max(seq), -1) + 1 FROM audit_entries WHERE audit_id = ?1",
        [audit],
        |r| r.get(0),
    )?;
    for t in to {
        let game = t.game;
        for rom in t.roms {
            let evidence = was
                .iter()
                .find(|e| e.roms.iter().any(|r| r.game == game && r.name == rom.name))
                .and_then(|e| e.evidence.clone())
                .ok_or_else(|| {
                    Error::Io(format!(
                        "{game}: the audit has no evidence for {} to carry over",
                        rom.name
                    ))
                })?;
            let roms = serde_json::to_string(&[RomRef {
                game: game.to_owned(),
                name: rom.name.clone(),
            }])
            .map_err(|e| Error::Io(e.to_string()))?;
            tx.execute(
                "INSERT INTO audit_entries (audit_id, seq, path, member, finding, roms, named_game, named_rom,
                                            evidence_level, algorithms, reason, read_at, reused, header_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, NULL, ?9, 0, ?10)",
                params![
                    audit,
                    next,
                    t.path,
                    (!t.loose).then_some(rom.name.as_str()),
                    Finding::Matched.as_str(),
                    roms,
                    evidence.level.as_str(),
                    evidence.algorithms.join(","),
                    evidence.read_at.map(|at| at.to_string()),
                    evidence.header.map(|h| h.bytes as i64)
                ],
            )?;
            next += 1;
        }
    }
    Ok(())
}

/// Records, in Source Set `set`'s Audit `audit`, that the release `r` placed in Library Folder `folder` is back in the
/// Source Set's folder as its files: each file put home whole takes the Entries of the placed file it is, with their
/// Evidence; each written from the ROMs gets an Entry for each ROM, with the Evidence its payload was matched by, where
/// the Audit has it. Every other Entry, and every payload read, of the placed files is forgotten, so that an
/// Incremental Audit reads the files home.
pub(crate) fn returned_entries(
    tx: &Transaction,
    (set, audit): (i64, i64),
    folder: i64,
    r: &title_select_api::build::PlanReturn,
) -> Result<()> {
    use title_select_api::report::{Finding, RomRef};
    use title_select_api::storage::placed_path;
    let from: Vec<String> = r
        .placed
        .iter()
        .map(|p| placed_path(folder, &p.path))
        .collect();
    let mut was = Vec::new();
    for path in &from {
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM audit_entries WHERE audit_id = ?1 AND path = ?2 ORDER BY seq",
            crate::audits::ENTRY_COLUMNS
        ))?;
        let mut rows = stmt.query(params![audit, path])?;
        while let Some(row) = rows.next()? {
            was.push(crate::audits::entry_from_row(row)?);
        }
    }
    let mut next: i64 = tx.query_row(
        "SELECT coalesce(max(seq), -1) + 1 FROM audit_entries WHERE audit_id = ?1",
        [audit],
        |r| r.get(0),
    )?;
    for f in &r.files {
        if let Some(whole) = &f.from {
            tx.execute(
                "UPDATE audit_entries SET path = ?3 WHERE audit_id = ?1 AND path = ?2",
                params![audit, placed_path(folder, whole), f.path],
            )?;
            continue;
        }
        for rom in &f.roms {
            let Some(evidence) = was
                .iter()
                .find(|e| {
                    e.finding == Finding::Matched
                        && e.roms
                            .iter()
                            .any(|r| r.game == f.game && r.name == rom.name)
                })
                .and_then(|e| e.evidence.clone())
            else {
                continue;
            };
            let roms = serde_json::to_string(&[RomRef {
                game: f.game.clone(),
                name: rom.name.clone(),
            }])
            .map_err(|e| Error::Io(e.to_string()))?;
            tx.execute(
                "INSERT INTO audit_entries (audit_id, seq, path, member, finding, roms, named_game, named_rom,
                                            evidence_level, algorithms, reason, read_at, reused, header_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, NULL, ?9, 0, ?10)",
                params![
                    audit,
                    next,
                    f.path,
                    (!f.loose).then_some(rom.name.as_str()),
                    Finding::Matched.as_str(),
                    roms,
                    evidence.level.as_str(),
                    evidence.algorithms.join(","),
                    evidence.read_at.map(|at| at.to_string()),
                    evidence.header.map(|h| h.bytes as i64)
                ],
            )?;
            next += 1;
        }
    }
    for path in &from {
        tx.execute(
            "DELETE FROM audit_entries WHERE audit_id = ?1 AND path = ?2",
            params![audit, path],
        )?;
        tx.execute(
            "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
            params![set, path],
        )?;
    }
    Ok(())
}

/// Flags Game `game` of Source Set `set` as needing review, raised by Build `build`, its placed file `path` as Audits
/// name it, and why: what a Build proved of it by extracting it. No Audit's verdict changes; the payload read an
/// Incremental Audit would reuse is forgotten, so that the next Audit reads the file again, and verifies it, or not.
pub(crate) fn raise_review(
    tx: &Transaction,
    (set, game): (i64, &str),
    path: &str,
    why: &str,
    build: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO review_flags (source_set_id, game, path, reason, build_id, raised_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![set, game, path, why, build, now()],
    )?;
    tx.execute(
        "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
        params![set, path],
    )?;
    Ok(())
}

/// The placed files of Source Set `set` flagged for review, as Audits name them.
pub(crate) fn reviewed_paths(tx: &Transaction, set: i64) -> Result<Vec<String>> {
    let mut stmt =
        tx.prepare("SELECT path FROM review_flags WHERE source_set_id = ?1 ORDER BY path")?;
    let paths = stmt
        .query_map([set], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(paths)
}

/// Clears the review flag of Source Set `set`'s placed file `path`, as Audits name it.
pub(crate) fn clear_review_at(tx: &Transaction, set: i64, path: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM review_flags WHERE source_set_id = ?1 AND path = ?2",
        params![set, path],
    )?;
    Ok(())
}

/// Clears the review flag of each of `games` of Source Set `set`, once each was verified home.
pub(crate) fn clear_reviews<'a>(
    tx: &Transaction,
    set: i64,
    games: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    for game in games {
        tx.execute(
            "DELETE FROM review_flags WHERE source_set_id = ?1 AND game = ?2",
            params![set, game],
        )?;
    }
    Ok(())
}
