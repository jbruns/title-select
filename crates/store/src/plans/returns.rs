//! Returning Placed Games (ADR 0023): which releases placed in a Library Folder a plan takes back to their Source
//! Set's folder, and how. A release goes back whole, every placed file of each of its Games, or not at all, when the
//! folder's Selection no longer chooses one of its Games, when Single Copy is off for the folder's system, when the
//! folder no longer holds the Source Set's Placed Games, or when you remove every built file. It goes back in the
//! format the Source Set's Storage Policy says: renamed when each placed file is a file of that format on the same
//! filesystem, otherwise written there, from its placed files, and verified against the DAT: a CHD's tracks extracted
//! from it, and a CD's cue sheet kept in the store. A Game still selected in the folder keeps its file there, as a built
//! copy.

use std::collections::{BTreeMap, HashSet};

use title_select_api::build::{
    ExpectedRom, FileIdentity, FileOutcome, PlanItem, PlanReturn, PlanSource, ReturnFile,
    ReturnHow, ReturnPlaced,
};
use title_select_api::storage::{Placement, placement, release_of};

use super::{Built, Folder, Inputs, same_roms};
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use crate::folders::open_folder;

/// How much a file written home is rounded up to, in what a return needs: more than any filesystem's block, so that
/// the space a file takes, and not only its bytes, is counted.
const BLOCK: u64 = 64 * 1024;

/// The most a TorrentZip of `roms` can take: their bytes, what deflate adds to bytes it can't compress, and each
/// member's headers.
fn zip_bound(roms: &[ExpectedRom]) -> u64 {
    roms.iter()
        .map(|r| r.size + r.size / 1000 + 256 + 2 * r.name.len() as u64)
        .sum::<u64>()
        + 1024
}

pub(crate) fn rounded(bytes: u64) -> u64 {
    bytes.div_ceil(BLOCK).max(1) * BLOCK
}

/// A placed file of a release, as the plan found it, and how it was built.
struct Found<'a> {
    placed: ReturnPlaced,
    built: &'a Built,
}

