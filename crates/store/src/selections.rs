//! Selections (ADR 0001): Jobs that run one Profile version on a Source Set's DAT Revision, and the explained
//! Selections they record, release by release. A kept Selection holds its DAT Revision's blob (ADR 0003).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use title_select_api::selection::{
    Alternative, CoverageDrift, Exclusion, GroupChoice, ReleaseRow, Role, Rule, Selection,
    SelectionGroup, SelectionPins, SelectionProfile, SelectionSummary, Winner,
};
use title_select_api::{
    Availability, DatRevisionRecord, GameStatus, Job, JobKind, JobState, ProfileVersionRef,
    SelectionRecord, SelectionReleaseRow, Workers,
};

use super::audits::{check_blob_present, choose_revision};
use super::dats::{REVISION_COLUMNS, revision_from_row};
use super::jobs::{check_no_active, finish_running, job_by_id};
use super::profiles::version_row;
use super::sets::set_by_name;
use super::{Error, Result, Store, now};
use crate::config::Config;

/// What a Running Selection Job needs to run the engine.
#[derive(Debug, Clone)]
pub struct SelectionInput {
    pub job: i64,
    pub profile: SelectionProfile,
    pub dat_revision: DatRevisionRecord,
    /// The DAT Revision's blob.
    pub dat: PathBuf,
}

/// Queues a Selection of the Source Set `name` with the Profile version `version_id`, pinned now to a DAT Revision
/// chosen as for an Audit. Refused while the Source Set has a Selection Job Queued or Running.
pub(super) fn enqueue(
    tx: &Transaction,
    name: &str,
    revision: Option<&str>,
    version_id: i64,
    actor: &str,
    workers: Workers,
) -> Result<Job> {
    let set = set_by_name(tx, name)?;
    check_no_active(tx, set.id, &set.record.name, JobKind::Selection)?;
    let (revision_id, chosen) = choose_revision(tx, &set, revision)?;
    check_blob_present(&chosen)?;
    tx.execute(
        "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, profile_version_id, actor, readers,
                           hashers, created_at)
         VALUES ('queued', 'selection', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            set.id,
            revision_id,
            version_id,
            actor,
            workers.readers as i64,
            workers.hashers as i64,
            now()
        ],
    )?;
    job_by_id(tx, tx.last_insert_rowid())
}

fn revision_by_id(c: &Connection, id: i64) -> Result<DatRevisionRecord> {
    Ok(c.query_row(
        &format!(
            "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id WHERE r.id = ?1"
        ),
        [id],
        revision_from_row,
    )?
    .1)
}

const SELECTION_QUERY: &str = "SELECT sel.id, s.name, r.sha256, r.version, p.name, v.version,
            (SELECT max(version) FROM profile_versions WHERE profile_id = p.id),
            sel.job_id, sel.system, sel.system_settings, sel.pins, sel.summary, sel.created_at,
            sel.pruned_at IS NOT NULL
     FROM selections sel
     JOIN source_sets s ON s.id = sel.source_set_id
     JOIN dat_revisions r ON r.id = sel.dat_revision_id
     JOIN profile_versions v ON v.id = sel.profile_version_id
     JOIN profiles p ON p.id = v.profile_id";

