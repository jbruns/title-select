//! Audits a Source Set against a DAT Revision (see GLOSSARY.md and ADR 0002).

pub mod build;
pub mod calibrate;
pub mod cap;
pub mod chd;
pub mod dat;
pub mod fixdat;
pub mod meter;
pub mod payload;
pub mod pool;
mod read;
pub mod sevenz;
pub mod torrentzip;
pub mod zipdir;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

pub use read::Pools;
pub use title_select_api::Workers;
pub use title_select_api::report::*;

use dat::Dat;
use payload::Checksums;
use read::{FileRead, Known, Origin, ToRead};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read DAT: {0}")]
    Dat(String),
    #[error("cannot read source set: {0}")]
    SourceSet(String),
    /// The read stopped because a worker it needed couldn't start.
    #[error("the audit stopped: {0}")]
    Workers(String),
}

/// One payload reader and one hash worker: what the stateless Audit uses.
pub const ONE_WORKER_EACH: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// How far an Audit has got: files whose Entries are read, of the files found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
}

/// How an Audit runs.
pub struct Options<'a> {
    /// The DAT Revision's original filename, as the report names it.
    pub file: String,
    /// A canonical path every file and directory must resolve inside. One that resolves outside is an Unreadable
    /// Entry and is not read, and the file read is the one its resolved path names.
    pub confine: Option<&'a Path>,
    /// At least one of each: `readers` threads read files, and `hashers` threads checksum their payloads.
    pub workers: Workers,
    /// The pools `workers` share with other Jobs, which say how many of them may work at each moment: a permit is
    /// taken before each file and payload. All of them work when absent. A smaller pool holds back the next file,
    /// and stops no read under way.
    pub pools: Option<Pools<'a>>,
    /// Called as each file's Entries are read, from the reader threads.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    /// Whether ZIP archives are claimed from their directories ([`AuditEvidence::Quick`]), as the report records.
    /// Which earlier reads may be reused is up to `reads`.
    pub evidence: AuditEvidence,
    /// Earlier payload reads to reuse under the change test, and where to record this Audit's reads.
    pub reads: Option<&'a dyn PayloadReads>,
    /// Where the payload bytes read are counted: for the Job, the Source Set's Root and in total.
    pub tally: meter::Tally,
    /// The files of the Source Set's Placed Games, read as its Entries from the Library Folders that hold them.
    pub placed: &'a [Placed<'a>],
}

/// The recorded files of a Source Set's Placed Games in one Library Folder (ADR 0023). Only these are read of it,
/// never another file there; a file that is gone is no Entry, so its Game is Missing or Incomplete.
pub struct Placed<'a> {
    /// The Library Folder's id, which names each Entry's path ([`title_select_api::storage::placed_path`]).
    pub id: i64,
    /// The canonical path of the folder's Root. Everything below it, the folder itself included, is opened without
    /// following a symbolic link.
    pub root: &'a Path,
    /// The Library Folder, relative to its Root, with `/` separators.
    pub folder: &'a str,
    /// Each file, relative to the folder, with `/` separators.
    pub files: Vec<String>,
    /// The pools of the folder's Root, as [`Options::pools`]; the Host's hash workers are the Source Set's.
    pub pools: Option<Pools<'a>>,
    /// Where the payload bytes read of it are counted: the Library Folder's Root's.
    pub tally: meter::Tally,
    /// Those of `files` that are CHDs, placed under Place Converted Games, and how to run chdman to read them.
    pub chds: Vec<PlacedChd>,
    pub chd: ChdAudit,
}

/// A Placed Game's file that is a CHD (ADR 0023): a full Audit extracts it with chdman and reads each track it gives
/// back as a payload of its own, named for the ROM it is to be; a quick inventory takes its header's SHA-1, while it is
/// the one it held when it was placed, as Claim Evidence of each. A CD's cue sheet, which a CHD doesn't hold, is read
/// from where it is kept, in the store, in either.
#[derive(Debug, Clone)]
pub struct PlacedChd {
    /// Relative to the Library Folder, with `/` separators, as in [`Placed::files`].
    pub rel: String,
    pub game: String,
    /// The SHA-1 its header held when extracting it gave back every track of its disc exactly.
    pub sha1: String,
    /// Its disc's ROMs as the DAT listed them when it was placed: a CD's cue sheet and tracks, or a DVD's image.
    pub roms: Vec<title_select_api::build::ExpectedRom>,
    /// A CD's cue sheet as kept in the store, or why it can't be read.
    pub cue: Option<Result<Vec<u8>, String>>,
}

