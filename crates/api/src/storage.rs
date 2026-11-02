//! The Storage Policy: how each folder stores a Game, as TorrentZip or loose, for Source Sets and for Play Libraries,
//! with a global default and per-system overrides.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
}

impl StorageFormat {
    pub fn label(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "TorrentZip",
            StorageFormat::Loose => "loose",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "torrent_zip",
            StorageFormat::Loose => "loose",
        }
    }

    pub fn parse(s: &str) -> Option<StorageFormat> {
        match s {
            "torrent_zip" => Some(StorageFormat::TorrentZip),
            "loose" => Some(StorageFormat::Loose),
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

impl ScopePolicy {
    /// What applies to `system`: its override where it has one, else the default.
    pub fn of(&self, system: &str) -> Storage {
        let own = self.systems.get(system);
        Storage {
            format: own.and_then(|s| s.format).unwrap_or(self.format),
            delete_originals: own
                .and_then(|s| s.delete_originals)
                .unwrap_or(self.delete_originals),
        }
    }
}

/// The whole Storage Policy. The default is TorrentZip everywhere, with originals kept.
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

/// Where Game `game`, with ROM names `roms`, is stored under `storage`, relative to its folder, and whether it is a
/// loose file: the ROM's own name for a loose single-ROM Game; else `<game>.zip`. The note says why a Game the
/// policy would store loose stays TorrentZip.
pub fn target_of(
    storage: Storage,
    game: &str,
    roms: &[&str],
) -> (String, bool, Option<&'static str>) {
    match (storage.format, roms) {
        (StorageFormat::Loose, [only]) if is_plain_name(only) => ((*only).to_owned(), true, None),
        (StorageFormat::Loose, [_]) => (format!("{game}.zip"), false, Some(NOT_A_FILE_NAME)),
        (StorageFormat::Loose, _) => (format!("{game}.zip"), false, Some(MULTI_ROM_STAYS_ZIP)),
        (StorageFormat::TorrentZip, _) => (format!("{game}.zip"), false, None),
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
        assert_eq!(scope.of("No-Intro/Atari").format, StorageFormat::Loose);
        assert_eq!(scope.of("No-Intro/Sega").format, StorageFormat::TorrentZip);
        scope.delete_originals = true;
        assert!(scope.of("No-Intro/Atari").delete_originals);
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
}
