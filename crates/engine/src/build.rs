//! Writing a Build's files: copying a source archive as-is or repacking verified payloads as TorrentZip, and checking
//! what was written before it is renamed into place.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use title_select_api::build::ExpectedRom;

use crate::dat::Dat;
use crate::payload::{Checksums, Hasher};
use crate::torrentzip::{self, Writer, compare_names};
use crate::zipdir::{self, DirectoryLimits, Gated};

const CHUNK: usize = 1 << 16;

/// Where one ROM's payload is read from.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub path: &'a Path,
    /// The member of the ZIP archive at `path`, or `None` for a plain file.
    pub member: Option<&'a str>,
}

/// Whether `checksums` are the payload `rom` expects: its size and every checksum it lists.
pub fn matches(checksums: &Checksums, rom: &ExpectedRom) -> bool {
    let same = |want: &Option<String>, got: &str| {
        want.as_deref()
            .is_none_or(|want| want.eq_ignore_ascii_case(got))
    };
    checksums.size == rom.size
        && same(&rom.crc, &checksums.crc)
        && same(&rom.md5, &checksums.md5)
        && same(&rom.sha1, &checksums.sha1)
        && same(&rom.sha256, &checksums.sha256)
}

/// Opens `file` as a ZIP archive through what `reader` makes, only once it is judged one within the default bounds.
fn archive<R: Read + Seek>(
    file: &File,
    reader: impl FnOnce() -> io::Result<R>,
) -> Result<zip::ZipArchive<R>, String> {
    match zipdir::open(file, &DirectoryLimits::default(), reader).map_err(|e| e.to_string())? {
        Gated::Archive(archive) => archive.map_err(|e| e.to_string()),
        Gated::Refused(why) => Err(why),
        Gated::NotZip => Err("it is not a ZIP archive".into()),
    }
}

/// The `.cue` sheets in `file`, a cue pack: a ZIP archive of them, by member name, each read and hashed. Other
/// members are passed over. A sheet larger than `limit` bytes, or a pack holding none, is refused.
pub fn cue_sheets(file: &File, limit: u64) -> Result<Vec<(String, Checksums)>, String> {
    let mut archive = archive(file, || Ok(BufReader::new(file)))?;
    let mut sheets = Vec::new();
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = member.name().map_err(|e| e.to_string())?.into_owned();
        let cue = name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("cue"));
        if !member.is_file() || !cue {
            continue;
        }
        let mut hasher = Hasher::new();
        let mut buf = vec![0; CHUNK];
        let mut read = 0u64;
        loop {
            let n = member.read(&mut buf).map_err(|e| format!("{name}: {e}"))?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > limit {
                return Err(format!("{name}: a cue sheet is at most {limit} bytes"));
            }
            hasher.update(&buf[..n]);
        }
        sheets.push((name, hasher.finish()));
    }
    if sheets.is_empty() {
        return Err("it holds no .cue sheet".into());
    }
    Ok(sheets)
}

/// Whether the archive at `path` is structurally valid TorrentZip, read from its central directory alone.
pub fn is_torrentzip(path: &Path) -> bool {
    File::open(path)
        .map_err(torrentzip::Error::from)
        .and_then(|f| torrentzip::central(&mut BufReader::new(f)))
        .is_ok()
}

/// Copies `source` into `dest`, a new, empty file the caller created. On Linux the standard library uses
/// `copy_file_range`, so that the filesystem may share or offload the copy; nowhere is a hardlink made.
pub fn copy(source: &Path, dest: &mut File) -> io::Result<u64> {
    let mut from = File::open(source)?;
    let copied = io::copy(&mut from, dest)?;
    dest.sync_all()?;
    Ok(copied)
}