/// How an Audit runs chdman to read a placed CHD: chdman, or why it can't be run; how long each run may take; and the
/// scratch directory it extracts into, a folder of its own for each CHD, removed once read.
#[derive(Debug, Clone)]
pub struct ChdAudit {
    pub chdman: Result<chd::Chdman, String>,
    pub limits: chd::Limits,
    pub tmp: std::path::PathBuf,
}

/// A file as `stat` sees it: what the change test compares. An earlier read of a file is reused only while every
/// field is as it was when the file was opened for that read. This is a heuristic: a file rewritten in place, with
/// its size kept and its times put back, would pass it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// Modification time, in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// Status change time, in nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
}

impl FileStamp {
    /// The stamp of the file at `path`, following symlinks; none where the platform has no inode or status change
    /// time, so that nothing is reused there.
    pub fn of(path: &Path) -> Option<FileStamp> {
        std::fs::metadata(path)
            .ok()
            .and_then(|m| FileStamp::from_metadata(&m))
    }

    #[cfg(unix)]
    pub fn from_metadata(m: &std::fs::Metadata) -> Option<FileStamp> {
        use std::os::unix::fs::MetadataExt;
        let ns = |s: i64, n: i64| s as i128 * 1_000_000_000 + n as i128;
        Some(FileStamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime_ns: ns(m.mtime(), m.mtime_nsec()),
            ctime_ns: ns(m.ctime(), m.ctime_nsec()),
        })
    }

    #[cfg(not(unix))]
    pub fn from_metadata(_: &std::fs::Metadata) -> Option<FileStamp> {
        None
    }
}

/// Every payload of a file, read in full from the file as `stamp` describes it, starting at `read_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRead {
    pub stamp: FileStamp,
    pub read_at: jiff::Timestamp,
    /// Each payload's member name in a ZIP archive, or none for a loose file, in archive order.
    pub members: Vec<(Option<String>, Checksums)>,
    /// The Header Rule the payloads were hashed under. A read is reused only under the same rule.
    pub header_rule: Option<HeaderRule>,
}

/// Earlier payload reads an Audit may reuse, and where it records its own. Both are called from the reader
/// threads, once per file.
pub trait PayloadReads: Sync {
    /// The last recorded read of the file at `path`, relative to the Source Set, if this Audit may reuse it. It is
    /// reused only if the file's stamp is still the one it records.
    fn prior(&self, path: &str) -> Option<PayloadRead>;
    /// Records a read this Audit made of every payload of the file at `path`. A file with any payload that could
    /// not be read is not recorded.
    fn record(&self, path: &str, read: &PayloadRead);
    /// Records the stamp the file at `path` had when this Audit could not read one of its payloads, so that what is
    /// done about it later can tell whether it changed since.
    fn record_unreadable(&self, _path: &str, _stamp: &FileStamp) {}
    /// Notes that this Audit reused the read `prior` returned for the file at `path`, whose stamp is still `stamp`.
    fn reused(&self, _path: &str, _stamp: &FileStamp) {}
    /// Notes the stamp the ZIP archive at `path` had when this Audit opened it to claim its members.
    fn claimed(&self, _path: &str, _stamp: &FileStamp) {}
}

