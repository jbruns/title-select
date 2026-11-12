#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha1::Digest;

/// A ROM as a DAT lists it: the checksums are computed from `payload` at test time.
pub struct RomSpec {
    pub name: &'static str,
    pub payload: Vec<u8>,
    pub sha256: bool,
    pub crc: bool,
}

pub fn rom(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        name,
        payload: payload.to_vec(),
        sha256: true,
        crc: true,
    }
}

pub fn rom_without_sha256(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        name,
        payload: payload.to_vec(),
        sha256: false,
        crc: true,
    }
}

/// A ROM listed with SHA-1 only, as some DATs do for ROMs whose CRC is unknown.
pub fn rom_without_crc(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        name,
        payload: payload.to_vec(),
        sha256: false,
        crc: false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn rom_xml(r: &RomSpec) -> String {
    let p = &r.payload;
    let mut checksums = String::new();
    if r.crc {
        checksums += &format!(
            r#" crc="{:08x}" md5="{}""#,
            crc32fast::hash(p),
            hex(&md5::Md5::digest(p))
        );
    }
    checksums += &format!(r#" sha1="{}""#, hex(&sha1::Sha1::digest(p)));
    if r.sha256 {
        checksums += &format!(r#" sha256="{}""#, hex(&sha2::Sha256::digest(p)));
    }
    format!(r#"<rom name="{}" size="{}"{checksums}/>"#, r.name, p.len())
}

/// Writes a No-Intro style Logiqx DAT with the given Games and returns its path.
pub fn write_dat(dir: &Path, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
    write_dat_version(dir, "test.dat", "20260101-000000", games)
}

/// Writes a revision of the `Test - System` DAT, with its header `version`, to `dir/file`.
pub fn write_dat_version(
    dir: &Path,
    file: &str,
    version: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    write_dat_full(dir, file, version, None, games)
}

/// Writes a DAT whose header names the Header Rule `rule` (`<clrmamepro header="…"/>`), as No-Intro's headerless
/// DATs do.
pub fn write_dat_with_header(dir: &Path, rule: &str, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
    write_dat_full(dir, "test.dat", "20260101-000000", Some(rule), games)
}

fn write_dat_full(
    dir: &Path,
    file: &str,
    version: &str,
    rule: Option<&str>,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    let clrmamepro = rule
        .map(|r| format!("    <clrmamepro header=\"{r}\"/>\n"))
        .unwrap_or_default();
    let mut xml = format!(
        r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Test - System</name>
    <version>{version}</version>
{clrmamepro}  </header>
"#
    );
    for (game, roms) in games {
        xml.push_str(&format!("  <game name=\"{game}\">\n"));
        for r in roms {
            xml.push_str(&format!("    {}\n", rom_xml(r)));
        }
        xml.push_str("  </game>\n");
    }
    xml.push_str("</datafile>\n");
    let path = dir.join(file);
    fs::write(&path, xml).unwrap();
    path
}

pub fn write_file(path: &Path, payload: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, payload).unwrap();
}

pub fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    write_zip_with(path, members, zip::CompressionMethod::Deflated);
}

/// Members stored uncompressed, so a test can find and alter their bytes in the archive.
pub fn write_stored_zip(path: &Path, members: &[(&str, &[u8])]) {
    write_zip_with(path, members, zip::CompressionMethod::Stored);
}

fn write_zip_with(path: &Path, members: &[(&str, &[u8])], method: zip::CompressionMethod) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    for (name, payload) in members {
        zip.start_file(*name, options).unwrap();
        zip.write_all(payload).unwrap();
    }
    zip.finish().unwrap();
}

/// Two temp dirs: one for the DAT, one for the Source Set.
pub struct Fixture {
    pub dat_dir: tempfile::TempDir,
    pub set: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            dat_dir: tempfile::tempdir().unwrap(),
            set: tempfile::tempdir().unwrap(),
        }
    }

    pub fn dat(&self, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
        write_dat(self.dat_dir.path(), games)
    }

    pub fn set_path(&self, rel: &str) -> PathBuf {
        self.set.path().join(rel)
    }
}

/// `len` bytes of noise from `seed`, the same for the same seed: a synthetic disc track.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A synthetic Redump-style disc, `<game>.cue` and `tracks` tracks `<game> (Track N).bin` of noise, the first
/// `track_len` bytes long; the cue sheet names each track, as a real one does.
pub fn disc(game: &str, tracks: usize, track_len: usize, seed: u64) -> Vec<RomSpec> {
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let mut cue = String::new();
    let mut roms = Vec::new();
    for n in 1..=tracks {
        let name = if tracks == 1 {
            format!("{game}.bin")
        } else {
            format!("{game} (Track {n}).bin")
        };
        let kind = if n == 1 { "MODE2/2352" } else { "AUDIO" };
        cue += &format!("FILE \"{name}\" BINARY\n  TRACK {n:02} {kind}\n    INDEX 01 00:00:00\n");
        let len = if n == 1 { track_len } else { 2352 * 4 + n };
        roms.push(rom(leak(name), &noise(seed + n as u64, len)));
    }
    roms.insert(0, rom(leak(format!("{game}.cue")), cue.as_bytes()));
    roms
}

/// The ROMs of a synthetic disc game, each written loose into `<dir>/<game>/`: a loose folder.
pub fn write_loose_folder(dir: &Path, game: &str, roms: &[RomSpec]) {
    for r in roms {
        write_file(&dir.join(game).join(r.name), &r.payload);
    }
}