/// The releases placed in the folder that go back to their Source Set's folder, with the paths in the folder of the
/// placed files they take; and says in `notes` why any that should go back stays.
pub(super) fn returns(
    config: &Config,
    inputs: &Inputs,
    folder: Option<&Folder>,
    items: &[PlanItem],
    notes: &mut Vec<String>,
) -> (Vec<PlanReturn>, HashSet<String>) {
    let mut out = Vec::new();
    let mut taken = HashSet::new();
    if inputs.placed.is_empty() {
        return (out, taken);
    }
    let Some((folder, scan)) = folder.and_then(|f| f.scan.as_ref().map(|s| (f, s))) else {
        return (out, taken);
    };
    let set = &inputs.folder.source_set;
    let remove_all = inputs.choices.remove_all;
    let mut releases: BTreeMap<String, Vec<&title_select_api::build::PlacedFile>> = BTreeMap::new();
    for p in &inputs.placed {
        releases.entry(release_of(&p.game)).or_default().push(p);
    }
    let selected = |game: &str| !remove_all && items.iter().any(|i| i.game == game);
    let mut stays: Vec<String> = Vec::new();
    let writable = config
        .root(&inputs.set_root)
        .is_some_and(|r| r.may_write(&inputs.set_path));
    let set_dir = open_folder(config, &inputs.set_root, &inputs.set_path);
    // A rename moves a file only within one filesystem; a test may say they are two.
    let same_fs = match (&set_dir, folder.dir.identity()) {
        (Ok(set), Ok(here)) => {
            set.identity().is_ok_and(|s| s.dev == here.dev) && !inputs.copy_across
        }
        _ => false,
    };
    let mut paths: HashSet<String> = HashSet::new();
    let (mut renamed, mut copied, mut converted, mut needs) = (0usize, 0usize, 0usize, 0u64);
    for (release, placed) in &releases {
        let mut games: Vec<&str> = placed.iter().map(|p| p.game.as_str()).collect();
        games.sort_unstable();
        games.dedup();
        // The folder keeps a release placed in it while its Games are selected there, Single Copy is on for its
        // system, and it holds the Source Set's Placed Games.
        let why = if remove_all {
            Some("you are removing every built file of this folder".to_owned())
        } else if let Some(game) = games.iter().find(|g| !selected(g)) {
            Some(format!("{game} is no longer selected here"))
        } else if !inputs.storage.single_copy {
            Some("single copy is off for this system".to_owned())
        } else if inputs.holding != Some(inputs.folder.id) {
            Some(format!(
                "this play library folder no longer holds source set {set}'s games"
            ))
        } else {
            None
        };
        let Some(why) = why else {
            continue;
        };
        if !writable {
            stays.push(format!(
                "{release} stays placed here: root {} doesn't let Title Select write to source set {set}'s folder",
                inputs.set_root
            ));
            continue;
        }
        let set_dir = match &set_dir {
            Ok(dir) => dir,
            Err(e) => {
                stays.push(format!(
                    "{release} stays placed here: source set {set}'s folder can't be opened ({e})"
                ));
                continue;
            }
        };
        match one(inputs, folder, scan, set_dir, placed, &selected) {
            Ok((found, files)) => {
                if let Some(clash) = files.iter().find(|f| !paths.insert(f.path.clone())) {
                    stays.push(format!(
                        "{release} stays placed here: another game returned now goes to {}",
                        clash.path
                    ));
                    continue;
                }
                let whole = files.iter().all(|f| f.from.is_some());
                let keeps = found.iter().any(|f| f.placed.keeps);
                let one_each = files.len() == found.len()
                    && found.iter().all(|f| {
                        files
                            .iter()
                            .filter(|o| o.from.as_deref() == Some(&f.placed.path))
                            .count()
                            == 1
                    });
                let how = match (whole, keeps) {
                    (true, false) if same_fs && one_each => ReturnHow::Rename,
                    (true, _) => ReturnHow::Copy,
                    (false, _) => ReturnHow::Convert,
                };
                let bytes: u64 = match how {
                    ReturnHow::Rename => 0,
                    _ => files
                        .iter()
                        .map(|f| {
                            rounded(match (&f.from, f.loose) {
                                (Some(from), _) => found
                                    .iter()
                                    .find(|p| p.placed.path == *from)
                                    .map_or(0, |p| p.placed.file.size),
                                (None, true) => f.roms.iter().map(|r| r.size).sum(),
                                (None, false) => zip_bound(&f.roms),
                            })
                        })
                        .sum(),
                };
                match how {
                    ReturnHow::Rename => renamed += games.len(),
                    ReturnHow::Copy => copied += games.len(),
                    ReturnHow::Convert => converted += games.len(),
                }
                needs += bytes;
                taken.extend(found.iter().map(|f| f.placed.path.clone()));
                out.push(PlanReturn {
                    release: release.clone(),
                    games: games.iter().map(|g| (*g).to_owned()).collect(),
                    how,
                    reason: why,
                    placed: found.into_iter().map(|f| f.placed).collect(),
                    files,
                    needs: bytes,
                });
            }
            Err(stay) => stays.push(format!("{release} stays placed here: {stay}")),
        }
    }
    let games = renamed + copied + converted;
    if games > 0 {
        let ways: Vec<String> = [
            (renamed, "moved by a rename"),
            (copied, "copied"),
            (converted, "converted back to the source set's format"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, how)| format!("{n} {how}"))
        .collect();
        notes.push(format!(
            "{} placed here {} back to source set {set}'s folder: {}. {} verified against the DAT there before the \
             files placed here go. The returns need at most {} free in root {}, and the plan can't run without it",
            match games {
                1 => "1 game".to_owned(),
                n => format!("{n} games"),
            },
            if games == 1 { "goes" } else { "go" },
            ways.join(", "),
            if games == 1 { "It is" } else { "Each is" },
            bytes(needs),
            inputs.set_root
        ));
    }
    notes.extend(stays);
    (out, taken)
}

