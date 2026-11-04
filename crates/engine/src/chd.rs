//! CHD (MAME's Compressed Hunks of Data): what a Play Library stores a disc as when its Storage Policy says so. chdman
//! builds each one as a subprocess, and extracts it once to check every track against the DAT before it is published;
//! afterwards only its v5 header is read, never its compressed data.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use title_select_api::build::ExpectedRom;
use title_select_api::storage::ChdKind;

use crate::build::verify_loose;

/// The environment variable naming the chdman to run, where the configuration names none.
pub const CHDMAN_ENV: &str = "TITLE_SELECT_CHDMAN";

/// The bytes of a CHD v5 header.
pub const V5_HEADER: usize = 124;

/// The bytes of a cue sheet read to find its tracks; a Redump sheet is a few kilobytes.
const CUE_MAX: u64 = 1 << 20;

/// The bytes of chdman's output kept for an error.
const LOG_TAIL: u64 = 2048;

/// How often a running chdman is checked for cancellation.
const POLL: Duration = Duration::from_millis(100);

/// What a CHD v5 header says of its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The bytes its hunks expand to.
    pub logical_bytes: u64,
    /// The SHA-1 of its raw data, the expanded hunks alone, as hex: the same however it is compressed and whatever
    /// metadata it carries.
    pub raw_sha1: String,
    /// The SHA-1 of its raw data and its checksummed metadata, such as a CD's track list, together, as hex: the
    /// same however it is compressed, and what a CHD is checked by once it is verified.
    pub sha1: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The v5 header of `file`, read from its first [`V5_HEADER`] bytes alone: its tag, length and version are checked,
/// nothing past it is read.
pub fn header(file: &File) -> Result<Header, String> {
    let mut raw = [0u8; V5_HEADER];
    file.read_exact_at(&mut raw, 0)
        .map_err(|e| format!("its CHD header can't be read: {e}"))?;
    if &raw[0..8] != b"MComprHD" {
        return Err("it is not a CHD".into());
    }
    let be32 = |at: usize| u32::from_be_bytes(raw[at..at + 4].try_into().expect("4 bytes"));
    let be64 = |at: usize| u64::from_be_bytes(raw[at..at + 8].try_into().expect("8 bytes"));
    let (length, version) = (be32(8), be32(12));
    if version != 5 || length as usize != V5_HEADER {
        return Err(format!(
            "it is a version {version} CHD, and only version 5 is read"
        ));
    }
    Ok(Header {
        logical_bytes: be64(32),
        raw_sha1: hex(&raw[64..84]),
        sha1: hex(&raw[84..104]),
    })
}

/// The file each track of the cue sheet `cue` is read from, in order: one `FILE` per track, each naming a plain file
/// beside the sheet, as a Redump sheet does. A sheet with a `FILE` holding no track or several is refused, since
/// chdman gives back one file per track.
pub fn cue_tracks(cue: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(cue).map_err(|_| "its cue sheet is not UTF-8 text")?;
    let mut files: Vec<(String, usize)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut words = line.splitn(2, char::is_whitespace);
        match words.next().map(str::to_ascii_uppercase).as_deref() {
            Some("FILE") => {
                let rest = words.next().unwrap_or_default().trim();
                let name = match rest.strip_prefix('"') {
                    Some(quoted) => quoted.split_once('"').map(|(name, _)| name),
                    None => rest.split_whitespace().next(),
                }
                .filter(|n| !n.is_empty())
                .ok_or_else(|| format!("its cue sheet has a FILE line naming no file: {line}"))?;
                if name.contains(['/', '\\']) || name == "." || name == ".." {
                    return Err(format!(
                        "its cue sheet names {name}, which is not a file beside it"
                    ));
                }
                files.push((name.to_owned(), 0));
            }
            Some("TRACK") => match files.last_mut() {
                Some((_, tracks)) => *tracks += 1,
                None => return Err("its cue sheet has a TRACK before any FILE".into()),
            },
            _ => {}
        }
    }
    if files.is_empty() {
        return Err("its cue sheet names no file".into());
    }
    if let Some((name, n)) = files.iter().find(|(_, n)| *n != 1) {
        return Err(format!(
            "its cue sheet reads {n} tracks from {name}, and a CHD is checked one track per file"
        ));
    }
    Ok(files.into_iter().map(|(name, _)| name).collect())
}

