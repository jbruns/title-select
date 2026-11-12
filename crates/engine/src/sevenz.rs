//! 7z archives, read only where they arrive in an Ingest Folder (ADR 0018), and never written.
//!
//! An archive's header is read first, within a bound, and judged without decoding: one that is encrypted, uses a
//! method other than LZMA, LZMA2 or zstd (and their filters), or declares a dictionary larger than the Operator's
//! limit is refused before any dictionary is allocated. Only then is it decoded, one block at a time, inside a
//! [`Gate`] that bounds how many archives decode at once across every job; each member streams through the hasher or
//! the writer and nothing decoded is kept.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use sevenz_rust2::{Archive, ArchiveEntry, BlockDecoder, EncoderMethod, Error, Password};
use title_select_api::build::ExpectedRom;

use crate::build::{
    Hashing, NESTED_ENDINGS, Payload, Probe, Probed, ZipLimits, archive_magic, copy_sized,
    hash_all, sized, wrong_size,
};
use crate::torrentzip::{Writer, compare_names};

const SIGNATURE: [u8; 6] = *b"7z\xbc\xaf\x27\x1c";
/// Bytes from the start of the file to where a 7z's packed streams are counted from.
const SIGNATURE_HEADER: u64 = 32;
const MIB: u64 = 1 << 20;
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

