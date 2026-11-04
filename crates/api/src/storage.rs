//! The Storage Policy: how each folder stores a Game, as TorrentZip, loose, a loose folder or CHD, for Source Sets and
//! for Play Libraries, with a global default and per-system overrides.

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
    /// systems' Source Sets (ADR 0013). A disc system's Play Library lays a disc's files out as its [`DiscLayout`]
    /// says (ADR 0014); in any other Play Library it stores a single-ROM Game loose, and any other stays TorrentZip.
    LooseFolder,
    /// `<game>.chd`, one CHD per disc, built by chdman from its `.cue` and `.bin` tracks or its `.iso`: only a disc
    /// system's Play Libraries store it, never a Source Set, which holds the DAT's exact bytes (ADR 0013). A disc
    /// it can't hold, and every other disc of its release then, stays TorrentZip.
    Chd,
}

impl StorageFormat {
    pub fn label(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "TorrentZip",
            StorageFormat::Loose => "loose",
            StorageFormat::LooseFolder => "loose folder",
            StorageFormat::Chd => "CHD",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StorageFormat::TorrentZip => "torrent_zip",
            StorageFormat::Loose => "loose",
            StorageFormat::LooseFolder => "loose_folder",
            StorageFormat::Chd => "chd",
        }
    }

    pub fn parse(s: &str) -> Option<StorageFormat> {
        match s {
            "torrent_zip" => Some(StorageFormat::TorrentZip),
            "loose" => Some(StorageFormat::Loose),
            "loose_folder" => Some(StorageFormat::LooseFolder),
            "chd" => Some(StorageFormat::Chd),
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

/// How a disc system's Play Library lays out the files of its discs stored loose or as CHD (ADR 0014). A disc
/// stored as TorrentZip is `<game>.zip` in the system folder whatever the layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscLayout {
    /// The layout ES-DE reads: a disc stored as one file, `.chd` or `.iso`, directly in the system folder; a
    /// single disc of several files, a `.cue` and its tracks, in a folder named exactly for its `.cue`,
    /// `<cue>/`; and every file of a Multi-disc Game flat in [`MULTIDISC`], with an `.m3u` named for the release,
    /// without its disc tag, in the system folder listing each disc's entry file in disc order.
    #[default]
    EsDe,
    /// Every file of every disc directly in the system folder, and no `.m3u`.
    Flat,
}

impl DiscLayout {
    pub fn label(self) -> &'static str {
        match self {
            DiscLayout::EsDe => "ES-DE",
            DiscLayout::Flat => "flat",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DiscLayout::EsDe => "es_de",
            DiscLayout::Flat => "flat",
        }
    }

    pub fn parse(s: &str) -> Option<DiscLayout> {
        match s {
            "es_de" => Some(DiscLayout::EsDe),
            "flat" => Some(DiscLayout::Flat),
            _ => None,
        }
    }
}

/// The folder, in a disc system's Play Library folder, that holds every file of its Multi-disc Games in the ES-DE
/// layout.
pub const MULTIDISC: &str = ".multidisc";

/// One system's overrides of its scope's defaults; `None` follows the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStorage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<StorageFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_originals: Option<bool>,
    /// A disc system's Play Library layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<DiscLayout>,
    /// Whether a disc system's Play Libraries put each disc's SBI file, from the system's latest fetched SBI pack,
    /// beside its `.cue` or `.chd`. Only a system opts in; there is no default to follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sbi: Option<bool>,
}

