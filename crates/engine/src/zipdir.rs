//! Bounds a ZIP archive's central directory from its end-of-central-directory records alone, before an archive
//! reader is built: the reader indexes every entry the directory declares, and keeps a ZIP64 end record's
//! extensible data, so a directory declaring millions of entries or an end record of gigabytes would exhaust memory
//! before any per-member limit could be checked.
//!
//! The end record is chosen as the archive reader chooses it. It searches backwards from the end of the file, skips
//! a candidate that is not consistent with the file, and falls back to an earlier one only when the directory of
//! the one it took does not end exactly where that record begins. Each candidate it could take is judged, and the
//! search stops at the first one it cannot fall back past, so signature bytes earlier in a payload are never judged.

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
/// The end record, the longest comment it can have, and the slack an archive reader searches beyond them.
const TAIL: u64 = EOCD_LEN + 4 + (1 << 16);
/// The most extensible data a ZIP64 end record may carry; archives written in practice carry none.
const ZIP64_EXTENSIBLE: u64 = 1 << 16;

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

/// The directory an end record declares, and where that record begins.
struct Directory {
    entries: u64,
    offset: u64,
    bytes: u64,
    end: u64,
}

impl Directory {
    /// Whether the directory ends exactly where its end record begins and starts with a directory header, so the
    /// archive reader takes this record and never falls back past it.
    fn abuts(&self, file: &File) -> io::Result<bool> {
        if self.entries == 0 {
            return Ok(self.bytes == 0);
        }
        if self.offset.checked_add(self.bytes) != Some(self.end) {
            return Ok(false);
        }
        let mut magic = [0; 4];
        Ok(read_at(file, self.offset, &mut magic)? && magic == CENTRAL_HEADER)
    }
}

/// Why the central directory of `file` is past `limits`, if it is. A file with no end record the archive reader
/// would take is not a ZIP archive and passes. Leaves the file's offset unspecified.
pub fn check_directory(file: &File, limits: &DirectoryLimits) -> io::Result<Option<String>> {
    let len = file.metadata()?.len();
    if len < EOCD_LEN {
        return Ok(None);
    }
    let start = len.saturating_sub(TAIL);
    let mut tail = vec![0; (len - start) as usize];
    if !read_at(file, start, &mut tail)? {
        return Ok(None);
    }
    let too_large = |entries: u64, bytes: u64| {
        if entries > limits.entries {
            Some(format!(
                "its directory declares {entries} members, more than the {} a ZIP is opened with",
                limits.entries
            ))
        } else if bytes > limits.bytes {
            Some(format!(
                "its directory is {bytes} bytes, more than the {} a ZIP is opened with",
                limits.bytes
            ))
        } else {
            None
        }
    };
    for at in (0..=tail.len() - EOCD_LEN as usize).rev() {
        if tail[at..at + 4] != EOCD {
            continue;
        }
        let record = &tail[at..at + EOCD_LEN as usize];
        let record_at = start + at as u64;
        let (disk, directory_disk) = (u16_at(record, 4), u16_at(record, 6));
        let (on_disk, entries) = (u16_at(record, 8), u16_at(record, 10));
        let (bytes, offset) = (u32_at(record, 12), u32_at(record, 16));
        if record_at + EOCD_LEN + u16_at(record, 20) > len {
            continue;
        }
        let may_be_zip64 = entries == u64::from(u16::MAX)
            || bytes == u64::from(u32::MAX)
            || offset == u64::from(u32::MAX);
        let mut locator = [0; ZIP64_LOCATOR_LEN as usize];
        let zip64 = may_be_zip64
            && record_at >= ZIP64_LOCATOR_LEN
            && read_at(file, record_at - ZIP64_LOCATOR_LEN, &mut locator)?
            && locator[..4] == ZIP64_LOCATOR;
        let directory = if zip64 {
            match zip64_directory(file, record_at - ZIP64_LOCATOR_LEN, &locator, &too_large)? {
                Zip64::Skipped => continue,
                Zip64::Refused(why) => return Ok(Some(why)),
                Zip64::Judged(directory) => directory,
            }
        } else {
            if disk != 0 || directory_disk != 0 || on_disk != entries {
                continue;
            }
            if if entries == 0 {
                offset > record_at
            } else {
                offset >= record_at
            } {
                continue;
            }
            if let Some(why) = too_large(entries, bytes) {
                return Ok(Some(why));
            }
            Some(Directory {
                entries,
                offset,
                bytes,
                end: record_at,
            })
        };
        if let Some(directory) = directory
            && directory.abuts(file)?
        {
            return Ok(None);
        }
    }
    Ok(None)
}