/// Writes the TorrentZip of `roms` into `dest`, a new, empty file the caller created, each ROM's payload read from
/// its source. A payload of the wrong size is refused; checksums are checked by [`verify`] afterwards.
pub fn repack(dest: &File, roms: &[(&ExpectedRom, Source)]) -> Result<(), String> {
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let mut writer = Writer::new(BufWriter::new(dest)).map_err(|e| e.to_string())?;
    for (rom, source) in order {
        let opened =
            File::open(source.path).map_err(|e| format!("{}: {e}", source.path.display()))?;
        let mut reader = BufReader::new(&opened);
        let added = match source.member {
            None => writer.add(&rom.name, rom.size, &mut reader),
            Some(member) => {
                let mut archive = archive(&opened, || Ok(reader))
                    .map_err(|e| format!("{}: {e}", source.path.display()))?;
                let mut payload = archive
                    .by_name(member)
                    .map_err(|e| format!("{} ({member}): {e}", source.path.display()))?;
                writer.add(&rom.name, rom.size, &mut payload)
            }
        };
        added.map_err(|e| format!("{} ({}): {e}", source.path.display(), rom.name))?;
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

/// A reader of a file that hashes it whole as it is read: each byte read in order from the start of the file is
/// hashed the first time it is read, and [`Hashing::finish`] reads only what was never read in order. A ZIP reader
/// that reads its central directory and then its members in order thus reads the file about once.
struct Hashing<'a> {
    file: &'a File,
    pos: u64,
    hashed: u64,
    sha256: Sha256,
}

impl<'a> Hashing<'a> {
    fn new(file: &'a File) -> io::Result<Self> {
        let mut file_ref = file;
        file_ref.seek(SeekFrom::Start(0))?;
        Ok(Hashing {
            file,
            pos: 0,
            hashed: 0,
            sha256: Sha256::new(),
        })
    }

    /// The file's SHA-256, reading what was not read in order, and its size.
    fn finish(mut self) -> io::Result<(String, u64)> {
        self.seek(SeekFrom::Start(self.hashed))?;
        let mut buf = vec![0; CHUNK];
        while self.read(&mut buf)? > 0 {}
        Ok((crate::hex(&self.sha256.finalize()), self.hashed))
    }
}

impl Read for Hashing<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut file = self.file;
        let n = file.read(buf)?;
        let end = self.pos + n as u64;
        if self.pos <= self.hashed && end > self.hashed {
            self.sha256
                .update(&buf[(self.hashed - self.pos) as usize..n]);
            self.hashed = end;
        }
        self.pos = end;
        Ok(n)
    }
}

impl Seek for Hashing<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let mut file = self.file;
        self.pos = file.seek(to)?;
        Ok(self.pos)
    }
}

/// Writes `roms` to `dest` as a TorrentZip, each read from the member of the same name in the ZIP `source`, an open
/// file read from its start. Returns the SHA-256 and size of `source`, hashed as it was read.
pub fn repack_from(
    dest: &File,
    roms: &[ExpectedRom],
    source: &File,
) -> Result<(String, u64), String> {
    let named: Vec<(&ExpectedRom, &str)> = roms.iter().map(|r| (r, r.name.as_str())).collect();
    repack_members(dest, &named, source)
}

