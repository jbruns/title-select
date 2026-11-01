//! TorrentZip (ADR 0007): our own writer and structural validator, implemented from the documented format (igir's
//! `docs/misc/torrentzip.md`, and PKWARE's `APPNOTE.TXT` for the ZIP64 records).
//!
//! A TorrentZip archive is deterministic: its members are sorted by lowercased name, deflated with zlib at level 9
//! (raw, `-15` window bits, memory level 8), and stamped with one fixed DOS time. No member has an extra field other
//! than ZIP64's, a comment, or attributes. The archive comment is `TORRENTZIPPED-` and the uppercase hex CRC-32 of the
//! central directory headers. The validator checks that structure; it does not recompress members to compare bytes.

use std::cmp::Ordering;
use std::io::{self, Read, Seek, SeekFrom, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// The archive comment's prefix; the CRC-32 of the central directory follows as 8 uppercase hex digits.
pub const COMMENT_PREFIX: &[u8] = b"TORRENTZIPPED-";
const COMMENT_LEN: usize = 22;
/// 23:32:00.
const DOS_TIME: u16 = 0xBC00;
/// 1996-12-24.
const DOS_DATE: u16 = 0x2198;
const FLAG_MAX_COMPRESSION: u16 = 0x0002;
const FLAG_UTF8: u16 = 0x0800;
const METHOD_DEFLATE: u16 = 8;
const VERSION: u16 = 20;
const VERSION_ZIP64: u16 = 45;
const ZIP64_EXTRA: u16 = 0x0001;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const LOCATOR_SIG: u32 = 0x0706_4b50;
const LOCAL_LEN: u64 = 30;
const CENTRAL_LEN: usize = 46;
const EOCD_LEN: u64 = 22;
const EOCD64_LEN: u64 = 56;
const LOCATOR_LEN: u64 = 20;
/// The largest central directory the validator reads.
const MAX_CENTRAL: u64 = 64 << 20;
const CHUNK: usize = 1 << 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The archive is not valid TorrentZip, or a member cannot be written as TorrentZip.
    #[error("not valid TorrentZip: {0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

/// Where ZIP64 takes over. Tests lower them to reach the ZIP64 records without writing 4 GiB.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// A size or offset at least this needs ZIP64.
    wide: u64,
    /// A member count at least this needs ZIP64.
    count: u64,
}

const LIMITS: Limits = Limits {
    wide: 0xFFFF_FFFF,
    count: 0xFFFF,
};

/// A member of a TorrentZip archive, as its central directory records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub size: u64,
    pub crc: u32,
    pub compressed: u64,
    /// Where the member's local header starts.
    pub offset: u64,
    /// Whether the name is stored as UTF-8 (general purpose flag 11), being outside code page 437.
    pub utf8: bool,
}

/// Code page 437's characters for bytes 0x80 to 0xFF; bytes below are ASCII.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// A name as TorrentZip stores it: in code page 437 when every character has a byte there, else in UTF-8 with the
/// UTF-8 flag. Returns the bytes and whether they are UTF-8.
pub fn encode_name(name: &str) -> (Vec<u8>, bool) {
    let cp437: Option<Vec<u8>> = name
        .chars()
        .map(|ch| {
            if ch.is_ascii() {
                Some(ch as u8)
            } else {
                CP437_HIGH
                    .iter()
                    .position(|c| *c == ch)
                    .map(|i| 0x80 + i as u8)
            }
        })
        .collect();
    match cp437 {
        Some(bytes) => (bytes, false),
        None => (name.as_bytes().to_vec(), true),
    }
}

fn decode_name(bytes: &[u8], utf8: bool) -> Result<String, Error> {
    if utf8 {
        return String::from_utf8(bytes.to_vec()).map_err(|_| invalid("a UTF-8 name is not UTF-8"));
    }
    Ok(bytes
        .iter()
        .map(|b| {
            if b.is_ascii() {
                *b as char
            } else {
                CP437_HIGH[(b - 0x80) as usize]
            }
        })
        .collect())
}

/// Why `name` could escape the folder a ZIP tool extracts it into, or `None`: an absolute path, a drive, a backslash, an
/// empty, `.` or `..` component, or a control character.
pub fn unsafe_name(name: &str) -> Option<&'static str> {
    if name.chars().any(char::is_control) {
        return Some("a member name holds a control character");
    }
    if name.contains('\\') {
        return Some("a member name holds a backslash");
    }
    if name.starts_with('/') || name.as_bytes().get(1) == Some(&b':') {
        return Some("a member name is an absolute path");
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Some("a member name has an empty, . or .. component");
    }
    None
}