fn json<T: serde::de::DeserializeOwned>(row: &Row, i: usize) -> rusqlite::Result<T> {
    let text: String = row.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn record_from_row(row: &Row) -> rusqlite::Result<SelectionRecord> {
    Ok(SelectionRecord {
        id: row.get(0)?,
        source_set: row.get(1)?,
        dat_revision_sha256: row.get(2)?,
        dat_revision_version: row.get(3)?,
        profile: ProfileVersionRef {
            name: row.get(4)?,
            version: row.get(5)?,
        },
        latest_profile_version: row.get(6)?,
        job: row.get(7)?,
        system: row.get(8)?,
        system_settings: row.get(9)?,
        pins: json(row, 10)?,
        summary: json(row, 11)?,
        created_at: row.get(12)?,
        pruned: row.get(13)?,
    })
}

pub(super) fn record_by_id(c: &Connection, id: i64) -> Result<SelectionRecord> {
    c.query_row(
        &format!("{SELECTION_QUERY} WHERE sel.id = ?1"),
        [id],
        record_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no selection {id}")))
}

/// Refuses a Selection whose releases were pruned.
fn check_full(c: &Connection, id: i64) -> Result<SelectionRecord> {
    let record = record_by_id(c, id)?;
    if record.pruned {
        return Err(Error::Refused(format!(
            "selection {id} was pruned; only its summary is kept"
        )));
    }
    Ok(record)
}

fn release_from_row(row: &Row) -> rusqlite::Result<ReleaseRow> {
    let role: String = row.get(1)?;
    Ok(ReleaseRow {
        group: row.get(0)?,
        role: Role::parse(&role).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown role {role}").into(),
            )
        })?,
        title: row.get(2)?,
        clone_of: row.get(3)?,
        reason: row.get(4)?,
        rule: Rule {
            stage: row.get(5)?,
            reference: row.get(6)?,
            message: row.get(7)?,
        },
        unresolved: row.get(8)?,
        from_choice: row.get(9)?,
    })
}

const RELEASE_COLUMNS: &str =
    "rel.group_name, rel.role, rel.title, rel.clone_of, rel.reason, rel.stage, rel.ref, rel.message,
     rel.unresolved, rel.from_choice, rel.seq";

/// The Source Set's latest finished, full Audit against the Selection's DAT Revision.
fn availability_audit(c: &Connection, selection: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT a.id FROM audits a JOIN selections sel
           ON sel.source_set_id = a.source_set_id AND sel.dat_revision_id = a.dat_revision_id
         WHERE sel.id = ?1 AND a.status = 'finished' AND a.pruned_at IS NULL
         ORDER BY a.finish_seq DESC LIMIT 1",
        [selection],
        |r| r.get(0),
    )
    .optional()?)
}

fn write_selection(
    tx: &Transaction,
    set_id: i64,
    revision_id: i64,
    version_id: i64,
    job: i64,
    selection: &Selection,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO selections (source_set_id, dat_revision_id, profile_version_id, job_id, system,
                                 system_settings, pins, summary, coverage_drift, stale_choices, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            set_id,
            revision_id,
            version_id,
            job,
            selection.system,
            selection.system_settings,
            to_json(&selection.pins),
            to_json(&selection.summary),
            to_json(&selection.coverage_drift),
            to_json(&selection.stale_choices),
            now()
        ],
    )?;
    let id = tx.last_insert_rowid();
    let mut groups = tx.prepare(
        "INSERT INTO selection_groups (selection_id, seq, name, unresolved, drift, choice)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (seq, g) in selection.groups.iter().enumerate() {
        groups.execute(params![
            id,
            seq as i64,
            g.group,
            g.unresolved,
            g.drift,
            g.choice.as_ref().map(to_json),
        ])?;
    }
    let mut releases = tx.prepare(
        "INSERT INTO selection_releases (selection_id, seq, group_name, role, title, clone_of, reason, stage,
                                         ref, message, unresolved, from_choice)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    for (seq, r) in selection.rows().iter().enumerate() {
        releases.execute(params![
            id,
            seq as i64,
            r.group,
            r.role.as_str(),
            r.title,
            r.clone_of,
            r.reason,
            r.rule.stage,
            r.rule.reference,
            r.rule.message,
            r.unresolved,
            r.from_choice,
        ])?;
    }
    Ok(id)
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("store values always serialize")
}