/// As [`repack_from`], each ROM read from the named member of `source`, which may be named otherwise; members not
/// named are left out.
pub fn repack_members(
    dest: &File,
    roms: &[(&ExpectedRom, &str)],
    source: &File,
) -> Result<(String, u64), String> {
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let mut archive = archive(source, || Ok(BufReader::new(Hashing::new(source)?)))?;
    let mut writer = Writer::new(BufWriter::new(dest)).map_err(|e| e.to_string())?;
    for (rom, member) in order {
        let mut payload = archive
            .by_name(member)
            .map_err(|e| format!("{member}: {e}"))?;
        writer
            .add(&rom.name, rom.size, &mut payload)
            .map_err(|e| format!("{}: {e}", rom.name))?;
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    archive
        .into_inner()
        .into_inner()
        .finish()
        .map_err(|e| e.to_string())
}

/// One ROM of [`repack_sources`]: the ROM, the index of the source it is read from, and the member of that source,
/// a ZIP archive, it is read from; `None` reads the whole source, a loose file.
pub type FromSource<'a> = (&'a ExpectedRom, usize, Option<&'a str>);

/// Writes the TorrentZip of `roms` into `dest`, a new, empty file the caller created, each ROM read from its source
/// in `sources`, each an open file read from its start. Returns the SHA-256 and size of each source, in order, each
/// hashed as it was read; a source no ROM is read from is read only to hash it. A source is read either as a ZIP
/// archive or as a loose file, never both.
pub fn repack_sources(
    dest: &File,
    roms: &[FromSource],
    sources: &[&File],
) -> Result<Vec<(String, u64)>, String> {
    enum Reading<'a> {
        Unread,
        Zip(Box<zip::ZipArchive<BufReader<Hashing<'a>>>>),
        Loose(Hashing<'a>),
    }
    let mut reading: Vec<Reading> = sources.iter().map(|_| Reading::Unread).collect();
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let mut writer = Writer::new(BufWriter::new(dest)).map_err(|e| e.to_string())?;
    for (rom, at, member) in order {
        let source = *sources
            .get(*at)
            .ok_or_else(|| format!("{}: no source {at}", rom.name))?;
        let slot = &mut reading[*at];
        let added = match (member, &mut *slot) {
            (Some(member), Reading::Unread | Reading::Zip(_)) => {
                if matches!(slot, Reading::Unread) {
                    let opened = archive(source, || Ok(BufReader::new(Hashing::new(source)?)))?;
                    *slot = Reading::Zip(Box::new(opened));
                }
                let Reading::Zip(archive) = slot else {
                    unreachable!("just opened")
                };
                let mut payload = archive
                    .by_name(member)
                    .map_err(|e| format!("{member}: {e}"))?;
                writer.add(&rom.name, rom.size, &mut payload)
            }
            (None, Reading::Unread) => {
                let mut hashing = Hashing::new(source).map_err(|e| e.to_string())?;
                let added = writer.add(&rom.name, rom.size, &mut hashing);
                *slot = Reading::Loose(hashing);
                added
            }
            _ => {
                return Err(format!(
                    "{}: a source is read once, as a ZIP archive or as a loose file",
                    rom.name
                ));
            }
        };
        added.map_err(|e| format!("{}: {e}", rom.name))?;
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    reading
        .into_iter()
        .zip(sources)
        .map(|(reading, source)| {
            match reading {
                Reading::Zip(archive) => archive.into_inner().into_inner().finish(),
                Reading::Loose(hashing) => hashing.finish(),
                Reading::Unread => Hashing::new(source).and_then(Hashing::finish),
            }
            .map_err(|e| e.to_string())
        })
        .collect()
}

/// Checks that `file`, read from its start, is valid TorrentZip whose members are exactly `roms`, under their names,
/// and that each member's payload, decompressed and hashed again, is the one its ROM expects.
pub fn verify(file: &File, roms: &[ExpectedRom]) -> Result<(), String> {
    let mut file = BufReader::new(file);
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let members = torrentzip::validate(&mut file).map_err(|e| e.to_string())?;
    let mut names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let mut wanted: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    wanted.sort_unstable();
    if names != wanted {
        return Err(format!(
            "it holds {} where the game's ROMs are {}",
            names.join(", "),
            wanted.join(", ")
        ));
    }
    let mut archive = archive(file.get_ref(), || Ok(file))?;
    for rom in roms {
        let mut member = archive
            .by_name(&rom.name)
            .map_err(|e| format!("{}: {e}", rom.name))?;
        let mut hasher = Hasher::new();
        let mut buf = vec![0; CHUNK];
        loop {
            let n = member
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", rom.name))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if !matches(&hasher.finish(), rom) {
            return Err(format!(
                "{}: its payload does not hash to the DAT's",
                rom.name
            ));
        }
    }
    Ok(())
}

/// Writes the one ROM of `roms` into `dest`, a new, empty file the caller created, as a loose file: its payload,
/// read from its source in `sources`, a member of a ZIP archive or a whole loose file. Returns the SHA-256 and size
/// of each source, as [`repack_sources`] does. A payload of the wrong size is refused; checksums are checked by
/// [`verify_loose`] afterwards.
pub fn write_loose(
    dest: &File,
    roms: &[FromSource],
    sources: &[&File],
) -> Result<Vec<(String, u64)>, String> {
    let [(rom, at, member)] = roms else {
        return Err(format!(
            "a loose file holds one ROM, not {}; a game with more stays a ZIP",
            roms.len()
        ));
    };
    let source = *sources
        .get(*at)
        .ok_or_else(|| format!("{}: no source {at}", rom.name))?;
    let mut out = BufWriter::new(dest);
    let copied = match member {
        Some(member) => {
            let mut archive = archive(source, || Ok(BufReader::new(Hashing::new(source)?)))?;
            let copied = {
                let mut payload = archive
                    .by_name(member)
                    .map_err(|e| format!("{member}: {e}"))?;
                io::copy(&mut (&mut payload).take(rom.size + 1), &mut out)
                    .map_err(|e| format!("{}: {e}", rom.name))?
            };
            let read = archive
                .into_inner()
                .into_inner()
                .finish()
                .map_err(|e| e.to_string())?;
            (copied, read)
        }
        None => {
            let mut hashing = Hashing::new(source).map_err(|e| e.to_string())?;
            let copied = io::copy(&mut (&mut hashing).take(rom.size + 1), &mut out)
                .map_err(|e| format!("{}: {e}", rom.name))?;
            (copied, hashing.finish().map_err(|e| e.to_string())?)
        }
    };
    if copied.0 != rom.size {
        return Err(format!(
            "{}: its payload is {} bytes, not the DAT's {}",
            rom.name, copied.0, rom.size
        ));
    }
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let mut read: Vec<(String, u64)> = Vec::with_capacity(sources.len());
    for (i, other) in sources.iter().enumerate() {
        if i == *at {
            read.push(copied.1.clone());
        } else {
            read.push(
                Hashing::new(other)
                    .and_then(Hashing::finish)
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(read)
}

/// Checks that `file`, read from its start, is exactly `rom` as a loose file: its size and every checksum the DAT
/// lists.
pub fn verify_loose(file: &File, rom: &ExpectedRom) -> Result<(), String> {
    let mut reader = file;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if !matches(&hasher.finish(), rom) {
        return Err(format!("{}: it does not hash to the DAT's ROM", rom.name));
    }
    Ok(())
}

/// The name and payload checksums of the one member of `file`, read from its start, when it is valid TorrentZip,
/// opened within the default bounds, holding exactly one member. The payload is decompressed and hashed again.
pub fn sole_payload(file: &File) -> Result<(String, Checksums), String> {
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let members = torrentzip::validate(&mut reader).map_err(|e| e.to_string())?;
    let [only] = members.as_slice() else {
        return Err(format!("it holds {} members, not one", members.len()));
    };
    let mut archive = archive(file, || Ok(reader))?;
    if archive.len() != 1 {
        return Err(format!("it holds {} members, not one", archive.len()));
    }
    let mut member = archive
        .by_name(&only.name)
        .map_err(|e| format!("{}: {e}", only.name))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = member
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", only.name))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok((only.name.clone(), hasher.finish()))
}

/// One payload a file holds: a ZIP member, or the whole file when it is not a ZIP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    /// The member's name; `None` for a file that is not a ZIP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub size: u64,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

impl Payload {
    fn new(name: Option<String>, sums: Checksums) -> Payload {
        Payload {
            name,
            size: sums.size,
            crc: sums.crc,
            md5: sums.md5,
            sha1: sums.sha1,
            sha256: sums.sha256,
        }
    }

    pub fn checksums(&self) -> Checksums {
        Checksums {
            size: self.size,
            crc: self.crc.clone(),
            md5: self.md5.clone(),
            sha1: self.sha1.clone(),
            sha256: self.sha256.clone(),
        }
    }
}

/// What reading a file in a Library Folder found: the SHA-256 of the whole file, and the payloads it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub sha256: String,
    pub size: u64,
    /// Whether it reads as a ZIP archive.
    pub zip: bool,
    /// Whether it is structurally valid TorrentZip.
    pub torrentzip: bool,
    pub payloads: Vec<Payload>,
    /// Why a member could not be read; its payloads are then incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn hash_all(file: &mut impl Read) -> io::Result<Checksums> {
    let mut hasher = Hasher::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Reads `file` from its start: hashes it whole, and, when it is a ZIP archive, each member. An error reading the
/// file itself is returned; one reading a member is recorded in the probe.
/// A ZIP archive is opened only through [`zipdir::open`]: one that begins as a ZIP but is not in canonical layout is
/// refused unread, and any other file is a loose file, never opened as an archive.
pub fn probe(file: &File) -> io::Result<Probe> {
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(0))?;
    let whole = hash_all(&mut reader)?;
    reader.seek(SeekFrom::Start(0))?;
    let torrentzip = torrentzip::validate(&mut reader).is_ok();
    reader.seek(SeekFrom::Start(0))?;
    let mut probe = Probe {
        sha256: whole.sha256.clone(),
        size: whole.size,
        zip: false,
        torrentzip,
        payloads: Vec::new(),
        error: None,
    };
    let mut archive = match zipdir::open(file, &DirectoryLimits::default(), || {
        reader.seek(SeekFrom::Start(0)).map(|_| reader)
    })? {
        Gated::Archive(Ok(archive)) => archive,
        Gated::Refused(why) => {
            probe.torrentzip = false;
            probe.zip = true;
            probe.error = Some(format!("not read: {why}"));
            return Ok(probe);
        }
        Gated::NotZip | Gated::Archive(Err(_)) => {
            probe.torrentzip = false;
            probe.payloads.push(Payload::new(None, whole));
            return Ok(probe);
        }
    };
    probe.zip = true;
    for i in 0..archive.len() {
        let mut member = match archive.by_index(i) {
            Ok(member) => member,
            Err(e) => {
                probe.error = Some(e.to_string());
                break;
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                probe.error = Some(e.to_string());
                break;
            }
        };
        match hash_all(&mut member) {
            Ok(sums) => probe.payloads.push(Payload::new(Some(name), sums)),
            Err(e) => {
                probe.error = Some(format!("{name}: {e}"));
                break;
            }
        }
    }
    Ok(probe)
}

/// How much of a ZIP archive [`probe_within`] reads before it gives up on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipLimits {
    pub members: usize,
    /// The most bytes one member may expand to.
    pub member_bytes: u64,
    /// The most bytes all members together may expand to.
    pub total_bytes: u64,
}

impl Default for ZipLimits {
    fn default() -> ZipLimits {
        ZipLimits {
            members: 4096,
            member_bytes: 16 << 30,
            total_bytes: 32 << 30,
        }
    }
}

/// What [`probe_within`] made of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probed {
    Read(Probe),
    /// A ZIP archive it would not read further, and why: it holds another archive, or more than the limits allow.
    Refused(String),
}

/// Endings of member names that make a ZIP a nested archive.
pub const NESTED_ENDINGS: [&str; 12] = [
    "zip", "7z", "rar", "gz", "tgz", "tar", "bz2", "xz", "zst", "lzh", "lha", "arj",
];

/// The archive format the first bytes of a member start with, if any.
fn archive_magic(head: &[u8]) -> Option<&'static str> {
    const MAGIC: [(&[u8], &str); 6] = [
        (b"PK\x03\x04", "ZIP"),
        (b"7z\xbc\xaf\x27\x1c", "7z"),
        (b"Rar!\x1a\x07", "RAR"),
        (b"\x1f\x8b\x08", "gzip"),
        (b"\xfd7zXZ\x00", "xz"),
        (b"\x28\xb5\x2f\xfd", "zstd"),
    ];
    MAGIC
        .iter()
        .find(|(magic, _)| head.starts_with(magic))
        .map(|(_, format)| *format)
}

/// As [`probe`], but a ZIP archive is first judged from its central directory alone: refused, before any member
/// is decompressed, when it has more members than `limits` allow, declares more bytes than they allow, has members
/// that overlap, or holds another archive by name; then from each member's first bytes, refused when one starts as
/// another archive; and only then read, refused as soon as the bytes actually decompressed pass a limit, whatever
/// the archive declared.
pub fn probe_within(file: &File, limits: &ZipLimits) -> io::Result<Probed> {
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(0))?;
    let whole = hash_all(&mut reader)?;
    reader.seek(SeekFrom::Start(0))?;
    let torrentzip = torrentzip::validate(&mut reader).is_ok();
    reader.seek(SeekFrom::Start(0))?;
    let mut probe = Probe {
        sha256: whole.sha256.clone(),
        size: whole.size,
        zip: false,
        torrentzip,
        payloads: Vec::new(),
        error: None,
    };
    let directory = DirectoryLimits {
        entries: limits.members as u64,
        ..DirectoryLimits::default()
    };
    let mut archive = match zipdir::open(file, &directory, || {
        reader.seek(SeekFrom::Start(0)).map(|_| reader)
    })? {
        Gated::Archive(Ok(archive)) => archive,
        Gated::Refused(why) => return Ok(Probed::Refused(why)),
        Gated::NotZip | Gated::Archive(Err(_)) => {
            probe.torrentzip = false;
            probe.payloads.push(Payload::new(None, whole));
            return Ok(Probed::Read(probe));
        }
    };
    probe.zip = true;
    if archive.len() > limits.members {
        return Ok(Probed::Refused(format!(
            "it has {} members, more than the {} a game's ZIP is read with",
            archive.len(),
            limits.members
        )));
    }
    let mut members = Vec::new();
    let mut declared = 0u64;
    let mut spans = Vec::new();
    for i in 0..archive.len() {
        let member = match archive.by_index_raw(i) {
            Ok(member) => member,
            Err(e) => {
                probe.error = Some(e.to_string());
                return Ok(Probed::Read(probe));
            }
        };
        spans.push((member.header_start(), member.compressed_size()));
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                probe.error = Some(e.to_string());
                return Ok(Probed::Read(probe));
            }
        };
        if let Some((_, ext)) = name.rsplit_once('.')
            && NESTED_ENDINGS.contains(&ext.to_ascii_lowercase().as_str())
        {
            return Ok(Probed::Refused(format!(
                "it holds another archive, {name}; nested archives are not opened"
            )));
        }
        if member.size() > limits.member_bytes {
            return Ok(Probed::Refused(format!(
                "{name} expands to {} bytes, more than the {} one member is read to",
                member.size(),
                limits.member_bytes
            )));
        }
        declared = declared.saturating_add(member.size());
        members.push((i, name, member.size()));
    }
    if declared > limits.total_bytes {
        return Ok(Probed::Refused(format!(
            "it expands to {declared} bytes, more than the {} a game's ZIP is read to",
            limits.total_bytes
        )));
    }
    spans.sort_unstable();
    if spans
        .windows(2)
        .any(|pair| pair[0].0.saturating_add(pair[0].1) > pair[1].0)
    {
        return Ok(Probed::Refused(
            "its members overlap one another, as only a crafted archive's do".into(),
        ));
    }
    for (i, name, _) in &members {
        let mut head = Vec::with_capacity(8);
        let read = archive
            .by_index(*i)
            .and_then(|member| Ok(member.take(8).read_to_end(&mut head)?));
        if let Err(e) = read {
            probe.error = Some(format!("{name}: {e}"));
            return Ok(Probed::Read(probe));
        }
        if let Some(format) = archive_magic(&head) {
            return Ok(Probed::Refused(format!(
                "it holds another archive, {name}, which reads as {format}; nested archives are not opened"
            )));
        }
    }
    let mut left = limits.total_bytes;
    for (i, name, size) in members {
        let mut member = match archive.by_index(i) {
            Ok(member) => member,
            Err(e) => {
                probe.error = Some(e.to_string());
                break;
            }
        };
        let cap = size.min(left);
        let mut bounded = (&mut member).take(cap.saturating_add(1));
        match hash_all(&mut bounded) {
            Ok(sums) if sums.size > cap => {
                return Ok(Probed::Refused(format!(
                    "{name} expands to more than its archive says; it was not read further"
                )));
            }
            Ok(sums) => {
                left -= sums.size;
                probe.payloads.push(Payload::new(Some(name), sums));
            }
            Err(e) => {
                probe.error = Some(format!("{name}: {e}"));
                break;
            }
        }
    }
    Ok(Probed::Read(probe))
}

