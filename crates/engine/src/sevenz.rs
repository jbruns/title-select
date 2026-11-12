//! 7z archives, read only where they arrive in an Ingest Folder (ADR 0018), and never written.
//!
//! An archive's header is read first, within a bound, and judged without decoding: one that is encrypted, uses a
//! method other than LZMA, LZMA2 or zstd (and their filters), or declares a dictionary larger than the Operator's
//! limit is refused before any dictionary is allocated. Only then is it decoded, one block at a time, inside a
//! [`Gate`] that bounds how many archives decode at once across every job; each member streams through the hasher or
//! the writer and nothing decoded is kept.

mod header;
mod zstd;

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use lzma_rust2::{Lzma2Reader, LzmaReader};
use sevenz_rust2::{Archive, ArchiveEntry, BlockDecoder, EncoderMethod, Error, Password};
use title_select_api::build::ExpectedRom;

use crate::build::{
    Hashing, NESTED_ENDINGS, Payload, Probe, Probed, ZipLimits, archive_magic, copy_sized,
    hash_all, sized, wrong_size,
};
use crate::torrentzip::{Writer, compare_names};
use header::Fault;
use zstd::{Guarded, Stop};

const SIGNATURE: [u8; 6] = *b"7z\xbc\xaf\x27\x1c";
/// Bytes from the start of the file to where a 7z's packed streams are counted from.
const SIGNATURE_HEADER: u64 = 32;
const MIB: u64 = 1 << 20;
/// The largest header read, encoded or decoded: far more than the 4096 entries a game's archive is read with need.
const HEADER_MAX: u64 = 8 * MIB;
/// Room above the dictionary limit for a decoder's own tables, and for the header the archive is described by.
const SLACK_MIB: u64 = 16;
/// The largest zstd window the decoder accepts: libzstd's default limit, which `sevenz-rust2` leaves in place.
const ZSTD_WINDOW_MAX: u64 = 1 << 27;

/// Whether `file` starts as a 7z archive. Reads without moving the file's offset.
pub fn is_7z(file: &File) -> io::Result<bool> {
    let mut head = [0; 6];
    let mut read = 0;
    while read < head.len() {
        match file.read_at(&mut head[read..], read as u64)? {
            0 => return Ok(false),
            n => read += n,
        }
    }
    Ok(head == SIGNATURE)
}

/// How many 7z archives may decode at once: one per permit, whichever job decodes them.
#[derive(Debug)]
pub struct Gate {
    permits: usize,
    busy: Mutex<usize>,
    freed: Condvar,
    most: AtomicUsize,
    headers: AtomicUsize,
    blocks: AtomicUsize,
}

/// One archive's turn to decode, given back when dropped.
#[derive(Debug)]
pub struct Permit<'a> {
    gate: &'a Gate,
}

impl Gate {
    /// A gate letting `permits` archives decode at once; at least one.
    pub fn new(permits: usize) -> Gate {
        Gate {
            permits: permits.max(1),
            busy: Mutex::new(0),
            freed: Condvar::new(),
            most: AtomicUsize::new(0),
            headers: AtomicUsize::new(0),
            blocks: AtomicUsize::new(0),
        }
    }

    /// Waits for a free permit and takes it.
    pub fn enter(&self) -> Permit<'_> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy >= self.permits {
            busy = self.freed.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy += 1;
        self.most.fetch_max(*busy, Ordering::SeqCst);
        Permit { gate: self }
    }

    /// The most permits ever taken at once.
    pub fn most_at_once(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }

    /// How many encoded headers have been decoded inside the gate.
    pub fn headers_decoded(&self) -> usize {
        self.headers.load(Ordering::SeqCst)
    }

    /// How many blocks have been decoded inside the gate, each from its start.
    pub fn blocks_decoded(&self) -> usize {
        self.blocks.load(Ordering::SeqCst)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut busy = self.gate.busy.lock().unwrap_or_else(|e| e.into_inner());
        *busy -= 1;
        self.gate.freed.notify_one();
    }
}

/// What decoding a 7z is bounded by: the gate it decodes inside, and the largest dictionary it may declare.
#[derive(Debug, Clone, Copy)]
pub struct Decoding<'a> {
    pub gate: &'a Gate,
    pub max_dictionary_mib: u32,
}

impl Decoding<'_> {
    fn limit_kb(&self) -> usize {
        ((u64::from(self.max_dictionary_mib) + SLACK_MIB) * 1024) as usize
    }
}

/// Why an archive is not decoded.
enum Judged {
    /// Its dictionary passes the limit; refused, as raising the limit would let it be read.
    TooLarge(String),
    /// It cannot be read at all: encrypted, damaged, or compressed with a method that is not read.
    Unreadable(String),
    /// It is larger, in its header, than a game's archive is read with.
    Refused(String),
}

