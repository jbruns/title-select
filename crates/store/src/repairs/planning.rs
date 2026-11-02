//! What a repair proposes, from an Audit's Payload Evidence and the assessed items of Ingest Folders, and the steps
//! that carry out what you choose. Nothing here touches a file.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use title_select_api::build::{ExpectedRom, invalid_name, kept_name};
use title_select_api::repair::{
    PlannedStep, RepairChoice, RepairItem, RepairKind, RepairRom, RepairStep,
};
use title_select_api::storage::{MULTI_ROM_STAYS_ZIP, Storage, StorageFormat, target_of};
use title_select_api::{Entry, EvidenceLevel, Finding, Report};
use title_select_engine::FileStamp;
use title_select_engine::dat::Dat;

use super::{as_read, seen_file};
use crate::checks::Seen;
use crate::ingest::FillCandidate;
use crate::{Error, Result};

pub(super) struct Inputs<'a> {
    pub report: &'a Report,
    pub dat: &'a Dat,
    /// Every regular file in the Source Set now, by path relative to its folder.
    pub files: &'a BTreeMap<String, Seen>,
    pub stamps: &'a HashMap<String, FileStamp>,
    /// The stamp of each file the Audit could not read every payload of, when it could stat it.
    pub unreadable: &'a HashMap<String, FileStamp>,
    /// Whether the file at a path is stored as TorrentZip.
    pub torrentzip: &'a dyn Fn(&str) -> bool,
    pub candidates: &'a [FillCandidate],
    pub choices: &'a BTreeMap<String, RepairChoice>,
    /// What the Storage Policy says for the Source Set's system.
    pub storage: Storage,
}

pub(super) struct Planned {
    pub items: Vec<RepairItem>,
    pub steps: Vec<PlannedStep>,
    pub notes: Vec<String>,
}

/// The one place the name and format a repair stores a Game in is decided, from the Storage Policy: a loose file
/// under its one ROM's name, or a TorrentZip as `<game>.zip`, in the Source Set's folder; and whether it is loose.
/// The note says why a Game the policy would store loose stays TorrentZip.
pub(super) fn storage_name(
    storage: Storage,
    game: &str,
    roms: &[ExpectedRom],
) -> (String, bool, Option<&'static str>) {
    let names: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
    target_of(storage, game, &names)
}

/// A ROM, by its Game and name.
type RomKey = (String, String);

/// A file the Audit read the payload of, unchanged since.
struct Usable<'a> {
    entries: Vec<&'a Entry>,
    unreadable: bool,
}

impl Usable<'_> {
    /// The ROMs its payloads are.
    fn holds(&self) -> BTreeSet<RomKey> {
        self.entries
            .iter()
            .filter(|e| {
                matches!(
                    e.finding,
                    Finding::Matched | Finding::Misnamed | Finding::Conflicting
                )
            })
            .flat_map(|e| e.roms.iter().map(|r| (r.game.clone(), r.name.clone())))
            .collect()
    }
}

/// How a Game is repaired.
enum Proposal {
    /// Written from ROMs read in the Source Set.
    Build { roms: Vec<RepairRom> },
    /// A file already as it should be but for its name, renamed once it is read again as the Game's ROMs.
    Rename {
        from: String,
        roms: Vec<ExpectedRom>,
    },
    /// Filled from the ingest candidate at this index.
    Fill { candidate: usize },
}

/// A proposed change, with the files it keeps aside.
struct Proposed {
    item: RepairItem,
    proposal: Option<Proposal>,
    /// The files it keeps aside, each with why.
    displaces: Vec<(String, String)>,
    /// The Game it makes whole.
    produces: Option<String>,
    /// Whether the files it displaces must leave a copy of each ROM they hold.
    checked: bool,
    /// Where it writes its Game, and whether as a loose file.
    target: Option<(String, bool)>,
    /// The original a conversion reads, which it may delete once its converted file is verified.
    converts: Option<String>,
}

fn entry_key(entry: &Entry) -> String {
    match &entry.member {
        Some(member) => format!("entry:{}#{member}", entry.path),
        None => format!("entry:{}", entry.path),
    }
}

