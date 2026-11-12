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

/// How often what a running chdman has written is measured against its budget.
const BUDGET_POLL: Duration = Duration::from_millis(10);

/// Why chdman was stopped for writing more than `budget` allows.
fn over_budget<T>(verb: &str, budget: Option<(&Path, u64)>) -> Result<T, Stop> {
    Err(Stop::Failed(format!(
        "chdman {verb} wrote more than the {} bytes the disc's files take, and was stopped",
        budget.map_or(0, |(_, most)| most)
    )))
}

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

/// The DAT's ROM for each track the cue sheet names, `names` as [`cue_tracks`] gives them, in its order: only when the
/// sheet names each of the DAT's `.bin` files exactly once, and nothing else. A track named twice could stand in for
/// one never named, which extraction would then never check.
pub fn cue_matches<'a>(
    names: &[String],
    roms: &'a [ExpectedRom],
) -> Result<Vec<&'a ExpectedRom>, String> {
    let bins: Vec<&ExpectedRom> = roms
        .iter()
        .filter(|r| !r.name.to_ascii_lowercase().ends_with(".cue"))
        .collect();
    let mut problems = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if !bins.iter().any(|r| r.name == *name) {
            problems.push(format!("names {name}, which the DAT doesn't list"));
        } else if names[..i].iter().filter(|n| *n == name).count() == 1 {
            problems.push(format!("names {name} more than once"));
        }
    }
    for rom in &bins {
        if !names.contains(&rom.name) {
            problems.push(format!("leaves out {}", rom.name));
        }
    }
    if !problems.is_empty() {
        return Err(format!(
            "its cue sheet must name each of the DAT's tracks once, but it {}",
            problems.join(", and ")
        ));
    }
    Ok(names
        .iter()
        .map(|name| {
            *bins
                .iter()
                .find(|r| r.name == *name)
                .expect("checked above")
        })
        .collect())
}

/// How a cue sheet lays out one track: what a CD's CHD keeps of it, and gives back when it is extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackLayout {
    pub number: u32,
    /// Its mode, such as `MODE1/2352` or `AUDIO`, in upper case.
    pub mode: String,
    /// Where its INDEX 00 and 01 are, in frames from the start of its file.
    pub index00: Option<u32>,
    pub index01: Option<u32>,
    /// Its PREGAP and POSTGAP, in frames.
    pub pregap: Option<u32>,
    pub postgap: Option<u32>,
}

/// `frames` as a cue sheet writes a position, `mm:ss:ff`.
fn msf(frames: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        frames / 75 / 60,
        frames / 75 % 60,
        frames % 75
    )
}

/// The layout of each track of the cue sheet `cue`, in order: file names, other commands, whitespace and case are
/// not part of it.
pub fn cue_layout(cue: &[u8]) -> Result<Vec<TrackLayout>, String> {
    let text = std::str::from_utf8(cue).map_err(|_| "its cue sheet is not UTF-8 text")?;
    let frames = |at: &str, line: &str| -> Result<u32, String> {
        let parts: Vec<u32> = at
            .split(':')
            .map(|p| p.parse::<u32>().ok().filter(|_| p.len() <= 3))
            .collect::<Option<_>>()
            .filter(|p: &Vec<u32>| p.len() == 3 && p[1] < 60 && p[2] < 75)
            .ok_or_else(|| format!("its cue sheet has a position it can't read: {line}"))?;
        Ok((parts[0] * 60 + parts[1]) * 75 + parts[2])
    };
    let mut tracks: Vec<TrackLayout> = Vec::new();
    for line in text.lines() {
        let words: Vec<String> = line
            .split_whitespace()
            .map(str::to_ascii_uppercase)
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let current = |tracks: &mut Vec<TrackLayout>| -> Result<(), String> {
            match tracks.is_empty() {
                true => Err(format!("its cue sheet has {} before any TRACK", words[0])),
                false => Ok(()),
            }
        };
        match words.as_slice() {
            ["TRACK", number, mode, ..] => tracks.push(TrackLayout {
                number: number
                    .parse()
                    .map_err(|_| format!("its cue sheet has a TRACK it can't read: {line}"))?,
                mode: (*mode).to_owned(),
                index00: None,
                index01: None,
                pregap: None,
                postgap: None,
            }),
            ["INDEX", n, at, ..] => {
                current(&mut tracks)?;
                let at = frames(at, line)?;
                let track = tracks.last_mut().expect("checked above");
                match n.parse::<u32>() {
                    Ok(0) => track.index00 = Some(at),
                    Ok(1) => track.index01 = Some(at),
                    Ok(_) => {}
                    Err(_) => {
                        return Err(format!("its cue sheet has an INDEX it can't read: {line}"));
                    }
                }
            }
            ["PREGAP", at, ..] => {
                current(&mut tracks)?;
                tracks.last_mut().expect("checked above").pregap = Some(frames(at, line)?);
            }
            ["POSTGAP", at, ..] => {
                current(&mut tracks)?;
                tracks.last_mut().expect("checked above").postgap = Some(frames(at, line)?);
            }
            _ => {}
        }
    }
    if tracks.is_empty() {
        return Err("its cue sheet lists no track".into());
    }
    Ok(tracks)
}