/// Whether `probe` is a ZIP holding exactly `roms`, under their names, each with the payload the DAT expects.
pub fn holds(probe: &Probe, roms: &[ExpectedRom]) -> bool {
    probe.zip
        && probe.error.is_none()
        && probe.payloads.len() == roms.len()
        && roms.iter().all(|rom| {
            probe
                .payloads
                .iter()
                .find(|p| p.name.as_deref() == Some(rom.name.as_str()))
                .is_some_and(|p| matches(&p.checksums(), rom))
        })
}

/// Whether `probe` holds exactly `roms` as stored: a ZIP as [`holds`] says, or, when `loose`, a loose file of the
/// one ROM of `roms` with the payload the DAT expects.
pub fn holds_as(probe: &Probe, roms: &[ExpectedRom], loose: bool) -> bool {
    if !loose {
        return holds(probe, roms);
    }
    let ([rom], [payload]) = (roms, probe.payloads.as_slice()) else {
        return false;
    };
    !probe.zip && probe.error.is_none() && matches(&payload.checksums(), rom)
}

/// The Games of `dat` whose ROMs are exactly the payloads `probe` holds, whatever they are named.
pub struct GameIndex<'a> {
    dat: &'a Dat,
    roms_of: Vec<Vec<usize>>,
}

impl<'a> GameIndex<'a> {
    pub fn new(dat: &'a Dat) -> GameIndex<'a> {
        let mut roms_of = vec![Vec::new(); dat.games.len()];
        for (i, rom) in dat.roms.iter().enumerate() {
            roms_of[rom.game].push(i);
        }
        GameIndex { dat, roms_of }
    }

    /// The first Game, in DAT order, whose every ROM is one of the payloads and that has as many ROMs as there are
    /// payloads.
    pub fn game_of(&self, probe: &Probe) -> Option<&'a str> {
        if probe.error.is_some() {
            return None;
        }
        let first = probe.payloads.first()?;
        let mut games: Vec<usize> = self
            .dat
            .candidates(first.size, &first.crc)
            .map(|rom| self.dat.roms[rom].game)
            .collect();
        games.sort_unstable();
        games.dedup();
        let sums: Vec<Checksums> = probe.payloads.iter().map(Payload::checksums).collect();
        games
            .into_iter()
            .find(|&game| {
                let roms = &self.roms_of[game];
                roms.len() == sums.len() && {
                    let mut used = vec![false; sums.len()];
                    roms.iter().all(|&rom| {
                        let found = sums
                            .iter()
                            .enumerate()
                            .find(|(i, s)| !used[*i] && s.matches(&self.dat.roms[rom]));
                        match found {
                            Some((i, _)) => {
                                used[i] = true;
                                true
                            }
                            None => false,
                        }
                    })
                }
            })
            .map(|game| self.dat.games[game].as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_read_out_of_order_is_still_hashed_whole_and_once_in_order() {
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut file = tempfile::tempfile().unwrap();
        io::Write::write_all(&mut file, &bytes).unwrap();
        let mut reader = Hashing::new(&file).unwrap();
        let mut buf = vec![0; 1000];
        // The end first, as a ZIP reader reads its central directory, then the start, then a skip ahead.
        reader.seek(SeekFrom::End(-1000)).unwrap();
        reader.read_exact(&mut buf).unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut buf).unwrap();
        reader.seek(SeekFrom::Start(5000)).unwrap();
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.hashed, 1000);
        let (sha256, size) = reader.finish().unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(sha256, crate::hex(&Sha256::digest(&bytes)));
    }