/// TorrentZip's member order: by the stored name's bytes with ASCII letters lowercased, then by the bytes as stored.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    compare_stored(&encode_name(a).0, &encode_name(b).0)
}

fn compare_stored(a: &[u8], b: &[u8]) -> Ordering {
    let lower = |s: &[u8]| s.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    lower(a).cmp(&lower(b)).then_with(|| a.cmp(b))
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A 32-bit field, or `0xFFFFFFFF` when ZIP64 carries the value.
fn narrow(v: u64, limits: Limits) -> u32 {
    if v >= limits.wide { u32::MAX } else { v as u32 }
}

/// Counts what passes through to the archive.
struct Counting<'a, W> {
    inner: &'a mut W,
    written: u64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes a TorrentZip archive member by member, in TorrentZip order.
pub struct Writer<W: Write + Seek> {
    out: W,
    start: u64,
    members: Vec<(Vec<u8>, Member)>,
    limits: Limits,
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(out: W) -> Result<Writer<W>, Error> {
        Writer::with_limits(out, LIMITS)
    }

    fn with_limits(mut out: W, limits: Limits) -> Result<Writer<W>, Error> {
        let start = out.stream_position()?;
        Ok(Writer {
            out,
            start,
            members: Vec::new(),
            limits,
        })
    }

    /// Deflates `size` bytes read from `payload` as the member `name`. Members must be added in [`compare_names`]
    /// order, and `size` must be the payload's exact size: it decides the member's header before it is read.
    pub fn add(&mut self, name: &str, size: u64, payload: &mut dyn Read) -> Result<(), Error> {
        if let Some(why) = unsafe_name(name) {
            return Err(invalid(format!("{name:?}: {why}")));
        }
        let (stored, utf8) = encode_name(name);
        if stored.is_empty() || stored.len() > u16::MAX as usize {
            return Err(invalid(format!(
                "{name:?}: a member name is 1 to 65535 bytes"
            )));
        }
        if let Some((last, _)) = self.members.last()
            && compare_stored(last, &stored) != Ordering::Less
        {
            return Err(invalid(format!(
                "{name}: members are added once each, in TorrentZip order"
            )));
        }
        let limits = self.limits;
        let offset = self.out.stream_position()? - self.start;
        let zip64 = size >= limits.wide;
        let flags = FLAG_MAX_COMPRESSION | if utf8 { FLAG_UTF8 } else { 0 };
        let local = local_header(&stored, flags, 0, size, 0, offset, zip64, limits);
        self.out.write_all(&local)?;

        let mut hasher = crc32fast::Hasher::new();
        let mut read = 0u64;
        let compressed = {
            let counting = Counting {
                inner: &mut self.out,
                written: 0,
            };
            let mut encoder = DeflateEncoder::new(counting, Compression::best());
            let mut buf = vec![0; CHUNK];
            loop {
                let n = payload.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                read += n as u64;
                encoder.write_all(&buf[..n])?;
            }
            encoder.finish()?.written
        };
        if read != size {
            return Err(invalid(format!(
                "{name}: the payload is {read} bytes, not the {size} expected"
            )));
        }
        if !zip64 && compressed >= limits.wide {
            return Err(invalid(format!(
                "{name}: deflating grew the payload past what a TorrentZip header without ZIP64 records"
            )));
        }
        let crc = hasher.finalize();
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(self.start + offset))?;
        self.out.write_all(&local_header(
            &stored, flags, crc, size, compressed, offset, zip64, limits,
        ))?;
        self.out.seek(SeekFrom::Start(end))?;
        self.members.push((
            stored,
            Member {
                name: name.to_owned(),
                size,
                crc,
                compressed,
                offset,
                utf8,
            },
        ));
        Ok(())
    }

    /// Writes the central directory and its end records, and returns the output.
    pub fn finish(mut self) -> Result<W, Error> {
        let limits = self.limits;
        let cd_offset = self.out.stream_position()? - self.start;
        let mut central = Vec::new();
        for (stored, m) in &self.members {
            central_header(&mut central, stored, m, limits);
        }
        let cd_size = central.len() as u64;
        let count = self.members.len() as u64;
        let mut tail = Vec::new();
        if cd_size >= limits.wide || cd_offset >= limits.wide || count >= limits.count {
            let eocd64 = cd_offset + cd_size;
            put32(&mut tail, EOCD64_SIG);
            put64(&mut tail, EOCD64_LEN - 12);
            put16(&mut tail, VERSION_ZIP64);
            put16(&mut tail, VERSION_ZIP64);
            put32(&mut tail, 0);
            put32(&mut tail, 0);
            put64(&mut tail, count);
            put64(&mut tail, count);
            put64(&mut tail, cd_size);
            put64(&mut tail, cd_offset);
            put32(&mut tail, LOCATOR_SIG);
            put32(&mut tail, 0);
            put64(&mut tail, eocd64);
            put32(&mut tail, 1);
        }
        put32(&mut tail, EOCD_SIG);
        put16(&mut tail, 0);
        put16(&mut tail, 0);
        let entries = count.min(limits.count) as u16;
        put16(&mut tail, entries);
        put16(&mut tail, entries);
        put32(&mut tail, narrow(cd_size, limits));
        put32(&mut tail, narrow(cd_offset, limits));
        let comment = format!("TORRENTZIPPED-{:08X}", crc32fast::hash(&central));
        put16(&mut tail, comment.len() as u16);
        tail.extend_from_slice(comment.as_bytes());
        self.out.write_all(&central)?;
        self.out.write_all(&tail)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[allow(clippy::too_many_arguments)]
fn local_header(
    stored: &[u8],
    flags: u16,
    crc: u32,
    size: u64,
    compressed: u64,
    offset: u64,
    zip64: bool,
    limits: Limits,
) -> Vec<u8> {
    let mut extra = Vec::new();
    if zip64 {
        put16(&mut extra, ZIP64_EXTRA);
        let len = if offset >= limits.wide { 24 } else { 16 };
        put16(&mut extra, len);
        put64(&mut extra, size);
        put64(&mut extra, compressed);
        if offset >= limits.wide {
            put64(&mut extra, offset);
        }
    }
    let mut out = Vec::with_capacity(LOCAL_LEN as usize + stored.len() + extra.len());
    put32(&mut out, LOCAL_SIG);
    put16(&mut out, if zip64 { VERSION_ZIP64 } else { VERSION });
    put16(&mut out, flags);
    put16(&mut out, METHOD_DEFLATE);
    put16(&mut out, DOS_TIME);
    put16(&mut out, DOS_DATE);
    put32(&mut out, crc);
    let wide = if zip64 { u32::MAX } else { 0 };
    put32(&mut out, if zip64 { wide } else { compressed as u32 });
    put32(&mut out, if zip64 { wide } else { size as u32 });
    put16(&mut out, stored.len() as u16);
    put16(&mut out, extra.len() as u16);
    out.extend_from_slice(stored);
    out.extend_from_slice(&extra);
    out
}

/// The central directory's ZIP64 extra field for `m`: each value too wide for its header field, in APPNOTE order.
fn central_extra(m: &Member, limits: Limits) -> Vec<u8> {
    let wide: Vec<u64> = [m.size, m.compressed, m.offset]
        .into_iter()
        .filter(|v| *v >= limits.wide)
        .collect();
    let mut extra = Vec::new();
    if !wide.is_empty() {
        put16(&mut extra, ZIP64_EXTRA);
        put16(&mut extra, (wide.len() * 8) as u16);
        for v in wide {
            put64(&mut extra, v);
        }
    }
    extra
}

fn central_header(out: &mut Vec<u8>, stored: &[u8], m: &Member, limits: Limits) {
    let extra = central_extra(m, limits);
    let flags = FLAG_MAX_COMPRESSION | if m.utf8 { FLAG_UTF8 } else { 0 };
    put32(out, CENTRAL_SIG);
    put16(out, 0);
    put16(
        out,
        if extra.is_empty() {
            VERSION
        } else {
            VERSION_ZIP64
        },
    );
    put16(out, flags);
    put16(out, METHOD_DEFLATE);
    put16(out, DOS_TIME);
    put16(out, DOS_DATE);
    put32(out, m.crc);
    put32(out, narrow(m.compressed, limits));
    put32(out, narrow(m.size, limits));
    put16(out, stored.len() as u16);
    put16(out, extra.len() as u16);
    put16(out, 0);
    put16(out, 0);
    put16(out, 0);
    put32(out, 0);
    put32(out, narrow(m.offset, limits));
    out.extend_from_slice(stored);
    out.extend_from_slice(&extra);
}

/// A little-endian cursor over a header's bytes.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Fields<'a> {
        Fields { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| invalid("a header is cut short"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

fn expect(what: &str, got: u64, want: u64) -> Result<(), Error> {
    if got == want {
        Ok(())
    } else {
        Err(invalid(format!("{what} is {got:#x}, not {want:#x}")))
    }
}

fn read_at(file: &mut (impl Read + Seek), at: u64, len: u64) -> Result<Vec<u8>, Error> {
    file.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0; len as usize];
    file.read_exact(&mut buf)
        .map_err(|_| invalid("the archive is cut short"))?;
    Ok(buf)
}

/// The central directory's location, as the end records give it.
struct End {
    cd_offset: u64,
    cd_size: u64,
    count: u64,
    /// Where the end records start: the central directory must end here.
    records_at: u64,
}

fn read_end(file: &mut (impl Read + Seek), len: u64, limits: Limits) -> Result<End, Error> {
    let tail = EOCD_LEN + COMMENT_LEN as u64;
    if len < tail {
        return Err(invalid("too short to be a ZIP archive"));
    }
    let eocd_at = len - tail;
    let bytes = read_at(file, eocd_at, tail)?;
    let mut f = Fields::new(&bytes);
    expect(
        "the end of central directory signature",
        f.u32()?.into(),
        EOCD_SIG.into(),
    )?;
    expect("the disk number", f.u16()?.into(), 0)?;
    expect("the central directory's disk", f.u16()?.into(), 0)?;
    let on_disk = f.u16()?;
    let entries = f.u16()?;
    expect("the entries on this disk", on_disk.into(), entries.into())?;
    let cd_size = f.u32()?;
    let cd_offset = f.u32()?;
    expect(
        "the archive comment's length",
        f.u16()?.into(),
        COMMENT_LEN as u64,
    )?;
    let comment = f.take(COMMENT_LEN)?;
    if !comment.starts_with(COMMENT_PREFIX)
        || !comment[COMMENT_PREFIX.len()..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
    {
        return Err(invalid("the archive comment is not TORRENTZIPPED-<CRC-32>"));
    }
    let needs64 = u64::from(entries) >= limits.count.min(0xFFFF)
        || u64::from(cd_size) >= limits.wide.min(0xFFFF_FFFF)
        || u64::from(cd_offset) >= limits.wide.min(0xFFFF_FFFF);
    if !needs64 {
        return Ok(End {
            cd_offset: cd_offset.into(),
            cd_size: cd_size.into(),
            count: entries.into(),
            records_at: eocd_at,
        });
    }
    let locator_at = eocd_at
        .checked_sub(LOCATOR_LEN)
        .ok_or_else(|| invalid("the ZIP64 end locator is missing"))?;
    let locator = read_at(file, locator_at, LOCATOR_LEN)?;
    let mut f = Fields::new(&locator);
    expect(
        "the ZIP64 end locator signature",
        f.u32()?.into(),
        LOCATOR_SIG.into(),
    )?;
    expect("the ZIP64 end record's disk", f.u32()?.into(), 0)?;
    let eocd64_at = f.u64()?;
    expect("the number of disks", f.u32()?.into(), 1)?;
    expect(
        "the ZIP64 end record's offset",
        eocd64_at,
        locator_at.saturating_sub(EOCD64_LEN),
    )?;
    let record = read_at(file, eocd64_at, EOCD64_LEN)?;
    let mut f = Fields::new(&record);
    expect(
        "the ZIP64 end record signature",
        f.u32()?.into(),
        EOCD64_SIG.into(),
    )?;
    expect("the ZIP64 end record's size", f.u64()?, EOCD64_LEN - 12)?;
    expect(
        "the ZIP64 end record's version made by",
        f.u16()?.into(),
        VERSION_ZIP64.into(),
    )?;
    expect(
        "the ZIP64 end record's version needed",
        f.u16()?.into(),
        VERSION_ZIP64.into(),
    )?;
    expect("the ZIP64 disk number", f.u32()?.into(), 0)?;
    expect("the ZIP64 central directory's disk", f.u32()?.into(), 0)?;
    let on_disk = f.u64()?;
    let count = f.u64()?;
    expect("the ZIP64 entries on this disk", on_disk, count)?;
    let size64 = f.u64()?;
    let offset64 = f.u64()?;
    expect("the entry count", entries.into(), count.min(limits.count))?;
    expect(
        "the central directory size",
        cd_size.into(),
        narrow(size64, limits).into(),
    )?;
    expect(
        "the central directory offset",
        cd_offset.into(),
        narrow(offset64, limits).into(),
    )?;
    Ok(End {
        cd_offset: offset64,
        cd_size: size64,
        count,
        records_at: eocd64_at,
    })
}

/// Reads and checks the central directory of the TorrentZip archive in `file`, without reading members: the end
/// records, the comment's CRC-32, and each header's fixed fields, ZIP64 use and order.
pub fn central(file: &mut (impl Read + Seek)) -> Result<Vec<Member>, Error> {
    central_with(file, LIMITS)
}

fn central_with(file: &mut (impl Read + Seek), limits: Limits) -> Result<Vec<Member>, Error> {
    let len = file.seek(SeekFrom::End(0))?;
    let end = read_end(file, len, limits)?;
    if end.cd_size > MAX_CENTRAL {
        return Err(invalid("the central directory is too large to check"));
    }
    expect(
        "the central directory's end",
        end.cd_offset.saturating_add(end.cd_size),
        end.records_at,
    )?;
    let cd = read_at(file, end.cd_offset, end.cd_size)?;
    let comment = read_at(file, len - COMMENT_LEN as u64, COMMENT_LEN as u64)?;
    let want = format!("TORRENTZIPPED-{:08X}", crc32fast::hash(&cd));
    if comment != want.as_bytes() {
        return Err(invalid(format!(
            "the comment is {}, but the central directory's CRC-32 makes it {want}",
            String::from_utf8_lossy(&comment)
        )));
    }
    let mut f = Fields::new(&cd);
    let mut members: Vec<(Vec<u8>, Member)> = Vec::new();
    while f.at < cd.len() {
        let header = f.take(CENTRAL_LEN)?;
        let mut h = Fields::new(header);
        expect(
            "a central header's signature",
            h.u32()?.into(),
            CENTRAL_SIG.into(),
        )?;
        expect("a central header's version made by", h.u16()?.into(), 0)?;
        let needed = h.u16()?;
        let flags = h.u16()?;
        expect(
            "a member's compression method",
            h.u16()?.into(),
            METHOD_DEFLATE.into(),
        )?;
        expect("a member's time", h.u16()?.into(), DOS_TIME.into())?;
        expect("a member's date", h.u16()?.into(), DOS_DATE.into())?;
        let crc = h.u32()?;
        let compressed32 = h.u32()?;
        let size32 = h.u32()?;
        let name_len = h.u16()? as usize;
        let extra_len = h.u16()? as usize;
        expect("a member's comment length", h.u16()?.into(), 0)?;
        expect("a member's disk", h.u16()?.into(), 0)?;
        expect("a member's internal attributes", h.u16()?.into(), 0)?;
        expect("a member's external attributes", h.u32()?.into(), 0)?;
        let offset32 = h.u32()?;
        let stored = f.take(name_len)?.to_vec();
        let extra = f.take(extra_len)?;
        let utf8 = flags & FLAG_UTF8 != 0;
        expect(
            "a member's flags",
            flags.into(),
            (FLAG_MAX_COMPRESSION | if utf8 { FLAG_UTF8 } else { 0 }).into(),
        )?;
        let name = decode_name(&stored, utf8)?;
        if encode_name(&name) != (stored.clone(), utf8) {
            return Err(invalid(format!(
                "{name}: its name is stored in UTF-8 only when code page 437 cannot hold it"
            )));
        }
        let mut wide = Fields::new(extra);
        if !extra.is_empty() {
            expect(
                "a member's extra field",
                wide.u16()?.into(),
                ZIP64_EXTRA.into(),
            )?;
            expect(
                "the ZIP64 field's length",
                wide.u16()?.into(),
                (extra_len - 4) as u64,
            )?;
        }
        let mut field = |v: u32| -> Result<u64, Error> {
            if v == u32::MAX {
                wide.u64()
            } else {
                Ok(v.into())
            }
        };
        let size = field(size32)?;
        let compressed = field(compressed32)?;
        let offset = field(offset32)?;
        if wide.at != extra.len() {
            return Err(invalid(format!(
                "{name}: its extra field holds more than ZIP64's"
            )));
        }
        let m = Member {
            name,
            size,
            crc,
            compressed,
            offset,
            utf8,
        };
        if central_extra(&m, limits) != extra {
            return Err(invalid(format!(
                "{}: ZIP64 is used for exactly the values too wide for their fields",
                m.name
            )));
        }
        expect(
            "a central header's version needed",
            needed.into(),
            if extra.is_empty() {
                VERSION
            } else {
                VERSION_ZIP64
            }
            .into(),
        )?;
        if let Some((last, _)) = members.last()
            && compare_stored(last, &stored) != Ordering::Less
        {
            return Err(invalid(format!(
                "{}: members are not sorted by lowercased name",
                m.name
            )));
        }
        members.push((stored, m));
    }
    expect("the entry count", members.len() as u64, end.count)?;
    Ok(members.into_iter().map(|(_, m)| m).collect())
}

/// Checks the whole structure of the TorrentZip archive in `file`: its central directory, as [`central`] does, and
/// that each local header matches it and the members lie end to end from the start of the file to the central
/// directory. Members are not decompressed.
pub fn validate(file: &mut (impl Read + Seek)) -> Result<Vec<Member>, Error> {
    validate_with(file, LIMITS)
}

fn validate_with(file: &mut (impl Read + Seek), limits: Limits) -> Result<Vec<Member>, Error> {
    let members = central_with(file, limits)?;
    let len = file.seek(SeekFrom::End(0))?;
    let end = read_end(file, len, limits)?;
    let mut at = 0u64;
    for m in &members {
        expect(
            &format!("{}: its local header's offset", m.name),
            m.offset,
            at,
        )?;
        let (stored, utf8) = encode_name(&m.name);
        let zip64 = m.size >= limits.wide;
        let flags = FLAG_MAX_COMPRESSION | if utf8 { FLAG_UTF8 } else { 0 };
        let want = local_header(
            &stored,
            flags,
            m.crc,
            m.size,
            m.compressed,
            m.offset,
            zip64,
            limits,
        );
        let got = read_at(file, at, want.len() as u64)?;
        if got != want {
            return Err(invalid(format!(
                "{}: its local header differs from TorrentZip's for its central directory entry",
                m.name
            )));
        }
        at = at
            .checked_add(want.len() as u64)
            .and_then(|a| a.checked_add(m.compressed))
            .ok_or_else(|| invalid("a member's size overflows"))?;
    }
    expect("the central directory's offset", end.cd_offset, at)?;
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn write(members: &[(&str, &[u8])], limits: Limits) -> Vec<u8> {
        let mut sorted = members.to_vec();
        sorted.sort_by(|a, b| compare_names(a.0, b.0));
        let mut w = Writer::with_limits(Cursor::new(Vec::new()), limits).unwrap();
        for (name, payload) in sorted {
            w.add(name, payload.len() as u64, &mut &payload[..])
                .unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn read_back(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut m = zip.by_index(i).unwrap();
                let mut out = Vec::new();
                m.read_to_end(&mut out).unwrap();
                (m.name().unwrap().into_owned(), out)
            })
            .collect()
    }

    #[test]
    fn members_are_sorted_by_lowercased_name_with_fixed_headers_and_the_central_directory_crc_in_the_comment()
     {
        let bytes = write(
            &[("b.bin", b"bee"), ("A.bin", b"ay"), ("a2.bin", b"two")],
            LIMITS,
        );

        let members = validate(&mut Cursor::new(&bytes)).unwrap();
        let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["A.bin", "a2.bin", "b.bin"]);
        assert_eq!(&bytes[..4], &LOCAL_SIG.to_le_bytes());
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 20);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 2);
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 0xBC00);
        assert_eq!(u16::from_le_bytes([bytes[12], bytes[13]]), 0x2198);
        let comment = &bytes[bytes.len() - 22..];
        assert!(comment.starts_with(b"TORRENTZIPPED-"));
        assert_eq!(
            read_back(&bytes),
            [
                ("A.bin".to_owned(), b"ay".to_vec()),
                ("a2.bin".to_owned(), b"two".to_vec()),
                ("b.bin".to_owned(), b"bee".to_vec()),
            ]
        );
    }

    #[test]
    fn a_name_outside_code_page_437_is_stored_in_utf8_with_the_flag_and_one_inside_is_not() {
        let bytes = write(&[("★.bin", b"star"), ("Café.bin", b"cafe")], LIMITS);

        let members = validate(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(
            members
                .iter()
                .map(|m| (m.name.as_str(), m.utf8))
                .collect::<Vec<_>>(),
            [("Café.bin", false), ("★.bin", true)]
        );
        // Café's é is code page 437's 0x82; the star is UTF-8 with flag 11.
        assert!(bytes.windows(8).any(|w| w == b"Caf\x82.bin"));
        let star = members[1].offset as usize;
        assert_eq!(
            u16::from_le_bytes([bytes[star + 6], bytes[star + 7]]),
            0x802
        );
        assert_eq!(read_back(&bytes)[1].0, "★.bin");
    }

    #[test]
    fn zip64_records_are_written_and_validated_where_values_reach_the_limits() {
        let limits = Limits { wide: 8, count: 2 };
        let bytes = write(&[("a", b"0123456789"), ("b", b"x"), ("c", b"yz")], limits);

        let members = validate_with(&mut Cursor::new(&bytes), limits).unwrap();
        assert_eq!(members.len(), 3);
        assert_eq!(members[0].size, 10);
        // The first member is wide, so its local header needs version 45 and carries the ZIP64 field.
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 45);
        assert!(bytes.windows(4).any(|w| w == EOCD64_SIG.to_le_bytes()));
        assert!(bytes.windows(4).any(|w| w == LOCATOR_SIG.to_le_bytes()));
        // Without the lowered limits the same bytes are not canonical.
        assert!(validate(&mut Cursor::new(&bytes)).is_err());
        // The ZIP64 records are laid out as an archive is opened with.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&bytes).unwrap();
        assert_eq!(
            crate::zipdir::check_directory(&file, &Default::default()).unwrap(),
            crate::zipdir::Layout::Zip
        );
    }

    #[test]
    fn the_validator_refuses_a_bad_comment_disorder_a_foreign_timestamp_and_a_plain_zip() {
        let good = write(&[("a.bin", b"a"), ("b.bin", b"b")], LIMITS);
        let mut bad_comment = good.clone();
        let n = bad_comment.len();
        bad_comment[n - 1] = if bad_comment[n - 1] == b'0' {
            b'1'
        } else {
            b'0'
        };
        assert!(validate(&mut Cursor::new(&bad_comment)).is_err());

        let mut bad_time = good.clone();
        bad_time[11] = 0;
        assert!(validate(&mut Cursor::new(&bad_time)).is_err());

        let mut w = Writer::new(Cursor::new(Vec::new())).unwrap();
        w.add("b.bin", 1, &mut &b"b"[..]).unwrap();
        assert!(w.add("a.bin", 1, &mut &b"a"[..]).is_err());

        let mut plain = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut plain);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("a.bin", options).unwrap();
            zip.write_all(b"a").unwrap();
            zip.finish().unwrap();
        }
        assert!(validate(&mut Cursor::new(plain.into_inner())).is_err());
    }

    #[test]
    fn a_payload_of_the_wrong_size_is_refused() {
        let mut w = Writer::new(Cursor::new(Vec::new())).unwrap();
        assert!(matches!(
            w.add("a.bin", 5, &mut &b"abc"[..]),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn member_names_that_could_escape_the_folder_are_refused() {
        for name in [
            "/etc/passwd",
            "C:evil.bin",
            "..\\evil.bin",
            "../evil.bin",
            "a/../../evil.bin",
            "a//b.bin",
            "./a.bin",
            "dir/",
            "a\0b.bin",
            "a\nb.bin",
        ] {
            let mut w = Writer::new(Cursor::new(Vec::new())).unwrap();
            assert!(
                matches!(w.add(name, 1, &mut &b"x"[..]), Err(Error::Invalid(_))),
                "{name:?}"
            );
        }
        let mut w = Writer::new(Cursor::new(Vec::new())).unwrap();
        w.add("disc/a.bin", 1, &mut &b"x"[..]).unwrap();
    }
}
