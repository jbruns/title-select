//! The Storage Policy: how each folder stores a Game, as TorrentZip, loose or a loose folder, for Source Sets and for
//! Play Libraries, with a global default and per-system overrides.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::build::invalid_name;

/// How a folder stores a Game.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageFormat {
    /// One TorrentZip per Game, named for it, holding its ROMs under their DAT names.
    #[default]
    TorrentZip,
    /// A Game with one ROM is that ROM, under its DAT name, directly in the folder. A Game with more stays
    /// TorrentZip.
    Loose,
    /// `<game>/`, holding each of the Game's ROMs loose under its DAT name: for any Game, and the default for disc
    /// systems' Source Sets (ADR 0013). Only Source Sets store it for now; a Play Library's disc layout is #100.
    LooseFolder,
}

impl StorageFormat {
    pub fn label(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "TorrentZip",
            StorageFormat::Loose => "loose",
            StorageFormat::LooseFolder => "loose folder",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "torrent_zip",
            StorageFormat::Loose => "loose",
            StorageFormat::LooseFolder => "loose_folder",
        }
    }

    pub fn parse(s: &str) -> Option<StorageFormat> {
        match s {
            "torrent_zip" => Some(StorageFormat::TorrentZip),
            "loose" => Some(StorageFormat::Loose),
            "loose_folder" => Some(StorageFormat::LooseFolder),
            _ => None,
        }
    }
}

/// The folders a Storage Policy scope covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageScope {
    SourceSets,
    PlayLibraries,
}

impl StorageScope {
    pub fn as_str(self) -> &'static str {
        match self {
            StorageScope::SourceSets => "source_sets",
            StorageScope::PlayLibraries => "play_libraries",
        }
    }
}

/// One system's overrides of its scope's defaults; `None` follows the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStorage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<StorageFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_originals: Option<bool>,
}

/// One scope's policy: its defaults, and the overrides of each system, keyed by its DAT as `<provider>/<name>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePolicy {
    #[serde(default)]
    pub format: StorageFormat,
    /// Whether a conversion deletes each original once its converted file is verified, rather than keeping it in
    /// Kept Files.
    #[serde(default)]
    pub delete_originals: bool,
    #[serde(default)]
    pub systems: BTreeMap<String, SystemStorage>,
}

/// Whether `system`, a DAT as `<provider>/<name>`, is a disc system: one whose DAT Redump publishes. Its Games are
/// discs, `.cue` sheets with their tracks or images, so its Source Sets default to loose folders (ADR 0013).
pub fn is_disc_system(system: &str) -> bool {
    system
        .split_once('/')
        .is_some_and(|(provider, _)| provider.eq_ignore_ascii_case("Redump"))
}

impl ScopePolicy {
    /// What applies to `system` in `scope`: its override where it has one, else the default, which for a disc
    /// system's Source Sets is a loose folder.
    pub fn of(&self, scope: StorageScope, system: &str) -> Storage {
        let own = self.systems.get(system);
        let default = match scope {
            StorageScope::SourceSets if is_disc_system(system) => StorageFormat::LooseFolder,
            _ => self.format,
        };
        Storage {
            format: own.and_then(|s| s.format).unwrap_or(default),
            delete_originals: own
                .and_then(|s| s.delete_originals)
                .unwrap_or(self.delete_originals),
        }
    }
}

/// The whole Storage Policy. The default is TorrentZip everywhere, except loose folders for disc systems' Source
/// Sets, with originals kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoragePolicy {
    #[serde(default)]
    pub source_sets: ScopePolicy,
    #[serde(default)]
    pub play_libraries: ScopePolicy,
}

impl StoragePolicy {
    pub fn scope(&self, scope: StorageScope) -> &ScopePolicy {
        match scope {
            StorageScope::SourceSets => &self.source_sets,
            StorageScope::PlayLibraries => &self.play_libraries,
        }
    }

    /// What applies to folders of `scope` bound to `system`.
    pub fn of(&self, scope: StorageScope, system: &str) -> Storage {
        self.scope(scope).of(scope, system)
    }
}

/// What the Storage Policy says for one folder's system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    pub format: StorageFormat,
    pub delete_originals: bool,
}

/// Whether `storage` is the default, TorrentZip with originals kept, which a plan leaves out of its JSON.
pub fn is_default(storage: &Storage) -> bool {
    *storage == Storage::default()
}

