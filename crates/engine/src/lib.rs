//! Audits a Source Set against a DAT Revision (see GLOSSARY.md and ADR 0002).

pub mod build;
pub mod dat;
pub mod payload;
mod read;
pub mod torrentzip;
pub mod zipdir;

use std::collections::BTreeMap;
use std::path::Path;

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
    /// At least one of each: `readers` files are read at once, and `hashers` threads checksum their payloads.
    pub workers: Workers,
    /// Called as each file's Entries are read, from the reader threads.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    /// Whether ZIP archives are claimed from their directories ([`AuditEvidence::Quick`]), as the report records.
    /// Which earlier reads may be reused is up to `reads`.
    pub evidence: AuditEvidence,
    /// Earlier payload reads to reuse under the change test, and where to record this Audit's reads.
    pub reads: Option<&'a dyn PayloadReads>,
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
}

/// Whether `name`, an entry anywhere in a Source Set, is one Title Select keeps there for itself and an Audit passes
/// over: its marker, its kept files and private `.deleting` folders, and a file it is writing under a temporary
/// name.
pub fn is_own_entry(name: &str) -> bool {
    name.starts_with(".title-select")
        || name == ".deleting"
        || (name.starts_with('.') && name.contains(".title-select-new-"))
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
            progress: None,
            evidence: AuditEvidence::Full,
            reads: None,
        },
    )
}

/// What the walk found, in report order.
enum Item {
    Unreadable { rel: String, reason: String },
    File { rel: String },
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
        if let Some(confine) = options.confine {
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
        files.push(ToRead { path, is_zip, rel });
    }

    let how = read::How {
        evidence: options.evidence,
        reads: options.reads,
    };
    let mut reads = read::read_files(&files, how, options.workers, options.progress).into_iter();
    let mut scan = Scan {
        covered: vec![None; dat.roms.len()],
        entries: Vec::new(),
        dat: &dat,
    };
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
    let games = scan.games();
    let mut findings: BTreeMap<_, _> = Finding::ALL.iter().map(|f| (*f, 0)).collect();
    for e in &scan.entries {
        *findings.entry(e.finding).or_default() += 1;
    }
    let mut statuses: BTreeMap<_, _> = GameStatus::ALL.iter().map(|g| (*g, 0)).collect();
    for g in &games {
        *statuses.entry(g.status).or_default() += 1;
    }
    Ok(Report {
        schema: SCHEMA,
        dat_revision: DatRevision {
            sha256: dat.sha256.clone(),
            name: dat.name.clone(),
            version: dat.version.clone(),
            file: options.file.clone(),
        },
        source_set: root.display().to_string(),
        evidence: options.evidence,
        started_at,
        finished_at: jiff::Timestamp::now(),
        summary: Summary {
            findings,
            games: statuses,
        },
        entries: scan.entries,
        games,
    })
}

/// What an Audit has found so far.
struct Scan<'d> {
    dat: &'d Dat,
    /// The strongest Evidence Level any Entry holding each ROM has.
    covered: Vec<Option<EvidenceLevel>>,
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
        let level = match known {
            Known::Payload(_) => EvidenceLevel::Payload,
            Known::Claim { .. } => EvidenceLevel::Claim,
        };
        for r in &payload {
            if self.covered[*r] != Some(EvidenceLevel::Payload) {
                self.covered[*r] = Some(level);
            }
        }
        self.entries.push(Entry {
            path,
            member,
            finding,
            evidence: Some(evidence(self.dat, &payload, origin)),
            roms: payload.iter().map(|r| rom_ref(self.dat, *r)).collect(),
            named_rom: named.map(|r| rom_ref(self.dat, r)),
            reason: None,
        });
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
            .map(|(name, roms)| {
                let missing_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r].is_none())
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let claimed_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r] == Some(EvidenceLevel::Claim))
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let status = match missing_roms.len() {
                    0 => GameStatus::Complete,
                    n if n == roms.len() => GameStatus::Missing,
                    _ => GameStatus::Incomplete,
                };
                Game {
                    name: name.clone(),
                    status,
                    missing_roms,
                    claimed_roms,
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