/// Whether `name`, an entry anywhere in a Source Set, is the `.title-select` directory Title Select keeps there for
/// itself, which an Audit passes over with all it holds: the marker, kept files, and any file being written under a
/// temporary name.
pub fn is_own_entry(name: &str) -> bool {
    title_select_api::storage::is_reserved_name(name)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Audits the Source Set at `root` against the DAT Revision at `dat_path`, reading every payload.
pub fn audit(dat_path: &Path, root: &Path) -> Result<Report, Error> {
    let file = dat_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    audit_with(
        dat_path,
        root,
        &Options {
            file,
            confine: None,
            workers: ONE_WORKER_EACH,
            pools: None,
            progress: None,
            evidence: AuditEvidence::Full,
            reads: None,
            tally: Default::default(),
            placed: &[],
        },
    )
}

/// What the walk found, in report order.
enum Item {
    Unreadable { rel: String, reason: String },
    File { rel: String },
}

/// What a walk of a Source Set found: its items in report order, and the files among them to read.
struct Walked {
    items: Vec<Item>,
    files: Vec<ToRead>,
}

/// Walks the Source Set at `root` as an Audit does, passing over Title Select's own entries and confined as
/// `confine` says. Stops, returning none, once `keep_going` says no.
fn walk(
    root: &Path,
    confine: Option<&Path>,
    keep_going: &dyn Fn() -> bool,
) -> Result<Option<Walked>, Error> {
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut items = Vec::new();
    let mut files: Vec<ToRead> = Vec::new();
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(true)
        .sort_by_file_name()
        .into_iter();
    while let Some(item) = walk.next() {
        if !keep_going() {
            return Ok(None);
        }
        let item = match item {
            Ok(item) => item,
            Err(err) if err.depth() == 0 => return Err(Error::SourceSet(err.to_string())),
            Err(err) => {
                let path = err.path().map(rel).unwrap_or_default();
                items.push(Item::Unreadable {
                    rel: path,
                    reason: err.to_string(),
                });
                continue;
            }
        };
        if item.depth() > 0 && is_own_entry(&item.file_name().to_string_lossy()) {
            if item.file_type().is_dir() {
                walk.skip_current_dir();
            }
            continue;
        }
        let mut path = item.path().to_path_buf();
        if let Some(confine) = confine {
            let refusal = match item.path().canonicalize() {
                Ok(real) if real.starts_with(confine) => {
                    path = real;
                    None
                }
                Ok(_) => Some("resolves outside the Root".to_owned()),
                Err(err) => Some(err.to_string()),
            };
            if let Some(reason) = refusal {
                if item.depth() == 0 {
                    return Err(Error::SourceSet(format!("{}: {reason}", root.display())));
                }
                if item.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                items.push(Item::Unreadable {
                    rel: rel(item.path()),
                    reason,
                });
                continue;
            }
        }
        if !item.file_type().is_file() {
            continue;
        }
        let is_zip = item
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        let rel = rel(item.path());
        items.push(Item::File { rel: rel.clone() });
        files.push(ToRead {
            path,
            is_zip,
            rel,
            beneath: None,
            chd: None,
        });
    }

    Ok(Some(Walked { items, files }))
}

/// What an Audit reads of the Placed Games in one Library Folder: each recorded file still there, in path order,
/// named by its path in the Source Set's report, with its stamp. One that is gone is left out; one that can't be
/// reached, is no longer a regular file or is a symbolic link is Unreadable. No symbolic link below the folder's Root
/// is followed: a folder that is one, or is in one, reads as Unreadable, never as the folder it leads to.
fn walk_placed(placed: &Placed) -> (Walked, Vec<Option<FileStamp>>) {
    let mut recorded: Vec<&String> = placed.files.iter().collect();
    recorded.sort();
    let gone = |e: &std::io::Error| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    };
    let base = placed.root;
    let mut items = Vec::new();
    let mut files = Vec::new();
    let mut stamps = Vec::new();
    for rel in recorded {
        let at = title_select_api::storage::placed_path(placed.id, rel);
        let path = placed
            .folder
            .split('/')
            .chain(rel.split('/'))
            .filter(|part| !part.is_empty())
            .fold(base.to_path_buf(), |path, part| path.join(part));
        let found = match read::open_beneath(base, &path) {
            Ok(file) => Ok((path, base, file.metadata().ok())),
            Err(e) if gone(&e) => continue,
            Err(e) => Err(e.to_string()),
        };
        match found {
            Err(reason) => items.push(Item::Unreadable { rel: at, reason }),
            Ok((path, base, metadata)) => {
                let is_zip = rel
                    .rsplit_once('.')
                    .is_some_and(|(_, e)| e.eq_ignore_ascii_case("zip"));
                let chd = placed.chds.iter().find(|c| c.rel == *rel).map(|c| {
                    Box::new(read::ChdRead {
                        placed: c.clone(),
                        audit: placed.chd.clone(),
                    })
                });
                items.push(Item::File { rel: at.clone() });
                files.push(ToRead {
                    path,
                    is_zip: is_zip && chd.is_none(),
                    rel: at,
                    beneath: Some(base.to_path_buf()),
                    chd,
                });
                stamps.push(metadata.and_then(|m| FileStamp::from_metadata(&m)));
            }
        }
    }
    (Walked { items, files }, stamps)
}