impl Judged {
    fn why(self) -> String {
        let (Judged::TooLarge(why) | Judged::Unreadable(why) | Judged::Refused(why)) = self;
        why
    }
}

fn too_large(bytes: u64, decoding: &Decoding) -> Judged {
    Judged::TooLarge(format!(
        "this 7z needs a {} MiB dictionary, more than max_dictionary_mib ({}); unpack it yourself or raise the limit",
        bytes.div_ceil(MIB),
        decoding.max_dictionary_mib
    ))
}

fn encrypted() -> Judged {
    Judged::Unreadable("it is encrypted, and an encrypted 7z is not read".into())
}

fn damaged(what: impl std::fmt::Display) -> Judged {
    Judged::Unreadable(format!("it is damaged: {what}"))
}

fn unsupported(id: &[u8]) -> Judged {
    let method = EncoderMethod::by_id(id)
        .map(|m| m.name().to_string())
        .unwrap_or_else(|| id.iter().map(|b| format!("{b:02x}")).collect());
    Judged::Unreadable(format!(
        "it is compressed with {method}, which is not read; only LZMA, LZMA2 and zstd are"
    ))
}

fn header_too_large(bytes: u64) -> Judged {
    Judged::Refused(format!(
        "its header is {} MiB, more than the {} MiB a 7z's header is read to",
        bytes.div_ceil(MIB),
        HEADER_MAX / MIB
    ))
}

fn zstd_too_large(window: u64) -> Judged {
    Judged::Unreadable(format!(
        "it needs a {} MiB zstd window, more than the {} MiB a zstd 7z is read with, whatever \
         max_dictionary_mib says; unpack it yourself",
        window.div_ceil(MIB),
        ZSTD_WINDOW_MAX / MIB
    ))
}

/// The largest zstd window decoded within `decoding`'s limit.
fn zstd_limit(decoding: &Decoding) -> u64 {
    (u64::from(decoding.max_dictionary_mib) * MIB).min(ZSTD_WINDOW_MAX)
}

/// Why the zstd frames a [`Guarded`] reader followed stopped it, if they did.
fn stopped<R>(source: &Guarded<R>, decoding: &Decoding) -> Option<Judged> {
    Some(match source.stopped.as_ref()? {
        Stop::Window(window) if *window > ZSTD_WINDOW_MAX => zstd_too_large(*window),
        Stop::Window(window) => too_large(*window, decoding),
        Stop::Bad(why) => damaged(format!("its zstd stream cannot be read: {why}")),
    })
}

/// A 7z whose header is judged: parsed within its bounds, and declaring no dictionary past the limit.
struct Judgement {
    archive: Archive,
    /// Where each zstd packed stream starts and ends in the file, to follow its frames as it is decoded.
    zstd: Vec<(u64, u64)>,
}

impl Judgement {
    /// Reads `source` from the file, following the frames of every zstd stream in it.
    fn guard<R: Read + Seek>(&self, source: R, decoding: &Decoding) -> io::Result<Guarded<R>> {
        Guarded::new(source, &self.zstd, zstd_limit(decoding))
    }

    /// Where member `i` is decoded: its block, if it has one.
    fn block_of(&self, i: usize) -> Option<usize> {
        self.archive.stream_map.file_block_index[i]
    }
}

