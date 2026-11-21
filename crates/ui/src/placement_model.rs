//! Single copy's pure logic: how many of a source set's games are placed and where, which library folder holds them,
//! where an audit found each game, and the games a build flagged as needing review.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use title_select_api::build::{BuiltFile, LibraryFolder, PlacedFile};
use title_select_api::storage::split_placed;
use title_select_api::{EntryRow, GameStatus, ReviewFlag, SourceSetSummary};

use crate::model::when;
use crate::parts::status::Placement;

/// How many games the source set's latest audit found present, in its coverage and outside it: what "N of M games
/// placed" counts against. A game with nothing to check is present only where the audit found its own files. None
/// before its first audit.
pub fn present_games(summary: &SourceSetSummary) -> Option<u64> {
    let present = |games: &BTreeMap<GameStatus, usize>| {
        [GameStatus::Complete, GameStatus::Incomplete]
            .iter()
            .filter_map(|s| games.get(s))
            .sum::<usize>() as u64
    };
    let latest = summary.latest.as_ref()?;
    let outside = summary
        .outside_coverage
        .as_ref()
        .map_or(0, |o| present(&o.games));
    Some(present(&latest.games) + outside + latest.nothing_to_check_found as u64)
}

/// "3 of the 10 games in your source set are placed in play: atari". M is the games its latest audit found present,
/// not those single copy could place, so the line counts what is placed against what the source set holds rather
/// than reading as progress. Without an audit to count against, "3 games in your source set are placed in …".
pub fn placed_line(placed: u64, games: Option<u64>, folder: &str) -> String {
    let verb = if placed == 1 { "is" } else { "are" };
    match games.map(|g| g.max(placed)) {
        Some(1) => format!("The 1 game in your source set is placed in {folder}"),
        Some(games) => {
            format!("{placed} of the {games} games in your source set {verb} placed in {folder}")
        }
        None if placed == 1 => format!("1 game in your source set is placed in {folder}"),
        None => format!("{placed} games in your source set are placed in {folder}"),
    }
}

/// How a library folder reads: "root: path", as the library folder lists show it, or by its number once it is gone.
pub fn folder_name(folders: &[LibraryFolder], id: i64) -> String {
    folders.iter().find(|f| f.id == id).map_or_else(
        || format!("library folder #{id}"),
        |f| format!("{}: {}", f.root, f.path),
    )
}

/// Which library folder holds the source set's placed games, and whether you choose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    /// No library folder is built from the source set, so none holds its games.
    NoFolder,
    /// One library folder is built from it: that one holds them.
    Only(i64),
    /// Several are: you choose the one that holds them, from `folders` by id, or none.
    Choose {
        folders: Vec<i64>,
        chosen: Option<i64>,
    },
}

impl Holding {
    /// Of `folders`, those bound to source set `set`, with `holding` the folder the service says holds its games.
    pub fn of(folders: &[LibraryFolder], set: &str, holding: Option<i64>) -> Holding {
        let bound: Vec<i64> = folders
            .iter()
            .filter(|f| f.source_set == set)
            .map(|f| f.id)
            .collect();
        match bound.as_slice() {
            [] => Holding::NoFolder,
            [only] => Holding::Only(*only),
            _ => Holding::Choose {
                chosen: holding.filter(|h| bound.contains(h)),
                folders: bound,
            },
        }
    }

    /// What the choice means for your builds.
    pub fn note(&self) -> &'static str {
        match self {
            Holding::NoFolder => {
                "No library folder is built from this source set, so none holds its games."
            }
            Holding::Only(_) => {
                "It's the only library folder built from this source set, so with single copy on, its builds place your games there."
            }
            Holding::Choose { chosen: None, .. } => {
                "Several library folders are built from this source set. Choose the one that holds its placed games: until you do, builds place nothing and copy each game instead."
            }
            Holding::Choose {
                chosen: Some(_), ..
            } => {
                "Changing it moves nothing now. The next build of the folder holding your games returns them to the source set, then the next build of the folder you chose places them there."
            }
        }
    }
}