/// Why an archive's header, read but not decoded, is not decoded.
enum Judged {
    /// Its dictionary passes the limit; refused, as raising the limit would let it be read.
    TooLarge(String),
    /// It cannot be read at all: encrypted, damaged, or compressed with a method that is not read.
    Unreadable(String),
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

/// Reads `file`'s 7z header, within the limit, and judges it before anything is decoded.
fn judge(file: &File, decoding: &Decoding) -> Result<Archive, Judged> {
    let mut reader = BufReader::new(file);
    let archive =
        match Archive::read_with_memory_limit(&mut reader, &Password::empty(), decoding.limit_kb())
        {
            Ok(archive) => archive,
            Err(Error::PasswordRequired | Error::MaybeBadPassword(_)) => return Err(encrypted()),
            Err(Error::MaxMemLimited { actaul_kb, .. }) => {
                return Err(too_large(actaul_kb as u64 * 1024, decoding));
            }
            Err(e) => {
                return Err(Judged::Unreadable(format!(
                    "it is not a readable 7z archive: {e}"
                )));
            }
        };
    let mut largest = 0u64;
    let mut zstd = 0u64;
    for (b, block) in archive.blocks.iter().enumerate() {
        let mut needs = 0u64;
        for coder in &block.coders {
            let id = coder.encoder_method_id();
            let size = dictionary(&archive, file, b, id, coder.properties())?;
            if id == EncoderMethod::ID_ZSTD {
                zstd = zstd.max(size);
            }
            needs = needs.saturating_add(size);
        }
        largest = largest.max(needs);
    }
    if largest > u64::from(decoding.max_dictionary_mib) * MIB {
        return Err(too_large(largest, decoding));
    }
    if zstd > ZSTD_WINDOW_MAX {
        return Err(Judged::Unreadable(format!(
            "it needs a {} MiB zstd window, more than the {} MiB a zstd 7z is read with, whatever \
             max_dictionary_mib says; unpack it yourself",
            zstd.div_ceil(MIB),
            ZSTD_WINDOW_MAX / MIB
        )));
    }
    Ok(archive)
}

/// The dictionary, or window, the coder `id` with `props` of block `b` declares; zero for a filter.
fn dictionary(
    archive: &Archive,
    file: &File,
    b: usize,
    id: &[u8],
    props: &[u8],
) -> Result<u64, Judged> {
    let damaged = |what: &str| Judged::Unreadable(format!("it is damaged: {what}"));
    match id {
        EncoderMethod::ID_LZMA => {
            let dict = props
                .get(1..5)
                .ok_or_else(|| damaged("its LZMA properties are too short"))?;
            Ok(u64::from(u32::from_le_bytes(dict.try_into().unwrap())))
        }
        EncoderMethod::ID_LZMA2 => match props.first() {
            Some(40) => Ok(u64::from(u32::MAX)),
            Some(&p) if p < 40 => Ok((2 | u64::from(p & 1)) << (p / 2 + 11)),
            _ => Err(damaged("its LZMA2 dictionary size is not valid")),
        },
        EncoderMethod::ID_ZSTD => zstd_window(archive, file, b)
            .map_err(|e| damaged(&format!("its zstd frame header cannot be read: {e}"))),
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
        other => {
            let method = EncoderMethod::by_id(other)
                .map(|m| m.name().to_string())
                .unwrap_or_else(|| other.iter().map(|b| format!("{b:02x}")).collect());
            Err(Judged::Unreadable(format!(
                "it is compressed with {method}, which is not read; only LZMA, LZMA2 and zstd are"
            )))
        }
    }
}

/// The window the first zstd frame of block `b`'s packed stream declares, read from its frame header.
fn zstd_window(archive: &Archive, file: &File, b: usize) -> io::Result<u64> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let map = &archive.stream_map;
    let stream = *map
        .block_first_pack_stream_index()
        .get(b)
        .ok_or_else(|| bad("no packed stream"))?;
    let offset = *map
        .pack_stream_offsets()
        .get(stream)
        .ok_or_else(|| bad("no packed stream"))?;
    let mut at = SIGNATURE_HEADER + archive.pack_pos() + offset;
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

/// Reads `file`, a 7z archive, from its start: judges its header, hashes it whole, then decodes each member within
/// `decoding`'s bounds and hashes it, holding none of it. As [`crate::build::probe_within`] does for a ZIP, an
/// archive holding another archive, by name or by its first bytes, or expanding past `limits`, is refused, and so is
/// one whose dictionary passes the limit, unread. One that is encrypted, damaged or uses a method that is not read
/// is read with the reason in its error.
pub fn probe(file: &File, limits: &ZipLimits, decoding: &Decoding) -> io::Result<Probed> {
    let judged = judge(file, decoding);
    if let Err(Judged::TooLarge(why)) = judged {
        return Ok(Probed::Refused(why));
    }
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
    let archive = match judged {
        Ok(archive) => archive,
        Err(Judged::TooLarge(why) | Judged::Unreadable(why)) => {
            probe.error = Some(why);
            return Ok(Probed::Read(probe));
        }
    };
    let members: Vec<&ArchiveEntry> = archive.files.iter().filter(|f| !f.is_directory).collect();
    if members.len() > limits.members {
        return Ok(Probed::Refused(format!(
            "it has {} members, more than the {} a game's archive is read with",
            members.len(),
            limits.members
        )));
    }
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

    let _permit = decoding.gate.enter();
    let mut read: Vec<Option<Payload>> = vec![None; archive.files.len()];
    let mut refused = None;
    let mut source = BufReader::new(file);
    let password = Password::empty();
    for b in 0..archive.blocks.len() {
        let mut index = archive.stream_map.block_first_file_index[b];
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            decoding.limit_kb(),
            b,
            &archive,
            &password,
            &mut source,
        );
        let decoded = decoder.for_each_entries(&mut |entry, member| {
            let i = index;
            index += 1;
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
            let mut bounded = io::Cursor::new(head).chain(member).take(entry.size.saturating_add(1));
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
        });
        if let Some(why) = refused {
            return Ok(Probed::Refused(why));
        }
        match decoded {
            Ok(_) => {}
            Err(Error::MaxMemLimited { actaul_kb, .. }) => {
                let (Judged::TooLarge(why) | Judged::Unreadable(why)) =
                    too_large(actaul_kb as u64 * 1024, decoding);
                return Ok(Probed::Refused(why));
            }
            Err(e) => {
                probe.error = Some(format!("it is damaged: {e}"));
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
            None if archive.stream_map.file_block_index[i].is_none() => {
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

/// A 7z opened to write from: its judged header, and the source read through a hasher.
struct Opened<'a> {
    archive: Archive,
    source: BufReader<Hashing<'a>>,
}

fn open<'a>(source: &'a File, decoding: &Decoding) -> Result<Opened<'a>, String> {
    let archive =
        judge(source, decoding).map_err(|(Judged::TooLarge(why) | Judged::Unreadable(why))| why)?;
    let source = BufReader::new(Hashing::new(source).map_err(|e| e.to_string())?);
    Ok(Opened { archive, source })
}

impl Opened<'_> {
    /// Decodes member `name` and hands it, with its size, to `write`. Only its block is decoded, from the block's
    /// start, as far as the member.
    fn with_member(
        &mut self,
        name: &str,
        decoding: &Decoding,
        write: &mut dyn FnMut(u64, &mut dyn Read) -> Result<(), String>,
    ) -> Result<(), String> {
        let missing = || format!("{name}: it is not in the archive");
        let i = self
            .archive
            .files
            .iter()
            .position(|f| f.name == name && !f.is_directory)
            .ok_or_else(missing)?;
        let Some(b) = self.archive.stream_map.file_block_index[i] else {
            return write(0, &mut io::empty());
        };
        let mut index = self.archive.stream_map.block_first_file_index[b];
        let mut written = None;
        let password = Password::empty();
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            decoding.limit_kb(),
            b,
            &self.archive,
            &password,
            &mut self.source,
        );
        decoder
            .for_each_entries(&mut |entry, member| {
                let at = index;
                index += 1;
                if at != i {
                    return Ok(true);
                }
                written = Some(write(entry.size, member));
                Ok(false)
            })
            .map_err(|e| format!("{name}: it is damaged: {e}"))?;
        written.ok_or_else(missing)?
    }