impl SystemStorage {
    /// Whether it overrides nothing.
    pub fn is_empty(&self) -> bool {
        self.format.is_none()
            && self.delete_originals.is_none()
            && self.layout.is_none()
            && self.sbi.is_none()
    }
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
    /// The layout of disc systems' Play Libraries; Source Sets have none.
    #[serde(default)]
    pub layout: DiscLayout,
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
        let discs = scope == StorageScope::PlayLibraries && is_disc_system(system);
        Storage {
            format: own.and_then(|s| s.format).unwrap_or(default),
            delete_originals: own
                .and_then(|s| s.delete_originals)
                .unwrap_or(self.delete_originals),
            layout: discs.then(|| own.and_then(|s| s.layout).unwrap_or(self.layout)),
            sbi: discs && own.and_then(|s| s.sbi).unwrap_or(false),
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
    /// How a disc system's Play Library lays out its discs; `None` for any other folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<DiscLayout>,
    /// Whether a disc system's Play Library puts each disc's SBI file beside it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sbi: bool,
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

/// Whether `name`, an entry anywhere in a folder Title Select writes, is one it keeps there for itself and an Audit
/// passes over: its marker and kept files (`.title-select…`), a private `.deleting` folder, and a file it is writing
/// under a temporary name (`.<name>.title-select-new-…`). Compared without case, since a folder may be on a
/// filesystem that ignores it. No Game's file, folder or ROM is ever written under such a name.
pub fn is_reserved_name(name: &str) -> bool {
    let name = name.to_lowercase();
    name.starts_with(".title-select")
        || name == ".deleting"
        || (name.starts_with('.') && name.contains(".title-select-new-"))
}

/// Whether `name` can name a file directly in a folder: not empty, no separator, not `.` or `..`, and not a
/// [reserved name](is_reserved_name).
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\', '\0'])
        && name != "."
        && name != ".."
        && !is_reserved_name(name)
}

/// Why a Game is stored as TorrentZip where the policy says loose or loose folder: its name or a ROM's is one Title
/// Select keeps its own work under.
pub const RESERVED_NAME_STAYS_ZIP: &str = "its name or a ROM's name is one Title Select keeps its own work under, \
     so it can't be stored loose, and it stays TorrentZip";

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
        (StorageFormat::Loose | StorageFormat::LooseFolder, [only]) if is_reserved_name(only) => {
            (format!("{game}.zip"), false, Some(RESERVED_NAME_STAYS_ZIP))
        }
        (StorageFormat::Loose | StorageFormat::LooseFolder, [_]) => {
            (format!("{game}.zip"), false, Some(NOT_A_FILE_NAME))
        }
        (StorageFormat::Loose | StorageFormat::LooseFolder, _) => {
            (format!("{game}.zip"), false, Some(MULTI_ROM_STAYS_ZIP))
        }
        // A CHD is decided per release, with each track's size, by [`chd_targets`]; anything else is TorrentZip.
        (StorageFormat::TorrentZip | StorageFormat::Chd, _) => (format!("{game}.zip"), false, None),
    }
}

/// How chdman builds a disc's CHD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChdKind {
    /// A CD: one `.cue` sheet and its raw `.bin` tracks, built with `createcd`.
    Cd,
    /// A DVD: one `.iso` image of 2048-byte sectors, built with `createdvd`.
    Dvd,
}

/// The bytes of one raw CD sector, which every `.bin` track a CHD holds is a whole number of.
pub const CD_SECTOR: u64 = 2352;

/// The bytes of one DVD sector, which an `.iso` a CHD holds is a whole number of.
pub const DVD_SECTOR: u64 = 2048;

/// Why a disc is stored as TorrentZip where the policy says CHD: it is not one `.cue` with its `.bin` tracks, nor one
/// `.iso`.
pub const NOT_A_CHD_DISC: &str = "it is not one .cue sheet with its .bin tracks, nor one .iso image, so it can't be \
     stored as CHD, and it stays TorrentZip";

/// Why a disc is stored as TorrentZip where the policy says CHD: a track is not whole sectors.
pub const NOT_WHOLE_SECTORS: &str = "a track or image is not a whole number of sectors, so a CHD can't hold its \
     exact bytes, and it stays TorrentZip";

/// Why a disc is stored as TorrentZip where the policy says CHD: its name can't name a file.
pub const NOT_A_CHD_NAME: &str =
    "its name is not a plain file name, so it can't be stored as CHD, and it stays TorrentZip";

