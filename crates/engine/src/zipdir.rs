//! Bounds a ZIP archive's central directory from its end records alone, before an archive reader is built: the
//! reader indexes every entry the directory declares, keeps a ZIP64 end record's extensible data, and, for a
//! directory that does not end where its end record begins, searches the file for directory headers. Any of these
//! could exhaust memory before a per-member limit is checked.
//!
//! So only a ZIP archive in canonical layout is opened, the layout TorrentZip and mainstream ZIP writers produce:
//! - the last end-record signature in the file's tail is the end record, and its comment runs exactly to the end of
//!   the file, holding no end-record, ZIP64 end-record or ZIP64 locator signature;
//! - with ZIP64, the locator sits immediately before the end record, and the ZIP64 end record it names ends exactly at
//!   the locator, with at most [`ZIP64_EXTENSIBLE`] bytes of extensible data;
//! - the directory ends exactly where its (ZIP64) end record begins, on one disk, and starts with a directory header,
//!   or is empty and declares no entries.
//!
//! The archive reader then takes that end record first and never searches beyond it. Anything else is refused.
//!
//! A file that does not begin as a ZIP archive is a loose file, whatever its tail holds, and is never opened as an
//! archive. [`open`] is the only way into the archive reader, and opens only a file judged [`Layout::Zip`].

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

const EOCD: [u8; 4] = *b"PK\x05\x06";
const EOCD_LEN: u64 = 22;
const ZIP64_LOCATOR: [u8; 4] = *b"PK\x06\x07";
const ZIP64_LOCATOR_LEN: u64 = 20;
const ZIP64_EOCD: [u8; 4] = *b"PK\x06\x06";
const ZIP64_EOCD_LEN: u64 = 56;
const CENTRAL_HEADER: [u8; 4] = *b"PK\x01\x02";
const CENTRAL_HEADER_LEN: u64 = 46;
const LOCAL_HEADER: [u8; 4] = *b"PK\x03\x04";
/// The end record, the longest comment it can have, and the slack the archive reader searches beyond them, so any
/// end record it could find is in the tail.
const TAIL: u64 = EOCD_LEN + 4 + (1 << 16);
/// The most extensible data a ZIP64 end record may carry; archives written in practice carry none.
pub const ZIP64_EXTENSIBLE: u64 = 1 << 16;

/// How large a central directory may be before an archive is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryLimits {
    pub entries: u64,
    pub bytes: u64,
}

impl Default for DirectoryLimits {
    fn default() -> DirectoryLimits {
        DirectoryLimits {
            entries: u16::MAX.into(),
            bytes: 64 << 20,
        }
    }
}

/// What a file's end records say about opening it as a ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// There is no end record, so no archive reader can open it.
    NotZip,
    /// In canonical layout and within bounds: an archive reader may open it.
    Zip,
    /// Never to be opened as an archive, for this reason.
    Refused(String),
}

fn u16_at(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]).into()
}

fn u32_at(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes")).into()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("eight bytes"))
}