/// The recorded files of the Placed Games in `placed` that are still there, each statted as [`stat_walk`] does and
/// named by its path in the Source Set's report.
pub fn stat_placed(placed: &Placed) -> Vec<StatEntry> {
    let (Walked { items, .. }, stamps) = walk_placed(placed);
    let mut stamps = stamps.into_iter();
    items
        .into_iter()
        .map(|item| match item {
            Item::Unreadable { rel, .. } => StatEntry::Unreadable { rel },
            Item::File { rel } => StatEntry::File {
                rel,
                stamp: stamps.next().expect("one stamp per file"),
            },
        })
        .collect()
}

/// A file a stat walk found, with its stamp; none where it could not be statted or the platform has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatEntry {
    File {
        rel: String,
        stamp: Option<FileStamp>,
    },
    /// A path the walk could not read, or that resolves outside the Root.
    Unreadable { rel: String },
}

/// Walks the Source Set at `root` as an Audit does, stats every file it finds and reads no payload. Stops, returning
/// none, once `keep_going` says no; it is asked before each entry and each stat.
pub fn stat_walk(
    root: &Path,
    confine: Option<&Path>,
    keep_going: &dyn Fn() -> bool,
) -> Result<Option<Vec<StatEntry>>, Error> {
    if !root.is_dir() {
        return Err(Error::SourceSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let Some(Walked { items, files }) = walk(root, confine, keep_going)? else {
        return Ok(None);
    };
    let mut files = files.into_iter();
    let mut found = Vec::with_capacity(items.len());
    for item in items {
        if !keep_going() {
            return Ok(None);
        }
        found.push(match item {
            Item::Unreadable { rel, .. } => StatEntry::Unreadable { rel },
            Item::File { rel } => {
                let file = files.next().expect("one file per item");
                StatEntry::File {
                    rel,
                    stamp: FileStamp::of(&file.path),
                }
            }
        });
    }
    Ok(Some(found))
}

/// Like [`audit`], with the DAT Revision's filename, confinement, worker counts and progress given by `options`.
pub fn audit_with(dat_path: &Path, root: &Path, options: &Options) -> Result<Report, Error> {
    let started_at = jiff::Timestamp::now();
    if !root.is_dir() {
        return Err(Error::SourceSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let dat = Dat::read(dat_path)?;
    let Walked { items, files } =
        walk(root, options.confine, &|| true)?.expect("an audit's walk is never stopped");
    let placed: Vec<Walked> = options.placed.iter().map(|p| walk_placed(p).0).collect();

    let header = dat.header();
    let sizes: HashSet<u64> = match header {
        Some(_) => dat.roms.iter().map(|r| r.size).collect(),
        None => HashSet::new(),
    };
    // A member whose size less the Header is a ROM's size is read, even when a ROM's size and CRC32 equal its raw
    // ones: a CRC32 match proves nothing of whether it holds a Header.
    let might_be_headered =
        |size: u64| header.is_some_and(|h| size > h.size() && sizes.contains(&(size - h.size())));
    let how = read::How {
        evidence: options.evidence,
        reads: options.reads,
        header,
        might_be_headered: &might_be_headered,
        tally: &options.tally,
    };
    // Progress counts every file read, the Source Set's own and its Placed Games' in each Library Folder.
    let total = files.len() + placed.iter().map(|w| w.files.len()).sum::<usize>();
    let progress_from = |before: usize| {
        move |p: Progress| {
            if let Some(progress) = options.progress {
                progress(Progress {
                    files_done: before + p.files_done,
                    files_total: total,
                });
            }
        }
    };
    let own = progress_from(0);
    let reads = read::read_files(
        &files,
        how,
        options.workers,
        options.pools,
        options.progress.map(|_| &own as &(dyn Fn(Progress) + Sync)),
    )
    .map_err(Error::Workers)?;
    let mut segments = vec![(items, reads.into_iter())];
    let mut before = files.len();
    for (walked, from) in placed.into_iter().zip(options.placed) {
        let progress = progress_from(before);
        let reads = read::read_files(
            &walked.files,
            read::How {
                tally: &from.tally,
                ..how
            },
            options.workers,
            from.pools,
            options
                .progress
                .map(|_| &progress as &(dyn Fn(Progress) + Sync)),
        )
        .map_err(Error::Workers)?;
        before += walked.files.len();
        segments.push((walked.items, reads.into_iter()));
    }
    let game_index: HashMap<&str, usize> = dat
        .games
        .iter()
        .enumerate()
        .map(|(i, g)| (g.as_str(), i))
        .collect();
    let parents = dat
        .cloneof
        .iter()
        .map(|p| p.as_deref().and_then(|p| game_index.get(p).copied()))
        .collect();
    let mut scan = Scan {
        game_index,
        parents,
        covered: vec![None; dat.roms.len()],
        elsewhere: vec![BTreeSet::new(); dat.roms.len()],
        header_missing: vec![false; dat.games.len()],
        entries: Vec::new(),
        dat: &dat,
    };
    for (items, mut reads) in segments {
        for item in items {
            match item {
                Item::Unreadable { rel, reason } => scan.unreadable(rel, None, reason),
                Item::File { rel } => {
                    let FileRead {
                        members,
                        error,
                        origin,
                    } = reads.next().expect("one read per file");
                    for (member, known) in members {
                        match known {
                            Ok(known) => scan.record(rel.clone(), member, &known, origin),
                            Err(reason) => scan.unreadable(rel.clone(), member, reason),
                        }
                    }
                    if let Some(reason) = error {
                        scan.unreadable(rel, None, reason);
                    }
                }
            }
        }
    }
    let games = scan.games();
    let mut findings: BTreeMap<_, _> = Finding::ALL.iter().map(|f| (*f, 0)).collect();
    for e in &scan.entries {
        *findings.entry(e.finding).or_default() += 1;
    }
    let mut statuses: BTreeMap<_, _> = GameStatus::ALL.iter().map(|g| (*g, 0)).collect();
    let mut warnings = BTreeMap::new();
    for g in &games {
        *statuses.entry(g.status).or_default() += 1;
        for w in &g.warnings {
            *warnings.entry(*w).or_default() += 1;
        }
    }
    Ok(Report {
        schema: SCHEMA,
        dat_revision: DatRevision {
            sha256: dat.sha256.clone(),
            name: dat.name.clone(),
            version: dat.version.clone(),
            file: options.file.clone(),
            header_rule: dat.header_rule.as_deref().map(NamedHeaderRule::new),
        },
        source_set: root.display().to_string(),
        evidence: options.evidence,
        started_at,
        finished_at: jiff::Timestamp::now(),
        summary: Summary {
            findings,
            games: statuses,
            warnings,
        },
        entries: scan.entries,
        games,
    })
}

/// What an Audit has found so far.
struct Scan<'d> {
    dat: &'d Dat,
    /// Each Game's index, by its name.
    game_index: HashMap<&'d str, usize>,
    /// The parent each Game is a clone of, by the Game's index.
    parents: Vec<Option<usize>>,
    /// The strongest Evidence Level any Entry holding each ROM for its own Game has.
    covered: Vec<Option<EvidenceLevel>>,
    /// For each ROM, the other Games whose ZIP or folder holds an Entry whose payload is that ROM's (ADR 0017).
    elsewhere: Vec<BTreeSet<usize>>,
    /// Each Game a Matched Entry without the Header its emulators need counts towards.
    header_missing: Vec<bool>,
    entries: Vec<Entry>,
}

impl Scan<'_> {
    fn unreadable(&mut self, path: String, member: Option<String>, reason: String) {
        self.entries.push(Entry {
            path,
            member,
            finding: Finding::Unreadable,
            roms: Vec::new(),
            named_rom: None,
            evidence: None,
            reason: Some(reason),
        });
    }

    fn record(&mut self, path: String, member: Option<String>, known: &Known, origin: Origin) {
        let full = member.as_deref().unwrap_or(&path);
        let name = full.rsplit(['/', '\\']).next().unwrap_or(full);
        let (finding, payload, named) = classify(self.dat, name, known);
        let container = self.container(&path, member.as_deref());
        // An Entry inside another Game's ZIP or folder makes a ROM Available Elsewhere, never present (ADR 0017). A
        // clone's parent's is the clone's own: merged and split sets keep a clone's files there.
        let elsewhere = |r: usize| {
            let game = self.dat.roms[r].game;
            container.filter(|g| *g != game && Some(*g) != self.parents[game])
        };
        let own: Vec<usize> = payload
            .iter()
            .copied()
            .filter(|r| elsewhere(*r).is_none())
            .collect();
        // Every ROM whose checksums the Entry's payload has, not only those it counts towards, so a track named for
        // its own Game still shows where another Game's copy is. Repair cannot yet source from those: it fills a
        // Game only from Entries that count towards it.
        let others: Vec<(usize, usize)> = matching_roms(self.dat, known)
            .into_iter()
            .filter_map(|r| elsewhere(r).map(|g| (r, g)))
            .collect();
        for (r, g) in others {
            self.elsewhere[r].insert(g);
        }
        let (level, header) = match known {
            Known::Payload(sums) => (EvidenceLevel::Payload, sums.header),
            Known::Claim { .. } => (EvidenceLevel::Claim, None),
        };
        if finding == Finding::Matched
            && header.is_none()
            && self.dat.header().is_some_and(HeaderRule::needed_to_play)
        {
            for r in &own {
                self.header_missing[self.dat.roms[*r].game] = true;
            }
        }
        for r in &own {
            if self.covered[*r] != Some(EvidenceLevel::Payload) {
                self.covered[*r] = Some(level);
            }
        }
        self.entries.push(Entry {
            path,
            member,
            finding,
            evidence: Some(Evidence {
                header: header.map(|bytes| Header { bytes }),
                ..evidence(self.dat, &payload, origin)
            }),
            roms: payload.iter().map(|r| rom_ref(self.dat, *r)).collect(),
            named_rom: named.map(|r| rom_ref(self.dat, r)),
            reason: None,
        });
    }

    /// The Game whose ZIP or folder holds the Entry at `path` (relative to the Source Set's root, or a Placed Game's
    /// in its Library Folder), `member` of it in
    /// a ZIP: the nearest of the ZIP, by its name less `.zip`, and the folders above it or the loose file that is
    /// named for a Game. None when none is.
    fn container(&self, path: &str, member: Option<&str>) -> Option<usize> {
        // A Placed Game's file is held as it is in its Library Folder.
        let path = title_select_api::storage::split_placed(path).map_or(path, |(_, rel)| rel);
        let mut parts = path.split(['/', '\\']).rev();
        let file = parts.next()?;
        let zip = member.and_then(|_| {
            let (stem, ext) = file.rsplit_once('.')?;
            ext.eq_ignore_ascii_case("zip").then_some(stem)
        });
        zip.into_iter()
            .chain(parts)
            .find_map(|name| self.game_index.get(name).copied())
    }

    fn games(&self) -> Vec<Game> {
        let mut roms_of: Vec<Vec<usize>> = vec![Vec::new(); self.dat.games.len()];
        for (r, rom) in self.dat.roms.iter().enumerate() {
            roms_of[rom.game].push(r);
        }
        self.dat
            .games
            .iter()
            .zip(roms_of)
            .zip(&self.header_missing)
            .map(|((name, roms), header_missing)| {
                let missing_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r].is_none())
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let available_elsewhere: Vec<AvailableElsewhere> = roms
                    .iter()
                    .filter(|r| self.covered[**r].is_none() && !self.elsewhere[**r].is_empty())
                    .map(|r| AvailableElsewhere {
                        rom: self.dat.roms[*r].name.clone(),
                        games: self.elsewhere[*r]
                            .iter()
                            .map(|g| self.dat.games[*g].clone())
                            .collect(),
                    })
                    .collect();
                let claimed_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r] == Some(EvidenceLevel::Claim))
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let status = match missing_roms.len() {
                    _ if !roms.iter().any(|r| self.dat.roms[*r].needed()) => {
                        GameStatus::NothingToCheck
                    }
                    0 => GameStatus::Complete,
                    n if n == roms.len() => GameStatus::Missing,
                    _ => GameStatus::Incomplete,
                };
                Game {
                    name: name.clone(),
                    status,
                    missing_roms,
                    claimed_roms,
                    warnings: header_missing
                        .then_some(GameWarning::HeaderMissing)
                        .into_iter()
                        .collect(),
                    available_elsewhere,
                }
            })
            .collect()
    }
}