/// Reads `file`'s 7z header and judges it before any block is decoded.
///
/// The header is read here rather than by `sevenz-rust2`, in three bounds. Its size is checked before it is read: at
/// most [`HEADER_MAX`], encoded or decoded. An encoded header is decoded here, within the dictionary limit, under the
/// permit its caller already holds. Then the plain header is walked as far as its count of files, and refused if any
/// count in it passes `members`. Only then does the crate parse it, from the plain bytes, never decoding anything
/// itself. So what the header phase holds at once is at most the raw header, the decoded one and the crate's copy of
/// it, three times [`HEADER_MAX`], beside one dictionary within the limit; and the entries the crate parses are at
/// most `members` files, blocks, packed streams and substreams, each block of at most four coders.
fn judge(file: &File, members: usize, decoding: &Decoding) -> Result<Judgement, Judged> {
    let len = file.metadata().map_err(damaged)?.len();
    let mut start = [0; SIGNATURE_HEADER as usize];
    file.read_exact_at(&mut start, 0).map_err(|_| {
        Judged::Unreadable("it is not a readable 7z archive: it is too short".into())
    })?;
    if start[..6] != SIGNATURE || start[6] != 0 {
        return Err(Judged::Unreadable(
            "it is not a readable 7z archive: its signature or version is not read".into(),
        ));
    }
    if crc32fast::hash(&start[12..]) != u32::from_le_bytes(start[8..12].try_into().unwrap()) {
        return Err(damaged("its start header's checksum does not match"));
    }
    let offset = u64::from_le_bytes(start[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(start[20..28].try_into().unwrap());
    let crc = u32::from_le_bytes(start[28..32].try_into().unwrap());
    if size > HEADER_MAX {
        return Err(header_too_large(size));
    }
    let at = SIGNATURE_HEADER
        .checked_add(offset)
        .filter(|at| at.checked_add(size).is_some_and(|end| end <= len))
        .ok_or_else(|| damaged("its header lies past its end"))?;
    let mut raw = vec![0; size as usize];
    file.read_exact_at(&mut raw, at).map_err(damaged)?;
    if crc32fast::hash(&raw) != crc {
        return Err(damaged("its header's checksum does not match"));
    }
    let plain = if raw.first() == Some(&header::K_ENCODED_HEADER) {
        decode_header(file, &raw, len, decoding)?
    } else {
        raw
    };
    let counts = header::counts(&plain, members).map_err(|fault| match fault {
        Fault::TooMany(n) => Judged::Refused(format!(
            "it lists {n} entries, more than the {members} a game's archive is read with"
        )),
        Fault::Damaged(what) => damaged(what),
    })?;
    let mut plain = Plain::new(offset, plain);
    let archive = match Archive::read_with_memory_limit(
        &mut plain,
        &Password::empty(),
        decoding.limit_kb(),
    ) {
        Ok(archive) => archive,
        Err(Error::PasswordRequired | Error::MaybeBadPassword(_)) => return Err(encrypted()),
        Err(e) => {
            return Err(Judged::Unreadable(format!(
                "it is not a readable 7z archive: {e}"
            )));
        }
    };
    if archive.files.len() != counts.files || archive.blocks.len() != counts.folders {
        return Err(damaged("its header lists its entries two ways"));
    }

    let mut largest = 0u64;
    let mut window = 0u64;
    let mut zstd = Vec::new();
    for (b, block) in archive.blocks.iter().enumerate() {
        let mut needs = 0u64;
        for coder in &block.coders {
            let id = coder.encoder_method_id();
            let size = dictionary(&archive, file, b, id, coder.properties())?;
            if id == EncoderMethod::ID_ZSTD {
                window = window.max(size);
            }
            needs = needs.saturating_add(size);
        }
        largest = largest.max(needs);
        if let Some(stream) = zstd_stream(&archive, b)? {
            zstd.push(stream);
        }
    }
    if largest > u64::from(decoding.max_dictionary_mib) * MIB {
        return Err(too_large(largest, decoding));
    }
    if window > ZSTD_WINDOW_MAX {
        return Err(zstd_too_large(window));
    }
    Ok(Judgement { archive, zstd })
}

/// Where block `b`'s packed stream lies in the file, if the block is zstd. A zstd coder is read only as the one coder
/// reading the block's one packed stream, so that its frames are followed as they are read.
fn zstd_stream(archive: &Archive, b: usize) -> Result<Option<(u64, u64)>, Judged> {
    let block = &archive.blocks[b];
    let is_zstd = |c: &sevenz_rust2::Coder| c.encoder_method_id() == EncoderMethod::ID_ZSTD;
    let zstd = block.coders.iter().filter(|c| is_zstd(c)).count();
    if zstd == 0 {
        return Ok(None);
    }
    let map = &archive.stream_map;
    let first = map.block_first_pack_stream_index()[b];
    let next = map
        .block_first_pack_stream_index()
        .get(b + 1)
        .copied()
        .unwrap_or(archive.pack_sizes().len());
    let reads_packed = block
        .ordered_coder_iter()
        .next()
        .is_some_and(|(_, c)| is_zstd(c));
    if zstd != 1 || next != first + 1 || !reads_packed {
        return Err(Judged::Unreadable(
            "it chains zstd after another coder, which is not read".into(),
        ));
    }
    let start =
        pack_start(archive, first).ok_or_else(|| damaged("its packed streams lie past its end"))?;
    let end = start
        .checked_add(archive.pack_sizes()[first])
        .ok_or_else(|| damaged("its packed streams lie past its end"))?;
    Ok(Some((start, end)))
}

/// Where packed stream `stream` starts in the file.
fn pack_start(archive: &Archive, stream: usize) -> Option<u64> {
    SIGNATURE_HEADER
        .checked_add(archive.pack_pos())?
        .checked_add(*archive.stream_map.pack_stream_offsets().get(stream)?)
}

/// Decodes `raw`, an encoded header of `file`, `len` bytes long: one block of one coder, decoded within the dictionary
/// limit to at most [`HEADER_MAX`] bytes.
fn decode_header(
    file: &File,
    raw: &[u8],
    len: u64,
    decoding: &Decoding,
) -> Result<Vec<u8>, Judged> {
    let mut c = header::Cursor::new(raw);
    c.byte().map_err(|_| damaged("its header ends early"))?;
    let streams = header::streams(&mut c, 1).map_err(|fault| match fault {
        Fault::TooMany(_) => damaged("its encoded header lists more than one stream"),
        Fault::Damaged(what) => damaged(what),
    })?;
    let ([folder], [packed]) = (streams.folders.as_slice(), streams.pack_sizes.as_slice()) else {
        return Err(damaged("its encoded header lists other than one stream"));
    };
    let ([coder], [unpack]) = (folder.coders.as_slice(), folder.unpack.as_slice()) else {
        return Err(Judged::Unreadable(
            "its header is encoded with more than one coder, which is not read".into(),
        ));
    };
    if !coder.simple {
        return Err(unsupported(coder.id));
    }
    let unpack = *unpack;
    if unpack > HEADER_MAX {
        return Err(header_too_large(unpack));
    }
    let start = SIGNATURE_HEADER
        .checked_add(streams.pack_pos)
        .filter(|start| start.checked_add(*packed).is_some_and(|end| end <= len))
        .ok_or_else(|| damaged("its encoded header lies past its end"))?;
    let end = start + packed;
    let dict = match coder.id {
        EncoderMethod::ID_LZMA => lzma_dictionary(coder.props)?,
        EncoderMethod::ID_LZMA2 => lzma2_dictionary(coder.props)?,
        EncoderMethod::ID_COPY | EncoderMethod::ID_ZSTD => 0,
        EncoderMethod::ID_AES256_SHA256 => return Err(encrypted()),
        other => return Err(unsupported(other)),
    };
    if dict > u64::from(decoding.max_dictionary_mib) * MIB {
        return Err(too_large(dict, decoding));
    }

    decoding.gate.headers.fetch_add(1, Ordering::SeqCst);
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start)).map_err(damaged)?;
    let zstd = if coder.id == EncoderMethod::ID_ZSTD {
        vec![(start, end)]
    } else {
        Vec::new()
    };
    let mut source = Guarded::new(reader, &zstd, zstd_limit(decoding)).map_err(damaged)?;
    let mut plain = Vec::with_capacity(unpack as usize);
    let read = {
        let packed = (&mut source).take(*packed);
        let decoder: io::Result<Box<dyn Read + '_>> = match coder.id {
            EncoderMethod::ID_LZMA => {
                LzmaReader::new_with_props(packed, unpack, coder.props[0], dict as u32, None)
                    .map(|r| Box::new(r) as Box<dyn Read>)
            }
            EncoderMethod::ID_LZMA2 => Ok(Box::new(Lzma2Reader::new(packed, dict as u32, None))),
            EncoderMethod::ID_ZSTD => {
                ::zstd::stream::read::Decoder::new(packed).and_then(|mut d| {
                    d.window_log_max(window_log_max(decoding))?;
                    Ok(Box::new(d) as Box<dyn Read>)
                })
            }
            _ => Ok(Box::new(packed)),
        };
        decoder.and_then(|d| d.take(unpack + 1).read_to_end(&mut plain))
    };
    if let Some(judged) = stopped(&source, decoding) {
        return Err(judged);
    }
    read.map_err(|e| damaged(format!("its header cannot be decoded: {e}")))?;
    if plain.len() as u64 != unpack {
        return Err(damaged("its header decodes to other than its size"));
    }
    if folder.crc.is_some_and(|crc| crc != crc32fast::hash(&plain)) {
        return Err(damaged("its header's checksum does not match"));
    }
    if plain.first() != Some(&header::K_HEADER) {
        return Err(damaged("its encoded header decodes to no header"));
    }
    Ok(plain)
}