enum Zip64 {
    /// The archive reader would not take this end record.
    Skipped,
    Refused(String),
    /// Every ZIP64 record the reader could take is within bounds; the one the locator names, if it is one the reader
    /// takes first.
    Judged(Option<Directory>),
}

/// Judges each ZIP64 end record the archive reader could take for the locator at `locator_at`. The reader takes one
/// only if it runs exactly up to the locator, at the place the locator names or anywhere after it, so every record
/// it could take lies in that span, which is bounded first.
fn zip64_directory(
    file: &File,
    locator_at: u64,
    locator: &[u8],
    too_large: &dyn Fn(u64, u64) -> Option<String>,
) -> io::Result<Zip64> {
    let (directory_disk, named, disks) =
        (u32_at(locator, 4), u64_at(locator, 8), u32_at(locator, 16));
    if named >= locator_at || disks > 1 {
        return Ok(Zip64::Skipped);
    }
    let span = locator_at - named;
    if span > ZIP64_EOCD_LEN + ZIP64_EXTENSIBLE {
        return Ok(Zip64::Refused(format!(
            "its ZIP64 directory record may run to {span} bytes, more than the {} a ZIP is opened with",
            ZIP64_EOCD_LEN + ZIP64_EXTENSIBLE
        )));
    }
    let mut records = vec![0; span as usize];
    if !read_at(file, named, &mut records)? {
        return Ok(Zip64::Skipped);
    }
    let mut taken = None;
    for at in 0..records.len().saturating_sub(ZIP64_EOCD_LEN as usize - 1) {
        let record = &records[at..];
        if record[..4] != ZIP64_EOCD || u64_at(record, 4).checked_add(12) != Some(span - at as u64)
        {
            continue;
        }
        let (disk, on_disk_of) = (u32_at(record, 16), u32_at(record, 20));
        let (on_disk, entries) = (u64_at(record, 24), u64_at(record, 32));
        let (bytes, offset) = (u64_at(record, 40), u64_at(record, 48));
        if let Some(why) = too_large(on_disk.max(entries), bytes) {
            return Ok(Zip64::Refused(why));
        }
        let record_at = named + at as u64;
        let consistent = on_disk_of == directory_disk
            && disk == 0
            && on_disk == entries
            && entries
                .saturating_mul(CENTRAL_HEADER_LEN)
                .saturating_add(offset)
                <= record_at;
        if at == 0 && consistent {
            taken = Some(Directory {
                entries,
                offset,
                bytes,
                end: record_at,
            });
        }
    }
    Ok(Zip64::Judged(taken))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
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

    /// A stored ZIP of one member, with `comment`.
    fn stored_zip(payload: &[u8], comment: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("a.bin", options).unwrap();
        zip.write_all(payload).unwrap();
        zip.set_raw_comment(comment.into()).unwrap();
        zip.finish().unwrap().into_inner()
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

    /// Asserts `bytes` passes the bound, and probes and audits as a ZIP holding `payload`.
    fn reads_normally(bytes: &[u8], payload: &[u8]) {
        assert_eq!(check_directory(&file_with(bytes), &INGEST).unwrap(), None);
        assert_eq!(
            check_directory(&file_with(bytes), &DirectoryLimits::default()).unwrap(),
            None
        );
        let probe = crate::build::probe(&file_with(bytes)).unwrap();
        assert_eq!(probe.error, None);
        assert_eq!(probe.payloads.len(), 1, "{probe:?}");

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
        let [(_, Ok(Known::Payload(sums)))] = read.members.as_slice() else {
            panic!("not one payload read");
        };
        let mut hasher = crate::payload::Hasher::new();
        hasher.update(payload);
        assert_eq!(sums.sha256, hasher.finish().sha256);
    }

    #[test]
    fn a_directory_declaring_too_many_members_or_bytes_is_refused_before_it_is_read() {
        let zip = stored_zip(b"aaaa", b"");
        let end = zip.len() - EOCD_LEN as usize;
        let forged = |entries: u16, bytes: u32| {
            let mut zip = zip.clone();
            zip[end + 8..end + 12]
                .copy_from_slice(&[entries.to_le_bytes(), entries.to_le_bytes()].concat());
            zip[end + 12..end + 16].copy_from_slice(&bytes.to_le_bytes());
            file_with(&zip)
        };

        assert!(
            check_directory(&forged(4097, 0), &INGEST)
                .unwrap()
                .is_some()
        );
        assert!(
            check_directory(&forged(1, 17 << 20), &INGEST)
                .unwrap()
                .is_some()
        );
        assert!(
            check_directory(&forged(4096, 1 << 20), &INGEST)
                .unwrap()
                .is_none()
        );
        assert!(
            check_directory(&file_with(&zip), &INGEST)
                .unwrap()
                .is_none()
        );
        assert!(
            check_directory(&file_with(b"not a zip"), &INGEST)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_zip64_directory_declaring_millions_of_members_is_refused() {
        let mut bytes = zip64_eocd(5_000_000, 0);
        bytes.extend(zip64_locator(0));
        bytes.extend(eocd(u16::MAX, 0, 0));

        let why = check_directory(&file_with(&bytes), &DirectoryLimits::default()).unwrap();

        assert!(why.is_some_and(|why| why.contains("members")));
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

        let why = check_directory(&file, &DirectoryLimits::default()).unwrap();

        assert!(why.is_some_and(|why| why.contains("ZIP64 directory record")));
        // Within the bound, the same record is judged on what it declares.
        let mut small = zip64_eocd(0, 1024);
        small.resize(ZIP64_EOCD_LEN as usize + 1024, 0);
        small.extend(zip64_locator(0));
        small.extend(eocd(u16::MAX, 0, 0));
        assert_eq!(
            check_directory(&file_with(&small), &DirectoryLimits::default()).unwrap(),
            None
        );
    }

    #[test]
    fn end_record_bytes_inside_a_torrentzip_payload_are_not_taken_for_its_end() {
        // Were it the end record, it would declare a directory of nearly 4 GiB.
        let fake = eocd(1, 0xffff_fff0, 0);
        let mut payload = noise(8192);
        payload[4000..4000 + fake.len()].copy_from_slice(&fake);
        let mut writer = crate::torrentzip::Writer::new(Cursor::new(Vec::new())).unwrap();
        writer
            .add("a.bin", payload.len() as u64, &mut payload.as_slice())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(
            bytes.windows(fake.len()).any(|w| w == fake),
            "the payload was stored as it is"
        );

        reads_normally(&bytes, &payload);
    }

    #[test]
    fn an_end_record_signature_in_the_archive_comment_is_not_taken_for_its_end() {
        let mut comment = b"repacked; see ".to_vec();
        let mut fake = eocd(60000, 0, 0);
        fake[4] = 1;
        comment.extend(&fake);
        comment.extend(b" for the format");
        let bytes = stored_zip(b"aaaa", &comment);

        reads_normally(&bytes, b"aaaa");
        // One consistent enough for the archive reader to try is still judged, since the reader would take it first.
        let plausible = stored_zip(b"aaaa", &[b"see ".as_slice(), &eocd(60000, 0, 0)].concat());
        let why = check_directory(&file_with(&plausible), &INGEST).unwrap();
        assert!(why.is_some_and(|why| why.contains("60000 members")));
    }
}