pub(super) fn read_rows(c: &Connection, id: i64) -> Result<Vec<ReleaseRow>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {RELEASE_COLUMNS} FROM selection_releases rel WHERE rel.selection_id = ?1 ORDER BY rel.seq"
    ))?;
    let rows = stmt.query_map([id], release_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn read_selection(c: &Connection, id: i64) -> Result<Selection> {
    check_full(c, id)?;
    let (dat_sha256, system, system_settings, pins, summary, coverage_drift, stale_choices) = c
        .query_row(
            "SELECT r.sha256, sel.system, sel.system_settings, sel.pins, sel.summary, sel.coverage_drift,
                    sel.stale_choices
             FROM selections sel JOIN dat_revisions r ON r.id = sel.dat_revision_id WHERE sel.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    json::<SelectionPins>(r, 3)?,
                    json::<SelectionSummary>(r, 4)?,
                    json::<CoverageDrift>(r, 5)?,
                    json::<BTreeMap<String, BTreeSet<String>>>(r, 6)?,
                ))
            },
        )?;
    let mut stmt = c.prepare(
        "SELECT name, unresolved, drift, choice FROM selection_groups WHERE selection_id = ?1 ORDER BY seq",
    )?;
    let mut groups: Vec<SelectionGroup> = stmt
        .query_map([id], |r| {
            let choice: Option<String> = r.get(3)?;
            let choice: Option<GroupChoice> = match choice {
                Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?),
                None => None,
            };
            Ok(SelectionGroup {
                group: r.get(0)?,
                unresolved: r.get(1)?,
                drift: r.get(2)?,
                choice,
                winners: Vec::new(),
                alternatives: Vec::new(),
                exclusions: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?;
    let index: BTreeMap<String, usize> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.group.clone(), i))
        .collect();
    for row in read_rows(c, id)? {
        let Some(&i) = index.get(&row.group) else {
            return Err(Error::Io(format!(
                "selection {id}: release {} names no group",
                row.title
            )));
        };
        let group = &mut groups[i];
        match row.role {
            Role::Winner => group.winners.push(Winner {
                title: row.title,
                rule: row.rule,
                unresolved: row.unresolved,
            }),
            Role::Alternative => group.alternatives.push(Alternative {
                title: row.title,
                clone_of: row.clone_of.unwrap_or_default(),
                rule: row.rule,
            }),
            Role::Exclusion => group.exclusions.push(Exclusion {
                title: row.title,
                rule: row.rule,
                reason: row.reason.unwrap_or_default(),
            }),
        }
    }
    Ok(Selection {
        pins,
        dat_sha256,
        system,
        system_settings,
        summary,
        coverage_drift,
        groups,
        stale_choices,
    })
}

impl Store {
    /// Queues a Selection of the Source Set `name` with the version `version` (by default the newest) of the
    /// Profile `profile`.
    pub fn enqueue_selection(
        &self,
        name: &str,
        revision: Option<&str>,
        profile: &str,
        version: Option<u32>,
        actor: &str,
        workers: Workers,
    ) -> Result<Job> {
        self.write(|tx| {
            let (version_id, _) = version_row(tx, profile, version)?;
            enqueue(tx, name, revision, version_id, actor, workers)
        })
    }