/// The window log a zstd decoder of a header is held to, as a backstop to following its frames: the least power of
/// two at or past the limit. The frames themselves are held to the limit exactly.
fn window_log_max(decoding: &Decoding) -> u32 {
    let limit = zstd_limit(decoding).max(1 << 10);
    64 - (limit - 1).leading_zeros()
}

/// A 7z with its header replaced by `plain`, decoded, as the crate is given it: a start header pointing to `plain`,
/// zeros where the packed streams are, and `plain`. Nothing but the header is ever read from it.
struct Plain {
    start: [u8; SIGNATURE_HEADER as usize],
    at: u64,
    plain: Vec<u8>,
    pos: u64,
}

impl Plain {
    fn new(offset: u64, plain: Vec<u8>) -> Plain {
        let mut start = [0; SIGNATURE_HEADER as usize];
        start[..6].copy_from_slice(&SIGNATURE);
        start[7] = 4;
        start[12..20].copy_from_slice(&offset.to_le_bytes());
        start[20..28].copy_from_slice(&(plain.len() as u64).to_le_bytes());
        start[28..32].copy_from_slice(&crc32fast::hash(&plain).to_le_bytes());
        let crc = crc32fast::hash(&start[12..]);
        start[8..12].copy_from_slice(&crc.to_le_bytes());
        Plain {
            start,
            at: SIGNATURE_HEADER + offset,
            plain,
            pos: 0,
        }
    }