/// Why a Game is stored as TorrentZip where the policy says loose.
pub const MULTI_ROM_STAYS_ZIP: &str = "it has more than one ROM, and only a game with one ROM can be stored loose, so it stays TorrentZip";

/// Why a single-ROM Game is stored as TorrentZip where the policy says loose.
pub const NOT_A_FILE_NAME: &str =
    "its ROM's name is not a plain file name, so it can't be stored loose, and it stays TorrentZip";

/// Whether `name` can name a file directly in a folder: not empty, no separator, not `.` or `..`, and not one of the
/// hidden names Title Select keeps its own work under.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\', '\0'])
        && name != "."
        && name != ".."
        && !name.starts_with(".title-select")
}

/// Why a Game is stored as TorrentZip where the policy says loose folder.
pub const NOT_FOLDER_NAMES: &str = "its name or a ROM's name is not a plain file name, so it can't be stored as a \
     loose folder, and it stays TorrentZip";

/// Where Game `game`, with ROM names `roms`, is stored under `storage`, relative to its folder, and whether it is a
/// loose file: the ROM's own name for a loose single-ROM Game; else `<game>.zip`. The note says why a Game the
/// policy would store loose stays TorrentZip. For a writer of one file per Game, a Build or an Addition, a loose
/// folder stores a single-ROM Game loose, as [`StorageFormat::Loose`] does; only a Repair of a Source Set writes
/// loose folders, through [`placement`].
pub fn target_of(
    storage: Storage,
    game: &str,
    roms: &[&str],
) -> (String, bool, Option<&'static str>) {
    match (storage.format, roms) {
        (StorageFormat::Loose | StorageFormat::LooseFolder, [only]) if is_plain_name(only) => {
            ((*only).to_owned(), true, None)
        }
        (StorageFormat::Loose | StorageFormat::LooseFolder, [_]) => {
            (format!("{game}.zip"), false, Some(NOT_A_FILE_NAME))
        }
        (StorageFormat::Loose | StorageFormat::LooseFolder, _) => {
            (format!("{game}.zip"), false, Some(MULTI_ROM_STAYS_ZIP))
        }
        (StorageFormat::TorrentZip, _) => (format!("{game}.zip"), false, None),
    }
}

/// Why a disc of a Multi-disc Game is stored as TorrentZip where the policy says loose folder: another of its discs
/// can't be, and every disc of a game is stored alike.
pub const RELEASE_STAYS_ZIP: &str = "another disc of the same game can't be stored as a loose folder, and every \
     disc of a game is stored alike, so it stays TorrentZip";