/// The chdman a Build runs, and how many threads it compresses with.
#[derive(Debug, Clone)]
pub struct Chdman {
    pub program: PathBuf,
    pub threads: usize,
}

/// Where chdman is: `configured`, else the program [`CHDMAN_ENV`] names, else `chdman` on the `PATH`.
pub fn locate(configured: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = configured {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os(CHDMAN_ENV).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("chdman"))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            format!(
                "chdman was not found: name it under [chd] chdman in the configuration, or with {CHDMAN_ENV}, or put \
                 it on the PATH"
            )
        })
}

/// Why chdman's work stopped short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// You cancelled it; chdman was stopped.
    Cancelled,
    /// It failed, or what it gave back does not match the DAT: why.
    Failed(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::Failed(why)
    }
}

/// The last bytes of the log at `path`, as text.
fn tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL)));
    let mut out = Vec::new();
    let _ = file.take(LOG_TAIL).read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    // chdman redraws its progress with carriage returns; only the last of each line is worth reading.
    text.lines()
        .map(|l| l.rsplit('\r').next().unwrap_or(l).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

impl Chdman {
    /// Runs chdman with `args` in `dir`, its output appended to `chdman.log` there, until it exits or `cancelled`
    /// says to stop it: then it is killed and waited for.
    pub fn run<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), Stop> {
        let log_path = dir.join("chdman.log");
        let log = File::options()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("{}: {e}", log_path.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let mut child = Command::new(&self.program)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("chdman could not be run as {}: {e}", self.program.display()))?;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if cancelled() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Stop::Cancelled);
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Stop::Failed(e.to_string()));
                }
            }
        };
        if status.success() {
            return Ok(());
        }
        let verb = args
            .first()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .unwrap_or_default();
        Err(Stop::Failed(format!(
            "chdman {verb} failed ({status}): {}",
            tail(&log_path)
        )))
    }

    /// Builds `dir/out.chd` from the disc whose ROMs are written loose in `dir/in`, `main` being its `.cue` or
    /// `.iso`.
    pub fn create(
        &self,
        dir: &Path,
        kind: ChdKind,
        main: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PathBuf, Stop> {
        let verb = match kind {
            ChdKind::Cd => "createcd",
            ChdKind::Dvd => "createdvd",
        };
        let input = Path::new("in").join(main);
        let threads = self.threads.max(1).to_string();
        self.run(
            dir,
            &[
                OsStr::new(verb),
                OsStr::new("-i"),
                input.as_os_str(),
                OsStr::new("-o"),
                OsStr::new("out.chd"),
                OsStr::new("-np"),
                OsStr::new(&threads),
            ],
            cancelled,
        )?;
        Ok(dir.join("out.chd"))
    }

    /// Extracts `dir/out.chd` into `dir/x`, and checks what it gives back against the disc's ROMs `roms`: each track
    /// against the `.bin` its cue sheet `cue` names, or the image against the `.iso`. Returns its header once every
    /// one matches. The cue sheet itself is not in the CHD; it was checked against the DAT before chdman read it.
    pub fn verify(
        &self,
        dir: &Path,
        kind: ChdKind,
        cue: Option<&[u8]>,
        roms: &[ExpectedRom],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Header, Stop> {
        let out = dir.join("x");
        std::fs::create_dir(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let expected: Vec<&ExpectedRom> = match (kind, cue) {
            (ChdKind::Cd, Some(cue)) => cue_tracks(cue)?
                .iter()
                .map(|name| {
                    roms.iter().find(|r| r.name == *name).ok_or_else(|| {
                        format!("its cue sheet names {name}, which the DAT doesn't list")
                    })
                })
                .collect::<Result<_, _>>()?,
            (ChdKind::Dvd, None) => roms.iter().collect(),
            _ => return Err(Stop::Failed("a CD is checked by its cue sheet".into())),
        };
        let tracks = expected.len();
        let bins = roms
            .iter()
            .filter(|r| !r.name.to_ascii_lowercase().ends_with(".cue"));
        if kind == ChdKind::Cd && bins.count() != tracks {
            return Err(Stop::Failed(format!(
                "its cue sheet names {tracks} tracks, and the DAT lists another number of .bin files"
            )));
        }
        let args: &[&str] = match kind {
            ChdKind::Cd => &[
                "extractcd",
                "-i",
                "out.chd",
                "-o",
                "x/x.cue",
                "-ob",
                "x/t%t.bin",
                "-sb",
            ],
            ChdKind::Dvd => &["extractdvd", "-i", "out.chd", "-o", "x/x.iso"],
        };
        self.run(dir, args, cancelled)?;
        let given: Vec<PathBuf> = match kind {
            ChdKind::Cd => (1..=tracks)
                .map(|n| out.join(format!("t{n}.bin")))
                .collect(),
            ChdKind::Dvd => vec![out.join("x.iso")],
        };
        let found = std::fs::read_dir(&out)
            .map_err(|e| format!("{}: {e}", out.display()))?
            .filter(|e| e.as_ref().map_or(true, |e| e.file_name() != "x.cue"))
            .count();
        if found != given.len() {
            return Err(Stop::Failed(format!(
                "chdman gave back {found} tracks, not the {} the DAT lists",
                given.len()
            )));
        }
        for (path, rom) in given.iter().zip(expected) {
            if cancelled() {
                return Err(Stop::Cancelled);
            }
            let file = File::open(path).map_err(|e| format!("{}: {e}", rom.name))?;
            let len = file.metadata().map_err(|e| e.to_string())?.len();
            if len != rom.size {
                return Err(Stop::Failed(format!(
                    "{}: chdman gave back {len} bytes, not the DAT's {}",
                    rom.name, rom.size
                )));
            }
            verify_loose(&file, rom)
                .map_err(|why| format!("chdman gave back other bytes: {why}"))?;
        }
        let chd = File::open(dir.join("out.chd")).map_err(|e| e.to_string())?;
        header(&chd).map_err(Stop::Failed)
    }
}

/// Reads at most [`CUE_MAX`] bytes of the cue sheet `file` from its start.
pub fn read_cue(file: &File) -> Result<Vec<u8>, String> {
    let mut reader = file;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    reader
        .take(CUE_MAX + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > CUE_MAX {
        return Err(format!("its cue sheet is more than {CUE_MAX} bytes"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn v5(raw: [u8; 20], sha1: [u8; 20]) -> Vec<u8> {
        let mut h = vec![0u8; V5_HEADER];
        h[0..8].copy_from_slice(b"MComprHD");
        h[8..12].copy_from_slice(&(V5_HEADER as u32).to_be_bytes());
        h[12..16].copy_from_slice(&5u32.to_be_bytes());
        h[32..40].copy_from_slice(&4096u64.to_be_bytes());
        h[64..84].copy_from_slice(&raw);
        h[84..104].copy_from_slice(&sha1);
        h
    }

    fn file_of(bytes: &[u8]) -> File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f
    }

    #[test]
    fn a_v5_header_gives_its_raw_and_combined_sha1() {
        let mut bytes = v5([0xab; 20], [0x01; 20]);
        bytes.extend_from_slice(&[0xff; 64]);
        let h = header(&file_of(&bytes)).unwrap();
        assert_eq!(h.raw_sha1, "ab".repeat(20));
        assert_eq!(h.sha1, "01".repeat(20));
        assert_eq!(h.logical_bytes, 4096);
    }

    #[test]
    fn anything_but_a_v5_chd_is_refused() {
        let mut v4 = v5([0; 20], [0; 20]);
        v4[12..16].copy_from_slice(&4u32.to_be_bytes());
        assert!(header(&file_of(&v4)).unwrap_err().contains("version 4"));
        assert_eq!(
            header(&file_of(&[0u8; V5_HEADER])).unwrap_err(),
            "it is not a CHD"
        );
        assert!(header(&file_of(b"MComprHD")).is_err());
    }

    #[test]
    fn a_cue_sheet_names_one_file_per_track() {
        let cue = b"FILE \"A (Track 1).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n\
                    file \"A (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 00 00:00:00\r\n";
        assert_eq!(
            cue_tracks(cue).unwrap(),
            vec!["A (Track 1).bin".to_string(), "A (Track 2).bin".into()]
        );
        assert!(cue_tracks(b"FILE \"a.bin\" BINARY\n TRACK 01 AUDIO\n TRACK 02 AUDIO\n").is_err());
        assert!(cue_tracks(b"FILE \"../a.bin\" BINARY\n TRACK 01 AUDIO\n").is_err());
        assert!(cue_tracks(b"TRACK 01 AUDIO\n").is_err());
        assert!(cue_tracks(b"REM nothing\n").is_err());
    }
}
