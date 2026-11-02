//! Reading payloads with bounded workers: `readers` threads open files and read (and decompress) their payloads,
//! handing each payload's bytes in chunks to one of `hashers` threads that checksums it. A file whose earlier read
//! passes the change test is not read again, and a quick Audit claims a ZIP archive's members from its directory.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};

use crate::payload::{Checksums, Hasher};
use crate::{AuditEvidence, FileStamp, PayloadRead, PayloadReads, Progress, Workers};

const CHUNK: usize = 1 << 16;
/// Chunks a reader may get ahead of its hash worker.
const CHUNKS_IN_FLIGHT: usize = 4;

/// What an Audit knows of one payload's checksums.
pub enum Known {
    /// Every checksum, computed from its bytes.
    Payload(Checksums),
    /// Its size and CRC32 as an archive's directory gives them.
    Claim { size: u64, crc: String },
}

/// Where a file's checksums came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Its payloads were read, starting at this time.
    Read(jiff::Timestamp),
    /// An earlier read, made at this time, was reused under the change test.
    Reused(jiff::Timestamp),
    /// Its members were claimed from its directory.
    Claimed,
}

/// The Entries of one file, in archive order, and an error about the file as a whole. A member that fails to read
/// has its own error, and the rest of the archive is still read; an error about the archive ends it.
pub struct FileRead {
    pub members: Vec<(Option<String>, Result<Known, String>)>,
    pub error: Option<String>,
    pub origin: Origin,
}

/// A file to read: where it is, whether it is a ZIP archive, and its path relative to the Source Set.
pub struct ToRead {
    pub path: PathBuf,
    pub is_zip: bool,
    pub rel: String,
}

/// How each file is read.
#[derive(Clone, Copy)]
pub struct How<'a> {
    pub evidence: AuditEvidence,
    pub reads: Option<&'a dyn PayloadReads>,
}

/// One payload for a hash worker: its chunks, and where to send its checksums.
struct HashJob {
    chunks: Receiver<Vec<u8>>,
    done: SyncSender<Checksums>,
}

/// Reads every file in `files`, returning one [`FileRead`] per file in the same order.
pub fn read_files(
    files: &[ToRead],
    how: How,
    workers: Workers,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Vec<FileRead> {
    let readers = workers.readers.clamp(1, files.len().max(1));
    let hashers = workers.hashers.max(1);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileRead>>> = Mutex::new(files.iter().map(|_| None).collect());
    let (jobs, queue) = sync_channel::<HashJob>(0);
    let queue = Mutex::new(queue);
    std::thread::scope(|s| {
        for _ in 0..hashers {
            s.spawn(|| {
                loop {
                    let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(job) = job else { break };
                    let mut hasher = Hasher::new();
                    for chunk in job.chunks {
                        hasher.update(&chunk);
                    }
                    let _ = job.done.send(hasher.finish());
                }
            });
        }
        for _ in 0..readers {
            let jobs = jobs.clone();
            let (next, done, results) = (&next, &done, &results);
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else {
                        break;
                    };
                    let read = read_or_reuse(file, how, &jobs);
                    results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(read);
                    let files_done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(progress) = progress {
                        progress(Progress {
                            files_done,
                            files_total: files.len(),
                        });
                    }
                }
            });
        }
        // The hash workers stop once every reader has dropped its sender.
        drop(jobs);
    });
    results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every file is read"))
        .collect()
}