fn entry_label(entry: &Entry) -> String {
    match &entry.member {
        Some(member) => format!("{} ({member})", entry.path),
        None => entry.path.clone(),
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn roms_of(dat: &Dat, game: usize) -> Vec<ExpectedRom> {
    dat.roms
        .iter()
        .filter(|r| r.game == game)
        .map(|rom| ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
        })
        .collect()
}

fn game_item(game: &str, target: &str, kind: RepairKind, summary: String) -> RepairItem {
    RepairItem {
        key: format!("game:{game}"),
        kind,
        game: Some(game.to_owned()),
        path: Some(target.to_owned()),
        member: None,
        summary,
        choices: vec![RepairChoice::Apply, RepairChoice::Leave],
        default: RepairChoice::Apply,
        chosen: RepairChoice::Apply,
        note: None,
    }
}

pub(super) fn plan(i: &Inputs) -> Result<Planned> {
    let mut notes = Vec::new();
    let mut by_path: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in &i.report.entries {
        by_path.entry(&entry.path).or_default().push(entry);
    }
    // Every file the Audit found holding each ROM, whether or not a repair may change it.
    let mut held_by: HashMap<RomKey, BTreeSet<&str>> = HashMap::new();
    let mut usable: BTreeMap<&str, Usable> = BTreeMap::new();
    for (path, entries) in &by_path {
        for entry in entries {
            if matches!(
                entry.finding,
                Finding::Matched | Finding::Misnamed | Finding::Conflicting
            ) {
                for rom in &entry.roms {
                    held_by
                        .entry((rom.game.clone(), rom.name.clone()))
                        .or_default()
                        .insert(path);
                }
            }
        }
        let Some(seen) = i.files.get(*path) else {
            notes.push(format!(
                "{path} is gone since the audit; audit again to repair what it held"
            ));
            continue;
        };
        if entries.iter().any(|e| {
            e.evidence
                .as_ref()
                .is_some_and(|ev| ev.level == EvidenceLevel::Claim)
        }) {
            notes.push(format!(
                "{path} was read only by its claim, so it is left as it is; run an incremental or full audit to \
                 repair it"
            ));
            continue;
        }
        let unreadable = entries.iter().any(|e| e.finding == Finding::Unreadable);
        let stamp = match unreadable {
            true => i.unreadable.get(*path),
            false => i.stamps.get(*path),
        };
        let unchanged = stamp.is_some_and(|stamp| as_read(seen, stamp));
        if unreadable && stamp.is_none() {
            notes.push(format!(
                "{path} could not be read by the audit, which could not tell what it was then, so it is left as it \
                 is; audit again to repair it"
            ));
            continue;
        }
        if !unchanged {
            notes.push(format!(
                "{path} changed since the audit read it, so it is left as it is; audit again to repair it"
            ));
            continue;
        }
        usable.insert(
            path,
            Usable {
                entries: entries.clone(),
                unreadable,
            },
        );
    }
    for path in i.files.keys() {
        if !by_path.contains_key(path.as_str()) {
            notes.push(format!(
                "{path} is new since the audit, so it is left as it is; audit again to repair it"
            ));
        }
    }

    let mut proposed: Vec<Proposed> = Vec::new();
    // The files a Game item or a Conflicting Entry is about, which are never kept aside as holding nothing.
    let mut referenced: BTreeSet<&str> = BTreeSet::new();

    // Conflicting Entries first: what you choose for each decides what a Game may be written from.
    let mut applied_conflicts: BTreeSet<(&str, Option<&str>)> = BTreeSet::new();
    let mut kept_conflicts: BTreeSet<&str> = BTreeSet::new();
    // Files holding a Conflicting Entry you left: never read from, displaced or kept aside by anything else.
    let mut left_conflicts: BTreeSet<&str> = BTreeSet::new();
    for (path, file) in &usable {
        for entry in file
            .entries
            .iter()
            .filter(|e| e.finding == Finding::Conflicting)
        {
            let key = entry_key(entry);
            let holds: Vec<String> = entry
                .roms
                .iter()
                .map(|r| format!("{} of {}", r.name, r.game))
                .collect();
            let named = entry
                .named_rom
                .as_ref()
                .map(|r| format!("{} of {}", r.name, r.game))
                .unwrap_or_else(|| "another ROM".into());
            let default = RepairChoice::Leave;
            let chosen = i.choices.get(&key).copied().unwrap_or(default);
            match chosen {
                RepairChoice::Apply => {
                    applied_conflicts.insert((path, entry.member.as_deref()));
                }
                RepairChoice::KeepAside => {
                    kept_conflicts.insert(path);
                }
                RepairChoice::Leave => {
                    left_conflicts.insert(path);
                }
            }
            referenced.insert(path);
            proposed.push(Proposed {
                item: RepairItem {
                    key,
                    kind: RepairKind::Conflict,
                    game: entry.roms.first().map(|r| r.game.clone()),
                    path: Some(path.to_string()),
                    member: entry.member.clone(),
                    summary: format!(
                        "{} holds {} while its name gives {named}",
                        entry_label(entry),
                        holds.join(", ")
                    ),
                    choices: vec![
                        RepairChoice::Leave,
                        RepairChoice::Apply,
                        RepairChoice::KeepAside,
                    ],
                    default,
                    chosen: default,
                    note: None,
                },
                proposal: None,
                displaces: match chosen {
                    RepairChoice::KeepAside => vec![(
                        path.to_string(),
                        "you chose to keep its conflicting entry aside".into(),
                    )],
                    _ => Vec::new(),
                },
                produces: None,
                checked: false,
                target: None,
                converts: None,
            });
        }
    }

    let mut stays_zip = 0;
    let mut used_candidates: BTreeSet<i64> = BTreeSet::new();
    let mut good: BTreeSet<String> = BTreeSet::new();
    for (index, game) in i.dat.games.iter().enumerate() {
        let roms = roms_of(i.dat, index);
        if roms.is_empty() {
            continue;
        }
        let (target, loose, why_zip) = storage_name(i.storage, game, &roms);
        match why_zip {
            Some(MULTI_ROM_STAYS_ZIP) => stays_zip += 1,
            Some(why) => notes.push(format!("{game}: {why}")),
            None => {}
        }
        if let Some(why) = invalid_name(&target) {
            notes.push(format!("{game} is left as it is: {why}"));
            continue;
        }
        // Each ROM's best copy: matched in the Game's own file first, then matched elsewhere, then misnamed, then a
        // conflicting entry you chose to take as the ROM it matches.
        let mut sources: Vec<Option<(u8, &str, &Entry)>> = vec![None; roms.len()];
        for (path, file) in &usable {
            if file.unreadable || kept_conflicts.contains(path) || left_conflicts.contains(path) {
                continue;
            }
            for entry in &file.entries {
                let rank = match entry.finding {
                    Finding::Matched if *path == target => 0,
                    Finding::Matched => 1,
                    Finding::Misnamed => 2,
                    Finding::Conflicting
                        if applied_conflicts.contains(&(*path, entry.member.as_deref())) =>
                    {
                        3
                    }
                    _ => continue,
                };
                for (n, rom) in roms.iter().enumerate() {
                    if entry
                        .roms
                        .iter()
                        .any(|r| r.game == *game && r.name == rom.name)
                        && sources[n].is_none_or(|(best, ..)| rank < best)
                    {
                        sources[n] = Some((rank, path, entry));
                    }
                }
            }
        }
        let occupant = i.files.contains_key(&target).then_some(target.as_str());
        let left_in_way = |path: &str| {
            format!(
                "{game} is left as it is: {path} is in the way, and it holds a conflicting entry you chose to leave; \
                 choose to keep that entry aside to repair it"
            )
        };
        if let Some(occupant) = occupant
            && left_conflicts.contains(occupant)
        {
            notes.push(left_in_way(occupant));
            continue;
        }
        if sources.iter().all(Option::is_some) {
            let sources: Vec<(u8, &str, &Entry)> = sources.into_iter().flatten().collect();
            let names: BTreeSet<&str> = roms.iter().map(|r| r.name.as_str()).collect();
            // A file holding exactly the Game's ROMs, each Matched under its name, stored as TorrentZip; or, `loose`,
            // a loose file that is the Game's one ROM, Matched under its name.
            let exact_as = |path: &str, loose: bool| {
                usable.get(path).is_some_and(|file| {
                    if loose {
                        return matches!(
                            (roms.as_slice(), file.entries.as_slice()),
                            ([only], [e]) if e.member.is_none()
                                && e.finding == Finding::Matched
                                && e.roms.iter().any(|r| r.game == *game && r.name == only.name)
                        );
                    }
                    let members: BTreeSet<Option<&str>> =
                        file.entries.iter().map(|e| e.member.as_deref()).collect();
                    file.entries.len() == roms.len()
                        && members.len() == roms.len()
                        && file.entries.iter().all(|e| {
                            e.finding == Finding::Matched
                                && e.member.as_deref().is_some_and(|m| names.contains(m))
                                && e.roms.iter().any(|r| {
                                    r.game == *game && Some(r.name.as_str()) == e.member.as_deref()
                                })
                        })
                        && (i.torrentzip)(path)
                })
            };
            let exact = |path: &str| exact_as(path, loose);
            if exact(&target) {
                good.insert(game.clone());
                referenced.insert(usable.get_key_value(target.as_str()).expect("usable").0);
                continue;
            }
            let files: BTreeSet<&str> = sources.iter().map(|(_, path, _)| *path).collect();
            referenced.extend(files.iter().copied());
            if let Some(occupant) = occupant {
                let Some((key, file)) = usable.get_key_value(occupant) else {
                    notes.push(format!(
                        "{game} is left as it is: {occupant} is in the way, and the audit did not read it as it is \
                         now"
                    ));
                    continue;
                };
                if file.unreadable {
                    notes.push(format!(
                        "{game} is left as it is: {occupant} is in the way, and it is unreadable; it is replaced \
                         only from an ingest folder"
                    ));
                    continue;
                }
                referenced.insert(key);
            }
            let single = files.iter().next().filter(|_| files.len() == 1).copied();
            let format = match loose {
                true => StorageFormat::Loose,
                false => StorageFormat::TorrentZip,
            };
            let mut converts = None;
            let (proposal, kind, summary, mut displaces) = match single {
                Some(from) if from != target && exact(from) => (
                    Proposal::Rename {
                        from: from.to_owned(),
                        roms: roms.clone(),
                    },
                    RepairKind::Rename,
                    format!("rename {from} to {target}"),
                    Vec::new(),
                ),
                // Exactly the Game, but stored as the other format: rewritten in this one.
                Some(from) if from != target && exact_as(from, !loose) => {
                    converts = Some(from.to_owned());
                    let roms = roms
                        .iter()
                        .zip(&sources)
                        .map(|(rom, (_, path, entry))| RepairRom {
                            rom: rom.clone(),
                            from: path.to_string(),
                            member: entry.member.clone(),
                        })
                        .collect();
                    (
                        Proposal::Build { roms },
                        RepairKind::Convert,
                        format!("convert {from} to {target}, stored {}", format.label()),
                        vec![(
                            from.to_owned(),
                            format!("it was converted to {target}, stored {}", format.label()),
                        )],
                    )
                }
                _ => {
                    let from: Vec<&str> = files.iter().copied().collect();
                    let (kind, summary) = match sources.iter().any(|(rank, ..)| *rank >= 2) {
                        true => (
                            RepairKind::Rename,
                            format!(
                                "write {target}, under the names the DAT gives, from {}",
                                from.join(", ")
                            ),
                        ),
                        false => (
                            RepairKind::Repack,
                            format!(
                                "repack {} as {target}, stored {}",
                                from.join(", "),
                                format.label()
                            ),
                        ),
                    };
                    let displaces = files
                        .iter()
                        .map(|path| {
                            (
                                path.to_string(),
                                format!("its ROMs were written to {target}"),
                            )
                        })
                        .collect();
                    let roms = roms
                        .iter()
                        .zip(&sources)
                        .map(|(rom, (_, path, entry))| RepairRom {
                            rom: rom.clone(),
                            from: path.to_string(),
                            member: entry.member.clone(),
                        })
                        .collect();
                    (Proposal::Build { roms }, kind, summary, displaces)
                }
            };
            if let Some(occupant) = occupant
                && !displaces.iter().any(|(path, _)| path == occupant)
            {
                displaces.push((
                    occupant.to_owned(),
                    format!("{target} was written in its place"),
                ));
            }
            proposed.push(Proposed {
                item: game_item(game, &target, kind, summary),
                proposal: Some(proposal),
                displaces,
                produces: Some(game.clone()),
                checked: true,
                target: Some((target.clone(), loose)),
                converts,
            });
            continue;
        }

        // Some ROM is not in the Source Set: filled from an Ingest Folder, when an item holds the Game whole.
        let attributable: Vec<&str> = by_path
            .iter()
            .filter(|(path, entries)| {
                **path == target
                    || entries.iter().any(|e| match e.finding {
                        Finding::Matched | Finding::Misnamed => {
                            e.roms.iter().any(|r| r.game == *game)
                        }
                        Finding::Mismatched => {
                            e.named_rom.as_ref().is_some_and(|r| r.game == *game)
                        }
                        Finding::Unreadable => {
                            let name = base_name(&e.path);
                            name == target || roms.iter().any(|r| r.name == name)
                        }
                        _ => false,
                    })
            })
            .map(|(path, _)| *path)
            .filter(|path| !kept_conflicts.contains(path))
            .collect();
        if let Some(path) = attributable.iter().find(|p| left_conflicts.contains(**p)) {
            notes.push(left_in_way(path));
            continue;
        }
        let Some(candidate) = i
            .candidates
            .iter()
            .position(|c| c.matched.game == *game && !used_candidates.contains(&c.item.id))
        else {
            if !attributable.is_empty() || occupant.is_some() {
                notes.push(format!(
                    "{game} is left as it is: no ingest folder holds it whole, with its payloads read"
                ));
            }
            continue;
        };
        if let Some(blocked) = attributable
            .iter()
            .chain(occupant.as_ref())
            .find(|path| !usable.contains_key(**path))
        {
            notes.push(format!(
                "{game} is left as it is: {blocked} is in the way, and the audit did not read it as it is now"
            ));
            continue;
        }
        let c = &i.candidates[candidate];
        used_candidates.insert(c.item.id);
        referenced.extend(attributable.iter().copied());
        let (kind, summary) = match attributable.is_empty() {
            true => (
                RepairKind::Fill,
                format!(
                    "write {target} from {} in ingest folder {}",
                    c.item.name, c.root
                ),
            ),
            false => (
                RepairKind::Replace,
                format!(
                    "keep {} aside, and write {target} from {} in ingest folder {}",
                    attributable.join(", "),
                    c.item.name,
                    c.root
                ),
            ),
        };
        proposed.push(Proposed {
            item: game_item(game, &target, kind, summary),
            target: Some((target.clone(), loose)),
            converts: None,
            proposal: Some(Proposal::Fill { candidate }),
            displaces: attributable
                .iter()
                .map(|path| {
                    (
                        path.to_string(),
                        format!("{game} was written from an ingest folder in its place"),
                    )
                })
                .collect(),
            produces: Some(game.clone()),
            checked: true,
        });
    }

    // The choices you made in place of the defaults; those for files holding nothing are checked below.
    for (key, choice) in i.choices {
        let Some(found) = proposed.iter_mut().find(|p| p.item.key == *key) else {
            if key.starts_with("file:") {
                continue;
            }
            return Err(Error::Refused(format!("the plan has no item {key}")));
        };
        if !found.item.choices.contains(choice) {
            return Err(Error::Refused(format!(
                "{key} cannot be set to {}",
                choice.label()
            )));
        }
        found.item.chosen = *choice;
    }

    // An item that would keep aside the last copy of a ROM no repair writes is left, as is, in turn, any item that
    // relied on it.
    loop {
        let applied = |p: &&Proposed| p.item.chosen != RepairChoice::Leave;
        let produced: BTreeSet<&str> = proposed
            .iter()
            .filter(applied)
            .filter_map(|p| p.produces.as_deref())
            .collect();
        let displaced: BTreeSet<&str> = proposed
            .iter()
            .filter(applied)
            .flat_map(|p| p.displaces.iter().map(|(path, _)| path.as_str()))
            .collect();
        let lost = proposed.iter().position(|p| {
            p.checked
                && p.item.chosen != RepairChoice::Leave
                && p.displaces.iter().any(|(path, _)| {
                    usable.get(path.as_str()).is_some_and(|file| {
                        file.holds().iter().any(|rom| {
                            !produced.contains(rom.0.as_str())
                                && !held_by
                                    .get(rom)
                                    .is_some_and(|by| by.iter().any(|p| !displaced.contains(p)))
                        })
                    })
                })
        });
        let Some(lost) = lost else {
            break;
        };
        let item = &mut proposed[lost].item;
        item.chosen = RepairChoice::Leave;
        item.note = Some(
            "left as it is: it would keep aside the only copy of a ROM no repair in this plan writes".into(),
        );
    }

    // A file holding nothing a Game needs: Unmatched Entries, or copies of ROMs a Game's own file holds.
    let covered = |game: &str| {
        good.contains(game)
            || proposed.iter().any(|p| {
                p.item.chosen != RepairChoice::Leave && p.produces.as_deref() == Some(game)
            })
    };
    let mut aside = Vec::new();
    for (path, file) in &usable {
        if referenced.contains(path) || file.unreadable || file.entries.is_empty() {
            continue;
        }
        let spare = file.entries.iter().all(|e| match e.finding {
            Finding::Unmatched => true,
            Finding::Matched | Finding::Misnamed => e.roms.iter().all(|r| covered(&r.game)),
            _ => false,
        });
        if !spare {
            continue;
        }
        let key = format!("file:{path}");
        let default = RepairChoice::KeepAside;
        let chosen = i.choices.get(&key).copied().unwrap_or(default);
        let choices = vec![RepairChoice::KeepAside, RepairChoice::Leave];
        if !choices.contains(&chosen) {
            return Err(Error::Refused(format!(
                "{key} cannot be set to {}",
                chosen.label()
            )));
        }
        let summary = match file.entries.iter().any(|e| e.finding != Finding::Unmatched) {
            true => format!("{path} holds only copies of ROMs a game's own file holds"),
            false => format!("{path} holds nothing a game needs"),
        };
        aside.push(Proposed {
            item: RepairItem {
                key,
                kind: RepairKind::KeepAside,
                game: None,
                path: Some(path.to_string()),
                member: None,
                summary: summary.clone(),
                choices,
                default,
                chosen,
                note: None,
            },
            proposal: None,
            displaces: vec![(path.to_string(), summary)],
            produces: None,
            checked: false,
            target: None,
            converts: None,
        });
    }
    for key in i.choices.keys() {
        if key.starts_with("file:") && !aside.iter().any(|p| p.item.key == *key) {
            return Err(Error::Refused(format!("the plan has no item {key}")));
        }
    }
    proposed.extend(aside);

    // What you chose to keep aside is kept aside even when it is the last copy of a ROM, which the item then says:
    // it can be restored from the kept files.
    let applied = |p: &&Proposed| p.item.chosen != RepairChoice::Leave;
    let produced: BTreeSet<&str> = proposed
        .iter()
        .filter(applied)
        .filter_map(|p| p.produces.as_deref())
        .collect();
    let displaced: BTreeSet<&str> = proposed
        .iter()
        .filter(applied)
        .flat_map(|p| p.displaces.iter().map(|(path, _)| path.as_str()))
        .collect();
    let last: Vec<(usize, Vec<String>)> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.checked && p.item.chosen == RepairChoice::KeepAside)
        .filter_map(|(n, p)| {
            let lost: BTreeSet<String> = p
                .displaces
                .iter()
                .filter_map(|(path, _)| usable.get(path.as_str()))
                .flat_map(|file| file.holds())
                .filter(|rom| {
                    !produced.contains(rom.0.as_str())
                        && !held_by
                            .get(rom)
                            .is_some_and(|by| by.iter().any(|p| !displaced.contains(p)))
                })
                .map(|(game, name)| format!("{name} of {game}"))
                .collect();
            (!lost.is_empty()).then(|| (n, lost.into_iter().collect()))
        })
        .collect();
    for (n, lost) in last {
        proposed[n].item.note = Some(format!(
            "this keeps aside the last copy in the source set of {}; you can restore it from the kept files",
            lost.join(", ")
        ));
    }

    if stays_zip > 0 {
        notes.push(format!(
            "{stays_zip} game{} with more than one ROM stay{} TorrentZip: only a game with one ROM can be stored loose",
            if stays_zip == 1 { "" } else { "s" },
            if stays_zip == 1 { "s" } else { "" },
        ));
    }
    let steps = steps_of(i, &proposed);
    Ok(Planned {
        items: proposed.into_iter().map(|p| p.item).collect(),
        steps,
        notes,
    })
}