    fn len(&self) -> u64 {
        self.at + self.plain.len() as u64
    }
}

impl Read for Plain {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.pos;
        let n = if pos < SIGNATURE_HEADER {
            let from = &self.start[pos as usize..];
            let n = from.len().min(buf.len());
            buf[..n].copy_from_slice(&from[..n]);
            n
        } else if pos < self.at {
            let n = (self.at - pos).min(buf.len() as u64) as usize;
            buf[..n].fill(0);
            n
        } else if pos < self.len() {
            let from = &self.plain[(pos - self.at) as usize..];
            let n = from.len().min(buf.len());
            buf[..n].copy_from_slice(&from[..n]);
            n
        } else {
            0
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Plain {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let to = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len().checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos =
            to.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek out of range"))?;
        Ok(self.pos)
    }
}

fn lzma_dictionary(props: &[u8]) -> Result<u64, Judged> {
    let dict = props
        .get(1..5)
        .ok_or_else(|| damaged("its LZMA properties are too short"))?;
    Ok(u64::from(u32::from_le_bytes(dict.try_into().unwrap())))
}

fn lzma2_dictionary(props: &[u8]) -> Result<u64, Judged> {
    match props.first() {
        Some(40) => Ok(u64::from(u32::MAX)),
        Some(&p) if p < 40 => Ok((2 | u64::from(p & 1)) << (p / 2 + 11)),
        _ => Err(damaged("its LZMA2 dictionary size is not valid")),
    }
}

/// The dictionary, or window, the coder `id` with `props` of block `b` declares; zero for a filter. A zstd coder's is
/// its first frame's window, read from the file; every later frame's is followed as it is decoded.
fn dictionary(
    archive: &Archive,
    file: &File,
    b: usize,
    id: &[u8],
    props: &[u8],
) -> Result<u64, Judged> {
    match id {
        EncoderMethod::ID_LZMA => lzma_dictionary(props),
        EncoderMethod::ID_LZMA2 => lzma2_dictionary(props),
        EncoderMethod::ID_ZSTD => zstd_window(archive, file, b)
            .map_err(|e| damaged(format!("its zstd frame header cannot be read: {e}"))),
        EncoderMethod::ID_COPY
        | EncoderMethod::ID_DELTA
        | EncoderMethod::ID_BCJ_X86
        | EncoderMethod::ID_BCJ_ARM
        | EncoderMethod::ID_BCJ_ARM64
        | EncoderMethod::ID_BCJ_ARM_THUMB
        | EncoderMethod::ID_BCJ_PPC
        | EncoderMethod::ID_BCJ_IA64
        | EncoderMethod::ID_BCJ_SPARC
        | EncoderMethod::ID_BCJ_RISCV => Ok(0),
        EncoderMethod::ID_AES256_SHA256 => Err(encrypted()),
        other => Err(unsupported(other)),
    }
}

/// The window the first zstd frame of block `b`'s packed stream declares, read from its frame header.
fn zstd_window(archive: &Archive, file: &File, b: usize) -> io::Result<u64> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let stream = *archive
        .stream_map
        .block_first_pack_stream_index()
        .get(b)
        .ok_or_else(|| bad("no packed stream"))?;
    let mut at = pack_start(archive, stream).ok_or_else(|| bad("no packed stream"))?;
    // Skippable frames carry no data; a few are passed over to reach the first real frame.
    for _ in 0..16 {
        let mut head = [0; 18];
        file.read_exact_at(&mut head[..8], at)?;
        let magic = u32::from_le_bytes(head[..4].try_into().unwrap());
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            at += 8 + u64::from(u32::from_le_bytes(head[4..8].try_into().unwrap()));
            continue;
        }
        if magic != 0xFD2F_B528 {
            return Err(bad("it is not a zstd frame"));
        }
        let fhd = head[4];
        let single = fhd & 0x20 != 0;
        let dict_id = [0, 1, 2, 4][usize::from(fhd & 3)];
        let fcs = match fhd >> 6 {
            0 => usize::from(single),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let len = 5 + usize::from(!single) + dict_id + fcs;
        let _ = file.read_at(&mut head[8..len.max(8)], at + 8)?;
        if !single {
            let wd = head[5];
            let base = 1u64 << (10 + u32::from(wd >> 3));
            return Ok(base + base / 8 * u64::from(wd & 7));
        }
        let field = &head[5 + dict_id..len];
        let mut size = [0; 8];
        size[..field.len()].copy_from_slice(field);
        let size = u64::from_le_bytes(size);
        return Ok(if fcs == 2 { size + 256 } else { size });
    }
    Err(bad("too many skippable frames"))
}