/// For a payload match, size plus each checksum that every matched ROM supplies, since every supplied
/// checksum was compared and agreed. Without one, only the size-and-CRC32 lookup that found no candidate. A claim
/// compares only size and CRC32.
fn evidence(dat: &Dat, roms: &[usize], origin: Origin) -> Evidence {
    let (level, read_at, reused) = match origin {
        Origin::Read(at) => (EvidenceLevel::Payload, Some(at), None),
        Origin::Reused(at) => (
            EvidenceLevel::Payload,
            Some(at),
            Some(Reuse::UnchangedFileHeuristic),
        ),
        Origin::Claimed => (EvidenceLevel::Claim, None, None),
    };
    let algorithms = if roms.is_empty() || level == EvidenceLevel::Claim {
        vec!["size".into(), "crc32".into()]
    } else {
        let all_supply = |has: fn(&dat::Rom) -> bool| roms.iter().all(|r| has(&dat.roms[*r]));
        let mut algorithms = vec!["size".into()];
        for (name, has) in [
            ("crc32", (|r| r.crc.is_some()) as fn(&dat::Rom) -> bool),
            ("md5", |r| r.md5.is_some()),
            ("sha1", |r| r.sha1.is_some()),
            ("sha256", |r| r.sha256.is_some()),
        ] {
            if all_supply(has) {
                algorithms.push(name.into());
            }
        }
        algorithms
    };
    Evidence {
        level,
        algorithms,
        read_at,
        reused,
        header: None,
    }
}

