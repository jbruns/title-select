//! What a repair proposes, from an Audit's Payload Evidence and the assessed items of Ingest Folders, and the steps
//! that carry out what you choose. Nothing here touches a file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use title_select_api::additions::NewAddition;
use title_select_api::build::{ExpectedRom, invalid_name, kept_name};
use title_select_api::repair::{
    PlannedStep, RepairChoice, RepairItem, RepairKind, RepairRom, RepairStep, SeenFile,
};
use title_select_api::storage::{
    MULTI_ROM_STAYS_ZIP, Placement, RELEASE_STAYS_ZIP, Storage, StorageFormat, is_reserved_name,
    placed_path, placement, release_of, split_placed,
};
use title_select_api::{Entry, EvidenceLevel, Finding, Report};
use title_select_engine::FileStamp;
use title_select_engine::dat::{Dat, Rom};
use title_select_engine::payload::Checksums;

use super::{as_read, seen_file};
use crate::checks::Seen;
use crate::ingest::FillCandidate;
use crate::kept::name_key;
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
    /// What to add each file chosen to be added as an Addition as, by its item's key.
    pub additions: &'a BTreeMap<String, NewAddition>,
    /// Every `.cue` sheet in the cue packs of the Source Set's DAT.
    pub cues: &'a [CueSheet],
    /// Whether the Source Set's Coverage covers a Game: a Game outside it is never filled (ADR 0021).
    pub covers: &'a dyn Fn(&str) -> bool,
    /// Where each Placed Game is (ADR 0023), by its name: the Library Folder holding it and the folder its files are
    /// in there.
    pub placed: &'a HashMap<String, Located>,
    /// The Game each Placed Game's file is a file of, by its path in the Audit: never a source for another.
    pub placed_files: &'a HashMap<String, String>,
    /// The Placed Games' files that are CHDs, by their paths in the Audit.
    pub placed_chds: &'a HashSet<String>,
    /// The files a build laid out beside each release's placed CHDs, such as its `.m3u`, by release: each by its path
    /// as the Audit would name it, as seen now. A release is released with them.
    pub beside: &'a BTreeMap<String, Vec<(String, Seen)>>,
}

/// Where a file, or a Game's files, are: a Library Folder, by its id, and the folder in it holding them (empty for
/// its top), for a Placed Game; none for the Source Set's own folder.
pub(super) type Located = Option<(i64, String)>;

/// Where the file at `path`, as an Audit names it, is.
fn located(path: &str) -> Located {
    split_placed(path).map(|(folder, rel)| (folder, base_name_dir(rel).to_owned()))
}

/// Why a repair leaves a Placed Game it would have to convert.
const PLACED_WRITES: &str = "it is placed in a play library folder, where a repair renames, repacks, replaces or \
     completes its files but doesn't yet convert them";

/// Why a repair leaves a Game placed as a healthy CHD, and every file of it.
const PLACED_CHD: &str = "it is placed as a CHD in a play library folder, verified by the audit, and a repair \
     doesn't change a healthy CHD";

/// Why a repair leaves a Game placed as a CHD that the Audit didn't read as it is now.
const PLACED_CHD_UNVERIFIED: &str = "it is placed as a CHD in a play library folder, and the audit didn't read every \
     CHD of it as it is now, so it needs review. Run a full audit. A placed CHD is your only copy of its game, and no \
     repair can fix one in place: if one doesn't verify, the next repair sets the game's CHDs aside in your source \
     set's kept files. Then, with a good copy of the game in an ingest folder, the repair after the audit rebuilds it in \
     your source set";

/// What becomes of each disc of a placed release a repair releases (ADR 0023): every CHD of it is set aside, never
/// split from the rest, and nothing of it is extracted or deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freed {
    /// Its CHD, which the Audit found damaged, is set aside in the Source Set's kept files.
    Damaged,
    /// Its CHD, which the Audit verified, is set aside with the rest of its release, which is never split.
    Healthy,
    /// Its CHD is gone: nothing is set aside, and its placement is forgotten as another disc's CHD is set aside.
    Gone,
}

/// A disc of a placed release a repair releases.
#[derive(Debug, Clone)]
struct Released {
    release: String,
    /// Its CHD, by its path in the Audit.
    chd: String,
    freed: Freed,
    /// Whether the release has more than one disc.
    several: bool,
    /// The files laid out beside the release's CHDs, set aside with its first disc.
    beside: Vec<String>,
    /// Whether the Audit proved each CHD of the release it found damaged so, by reading what it holds: one chdman
    /// couldn't extract may be damaged, or chdman missing, stopped or short of space, so it is released only by the
    /// Operator's choice.
    proven: bool,
}

/// A `.cue` sheet in a cue pack in the store, with the checksums it was read as.
#[derive(Debug, Clone)]
pub(crate) struct CueSheet {
    /// The cue pack's SHA-256, which names its file in the store.
    pub pack: String,
    /// The cue pack's file name, as imported.
    pub file: String,
    /// The member of the cue pack it is.
    pub member: String,
    pub sums: Checksums,
}

impl CueSheet {
    /// Whether it is `rom`: its size and every checksum the DAT gives.
    fn is(&self, rom: &ExpectedRom) -> bool {
        title_select_engine::build::matches(&self.sums, rom)
    }
}

pub(super) struct Planned {
    pub items: Vec<RepairItem>,
    pub steps: Vec<PlannedStep>,
    pub notes: Vec<String>,
    /// The items that release a disc of a placed release, by key, unless the Operator chose to leave them.
    pub released: BTreeSet<String>,
}

/// A ROM, by its Game and name.
type RomKey = (String, String);

/// A file the Audit read the payload of, unchanged since.
struct Usable<'a> {
    entries: Vec<&'a Entry>,
    unreadable: bool,
}

impl Usable<'_> {
    /// The ROMs each of its payloads is.
    fn payloads(&self) -> impl Iterator<Item = Vec<RomKey>> {
        self.entries
            .iter()
            .filter(|e| {
                matches!(
                    e.finding,
                    Finding::Matched | Finding::Misnamed | Finding::Conflicting
                )
            })
            .map(|e| {
                e.roms
                    .iter()
                    .map(|r| (r.game.clone(), r.name.clone()))
                    .collect()
            })
    }
}

/// What the plan keeps of the ROMs the files it keeps aside hold.
struct Preserved<'a> {
    /// Every ROM of the DAT, by its Game and name.
    dat: &'a HashMap<RomKey, &'a Rom>,
    /// Every file the Audit found holding each ROM.
    held_by: &'a HashMap<RomKey, BTreeSet<&'a str>>,
    /// The Games an applied item makes whole.
    produced: BTreeSet<&'a str>,
    /// The files an applied item keeps aside.
    displaced: BTreeSet<&'a str>,
}

impl<'a> Preserved<'a> {
    /// What `proposed`, as chosen now, keeps.
    fn of(
        proposed: &'a [Proposed],
        dat: &'a HashMap<RomKey, &'a Rom>,
        held_by: &'a HashMap<RomKey, BTreeSet<&'a str>>,
    ) -> Self {
        let applied = |p: &&Proposed| p.item.chosen != RepairChoice::Leave;
        Preserved {
            dat,
            held_by,
            produced: proposed
                .iter()
                .filter(applied)
                .filter_map(|p| p.produces.as_deref())
                .collect(),
            displaced: proposed
                .iter()
                .filter(applied)
                .flat_map(|p| p.displaces.iter().map(|(path, _)| path.as_str()))
                .collect(),
        }
    }

    /// The ROMs `file` holds that would have no copy left once it is kept aside: no file that stays holds one, and no
    /// applied item writes its payload, as a ROM of whichever Game, that the DAT pins at least as closely as it.
    fn lost(&self, file: &Usable) -> BTreeSet<RomKey> {
        let mut lost = BTreeSet::new();
        for roms in file.payloads() {
            for rom in &roms {
                let held = self
                    .held_by
                    .get(rom)
                    .is_some_and(|by| by.iter().any(|p| !self.displaced.contains(p)));
                let pinned = |w: &RomKey| {
                    w == rom
                        || (self.dat.get(w).zip(self.dat.get(rom))).is_some_and(|(w, r)| pins(w, r))
                };
                let written = roms
                    .iter()
                    .any(|w| self.produced.contains(w.0.as_str()) && pinned(w));
                if !held && !written {
                    lost.insert(rom.clone());
                }
            }
        }
        lost
    }
}