    fn finish(self) -> Result<(String, u64), String> {
        self.source.into_inner().finish().map_err(|e| e.to_string())
    }
}

/// Writes `roms` to `dest`, a new, empty file the caller created, as a TorrentZip, each ROM read from the named
/// member of `source`, a 7z archive, within `decoding`'s bounds. Returns the SHA-256 and size of `source`, hashed as
/// it was read. Checksums are checked by [`crate::build::verify`] afterwards.
pub fn repack(
    dest: &File,
    roms: &[(&ExpectedRom, &str)],
    source: &File,
    decoding: &Decoding,
) -> Result<(String, u64), String> {
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let mut opened = open(source, decoding)?;
    let _permit = decoding.gate.enter();
    let mut writer = Writer::new(BufWriter::new(dest)).map_err(|e| e.to_string())?;
    for (rom, member) in order {
        opened.with_member(member, decoding, &mut |len, payload| {
            let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", rom.name);
            let (size, mut payload) = sized(rom, len, payload).map_err(|e| failed(&e))?;
            writer
                .add(&rom.name, size, &mut payload)
                .map_err(|e| failed(&e))
        })?;
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    opened.finish()
}

/// Writes `rom` into `dest`, a new, empty file the caller created, as a loose file: its payload, read from member
/// `member` of `source`, a 7z archive, within `decoding`'s bounds. Returns the SHA-256 and size of `source`, hashed
/// as it was read. A payload of the wrong size is refused; checksums are checked by
/// [`crate::build::verify_loose`] afterwards.
pub fn write_loose(
    dest: &File,
    rom: &ExpectedRom,
    member: &str,
    source: &File,
    decoding: &Decoding,
) -> Result<(String, u64), String> {
    let mut opened = open(source, decoding)?;
    let _permit = decoding.gate.enter();
    let mut out = BufWriter::new(dest);
    let mut copied = None;
    opened.with_member(member, decoding, &mut |len, payload| {
        copied = Some(copy_sized(rom, len, payload, &mut out)?);
        Ok(())
    })?;
    let (copied, size) = copied.ok_or_else(|| format!("{member}: it is not in the archive"))?;
    if copied != size {
        return Err(wrong_size(rom, copied, size));
    }
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    opened.finish()
}