/// Each game an audit found placed, with the library folder it was placed in: a game whose entries name a placed
/// path. A game not listed was in its source set's folder.
pub fn game_placements(entries: &[EntryRow]) -> HashMap<String, i64> {
    let mut placed = HashMap::new();
    for row in entries {
        if let Some((folder, _)) = split_placed(&row.entry.path) {
            for rom in &row.entry.roms {
                placed.insert(rom.game.clone(), folder);
            }
        }
    }
    placed
}

/// The files a library folder holds now as placed games' only copies, by path, as its placed-file records say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnlyCopies(HashSet<String>);

impl OnlyCopies {
    pub fn of(placed: &[PlacedFile]) -> OnlyCopies {
        OnlyCopies(placed.iter().map(|p| p.path.clone()).collect())
    }

    /// Whether a built file is its game's only copy: whether the folder holds it placed now. Never read from the kind
    /// its build gave it: a file built here before its game was placed keeps its copy or repack kind, and a file
    /// left here as a built copy once its game returns is no longer the only copy, whatever its kind.
    pub fn placement(&self, file: &BuiltFile) -> Option<Placement> {
        self.0.contains(&file.target).then_some(Placement::Placed)
    }
}

/// One read of the service, stamped with the job-settled count it was started at: a screen reads again each time a
/// job settles, and a read stamped with an older count is out of date.
pub type Stamped<T> = (u64, Result<T, String>);

/// A read a screen repeats whenever a job settles, as the screen shows it: the last good value, whether a newer read
/// is under way, and why the latest read failed. While `reading` or `problem` holds, the screen says so with the shared
/// pending and problem parts and never shows the last good value as certain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fresh<T> {
    pub last: Option<T>,
    pub reading: bool,
    pub problem: Option<String>,
}

impl<T> Default for Fresh<T> {
    fn default() -> Self {
        Fresh {
            last: None,
            reading: true,
            problem: None,
        }
    }
}

impl<T: Clone> Fresh<T> {
    /// After `read`, the resource's value now, with `latest` the job-settled count its newest read was started at. The
    /// value is None before the first read lands, and while a newer read runs it is still the previous read's, with
    /// its older stamp. A failed read keeps the last good value.
    pub fn next(&self, read: Option<&Stamped<T>>, latest: u64) -> Fresh<T> {
        let Some((at, result)) = read else {
            return Fresh {
                last: self.last.clone(),
                reading: true,
                problem: None,
            };
        };
        let reading = *at != latest;
        match result {
            Ok(value) => Fresh {
                last: Some(value.clone()),
                reading,
                problem: None,
            },
            Err(message) => Fresh {
                last: self.last.clone(),
                reading,
                problem: Some(message.clone()),
            },
        }
    }

    /// The value, when the latest read is done and good.
    pub fn certain(&self) -> Option<&T> {
        if self.reading || self.problem.is_some() {
            return None;
        }
        self.last.as_ref()
    }
}

impl Fresh<OnlyCopies> {
    /// Whether a built file is its game's only copy, as far as is known now: from the folder's placed files once the
    /// latest read of them is done and good. Until then, or when it fails, a file the last good read found placed
    /// stays marked the only copy, and any other is not known: never "not the only copy" on stale or missing data.
    pub fn only_copy(&self, file: &BuiltFile) -> Option<Placement> {
        match (self.certain(), &self.last) {
            (Some(copies), _) => copies.placement(file),
            (None, Some(last)) if last.placement(file).is_some() => Some(Placement::Placed),
            (None, _) => Some(Placement::Unknown),
        }
    }
}

/// A library folder's built files, each with whether it is its game's only copy now. `files` is the last good read of
/// the built files, kept apart from `copies` so a failed read of the files never freezes their markers: they always
/// follow the latest placed files.
pub fn marked(
    files: &[BuiltFile],
    copies: &Fresh<OnlyCopies>,
) -> Vec<(BuiltFile, Option<Placement>)> {
    files
        .iter()
        .map(|f| (f.clone(), copies.only_copy(f)))
        .collect()
}

/// What a list of built files says while it reads which are their games' only copies.
pub const READING_ONLY_COPIES: &str = "Reading which files are their games' only copies…";