/// The steps that carry out what each item chosen does: every Build first, then every Keep, every Rename, every
/// Publish and every Fill. A conversion that shares no file with another item then follows, each in turn: its Build,
/// Keep and Publish, and, when the Storage Policy deletes originals, the Delete of its original, so that a folder
/// being converted holds at most one converted file more than it did at any time.
fn steps_of(i: &Inputs, proposed: &[Proposed]) -> Vec<PlannedStep> {
    let applied: Vec<&Proposed> = proposed
        .iter()
        .filter(|p| p.item.chosen != RepairChoice::Leave)
        .collect();
    let touches = |p: &Proposed| -> BTreeSet<String> {
        let mut paths: BTreeSet<String> = p.displaces.iter().map(|(path, _)| path.clone()).collect();
        match &p.proposal {
            Some(Proposal::Build { roms }) => paths.extend(roms.iter().map(|r| r.from.clone())),
            Some(Proposal::Rename { from, .. }) => {
                paths.insert(from.clone());
            }
            _ => {}
        }
        if let Some((target, _)) = &p.target {
            paths.insert(target.clone());
        }
        paths
    };
    let alone = |p: &Proposed| {
        let mine = touches(p);
        applied
            .iter()
            .filter(|o| o.item.key != p.item.key)
            .all(|o| touches(o).is_disjoint(&mine))
    };
    let (turns, together): (Vec<&Proposed>, Vec<&Proposed>) = applied
        .iter()
        .partition(|p| p.converts.is_some() && alone(p));
    let mut steps = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let build_of = |p: &Proposed, roms: &[RepairRom]| {
        let (target, loose) = p.target.clone().expect("a game's target");
        let sources = roms
            .iter()
            .map(|r| (r.from.clone(), seen_file(&i.files[&r.from])))
            .collect();
        (
            target.clone(),
            PlannedStep {
                item: p.item.key.clone(),
                step: RepairStep::Build {
                    target,
                    roms: roms.to_vec(),
                    sources,
                    loose,
                },
            },
        )
    };
    let mut builds: Vec<(&str, u32, String)> = Vec::new();
    for p in &together {
        if let Some(Proposal::Build { roms }) = &p.proposal {
            let (target, step) = build_of(p, roms);
            builds.push((&p.item.key, steps.len() as u32, target));
            steps.push(step);
        }
    }
    let built_by =
        |key: &str| -> Vec<u32> { builds.iter().filter(|b| b.0 == key).map(|b| b.1).collect() };
    let mut keeps: Vec<PlannedStep> = Vec::new();
    for p in &together {
        keeps_of(i, p, built_by(&p.item.key), &mut names, &mut keeps);
    }
    steps.extend(keeps);
    for p in &together {
        if let Some(Proposal::Rename { from, roms }) = &p.proposal {
            let (to, loose) = p.target.clone().expect("a game's target");
            steps.push(PlannedStep {
                item: p.item.key.clone(),
                step: RepairStep::Rename {
                    from: from.clone(),
                    to,
                    seen: seen_file(&i.files[from]),
                    roms: roms.clone(),
                    loose,
                },
            });
        }
    }
    for (item, build, target) in &builds {
        steps.push(PlannedStep {
            item: item.to_string(),
            step: RepairStep::Publish {
                build: *build,
                target: target.clone(),
            },
        });
    }
    for p in &together {
        if let Some(Proposal::Fill { candidate }) = &p.proposal {
            steps.push(fill_of(i, p, &i.candidates[*candidate]));
        }
    }
    for p in turns {
        let (Some(Proposal::Build { roms }), Some(original)) = (&p.proposal, &p.converts) else {
            unreachable!("a conversion builds");
        };
        let build = steps.len() as u32;
        let (target, step) = build_of(p, roms);
        steps.push(step);
        let mut keeps = Vec::new();
        keeps_of(i, p, vec![build], &mut names, &mut keeps);
        let keep = steps.len() as u32
            + keeps
                .iter()
                .position(|k| matches!(&k.step, RepairStep::Keep { path, .. } if path == original))
                .expect("the original is kept") as u32;
        steps.extend(keeps);
        let publish = steps.len() as u32;
        steps.push(PlannedStep {
            item: p.item.key.clone(),
            step: RepairStep::Publish { build, target },
        });
        if i.storage.delete_originals {
            steps.push(PlannedStep {
                item: p.item.key.clone(),
                step: RepairStep::Delete {
                    path: original.clone(),
                    build,
                    keep,
                    publish,
                },
            });
        }
    }
    steps
}