/// The release a disc belongs to: Game `game` without its `(Disc N)` tag, so that every disc of a Multi-disc Game
/// has the same one, which a repair stores every disc of alike. This is only the tag Redump names discs with;
/// recognizing Multi-disc Games in general is #98.
pub fn release_of(game: &str) -> String {
    let mut out = String::with_capacity(game.len());
    let mut rest = game;
    while let Some(at) = rest.find(" (Disc ") {
        let tag = &rest[at + 2..];
        match tag.find(')') {
            Some(end) => {
                out.push_str(&rest[..at]);
                rest = &tag[end + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Where a Repair stores a Game in a Source Set's folder, relative to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// One TorrentZip, `<game>.zip`, holding its ROMs.
    TorrentZip(String),
    /// Its one ROM, loose under its DAT name.
    Loose(String),
    /// A loose folder: `folder`, `<game>`, holding each ROM loose as `<game>/<rom>`, in the order of `roms`.
    Folder { folder: String, files: Vec<String> },
}

impl Placement {
    /// Every file it is made of, relative to the Source Set's folder.
    pub fn files(&self) -> Vec<String> {
        match self {
            Placement::TorrentZip(name) | Placement::Loose(name) => vec![name.clone()],
            Placement::Folder { files, .. } => files.clone(),
        }
    }

    /// The format it is stored in.
    pub fn format(&self) -> StorageFormat {
        match self {
            Placement::TorrentZip(_) => StorageFormat::TorrentZip,
            Placement::Loose(_) => StorageFormat::Loose,
            Placement::Folder { .. } => StorageFormat::LooseFolder,
        }
    }
}

/// The one decision of how a Repair stores Game `game`, with ROM names `roms`, in a Source Set under `storage`, with
/// the note saying why a Game is not stored as the policy says. A loose folder holds any Game whose name and ROM
/// names are plain file names, each ROM once; every other Game stays TorrentZip, so a Game's ROMs are never split
/// across formats.
pub fn placement(storage: Storage, game: &str, roms: &[&str]) -> (Placement, Option<&'static str>) {
    if storage.format == StorageFormat::LooseFolder {
        let distinct: std::collections::BTreeSet<String> =
            roms.iter().map(|r| r.to_lowercase()).collect();
        let usable = |name: &str| is_plain_name(name) && invalid_name(name).is_none();
        if !roms.is_empty()
            && usable(game)
            && distinct.len() == roms.len()
            && roms.iter().all(|r| usable(r))
        {
            return (
                Placement::Folder {
                    folder: game.to_owned(),
                    files: roms.iter().map(|r| format!("{game}/{r}")).collect(),
                },
                None,
            );
        }
        return (
            Placement::TorrentZip(format!("{game}.zip")),
            Some(NOT_FOLDER_NAMES),
        );
    }
    match target_of(storage, game, roms) {
        (name, true, note) => (Placement::Loose(name), note),
        (name, false, note) => (Placement::TorrentZip(name), note),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_system_override_beats_the_default() {
        let mut scope = ScopePolicy::default();
        scope.systems.insert(
            "No-Intro/Atari".into(),
            SystemStorage {
                format: Some(StorageFormat::Loose),
                delete_originals: None,
            },
        );
        let of = |system| scope.of(StorageScope::SourceSets, system);
        assert_eq!(of("No-Intro/Atari").format, StorageFormat::Loose);
        assert_eq!(of("No-Intro/Sega").format, StorageFormat::TorrentZip);
        scope.delete_originals = true;
        assert!(
            scope
                .of(StorageScope::SourceSets, "No-Intro/Atari")
                .delete_originals
        );
    }

    #[test]
    fn only_a_single_rom_game_is_loose() {
        let loose = Storage {
            format: StorageFormat::Loose,
            delete_originals: false,
        };
        assert_eq!(
            target_of(loose, "Pong", &["pong.a26"]),
            ("pong.a26".into(), true, None)
        );
        let (name, is_loose, note) = target_of(loose, "Pair", &["a.bin", "b.bin"]);
        assert_eq!((name.as_str(), is_loose), ("Pair.zip", false));
        assert!(note.is_some());
    }

    #[test]
    fn a_disc_systems_source_sets_default_to_loose_folders_unless_it_overrides() {
        let mut policy = StoragePolicy::default();
        let psx = "Redump/Sony - PlayStation";
        assert_eq!(
            policy.of(StorageScope::SourceSets, psx).format,
            StorageFormat::LooseFolder
        );
        assert_eq!(
            policy.of(StorageScope::PlayLibraries, psx).format,
            StorageFormat::TorrentZip
        );
        assert_eq!(
            policy.of(StorageScope::SourceSets, "No-Intro/Atari").format,
            StorageFormat::TorrentZip
        );
        policy.source_sets.systems.insert(
            psx.into(),
            SystemStorage {
                format: Some(StorageFormat::TorrentZip),
                delete_originals: None,
            },
        );
        assert_eq!(
            policy.of(StorageScope::SourceSets, psx).format,
            StorageFormat::TorrentZip
        );
    }

    #[test]
    fn a_loose_folder_holds_every_rom_of_a_game_or_none() {
        let folder = Storage {
            format: StorageFormat::LooseFolder,
            delete_originals: false,
        };
        let (placed, note) = placement(
            folder,
            "Disc (USA)",
            &["Disc (USA).cue", "Disc (USA) (Track 1).bin"],
        );
        assert_eq!(
            placed,
            Placement::Folder {
                folder: "Disc (USA)".into(),
                files: vec![
                    "Disc (USA)/Disc (USA).cue".into(),
                    "Disc (USA)/Disc (USA) (Track 1).bin".into()
                ],
            }
        );
        assert_eq!(note, None);
        let (placed, note) = placement(folder, "Odd", &["ok.bin", "sub/track.bin"]);
        assert_eq!(placed, Placement::TorrentZip("Odd.zip".into()));
        assert_eq!(note, Some(NOT_FOLDER_NAMES));
    }

    #[test]
    fn the_discs_of_a_game_share_its_release() {
        assert_eq!(
            release_of("Saga (USA) (Disc 1) (Rev 1)"),
            "Saga (USA) (Rev 1)"
        );
        assert_eq!(release_of("Saga (USA) (Disc 2)"), "Saga (USA)");
        assert_eq!(release_of("Solo (USA)"), "Solo (USA)");
    }
}