/// What a list of built files says when it can't read which are their games' only copies.
pub fn only_copies_unknown(message: &str) -> String {
    format!(
        "Couldn't read which files are their games' only copies: {message}. A file marked not known may be one, so \
         don't delete it by hand."
    )
}

/// The games a build flagged as needing review, by name, as a table of game statuses knows them now.
pub type FlagsNow = Fresh<Arc<BTreeMap<String, ReviewFlag>>>;

impl FlagsNow {
    /// For a table with no source set to ask, such as the bench's: no game is flagged, certainly.
    pub fn none() -> FlagsNow {
        Fresh {
            last: Some(Arc::default()),
            reading: false,
            problem: None,
        }
    }

    /// The game's flag in the last good read. It stays marked while a newer read runs or after one fails, since a
    /// flag is a warning: the table says the flags are being read, or couldn't be.
    pub fn get(&self, game: &str) -> Option<&ReviewFlag> {
        self.last.as_ref()?.get(game)
    }

    /// Whether any of `games` is flagged in the last good read.
    pub fn any<'a>(&self, mut games: impl Iterator<Item = &'a str>) -> bool {
        games.any(|g| self.get(g).is_some())
    }
}

/// A read of a source set's review flags, by game.
pub fn stamped_flags(
    at: u64,
    read: Result<Vec<ReviewFlag>, String>,
) -> Stamped<Arc<BTreeMap<String, ReviewFlag>>> {
    (at, read.map(|f| Arc::new(flags_by_game(&f))))
}

/// What a table of game statuses says while it reads which games need review.
pub const READING_FLAGS: &str = "Reading which games need review…";

/// What a table of game statuses says when it can't read which games need review.
pub fn flags_unknown(message: &str) -> String {
    format!(
        "Couldn't read which games need review: {message}. The marks show the last good read, so a game without one \
         may still need review."
    )
}

/// "1 game needs review", "3 games need review".
pub fn review_line(n: usize) -> String {
    match n {
        1 => "1 game needs review".to_string(),
        n => format!("{n} games need review"),
    }
}

/// The review flags, by game.
pub fn flags_by_game(flags: &[ReviewFlag]) -> BTreeMap<String, ReviewFlag> {
    flags.iter().map(|f| (f.game.clone(), f.clone())).collect()
}

