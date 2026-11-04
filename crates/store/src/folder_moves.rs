//! Moving or renaming a Library Folder within its Root: a plan the Operator approves by its digest, carried out as
//! one exclusive rename of the folder itself, so nothing in it is copied and its kept files and marker go with it.
//! The move is recorded as approved before the rename, so a stop part-way is settled at the next start from where
//! the folder's identity is found.

use std::io::ErrorKind;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use title_select_api::lifecycle::{FolderMove, PlanFolderMove, RegistrationKind};

use super::folder_dir::{FolderDir, Identity};
use super::folders::{
    check_library_path, folder_by_id, marker_problem, open_writable_folder, store_id,
};
use super::lifecycle::{holds_of, library_folder_work};
use super::{Error, Result, Store, now};
use crate::config::Config;

/// A move as stored, with the identity of the folder it moves.
struct Row {
    plan: FolderMove,
    identity: Identity,
}

fn move_by_id(c: &Connection, id: i64) -> Result<Row> {
    c.query_row(
        "SELECT id, folder_id, root, from_path, to_path, digest, created_at, approved_by, approved_at, finished_at,
                outcome, dev, ino
         FROM library_folder_moves WHERE id = ?1",
        [id],
        |r| {
            Ok(Row {
                plan: FolderMove {
                    id: r.get(0)?,
                    folder: r.get(1)?,
                    root: r.get(2)?,
                    from_path: r.get(3)?,
                    to_path: r.get(4)?,
                    digest: r.get(5)?,
                    created_at: r.get(6)?,
                    approved_by: r.get(7)?,
                    approved_at: r.get(8)?,
                    finished_at: r.get(9)?,
                    outcome: r.get(10)?,
                },
                identity: Identity::from_sql(r.get(11)?, r.get(12)?),
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no move {id} of a play library folder")))
}

fn digest(folder: i64, root: &str, from: &str, to: &str, identity: Identity) -> String {
    let bytes = serde_json::to_vec(&(folder, root, from, to, identity.to_sql()))
        .expect("a move serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `rel` split into its parent's path and its own name.
fn split(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

/// Opens the folder `rel` under `root`, creating each missing component.
fn open_creating(root: FolderDir, rel: &str) -> std::io::Result<FolderDir> {
    let mut dir = root;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        dir = match dir.sub(part) {
            Ok(sub) => sub,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                dir.mkdir(part)?;
                dir.sub(part)?
            }
            Err(e) => return Err(e),
        };
    }
    Ok(dir)
}

/// The identity of the folder `rel` in the Root `root`, if it can be opened.
fn identity_at(config: &Config, root: &str, rel: &str) -> Option<Identity> {
    open_writable_folder(config, root, rel)
        .ok()
        .and_then(|dir| dir.identity().ok())
}

fn finish(c: &Connection, id: i64, outcome: &str) -> Result<()> {
    c.execute(
        "UPDATE library_folder_moves SET finished_at = ?2, outcome = ?3 WHERE id = ?1",
        params![id, now(), outcome],
    )?;
    Ok(())
}

impl Store {
    /// Plans moving or renaming Library Folder `id` to `to.path` in the same Root. Nothing is moved until the plan
    /// is approved; it is refused while anything holds the folder, or where a new Library Folder could not be.
    pub fn plan_library_folder_move(
        &self,
        config: &Config,
        id: i64,
        to: &PlanFolderMove,
    ) -> Result<FolderMove> {
        let (row, store) = self.read(|c| {
            let row = folder_by_id(c, id)?;
            row.check_registered()?;
            let holds = holds_of(c, RegistrationKind::LibraryFolder, id)?;
            if !holds.is_empty() {
                return Err(Error::Conflict(format!(
                    "play library folder {id} can't be moved yet: {}",
                    holds.join("; ")
                )));
            }
            Ok((row, store_id(c)?))
        })?;
        let to_path = self.check_library_path(config, &row.root, &to.path)?;
        let dir = open_writable_folder(config, &row.root, &row.path).map_err(Error::Refused)?;
        if let Some(problem) = marker_problem(&dir, &store, id, row.generation, true) {
            return Err(Error::Refused(format!(
                "{}: {problem}; nothing was moved",
                dir.path().display()
            )));
        }
        let identity = dir.identity().map_err(|e| Error::Io(e.to_string()))?;
        let (parent, name) = split(&to_path);
        if let Ok(parent) = open_writable_folder(config, &row.root, parent)
            && parent
                .stat(name)
                .map_err(|e| Error::Io(e.to_string()))?
                .is_some()
        {
            return Err(Error::Refused(format!(
                "{to_path} is already there in root {}; choose a path that doesn't exist yet",
                row.root
            )));
        }
        let digest = digest(id, &row.root, &row.path, &to_path, identity);
        self.write(|tx| {
            let (dev, ino) = identity.to_sql();
            tx.execute(
                "INSERT INTO library_folder_moves (folder_id, root, from_path, to_path, dev, ino, digest, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, row.root, row.path, to_path, dev, ino, digest, now()],
            )?;
            Ok(move_by_id(tx, tx.last_insert_rowid())?.plan)
        })
    }

    /// The move `id` of a Library Folder.
    pub fn library_folder_move(&self, id: i64) -> Result<FolderMove> {
        self.read(|c| Ok(move_by_id(c, id)?.plan))
    }

    /// Approves and carries out move `id` if `digest` is still its digest: the folder is renamed, never copied,
    /// to its new path, creating the folders above it, and its registration follows it.
    pub fn approve_library_folder_move(
        &self,
        config: &Config,
        id: i64,
        digest: &str,
        actor: &str,
    ) -> Result<FolderMove> {
        // Reserving the destination is serialized with registering Library Folders, which check it.
        let _lifecycle = self.lifecycle_lock();
        let planned = self.write(|tx| {
            let row = move_by_id(tx, id)?;
            if row.plan.approved_at.is_some() {
                return Err(Error::Conflict(format!(
                    "move {id} was already approved; plan the move again"
                )));
            }
            if row.plan.digest != digest {
                return Err(Error::Conflict(format!(
                    "move {id} isn't the plan you reviewed; review it again"
                )));
            }
            let folder = folder_by_id(tx, row.plan.folder)?;
            folder.check_registered()?;
            if folder.root != row.plan.root || folder.path != row.plan.from_path {
                return Err(Error::Conflict(format!(
                    "play library folder {} is no longer at {}; plan the move again",
                    folder.id, row.plan.from_path
                )));
            }
            let holds = holds_of(tx, RegistrationKind::LibraryFolder, folder.id)?;
            if !holds.is_empty() {
                return Err(Error::Conflict(format!(
                    "play library folder {} can't be moved yet: {}",
                    folder.id,
                    holds.join("; ")
                )));
            }
            // What was registered since the plan may overlap where it goes; approving reserves the destination.
            check_library_path(tx, config, &row.plan.root, &row.plan.to_path).map_err(
                |e| match e {
                    Error::Refused(why) => Error::Conflict(format!("{why}; plan the move again")),
                    other => other,
                },
            )?;
            tx.execute(
                "UPDATE library_folder_moves SET approved_by = ?2, approved_at = ?3 WHERE id = ?1",
                params![id, actor, now()],
            )?;
            Ok(row)
        })?;
        let plan = &planned.plan;
        let carried = self.write(|tx| {
            let work = library_folder_work(tx, plan.folder)?;
            if !work.is_empty() {
                return Ok(Err(format!(
                    "the folder was busy, so it was left where it is: {}",
                    work.join("; ")
                )));
            }
            match self.rename_folder(config, plan, planned.identity) {
                Ok(()) => {
                    tx.execute(
                        "UPDATE library_folders SET path = ?2 WHERE id = ?1",
                        params![plan.folder, plan.to_path],
                    )?;
                    finish(tx, id, "moved")?;
                    Ok(Ok(()))
                }
                Err(reason) => Ok(Err(reason)),
            }
        })?;
        if let Err(reason) = carried {
            self.write(|tx| finish(tx, id, &format!("failed: {reason}")))?;
            return Err(Error::Refused(format!(
                "play library folder {} was not moved: {reason}",
                plan.folder
            )));
        }
        self.library_folder_move(id)
    }

    /// Renames the folder of `plan`, checked to still be the one with `identity`, to its new path.
    fn rename_folder(
        &self,
        config: &Config,
        plan: &FolderMove,
        identity: Identity,
    ) -> std::result::Result<(), String> {
        let (from_parent, from_name) = split(&plan.from_path);
        let (to_parent, to_name) = split(&plan.to_path);
        let found = open_writable_folder(config, &plan.root, &plan.from_path)?
            .identity()
            .map_err(|e| e.to_string())?;
        if found != identity {
            return Err(format!(
                "{} is not the folder that was planned to move; it was left as it is",
                plan.from_path
            ));
        }
        let source = open_writable_folder(config, &plan.root, from_parent)?;
        let root = open_writable_folder(config, &plan.root, "")?;
        let dest = open_creating(root, to_parent).map_err(|e| e.to_string())?;
        source
            .rename_new(from_name, &dest, to_name)
            .map_err(|e| e.to_string())?;
        let _ = source.sync();
        let _ = dest.sync();
        Ok(())
    }

    /// Run when the service starts: settles each approved move of a Library Folder the service stopped while
    /// carrying out, from where the folder is found by its identity. Returns, for each move left unsettled because
    /// the folder is at neither path, the move and why; it holds its folder until a later start settles it.
    pub fn recover_library_folder_moves(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let unfinished: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM library_folder_moves WHERE approved_at IS NOT NULL AND finished_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut left = Vec::new();
        for id in unfinished {
            let row = self.read(|c| move_by_id(c, id))?;
            let plan = &row.plan;
            if identity_at(config, &plan.root, &plan.to_path) == Some(row.identity) {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE library_folders SET path = ?2 WHERE id = ?1",
                        params![plan.folder, plan.to_path],
                    )?;
                    finish(tx, id, "moved")
                })?;
            } else if identity_at(config, &plan.root, &plan.from_path) == Some(row.identity) {
                self.write(|tx| {
                    finish(
                        tx,
                        id,
                        "failed: the service stopped before the folder was moved, so it is where it was",
                    )
                })?;
            } else {
                left.push((
                    id,
                    format!(
                        "play library folder {} is found at neither {} nor {} in root {}",
                        plan.folder, plan.from_path, plan.to_path, plan.root
                    ),
                ));
            }
        }
        Ok(left)
    }
}