/// How the layout `given` differs from the DAT's cue sheet's, `dat`, or none when they are the same.
pub fn layout_difference(dat: &[TrackLayout], given: &[TrackLayout]) -> Option<String> {
    if dat.len() != given.len() {
        return Some(format!(
            "it has {} tracks, and the DAT's cue sheet lists {}",
            given.len(),
            dat.len()
        ));
    }
    let at = |p: Option<u32>| p.map_or("nowhere".to_owned(), msf);
    let gap = |p: Option<u32>| p.map_or("none".to_owned(), msf);
    for (n, (d, g)) in dat.iter().zip(given).enumerate() {
        let n = n + 1;
        let why = if d.number != g.number {
            format!("track {n} is numbered {}, not {}", g.number, d.number)
        } else if d.mode != g.mode {
            format!("track {n} is {}, not {}", g.mode, d.mode)
        } else if d.index00 != g.index00 {
            format!(
                "track {n} starts INDEX 00 at {}, not {}",
                at(g.index00),
                at(d.index00)
            )
        } else if d.index01 != g.index01 {
            format!(
                "track {n} starts INDEX 01 at {}, not {}",
                at(g.index01),
                at(d.index01)
            )
        } else if d.pregap != g.pregap {
            format!(
                "track {n} has PREGAP {}, not {}",
                gap(g.pregap),
                gap(d.pregap)
            )
        } else if d.postgap != g.postgap {
            format!(
                "track {n} has POSTGAP {}, not {}",
                gap(g.postgap),
                gap(d.postgap)
            )
        } else {
            continue;
        };
        return Some(why);
    }
    None
}

/// One track of a CD's CHD, as its metadata says.
#[derive(Debug)]
struct ChdTrack {
    number: u64,
    kind: String,
    /// The sectors it takes in the CHD, its padding included.
    frames: u64,
    /// For a GD-ROM, the sectors of padding in `frames` that are never given back.
    pad: u64,
}

/// The most metadata entries read from a CHD: a CD has at most 99 tracks.
const META_MAX: usize = 256;

/// The sector a GD-ROM's high-density area starts at: chdman pads the single-density area up to it.
const GD_HIGH_DENSITY: u64 = 45_000;

