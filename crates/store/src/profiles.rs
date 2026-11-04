//! Named Selection Profiles, each with a linear history of immutable versions (ADR 0001). A version is the
//! Profile's canonical JSON and its SHA-256; saving a Profile identical to its newest version creates none.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};
use title_select_api::selection::SelectionProfile;
use title_select_api::{
    Job, ProfileRecord, ProfileVersion, SavedProfile, StartSelectionFor, Workers,
};

use super::{Error, Result, Store, now};

const VERSION_QUERY: &str =
    "SELECT p.name, v.version, v.sha256, v.saved_at, v.saved_by, v.json, v.id
     FROM profile_versions v JOIN profiles p ON p.id = v.profile_id";

fn version_from_row(row: &Row) -> rusqlite::Result<(i64, ProfileVersion)> {
    let json: String = row.get(5)?;
    let profile = serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok((
        row.get(6)?,
        ProfileVersion {
            name: row.get(0)?,
            version: row.get(1)?,
            sha256: row.get(2)?,
            saved_at: row.get(3)?,
            saved_by: row.get(4)?,
            profile,
        },
    ))
}

/// The version `version` of the Profile `name`, or its newest, with its row id.
pub(super) fn version_row(
    c: &Connection,
    name: &str,
    version: Option<u32>,
) -> Result<(i64, ProfileVersion)> {
    let profile_id = profile_id(c, name)?;
    let found = match version {
        Some(v) => c
            .query_row(
                &format!("{VERSION_QUERY} WHERE v.profile_id = ?1 AND v.version = ?2"),
                params![profile_id, v],
                version_from_row,
            )
            .optional()?,
        None => newest(c, profile_id)?,
    };
    found.ok_or_else(|| match version {
        Some(v) => Error::NotFound(format!("selection profile {name} has no version {v}")),
        None => Error::NotFound(format!("selection profile {name} has no saved version")),
    })
}

fn newest(c: &Connection, profile_id: i64) -> Result<Option<(i64, ProfileVersion)>> {
    Ok(c.query_row(
        &format!("{VERSION_QUERY} WHERE v.profile_id = ?1 ORDER BY v.version DESC LIMIT 1"),
        [profile_id],
        version_from_row,
    )
    .optional()?)
}

/// The Profile `name`, refused while it is Retired: a Retired Profile is hidden from new choices.
pub(super) fn active_profile_id(c: &Connection, name: &str) -> Result<i64> {
    let id = profile_id(c, name)?;
    let retired: bool = c.query_row(
        "SELECT retired_at IS NOT NULL FROM profiles WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    if retired {
        return Err(Error::Refused(format!(
            "selection profile {name} is retired; restore it first"
        )));
    }
    Ok(id)
}

pub(super) fn profile_id(c: &Connection, name: &str) -> Result<i64> {
    c.query_row("SELECT id FROM profiles WHERE name = ?1", [name], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no selection profile named {name}")))
}

fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.trim() != name
        || name.chars().count() > 100
        || name.chars().any(char::is_control)
        || name.contains('/')
    {
        return Err(Error::Refused(format!(
            "{name:?} can't be a selection profile name: use 1 to 100 characters, with no slash and no space at \
             either end"
        )));
    }
    Ok(())
}

fn create(tx: &Transaction, name: &str) -> Result<i64> {
    check_name(name)?;
    tx.execute(
        "INSERT INTO profiles (name, created_at) VALUES (?1, ?2)",
        params![name, now()],
    )?;
    Ok(tx.last_insert_rowid())
}

fn record(c: &Connection, id: i64, name: String, created_at: String) -> Result<ProfileRecord> {
    Ok(ProfileRecord {
        name,
        created_at,
        latest: newest(c, id)?.map(|(_, v)| v),
        retired_at: c.query_row("SELECT retired_at FROM profiles WHERE id = ?1", [id], |r| {
            r.get(0)
        })?,
    })
}