/// Keeps every file holding a ROM of a present Game outside the Coverage where it is (ADR 0021): no other Game's item
/// keeps it aside, moves it, or deletes it once converted. Such a Game is never filled, so the file may be all it has.
/// An item that reads it copies from it instead; one that must put its Game where the file is is dropped, with why.
fn keep_uncovered(
    i: &Inputs,
    by_path: &BTreeMap<&str, Vec<&Entry>>,
    reported: &HashMap<&str, &title_select_api::report::Game>,
    proposed: &mut Vec<Proposed>,
    notes: &mut Vec<String>,
) {
    let kept_for: HashMap<&str, BTreeSet<&str>> = by_path
        .iter()
        .filter_map(|(path, entries)| {
            let games: BTreeSet<&str> = entries
                .iter()
                .flat_map(|e| &e.roms)
                .map(|r| r.game.as_str())
                .filter(|g| {
                    !(i.covers)(g)
                        && reported
                            .get(g)
                            .is_some_and(|r| r.status != title_select_api::GameStatus::Missing)
                })
                .collect();
            (!games.is_empty()).then_some((*path, games))
        })
        .collect();
    if kept_for.is_empty() {
        return;
    }
    proposed.retain_mut(|p| {
        let Some(game) = p.item.game.clone() else {
            return true;
        };
        let others = |path: &str| -> Vec<&str> {
            kept_for
                .get(path)
                .map(|games| games.iter().copied().filter(|g| *g != game).collect())
                .unwrap_or_default()
        };
        let mut kept: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        let mut keep = |path: &str| {
            let games = others(path);
            let found = !games.is_empty();
            if found {
                kept.insert(path.to_owned(), games);
            }
            found
        };
        // Where the item puts its Game: a file there that must stay leaves it nowhere to go.
        let mut targets: Vec<&str> = p.target.iter().map(|(t, _)| t.as_str()).collect();
        if let Some(Proposal::Folder { parts }) = &p.proposal {
            targets.extend(parts.iter().map(|part| match part {
                Part::Build { target, .. } | Part::Fill { target, .. } => target.as_str(),
                Part::Rename { to, .. } => to.as_str(),
            }));
        }
        if let Some(path) = targets.into_iter().find(|t| keep(t)) {
            notes.push(format!(
                "{game} is left as it is: {path} is in the way, and {} still holds it, outside your coverage",
                kept[path].join(", ")
            ));
            return false;
        }
        p.displaces.retain(|(path, _)| !keep(path));
        p.converts.retain(|path| !keep(path));
        let member_of = |from: &str, rom: &ExpectedRom| {
            by_path.get(from).and_then(|entries| {
                entries
                    .iter()
                    .find(|e| e.roms.iter().any(|r| r.game == game && r.name == rom.name))
                    .and_then(|e| e.member.clone())
            })
        };
        let mut copied = None;
        match &mut p.proposal {
            Some(Proposal::Rename { from, roms }) if keep(from) => {
                copied = Some(from.clone());
                let roms = roms
                    .iter()
                    .map(|rom| RepairRom {
                        rom: rom.clone(),
                        from: from.clone(),
                        member: member_of(from, rom),
                        cue_pack: None,
                    })
                    .collect();
                p.proposal = Some(Proposal::Build { roms });
            }
            Some(Proposal::Folder { parts }) => {
                for part in parts.iter_mut() {
                    if let Part::Rename { from, to, rom } = part
                        && keep(from)
                    {
                        *part = Part::Build {
                            target: to.clone(),
                            rom: RepairRom {
                                rom: rom.clone(),
                                from: from.clone(),
                                member: None,
                                cue_pack: None,
                            },
                        };
                    }
                }
            }
            _ => {}
        }
        if let (Some(from), Some((target, _))) = (copied, &p.target) {
            p.item.summary = format!("write {target} from {from}");
        }
        if !kept.is_empty() {
            let stays: Vec<String> = kept
                .iter()
                .map(|(path, games)| format!("{path} stays for {}", games.join(", ")))
                .collect();
            p.item.summary = format!(
                "{}; {}, outside your coverage",
                p.item.summary,
                stays.join("; ")
            );
        }
        true
    });
}

/// Whether bytes that are `w`, as the DAT gives it, are also `r`: the same size, with every checksum `r` gives also
/// given for `w`, and alike.
fn pins(w: &Rom, r: &Rom) -> bool {
    let covered = |w: &Option<String>, r: &Option<String>| match (w, r) {
        (_, None) => true,
        (Some(w), Some(r)) => w.eq_ignore_ascii_case(r),
        (None, Some(_)) => false,
    };
    w.size == r.size
        && covered(&w.crc, &r.crc)
        && covered(&w.md5, &r.md5)
        && covered(&w.sha1, &r.sha1)
        && covered(&w.sha256, &r.sha256)
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
    /// A loose folder, written file by file: each ROM's file built or renamed into place, unless it is already.
    Folder { parts: Vec<Part> },
    /// Filled from the ingest candidate at this index.
    Fill { candidate: usize },
}

/// How one file of a loose folder is put in place.
enum Part {
    /// Written from a ROM read in the Source Set or a cue pack.
    Build { target: String, rom: RepairRom },
    /// A loose file that is the ROM, renamed into place once it is read again as it.
    Rename {
        from: String,
        to: String,
        rom: ExpectedRom,
    },
    /// Moved or extracted into place from the ingest candidate at this index.
    Fill {
        target: String,
        rom: ExpectedRom,
        candidate: usize,
    },
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
    /// Where it writes its Game, and whether as a loose file; none for a loose folder, whose parts say.
    target: Option<(String, bool)>,
    /// The originals a conversion reads, which it may delete once its converted files are verified.
    converts: Vec<String>,
    /// The disc of a placed release it releases, and what becomes of its CHD.
    release: Option<Released>,
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

pub(super) fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder holding `path`, relative to the Source Set's folder.
fn base_name_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Whether a ROM named `name` is a `.cue` sheet.
fn is_cue(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("cue"))
}

/// How a repair stores each Game of the DAT, by its index, with why it is not stored as the policy says; none for a
/// Game with no ROMs. Every disc of a release is stored alike, whatever the policy: when one can't be stored as it
/// says, as when one disc has more ROMs than a loose file holds, every disc stays TorrentZip.
fn placements_of(i: &Inputs) -> Vec<Option<(Placement, Option<&'static str>)>> {
    let mut placed: Vec<Option<(Placement, Option<&'static str>)>> = (0..i.dat.games.len())
        .map(|index| {
            let roms = roms_of(i.dat, index);
            let names: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
            (!roms.is_empty()).then(|| placement(i.storage, &i.dat.games[index], &names))
        })
        .collect();
    let mut formats: BTreeMap<String, HashSet<StorageFormat>> = BTreeMap::new();
    for (p, game) in placed.iter().zip(&i.dat.games) {
        if let Some((placed, _)) = p {
            formats
                .entry(release_of(game))
                .or_default()
                .insert(placed.format());
        }
    }
    for (p, game) in placed.iter_mut().zip(&i.dat.games) {
        if let Some((placed, _)) = p
            && placed.format() != StorageFormat::TorrentZip
            && formats[&release_of(game)].len() > 1
        {
            *p = Some((
                Placement::TorrentZip(format!("{game}.zip")),
                Some(RELEASE_STAYS_ZIP),
            ));
        }
    }
    placed
}

/// Why `items`, as chosen, would repair one disc of a release while leaving another, which would store the discs of
/// one game apart; none when every disc of each release is chosen alike.
pub(crate) fn split_release(items: &[RepairItem]) -> Option<String> {
    let mut seen: BTreeMap<String, (&str, bool)> = BTreeMap::new();
    for item in items.iter().filter(|it| it.key.starts_with("game:")) {
        let Some(game) = item.game.as_deref() else {
            continue;
        };
        let applied = item.chosen != RepairChoice::Leave;
        match seen.get(&release_of(game)) {
            Some((other, was)) if *was != applied => {
                let (repaired, left) = match applied {
                    true => (game, *other),
                    false => (*other, game),
                };
                return Some(format!(
                    "{left} would be left as it is while {repaired} is repaired, and every disc of a game is \
                     repaired alike, so they are stored alike; choose the same for both"
                ));
            }
            Some(_) => {}
            None => {
                seen.insert(release_of(game), (game, applied));
            }
        }
    }
    None
}

/// How the files holding game `game`'s ROMs, by the Audit, are stored; `None` for a file of it whose format can't
/// be told, as an unreadable one where the Game's ZIP, loose folder (the folder itself, as an Audit reports an
/// unreadable directory, or a file in it) or a ROM of it would be.
fn stored_formats(
    by_path: &BTreeMap<&str, Vec<&Entry>>,
    dat: &Dat,
    index: usize,
) -> HashSet<Option<StorageFormat>> {
    let game = dat.games[index].as_str();
    let zip = format!("{game}.zip");
    let folder = format!("{game}/");
    let roms = roms_of(dat, index);
    by_path
        .iter()
        .filter_map(|(path, entries)| {
            let holds = entries.iter().any(|e| match e.finding {
                Finding::Matched | Finding::Misnamed => e.roms.iter().any(|r| r.game == game),
                Finding::Mismatched => e.named_rom.as_ref().is_some_and(|r| r.game == game),
                _ => false,
            });
            if !holds {
                let name = base_name(path);
                let unreadable = entries.iter().any(|e| e.finding == Finding::Unreadable)
                    && (name == zip
                        || name == game
                        || path.starts_with(&folder)
                        || roms.iter().any(|r| r.name == name));
                return unreadable.then_some(None);
            }
            // An archive is a ZIP wherever it is; a file nested in a folder is a loose folder's only when it is
            // the Game's.
            let archive = entries.iter().any(|e| e.member.is_some()) || name_is_zip(path);
            Some(Some(match (archive, path.contains('/')) {
                (true, _) => StorageFormat::TorrentZip,
                (false, true) if path.starts_with(&folder) => StorageFormat::LooseFolder,
                (false, true) => return Some(None),
                (false, false) => StorageFormat::Loose,
            }))
        })
        .collect()
}

/// Whether `path` names a ZIP archive.
fn name_is_zip(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("zip"))
}