/// The tracks a CD's CHD lists in its metadata, by number, and whether it is a GD-ROM; only the metadata is read,
/// never its hunks.
fn chd_tracks(file: &File) -> Result<(bool, Vec<ChdTrack>), String> {
    let mut raw = [0u8; 8];
    file.read_exact_at(&mut raw, 48)
        .map_err(|e| format!("its CHD header can't be read: {e}"))?;
    let mut at = u64::from_be_bytes(raw);
    let (mut cd, mut gd) = (Vec::new(), Vec::new());
    for _ in 0..META_MAX {
        if at == 0 {
            break;
        }
        let mut entry = [0u8; 16];
        file.read_exact_at(&mut entry, at)
            .map_err(|e| format!("its metadata can't be read: {e}"))?;
        let tag: [u8; 4] = entry[0..4].try_into().expect("4 bytes");
        let length = u32::from_be_bytes([0, entry[5], entry[6], entry[7]]) as usize;
        let next = u64::from_be_bytes(entry[8..16].try_into().expect("8 bytes"));
        match &tag {
            b"CHT2" | b"CHTR" | b"CHGD" => {
                let mut data = vec![0u8; length];
                file.read_exact_at(&mut data, at + 16)
                    .map_err(|e| format!("its metadata can't be read: {e}"))?;
                let text = String::from_utf8_lossy(&data);
                let field = |key: &str| {
                    text.split(|c: char| c.is_whitespace() || c == '\0')
                        .find_map(|word| word.strip_prefix(key)?.strip_prefix(':'))
                        .map(str::to_owned)
                };
                let number = |key: &str| -> Result<u64, String> {
                    field(key).and_then(|v| v.parse().ok()).ok_or_else(|| {
                        format!("its track metadata has no {key} it can read: {text}")
                    })
                };
                let track = ChdTrack {
                    number: number("TRACK")?,
                    kind: field("TYPE").unwrap_or_default(),
                    frames: number("FRAMES")?,
                    pad: if &tag == b"CHGD" { number("PAD")? } else { 0 },
                };
                match &tag {
                    b"CHGD" => gd.push(track),
                    _ => cd.push(track),
                }
            }
            b"CHCD" | b"CHGT" => {
                return Err("its track list is in an older form, which isn't read; convert it with a newer chdman".into());
            }
            _ => {}
        }
        at = next;
    }
    if at != 0 {
        return Err(format!("it has more than {META_MAX} metadata entries"));
    }
    let is_gd = !gd.is_empty();
    if is_gd && !cd.is_empty() {
        return Err("it lists both CD and GD-ROM tracks".into());
    }
    let mut tracks = if is_gd { gd } else { cd };
    tracks.sort_by_key(|t| t.number);
    if tracks
        .iter()
        .enumerate()
        .any(|(i, t)| t.number != i as u64 + 1)
    {
        return Err("its tracks aren't numbered 1 onwards, each once".into());
    }
    Ok((is_gd, tracks))
}

/// Whether the CHD `file` can hold no more than the disc whose files are `bins`, before it is copied or extracted:
/// a CD's `.bin` tracks in its cue sheet's order, or a DVD's image. A CD's track metadata, which chdman sizes what it
/// gives back by, must list those tracks, each in raw 2352-byte sectors, and as many sectors as the DAT's track; a
/// GD-ROM's tracks shift sectors between them, so their sectors together, less the padding up to its high-density
/// area, must be the DAT's. Its header's logical size must then be no more than those sectors, each track rounded up
/// to chdman's 4-sector padding, at 2448 bytes a sector, its data and subcode; a DVD's no more than its image in whole
/// 2048-byte sectors.
pub fn fits(file: &File, kind: ChdKind, bins: &[&ExpectedRom]) -> Result<(), String> {
    let logical = header(file)?.logical_bytes;
    let (bound, what) = match kind {
        ChdKind::Dvd => (
            bins.iter()
                .map(|r| r.size.div_ceil(2048) * 2048)
                .sum::<u64>(),
            "the DAT's image takes",
        ),
        ChdKind::Cd => {
            let (gd, tracks) = chd_tracks(file)?;
            if tracks.len() != bins.len() {
                return Err(format!(
                    "it has {} tracks, and the DAT's cue sheet lists {}",
                    tracks.len(),
                    bins.len()
                ));
            }
            let mut wanted = 0u64;
            for (track, rom) in tracks.iter().zip(bins) {
                let n = track.number;
                if !matches!(track.kind.as_str(), "MODE1_RAW" | "MODE2_RAW" | "AUDIO") {
                    return Err(format!(
                        "track {n} is {}, which doesn't give back the 2352-byte sectors the DAT's tracks hold",
                        track.kind
                    ));
                }
                if rom.size % 2352 != 0 {
                    return Err(format!("{} is not whole 2352-byte sectors", rom.name));
                }
                let sectors = rom.size / 2352;
                if track.pad > track.frames {
                    return Err(format!("track {n} pads more sectors than it has"));
                }
                if !gd && track.frames != sectors {
                    return Err(format!(
                        "track {n} has {} frames, and the DAT's {} has {sectors}",
                        track.frames, rom.name
                    ));
                }
                wanted += sectors;
            }
            let given: u64 = tracks
                .iter()
                .map(|t| t.frames - t.pad)
                .fold(0, u64::saturating_add);
            let pad: u64 = tracks.iter().map(|t| t.pad).fold(0, u64::saturating_add);
            if gd && (given != wanted || pad > GD_HIGH_DENSITY) {
                return Err(format!(
                    "its tracks give back {given} sectors after {pad} of padding, and the DAT's hold {wanted}"
                ));
            }
            (
                tracks
                    .iter()
                    .map(|t| {
                        t.frames
                            .next_multiple_of(CD_TRACK_PADDING)
                            .saturating_mul(CD_FRAME)
                    })
                    .fold(0, u64::saturating_add),
                "its tracks take",
            )
        }
    };
    if logical > bound {
        return Err(format!(
            "its header says it expands to {logical} bytes, more than the {bound} {what}"
        ));
    }
    Ok(())
}