/// Whether, and as which kind, a disc `game` with ROMs `roms`, by name and size, can be stored as a CHD whose
/// extraction gives back its exact bytes: one `.cue` with every other ROM a `.bin` of whole raw sectors, or one
/// `.iso` of whole DVD sectors. The cue sheet itself is not inside the CHD; it is checked against the DAT before
/// chdman reads it, and each track afterwards. Otherwise the note says why not.
pub fn chd_of(game: &str, roms: &[(&str, u64)]) -> Result<ChdKind, &'static str> {
    let ext = |name: &str| {
        name.rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let target = format!("{game}.chd");
    if !is_plain_name(&target) || invalid_name(&target).is_some() {
        return Err(NOT_A_CHD_NAME);
    }
    let names: Vec<String> = roms.iter().map(|(n, _)| ext(n)).collect();
    let kind = match names.iter().filter(|e| *e == "cue").count() {
        1 if roms.len() > 1 && names.iter().all(|e| e == "cue" || e == "bin") => ChdKind::Cd,
        0 if roms.len() == 1 && names[0] == "iso" => ChdKind::Dvd,
        _ => return Err(NOT_A_CHD_DISC),
    };
    let whole = roms
        .iter()
        .zip(&names)
        .all(|((_, size), e)| match e.as_str() {
            "bin" => *size > 0 && size % CD_SECTOR == 0,
            "iso" => *size > 0 && size % DVD_SECTOR == 0,
            _ => true,
        });
    if !whole {
        return Err(NOT_WHOLE_SECTORS);
    }
    Ok(kind)
}

/// Each of `games`, by name with its ROMs' names and sizes, that `storage` stores as a CHD, with its kind; or why it
/// stays TorrentZip. Every disc of a release is stored alike: once one can't be a CHD, none of them is. Empty unless
/// the policy says CHD.
pub fn chd_targets<'a>(
    storage: Storage,
    games: impl IntoIterator<Item = (&'a str, Vec<(&'a str, u64)>)>,
) -> BTreeMap<String, Result<ChdKind, &'static str>> {
    if storage.format != StorageFormat::Chd {
        return BTreeMap::new();
    }
    let mut out: BTreeMap<String, Result<ChdKind, &'static str>> = games
        .into_iter()
        .map(|(game, roms)| (game.to_owned(), chd_of(game, &roms)))
        .collect();
    let refused: std::collections::BTreeSet<String> = out
        .iter()
        .filter(|(_, r)| r.is_err())
        .map(|(g, _)| release_of(g))
        .collect();
    for (game, chd) in out.iter_mut() {
        if chd.is_ok() && refused.contains(&release_of(game)) {
            *chd = Err(RELEASE_STAYS_ZIP);
        }
    }
    out
}

/// Why a disc of a Multi-disc Game is stored as TorrentZip where the policy says loose, loose folder or CHD: another of
/// its discs can't be, and every disc of a game is stored alike.
pub const RELEASE_STAYS_ZIP: &str = "another disc of the same game can't be stored as the storage policy says, and \
     every disc of a game is stored alike, so it stays TorrentZip";

/// The release a disc belongs to: Game `game` without its disc tag, once disc names are normalized, so that every
/// disc of a Multi-disc Game has the same one, which a repair stores every disc of alike. A Game that is no disc is
/// its own release.
pub fn release_of(game: &str) -> String {
    crate::discs::disc(game).map_or_else(|| game.to_owned(), |d| d.release)
}

/// Why a disc of several files is stored as TorrentZip where the policy says loose folder in the ES-DE layout: no one
/// `.cue` sheet names the disc for an emulator.
pub const NOT_A_CUE_FOLDER: &str = "it has several files but not exactly one .cue sheet for an emulator to open, so \
     it can't be stored as a loose folder, and it stays TorrentZip";

/// How a Play Library stores one Game, as [`lay_out`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Laid {
    /// TorrentZip, loose, or CHD: a loose folder's files are each loose.
    pub format: StorageFormat,
    /// Every file it is made of, relative to the folder: `<game>.zip`, the CHD, or one loose file per ROM, in ROM
    /// order.
    pub files: Vec<String>,
    /// The file an emulator is pointed at, for a disc of a disc layout: its `.chd`, its one image, or its `.cue`.
    pub entry: Option<String>,
    /// Why it is stored as TorrentZip where the policy says otherwise.
    pub note: Option<&'static str>,
}

impl Laid {
    fn zip(game: &str, note: Option<&'static str>) -> Laid {
        Laid {
            format: StorageFormat::TorrentZip,
            files: vec![format!("{game}.zip")],
            entry: None,
            note,
        }
    }
}