/// A game item, by its index in `proposed`, that would store its disc otherwise than another disc of its release no
/// applied item repairs, which stays stored as it is, with that other disc's name; none when no release is split. A
/// disc already stored as the policy says, `good`, is taken as stored so.
fn release_split_by_unrepaired(
    i: &Inputs,
    by_path: &BTreeMap<&str, Vec<&Entry>>,
    good: &BTreeSet<String>,
    proposed: &[Proposed],
    released: &BTreeMap<String, Vec<(String, Released)>>,
) -> Option<(usize, String)> {
    let placements = placements_of(i);
    let repaired: HashMap<&str, usize> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| p.item.key.starts_with("game:") && p.item.chosen != RepairChoice::Leave)
        .filter_map(|(n, p)| Some((p.produces.as_deref()?, n)))
        .collect();
    // Each disc of each release: the item repairing it, if any, its name, and how it is stored once repaired.
    type Disc<'a> = (Option<usize>, &'a str, HashSet<Option<StorageFormat>>);
    let mut releases: BTreeMap<String, Vec<Disc>> = BTreeMap::new();
    for (index, game) in i.dat.games.iter().enumerate() {
        // A release released from where it is placed is released whole.
        if released.contains_key(&release_of(game)) {
            continue;
        }
        let formats = match (repaired.get(game.as_str()), &placements[index]) {
            (Some(_), Some((placed, _))) => HashSet::from([Some(placed.format())]),
            (None, Some((placed, _))) if good.contains(game) => {
                HashSet::from([Some(placed.format())])
            }
            _ => stored_formats(by_path, i.dat, index),
        };
        if !formats.is_empty() {
            releases.entry(release_of(game)).or_default().push((
                repaired.get(game.as_str()).copied(),
                game,
                formats,
            ));
        }
    }
    releases.values().find_map(|discs| {
        discs.iter().find_map(|(repairing, _, formats)| {
            let repairing = (*repairing)?;
            discs
                .iter()
                .find(|(other, _, stays)| other.is_none() && stays != formats)
                .map(|(_, stays, _)| (repairing, stays.to_string()))
        })
    })
}

/// Leaves every other disc of the release of game item `left`, which is left, saying why.
fn leave_release(proposed: &mut [Proposed], left: usize) {
    let Some(game) = proposed[left].item.game.clone() else {
        return;
    };
    let release = release_of(&game);
    for (n, p) in proposed.iter_mut().enumerate() {
        if n != left
            && p.item.key.starts_with("game:")
            && p.item.chosen != RepairChoice::Leave
            && p.item
                .game
                .as_deref()
                .is_some_and(|g| release_of(g) == release)
        {
            p.item.chosen = RepairChoice::Leave;
            p.item.note = Some(format!(
                "left as it is: {game}, another disc of the same game, is left as it is, and every disc of a game \
                 is repaired alike"
            ));
        }
    }
}

/// Whether the placed CHD at `path` is healthy: every Entry of it Matched on Payload Evidence, and its Game Complete
/// on them.
fn healthy_chd(i: &Inputs, path: &str) -> bool {
    let mut entries = i
        .report
        .entries
        .iter()
        .filter(|e| e.path == path)
        .peekable();
    entries.peek().is_some()
        && entries.all(|e| {
            e.finding == Finding::Matched
                && e.evidence
                    .as_ref()
                    .is_some_and(|ev| ev.level == EvidenceLevel::Payload)
        })
        && i.placed_files.get(path).is_some_and(|game| {
            i.report.games.iter().any(|g| {
                g.name == *game
                    && g.status == title_select_api::GameStatus::Complete
                    && g.claimed_roms.is_empty()
            })
        })
}

/// The placed releases a repair releases (ADR 0023), each disc of each with what becomes of its CHD: every release
/// one of whose CHDs the Audit read as it is now and found damaged, or one of whose CHDs is gone, while each other CHD
/// of it the Audit read as it is now. A release is released whole: every CHD of it still there is set aside, healthy
/// or not, and a gone one leaves nothing to set aside; one every CHD of which is gone is released for the files laid
/// out beside them, and only when there are some. A release one of whose CHDs the Audit didn't read as it is now, as
/// when it changed since or was read only by its claim, is not released, and needs review.
fn releases(
    i: &Inputs,
    by_path: &BTreeMap<&str, Vec<&Entry>>,
    usable: &BTreeMap<&str, Usable>,
) -> BTreeMap<String, Vec<(String, Released)>> {
    let mut discs: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for path in i.placed_chds {
        if let Some(game) = i.placed_files.get(path) {
            discs
                .entry(release_of(game))
                .or_default()
                .push((game.clone(), path.clone()));
        }
    }
    let mut released = BTreeMap::new();
    for (release, mut chds) in discs {
        chds.sort();
        let mut freed = Vec::new();
        let mut proven = true;
        for (game, path) in &chds {
            let read = by_path.contains_key(path.as_str());
            let how = match (read, usable.contains_key(path.as_str())) {
                (false, _) if !i.files.contains_key(path) => Some(Freed::Gone),
                (true, true) if healthy_chd(i, path) => Some(Freed::Healthy),
                (true, true) => {
                    proven &= usable.get(path.as_str()).is_some_and(|u| !u.unreadable);
                    Some(Freed::Damaged)
                }
                _ => None,
            };
            freed.push((game, path, how));
        }
        let unsettled = freed.iter().any(|(_, _, how)| how.is_none());
        // A disc of the release placed no longer, and missing from the Source Set, was set aside by a repair that
        // stopped short of the rest of it: the release is released again, whole.
        let parted = i.dat.games.iter().any(|g| {
            release_of(g) == release
                && !chds.iter().any(|(game, _)| game == g)
                && !i.placed_files.values().any(|placed| placed == g)
                && i.report
                    .games
                    .iter()
                    .any(|s| s.name == *g && s.status == title_select_api::GameStatus::Missing)
        });
        let damaged = parted
            || freed
                .iter()
                .any(|(_, _, how)| matches!(how, Some(Freed::Damaged | Freed::Gone)));
        // A release every CHD of which is gone is released only for what is laid out beside them: its records are
        // dropped as the last of that is set aside. With nothing beside them, each disc is filled as any Placed Game
        // with no file left is.
        let present = freed
            .iter()
            .any(|(_, _, how)| matches!(how, Some(Freed::Damaged | Freed::Healthy)));
        if unsettled || !damaged || !(present || i.beside.contains_key(&release)) {
            continue;
        }
        let several = i
            .dat
            .games
            .iter()
            .filter(|g| release_of(g) == release)
            .count()
            > 1;
        let mut beside = i
            .beside
            .get(&release)
            .map(|files| files.iter().map(|(path, _)| path.clone()).collect())
            .unwrap_or_default();
        let discs = freed
            .into_iter()
            .map(|(game, chd, how)| {
                (
                    game.clone(),
                    Released {
                        release: release.clone(),
                        chd: chd.clone(),
                        freed: how.expect("settled"),
                        several,
                        beside: std::mem::take(&mut beside),
                        proven,
                    },
                )
            })
            .collect();
        released.insert(release, discs);
    }
    released
}

/// What releasing disc `game` of a placed release, as `r` says, means for the Operator.
fn released_note(game: &str, r: &Released) -> String {
    let (chd, release) = (&r.chd, &r.release);
    let found = match r.freed {
        Freed::Damaged if !r.proven => format!(
            "{chd} is the only copy of {game}, and the audit couldn't read it: chdman couldn't extract it. A damaged CHD \
             does that, but so does chdman missing or stopped, or too little free space, so it is left as it is unless \
             you choose otherwise. Audit it fully again once chdman can run; if it still can't be read, choose to set \
             it aside."
        ),
        Freed::Damaged => format!("{chd} is the only copy of {game}, and it didn't verify."),
        Freed::Healthy if !r.proven => format!(
            "{chd} is the only copy of {game}. It verified, but the audit couldn't read another disc of {release}, so \
             every disc is left as it is unless you choose otherwise."
        ),
        Freed::Healthy => format!(
            "{chd} is the only copy of {game}. It verified, but another disc of {release} didn't, or is gone."
        ),
        Freed::Gone => {
            format!("{chd}, the only copy of {game}, is gone: there is nothing of it to set aside.")
        }
    };
    // Left by default unless the damage is proven, so what setting aside does is said as the Operator's choice.
    let (this, then) = match r.proven {
        true => (
            "this sets",
            format!("This repair only sets files aside, so {game} is then missing"),
        ),
        false => (
            "if you choose to, this sets",
            format!("A repair only sets files aside, so once they are, {game} is missing"),
        ),
    };
    let aside = match (r.several, r.freed) {
        // A release of one disc whose CHD is gone is released only for the files laid out beside it, and by default.
        (false, Freed::Gone) => {
            "This sets the files laid out beside it aside in your source set's kept files, where you can delete \
             them once you no longer need them"
                .to_owned()
        }
        (false, _) => format!(
            "A repair can't fix a CHD in place, so {this} it aside in your source set's kept files, where you can \
             delete it once you no longer need it"
        ),
        (true, _) => format!(
            "A repair can't fix a CHD in place, and a repair never sets one disc of a game aside alone, so {this} \
             every disc of {release} aside in your source set's kept files, each CHD as it is, with the files laid out beside them, \
             where you can delete them once you no longer need them"
        ),
    };
    // An ingest folder's copy of a healthy disc was assessed as already held while its CHD was placed.
    let rescan = match r.freed {
        Freed::Healthy => {
            " A copy of it in an ingest folder found while it was placed counts as already held, so scan that folder \
             again after the audit."
        }
        _ => "",
    };
    format!(
        "{found} {aside}. {then}: with a good copy of it in an ingest folder, the next repair after the audit this one \
         queues rebuilds it in your source set, and the next build places it again.{rescan}"
    )
}