pub(crate) fn bytes(n: u64) -> String {
    match n {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

/// How the release of `placed`, the placed files of its Games, goes back: each placed file as found, and each file
/// put in the Source Set's folder `set_dir`, from them; or why it stays.
fn one<'a>(
    inputs: &'a Inputs,
    folder: &Folder,
    scan: &crate::checks::Scan,
    set_dir: &FolderDir,
    placed: &[&title_select_api::build::PlacedFile],
    selected: &dyn Fn(&str) -> bool,
) -> Result<(Vec<Found<'a>>, Vec<ReturnFile>), String> {
    let mut found: Vec<Found> = Vec::new();
    for p in placed {
        let outcome = scan
            .files
            .iter()
            .find(|f| f.target == p.path)
            .map(|f| f.outcome);
        if !matches!(
            outcome,
            Some(FileOutcome::Unchanged | FileOutcome::Refreshed)
        ) {
            return Err(format!(
                "the check didn't find {} as it was placed; audit the source set to see what it holds",
                p.path
            ));
        }
        // A CHD goes back as its disc's files, extracted from it and verified against the DAT, with a CD's cue sheet
        // from the store.
        let Some(built) = inputs
            .built
            .get(&p.path)
            .filter(|b| !b.roms.is_empty() && b.chd == p.chd)
        else {
            return Err(format!("what {} holds isn't recorded", p.path));
        };
        let st = folder
            .dir
            .open_file(&p.path)
            .and_then(|file| Ok(rustix::fs::fstat(&file)?))
            .map_err(|e| format!("{} can't be read ({e})", p.path))?;
        let identity = Identity::of(&st);
        found.push(Found {
            placed: ReturnPlaced {
                path: p.path.clone(),
                game: p.game.clone(),
                file: file_stat(&st),
                identity: FileIdentity {
                    dev: identity.dev,
                    ino: identity.ino,
                },
                ctime_ns: ctime_ns(&st),
                keeps: selected(&p.game),
            },
            built,
        });
    }
    let mut games: Vec<&str> = found.iter().map(|f| f.placed.game.as_str()).collect();
    games.sort_unstable();
    games.dedup();
    let mut files = Vec::new();
    for game in games {
        let of_game: Vec<&Found> = found.iter().filter(|f| f.placed.game == game).collect();
        let roms: Vec<ExpectedRom> = of_game
            .iter()
            .flat_map(|f| f.built.roms.iter().cloned())
            .collect();
        let names: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
        let (laid, _) = placement(inputs.set_storage, game, &names);
        let outputs: Vec<(String, bool, Vec<ExpectedRom>)> = match laid {
            Placement::TorrentZip(name) => vec![(name, false, roms.clone())],
            Placement::Loose(name) => vec![(name, true, roms.clone())],
            Placement::Folder { files, .. } => files
                .into_iter()
                .zip(&roms)
                .map(|(path, rom)| (path, true, vec![rom.clone()]))
                .collect(),
        };
        for (path, loose, roms) in outputs {
            match set_dir.stat(&path) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    return Err(format!(
                        "{path} is in source set {}'s folder already, and nothing is written over it",
                        inputs.folder.source_set
                    ));
                }
                Err(e) => return Err(format!("{path} can't be checked ({e})")),
            }
            // The placed file that is this file whole: a TorrentZip of the same ROMs, or the same loose ROM.
            let from = of_game
                .iter()
                .find(|f| {
                    !f.built.chd
                        && f.built.loose == loose
                        && (loose || f.built.torrentzip)
                        && same_roms(&f.built.roms, &roms)
                })
                .map(|f| f.placed.path.clone());
            let mut sources = Vec::new();
            for rom in &roms {
                let Some(f) = of_game
                    .iter()
                    .find(|f| f.built.roms.iter().any(|r| r.name == rom.name))
                else {
                    return Err(format!("no file placed here holds {}", rom.name));
                };
                sources.push(PlanSource {
                    rom: rom.name.clone(),
                    path: f.placed.path.clone(),
                    member: (!f.built.loose || f.built.chd).then(|| rom.name.clone()),
                    finding: None,
                    evidence: None,
                    file: f.placed.file.clone(),
                    identity: Some(f.placed.identity),
                    ctime_ns: Some(f.placed.ctime_ns),
                });
            }
            files.push(ReturnFile {
                path,
                game: game.to_owned(),
                loose,
                roms,
                from,
                sources,
            });
        }
    }
    Ok((found, files))
}