fn read_at(mut file: &File, at: u64, buf: &mut [u8]) -> io::Result<bool> {
    file.seek(SeekFrom::Start(at))?;
    match file.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

fn contains(haystack: &[u8], needle: &[u8; 4]) -> bool {
    haystack.windows(4).any(|w| w == needle)
}

/// Whether `file` begins as a ZIP archive does, with a member's local header or, when empty, its end record.
fn starts_as_zip(file: &File) -> io::Result<bool> {
    let mut magic = [0; 4];
    Ok(read_at(file, 0, &mut magic)? && (magic == LOCAL_HEADER || magic == EOCD))
}

fn unsupported(why: &str) -> Layout {
    Layout::Refused(format!("this ZIP's layout isn't supported: {why}"))
}

/// Judges `file`: a loose file unless it begins as a ZIP archive, and otherwise by its layout and its central
/// directory against `limits`. Leaves the file's offset unspecified.
pub fn classify(file: &File, limits: &DirectoryLimits) -> io::Result<Layout> {
    if !starts_as_zip(file)? {
        return Ok(Layout::NotZip);
    }
    check_directory(file, limits)
}

/// What [`open`] made of a file.
pub enum Gated<R: Read + Seek> {
    /// Not a ZIP archive: a loose file, never opened as an archive.
    NotZip,
    /// A ZIP archive never to be opened, for this reason.
    Refused(String),
    /// In canonical layout and within bounds, as the archive reader opened it.
    Archive(zip::result::ZipResult<zip::ZipArchive<R>>),
}

/// Opens `file` as a ZIP archive, reading it through what `reader` makes, only once [`classify`] judges it a ZIP
/// within `limits`.
pub fn open<R: Read + Seek>(
    file: &File,
    limits: &DirectoryLimits,
    reader: impl FnOnce() -> io::Result<R>,
) -> io::Result<Gated<R>> {
    match classify(file, limits)? {
        Layout::NotZip => Ok(Gated::NotZip),
        Layout::Refused(why) => Ok(Gated::Refused(why)),
        Layout::Zip => {
            #[cfg(test)]
            OPENED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(file.metadata()?.len());
            Ok(Gated::Archive(zip::ZipArchive::new(reader()?)))
        }
    }
}

/// The length of each file [`open`] handed to the archive reader.
#[cfg(test)]
pub(crate) static OPENED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// Judges `file`'s layout and its central directory against `limits`, whatever it begins with.
pub(crate) fn check_directory(file: &File, limits: &DirectoryLimits) -> io::Result<Layout> {
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL);
    let mut tail = vec![0; (len - start) as usize];
    if !read_at(file, start, &mut tail)? {
        return Ok(Layout::NotZip);
    }
    let Some(at) = tail.windows(4).rposition(|w| w == EOCD) else {
        return Ok(Layout::NotZip);
    };
    let record_at = start + at as u64;
    let Some(record) = tail.get(at..at + EOCD_LEN as usize) else {
        return Ok(unsupported("its last end record is cut short"));
    };
    let comment = &tail[at + EOCD_LEN as usize..];
    if u16_at(record, 20) != comment.len() as u64 {
        return Ok(unsupported(
            "its comment does not run exactly to the end of the file",
        ));
    }
    if contains(comment, &ZIP64_EOCD) || contains(comment, &ZIP64_LOCATOR) {
        return Ok(unsupported("its comment holds another end record"));
    }
    let (disk, directory_disk) = (u16_at(record, 4), u16_at(record, 6));
    let (on_disk, entries) = (u16_at(record, 8), u16_at(record, 10));
    let (bytes, offset) = (u32_at(record, 12), u32_at(record, 16));
    if disk != 0 || directory_disk != 0 || on_disk != entries {
        return Ok(unsupported("it spans more than one disk"));
    }
    let zip64 = entries == u64::from(u16::MAX)
        || bytes == u64::from(u32::MAX)
        || offset == u64::from(u32::MAX);
    let (entries, bytes, offset, end) = if zip64 {
        match zip64_directory(file, record_at)? {
            Ok(directory) => directory,
            Err(why) => return Ok(why),
        }
    } else {
        (entries, bytes, offset, record_at)
    };
    if entries > limits.entries {
        return Ok(Layout::Refused(format!(
            "its directory declares {entries} members, more than the {} a ZIP is opened with",
            limits.entries
        )));
    }
    if bytes > limits.bytes {
        return Ok(Layout::Refused(format!(
            "its directory is {bytes} bytes, more than the {} a ZIP is opened with",
            limits.bytes
        )));
    }
    if offset.checked_add(bytes) != Some(end) {
        return Ok(unsupported(
            "its directory does not end where its end record begins",
        ));
    }
    if entries == 0 {
        return Ok(match bytes {
            0 => Layout::Zip,
            _ => unsupported("its directory has bytes but no members"),
        });
    }
    if entries.saturating_mul(CENTRAL_HEADER_LEN) > bytes {
        return Ok(unsupported(
            "its directory is too small for the members it declares",
        ));
    }
    let mut magic = [0; 4];
    if !read_at(file, offset, &mut magic)? || magic != CENTRAL_HEADER {
        return Ok(unsupported(
            "its directory does not start with a directory header",
        ));
    }
    Ok(Layout::Zip)
}