fn rom_ref(dat: &Dat, r: usize) -> RomRef {
    RomRef {
        game: dat.games[dat.roms[r].game].clone(),
        name: dat.roms[r].name.clone(),
    }
}

/// Decides an Entry's Finding by payload before name (ADR 0002). Returns the Finding, the ROMs the payload
/// matches, and the ROM the name gives when that differs from the payload's.
fn classify(dat: &Dat, name: &str, known: &Known) -> (Finding, Vec<usize>, Option<usize>) {
    let payload = matching_roms(dat, known);
    let named = dat.roms_named(name);
    // Every ROM with this name and payload, so an identical ROM shared by several Games counts for each.
    let both: Vec<usize> = named
        .iter()
        .copied()
        .filter(|r| payload.contains(r))
        .collect();
    if !both.is_empty() {
        return (Finding::Matched, both, None);
    }
    match (payload.is_empty(), named.first()) {
        (false, None) => (Finding::Misnamed, payload, None),
        (false, Some(n)) => (Finding::Conflicting, payload, Some(*n)),
        (true, Some(n)) => (Finding::Mismatched, payload, Some(*n)),
        (true, None) => (Finding::Unmatched, payload, None),
    }
}

/// The ROMs whose every supplied checksum equals the payload's; for a claim, the ROMs whose size and CRC32 equal
/// the claimed ones.
fn matching_roms(dat: &Dat, known: &Known) -> Vec<usize> {
    match known {
        Known::Payload(sums) => dat
            .candidates(sums.size, &sums.crc)
            .filter(|r| sums.matches(&dat.roms[*r]))
            .collect(),
        Known::Claim { size, crc } => dat
            .candidates(*size, crc)
            .filter(|r| dat.roms[*r].crc.as_deref() == Some(crc.as_str()))
            .collect(),
    }
}