fn roms_of(dat: &Dat, game: usize) -> Vec<ExpectedRom> {
    dat.roms
        .iter()
        .filter(|r| r.game == game)
        .map(|rom| dat.expected(rom))
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
        addition: None,
        addition_rom: None,
        addition_target: None,
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
    // The placed releases released, for which nothing else is planned: a repair that releases one only sets its files
    // aside, and nothing is ever read from their CHDs or filled for any Game of one, which the next repair does.
    let released = releases(i, &by_path, &usable);
    let released_paths: HashSet<&str> = released
        .values()
        .flatten()
        .map(|(_, r)| r.chd.as_str())
        .collect();
    referenced.extend(
        usable
            .keys()
            .copied()
            .filter(|path| released_paths.contains(path)),
    );

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
                RepairChoice::AddAddition => {
                    return Err(Error::Refused(format!(
                        "{key} cannot be set to {}",
                        chosen.label()
                    )));
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
                    addition: None,
                    addition_rom: None,
                    addition_target: None,
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
                converts: Vec::new(),
                release: None,
            });
        }
    }

    let mut stays_zip = 0;
    let mut used_candidates: BTreeSet<i64> = BTreeSet::new();
    let mut good: BTreeSet<String> = BTreeSet::new();
    let placements = placements_of(i);
    let reported: HashMap<&str, &title_select_api::report::Game> = i
        .report
        .games
        .iter()
        .map(|g| (g.name.as_str(), g))
        .collect();
    // Where each Game is: where it is placed, or else where every file the Audit found holding a ROM of it is.
    let mut found_at: HashMap<&str, BTreeSet<Located>> = HashMap::new();
    for ((game, _), paths) in &held_by {
        found_at
            .entry(game.as_str())
            .or_default()
            .extend(paths.iter().map(|p| located(p)));
    }
    for (index, game) in i.dat.games.iter().enumerate() {
        let roms = roms_of(i.dat, index);
        let Some((placed, why_zip)) = &placements[index] else {
            continue;
        };
        if released.contains_key(&release_of(game)) {
            continue;
        }
        let location: Located = match (i.placed.get(game), found_at.get(game.as_str())) {
            (Some(at), _) => at.clone(),
            (None, None) => None,
            (None, Some(at)) if at.len() == 1 => at.iter().next().cloned().flatten(),
            (None, Some(_)) => {
                notes.push(format!(
                    "{game} is left as it is: its files are both in the source set's folder and placed in a play \
                     library folder"
                ));
                continue;
            }
        };
        // A Placed Game stays stored as it is placed, under the names the DAT gives: a TorrentZip, its one ROM loose,
        // or its ROMs loose together, in the folder of the Library Folder it is in.
        let placed_as;
        let (placed, why_zip) = match &location {
            None => (placed, why_zip),
            Some((folder, dir)) => {
                let at = |name: &str| match dir.is_empty() {
                    true => placed_path(*folder, name),
                    false => placed_path(*folder, &format!("{dir}/{name}")),
                };
                let held: BTreeSet<&str> = roms
                    .iter()
                    .filter_map(|r| held_by.get(&(game.clone(), r.name.clone())))
                    .flatten()
                    .copied()
                    .collect();
                placed_as = match (held.iter().next(), roms.as_slice()) {
                    (Some(only), _) if held.len() == 1 && name_is_zip(only) => {
                        Placement::TorrentZip(at(&format!("{game}.zip")))
                    }
                    (_, [rom]) => Placement::Loose(at(&rom.name)),
                    _ => Placement::Folder {
                        folder: at("").trim_end_matches('/').to_owned(),
                        files: roms.iter().map(|r| at(&r.name)).collect(),
                    },
                };
                (&placed_as, &None)
            }
        };
        match why_zip {
            Some(MULTI_ROM_STAYS_ZIP) => stays_zip += 1,
            Some(why) => notes.push(format!("{game}: {why}")),
            None => {}
        }
        let targets = placed.files();
        let folder = match placed {
            Placement::Folder { folder, .. } => Some(folder.as_str()),
            _ => None,
        };
        let loose = matches!(placed, Placement::Loose(_));
        // What a game item names: its file, or its loose folder.
        let target = match folder {
            Some(folder) => format!("{folder}/"),
            None => targets[0].clone(),
        };
        // A Placed Game's target is named as one in the Source Set's folder is, in the folder it is placed in.
        let own = match &location {
            Some(_) => base_name(&target),
            None => target.as_str(),
        };
        if folder.is_none()
            && let Some(why) = invalid_name(own).or_else(|| {
                is_reserved_name(own)
                    .then(|| format!("{own} is a name Title Select keeps its own work under"))
            })
        {
            notes.push(format!("{game} is left as it is: {why}"));
            continue;
        }
        let is_target = |path: &str| targets.iter().any(|t| t == path);
        // A Game outside the Coverage keeps what it has, renamed and repacked, but none of its missing ROMs is filled:
        // not from another Game's file, a cue pack or an Ingest Folder.
        let outside = !(i.covers)(game);
        let withheld = |rom: &str| {
            outside
                && reported.get(game.as_str()).is_none_or(|g| {
                    g.status == title_select_api::GameStatus::Missing
                        || g.missing_roms.iter().any(|m| m == rom)
                })
        };
        // Each ROM's best copy: matched in the Game's own file first, then matched elsewhere, then misnamed, then a
        // conflicting entry you chose to take as the ROM it matches.
        let mut sources: Vec<Option<(u8, &str, &Entry)>> = vec![None; roms.len()];
        for (path, file) in &usable {
            if file.unreadable
                || kept_conflicts.contains(path)
                || left_conflicts.contains(path)
                || located(path) != location
                || i.placed_files.get(*path).is_some_and(|of| of != game)
            {
                continue;
            }
            for entry in &file.entries {
                let rank = match entry.finding {
                    Finding::Matched if is_target(path) => 0,
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
                        && !withheld(&rom.name)
                        && sources[n].is_none_or(|(best, ..)| rank < best)
                    {
                        sources[n] = Some((rank, path, entry));
                    }
                }
            }
        }
        // A `.cue` sheet no file of the Source Set holds, from a cue pack.
        let cues: Vec<Option<&CueSheet>> = roms
            .iter()
            .zip(&sources)
            .map(|(rom, source)| match source {
                None if is_cue(&rom.name) && !withheld(&rom.name) => {
                    i.cues.iter().find(|c| c.is(rom))
                }
                _ => None,
            })
            .collect();
        // For a loose folder, each ROM neither the Source Set nor a cue pack holds, from an Ingest Folder item that holds
        // it, with its payload read: a disc's tracks may arrive one by one.
        let ingested: Vec<Option<usize>> = roms
            .iter()
            .enumerate()
            .map(|(n, rom)| {
                if folder.is_none() || sources[n].is_some() || cues[n].is_some() || outside {
                    return None;
                }
                i.candidates.iter().position(|c| {
                    c.matched.game == *game
                        && !used_candidates.contains(&c.item.id)
                        && c.matched.roms.iter().any(|m| m.rom == rom.name)
                })
            })
            .collect();
        // Whether ROM `n` of the Game has nowhere to come from.
        let unsourced =
            |n: usize| sources[n].is_none() && cues[n].is_none() && ingested[n].is_none();
        let occupants: Vec<&str> = targets
            .iter()
            .filter(|t| i.files.contains_key(*t))
            .map(String::as_str)
            .collect();
        let occupant = occupants.first().copied().filter(|_| folder.is_none());
        let left_in_way = |path: &str| {
            format!(
                "{game} is left as it is: {path} is in the way, and it holds a conflicting entry you chose to leave; \
                 choose to keep that entry aside to repair it"
            )
        };
        if let Some(occupant) = occupants.iter().find(|o| left_conflicts.contains(**o)) {
            notes.push(left_in_way(occupant));
            continue;
        }
        if (0..roms.len()).all(|n| !unsourced(n)) || ingested.iter().any(Option::is_some) {
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
            // A loose file that is exactly ROM `rom` of the Game, Matched under its name.
            let exact_loose = |path: &str, rom: &ExpectedRom| {
                usable.get(path).is_some_and(|file| {
                    matches!(file.entries.as_slice(), [e] if e.member.is_none()
                        && e.finding == Finding::Matched
                        && e.roms.iter().any(|r| r.game == *game && r.name == rom.name))
                })
            };
            let exact = |path: &str| exact_as(path, loose);
            let in_place = match folder {
                Some(_) => roms
                    .iter()
                    .zip(&targets)
                    .all(|(rom, t)| exact_loose(t, rom)),
                None => exact(&target),
            };
            if in_place {
                good.insert(game.clone());
                for t in &targets {
                    referenced.insert(usable.get_key_value(t.as_str()).expect("usable").0);
                }
                continue;
            }
            let local: Vec<(u8, &str, &Entry)> = sources.iter().flatten().copied().collect();
            let files: BTreeSet<&str> = local.iter().map(|(_, path, _)| *path).collect();
            referenced.extend(files.iter().copied());
            // Each file in the way: kept aside, unless it is already the ROM a loose folder holds there.
            let mut blocked = None;
            for (n, t) in targets.iter().enumerate() {
                if !i.files.contains_key(t)
                    || (folder.is_some() && (exact_loose(t, &roms[n]) || unsourced(n)))
                {
                    continue;
                }
                match usable.get_key_value(t.as_str()) {
                    Some((key, file)) if file.unreadable && ingested[n].is_some() => {
                        referenced.insert(key);
                    }
                    None => {
                        blocked = Some(format!(
                            "{game} is left as it is: {t} is in the way, and the audit did not read it as it is \
                             now"
                        ));
                    }
                    Some((_, file)) if file.unreadable => {
                        blocked = Some(format!(
                            "{game} is left as it is: {t} is in the way, and it is unreadable; it is replaced \
                             only from an ingest folder"
                        ));
                    }
                    Some((key, _)) => {
                        referenced.insert(key);
                    }
                }
            }
            if let Some(why) = blocked {
                notes.push(why);
                continue;
            }
            let rom_from = |n: usize| -> RepairRom {
                match (sources[n], cues[n]) {
                    (Some((_, path, entry)), _) => RepairRom {
                        rom: roms[n].clone(),
                        from: path.to_string(),
                        member: entry.member.clone(),
                        cue_pack: None,
                    },
                    (None, Some(cue)) => RepairRom {
                        rom: roms[n].clone(),
                        from: format!("cue pack {}", cue.file),
                        member: Some(cue.member.clone()),
                        cue_pack: Some(cue.pack.clone()),
                    },
                    (None, None) => unreachable!("a ROM written from the source set or a cue pack"),
                }
            };
            // A `.cue` sheet from a cue pack fills one the Source Set lacks, or replaces one that is not the DAT's.
            let cued: Vec<&str> = roms
                .iter()
                .zip(&cues)
                .filter(|(_, c)| c.is_some())
                .map(|(rom, _)| rom.name.as_str())
                .collect();
            let packs: BTreeSet<&str> = cues.iter().flatten().map(|c| c.file.as_str()).collect();
            let cue_kind = || {
                let mismatched = i.report.entries.iter().any(|e| {
                    e.finding == Finding::Mismatched
                        && e.named_rom
                            .as_ref()
                            .is_some_and(|r| r.game == *game && cued.contains(&r.name.as_str()))
                });
                let packs = packs.iter().copied().collect::<Vec<_>>().join(", ");
                match mismatched {
                    true => (
                        RepairKind::Replace,
                        format!(
                            "replace {} of {target} from cue pack {packs}",
                            cued.join(", ")
                        ),
                    ),
                    false => (
                        RepairKind::Fill,
                        format!("fill {} of {target} from cue pack {packs}", cued.join(", ")),
                    ),
                }
            };
            if let Some(folder) = folder {
                let mut parts = Vec::new();
                let mut displaces: Vec<(String, String)> = Vec::new();
                let mut renamed: BTreeSet<&str> = BTreeSet::new();
                let mut missing: Vec<&str> = Vec::new();
                for (n, rom) in roms.iter().enumerate() {
                    let to = &targets[n];
                    if exact_loose(to, rom) {
                        continue;
                    }
                    if unsourced(n) {
                        missing.push(&rom.name);
                        continue;
                    }
                    if i.files.contains_key(to) {
                        displaces.push((to.clone(), format!("{to} was written in its place")));
                    }
                    if let Some(candidate) = ingested[n] {
                        parts.push(Part::Fill {
                            target: to.clone(),
                            rom: rom.clone(),
                            candidate,
                        });
                        continue;
                    }
                    match sources[n] {
                        // A loose file that is this ROM and holds nothing another Game needs is moved, not copied.
                        Some((_, path, entry))
                            if entry.member.is_none()
                                && matches!(
                                    entry.finding,
                                    Finding::Matched | Finding::Misnamed
                                )
                                && !is_target(path)
                                && !renamed.contains(path)
                                && usable[path].entries.len() == 1
                                && entry.roms.iter().all(|r| r.game == *game) =>
                        {
                            renamed.insert(path);
                            parts.push(Part::Rename {
                                from: path.to_string(),
                                to: to.clone(),
                                rom: rom.clone(),
                            });
                        }
                        _ => parts.push(Part::Build {
                            target: to.clone(),
                            rom: rom_from(n),
                        }),
                    }
                }
                let read: BTreeSet<&str> = parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Build { rom, .. } if rom.cue_pack.is_none() => {
                            Some(rom.from.as_str())
                        }
                        _ => None,
                    })
                    .filter(|path| !renamed.contains(path))
                    .collect();
                let fills: Vec<&FillCandidate> = parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Fill { candidate, .. } => Some(&i.candidates[*candidate]),
                        _ => None,
                    })
                    .fold(Vec::new(), |mut found, c| {
                        if !found.iter().any(|f| f.item.id == c.item.id) {
                            found.push(c);
                        }
                        found
                    });
                // Exactly the Game as one TorrentZip, every file of the folder written from it.
                let converts: Vec<String> = match read.iter().next() {
                    Some(&zip)
                        if read.len() == 1
                            && fills.is_empty()
                            && cued.is_empty()
                            && parts.len() == roms.len()
                            && renamed.is_empty()
                            && exact_as(zip, false) =>
                    {
                        vec![zip.to_owned()]
                    }
                    _ => Vec::new(),
                };
                for path in &read {
                    if !displaces.iter().any(|(p, _)| p == path) {
                        let why = match converts.is_empty() {
                            true => format!("its ROMs were written to {target}"),
                            false => {
                                format!("it was converted to {target}, stored as a loose folder")
                            }
                        };
                        displaces.push((path.to_string(), why));
                    }
                }
                let from: Vec<&str> = files.iter().copied().collect();
                let (kind, mut summary) = if !converts.is_empty() {
                    (
                        RepairKind::Convert,
                        format!(
                            "convert {} to {target}, stored as a loose folder",
                            converts[0]
                        ),
                    )
                } else if !fills.is_empty() {
                    let items: Vec<String> = fills
                        .iter()
                        .map(|c| format!("{} in ingest folder {}", c.item.name, c.root))
                        .collect();
                    let filled: Vec<&str> = parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Fill { rom, .. } => Some(rom.name.as_str()),
                            _ => None,
                        })
                        .collect();
                    match displaces.is_empty() {
                        true => (
                            RepairKind::Fill,
                            format!(
                                "write {} into {target} from {}",
                                filled.join(", "),
                                items.join(", ")
                            ),
                        ),
                        false => (
                            RepairKind::Replace,
                            format!(
                                "keep {} aside, and write {} into {target} from {}",
                                displaces
                                    .iter()
                                    .map(|(path, _)| path.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                filled.join(", "),
                                items.join(", ")
                            ),
                        ),
                    }
                } else if !cued.is_empty() {
                    cue_kind()
                } else if local.iter().any(|(rank, ..)| *rank >= 2) {
                    (
                        RepairKind::Rename,
                        format!(
                            "write {target}, under the names the DAT gives, from {}",
                            from.join(", ")
                        ),
                    )
                } else if parts.iter().all(|p| matches!(p, Part::Rename { .. })) {
                    (
                        RepairKind::Rename,
                        format!("move {} into {target}", from.join(", ")),
                    )
                } else {
                    (
                        RepairKind::Repack,
                        format!(
                            "repack {} as {target}, stored as a loose folder",
                            from.join(", ")
                        ),
                    )
                };
                if !missing.is_empty() {
                    let still = format!(
                        "{} still missing: no file of the source set, cue pack or ingest folder holds it",
                        missing.join(", ")
                    );
                    summary = format!("{summary}; {still}");
                    notes.push(format!("{game}: {still}"));
                }
                used_candidates.extend(fills.iter().map(|c| c.item.id));
                proposed.push(Proposed {
                    item: game_item(game, folder, kind, summary),
                    proposal: Some(Proposal::Folder { parts }),
                    displaces,
                    produces: missing.is_empty().then(|| game.clone()),
                    checked: true,
                    target: None,
                    converts,
                    release: None,
                });
                continue;
            }
            let single = files
                .iter()
                .next()
                .filter(|_| files.len() == 1 && cued.is_empty())
                .copied();
            let format = match loose {
                true => StorageFormat::Loose,
                false => StorageFormat::TorrentZip,
            };
            let all_roms = || (0..roms.len()).map(rom_from).collect::<Vec<_>>();
            // The files of the Game as a loose folder, when that is what the Source Set holds it as, each exactly
            // its ROM: converted into this format.
            let as_folder: Vec<String> = {
                let names: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
                let folder_storage = Storage {
                    format: StorageFormat::LooseFolder,
                    ..i.storage
                };
                match placement(folder_storage, game, &names).0 {
                    Placement::Folder { files: f, .. }
                        if f.len() > 1
                            && cued.is_empty()
                            && f.iter().map(String::as_str).collect::<BTreeSet<_>>() == files
                            && roms
                                .iter()
                                .zip(&f)
                                .all(|(rom, path)| exact_loose(path, rom)) =>
                    {
                        f
                    }
                    _ => Vec::new(),
                }
            };
            let mut converts = Vec::new();
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
                    converts = vec![from.to_owned()];
                    (
                        Proposal::Build { roms: all_roms() },
                        RepairKind::Convert,
                        format!("convert {from} to {target}, stored {}", format.label()),
                        vec![(
                            from.to_owned(),
                            format!("it was converted to {target}, stored {}", format.label()),
                        )],
                    )
                }
                _ if !as_folder.is_empty() => {
                    converts = as_folder.clone();
                    let folder = base_name_dir(&as_folder[0]);
                    (
                        Proposal::Build { roms: all_roms() },
                        RepairKind::Convert,
                        format!("convert {folder}/ to {target}, stored {}", format.label()),
                        as_folder
                            .iter()
                            .map(|path| {
                                (
                                    path.clone(),
                                    format!(
                                        "it was converted to {target}, stored {}",
                                        format.label()
                                    ),
                                )
                            })
                            .collect(),
                    )
                }
                _ => {
                    let from: Vec<&str> = files.iter().copied().collect();
                    let (kind, summary) = if !cued.is_empty() {
                        cue_kind()
                    } else if local.iter().any(|(rank, ..)| *rank >= 2) {
                        (
                            RepairKind::Rename,
                            format!(
                                "write {target}, under the names the DAT gives, from {}",
                                from.join(", ")
                            ),
                        )
                    } else {
                        (
                            RepairKind::Repack,
                            format!(
                                "repack {} as {target}, stored {}",
                                from.join(", "),
                                format.label()
                            ),
                        )
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
                    (
                        Proposal::Build { roms: all_roms() },
                        kind,
                        summary,
                        displaces,
                    )
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
                release: None,
            });
            continue;
        }

        // Some ROM is not in the Source Set: filled from an Ingest Folder, when an item holds the Game whole.
        let attributable: Vec<&str> = by_path
            .iter()
            .filter(|(path, entries)| {
                is_target(path)
                    || entries.iter().any(|e| match e.finding {
                        Finding::Matched | Finding::Misnamed => {
                            e.roms.iter().any(|r| r.game == *game)
                        }
                        Finding::Mismatched => {
                            e.named_rom.as_ref().is_some_and(|r| r.game == *game)
                        }
                        Finding::Unreadable => {
                            let name = base_name(&e.path);
                            is_target(name) || roms.iter().any(|r| r.name == name)
                        }
                        _ => false,
                    })
            })
            .map(|(path, _)| *path)
            .filter(|path| !kept_conflicts.contains(path) && !released_paths.contains(path))
            .collect();
        if let Some(path) = attributable.iter().find(|p| left_conflicts.contains(**p)) {
            notes.push(left_in_way(path));
            continue;
        }
        if outside {
            if !attributable.is_empty() || !occupants.is_empty() {
                notes.push(format!(
                    "{game} is left as it is: it is outside your coverage, so nothing fills the ROMs it is missing"
                ));
            }
            continue;
        }
        if folder.is_some() {
            if !attributable.is_empty() || !occupants.is_empty() {
                notes.push(format!(
                    "{game} is left as it is: no ingest folder holds a ROM it is missing, with its payload read"
                ));
            }
            continue;
        }
        let Some(candidate) = i.candidates.iter().position(|c| {
            c.matched.game == *game && c.matched.complete && !used_candidates.contains(&c.item.id)
        }) else {
            if !attributable.is_empty() || !occupants.is_empty() {
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
            converts: Vec::new(),
            release: None,
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

    keep_uncovered(i, &by_path, &reported, &mut proposed, &mut notes);

    // A Placed Game is renamed, repacked, replaced or completed where it is placed, never converted. One with a file
    // still there is completed there, from an ingest folder too, so that a Game is never split between two folders;
    // one with none left is no Placed Game here, and is filled in the Source Set's folder.
    let writes_placed: Vec<usize> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.item.key.starts_with("game:")
                && p.item
                    .path
                    .as_deref()
                    .is_some_and(|t| split_placed(t).is_some())
                && !(p.converts.is_empty() && p.proposal.is_some())
        })
        .map(|(n, _)| n)
        .collect();
    for n in writes_placed {
        let item = &mut proposed[n].item;
        item.choices = vec![RepairChoice::Leave];
        item.default = RepairChoice::Leave;
        item.chosen = RepairChoice::Leave;
        item.note = Some(format!("left as it is: {PLACED_WRITES}"));
        leave_release(&mut proposed, n);
    }

    // A healthy CHD, every Entry of it Matched on Payload Evidence and its Game Complete on them, is left as it is:
    // a repair would only repack it into the files the Source Set stores. A disc placed as a CHD is never completed
    // where it is placed, since that would place it as files of two formats: a Game one of whose CHDs is damaged or
    // gone is released whole, below, and one the Audit didn't read as it is now is left as it is, and needs review.
    let healthy: HashSet<&str> = i
        .placed_chds
        .iter()
        .map(String::as_str)
        .filter(|path| healthy_chd(i, path))
        .collect();
    // The CHDs the Audit found where they are placed, any of their Entries reported.
    let present: HashSet<&str> = i
        .placed_chds
        .iter()
        .map(String::as_str)
        .filter(|path| i.report.entries.iter().any(|e| e.path == *path))
        .collect();
    let chd_games: HashSet<&str> = present
        .iter()
        .filter_map(|path| i.placed_files.get(*path))
        .map(String::as_str)
        .collect();
    // Of those, the Games each of whose CHDs is there and healthy; each other one needs review.
    let healthy_games: HashSet<&str> = chd_games
        .iter()
        .copied()
        .filter(|game| {
            i.placed_chds
                .iter()
                .filter(|path| i.placed_files.get(*path).is_some_and(|g| g == game))
                .all(|path| healthy.contains(path.as_str()))
        })
        .collect();
    // The releases not released here one of whose CHDs is recorded placed and is there, each with one such CHD: no
    // Game of one is filled, rebuilt or written in the Source Set's folder, whatever the Audit read, since that would
    // split it. The next Repair after a Full Audit releases it, or leaves it placed.
    let mut placed_releases: BTreeMap<String, &str> = BTreeMap::new();
    for path in i
        .placed_chds
        .iter()
        .filter(|path| i.files.contains_key(*path))
    {
        if let Some(game) = i.placed_files.get(path)
            && !released.contains_key(&release_of(game))
        {
            let chd = placed_releases.entry(release_of(game)).or_insert(path);
            *chd = (*chd).min(path.as_str());
        }
    }
    let touches_chd: Vec<usize> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.item.game.as_deref().is_some_and(|g| {
                chd_games.contains(g) || placed_releases.contains_key(&release_of(g))
            }) || p.item.path.as_deref().is_some_and(|t| present.contains(t))
                || p.displaces
                    .iter()
                    .any(|(d, _)| present.contains(d.as_str()))
        })
        .map(|(n, _)| n)
        .collect();
    for n in touches_chd {
        let p = &proposed[n];
        // The Game whose CHD the item touches, by its own name or by the file's.
        let game = p
            .item
            .game
            .as_deref()
            .filter(|g| chd_games.contains(g))
            .or_else(|| {
                p.item
                    .path
                    .as_deref()
                    .into_iter()
                    .chain(p.displaces.iter().map(|(d, _)| d.as_str()))
                    .find_map(|t| i.placed_files.get(t).map(String::as_str))
            });
        // A Game none of whose CHDs the Audit found, of a release one of whose CHDs is still placed.
        let sibling = p
            .item
            .game
            .clone()
            .filter(|_| game.is_none())
            .and_then(|g| Some((placed_releases.get(&release_of(&g)).copied()?, g)));
        let note = match sibling {
            Some((chd, game)) => format!(
                "left as it is: {chd}, a disc of {}, is still placed as a CHD in a play library folder. A repair \
                 never splits a game's discs, so {game} isn't repaired in your source set while one is placed. Run a full \
                 audit: if a placed CHD of the game doesn't verify or is gone, the next repair sets the game's CHDs \
                 aside in your source set's kept files, and the repair after the audit rebuilds the game",
                release_of(&game)
            ),
            None => match game.is_some_and(|g| healthy_games.contains(g)) {
                true => format!("left as it is: {PLACED_CHD}"),
                false => format!("left as it is: {PLACED_CHD_UNVERIFIED}"),
            },
        };
        let item = &mut proposed[n].item;
        item.choices = vec![RepairChoice::Leave];
        item.default = RepairChoice::Leave;
        item.chosen = RepairChoice::Leave;
        item.note = Some(note);
        leave_release(&mut proposed, n);
    }

    // Each disc of a release released: its CHD set aside, damaged or healthy, and a gone one's placement dropped.
    for discs in released.values() {
        for (game, r) in discs {
            let chd = r.chd.as_str();
            let mut p = Proposed {
                item: game_item(
                    game,
                    chd,
                    RepairKind::KeepAside,
                    match r.freed {
                        Freed::Gone if r.beside.is_empty() => {
                            format!("forget {chd}, which is gone; nothing is kept")
                        }
                        Freed::Gone => format!(
                            "forget {chd}, which is gone, and keep the files laid out beside it aside in your \
                             source set's kept files"
                        ),
                        _ => format!("keep {chd} aside in your source set's kept files"),
                    },
                ),
                proposal: None,
                displaces: Vec::new(),
                produces: None,
                checked: false,
                target: None,
                converts: Vec::new(),
                release: None,
            };
            p.item.note = Some(released_note(game, r));
            p.item.choices = vec![RepairChoice::KeepAside, RepairChoice::Leave];
            p.item.default = match r.proven {
                true => RepairChoice::KeepAside,
                false => RepairChoice::Leave,
            };
            p.item.chosen = p.item.default;
            p.release = Some(r.clone());
            proposed.push(p);
        }
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
    // Every disc of a release is repaired alike: choosing to repair one and leave another is refused, and leaving one
    // leaves them all.
    let explicit: Vec<RepairItem> = proposed
        .iter()
        .filter(|p| i.choices.contains_key(&p.item.key))
        .map(|p| p.item.clone())
        .collect();
    if let Some(why) = split_release(&explicit) {
        return Err(Error::Refused(why));
    }
    let left: Vec<usize> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.item.key.starts_with("game:")
                && p.item.chosen == RepairChoice::Leave
                && i.choices.contains_key(&p.item.key)
        })
        .map(|(n, _)| n)
        .collect();
    for n in left {
        leave_release(&mut proposed, n);
    }
    // Choosing to set one disc of a placed release aside sets every disc of it aside, as it is never split.
    let chosen_aside: BTreeSet<String> = proposed
        .iter()
        .filter(|p| p.item.chosen != RepairChoice::Leave && i.choices.contains_key(&p.item.key))
        .filter_map(|p| p.release.as_ref().map(|r| r.release.clone()))
        .collect();
    for p in proposed.iter_mut().filter(|p| {
        p.release
            .as_ref()
            .is_some_and(|r| chosen_aside.contains(&r.release))
    }) {
        p.item.chosen = RepairChoice::KeepAside;
    }

    // An item that would keep aside the last copy of a ROM no repair writes is left, as is, in turn, any item that
    // relied on it.
    let dat_roms: HashMap<RomKey, &Rom> = i
        .dat
        .roms
        .iter()
        .map(|r| ((i.dat.games[r.game].clone(), r.name.clone()), r))
        .collect();
    loop {
        let preserved = Preserved::of(&proposed, &dat_roms, &held_by);
        let lost = proposed.iter().position(|p| {
            p.checked
                && p.item.chosen != RepairChoice::Leave
                && p.displaces.iter().any(|(path, _)| {
                    usable
                        .get(path.as_str())
                        .is_some_and(|file| !preserved.lost(file).is_empty())
                })
        });
        let Some(lost) = lost else {
            // A disc no item repairs stays stored as it is, so no other disc of its game is stored otherwise.
            let Some((repaired, stays)) =
                release_split_by_unrepaired(i, &by_path, &good, &proposed, &released)
            else {
                break;
            };
            let game = proposed[repaired].item.game.clone().unwrap_or_default();
            for p in proposed.iter_mut().filter(|p| {
                p.item.key.starts_with("game:")
                    && p.item.chosen != RepairChoice::Leave
                    && p.item
                        .game
                        .as_deref()
                        .is_some_and(|g| release_of(g) == release_of(&game))
            }) {
                p.item.chosen = RepairChoice::Leave;
                p.item.note = Some(format!(
                    "left as it is: {stays}, another disc of the same game, can't be repaired now and stays \
                     stored as it is, and every disc of a game is stored alike"
                ));
            }
            continue;
        };
        let item = &mut proposed[lost].item;
        item.chosen = RepairChoice::Leave;
        item.note = Some(
            "left as it is: it would keep aside the only copy of a ROM no repair in this plan writes".into(),
        );
        if proposed[lost].item.key.starts_with("game:") {
            leave_release(&mut proposed, lost);
        }
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
        // A file holding one ROM no DAT knows can be added as an Addition, from where it is kept.
        let addable =
            matches!(file.entries.as_slice(), [only] if only.finding == Finding::Unmatched);
        let choices = match addable {
            true => vec![
                RepairChoice::KeepAside,
                RepairChoice::AddAddition,
                RepairChoice::Leave,
            ],
            false => vec![RepairChoice::KeepAside, RepairChoice::Leave],
        };
        if !choices.contains(&chosen) {
            return Err(Error::Refused(format!(
                "{key} cannot be set to {}",
                chosen.label()
            )));
        }
        let addition = match (chosen, i.additions.get(&key)) {
            (RepairChoice::AddAddition, Some(new)) => Some(new.clone()),
            (RepairChoice::AddAddition, None) => {
                return Err(Error::Refused(format!(
                    "{key}: choose the system and name to add it as an addition under"
                )));
            }
            (_, Some(_)) => {
                return Err(Error::Refused(format!(
                    "{key} is not chosen to be added as an addition"
                )));
            }
            (_, None) => None,
        };
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
                addition,
                addition_rom: None,
                addition_target: None,
            },
            proposal: None,
            displaces: vec![(path.to_string(), summary)],
            produces: None,
            checked: false,
            target: None,
            converts: Vec::new(),
            release: None,
        });
    }
    for key in i.choices.keys().chain(i.additions.keys()) {
        if key.starts_with("file:") && !aside.iter().any(|p| p.item.key == *key) {
            return Err(Error::Refused(format!("the plan has no item {key}")));
        }
    }
    proposed.extend(aside);

    // What you chose to keep aside is kept aside even when it is the last copy of a ROM, which the item then says:
    // it can be restored from the kept files.
    let preserved = Preserved::of(&proposed, &dat_roms, &held_by);
    let last: Vec<(usize, Vec<String>)> = proposed
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            !p.checked
                && matches!(
                    p.item.chosen,
                    RepairChoice::KeepAside | RepairChoice::AddAddition
                )
        })
        .filter_map(|(n, p)| {
            let lost: BTreeSet<String> = p
                .displaces
                .iter()
                .filter_map(|(path, _)| usable.get(path.as_str()))
                .flat_map(|file| preserved.lost(file))
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
    let releasing = proposed
        .iter()
        .filter(|p| p.release.is_some() && p.item.chosen != RepairChoice::Leave)
        .map(|p| p.item.key.clone())
        .collect();
    Ok(Planned {
        items: proposed.into_iter().map(|p| p.item).collect(),
        steps,
        notes,
        released: releasing,
    })
}

/// The steps that carry out what each item chosen does: every Build first, then every Keep of a file in the way of a
/// new one, every Rename, every Publish, every Fill, and then every Keep of an original, once all its item put in place
/// is there. A conversion that shares no file with another item then follows, each in turn: its Builds,
/// the Keep of what it displaces from its new files' names, its Publishes, the Keep of each original and, when the
/// Storage Policy deletes originals, the Delete of each original, so that a folder being converted holds at most one
/// converted Game more than it did at any time.
fn steps_of(i: &Inputs, proposed: &[Proposed]) -> Vec<PlannedStep> {
    let applied: Vec<&Proposed> = proposed
        .iter()
        .filter(|p| p.item.chosen != RepairChoice::Leave)
        .collect();
    let touches = |p: &Proposed| -> BTreeSet<String> {
        let mut paths: BTreeSet<String> =
            p.displaces.iter().map(|(path, _)| path.clone()).collect();
        let local = |r: &&RepairRom| r.cue_pack.is_none();
        match &p.proposal {
            Some(Proposal::Build { roms }) => {
                paths.extend(roms.iter().filter(local).map(|r| r.from.clone()))
            }
            Some(Proposal::Rename { from, .. }) => {
                paths.insert(from.clone());
            }
            Some(Proposal::Folder { parts }) => {
                for part in parts {
                    match part {
                        Part::Build { target, rom } => {
                            paths.insert(target.clone());
                            if rom.cue_pack.is_none() {
                                paths.insert(rom.from.clone());
                            }
                        }
                        Part::Rename { from, to, .. } => {
                            paths.insert(from.clone());
                            paths.insert(to.clone());
                        }
                        Part::Fill { target, .. } => {
                            paths.insert(target.clone());
                        }
                    }
                }
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
        .partition(|p| !p.converts.is_empty() && alone(p));
    let mut steps = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    released_steps(i, &applied, &mut names, &mut steps);
    let build_step = |p: &Proposed, target: String, loose: bool, roms: Vec<RepairRom>| {
        let sources = roms
            .iter()
            .filter(|r| r.cue_pack.is_none())
            .map(|r| (r.from.clone(), seen_file(&i.files[&r.from])))
            .collect();
        PlannedStep {
            item: p.item.key.clone(),
            step: RepairStep::Build {
                target,
                roms,
                sources,
                loose,
            },
        }
    };
    // Each file `p` builds, with its Build step.
    let builds_of = |p: &Proposed| -> Vec<(String, PlannedStep)> {
        match &p.proposal {
            Some(Proposal::Build { roms }) => {
                let (target, loose) = p.target.clone().expect("a game's target");
                vec![(target.clone(), build_step(p, target, loose, roms.clone()))]
            }
            Some(Proposal::Folder { parts }) => parts
                .iter()
                .filter_map(|part| match part {
                    Part::Build { target, rom } => Some((
                        target.clone(),
                        build_step(p, target.clone(), true, vec![rom.clone()]),
                    )),
                    Part::Rename { .. } | Part::Fill { .. } => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut builds: Vec<(&str, u32, String)> = Vec::new();
    for p in &together {
        for (target, step) in builds_of(p) {
            builds.push((&p.item.key, steps.len() as u32, target));
            steps.push(step);
        }
    }
    let built_by =
        |key: &str| -> Vec<u32> { builds.iter().filter(|b| b.0 == key).map(|b| b.1).collect() };
    // Every name a file is put in place under: what is there now is in the way, and is kept aside first. Any other
    // file an item displaces, an original it reads, is kept aside only once all the item puts in place is there.
    let mut destinations: BTreeSet<&str> = builds.iter().map(|b| b.2.as_str()).collect();
    for p in &together {
        if let Some((target, _)) = &p.target {
            destinations.insert(target);
        }
        if let Some(Proposal::Folder { parts }) = &p.proposal {
            for part in parts {
                match part {
                    Part::Build { target, .. } | Part::Fill { target, .. } => {
                        destinations.insert(target);
                    }
                    Part::Rename { to, .. } => {
                        destinations.insert(to);
                    }
                }
            }
        }
    }
    let mut keeps: Vec<PlannedStep> = Vec::new();
    for p in &together {
        keeps_of(
            i,
            p,
            |path| destinations.contains(path),
            built_by(&p.item.key),
            &mut names,
            &mut keeps,
        );
    }
    steps.extend(keeps);
    let rename =
        |p: &Proposed, from: &str, to: String, roms: Vec<ExpectedRom>, loose: bool| PlannedStep {
            item: p.item.key.clone(),
            step: RepairStep::Rename {
                from: from.to_owned(),
                to,
                seen: seen_file(&i.files[from]),
                roms,
                loose,
            },
        };
    for p in &together {
        match &p.proposal {
            Some(Proposal::Rename { from, roms }) => {
                let (to, loose) = p.target.clone().expect("a game's target");
                steps.push(rename(p, from, to, roms.clone(), loose));
            }
            Some(Proposal::Folder { parts }) => {
                for part in parts {
                    if let Part::Rename { from, to, rom } = part {
                        steps.push(rename(p, from, to.clone(), vec![rom.clone()], true));
                    }
                }
            }
            _ => {}
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
        match &p.proposal {
            Some(Proposal::Fill { candidate }) => {
                steps.push(fill_of(i, p, &i.candidates[*candidate]));
            }
            Some(Proposal::Folder { parts }) => {
                for (n, part) in parts.iter().enumerate() {
                    if let Part::Fill {
                        target,
                        rom,
                        candidate,
                    } = part
                    {
                        // Each file but the last written from one item leaves the item in place.
                        let leave = parts[n + 1..].iter().any(
                            |later| matches!(later, Part::Fill { candidate: c, .. } if c == candidate),
                        );
                        steps.push(folder_fill_of(
                            p,
                            target,
                            rom,
                            &i.candidates[*candidate],
                            leave,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    let mut originals: Vec<PlannedStep> = Vec::new();
    for p in &together {
        let placed: Vec<u32> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.item == p.item.key
                    && matches!(
                        s.step,
                        RepairStep::Build { .. }
                            | RepairStep::Rename { .. }
                            | RepairStep::Publish { .. }
                            | RepairStep::Fill { .. }
                    )
            })
            .map(|(n, _)| n as u32)
            .collect();
        keeps_of(
            i,
            p,
            |path| !destinations.contains(path),
            placed,
            &mut names,
            &mut originals,
        );
    }
    steps.extend(originals);
    for p in turns {
        let originals = &p.converts;
        // What you approved keeping aside from the new files' names goes first; the new files are in place before
        // any original is moved, so a destination still taken leaves the originals where they are.
        let mut built = Vec::new();
        for (target, step) in builds_of(p) {
            built.push((steps.len() as u32, target));
            steps.push(step);
        }
        assert!(!built.is_empty(), "a conversion builds");
        let mut occupants = Vec::new();
        keeps_of(
            i,
            p,
            |path| !originals.iter().any(|o| o == path),
            built.iter().map(|b| b.0).collect(),
            &mut names,
            &mut occupants,
        );
        steps.extend(occupants);
        let mut pairs = Vec::new();
        for (build, target) in built {
            pairs.push((build, steps.len() as u32));
            steps.push(PlannedStep {
                item: p.item.key.clone(),
                step: RepairStep::Publish { build, target },
            });
        }
        let needs: Vec<u32> = pairs.iter().flat_map(|&(b, p)| [b, p]).collect();
        for original in originals {
            let keep = steps.len() as u32;
            let mut keeps = Vec::new();
            keeps_of(
                i,
                p,
                |path| path == original,
                needs.clone(),
                &mut names,
                &mut keeps,
            );
            assert_eq!(keeps.len(), 1, "the original is kept");
            steps.extend(keeps);
            if i.storage.delete_originals {
                let (build, publish) = pairs[0];
                steps.push(PlannedStep {
                    item: p.item.key.clone(),
                    step: RepairStep::Delete {
                        path: original.clone(),
                        build,
                        keep,
                        publish,
                        also: pairs[1..].to_vec(),
                    },
                });
            }
        }
    }
    steps
}

/// A kept name for `path` whose [`name_key`] is not yet in `names`, taken.
fn kept_as(path: &str, names: &mut BTreeSet<String>) -> String {
    let base = base_name(path);
    let name = (1..)
        .map(|n| kept_name(base, n))
        .find(|name| !names.contains(&name_key(name)))
        .expect("a free name");
    names.insert(name_key(&name));
    name
}

/// Appends to `steps` the steps that release each placed release of `applied`, before any other step (ADR 0023): a
/// Keep step for each file laid out beside its CHDs, then one for each CHD of it still there, needing every one of
/// those, so that a release is found again by its CHDs until the last of them is kept, and a file beside them that
/// can't be set aside leaves the whole release placed. A release every CHD of which is gone is found again by its
/// records alone, which the last Keep of a file beside them drops: that Keep needs every one before it. Nothing of a
/// release is extracted, deleted, put home or filled, so each step ends with its file either still placed or kept: a
/// stop between them leaves a release with some discs placed and the rest kept, which the next repair releases.
fn released_steps(
    i: &Inputs,
    applied: &[&Proposed],
    names: &mut BTreeSet<String>,
    steps: &mut Vec<PlannedStep>,
) {
    let mut releases: BTreeMap<&str, Vec<&Proposed>> = BTreeMap::new();
    for p in applied {
        if let Some(r) = &p.release {
            releases.entry(r.release.as_str()).or_default().push(p);
        }
    }
    let seen = |path: &str| -> SeenFile {
        match i.files.get(path) {
            Some(seen) => seen_file(seen),
            None => i
                .beside
                .values()
                .flatten()
                .find(|(p, _)| p == path)
                .map(|(_, seen)| seen_file(seen))
                .expect("a file beside a placed release, as seen"),
        }
    };
    for discs in releases.into_values() {
        let mut keep = |p: &Proposed, path: &str, reason: String, needs: Vec<u32>| {
            steps.push(PlannedStep {
                item: p.item.key.clone(),
                step: RepairStep::Keep {
                    path: path.to_owned(),
                    kept_name: kept_as(path, names),
                    reason,
                    seen: seen(path),
                    needs,
                    affects: Vec::new(),
                },
            });
            steps.len() as u32 - 1
        };
        let gone = discs
            .iter()
            .all(|p| p.release.as_ref().is_some_and(|r| r.freed == Freed::Gone));
        let files: Vec<(&Proposed, &Released, &String)> = discs
            .iter()
            .flat_map(|p| {
                let r = p.release.as_ref().expect("a release's disc");
                r.beside.iter().map(move |path| (*p, r, path))
            })
            .collect();
        let mut beside = Vec::new();
        for (n, (p, r, path)) in files.iter().enumerate() {
            let last_of_gone = gone && n + 1 == files.len();
            let needs = match last_of_gone {
                true => beside.clone(),
                false => Vec::new(),
            };
            beside.push(keep(
                p,
                path,
                format!(
                    "it was laid out beside the CHDs of {}, set aside with them",
                    r.release
                ),
                needs,
            ));
        }
        for p in &discs {
            let r = p.release.as_ref().expect("a release's disc");
            let game = p.item.game.as_deref().unwrap_or(&r.release);
            let reason = match r.freed {
                Freed::Damaged => format!(
                    "it was the only copy of {game}, a CHD that didn't verify, set aside by a repair"
                ),
                Freed::Healthy => format!(
                    "it was the only copy of {game}, set aside by a repair with the other discs of {}, one of which \
                     didn't verify or was gone",
                    r.release
                ),
                Freed::Gone => continue,
            };
            keep(p, &r.chd, reason, beside.clone());
        }
    }
}

/// Appends to `keeps` a Keep step for each file `p` displaces whose path is `chosen`, needing the steps `needs`, under
/// a kept name whose [`name_key`] is not yet in `names`; a file another item already keeps aside is kept once, for both.
fn keeps_of(
    i: &Inputs,
    p: &Proposed,
    chosen: impl Fn(&str) -> bool,
    needs: Vec<u32>,
    names: &mut BTreeSet<String>,
    keeps: &mut Vec<PlannedStep>,
) {
    for (path, reason) in p.displaces.iter().filter(|(path, _)| chosen(path)) {
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
        let name = kept_as(path, names);
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
            cue_pack: None,
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
            leave: false,
        },
    }
}

/// The Fill step of `p` that writes `target`, a file of a loose folder, as `rom` from ingest candidate `c`; `leave`
/// leaves the item in place for a later Fill step.
fn folder_fill_of(
    p: &Proposed,
    target: &str,
    rom: &ExpectedRom,
    c: &FillCandidate,
    leave: bool,
) -> PlannedStep {
    let member = c
        .matched
        .roms
        .iter()
        .find(|m| m.rom == rom.name)
        .and_then(|m| m.member.clone());
    // The original is deleted only once what was written reads as exactly this ROM, from a loose file.
    let exact = (!c.item.assessment.zip && member.is_none()).then(|| rom.clone());
    PlannedStep {
        item: p.item.key.clone(),
        step: RepairStep::Fill {
            target: target.to_owned(),
            ingest_root: c.root.clone(),
            item: c.item.id,
            name: c.item.name.clone(),
            seen: seen_file(&c.seen),
            sha256: c.item.assessment.sha256.clone(),
            roms: vec![RepairRom {
                rom: rom.clone(),
                from: c.item.name.clone(),
                member,
                cue_pack: None,
            }],
            exact,
            loose: true,
            leave,
        },
    }
}