/// Appends to `keeps` a Keep step for each file `p` displaces, needing the Build steps `needs`, under a kept name
/// not yet in `names`; a file another item already keeps aside is kept once, for both.
fn keeps_of(
    i: &Inputs,
    p: &Proposed,
    needs: Vec<u32>,
    names: &mut BTreeSet<String>,
    keeps: &mut Vec<PlannedStep>,
) {
    for (path, reason) in &p.displaces {
        if let Some(keep) = keeps
            .iter_mut()
            .find(|k| matches!(&k.step, RepairStep::Keep { path: at, .. } if at == path))
        {
            let RepairStep::Keep {
                needs: theirs,
                affects,
                ..
            } = &mut keep.step
            else {
                unreachable!("a keep step");
            };
            theirs.extend(needs.iter().copied());
            if keep.item != p.item.key && !affects.contains(&p.item.key) {
                affects.push(p.item.key.clone());
            }
            continue;
        }
        let base = base_name(path);
        let name = (1..)
            .map(|n| kept_name(base, n))
            .find(|name| !names.contains(name))
            .expect("a free name");
        names.insert(name.clone());
        keeps.push(PlannedStep {
            item: p.item.key.clone(),
            step: RepairStep::Keep {
                path: path.clone(),
                kept_name: name,
                reason: reason.clone(),
                seen: seen_file(&i.files[path]),
                needs: needs.clone(),
                affects: Vec::new(),
            },
        });
    }
}