/// The members, size, offset and end of the directory the ZIP64 records before the end record at `record_at`
/// declare, or why they are refused.
fn zip64_directory(
    file: &File,
    record_at: u64,
) -> io::Result<Result<(u64, u64, u64, u64), Layout>> {
    let mut locator = [0; ZIP64_LOCATOR_LEN as usize];
    if record_at < ZIP64_LOCATOR_LEN
        || !read_at(file, record_at - ZIP64_LOCATOR_LEN, &mut locator)?
        || locator[..4] != ZIP64_LOCATOR
    {
        return Ok(Err(unsupported(
            "its ZIP64 locator is not just before its end record",
        )));
    }
    let locator_at = record_at - ZIP64_LOCATOR_LEN;
    let (directory_disk, named, disks) = (
        u32_at(&locator, 4),
        u64_at(&locator, 8),
        u32_at(&locator, 16),
    );
    if directory_disk != 0 || disks > 1 {
        return Ok(Err(unsupported("it spans more than one disk")));
    }
    let Some(span) = locator_at
        .checked_sub(named)
        .filter(|&span| span >= ZIP64_EOCD_LEN)
    else {
        return Ok(Err(unsupported(
            "its ZIP64 end record is not before its locator",
        )));
    };
    if span > ZIP64_EOCD_LEN + ZIP64_EXTENSIBLE {
        return Ok(Err(Layout::Refused(format!(
            "its ZIP64 end record runs to {span} bytes, more than the {} a ZIP is opened with",
            ZIP64_EOCD_LEN + ZIP64_EXTENSIBLE
        ))));
    }
    let mut record = [0; ZIP64_EOCD_LEN as usize];
    if !read_at(file, named, &mut record)? || record[..4] != ZIP64_EOCD {
        return Ok(Err(unsupported(
            "its ZIP64 end record is not where its locator says",
        )));
    }
    if u64_at(&record, 4).checked_add(12) != Some(span) {
        return Ok(Err(unsupported(
            "its ZIP64 end record does not end at its locator",
        )));
    }
    let (disk, of_disk) = (u32_at(&record, 16), u32_at(&record, 20));
    let (on_disk, entries) = (u64_at(&record, 24), u64_at(&record, 32));
    if disk != 0 || of_disk != 0 || on_disk != entries {
        return Ok(Err(unsupported("it spans more than one disk")));
    }
    Ok(Ok((
        entries,
        u64_at(&record, 40),
        u64_at(&record, 48),
        named,
    )))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
    use crate::build::{Probed, ZipLimits, probe, probe_within};
    use crate::read::{Known, ToRead, read_files};
    use crate::{AuditEvidence, Workers};

    const INGEST: DirectoryLimits = DirectoryLimits {
        entries: 4096,
        bytes: 16 << 20,
    };

    fn file_with(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    fn layout(bytes: &[u8]) -> Layout {
        check_directory(&file_with(bytes), &INGEST).unwrap()
    }

    fn refused(bytes: &[u8]) -> String {
        match layout(bytes) {
            Layout::Refused(why) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// A ZIP the zip crate writes of `members`, with `comment`.
    fn written(
        members: &[(&str, &[u8])],
        options: zip::write::SimpleFileOptions,
        comment: &[u8],
    ) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, payload) in members {
            zip.start_file(*name, options).unwrap();
            zip.write_all(payload).unwrap();
        }
        zip.set_raw_comment(comment.into()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn stored() -> zip::write::SimpleFileOptions {
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
    }

    fn torrentzip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = crate::torrentzip::Writer::new(Cursor::new(Vec::new())).unwrap();
        for (name, payload) in members {
            writer
                .add(name, payload.len() as u64, &mut &payload[..])
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// The end record of a single-disk directory of `entries` members, `bytes` long at `offset`.
    fn eocd(entries: u16, bytes: u32, offset: u32) -> Vec<u8> {
        let mut record = EOCD.to_vec();
        record.extend([0, 0, 0, 0]);
        record.extend(entries.to_le_bytes());
        record.extend(entries.to_le_bytes());
        record.extend(bytes.to_le_bytes());
        record.extend(offset.to_le_bytes());
        record.extend(0u16.to_le_bytes());
        record
    }

    /// A ZIP64 end record of `entries` members whose record runs `extensible` bytes past its fixed part.
    fn zip64_eocd(entries: u64, extensible: u64) -> Vec<u8> {
        let mut record = ZIP64_EOCD.to_vec();
        record.extend((44 + extensible).to_le_bytes());
        record.extend([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        record.extend(entries.to_le_bytes());
        record.extend(entries.to_le_bytes());
        record.extend(0u64.to_le_bytes());
        record.extend(0u64.to_le_bytes());
        record
    }

    fn zip64_locator(record_at: u64) -> Vec<u8> {
        let mut locator = ZIP64_LOCATOR.to_vec();
        locator.extend(0u32.to_le_bytes());
        locator.extend(record_at.to_le_bytes());
        locator.extend(1u32.to_le_bytes());
        locator
    }

    /// Bytes no deflater can shrink, so they are stored as they are.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// Asserts `bytes` is in canonical layout, and probes and audits as a ZIP holding `payloads` in order.
    fn reads_normally(bytes: &[u8], payloads: &[&[u8]]) {
        assert_eq!(layout(bytes), Layout::Zip);
        assert_eq!(
            check_directory(&file_with(bytes), &DirectoryLimits::default()).unwrap(),
            Layout::Zip
        );
        let probe = probe(&file_with(bytes)).unwrap();
        assert_eq!(probe.error, None);
        assert_eq!(probe.payloads.len(), payloads.len(), "{probe:?}");
        let Probed::Read(within) = probe_within(&file_with(bytes), &ZipLimits::default()).unwrap()
        else {
            panic!("refused within a game's limits");
        };
        assert_eq!(within.payloads.len(), payloads.len());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zip");
        std::fs::write(&path, bytes).unwrap();
        let files = [ToRead {
            path,
            is_zip: true,
            rel: "a.zip".into(),
        }];
        let how = crate::read::How {
            evidence: AuditEvidence::Incremental,
            reads: None,
        };
        let workers = Workers {
            readers: 1,
            hashers: 1,
        };
        let read = read_files(&files, how, workers, None).pop().unwrap();
        assert_eq!(read.error, None);
        assert_eq!(read.members.len(), payloads.len());
        for ((_, known), payload) in read.members.iter().zip(payloads) {
            let Ok(Known::Payload(sums)) = known else {
                panic!("a payload was not read");
            };
            let mut hasher = crate::payload::Hasher::new();
            hasher.update(payload);
            assert_eq!(sums.sha256, hasher.finish().sha256);
        }
    }

    /// Asserts `bytes`, beginning as a ZIP, is refused unread by every reader.
    fn refused_by_every_reader(bytes: &[u8]) {
        assert!(matches!(
            probe_within(&file_with(bytes), &ZipLimits::default()).unwrap(),
            Probed::Refused(_)
        ));
        let probe = probe(&file_with(bytes)).unwrap();
        assert!(
            probe.payloads.is_empty() && probe.error.is_some(),
            "{probe:?}"
        );
    }

    #[test]
    fn torrentzips_and_archives_mainstream_writers_make_are_read_normally() {
        let golden = include_bytes!("../tests/data/golden-game.zip");
        assert_eq!(layout(golden), Layout::Zip);
        let a = noise(3000);
        reads_normally(
            &torrentzip(&[("a.bin", &a), ("b.bin", b"bbbb")]),
            &[&a, b"bbbb"],
        );
        let deflated = zip::write::SimpleFileOptions::default();
        reads_normally(
            &written(&[("a.bin", b"aaaa"), ("b.bin", &a)], deflated, b""),
            &[b"aaaa", &a],
        );
        reads_normally(
            &written(&[("a.bin", b"aaaa")], stored(), b"made for a test"),
            &[b"aaaa"],
        );
        reads_normally(
            &written(&[("a.bin", b"aaaa")], stored().large_file(true), b""),
            &[b"aaaa"],
        );
        assert_eq!(layout(&written(&[], stored(), b"")), Layout::Zip);
    }

    #[test]
    fn end_record_bytes_inside_a_payload_are_not_taken_for_its_end() {
        // Were it the end record, it would declare a directory of nearly 4 GiB.
        let fake = eocd(1, 0xffff_fff0, 0);
        let mut payload = noise(8192);
        payload[4000..4000 + fake.len()].copy_from_slice(&fake);
        let bytes = torrentzip(&[("a.bin", &payload)]);
        assert!(
            bytes.windows(fake.len()).any(|w| w == fake),
            "the payload was stored as it is"
        );

        reads_normally(&bytes, &[&payload]);
    }

    #[test]
    fn a_directory_declaring_too_many_members_or_bytes_is_refused_before_it_is_read() {
        let zip = written(&[("a.bin", b"aaaa")], stored(), b"");
        let end = zip.len() - EOCD_LEN as usize;
        let forged = |entries: u16, bytes: u32| {
            let mut zip = zip.clone();
            zip[end + 8..end + 12]
                .copy_from_slice(&[entries.to_le_bytes(), entries.to_le_bytes()].concat());
            zip[end + 12..end + 16].copy_from_slice(&bytes.to_le_bytes());
            zip
        };

        assert!(refused(&forged(4097, 0)).contains("4097 members"));
        assert!(refused(&forged(1, 17 << 20)).contains("bytes"));
        assert_eq!(layout(b"not a zip"), Layout::NotZip);
    }

    #[test]
    fn a_zip64_directory_declaring_millions_of_members_is_refused() {
        let mut bytes = zip64_eocd(5_000_000, 0);
        bytes.extend(zip64_locator(0));
        bytes.extend(eocd(u16::MAX, 0, 0));

        assert!(refused(&bytes).contains("members"));
    }

    #[test]
    fn a_zip64_end_record_with_gigabytes_of_extensible_data_is_refused() {
        let extensible = 8u64 << 30;
        let file = file_with(&zip64_eocd(0, extensible));
        // Sparse: the extensible data takes no space, but a reader trusting the record would allocate it.
        file.set_len(ZIP64_EOCD_LEN + extensible).unwrap();
        let mut tail = zip64_locator(0);
        tail.extend(eocd(u16::MAX, 0, 0));
        (&file).seek(SeekFrom::End(0)).unwrap();
        (&file).write_all(&tail).unwrap();

        let Layout::Refused(why) = check_directory(&file, &DirectoryLimits::default()).unwrap()
        else {
            panic!("not refused");
        };

        assert!(why.contains("ZIP64 end record"), "{why}");
    }

    #[test]
    fn a_directory_that_does_not_end_at_its_end_record_is_refused_however_many_headers_follow() {
        // A reader would search all of these for the directory's start, remembering each.
        let mut bytes = LOCAL_HEADER.to_vec();
        for _ in 0..(1 << 18) {
            bytes.extend(CENTRAL_HEADER);
        }
        bytes.extend(eocd(1, 0, 0));

        assert!(refused(&bytes).contains("layout isn't supported"));
        refused_by_every_reader(&bytes);
    }

    #[test]
    fn an_end_record_in_the_comment_or_bytes_after_it_are_refused() {
        let zip = || written(&[("a.bin", b"aaaa")], stored(), b"");
        let in_comment = |fake: &[u8]| {
            let mut bytes = zip();
            let comment = [b"see ".as_slice(), fake].concat();
            let at = bytes.len() - 2;
            bytes[at..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
            bytes.extend(comment);
            bytes
        };
        let mut trailing = zip();
        trailing.extend(b"appended");

        for bytes in [
            in_comment(&eocd(1, 0, 0)),
            in_comment(&eocd(60000, 0, 0)),
            in_comment(&ZIP64_LOCATOR),
            trailing,
        ] {
            // One at the very end is judged as the end record, and refused on what it declares.
            let why = refused(&bytes);
            assert!(
                why.contains("layout isn't supported") || why.contains("60000 members"),
                "{why}"
            );
            refused_by_every_reader(&bytes);
        }
    }

    #[test]
    fn a_loose_file_whose_tail_happens_to_hold_an_end_record_is_still_a_loose_file() {
        let mut rom = noise(4096);
        rom[4000..4004].copy_from_slice(&EOCD);
        assert!(matches!(layout(&rom), Layout::Refused(_)));

        let probe = probe(&file_with(&rom)).unwrap();

        assert!(!probe.zip);
        assert_eq!(probe.payloads.len(), 1);
        assert_eq!(probe.error, None);
    }

    /// Asserts the file `bytes`, whose length no other test's file has, is never handed to the archive reader by
    /// either probe or an audit, and that both probes hash it whole as a loose file.
    fn never_opened_but_hashed_whole(bytes: &[u8]) {
        let whole = {
            let mut hasher = crate::payload::Hasher::new();
            hasher.update(bytes);
            hasher.finish()
        };
        let probe = probe(&file_with(bytes)).unwrap();
        assert!(!probe.zip, "{probe:?}");
        assert_eq!(probe.payloads.len(), 1);
        assert_eq!(probe.payloads[0].sha256, whole.sha256);
        let Probed::Read(within) = probe_within(&file_with(bytes), &ZipLimits::default()).unwrap()
        else {
            panic!("refused as a ZIP");
        };
        assert!(!within.zip);
        assert_eq!(within.payloads.len(), 1);
        assert_eq!(within.payloads[0].sha256, whole.sha256);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zip");
        std::fs::write(&path, bytes).unwrap();
        let files = [ToRead {
            path,
            is_zip: true,
            rel: "a.zip".into(),
        }];
        let how = crate::read::How {
            evidence: AuditEvidence::Incremental,
            reads: None,
        };
        let workers = Workers {
            readers: 1,
            hashers: 1,
        };
        let read = read_files(&files, how, workers, None).pop().unwrap();
        assert!(read.members.is_empty());
        assert!(read.error.is_some_and(|e| e.contains("not a ZIP")));

        let opened = OPENED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !opened.contains(&(bytes.len() as u64)),
            "the archive reader was given it"
        );
    }

    #[test]
    fn an_end_record_only_the_archive_reader_would_search_for_is_never_given_to_it() {
        // The reader looks a few bytes further back than an end record with the longest comment could sit, so one
        // followed by its 65535-byte comment and 13 more bytes is beyond the tail judged, but within the reader's.
        let mut bytes = LOCAL_HEADER.to_vec();
        for _ in 0..1237 {
            bytes.extend(CENTRAL_HEADER);
        }
        let mut record = eocd(1, 0, 0);
        record[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend(record);
        bytes.extend(vec![b'c'; u16::MAX as usize]);
        bytes.extend(b"thirteen more");
        assert_eq!(layout(&bytes), Layout::NotZip);
        assert!(
            !matches!(
                zip::ZipArchive::new(Cursor::new(&bytes)),
                Err(zip::result::ZipError::InvalidArchive(why)) if why.contains("Could not find EOCD")
            ),
            "the reader would find the end record"
        );

        never_opened_but_hashed_whole(&bytes);
    }

    #[test]
    fn a_zip_after_other_bytes_is_a_loose_file_whatever_its_tail() {
        let mut out = Cursor::new(
            b"#!/bin/sh\necho a self-extracting archive, padded to a length of its own\n".to_vec(),
        );
        out.set_position(out.get_ref().len() as u64);
        let mut zip = zip::ZipWriter::new(out);
        zip.start_file("a.bin", stored()).unwrap();
        zip.write_all(&noise(1999)).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert_eq!(layout(&bytes), Layout::Zip, "its tail alone is canonical");
        assert_eq!(
            classify(&file_with(&bytes), &INGEST).unwrap(),
            Layout::NotZip
        );

        never_opened_but_hashed_whole(&bytes);
    }
}