/// Why decoding a block failed: its zstd frames, the crate's own dictionary check, or damage.
fn decode_failed<R>(e: Error, source: &Guarded<R>, decoding: &Decoding) -> Judged {
    if let Some(judged) = stopped(source, decoding) {
        return judged;
    }
    match e {
        Error::MaxMemLimited { actaul_kb, .. } => too_large(actaul_kb as u64 * 1024, decoding),
        Error::PasswordRequired | Error::MaybeBadPassword(_) => encrypted(),
        e => damaged(e),
    }
}

/// What a block's members are handed to, each with its index; it returns whether to go on.
type Each<'e> = dyn FnMut(usize, &ArchiveEntry, &mut dyn Read) -> Result<bool, Error> + 'e;

/// Decodes block `b` of `judged` once, from `source`, handing each of its members, by index, to `each`, until `each`
/// returns false.
fn decode_block<R: Read + Seek>(
    judged: &Judgement,
    b: usize,
    source: &mut Guarded<R>,
    decoding: &Decoding,
    each: &mut Each,
) -> Result<(), Judged> {
    decoding.gate.blocks.fetch_add(1, Ordering::SeqCst);
    let mut index = judged.archive.stream_map.block_first_file_index[b];
    let password = Password::empty();
    let decoder = BlockDecoder::new_with_memory_limit(
        1,
        decoding.limit_kb(),
        b,
        &judged.archive,
        &password,
        source,
    );
    let decoded = decoder.for_each_entries(&mut |entry, member| {
        let i = index;
        index += 1;
        each(i, entry, member)
    });
    decoded
        .map(|_| ())
        .map_err(|e| decode_failed(e, source, decoding))
}

/// Reads `file`, a 7z archive, from its start: judges its header, hashes it whole, then decodes each member within
/// `decoding`'s bounds and hashes it, holding none of it. One permit of the gate is held throughout, from before the
/// header is read. As [`crate::build::probe_within`] does for a ZIP, an archive holding another archive, by name or
/// by its first bytes, or expanding past `limits`, is refused, and so is one whose header lists more than `limits`
/// allow or whose dictionary passes the limit, unread. One that is encrypted, damaged or uses a method that is not
/// read is read with the reason in its error.
pub fn probe(file: &File, limits: &ZipLimits, decoding: &Decoding) -> io::Result<Probed> {
    let _permit = decoding.gate.enter();
    let judged = match judge(file, limits.members, decoding) {
        Err(Judged::TooLarge(why) | Judged::Refused(why)) => return Ok(Probed::Refused(why)),
        judged => judged,
    };
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(0))?;
    let whole = hash_all(&mut reader)?;
    let mut probe = Probe {
        sha256: whole.sha256,
        size: whole.size,
        zip: false,
        torrentzip: false,
        sevenz: true,
        payloads: Vec::new(),
        error: None,
        header_rule: None,
    };
    let judged = match judged {
        Ok(judged) => judged,
        Err(judged) => {
            probe.error = Some(judged.why());
            return Ok(Probed::Read(probe));
        }
    };
    let archive = &judged.archive;
    let members: Vec<&ArchiveEntry> = archive.files.iter().filter(|f| !f.is_directory).collect();
    let mut declared = 0u64;
    for member in &members {
        let name = &member.name;
        if let Some((_, ext)) = name.rsplit_once('.')
            && NESTED_ENDINGS.contains(&ext.to_ascii_lowercase().as_str())
        {
            return Ok(Probed::Refused(format!(
                "it holds another archive, {name}; nested archives are not opened"
            )));
        }
        if member.size > limits.member_bytes {
            return Ok(Probed::Refused(format!(
                "{name} expands to {} bytes, more than the {} one member is read to",
                member.size, limits.member_bytes
            )));
        }
        declared = declared.saturating_add(member.size);
    }
    if declared > limits.total_bytes {
        return Ok(Probed::Refused(format!(
            "it expands to {declared} bytes, more than the {} a game's archive is read to",
            limits.total_bytes
        )));
    }

    let mut read: Vec<Option<Payload>> = vec![None; archive.files.len()];
    let mut refused = None;
    let mut source = judged.guard(BufReader::new(file), decoding)?;
    for b in 0..archive.blocks.len() {
        let decoded = decode_block(
            &judged,
            b,
            &mut source,
            decoding,
            &mut |i, entry, member| {
                if entry.is_directory {
                    return Ok(true);
                }
                let mut head = Vec::with_capacity(8);
                member.take(8).read_to_end(&mut head)?;
                if let Some(format) = archive_magic(&head) {
                    refused = Some(format!(
                        "it holds another archive, {}, which reads as {format}; nested archives are not opened",
                        entry.name
                    ));
                    return Ok(false);
                }
                let mut bounded = io::Cursor::new(head)
                    .chain(member)
                    .take(entry.size.saturating_add(1));
                let sums = hash_all(&mut bounded)?;
                if sums.size > entry.size {
                    refused = Some(format!(
                        "{} expands to more than its archive says; it was not read further",
                        entry.name
                    ));
                    return Ok(false);
                }
                read[i] = Some(Payload::new(Some(entry.name.clone()), sums));
                Ok(true)
            },
        );
        if let Some(why) = refused {
            return Ok(Probed::Refused(why));
        }
        match decoded {
            Ok(()) => {}
            Err(Judged::TooLarge(why) | Judged::Refused(why)) => return Ok(Probed::Refused(why)),
            Err(Judged::Unreadable(why)) => {
                probe.error = Some(why);
                return Ok(Probed::Read(probe));
            }
        }
    }
    for (i, file) in archive.files.iter().enumerate() {
        if file.is_directory {
            continue;
        }
        let payload = match read[i].take() {
            Some(payload) => payload,
            None if judged.block_of(i).is_none() => {
                Payload::new(Some(file.name.clone()), hash_all(&mut io::empty())?)
            }
            None => {
                probe.error = Some(format!("it is damaged: {} was not decoded", file.name));
                return Ok(Probed::Read(probe));
            }
        };
        probe.payloads.push(payload);
    }
    Ok(Probed::Read(probe))
}