/// How a Play Library stores each of `games`, by name with its ROMs' names and sizes, under `storage`: the one
/// decision of each Game's files and format. A disc system's layout ([`DiscLayout`]) places its discs stored loose or
/// as CHD; a Multi-disc Game is a release with more than one disc among `games`. Every disc of a release is stored
/// alike: once one can't be stored as the policy says, each stays TorrentZip.
pub fn lay_out<'a>(
    storage: Storage,
    games: impl IntoIterator<Item = (&'a str, Vec<(&'a str, u64)>)>,
) -> BTreeMap<String, Laid> {
    let games: Vec<(&str, Vec<(&str, u64)>)> = games.into_iter().collect();
    let chds = chd_targets(storage, games.iter().map(|(g, r)| (*g, r.clone())));
    let mut discs: BTreeMap<String, usize> = BTreeMap::new();
    for (game, _) in &games {
        if let Some(disc) = crate::discs::disc(game) {
            *discs.entry(disc.release).or_default() += 1;
        }
    }
    let multidisc = |game: &str| {
        crate::discs::disc(game).is_some_and(|d| discs.get(&d.release).is_some_and(|n| *n > 1))
    };
    let usable = |name: &str| is_plain_name(name) && invalid_name(name).is_none();
    let ext = |name: &str| {
        name.rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let mut out: BTreeMap<String, Laid> = BTreeMap::new();
    for (game, roms) in &games {
        let names: Vec<&str> = roms.iter().map(|(n, _)| *n).collect();
        let laid = match (storage.format, storage.layout) {
            (StorageFormat::Chd, _) => match chds.get(*game) {
                Some(Ok(_)) => {
                    let target = format!("{game}.chd");
                    Laid {
                        format: StorageFormat::Chd,
                        files: vec![target.clone()],
                        entry: storage.layout.map(|_| target),
                        note: None,
                    }
                }
                Some(Err(note)) => Laid::zip(game, Some(note)),
                None => Laid::zip(game, None),
            },
            (StorageFormat::LooseFolder, Some(layout)) if names.len() > 1 => {
                let distinct: std::collections::BTreeSet<String> =
                    names.iter().map(|r| r.to_lowercase()).collect();
                let cues: Vec<&str> = names.iter().copied().filter(|n| ext(n) == "cue").collect();
                if distinct.len() != names.len() || !names.iter().all(|n| usable(n)) {
                    let reserved = names.iter().any(|r| is_reserved_name(r));
                    Laid::zip(
                        game,
                        Some(match reserved {
                            true => RESERVED_NAME_STAYS_ZIP,
                            false => NOT_FOLDER_NAMES,
                        }),
                    )
                } else if layout == DiscLayout::EsDe && cues.len() != 1 {
                    Laid::zip(game, Some(NOT_A_CUE_FOLDER))
                } else {
                    let dir = match (layout, cues.as_slice()) {
                        (DiscLayout::EsDe, [cue]) if !multidisc(game) => Some((*cue).to_owned()),
                        _ => None,
                    };
                    let at = |name: &str| match &dir {
                        Some(dir) => format!("{dir}/{name}"),
                        None => name.to_owned(),
                    };
                    Laid {
                        format: StorageFormat::Loose,
                        files: names.iter().map(|n| at(n)).collect(),
                        entry: match cues.as_slice() {
                            [cue] => Some(at(cue)),
                            _ => None,
                        },
                        note: None,
                    }
                }
            }
            (StorageFormat::Loose | StorageFormat::LooseFolder, layout) => {
                match target_of(storage, game, &names) {
                    (target, true, _) => Laid {
                        format: StorageFormat::Loose,
                        entry: layout.map(|_| target.clone()),
                        files: vec![target],
                        note: None,
                    },
                    (_, false, note) => Laid::zip(game, note),
                }
            }
            (StorageFormat::TorrentZip, _) => Laid::zip(game, None),
        };
        out.insert((*game).to_owned(), laid);
    }
    // Every disc of a release alike: CHD already is, by [`chd_targets`].
    if matches!(
        storage.format,
        StorageFormat::Loose | StorageFormat::LooseFolder
    ) {
        let refused: std::collections::BTreeSet<String> = out
            .iter()
            .filter(|(g, l)| l.note.is_some() && multidisc(g))
            .map(|(g, _)| release_of(g))
            .collect();
        for (game, laid) in out.iter_mut() {
            if laid.format != StorageFormat::TorrentZip && refused.contains(&release_of(game)) {
                *laid = Laid::zip(game, Some(RELEASE_STAYS_ZIP));
            }
        }
    }
    if storage.layout == Some(DiscLayout::EsDe) {
        for (game, laid) in out.iter_mut() {
            if laid.format != StorageFormat::TorrentZip && multidisc(game) {
                let within = |name: &String| format!("{MULTIDISC}/{name}");
                laid.files = laid.files.iter().map(within).collect();
                laid.entry = laid.entry.as_ref().map(within);
            }
        }
    }
    out
}

/// The `.m3u` a Multi-disc Game's release is listed by in the ES-DE layout, in the system folder.
pub fn playlist_name(release: &str) -> String {
    format!("{release}.m3u")
}

/// What an `.m3u` holds: each disc's entry file, relative to the system folder, one to a line, in disc order.
pub fn playlist(entries: &[&str]) -> String {
    entries.iter().map(|e| format!("{e}\n")).collect()
}

/// Where a disc's SBI file goes: beside its entry file `entry`, a `.cue` or a `.chd`, named for it.
pub fn sbi_beside(entry: &str) -> Option<String> {
    let (stem, ext) = entry.rsplit_once('.')?;
    matches!(ext.to_ascii_lowercase().as_str(), "cue" | "chd").then(|| format!("{stem}.sbi"))
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
        let reserved = is_reserved_name(game) || roms.iter().any(|r| is_reserved_name(r));
        return (
            Placement::TorrentZip(format!("{game}.zip")),
            Some(match reserved {
                true => RESERVED_NAME_STAYS_ZIP,
                false => NOT_FOLDER_NAMES,
            }),
        );
    }
    // A Source Set never holds a CHD: were the policy to say so, a Repair writes TorrentZip.
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
                ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
    fn a_game_or_rom_named_as_title_select_work_is_never_stored_loose() {
        let folder = Storage {
            format: StorageFormat::LooseFolder,
            delete_originals: false,
            ..Default::default()
        };
        for (game, roms) in [
            (".deleting", &["a.bin", "b.bin"][..]),
            (".Title-Select-Retained", &["a.bin", "b.bin"]),
            ("Disc", &["a.bin", ".title-select"]),
            ("Disc", &["a.bin", ".b.bin.title-select-new-1"]),
        ] {
            let (placed, note) = placement(folder, game, roms);
            assert_eq!(
                placed,
                Placement::TorrentZip(format!("{game}.zip")),
                "{game}"
            );
            assert_eq!(note, Some(RESERVED_NAME_STAYS_ZIP), "{game}");
        }
        let (placed, note) = placement(folder, "Solo", &[".DELETING"]);
        assert_eq!(placed, Placement::TorrentZip("Solo.zip".into()));
        assert_eq!(note, Some(RESERVED_NAME_STAYS_ZIP));
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

    #[test]
    fn a_disc_is_a_chd_when_its_bytes_can_come_back_whole() {
        let cd = [
            ("A.cue", 90),
            ("A (Track 1).bin", CD_SECTOR * 10),
            ("A (Track 2).bin", CD_SECTOR),
        ];
        assert_eq!(chd_of("A", &cd), Ok(ChdKind::Cd));
        assert_eq!(chd_of("B", &[("B.iso", DVD_SECTOR * 3)]), Ok(ChdKind::Dvd));
        assert_eq!(
            chd_of("C", &[("C.cue", 90), ("C.bin", CD_SECTOR + 1)]),
            Err(NOT_WHOLE_SECTORS)
        );
        assert_eq!(
            chd_of("D", &[("D.gdi", 90), ("D.raw", CD_SECTOR)]),
            Err(NOT_A_CHD_DISC)
        );
        assert_eq!(chd_of("E", &[("E.cue", 90)]), Err(NOT_A_CHD_DISC));
        assert_eq!(chd_of("F/G", &[("F.iso", DVD_SECTOR)]), Err(NOT_A_CHD_NAME));
    }

    #[test]
    fn every_disc_of_a_release_is_a_chd_or_none_is() {
        let chd = Storage {
            format: StorageFormat::Chd,
            delete_originals: false,
            ..Default::default()
        };
        let good = |n: &'static str| vec![(n, 90), ("t.bin", CD_SECTOR)];
        let targets = chd_targets(
            chd,
            [
                ("Saga (USA) (Disc 1)", good("a.cue")),
                ("Saga (USA) (Disc 2)", vec![("b.cue", 90), ("t.bin", 7)]),
                ("Solo (USA)", good("s.cue")),
            ],
        );
        assert_eq!(targets["Saga (USA) (Disc 1)"], Err(RELEASE_STAYS_ZIP));
        assert_eq!(targets["Saga (USA) (Disc 2)"], Err(NOT_WHOLE_SECTORS));
        assert_eq!(targets["Solo (USA)"], Ok(ChdKind::Cd));
        assert!(chd_targets(Storage::default(), [("Solo (USA)", good("s.cue"))]).is_empty());
    }

    fn play(format: StorageFormat, layout: DiscLayout) -> Storage {
        Storage {
            format,
            layout: Some(layout),
            ..Storage::default()
        }
    }

    /// A PlayStation-like DAT: a cue/bin single disc, a one-image single disc, and a cue/bin release on two discs.
    fn psx() -> Vec<(&'static str, Vec<(&'static str, u64)>)> {
        vec![
            (
                "Orbit (USA)",
                vec![
                    ("Orbit (USA).cue", 90),
                    ("Orbit (USA) (Track 1).bin", CD_SECTOR * 4),
                    ("Orbit (USA) (Track 2).bin", CD_SECTOR),
                ],
            ),
            ("Atlas (USA)", vec![("Atlas (USA).iso", DVD_SECTOR * 4)]),
            (
                "Saga (USA) (Disc 1)",
                vec![
                    ("Saga (USA) (Disc 1).cue", 90),
                    ("Saga (USA) (Disc 1).bin", CD_SECTOR * 2),
                ],
            ),
            (
                "Saga (USA) (Disc 2)",
                vec![
                    ("Saga (USA) (Disc 2).cue", 90),
                    ("Saga (USA) (Disc 2).bin", CD_SECTOR * 2),
                ],
            ),
        ]
    }

    fn files(
        laid: &BTreeMap<String, Laid>,
        game: &str,
    ) -> (StorageFormat, Vec<String>, Option<String>) {
        let l = &laid[game];
        (l.format, l.files.clone(), l.entry.clone())
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn the_es_de_layout_puts_a_cue_folder_in_a_folder_named_for_its_cue_and_every_disc_of_a_release_in_multidisc()
     {
        let laid = lay_out(play(StorageFormat::LooseFolder, DiscLayout::EsDe), psx());
        assert_eq!(
            files(&laid, "Orbit (USA)"),
            (
                StorageFormat::Loose,
                owned(&[
                    "Orbit (USA).cue/Orbit (USA).cue",
                    "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
                    "Orbit (USA).cue/Orbit (USA) (Track 2).bin"
                ]),
                Some("Orbit (USA).cue/Orbit (USA).cue".into())
            )
        );
        assert_eq!(
            files(&laid, "Atlas (USA)"),
            (
                StorageFormat::Loose,
                owned(&["Atlas (USA).iso"]),
                Some("Atlas (USA).iso".into())
            )
        );
        assert_eq!(
            files(&laid, "Saga (USA) (Disc 2)"),
            (
                StorageFormat::Loose,
                owned(&[
                    ".multidisc/Saga (USA) (Disc 2).cue",
                    ".multidisc/Saga (USA) (Disc 2).bin"
                ]),
                Some(".multidisc/Saga (USA) (Disc 2).cue".into())
            )
        );
    }

    #[test]
    fn the_es_de_layout_puts_a_chd_in_the_system_folder_or_in_multidisc() {
        let laid = lay_out(play(StorageFormat::Chd, DiscLayout::EsDe), psx());
        assert_eq!(
            files(&laid, "Orbit (USA)"),
            (
                StorageFormat::Chd,
                owned(&["Orbit (USA).chd"]),
                Some("Orbit (USA).chd".into())
            )
        );
        assert_eq!(
            files(&laid, "Saga (USA) (Disc 1)"),
            (
                StorageFormat::Chd,
                owned(&[".multidisc/Saga (USA) (Disc 1).chd"]),
                Some(".multidisc/Saga (USA) (Disc 1).chd".into())
            )
        );
    }

    #[test]
    fn the_flat_layout_puts_every_file_in_the_system_folder() {
        let laid = lay_out(play(StorageFormat::LooseFolder, DiscLayout::Flat), psx());
        assert_eq!(
            files(&laid, "Saga (USA) (Disc 1)").1,
            owned(&["Saga (USA) (Disc 1).cue", "Saga (USA) (Disc 1).bin"])
        );
        let laid = lay_out(play(StorageFormat::Chd, DiscLayout::Flat), psx());
        assert_eq!(
            files(&laid, "Saga (USA) (Disc 1)").1,
            owned(&["Saga (USA) (Disc 1).chd"])
        );
    }

    #[test]
    fn a_disc_of_several_files_without_one_cue_stays_zip_and_takes_its_release_with_it() {
        let mut games = psx();
        games[3].1 = vec![("Saga (USA) (Disc 2).gdi", 90), ("track01.raw", 4)];
        let laid = lay_out(play(StorageFormat::LooseFolder, DiscLayout::EsDe), games);
        let zip = |game: &str| (laid[game].format, laid[game].files.clone(), laid[game].note);
        assert_eq!(
            zip("Saga (USA) (Disc 2)"),
            (
                StorageFormat::TorrentZip,
                owned(&["Saga (USA) (Disc 2).zip"]),
                Some(NOT_A_CUE_FOLDER)
            )
        );
        assert_eq!(
            zip("Saga (USA) (Disc 1)"),
            (
                StorageFormat::TorrentZip,
                owned(&["Saga (USA) (Disc 1).zip"]),
                Some(RELEASE_STAYS_ZIP)
            )
        );
    }

    #[test]
    fn loose_stores_only_a_single_file_disc_loose_and_a_release_alike() {
        let laid = lay_out(play(StorageFormat::Loose, DiscLayout::EsDe), psx());
        assert_eq!(laid["Atlas (USA)"].files, owned(&["Atlas (USA).iso"]));
        assert_eq!(laid["Orbit (USA)"].note, Some(MULTI_ROM_STAYS_ZIP));
        assert_eq!(
            laid["Saga (USA) (Disc 1)"].format,
            StorageFormat::TorrentZip
        );
    }

    #[test]
    fn without_a_disc_layout_a_loose_folder_stores_only_a_single_rom_game_loose() {
        let storage = Storage {
            format: StorageFormat::LooseFolder,
            ..Storage::default()
        };
        let laid = lay_out(storage, psx());
        assert_eq!(laid["Atlas (USA)"].files, owned(&["Atlas (USA).iso"]));
        assert_eq!(laid["Atlas (USA)"].entry, None);
        assert_eq!(laid["Orbit (USA)"].files, owned(&["Orbit (USA).zip"]));
    }

    #[test]
    fn a_playlist_lists_each_entry_on_its_own_line() {
        assert_eq!(
            playlist(&[".multidisc/A (Disc 1).cue", ".multidisc/A (Disc 2).chd"]),
            ".multidisc/A (Disc 1).cue\n.multidisc/A (Disc 2).chd\n"
        );
        assert_eq!(sbi_beside("X.cue/X.cue").as_deref(), Some("X.cue/X.sbi"));
        assert_eq!(
            sbi_beside(".multidisc/X.CHD").as_deref(),
            Some(".multidisc/X.sbi")
        );
        assert_eq!(sbi_beside("X.iso"), None);
    }

    #[test]
    fn only_a_disc_systems_play_libraries_have_a_disc_layout() {
        let mut policy = StoragePolicy::default();
        let psx = "Redump/Sony - PlayStation";
        assert_eq!(
            policy.of(StorageScope::PlayLibraries, psx).layout,
            Some(DiscLayout::EsDe)
        );
        assert_eq!(policy.of(StorageScope::SourceSets, psx).layout, None);
        assert_eq!(
            policy
                .of(StorageScope::PlayLibraries, "No-Intro/Atari")
                .layout,
            None
        );
        policy.play_libraries.systems.insert(
            psx.into(),
            SystemStorage {
                layout: Some(DiscLayout::Flat),
                sbi: Some(true),
                ..SystemStorage::default()
            },
        );
        let storage = policy.of(StorageScope::PlayLibraries, psx);
        assert_eq!(
            (storage.layout, storage.sbi),
            (Some(DiscLayout::Flat), true)
        );
    }
}
