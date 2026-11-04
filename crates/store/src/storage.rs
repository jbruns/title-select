//! The Storage Policy: the one place that decides how a folder stores a Game, as TorrentZip, loose or a loose folder,
//! and whether a
//! conversion deletes each original once its converted file is verified.

use rusqlite::{Connection, params};
use title_select_api::storage::{
    ScopePolicy, Storage, StorageFormat, StoragePolicy, StorageScope, SystemStorage,
};

use crate::{Error, Result, Store, now};

const SCOPES: [StorageScope; 2] = [StorageScope::SourceSets, StorageScope::PlayLibraries];

fn load_scope(c: &Connection, scope: StorageScope) -> Result<ScopePolicy> {
    let mut stmt = c.prepare(
        "SELECT system, format, delete_originals FROM storage_policy WHERE scope = ?1 ORDER BY system",
    )?;
    let rows = stmt
        .query_map([scope.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<bool>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut policy = ScopePolicy::default();
    for (system, format, delete_originals) in rows {
        let format = format.as_deref().and_then(StorageFormat::parse);
        if system.is_empty() {
            policy.format = format.unwrap_or_default();
            policy.delete_originals = delete_originals.unwrap_or(false);
        } else {
            policy.systems.insert(
                system,
                SystemStorage {
                    format,
                    delete_originals,
                },
            );
        }
    }
    Ok(policy)
}

/// What the Storage Policy says for folders of `scope` bound to `system`, a DAT as `<provider>/<name>`: the
/// system's override where it has one, else the scope's default, which is a loose folder for a disc system's Source
/// Sets. Every writer of a Game into a folder asks here.
pub(crate) fn policy(c: &Connection, scope: StorageScope, system: &str) -> Result<Storage> {
    Ok(load_scope(c, scope)?.of(scope, system))
}

fn check(policy: &StoragePolicy) -> Result<()> {
    // How a play library lays out a disc system's games is still to come (#100).
    let play = &policy.play_libraries;
    let folder = Some(StorageFormat::LooseFolder);
    if play.format == StorageFormat::LooseFolder
        || play.systems.values().any(|s| s.format == folder)
    {
        return Err(Error::Refused(
            "play libraries can't store games as loose folders yet: choose TorrentZip or loose"
                .into(),
        ));
    }
    for scope in SCOPES {
        for (system, own) in &policy.scope(scope).systems {
            let Some((provider, name)) = system.split_once('/') else {
                return Err(Error::Refused(format!(
                    "{system} is not a system: name its DAT as <provider>/<name>"
                )));
            };
            if provider.is_empty() || name.is_empty() {
                return Err(Error::Refused(format!(
                    "{system} is not a system: name its DAT as <provider>/<name>"
                )));
            }
            if own.format.is_none() && own.delete_originals.is_none() {
                return Err(Error::Refused(format!(
                    "{system} overrides nothing; remove it to follow the default"
                )));
            }
        }
    }
    Ok(())
}

impl Store {
    /// The whole Storage Policy.
    pub fn storage_policy(&self) -> Result<StoragePolicy> {
        self.read(|c| {
            Ok(StoragePolicy {
                source_sets: load_scope(c, StorageScope::SourceSets)?,
                play_libraries: load_scope(c, StorageScope::PlayLibraries)?,
            })
        })
    }

    /// Replaces the whole Storage Policy with `policy`, as `actor`. Nothing in any folder changes: the next plan of
    /// each folder whose format changed proposes its conversions, for you to review.
    pub fn set_storage_policy(&self, policy: &StoragePolicy, actor: &str) -> Result<StoragePolicy> {
        check(policy)?;
        self.write(|tx| {
            tx.execute("DELETE FROM storage_policy", [])?;
            let at = now();
            for scope in SCOPES {
                let own = policy.scope(scope);
                tx.execute(
                    "INSERT INTO storage_policy (scope, system, format, delete_originals, updated_by, updated_at)
                     VALUES (?1, '', ?2, ?3, ?4, ?5)",
                    params![
                        scope.as_str(),
                        own.format.as_str(),
                        own.delete_originals,
                        actor,
                        at
                    ],
                )?;
                for (system, s) in &own.systems {
                    tx.execute(
                        "INSERT INTO storage_policy (scope, system, format, delete_originals, updated_by,
                                                     updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            scope.as_str(),
                            system,
                            s.format.map(StorageFormat::as_str),
                            s.delete_originals,
                            actor,
                            at
                        ],
                    )?;
                }
            }
            Ok(())
        })?;
        self.storage_policy()
    }
}