/// The source a 7z is written from: read through a hasher, its zstd frames followed.
type Source<'a> = Guarded<BufReader<Hashing<'a>>>;

/// Judges `source`'s header and opens it to write from, under the permit its caller holds.
fn open<'a>(source: &'a File, decoding: &Decoding) -> Result<(Judgement, Source<'a>), String> {
    let judged = judge(source, ZipLimits::default().members, decoding).map_err(Judged::why)?;
    let hashing = Hashing::new(source).map_err(|e| e.to_string())?;
    let source = judged
        .guard(BufReader::new(hashing), decoding)
        .map_err(|e| e.to_string())?;
    Ok((judged, source))
}

/// Hashes the rest of `source`, as it was not read, and returns its SHA-256 and size.
fn finish(source: Source) -> Result<(String, u64), String> {
    source
        .into_inner()
        .into_inner()
        .finish()
        .map_err(|e| e.to_string())
}

/// The index of member `name` of `archive`.
fn member_index(archive: &Archive, name: &str) -> Result<usize, String> {
    archive
        .files
        .iter()
        .position(|f| f.name == name && !f.is_directory)
        .ok_or_else(|| format!("{name}: it is not in the archive"))
}

/// Writes `roms` to `dest`, a new, empty file the caller created, as a TorrentZip, each ROM read from the named
/// member of `source`, a 7z archive, within `decoding`'s bounds. Returns the SHA-256 and size of `source`, hashed as
/// it was read. Checksums are checked by [`crate::build::verify`] afterwards.
///
/// One permit of the gate is held throughout, from before the header is read. Each block holding a requested member
/// is decoded once, in the archive's order, while TorrentZip writes members in its own: a member decoded before its
/// turn, or requested more than once, waits in `scratch`, an empty directory the caller made and removes, read no
/// further than its declared size, and is removed once written.
pub fn repack(
    dest: &File,
    roms: &[(&ExpectedRom, &str)],
    source: &File,
    decoding: &Decoding,
    scratch: &Path,
) -> Result<(String, u64), String> {
    let _permit = decoding.gate.enter();
    let (judged, mut source) = open(source, decoding)?;
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let wanted = order
        .iter()
        .map(|(_, member)| member_index(&judged.archive, member))
        .collect::<Result<Vec<_>, _>>()?;
    let mut writer = Writer::new(BufWriter::new(dest)).map_err(|e| e.to_string())?;
    let mut turns = Turns {
        order: &order,
        wanted: &wanted,
        next: 0,
        staged: HashMap::new(),
        scratch,
    };
    turns.drain(&judged, &mut writer)?;
    let blocks: BTreeSet<usize> = wanted.iter().filter_map(|&i| judged.block_of(i)).collect();
    for b in blocks {
        let last = wanted
            .iter()
            .copied()
            .filter(|&i| judged.block_of(i) == Some(b))
            .max()
            .unwrap_or(0);
        let mut failed = None;
        decode_block(
            &judged,
            b,
            &mut source,
            decoding,
            &mut |i, entry, member| {
                if wanted.contains(&i)
                    && let Err(why) = turns.take(i, entry, member, &judged, &mut writer)
                {
                    failed = Some(why);
                    return Ok(false);
                }
                Ok(i < last)
            },
        )
        .map_err(Judged::why)?;
        if let Some(why) = failed {
            return Err(stopped(&source, decoding).map_or(why, Judged::why));
        }
    }
    if let Some(&(rom, member)) = order.get(turns.next) {
        return Err(format!("{}: {member} was not decoded", rom.name));
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    finish(source)
}

type ZipOut<'a> = Writer<BufWriter<&'a File>>;

/// Which of a repack's ROMs is written next, in TorrentZip's order, and the members waiting in scratch for theirs.
struct Turns<'r> {
    order: &'r [&'r (&'r ExpectedRom, &'r str)],
    /// The member index each ROM of `order` is read from.
    wanted: &'r [usize],
    next: usize,
    /// Members decoded before their turn: each one's scratch file, and how many ROMs are still to be read from it.
    staged: HashMap<usize, (PathBuf, usize)>,
    scratch: &'r Path,
}

