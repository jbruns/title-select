//! The Source Set registry: each Source Set is a Root plus a relative path, bound to a DAT.

use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::dats::split_dat;
use super::{Error, Result, Store, now};
use crate::config::Config;
use title_select_api::paths::{PathTemplate, TokenValues, relative_path};

pub use title_select_api::{LastVerified, SourceSetRecord};

pub(super) struct SourceSetRow {
    pub id: i64,
    pub dat_id: i64,
    pub record: SourceSetRecord,
}

const SET_QUERY: &str =
    "SELECT s.id, s.dat_id, s.name, s.root, s.path, d.provider || '/' || d.name, s.added_at,
            r.sha256, r.version, a.finished_at
     FROM source_sets s
     JOIN dats d ON d.id = s.dat_id
     LEFT JOIN audits a ON a.id = (SELECT id FROM audits
                                  WHERE source_set_id = s.id AND status = 'finished'
                                  ORDER BY finish_seq DESC LIMIT 1)
     LEFT JOIN dat_revisions r ON r.id = a.dat_revision_id";

fn set_from_row(row: &Row) -> rusqlite::Result<SourceSetRow> {
    let sha256: Option<String> = row.get(7)?;
    let last_verified_against = match sha256 {
        Some(sha256) => Some(LastVerified {
            sha256,
            version: row.get(8)?,
            finished_at: row.get(9)?,
        }),
        None => None,
    };
    Ok(SourceSetRow {
        id: row.get(0)?,
        dat_id: row.get(1)?,
        record: SourceSetRecord {
            name: row.get(2)?,
            root: row.get(3)?,
            path: row.get(4)?,
            dat: row.get(5)?,
            added_at: row.get(6)?,
            last_verified_against,
        },
    })
}

pub(super) fn set_by_name(c: &Connection, name: &str) -> Result<SourceSetRow> {
    c.query_row(
        &format!("{SET_QUERY} WHERE s.name = ?1"),
        [name],
        set_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no source set named {name}")))
}

pub(super) fn set_by_id(c: &Connection, id: i64) -> Result<SourceSetRow> {
    c.query_row(&format!("{SET_QUERY} WHERE s.id = ?1"), [id], set_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no source set {id}")))
}

/// A Source Set's folder, checked to lie inside its Root.
pub(super) struct Resolved {
    /// Relative to the Root, with `/` separators.
    pub rel: String,
    /// The Root's path, as configured, joined with `rel`.
    pub path: PathBuf,
    /// The Root's canonical path, which every file an Audit reads must resolve inside.
    pub canonical_root: PathBuf,
}

/// Checks that `rel` names a directory inside the Root called `root`.
///
/// Refuses an absolute path, `..`, and a path that, after following symlinks, resolves outside the Root.
pub(super) fn resolve(config: &Config, root: &str, rel: &str) -> Result<Resolved> {
    let root_config = config
        .root(root)
        .ok_or_else(|| Error::Refused(format!("no root named {root} is configured")))?;
    let rel = relative_path(rel)
        .map_err(|e| Error::Refused(format!("{rel}: a source set's path {e}")))?;
    // Defence in depth: the Host's own reading of the path must agree that every component is a plain folder name.
    if !Path::new(&rel)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::Refused(format!(
            "{rel}: a source set's path must be relative to its root"
        )));
    }
    let canonical_root = root_config.path.canonicalize().map_err(|e| {
        Error::Refused(format!("root {root} ({}): {e}", root_config.path.display()))
    })?;
    let configured = rel
        .split('/')
        .fold(root_config.path.clone(), |p, part| p.join(part));
    let canonical = configured
        .canonicalize()
        .map_err(|e| Error::Refused(format!("{}: {e}", configured.display())))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Error::Refused(format!(
            "{rel}: resolves outside root {root}"
        )));
    }
    if !canonical.is_dir() {
        return Err(Error::Refused(format!(
            "{}: not a directory",
            configured.display()
        )));
    }
    Ok(Resolved {
        rel,
        path: configured,
        canonical_root,
    })
}

impl Store {
    /// The path a Path Template proposes for a Source Set bound to `dat` (`<provider>/<name>`): `template` if given,
    /// else the configured default. The path is not checked against any Root: registering it does that.
    pub fn propose_source_set_path(
        &self,
        config: &Config,
        dat: &str,
        template: Option<&str>,
    ) -> Result<String> {
        let template = match template {
            Some(text) => PathTemplate::parse(text).map_err(|e| Error::Refused(e.to_string()))?,
            None => config.path_templates.source_set.clone(),
        };
        let (provider, dat_name) = split_dat(dat)?;
        self.read(|c| {
            c.query_row(
                "SELECT 1 FROM dats WHERE provider = ?1 AND name = ?2",
                params![provider, dat_name],
                |_| Ok(()),
            )
            .optional()?
            .ok_or_else(|| Error::Refused(format!("no DAT {dat} has been imported")))
        })?;
        template
            .render(&TokenValues::for_dat(provider, dat_name))
            .map_err(|e| Error::Refused(e.to_string()))
    }

    /// Registers the folder `rel` in the Root `root` as a Source Set bound to `dat` (`<provider>/<name>`).
    pub fn add_source_set(
        &self,
        config: &Config,
        root: &str,
        rel: &str,
        dat: &str,
    ) -> Result<SourceSetRecord> {
        if config.root(root).is_some_and(|r| r.writable) {
            return Err(Error::Refused(format!(
                "root {root} is writable, and only library folders go in a writable root; add the source set from a \
                 read-only root"
            )));
        }
        let rel = resolve(config, root, rel)?.rel;
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
        let (provider, dat_name) = split_dat(dat)?;
        self.write(|tx| {
            let dat_id: i64 = tx
                .query_row(
                    "SELECT id FROM dats WHERE provider = ?1 AND name = ?2",
                    params![provider, dat_name],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| Error::Refused(format!("no DAT {dat} has been imported")))?;
            let clash: Option<String> = tx
                .query_row(
                    "SELECT name FROM source_sets WHERE name = ?1 OR (root = ?2 AND path = ?3)",
                    params![name, root, rel],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(clash) = clash {
                return Err(Error::Refused(format!(
                    "a source set named {clash} already uses that name or folder"
                )));
            }
            tx.execute(
                "INSERT INTO source_sets (name, root, path, dat_id, added_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, root, rel, dat_id, now()],
            )?;
            Ok(set_by_name(tx, &name)?.record)
        })
    }

    /// Every registered Source Set, by name.
    pub fn source_sets(&self) -> Result<Vec<SourceSetRecord>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{SET_QUERY} ORDER BY s.name"))?;
            let rows = stmt.query_map([], |r| set_from_row(r).map(|s| s.record))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    pub fn source_set(&self, name: &str) -> Result<SourceSetRecord> {
        self.read(|c| set_by_name(c, name).map(|s| s.record))
    }
}
