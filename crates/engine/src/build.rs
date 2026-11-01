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
        let mut reader = BufReader::new(opened);
        let added = match source.member {
            None => writer.add(&rom.name, rom.size, &mut reader),
            Some(member) => {
                let mut archive = zip::ZipArchive::new(reader)
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
    let reader = BufReader::new(Hashing::new(source).map_err(|e| e.to_string())?);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
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
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
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

/// What reading a file in a Managed Folder found: the SHA-256 of the whole file, and the payloads it holds.
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
    let Ok(mut archive) = zip::ZipArchive::new(reader) else {
        probe.torrentzip = false;
        probe.payloads.push(Payload::new(None, whole));
        return Ok(probe);
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
}
