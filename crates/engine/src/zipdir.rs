//! Bounds a ZIP archive's central directory from its end-of-central-directory records alone, before an archive
//! reader is built: the reader indexes every entry the directory declares, so a directory declaring millions of
//! entries would exhaust memory before any per-member limit could be checked.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

const EOCD: [u8; 4] = *b"PK\x05\x06";
const EOCD_LEN: u64 = 22;
const ZIP64_LOCATOR: [u8; 4] = *b"PK\x06\x07";
const ZIP64_LOCATOR_LEN: u64 = 20;
const ZIP64_EOCD: [u8; 4] = *b"PK\x06\x06";
const ZIP64_EOCD_LEN: usize = 56;
/// The end record, the longest comment it can have, and the slack an archive reader searches beyond them.
const TAIL: u64 = EOCD_LEN + 4 + (1 << 16);

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

/// Why the central directory of `file` is past `limits`, if it is. Every end record in the file's tail is judged,
/// with its ZIP64 record when it has one, so the bound holds whichever one a reader settles on. A file with no end
/// record is not a ZIP archive and passes. Leaves the file's offset unspecified.
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
        let (on_disk, entries, bytes) = (u16_at(record, 8), u16_at(record, 10), u32_at(record, 12));
        if let Some(why) = too_large(on_disk.max(entries), bytes) {
            return Ok(Some(why));
        }
        let record_at = start + at as u64;
        if record_at < ZIP64_LOCATOR_LEN {
            continue;
        }
        let mut locator = [0; ZIP64_LOCATOR_LEN as usize];
        if !read_at(file, record_at - ZIP64_LOCATOR_LEN, &mut locator)?
            || locator[..4] != ZIP64_LOCATOR
        {
            continue;
        }
        let mut zip64 = [0; ZIP64_EOCD_LEN];
        let zip64_at = u64_at(&locator, 8);
        if zip64_at >= len || !read_at(file, zip64_at, &mut zip64)? || zip64[..4] != ZIP64_EOCD {
            // A reader may look for the record elsewhere; one it cannot find here is refused rather than trusted.
            return Ok(Some(
                "its ZIP64 directory record is not where it says it is, as only a crafted archive's is".into(),
            ));
        }
        let (on_disk, entries, bytes) =
            (u64_at(&zip64, 24), u64_at(&zip64, 32), u64_at(&zip64, 40));
        if let Some(why) = too_large(on_disk.max(entries), bytes) {
            return Ok(Some(why));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file_with(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    fn eocd(entries: u16, bytes: u32) -> Vec<u8> {
        let mut record = EOCD.to_vec();
        record.extend([0, 0, 0, 0]);
        record.extend(entries.to_le_bytes());
        record.extend(entries.to_le_bytes());
        record.extend(bytes.to_le_bytes());
        record.extend(0u32.to_le_bytes());
        record.extend(0u16.to_le_bytes());
        record
    }

    #[test]
    fn a_directory_declaring_too_many_members_or_bytes_is_refused_before_it_is_read() {
        let limits = DirectoryLimits {
            entries: 4096,
            bytes: 16 << 20,
        };
        assert!(
            check_directory(&file_with(&eocd(4097, 0)), &limits)
                .unwrap()
                .is_some()
        );
        assert!(
            check_directory(&file_with(&eocd(1, 17 << 20)), &limits)
                .unwrap()
                .is_some()
        );
        assert!(
            check_directory(&file_with(&eocd(4096, 1 << 20)), &limits)
                .unwrap()
                .is_none()
        );
        assert!(
            check_directory(&file_with(b"not a zip"), &limits)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_zip64_directory_declaring_millions_of_members_is_refused() {
        let mut bytes = ZIP64_EOCD.to_vec();
        bytes.extend(44u64.to_le_bytes());
        bytes.extend([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend(5_000_000u64.to_le_bytes());
        bytes.extend(5_000_000u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(ZIP64_LOCATOR);
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(eocd(1, 0));

        let why = check_directory(&file_with(&bytes), &DirectoryLimits::default()).unwrap();

        assert!(why.is_some_and(|why| why.contains("members")));
    }
}