/// Reuses the file's earlier read if it passes the change test; else claims a ZIP archive's members in a quick
/// Audit; else reads its payloads, recording the read when every one of them was read.
fn read_or_reuse(file: &ToRead, how: How, jobs: &SyncSender<HashJob>) -> FileRead {
    if let Some(reads) = how.reads {
        let now = std::fs::metadata(&file.path)
            .ok()
            .and_then(|m| FileStamp::from_metadata(&m));
        if let (Some(now), Some(prior)) = (now, reads.prior(&file.rel))
            && prior.stamp == now
        {
            reads.reused(&file.rel, &now);
            return FileRead {
                members: prior
                    .members
                    .into_iter()
                    .map(|(m, sums)| (m, Ok(Known::Payload(sums))))
                    .collect(),
                error: None,
                origin: Origin::Reused(prior.read_at),
            };
        }
    }
    if how.evidence == AuditEvidence::Quick && file.is_zip {
        return claim_file(&file.path);
    }
    let read_at = jiff::Timestamp::now();
    let (read, stamp) = read_file(&file.path, file.is_zip, jobs, read_at);
    if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
        let members: Option<Vec<_>> = read
            .members
            .iter()
            .map(|(m, sums)| match sums {
                Ok(Known::Payload(sums)) => Some((m.clone(), sums.clone())),
                _ => None,
            })
            .collect();
        match members {
            Some(members) if read.error.is_none() => reads.record(
                &file.rel,
                &PayloadRead {
                    stamp,
                    read_at,
                    members,
                },
            ),
            _ => reads.record_unreadable(&file.rel, &stamp),
        }
    }
    read
}

/// Opens `file` as a ZIP archive once its central directory is known to be within bounds.
fn bounded_archive(file: File) -> Result<zip::ZipArchive<File>, String> {
    use crate::zipdir::{DirectoryLimits, Gated, open};
    match open(&file, &DirectoryLimits::default(), || file.try_clone())
        .map_err(|e| e.to_string())?
    {
        Gated::Archive(archive) => archive.map_err(|e| e.to_string()),
        Gated::Refused(why) => Err(format!("not read: {why}")),
        Gated::NotZip => Err("not read: it is not a ZIP archive".into()),
    }
}

/// Claims each member of the ZIP archive at `path` from its directory: its name, size and CRC32.
fn claim_file(path: &Path) -> FileRead {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Claimed,
    };
    let mut zip = match File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|f| bounded_archive(f).map_err(|e| e.to_string()))
    {
        Ok(zip) => zip,
        Err(e) => {
            read.error = Some(e);
            return read;
        }
    };
    for i in 0..zip.len() {
        let member = match zip.by_index_raw(i) {
            Ok(member) => member,
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        let claim = Known::Claim {
            size: member.size(),
            crc: format!("{:08x}", member.crc32()),
        };
        read.members.push((Some(name), Ok(claim)));
    }
    read
}

/// Reads every payload of the file at `path`, returning what was read and the file as it was when opened.
fn read_file(
    path: &Path,
    is_zip: bool,
    jobs: &SyncSender<HashJob>,
    read_at: jiff::Timestamp,
) -> (FileRead, Option<FileStamp>) {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Read(read_at),
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            read.error = Some(e.to_string());
            return (read, None);
        }
    };
    let stamp = file
        .metadata()
        .ok()
        .and_then(|m| FileStamp::from_metadata(&m));
    if !is_zip {
        match checksum(file, jobs) {
            Ok(sums) => read.members.push((None, Ok(Known::Payload(sums)))),
            Err(e) => read.error = Some(e),
        }
        return (read, stamp);
    }
    let mut zip = match bounded_archive(file) {
        Ok(zip) => zip,
        Err(e) => {
            read.error = Some(e);
            return (read, stamp);
        }
    };
    for i in 0..zip.len() {
        let member = match zip.by_index(i) {
            Ok(member) => member,
            Err(e) => {
                read.error = Some(e.to_string());
                return (read, stamp);
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                read.error = Some(e.to_string());
                return (read, stamp);
            }
        };
        read.members
            .push((Some(name), checksum(member, jobs).map(Known::Payload)));
    }
    (read, stamp)
}

/// Reads `r` to its end, handing its bytes to a hash worker, and returns their checksums.
fn checksum(mut r: impl Read, jobs: &SyncSender<HashJob>) -> Result<Checksums, String> {
    let (chunks, chunks_rx) = sync_channel(CHUNKS_IN_FLIGHT);
    let (done_tx, done) = sync_channel(1);
    // Waits for a free hash worker.
    jobs.send(HashJob {
        chunks: chunks_rx,
        done: done_tx,
    })
    .map_err(|_| "no hash worker is running".to_owned())?;
    loop {
        let mut buf = vec![0u8; CHUNK];
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        chunks
            .send(buf)
            .map_err(|_| "the hash worker stopped".to_owned())?;
    }
    drop(chunks);
    done.recv()
        .map_err(|_| "the hash worker stopped".to_owned())
}