/// Why a game needs review and what found it, for its detail.
pub fn flag_detail(flag: &ReviewFlag) -> String {
    format!(
        "{}. Build #{} found this, {}. A full audit that verifies the file, or returning the game to your source \
         set, clears it.",
        flag.reason.trim_end_matches('.'),
        flag.build,
        when(&flag.raised_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(latest: serde_json::Value, outside: Option<serde_json::Value>) -> SourceSetSummary {
        let mut s = json!({
            "set": { "name": "Atari", "root": "sets", "path": "Atari", "dat": "No-Intro/Atari", "added_at": "t",
                     "last_verified_against": null },
            "latest": latest,
        });
        if let Some(outside) = outside {
            s["outside_coverage"] = outside;
        }
        serde_json::from_value(s).unwrap()
    }

    fn audit(games: serde_json::Value) -> serde_json::Value {
        json!({
            "id": 1, "source_set": "Atari", "started_at": "t", "finished_at": "t", "dat_revision_sha256": "r",
            "dat_revision_version": "1", "findings": {}, "games": games, "pruned": false,
        })
    }

    fn folder(id: i64, set: &str, path: &str) -> LibraryFolder {
        serde_json::from_value(json!({
            "id": id, "root": "play", "path": path, "system": "No-Intro/Atari", "source_set": set,
            "profile": "Everyday", "generation": 0, "created_at": "t", "writable": true, "build": null, "files": 0,
            "active_job": null, "latest_plan": null,
        }))
        .unwrap()
    }

    #[test]
    fn present_games_count_a_game_with_nothing_to_check_only_where_its_files_were_found() {
        let mut latest = audit(json!({"complete": 1, "nothing_to_check": 3}));
        latest["nothing_to_check_found"] = json!(1);
        let present = present_games(&summary(latest, None));
        assert_eq!(present, Some(2));
        assert_eq!(
            placed_line(1, present, "play: atari"),
            "1 of the 2 games in your source set is placed in play: atari"
        );
    }

    #[test]
    fn present_games_count_the_complete_and_incomplete_in_and_outside_the_coverage() {
        let counted = summary(
            audit(json!({"complete": 7, "incomplete": 2, "missing": 40})),
            Some(json!({"games": {"complete": 1, "missing": 9}, "unknown_region": 0})),
        );
        assert_eq!(present_games(&counted), Some(10));
        let whole = summary(audit(json!({"complete": 3})), None);
        assert_eq!(present_games(&whole), Some(3));
        let never = summary(serde_json::Value::Null, None);
        assert_eq!(present_games(&never), None);
    }

    #[test]
    fn the_placed_count_reads_against_the_games_present() {
        assert_eq!(
            placed_line(3, Some(10), "play: atari"),
            "3 of the 10 games in your source set are placed in play: atari"
        );
        assert_eq!(
            placed_line(1, Some(10), "play: atari"),
            "1 of the 10 games in your source set is placed in play: atari"
        );
        assert_eq!(
            placed_line(1, Some(1), "play: atari"),
            "The 1 game in your source set is placed in play: atari"
        );
        assert_eq!(
            placed_line(1, None, "play: atari"),
            "1 game in your source set is placed in play: atari"
        );
        assert_eq!(
            placed_line(4, Some(2), "play: atari"),
            "4 of the 4 games in your source set are placed in play: atari",
            "an audit older than the placements never counts fewer games than are placed"
        );
    }

    #[test]
    fn a_folder_reads_by_its_root_and_path_or_its_number_once_gone() {
        let folders = [folder(4, "Atari", "atari")];
        assert_eq!(folder_name(&folders, 4), "play: atari");
        assert_eq!(folder_name(&folders, 9), "library folder #9");
    }

    #[test]
    fn you_choose_the_holding_folder_only_when_several_are_bound() {
        let folders = [
            folder(1, "Atari", "a"),
            folder(2, "Other", "o"),
            folder(3, "Atari", "b"),
        ];
        assert_eq!(Holding::of(&folders, "Nothing", None), Holding::NoFolder);
        assert_eq!(
            Holding::of(&folders[..2], "Atari", Some(1)),
            Holding::Only(1)
        );
        assert_eq!(
            Holding::of(&folders, "Atari", None),
            Holding::Choose {
                folders: vec![1, 3],
                chosen: None
            }
        );
        assert_eq!(
            Holding::of(&folders, "Atari", Some(3)),
            Holding::Choose {
                folders: vec![1, 3],
                chosen: Some(3)
            }
        );
        let unchosen = Holding::Choose {
            folders: vec![1, 3],
            chosen: None,
        };
        assert!(unchosen.note().contains("copy"), "{}", unchosen.note());
        let chosen = Holding::Choose {
            folders: vec![1, 3],
            chosen: Some(3),
        };
        assert!(chosen.note().contains("moves nothing"), "{}", chosen.note());
    }

    #[test]
    fn a_game_is_placed_where_its_entries_name_a_placed_path() {
        let entry = |seq: u64, path: &str, game: &str| -> EntryRow {
            serde_json::from_value(json!({
                "seq": seq, "path": path, "finding": "matched",
                "roms": [{"game": game, "name": format!("{game}.a26")}],
            }))
            .unwrap()
        };
        let placed = game_placements(&[
            entry(1, "Pong (USA).zip", "Pong (USA)"),
            entry(2, ".title-select/placed/7/Combat (USA).zip", "Combat (USA)"),
            entry(
                3,
                ".title-select/placed/7/disc/Orbit (Track 1).bin",
                "Orbit",
            ),
            entry(
                4,
                ".title-select/placed/7/disc/Orbit (Track 2).bin",
                "Orbit",
            ),
        ]);
        assert_eq!(
            placed,
            HashMap::from([("Combat (USA)".to_string(), 7), ("Orbit".to_string(), 7)])
        );
    }

    fn built(target: &str, kind: &str) -> BuiltFile {
        serde_json::from_value(json!({
            "target": target, "game": "Pong (USA)", "kind": kind, "build": 1, "selection": 1,
            "audit": 1, "dat_revision": "r", "sources": [], "roms": [], "file": {"size": 1, "mtime_ns": 0},
        }))
        .unwrap()
    }

    fn placed(path: &str) -> PlacedFile {
        serde_json::from_value(json!({
            "game": "Pong (USA)", "library_folder": 7, "path": path, "file": {"size": 1, "mtime_ns": 0},
            "dev": 1, "ino": 2, "ctime_ns": 0,
        }))
        .unwrap()
    }

    #[test]
    fn a_file_is_the_only_copy_while_it_is_placed_whatever_its_build_kind() {
        let pong = "Pong (USA).zip";
        // Already built here as a repack, then placed: the build kind stays, the file is now the only copy.
        let placed_now = OnlyCopies::of(&[placed(pong)]);
        assert_eq!(
            placed_now.placement(&built(pong, "repack")),
            Some(Placement::Placed)
        );
        assert_eq!(
            placed_now.placement(&built(pong, "copy")),
            Some(Placement::Placed)
        );
        assert_eq!(
            placed_now.placement(&built("Combat (USA).zip", "copy")),
            None
        );
        // Returned to the source set, with a built copy kept here: no longer the only copy, even of kind place.
        let returned = OnlyCopies::of(&[]);
        assert_eq!(returned.placement(&built(pong, "place")), None);
        assert_eq!(returned.placement(&built(pong, "repack")), None);
    }

    fn review_flag(game: &str) -> ReviewFlag {
        ReviewFlag {
            game: game.into(),
            path: format!(".title-select/placed/7/{game}.chd"),
            reason: "track 2's SHA-1 differs from the DAT's".into(),
            build: 12,
            raised_at: "2026-11-01T10:00:00Z".into(),
        }
    }

    #[test]
    fn a_read_is_certain_only_once_the_latest_is_done_and_good() {
        let first = Fresh::<u32>::default();
        assert!(
            first.reading && first.certain().is_none(),
            "nothing read yet"
        );
        let read = first.next(Some(&(1, Ok(7))), 1);
        assert_eq!(read.certain(), Some(&7));
        // A job settled: the resource still holds the read stamped 1 while the read stamped 2 runs.
        let rereading = read.next(Some(&(1, Ok(7))), 2);
        assert!(rereading.reading);
        assert_eq!((rereading.certain(), rereading.last), (None, Some(7)));
        let failed = rereading.next(Some(&(2, Err("timed out".into()))), 2);
        assert_eq!(failed.problem.as_deref(), Some("timed out"));
        assert_eq!(
            (failed.certain(), failed.last),
            (None, Some(7)),
            "a failure keeps the last good value"
        );
        let again = failed.next(Some(&(3, Ok(8))), 3);
        assert_eq!(
            (again.certain(), again.problem.as_deref()),
            (Some(&8), None)
        );
    }

    #[test]
    fn a_table_marks_the_last_flags_read_and_says_when_they_arent_current() {
        let none = FlagsNow::default();
        assert!(none.reading && !none.any(["Orbit"].into_iter()));
        let known = none.next(Some(&stamped_flags(1, Ok(vec![review_flag("Orbit")]))), 1);
        assert_eq!(known.get("Orbit"), Some(&review_flag("Orbit")));
        assert!(known.any(["Pong", "Orbit"].into_iter()) && !known.any(["Pong"].into_iter()));
        assert!(!known.reading && known.problem.is_none());
        // After a job, the old flags stay marked but the table shows they're being read again.
        let rereading = known.next(Some(&stamped_flags(1, Ok(vec![review_flag("Orbit")]))), 2);
        assert!(rereading.reading && rereading.get("Orbit").is_some());
        let failed = rereading.next(
            Some(&stamped_flags(2, Err("the service is unreachable".into()))),
            2,
        );
        assert!(failed.get("Orbit").is_some() && failed.problem.is_some());
        assert_eq!(
            flags_unknown("the service is unreachable"),
            "Couldn't read which games need review: the service is unreachable. The marks show the last good read, so \
             a game without one may still need review."
        );
        assert!(FlagsNow::none().certain().is_some());
    }

    #[test]
    fn a_file_is_not_known_to_be_an_only_copy_until_its_folders_placed_files_are_read() {
        let pong = built("Pong (USA).zip", "repack");
        let combat = built("Combat (USA).zip", "copy");
        let files = [pong.clone(), combat.clone()];
        let markers = |copies: &Fresh<OnlyCopies>| -> Vec<Option<Placement>> {
            marked(&files, copies).into_iter().map(|(_, p)| p).collect()
        };
        // Loading: neither is known, never "not the only copy".
        let loading = Fresh::<OnlyCopies>::default();
        assert_eq!(
            markers(&loading),
            [Some(Placement::Unknown), Some(Placement::Unknown)]
        );
        // Loaded: the placed one is the only copy, the other certainly isn't.
        let loaded = loading.next(
            Some(&(1, Ok(OnlyCopies::of(&[placed("Pong (USA).zip")])))),
            1,
        );
        assert_eq!(markers(&loaded), [Some(Placement::Placed), None]);
        // Failed with nothing read before: neither is known.
        let failed = loading.next(Some(&(1, Err("timed out".into()))), 1);
        assert_eq!(
            markers(&failed),
            [Some(Placement::Unknown), Some(Placement::Unknown)]
        );
        // Failed or reading after a good read: the last placed one stays marked, the other isn't known.
        let failed_later = loaded.next(Some(&(2, Err("timed out".into()))), 2);
        assert_eq!(
            markers(&failed_later),
            [Some(Placement::Placed), Some(Placement::Unknown)]
        );
        let rereading = loaded.next(
            Some(&(1, Ok(OnlyCopies::of(&[placed("Pong (USA).zip")])))),
            2,
        );
        assert_eq!(
            markers(&rereading),
            [Some(Placement::Placed), Some(Placement::Unknown)]
        );
        assert_eq!(
            only_copies_unknown("timed out"),
            "Couldn't read which files are their games' only copies: timed out. A file marked not known may be one, \
             so don't delete it by hand."
        );
    }

    #[test]
    fn markers_follow_the_latest_placed_files_while_the_built_files_cant_be_read() {
        // The last good read of the built files, kept while later reads of them fail.
        let files = [
            built("Pong (USA).zip", "place"),
            built("Combat (USA).zip", "repack"),
        ];
        let markers = |copies: &Fresh<OnlyCopies>| -> Vec<Option<Placement>> {
            marked(&files, copies).into_iter().map(|(_, p)| p).collect()
        };
        let before = Fresh::default().next(
            Some(&(1, Ok(OnlyCopies::of(&[placed("Pong (USA).zip")])))),
            1,
        );
        assert_eq!(markers(&before), [Some(Placement::Placed), None]);
        // Pong returned with its built copy kept here, and Combat, already built here, placed.
        let after = before.next(
            Some(&(2, Ok(OnlyCopies::of(&[placed("Combat (USA).zip")])))),
            2,
        );
        assert_eq!(markers(&after), [None, Some(Placement::Placed)]);
    }

    #[test]
    fn a_flagged_game_says_why_and_which_build_found_it() {
        let flag = ReviewFlag {
            game: "Orbit".into(),
            path: ".title-select/placed/7/Orbit.chd".into(),
            reason: "track 2's SHA-1 differs from the DAT's".into(),
            build: 12,
            raised_at: "2026-11-01T10:00:00Z".into(),
        };
        let detail = flag_detail(&flag);
        assert!(
            detail.starts_with("track 2's SHA-1 differs from the DAT's. Build #12 found this"),
            "{detail}"
        );
        assert_eq!(flags_by_game(std::slice::from_ref(&flag))["Orbit"], flag);
        assert_eq!(review_line(1), "1 game needs review");
        assert_eq!(review_line(3), "3 games need review");
    }
}