impl Turns<'_> {
    /// Takes member `i`, decoded now: writes it if it is next and read once, else stages it.
    fn take(
        &mut self,
        i: usize,
        entry: &ArchiveEntry,
        member: &mut dyn Read,
        judged: &Judgement,
        writer: &mut ZipOut,
    ) -> Result<(), String> {
        let uses = self.wanted.iter().filter(|&&w| w == i).count();
        if uses == 1 && self.wanted.get(self.next) == Some(&i) {
            self.write(entry.size, member, writer)?;
        } else {
            let path = self.scratch.join(i.to_string());
            let mut file = File::create_new(&path).map_err(|e| format!("{}: {e}", entry.name))?;
            let copied = io::copy(&mut member.take(entry.size.saturating_add(1)), &mut file)
                .map_err(|e| format!("{}: {e}", entry.name))?;
            if copied != entry.size {
                return Err(format!(
                    "{}: it expands to other than its archive says; it was not read further",
                    entry.name
                ));
            }
            self.staged.insert(i, (path, uses));
        }
        self.drain(judged, writer)
    }

    /// Writes the next ROM from `payload`, `len` bytes long.
    fn write(
        &mut self,
        len: u64,
        payload: &mut dyn Read,
        writer: &mut ZipOut,
    ) -> Result<(), String> {
        let rom = self.order[self.next].0;
        let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", rom.name);
        let (size, mut payload) = sized(rom, len, payload).map_err(|e| failed(&e))?;
        writer
            .add(&rom.name, size, &mut payload)
            .map_err(|e| failed(&e))?;
        self.next += 1;
        Ok(())
    }

    /// Writes every ROM whose turn has come and whose member is staged or empty, without a block.
    fn drain(&mut self, judged: &Judgement, writer: &mut ZipOut) -> Result<(), String> {
        while let Some(&i) = self.wanted.get(self.next) {
            if judged.block_of(i).is_none() {
                self.write(0, &mut io::empty(), writer)?;
                continue;
            }
            let Some((path, uses)) = self.staged.get_mut(&i) else {
                return Ok(());
            };
            let name = &self.order[self.next].0.name;
            let mut file = File::open(&*path).map_err(|e| format!("{name}: {e}"))?;
            let len = file.metadata().map_err(|e| format!("{name}: {e}"))?.len();
            *uses -= 1;
            if *uses == 0 {
                let (path, _) = self.staged.remove(&i).unwrap();
                std::fs::remove_file(&path).map_err(|e| format!("{name}: {e}"))?;
            }
            self.write(len, &mut file, writer)?;
        }
        Ok(())
    }
}

/// Writes `rom` into `dest`, a new, empty file the caller created, as a loose file: its payload, read from member
/// `member` of `source`, a 7z archive, within `decoding`'s bounds. Returns the SHA-256 and size of `source`, hashed
/// as it was read. One permit of the gate is held throughout, from before the header is read, and the member's block
/// is decoded once. A payload of the wrong size is refused; checksums are checked by
/// [`crate::build::verify_loose`] afterwards.
pub fn write_loose(
    dest: &File,
    rom: &ExpectedRom,
    member: &str,
    source: &File,
    decoding: &Decoding,
) -> Result<(String, u64), String> {
    let _permit = decoding.gate.enter();
    let (judged, mut source) = open(source, decoding)?;
    let wanted = member_index(&judged.archive, member)?;
    let mut out = BufWriter::new(dest);
    let mut copied = None;
    match judged.block_of(wanted) {
        None => copied = Some(copy_sized(rom, 0, io::empty(), &mut out)),
        Some(b) => decode_block(
            &judged,
            b,
            &mut source,
            decoding,
            &mut |i, entry, payload| {
                if i != wanted {
                    return Ok(true);
                }
                copied = Some(copy_sized(rom, entry.size, payload, &mut out));
                Ok(false)
            },
        )
        .map_err(Judged::why)?,
    }
    let copied = copied.ok_or_else(|| format!("{member}: it was not decoded"))?;
    let (copied, size) =
        copied.map_err(|why| stopped(&source, decoding).map_or(why, Judged::why))?;
    if copied != size {
        return Err(wrong_size(rom, copied, size));
    }
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    finish(source)
}