    /// The Profile and DAT blob the Running Selection Job `id` runs.
    pub fn selection_input(&self, id: i64) -> Result<SelectionInput> {
        self.read(|c| {
            let (kind, state, revision_id, profile_json): (String, String, i64, Option<String>) = c
                .query_row(
                    "SELECT j.kind, j.state, j.dat_revision_id, v.json
                     FROM jobs j LEFT JOIN profile_versions v ON v.id = j.profile_version_id
                     WHERE j.id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("no job {id}")))?;
            if kind != JobKind::Selection.as_str() {
                return Err(Error::Refused(format!("job {id} is not a selection")));
            }
            if state != JobState::Running.as_str() {
                return Err(Error::Refused(format!("job {id} is {state}, not running")));
            }
            let profile = serde_json::from_str(&profile_json.unwrap_or_default())
                .map_err(|e| Error::Io(format!("job {id}'s selection profile: {e}")))?;
            let dat_revision = revision_by_id(c, revision_id)?;
            check_blob_present(&dat_revision)?;
            Ok(SelectionInput {
                job: id,
                profile,
                dat: self.blob_path(&dat_revision.sha256),
                dat_revision,
            })
        })
    }

    /// Records the Selection the Running Selection Job `id` made and marks the Job Succeeded, in one transaction;
    /// then prunes the Source Set's Selections and drops unheld DAT blobs. Returns the Job as it ends.
    pub fn record_selection(&self, config: &Config, id: i64, selection: &Selection) -> Result<Job> {
        let set_id = self.write(|tx| {
            let (set_id, revision_id, version_id): (i64, i64, i64) = tx.query_row(
                "SELECT source_set_id, dat_revision_id, profile_version_id FROM jobs
                 WHERE id = ?1 AND kind = 'selection'",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let revision = revision_by_id(tx, revision_id)?;
            if revision.sha256 != selection.dat_sha256 {
                return Err(Error::Io(format!(
                    "job {id}: the selection was made from another DAT than revision {}",
                    revision.sha256
                )));
            }
            let selection_id = write_selection(tx, set_id, revision_id, version_id, id, selection)?;
            finish_running(tx, id, JobState::Succeeded, None)?;
            tx.execute(
                "UPDATE jobs SET selection_id = ?2 WHERE id = ?1",
                params![id, selection_id],
            )?;
            Ok(set_id)
        });
        let set_id = match set_id {
            Ok(set_id) => set_id,
            Err(err) => {
                let _ = self.fail_job(id, &err.to_string());
                return Err(err);
            }
        };
        self.prune_selections(set_id, config.selections.keep)?;
        self.retain_blobs()?;
        self.job(id)
    }

    /// Every Selection made for the Source Set, newest first.
    pub fn selections(&self, source_set: &str) -> Result<Vec<SelectionRecord>> {
        self.read(|c| {
            let set = set_by_name(c, source_set)?;
            let mut stmt = c.prepare(&format!(
                "{SELECTION_QUERY} WHERE sel.source_set_id = ?1 ORDER BY sel.id DESC"
            ))?;
            let rows = stmt.query_map([set.id], record_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    pub fn selection_record(&self, id: i64) -> Result<SelectionRecord> {
        self.read(|c| record_by_id(c, id))
    }

    /// The explained Selection, as the engine and the Operator's choices made it.
    pub fn selection(&self, id: i64) -> Result<Selection> {
        // One read transaction, so a prune between the queries cannot leave the record without its groups.
        self.read(|c| {
            let tx = c.unchecked_transaction()?;
            read_selection(&tx, id)
        })
    }

    /// Every release of the Selection, in order.
    pub fn selection_rows(&self, id: i64) -> Result<Vec<ReleaseRow>> {
        self.read(|c| {
            let tx = c.unchecked_transaction()?;
            check_full(&tx, id)?;
            read_rows(&tx, id)
        })
    }

    /// The latest full Selection for the Source Set against the DAT Revision with this SHA-256.
    pub fn latest_selection(&self, source_set: &str, sha256: &str) -> Result<Option<i64>> {
        self.read(|c| {
            let set = set_by_name(c, source_set)?;
            Ok(c.query_row(
                "SELECT sel.id FROM selections sel JOIN dat_revisions r ON r.id = sel.dat_revision_id
                 WHERE sel.source_set_id = ?1 AND r.sha256 = ?2 AND sel.pruned_at IS NULL
                 ORDER BY sel.id DESC LIMIT 1",
                params![set.id, sha256],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// Up to `limit` of the Selection's releases after the one at `after`, optionally only those with `role`, or
    /// in the group `group`, each with its availability.
    pub fn selection_releases(
        &self,
        id: i64,
        after: Option<u64>,
        limit: usize,
        role: Option<Role>,
        group: Option<&str>,
    ) -> Result<(Vec<SelectionReleaseRow>, Option<u64>)> {
        self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            check_full(&tx, id)?;
            let audit = availability_audit(&tx, id)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {RELEASE_COLUMNS},
                        g.status, g.claimed_roms <> '[]'
                 FROM selection_releases rel
                 LEFT JOIN audit_games g ON g.audit_id = ?5 AND g.name = rel.title
                 WHERE rel.selection_id = ?1 AND rel.seq > ?2 AND (?3 IS NULL OR rel.role = ?3)
                   AND (?6 IS NULL OR rel.group_name = ?6)
                 ORDER BY rel.seq LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                id,
                after.map_or(-1, |a| a as i64),
                role.map(Role::as_str),
                limit as i64 + 1,
                audit,
                group
            ])?;
            let mut items = Vec::new();
            while let Some(row) = rows.next()? {
                let status: Option<String> = row.get(11)?;
                items.push(SelectionReleaseRow {
                    seq: row.get::<_, i64>(10)? as u64,
                    release: release_from_row(row)?,
                    availability: status.as_deref().and_then(GameStatus::parse),
                    claimed: row.get::<_, Option<bool>>(12)?.unwrap_or(false),
                });
            }
            let more = items.len() > limit;
            items.truncate(limit);
            let next = if more {
                items.last().map(|r| r.seq)
            } else {
                None
            };
            Ok((items, next))
        })
    }

    /// The Game Statuses of the Source Set's latest full Audit against the Selection's DAT Revision.
    pub fn selection_availability(&self, id: i64) -> Result<Availability> {
        self.read(|c| {
            record_by_id(c, id)?;
            let audit = availability_audit(c, id)?;
            let mut games = BTreeMap::new();
            let mut claimed = BTreeSet::new();
            if let Some(audit) = audit {
                let mut stmt = c.prepare(
                    "SELECT name, status, claimed_roms <> '[]' FROM audit_games WHERE audit_id = ?1",
                )?;
                let mut rows = stmt.query([audit])?;
                while let Some(row) = rows.next()? {
                    let name: String = row.get(0)?;
                    let status: String = row.get(1)?;
                    if row.get(2)? {
                        claimed.insert(name.clone());
                    }
                    if let Some(status) = GameStatus::parse(&status) {
                        games.insert(name, status);
                    }
                }
            }
            Ok(Availability {
                audit,
                games,
                claimed,
            })
        })
    }

    /// Prunes the Source Set's Selections beyond the latest `keep` to their records, except any cited as
    /// provenance, such as by a Managed Folder's current Build. The latest is always kept.
    fn prune_selections(&self, set_id: i64, keep: usize) -> Result<Vec<i64>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, EXISTS (SELECT 1 FROM selection_citations c WHERE c.selection_id = sel.id)
                 FROM selections sel WHERE source_set_id = ?1 AND pruned_at IS NULL ORDER BY id DESC",
            )?;
            let full = stmt
                .query_map([set_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let pruned: Vec<i64> = full
                .into_iter()
                .skip(keep.max(1))
                .filter(|(_, cited)| !cited)
                .map(|(id, _)| id)
                .collect();
            let at = now();
            for id in &pruned {
                tx.execute(
                    "DELETE FROM selection_releases WHERE selection_id = ?1",
                    [id],
                )?;
                tx.execute("DELETE FROM selection_groups WHERE selection_id = ?1", [id])?;
                tx.execute(
                    "UPDATE selections SET pruned_at = ?1 WHERE id = ?2",
                    params![at, id],
                )?;
            }
            Ok(pruned)
        })
    }

    /// The DAT Revision a Selection of the Source Set would use, and its blob, for a preview.
    pub fn selection_revision(
        &self,
        source_set: &str,
        revision: Option<&str>,
    ) -> Result<(DatRevisionRecord, PathBuf)> {
        self.read(|c| {
            let set = set_by_name(c, source_set)?;
            let (_, chosen) = choose_revision(c, &set, revision)?;
            check_blob_present(&chosen)?;
            let blob = self.blob_path(&chosen.sha256);
            Ok((chosen, blob))
        })
    }
}
