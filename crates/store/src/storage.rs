//! The Storage Policy: the one place that decides how a folder stores a Game, as TorrentZip, loose, a loose folder or
//! CHD, and whether a conversion deletes each original once its converted file is verified.

use rusqlite::{Connection, OptionalExtension, params};
use title_select_api::storage::{
    DiscLayout, ScopePolicy, Storage, StorageFormat, StoragePolicy, StorageScope, SystemStorage,
    is_disc_system,
};

use crate::{Error, Result, Store, now};

const SCOPES: [StorageScope; 2] = [StorageScope::SourceSets, StorageScope::PlayLibraries];

fn load_scope(c: &Connection, scope: StorageScope) -> Result<ScopePolicy> {
    let mut stmt = c.prepare(
        "SELECT system, format, delete_originals, layout, sbi FROM storage_policy WHERE scope = ?1
         ORDER BY system",
    )?;
    let rows = stmt
        .query_map([scope.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<bool>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<bool>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut policy = ScopePolicy::default();
    for (system, format, delete_originals, layout, sbi) in rows {
        let format = format.as_deref().and_then(StorageFormat::parse);
        let layout = layout.as_deref().and_then(DiscLayout::parse);
        if system.is_empty() {
            policy.format = format.unwrap_or_default();
            policy.delete_originals = delete_originals.unwrap_or(false);
            policy.layout = layout.unwrap_or_default();
        } else {
            policy.systems.insert(
                system,
                SystemStorage {
                    format,
                    delete_originals,
                    layout,
                    sbi,
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

/// Respells each system override whose provider the store now spells otherwise, as [`crate::dats::canonical_dat`]:
/// one saved before its provider's first DAT was imported keeps the spelling it was given until then. Two overrides
/// that become one system merge, the more cautious winning: an override that keeps originals over one that follows
/// the default, and that over one that deletes them; the format of the one already under the kept spelling, else the
/// first by name, unless it sets none; and so the layout and SBI choice.
pub(crate) fn respell_overrides(c: &Connection) -> Result<()> {
    // A migration before the disc layout's respells too, before its columns exist.
    let laid: bool = c.query_row(
        "SELECT count(*) > 0 FROM pragma_table_info('storage_policy') WHERE name = 'layout'",
        [],
        |r| r.get(0),
    )?;
    let discs = if laid { "layout, sbi" } else { "NULL, NULL" };
    let rows = c
        .prepare(&format!(
            "SELECT scope, system, format, delete_originals, {discs} FROM storage_policy
             WHERE system <> '' ORDER BY scope, system"
        ))?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<bool>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<bool>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (scope, system, format, delete_originals, layout, sbi) in rows {
        let canonical = crate::dats::canonical_dat(c, &system)?;
        if canonical == system {
            continue;
        }
        type Row = (Option<String>, Option<bool>, Option<String>, Option<bool>);
        let kept: Option<Row> = c
            .query_row(
                &format!(
                    "SELECT format, delete_originals, {discs} FROM storage_policy
                     WHERE scope = ?1 AND system = ?2"
                ),
                params![scope, canonical],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        match kept {
            None => {
                c.execute(
                    "UPDATE storage_policy SET system = ?3 WHERE scope = ?1 AND system = ?2",
                    params![scope, system, canonical],
                )?;
            }
            Some((kept_format, kept_delete, kept_layout, kept_sbi)) => {
                let caution = |d: Option<bool>| match d {
                    Some(false) => 0,
                    None => 1,
                    Some(true) => 2,
                };
                let delete = if caution(delete_originals) < caution(kept_delete) {
                    delete_originals
                } else {
                    kept_delete
                };
                c.execute(
                    "UPDATE storage_policy SET format = ?3, delete_originals = ?4 WHERE scope = ?1 AND system = ?2",
                    params![scope, canonical, kept_format.or(format), delete],
                )?;
                if laid {
                    c.execute(
                        "UPDATE storage_policy SET layout = ?3, sbi = ?4 WHERE scope = ?1 AND system = ?2",
                        params![scope, canonical, kept_layout.or(layout), kept_sbi.or(sbi)],
                    )?;
                }
                c.execute(
                    "DELETE FROM storage_policy WHERE scope = ?1 AND system = ?2",
                    params![scope, system],
                )?;
            }
        }
    }
    Ok(())
}

fn check(policy: &StoragePolicy) -> Result<()> {
    let play = &policy.play_libraries;
    // A source set holds each game as the DAT lists it, so only a disc system's play libraries lay out its discs.
    let sources = &policy.source_sets;
    if sources.layout != DiscLayout::default()
        || sources
            .systems
            .values()
            .any(|s| s.layout.is_some() || s.sbi.is_some())
    {
        return Err(Error::Refused(
            "source sets have no disc layout and no SBI files: set them for play libraries".into(),
        ));
    }
    if let Some((system, _)) = play
        .systems
        .iter()
        .find(|(system, s)| (s.layout.is_some() || s.sbi.is_some()) && !is_disc_system(system))
    {
        return Err(Error::Refused(format!(
            "{system} is not a disc system, so its play libraries have no disc layout and no SBI files"
        )));
    }
    // A play library lays out only a disc system's loose files (ADR 0014); a cartridge game stays TorrentZip or loose.
    let folder = Some(StorageFormat::LooseFolder);
    if play.format == StorageFormat::LooseFolder {
        return Err(Error::Refused(
            "play libraries store loose files only for disc systems: choose them for each disc system's play \
             libraries, not as the default"
                .into(),
        ));
    }
    if let Some((system, _)) = play
        .systems
        .iter()
        .find(|(system, s)| s.format == folder && !is_disc_system(system))
    {
        return Err(Error::Refused(format!(
            "{system} is not a disc system, so its play libraries can't store games as loose files laid out \
             by disc"
        )));
    }
    // A CHD is only ever a play library's copy of a disc: a source set holds the DAT's exact bytes (ADR 0013), and a
    // system that is no disc system has no disc to store as one.
    let chd = Some(StorageFormat::Chd);
    if policy.source_sets.format == StorageFormat::Chd
        || policy.source_sets.systems.values().any(|s| s.format == chd)
    {
        return Err(Error::Refused(
            "source sets can't store games as CHD: they keep each file exactly as the DAT lists it"
                .into(),
        ));
    }
    if play.format == StorageFormat::Chd {
        return Err(Error::Refused(
            "CHD is for disc systems only: choose it for each disc system's play libraries, not as the default"
                .into(),
        ));
    }
    if let Some((system, _)) = play
        .systems
        .iter()
        .find(|(system, s)| s.format == chd && !is_disc_system(system))
    {
        return Err(Error::Refused(format!(
            "{system} is not a disc system, so its play libraries can't store games as CHD"
        )));
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
            if own.is_empty() {
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
                    "INSERT INTO storage_policy (scope, system, format, delete_originals, layout, updated_by,
                                                 updated_at)
                     VALUES (?1, '', ?2, ?3, ?4, ?5, ?6)",
                    params![
                        scope.as_str(),
                        own.format.as_str(),
                        own.delete_originals,
                        (scope == StorageScope::PlayLibraries).then(|| own.layout.as_str()),
                        actor,
                        at
                    ],
                )?;
                let mut kept = std::collections::BTreeMap::new();
                for (system, s) in &own.systems {
                    let canonical = crate::dats::canonical_dat(tx, system)?;
                    if let Some(other) = kept.insert(canonical.clone(), system) {
                        return Err(Error::Refused(format!(
                            "{other} and {system} are one system, {canonical}: set it once"
                        )));
                    }
                    let system = &canonical;
                    tx.execute(
                        "INSERT INTO storage_policy (scope, system, format, delete_originals, layout, sbi,
                                                     updated_by, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            scope.as_str(),
                            system,
                            s.format.map(StorageFormat::as_str),
                            s.delete_originals,
                            s.layout.map(DiscLayout::as_str),
                            s.sbi,
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

#[cfg(test)]
mod tests {
    use super::policy as policy_of;
    use super::*;

    #[test]
    fn an_override_saved_before_its_dat_is_imported_follows_the_spelling_the_import_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut policy = StoragePolicy::default();
        let own = |format, delete_originals| SystemStorage {
            format,
            delete_originals,
            ..Default::default()
        };
        let systems = &mut policy.source_sets.systems;
        systems.insert("redump/PSX".into(), own(None, Some(false)));
        systems.insert(
            "REDUMP/PSX".into(),
            own(Some(StorageFormat::Loose), Some(true)),
        );
        systems.insert("redump/PS2".into(), own(None, Some(true)));
        store.set_storage_policy(&policy, "Operator").unwrap();

        for name in ["PSX", "PS2"] {
            let dat = format!(
                "<?xml version=\"1.0\"?><datafile><header><name>{name}</name><version>1</version></header></datafile>"
            );
            store
                .import_dat_bytes(dat.as_bytes(), "x.dat", "Redump", None)
                .unwrap();
        }

        let systems = store.storage_policy().unwrap().source_sets.systems;
        // Of two overrides now of one system, the one that keeps originals wins.
        assert_eq!(
            systems.into_iter().collect::<Vec<_>>(),
            [
                ("Redump/PS2".to_owned(), own(None, Some(true))),
                (
                    "Redump/PSX".to_owned(),
                    own(Some(StorageFormat::Loose), Some(false))
                ),
            ]
        );
    }

    #[test]
    fn a_disc_systems_play_libraries_keep_their_layout_and_sbi_choice_and_store_loose_folders() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let psx = "Redump/Sony - PlayStation";
        let mut policy = StoragePolicy::default();
        policy.play_libraries.layout = DiscLayout::Flat;
        policy.play_libraries.systems.insert(
            psx.into(),
            SystemStorage {
                format: Some(StorageFormat::LooseFolder),
                layout: Some(DiscLayout::EsDe),
                sbi: Some(true),
                ..SystemStorage::default()
            },
        );
        assert_eq!(
            store.set_storage_policy(&policy, "Operator").unwrap(),
            policy
        );
        let storage = store
            .read(|c| policy_of(c, StorageScope::PlayLibraries, psx))
            .unwrap();
        assert_eq!(
            (storage.format, storage.layout, storage.sbi),
            (StorageFormat::LooseFolder, Some(DiscLayout::EsDe), true)
        );

        let mut on_sources = StoragePolicy::default();
        on_sources.source_sets.systems.insert(
            psx.into(),
            SystemStorage {
                sbi: Some(true),
                ..SystemStorage::default()
            },
        );
        assert!(store.set_storage_policy(&on_sources, "Operator").is_err());
        let mut no_discs = StoragePolicy::default();
        no_discs.play_libraries.systems.insert(
            "No-Intro/Atari".into(),
            SystemStorage {
                layout: Some(DiscLayout::Flat),
                ..SystemStorage::default()
            },
        );
        assert!(store.set_storage_policy(&no_discs, "Operator").is_err());

        // A cartridge game is never laid out loose in a play library, by default or by its system.
        let mut folders = StoragePolicy::default();
        folders.play_libraries.format = StorageFormat::LooseFolder;
        assert!(store.set_storage_policy(&folders, "Operator").is_err());
        let mut cartridges = StoragePolicy::default();
        cartridges.play_libraries.systems.insert(
            "No-Intro/Atari".into(),
            SystemStorage {
                format: Some(StorageFormat::LooseFolder),
                ..SystemStorage::default()
            },
        );
        assert!(store.set_storage_policy(&cartridges, "Operator").is_err());
    }
}