/// The bytes a CD's CHD keeps for each sector: 2352 of data and 96 of subcode.
const CD_FRAME: u64 = 2448;

/// The sectors chdman pads each CD track to a multiple of.
const CD_TRACK_PADDING: u64 = 4;

/// How long chdman may run on one disc: `base`, and `per_gib` for each GiB of the disc's tracks, prorated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub base: Duration,
    pub per_gib: Duration,
}

impl Limits {
    /// The time chdman may take over a disc of `bytes`.
    pub fn for_bytes(&self, bytes: u64) -> Duration {
        let per_gib = self.per_gib.as_nanos();
        let extra = per_gib.saturating_mul(u128::from(bytes)) >> 30;
        self.base.saturating_add(Duration::from_nanos(
            u64::try_from(extra).unwrap_or(u64::MAX),
        ))
    }
}

/// The chdman a Build runs, how many threads it compresses with, and how long each run may take before it is
/// stopped.
#[derive(Debug, Clone)]
pub struct Chdman {
    pub program: PathBuf,
    pub threads: usize,
    pub deadline: Duration,
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
    /// Runs chdman with `args` in `dir`, its output appended to `chdman.log` there, until it exits, `cancelled` says
    /// to stop it, or it outruns its deadline: then it is killed and waited for.
    pub fn run<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), Stop> {
        self.run_within(dir, args, cancelled, None)
    }

    /// As [`Chdman::run`], and, given a `budget`, stops chdman once the files it writes in the folder named there take
    /// more than its bytes: what it writes is checked as it runs, not once it is done.
    pub fn run_within<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
        cancelled: &dyn Fn() -> bool,
        budget: Option<(&Path, u64)>,
    ) -> Result<(), Stop> {
        let over = || {
            budget.and_then(|(out, most)| {
                let written: u64 = std::fs::read_dir(out)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .fold(0, u64::saturating_add);
                (written > most).then_some(most)
            })
        };
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
        let verb = args
            .first()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .unwrap_or_default();
        let started = std::time::Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if cancelled() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Stop::Cancelled);
                }
                Ok(None) if started.elapsed() > self.deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Stop::Failed(format!(
                        "chdman {verb} took longer than {}, the most this disc is given, and was stopped. If your \
                         host needs longer, raise timeout_seconds under [chd] in the configuration",
                        match self.deadline.as_secs() {
                            1 => "1 second".to_string(),
                            n => format!("{n} seconds"),
                        }
                    )));
                }
                Ok(None) if over().is_some() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break over_budget(&verb, budget)?;
                }
                Ok(None) => std::thread::sleep(match budget {
                    Some(_) => BUDGET_POLL,
                    None => POLL,
                }),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Stop::Failed(e.to_string()));
                }
            }
        };
        if over().is_some() {
            return over_budget(&verb, budget);
        }
        if status.success() {
            return Ok(());
        }
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
        let expected: Vec<&ExpectedRom> = match (kind, cue) {
            (ChdKind::Cd, Some(cue)) => cue_matches(&cue_tracks(cue)?, roms)?,
            (ChdKind::Dvd, None) => roms.iter().collect(),
            _ => return Err(Stop::Failed("a CD is checked by its cue sheet".into())),
        };
        self.extract_and_match(dir, kind, &expected, cancelled)
    }

    /// As [`Chdman::verify`], for a CHD a build did not make, such as one you laid out yourself: a CD's tracks are
    /// matched against the `.bin` files its cue sheet `cue`, the DAT's, names, and the layout chdman gives back with
    /// them, each track's mode, its INDEX 00 and 01, and any PREGAP or POSTGAP, must be that cue sheet's too. A build
    /// makes its CHD from that cue sheet, so its layout is right by construction; a CHD you made is not.
    pub fn verify_adopted(
        &self,
        dir: &Path,
        kind: ChdKind,
        cue: Option<&[u8]>,
        roms: &[ExpectedRom],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Header, Stop> {
        let header = self.verify(dir, kind, cue, roms, cancelled)?;
        if let Some(cue) = cue {
            let given = dir.join("x").join("x.cue");
            let given = File::open(&given)
                .map_err(|e| e.to_string())
                .and_then(|f| read_cue(&f))
                .map_err(|e| format!("chdman gave back no cue sheet: {e}"))?;
            if let Some(why) = layout_difference(&cue_layout(cue)?, &cue_layout(&given)?) {
                return Err(Stop::Failed(format!(
                    "its tracks hold the DAT's bytes, but not as the DAT's cue sheet lays them out: {why}"
                )));
            }
        }
        Ok(header)
    }

    /// Extracts `dir/out.chd` into `dir/x`, and checks that it gives back exactly `expected`, in order. Returns its
    /// header once every one matches.
    fn extract_and_match(
        &self,
        dir: &Path,
        kind: ChdKind,
        expected: &[&ExpectedRom],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Header, Stop> {
        let out = dir.join("x");
        std::fs::create_dir(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let tracks = expected.len();
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
        // What it gives back is the DAT's files, and a cue sheet: never more.
        let budget = expected
            .iter()
            .map(|r| r.size)
            .fold(CUE_MAX, u64::saturating_add);
        self.run_within(dir, args, cancelled, Some((&out, budget)))?;
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
        for (path, rom) in given.iter().zip(expected.iter().copied()) {
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

    fn rom(name: &str) -> ExpectedRom {
        ExpectedRom {
            name: name.into(),
            size: 2352,
            crc: None,
            md5: None,
            sha1: None,
            sha256: None,
            header: None,
        }
    }

    fn sized(name: &str, size: u64) -> ExpectedRom {
        ExpectedRom { size, ..rom(name) }
    }

    #[test]
    fn a_cue_sheet_names_each_of_the_dats_tracks_exactly_once() {
        let roms = [rom("x.cue"), rom("a.bin"), rom("b.bin"), rom("c.bin")];
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let matched = cue_matches(&names(&["a.bin", "b.bin", "c.bin"]), &roms).unwrap();
        assert_eq!(
            matched.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["a.bin", "b.bin", "c.bin"]
        );
        // A repeated track hides an omitted one.
        let error = cue_matches(&names(&["a.bin", "c.bin", "a.bin"]), &roms).unwrap_err();
        assert!(
            error.contains("a.bin") && error.contains("b.bin"),
            "{error}"
        );
        let error = cue_matches(&names(&["a.bin", "c.bin"]), &roms).unwrap_err();
        assert!(error.contains("b.bin"), "{error}");
        let error = cue_matches(&names(&["a.bin", "b.bin", "c.bin", "d.bin"]), &roms).unwrap_err();
        assert!(error.contains("d.bin"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_cue_sheets_layout_ignores_names_whitespace_and_case() {
        let dat = cue_layout(
            b"FILE \"a (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
              FILE \"a (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n",
        )
        .unwrap();
        let given = cue_layout(
            b"file t1.bin binary\r\ntrack 1 mode1/2352\r\n\tindex 1 0:0:0\r\n\
              file t2.bin binary\r\ntrack 2 audio\r\nindex 0 00:00:00\r\nindex 1 00:02:00\r\n",
        )
        .unwrap();
        assert_eq!(layout_difference(&dat, &given), None);
        let mut moved = given.clone();
        moved[1].index01 = Some(75);
        assert_eq!(
            layout_difference(&dat, &moved).as_deref(),
            Some("track 2 starts INDEX 01 at 00:01:00, not 00:02:00")
        );
        let mut gap = given.clone();
        gap[1].pregap = Some(150);
        assert_eq!(
            layout_difference(&dat, &gap).as_deref(),
            Some("track 2 has PREGAP 00:02:00, not none")
        );
        assert!(layout_difference(&dat, &given[..1]).is_some());
    }

    /// A v5 CHD's header saying it expands to `logical` bytes, and its metadata `entries`, each a tag and its text, as
    /// chdman writes them; no hunks.
    fn chd_of_metadata(logical: u64, entries: &[(&[u8; 4], String)]) -> File {
        let mut out = vec![0u8; V5_HEADER];
        out[0..8].copy_from_slice(b"MComprHD");
        out[8..12].copy_from_slice(&(V5_HEADER as u32).to_be_bytes());
        out[12..16].copy_from_slice(&5u32.to_be_bytes());
        out[32..40].copy_from_slice(&logical.to_be_bytes());
        out[48..56].copy_from_slice(&(V5_HEADER as u64).to_be_bytes());
        for (i, (tag, text)) in entries.iter().enumerate() {
            let mut data = text.clone().into_bytes();
            data.push(0);
            let next = match i + 1 == entries.len() {
                true => 0,
                false => (out.len() + 16 + data.len()) as u64,
            };
            out.extend_from_slice(*tag);
            out.push(0x01);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
            out.extend_from_slice(&next.to_be_bytes());
            out.extend_from_slice(&data);
        }
        file_of(&out)
    }

    fn cd_track(n: u32, kind: &str, frames: u64) -> (&'static [u8; 4], String) {
        (
            b"CHT2",
            format!(
                "TRACK:{n} TYPE:{kind} SUBTYPE:NONE FRAMES:{frames} PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"
            ),
        )
    }

    fn gd_track(n: u32, frames: u64, pad: u64) -> (&'static [u8; 4], String) {
        (
            b"CHGD",
            format!(
                "TRACK:{n} TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:{frames} PAD:{pad} PREGAP:0 PGTYPE:MODE1 PGSUB:NONE \
                 POSTGAP:0"
            ),
        )
    }

    #[test]
    fn a_cds_track_metadata_must_fit_the_dats_tracks_before_it_is_extracted() {
        let bins = [
            sized("a (Track 1).bin", 2352 * 301),
            sized("a (Track 2).bin", 2352 * 200),
        ];
        let bins: Vec<&ExpectedRom> = bins.iter().collect();
        // As chdman 0.289 made it from these tracks: 304 and 200 sectors, at 2448 bytes each.
        let made = [cd_track(1, "MODE1_RAW", 301), cd_track(2, "AUDIO", 200)];
        assert_eq!(
            fits(&chd_of_metadata(1_233_792, &made), ChdKind::Cd, &bins),
            Ok(())
        );

        let inflated = [cd_track(1, "MODE1_RAW", 90_000), cd_track(2, "AUDIO", 200)];
        let why = fits(&chd_of_metadata(1_233_792, &inflated), ChdKind::Cd, &bins).unwrap_err();
        assert!(why.contains("track 1 has 90000 frames"), "{why}");
        let fewer = [cd_track(1, "MODE1_RAW", 301)];
        let why = fits(&chd_of_metadata(1_233_792, &fewer), ChdKind::Cd, &bins).unwrap_err();
        assert!(why.contains("it has 1 tracks"), "{why}");
        let cooked = [cd_track(1, "MODE1", 301), cd_track(2, "AUDIO", 200)];
        assert!(fits(&chd_of_metadata(1_233_792, &cooked), ChdKind::Cd, &bins).is_err());
        let why = fits(&chd_of_metadata(1_233_793, &made), ChdKind::Cd, &bins).unwrap_err();
        assert!(
            why.contains("its header says it expands to 1233793 bytes"),
            "{why}"
        );
    }

    #[test]
    fn a_gd_roms_padding_to_its_high_density_area_fits() {
        // As chdman 0.289 made a GD-ROM of tracks of 300, 225 and 500 sectors: the high-density area starts at
        // sector 45000, padded to by track 2.
        let bins = [
            sized("t1.bin", 2352 * 300),
            sized("t2.bin", 2352 * 225),
            sized("t3.bin", 2352 * 500),
        ];
        let bins: Vec<&ExpectedRom> = bins.iter().collect();
        let made = [
            gd_track(1, 450, 0),
            gd_track(2, 44_550, 44_475),
            gd_track(3, 500, 0),
        ];
        assert_eq!(
            fits(&chd_of_metadata(111_393_792, &made), ChdKind::Cd, &bins),
            Ok(())
        );
        let padded = [
            gd_track(1, 450, 0),
            gd_track(2, 90_550, 90_475),
            gd_track(3, 500, 0),
        ];
        assert!(fits(&chd_of_metadata(225_000_000, &padded), ChdKind::Cd, &bins).is_err());
    }

    #[test]
    fn a_dvds_header_must_fit_its_image() {
        let iso = [sized("a.iso", 2048 * 400)];
        let iso: Vec<&ExpectedRom> = iso.iter().collect();
        assert_eq!(
            fits(&chd_of_metadata(819_200, &[]), ChdKind::Dvd, &iso),
            Ok(())
        );
        assert!(fits(&chd_of_metadata(819_201, &[]), ChdKind::Dvd, &iso).is_err());
    }

    #[test]
    fn chdman_writing_past_its_budget_is_stopped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("endless");
        std::fs::write(
            &program,
            "#!/bin/sh\nmkdir -p x\nwhile :; do head -c 65536 /dev/zero >> x/t1.bin; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let chdman = Chdman {
            program,
            threads: 1,
            deadline: Duration::from_secs(60),
        };
        let started = std::time::Instant::now();
        let stop = chdman
            .run_within(
                dir.path(),
                &["extractcd"],
                &|| false,
                Some((&dir.path().join("x"), 1 << 20)),
            )
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(30));
        let Stop::Failed(why) = stop else {
            panic!("{stop:?}")
        };
        assert!(why.contains("more than the 1048576 bytes"), "{why}");
    }

    /// A synthetic GD-ROM, made and extracted by the chdman here, when there is one.
    #[test]
    fn a_gd_rom_chdman_makes_fits_and_gives_back_its_cue_sheets_layout() {
        let Ok(program) = locate(None).map_err(|e| eprintln!("skipped: {e}")) else {
            if std::env::var_os("CI").is_some() {
                panic!("CI runs this test with chdman");
            }
            return;
        };
        if !program.is_file() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in");
        std::fs::create_dir(&input).unwrap();
        let mut roms = Vec::new();
        for (n, frames) in [(1u8, 300u64), (2, 225), (3, 500)] {
            let bytes: Vec<u8> = (0..2352 * frames)
                .map(|i| (i * 31 + u64::from(n)) as u8)
                .collect();
            let name = format!("t{n}.bin");
            std::fs::write(input.join(&name), &bytes).unwrap();
            roms.push(ExpectedRom {
                sha1: Some(hex(&<sha1::Sha1 as sha1::Digest>::digest(&bytes))),
                ..sized(&name, bytes.len() as u64)
            });
        }
        let cue = "REM SINGLE-DENSITY AREA\nFILE \"t1.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
                   FILE \"t2.bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n\
                   REM HIGH-DENSITY AREA\nFILE \"t3.bin\" BINARY\n  TRACK 03 MODE1/2352\n    INDEX 01 00:00:00\n";
        std::fs::write(input.join("g.cue"), cue).unwrap();
        roms.push(sized("g.cue", cue.len() as u64));
        let chdman = Chdman {
            program,
            threads: 1,
            deadline: Duration::from_secs(120),
        };
        chdman
            .create(dir.path(), ChdKind::Cd, "g.cue", &|| false)
            .unwrap();
        let bins: Vec<&ExpectedRom> = roms[..3].iter().collect();
        let made = File::open(dir.path().join("out.chd")).unwrap();
        assert_eq!(fits(&made, ChdKind::Cd, &bins), Ok(()));
        chdman
            .verify_adopted(
                dir.path(),
                ChdKind::Cd,
                Some(cue.as_bytes()),
                &roms,
                &|| false,
            )
            .unwrap();
    }

    #[test]
    fn chdman_running_past_its_deadline_is_stopped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("slow");
        std::fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let chdman = Chdman {
            program,
            threads: 1,
            deadline: Duration::from_millis(300),
        };
        let started = std::time::Instant::now();
        let stop = chdman
            .run(dir.path(), &["createcd"], &|| false)
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        let Stop::Failed(why) = stop else {
            panic!("{stop:?}")
        };
        assert!(why.contains("longer than"), "{why}");
    }

    #[test]
    fn the_deadline_grows_with_the_disc() {
        let limits = Limits {
            base: Duration::from_secs(600),
            per_gib: Duration::from_secs(120),
        };
        assert_eq!(limits.for_bytes(0), Duration::from_secs(600));
        assert_eq!(limits.for_bytes(1 << 30), Duration::from_secs(720));
        assert_eq!(limits.for_bytes(3 << 29), Duration::from_secs(780));
    }
}