    #[test]
    fn repacking_from_several_sources_hashes_each_whole_and_verifies() {
        let file_of = |bytes: &[u8]| {
            let mut file = tempfile::tempfile().unwrap();
            io::Write::write_all(&mut file, bytes).unwrap();
            file
        };
        let rom_of = |name: &str, bytes: &[u8]| ExpectedRom {
            name: name.into(),
            size: bytes.len() as u64,
            crc: Some(format!("{:08x}", crc32fast::hash(bytes))),
            md5: None,
            sha1: None,
            sha256: None,
        };
        let zipped = zip_of(&[("x.bin", b"aaaa"), ("extra.bin", b"zz")]);
        let zip = file_of(&zipped);
        let loose = file_of(b"bbbbbb");
        let unread = file_of(b"cc");
        let (a, b) = (rom_of("a.bin", b"aaaa"), rom_of("b.bin", b"bbbbbb"));
        let dest = tempfile::tempfile().unwrap();

        let hashed = repack_sources(
            &dest,
            &[(&b, 1, None), (&a, 0, Some("x.bin"))],
            &[&zip, &loose, &unread],
        )
        .unwrap();

        verify(&dest, &[a, b]).unwrap();
        let sha = |bytes: &[u8]| crate::hex(&Sha256::digest(bytes));
        assert_eq!(
            hashed,
            [
                (sha(&zipped), zipped.len() as u64),
                (sha(b"bbbbbb"), 6),
                (sha(b"cc"), 2)
            ]
        );
    }

    fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, bytes) in members {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            io::Write::write_all(&mut zip, bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn probed(bytes: &[u8], limits: &ZipLimits) -> Probed {
        let mut file = tempfile::tempfile().unwrap();
        io::Write::write_all(&mut file, bytes).unwrap();
        probe_within(&file, limits).unwrap()
    }

    fn refused(probed: Probed) -> String {
        match probed {
            Probed::Refused(why) => why,
            Probed::Read(probe) => panic!("read: {probe:?}"),
        }
    }

    #[test]
    fn a_zip_past_its_limits_or_holding_an_archive_is_refused_before_its_members_are_read() {
        let roomy = ZipLimits::default();
        let one_game = zip_of(&[("a.bin", b"aaaa"), ("b.bin", b"bbbb")]);
        assert!(matches!(probed(&one_game, &roomy), Probed::Read(p) if p.payloads.len() == 2));

        let few = ZipLimits {
            members: 1,
            ..roomy
        };
        assert!(refused(probed(&one_game, &few)).contains("2 members"));
        let small = ZipLimits {
            member_bytes: 3,
            ..roomy
        };
        assert!(refused(probed(&one_game, &small)).contains("a.bin expands to 4 bytes"));
        let tight = ZipLimits {
            total_bytes: 7,
            ..roomy
        };
        assert!(refused(probed(&one_game, &tight)).contains("expands to 8 bytes"));

        let by_name = zip_of(&[("game.bin", b"rom"), ("disc.7z", b"not really")]);
        assert!(refused(probed(&by_name, &roomy)).contains("disc.7z"));
        let by_magic = zip_of(&[("game.bin", b"rom"), ("data.bin", &one_game)]);
        assert!(refused(probed(&by_magic, &roomy)).contains("data.bin, which reads as ZIP"));
    }

    #[test]
    fn a_directory_declaring_more_members_than_allowed_is_refused_before_the_archive_is_opened() {
        let mut bytes = zip_of(&[("a.bin", b"aaaa")]);
        let end = bytes.len() - 22;
        assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
        bytes[end + 8..end + 12].copy_from_slice(&[0x60, 0xea, 0x60, 0xea]);

        assert!(refused(probed(&bytes, &ZipLimits::default())).contains("60000 members"));
        // An Audit's probe allows any 32-bit count, but not a directory larger than its bound.
        bytes[end + 12..end + 16].copy_from_slice(&(100u32 << 20).to_le_bytes());
        let mut file = tempfile::tempfile().unwrap();
        io::Write::write_all(&mut file, &bytes).unwrap();
        let probe = probe(&file).unwrap();
        assert!(probe.payloads.is_empty(), "{probe:?}");
        assert!(probe.error.unwrap().contains("not read"));
    }

    #[test]
    fn a_member_that_expands_past_its_declared_size_is_refused_without_reading_on() {
        let mut bytes = zip_of(&[("zeros.bin", &[0u8; 100_000])]);
        // Declare 10 bytes, in both the local and central headers, for what expands to 100000.
        for (signature, offset) in [(b"PK\x03\x04", 22), (b"PK\x01\x02", 24)] {
            let at = bytes
                .windows(4)
                .position(|w| w == signature.as_slice())
                .unwrap();
            bytes[at + offset..at + offset + 4].copy_from_slice(&10u32.to_le_bytes());
        }

        match probed(&bytes, &ZipLimits::default()) {
            Probed::Refused(why) => assert!(why.contains("more than its archive says"), "{why}"),
            Probed::Read(probe) => {
                assert!(probe.payloads.is_empty(), "{probe:?}");
                assert!(probe.error.unwrap().contains("declared"));
            }
        }
    }
}