/// The Fill step of `p`, from ingest candidate `c`.
fn fill_of(i: &Inputs, p: &Proposed, c: &FillCandidate) -> PlannedStep {
    let game = p.item.game.as_deref().expect("a game");
    let (target, loose) = p.target.clone().expect("a game's target");
    let index = i
        .dat
        .games
        .iter()
        .position(|g| g == game)
        .expect("a game of the DAT");
    let roms: Vec<RepairRom> = roms_of(i.dat, index)
        .into_iter()
        .map(|rom| RepairRom {
            member: c
                .matched
                .roms
                .iter()
                .find(|m| m.rom == rom.name)
                .and_then(|m| m.member.clone()),
            rom,
            from: c.item.name.clone(),
        })
        .collect();
    // The original is deleted only once what was written reads as exactly this one ROM, from a loose file.
    let exact = match roms.as_slice() {
        [only] if !c.item.assessment.zip && only.member.is_none() => Some(only.rom.clone()),
        _ => None,
    };
    PlannedStep {
        item: p.item.key.clone(),
        step: RepairStep::Fill {
            target,
            ingest_root: c.root.clone(),
            item: c.item.id,
            name: c.item.name.clone(),
            seen: seen_file(&c.seen),
            sha256: c.item.assessment.sha256.clone(),
            roms,
            exact,
            loose,
        },
    }
}