/// SHA-256 of the Profile's canonical JSON, and that JSON.
pub fn canonical(profile: &SelectionProfile) -> (String, String) {
    let json = profile.canonical_json();
    let sha = Sha256::digest(json.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (sha, json)
}

impl Store {
    /// Creates the Profile `name` with no versions. Refused if one has that name.
    pub fn create_profile(&self, name: &str) -> Result<ProfileRecord> {
        self.write(|tx| {
            if profile_id(tx, name).is_ok() {
                return Err(Error::Conflict(format!(
                    "a selection profile named {name} already exists"
                )));
            }
            let id = create(tx, name)?;
            let created_at: String =
                tx.query_row("SELECT created_at FROM profiles WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            record(tx, id, name.to_owned(), created_at)
        })
    }

    /// Every Profile, by name, each with its newest version.
    pub fn profiles(&self) -> Result<Vec<ProfileRecord>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT id, name, created_at FROM profiles ORDER BY name")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<(i64, String, String)>, _>>()?;
            rows.into_iter()
                .map(|(id, name, created_at)| record(c, id, name, created_at))
                .collect()
        })
    }

    pub fn profile(&self, name: &str) -> Result<ProfileRecord> {
        self.read(|c| {
            let id = profile_id(c, name)?;
            let created_at: String =
                c.query_row("SELECT created_at FROM profiles WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            record(c, id, name.to_owned(), created_at)
        })
    }

    /// Every version of the Profile, newest first.
    pub fn profile_versions(&self, name: &str) -> Result<Vec<ProfileVersion>> {
        self.read(|c| {
            let id = profile_id(c, name)?;
            let mut stmt = c.prepare(&format!(
                "{VERSION_QUERY} WHERE v.profile_id = ?1 ORDER BY v.version DESC"
            ))?;
            let rows = stmt.query_map([id], version_from_row)?;
            Ok(rows.map(|r| r.map(|(_, v)| v)).collect::<Result<_, _>>()?)
        })
    }

    /// The version `version` of the Profile, or its newest.
    pub fn profile_version(&self, name: &str, version: Option<u32>) -> Result<ProfileVersion> {
        self.read(|c| version_row(c, name, version).map(|(_, v)| v))
    }

    /// Saves `profile` as the next version of the Profile `name`, creating the Profile if it has none, and queues a
    /// Selection with that version for each of `queue`, in one transaction. A Profile identical to the newest
    /// version creates no version; the Selections are then queued with the newest. Refused as a conflict when
    /// `base_version` is not the newest version, so that a save never silently overwrites a newer one.
    pub fn save_profile(
        &self,
        name: &str,
        profile: &SelectionProfile,
        base_version: Option<u32>,
        actor: &str,
        queue: &[StartSelectionFor],
        workers: Workers,
    ) -> Result<SavedProfile> {
        let (sha256, json) = canonical(profile);
        self.write(|tx| {
            let id = match active_profile_id(tx, name) {
                Ok(id) => id,
                Err(Error::NotFound(_)) => create(tx, name)?,
                Err(err) => return Err(err),
            };
            let latest = newest(tx, id)?;
            let latest_version = latest.as_ref().map(|(_, v)| v.version);
            let unchanged = latest.as_ref().is_some_and(|(_, v)| v.sha256 == sha256);
            if base_version != latest_version && !unchanged {
                return Err(Error::Conflict(match latest_version {
                    Some(v) => format!(
                        "selection profile {name} is at version {v} now; reload it and make your changes again"
                    ),
                    None => format!("selection profile {name} has no saved version yet"),
                }));
            }
            let (version_id, version, created) = match latest {
                Some((vid, v)) if unchanged => (vid, v, false),
                _ => {
                    let next = latest_version.unwrap_or(0) + 1;
                    tx.execute(
                        "INSERT INTO profile_versions (profile_id, version, json, sha256, saved_at, saved_by)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![id, next, json, sha256, now(), actor],
                    )?;
                    let (vid, v) = version_row(tx, name, Some(next))?;
                    (vid, v, true)
                }
            };
            let jobs = queue
                .iter()
                .map(|start| {
                    super::selections::enqueue(
                        tx,
                        &start.source_set,
                        start.revision.as_deref(),
                        version_id,
                        actor,
                        workers,
                    )
                })
                .collect::<Result<Vec<Job>>>()?;
            Ok(SavedProfile {
                version,
                created,
                jobs,
            })
        })
    }

    /// Retires the Profile `name`: hidden from new choices, its versions kept for the Selections made with them.
    /// Refused while a registered Library Folder is bound to it.
    pub fn retire_profile(&self, name: &str) -> Result<ProfileRecord> {
        self.write(|tx| {
            let id = profile_id(tx, name)?;
            let mut stmt = tx.prepare(
                "SELECT root || '/' || path FROM library_folders
                 WHERE profile_id = ?1 AND unregistered_at IS NULL ORDER BY root, path",
            )?;
            let bound = stmt
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if !bound.is_empty() {
                return Err(Error::Conflict(format!(
                    "selection profile {name} is still chosen by play library folder {}; choose another profile \
                     for it, or unregister it, first",
                    bound.join(", ")
                )));
            }
            tx.execute(
                "UPDATE profiles SET retired_at = coalesce(retired_at, ?2) WHERE id = ?1",
                params![id, now()],
            )?;
            let created_at: String =
                tx.query_row("SELECT created_at FROM profiles WHERE id = ?1", [id], |r| r.get(0))?;
            record(tx, id, name.to_owned(), created_at)
        })
    }

    /// Restores the Retired Profile `name`, so it can be chosen again.
    pub fn restore_profile(&self, name: &str) -> Result<ProfileRecord> {
        self.write(|tx| {
            let id = profile_id(tx, name)?;
            tx.execute("UPDATE profiles SET retired_at = NULL WHERE id = ?1", [id])?;
            let created_at: String =
                tx.query_row("SELECT created_at FROM profiles WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            record(tx, id, name.to_owned(), created_at)
        })
    }
}
