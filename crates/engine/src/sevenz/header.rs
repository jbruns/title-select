//! A 7z's next header, walked before `sevenz-rust2` parses it.
//!
//! `sevenz-rust2` bounds the counts a header declares only by the header's length, and allocates for them before
//! anything else checks them: a few MiB of header listing millions of entries would allocate hundreds of MiB. This
//! walk reads the header in the same grammar, byte for byte, as far as the files' count, allocating nothing per
//! entry, and stops at the first count past the limit, so that the crate only ever parses a header whose counts are
//! within it. It also reads the streams of an encoded header, so that the header is decoded here, within the
//! dictionary limit, rather than by the crate.

pub(super) const K_HEADER: u8 = 0x01;
const K_END: u8 = 0x00;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS_INFO: u8 = 0x03;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
pub(super) const K_ENCODED_HEADER: u8 = 0x17;

/// The most coders one block may chain: four, as BCJ2's, the largest a 7z writer chains.
const CODERS_MAX: u64 = 4;
/// The most input streams one block's coders may take together: BCJ2's four, and one each for the others.
const IN_STREAMS_MAX: u64 = 8;

/// Why a header is not handed to the crate.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// It does not parse.
    Damaged(String),
    /// It lists more entries of some kind than an archive is read with.
    TooMany(u64),
}

fn damaged<T>(what: &str) -> Result<T, Fault> {
    Err(Fault::Damaged(what.to_string()))
}

/// One coder of a block.
#[derive(Debug)]
pub(super) struct Coder<'h> {
    pub id: &'h [u8],
    pub props: &'h [u8],
    pub simple: bool,
}

/// One block (a folder, in 7z's own terms) of coders.
#[derive(Debug)]
pub(super) struct Folder<'h> {
    pub coders: Vec<Coder<'h>>,
    /// One size per coder's output.
    pub unpack: Vec<u64>,
    pub crc: Option<u32>,
}

/// A streams info: where its packed streams are, and the blocks they decode in.
#[derive(Debug, Default)]
pub(super) struct Streams<'h> {
    pub pack_pos: u64,
    pub pack_sizes: Vec<u64>,
    pub folders: Vec<Folder<'h>>,
}

/// What a plain header lists, each count no more than the limit it was walked with.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Counts {
    pub files: usize,
    pub folders: usize,
}

/// Reads a header as `sevenz-rust2` does.
pub(super) struct Cursor<'h> {
    bytes: &'h [u8],
    at: usize,
}

impl<'h> Cursor<'h> {
    pub fn new(bytes: &'h [u8]) -> Cursor<'h> {
        Cursor { bytes, at: 0 }
    }

    pub fn byte(&mut self) -> Result<u8, Fault> {
        let b = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| Fault::Damaged("its header ends early".into()))?;
        self.at += 1;
        Ok(b)
    }

    fn take(&mut self, n: u64) -> Result<&'h [u8], Fault> {
        let left = (self.bytes.len() - self.at) as u64;
        if n > left {
            return damaged("its header ends early");
        }
        let taken = &self.bytes[self.at..self.at + n as usize];
        self.at += n as usize;
        Ok(taken)
    }

    /// A 7z number: the leading one bits of the first byte count the bytes after it.
    fn number(&mut self) -> Result<u64, Fault> {
        let first = u64::from(self.byte()?);
        let mut mask = 0x80;
        let mut value = 0;
        for i in 0..8 {
            if first & mask == 0 {
                return Ok(value | ((first & (mask - 1)) << (8 * i)));
            }
            value |= u64::from(self.byte()?) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, Fault> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

fn counted(n: u64, limit: usize) -> Result<usize, Fault> {
    if n > limit as u64 {
        return Err(Fault::TooMany(n));
    }
    Ok(n as usize)
}

/// Reads `n` digests, `n` no more than the limit: whether each is defined, then each defined one.
fn digests(c: &mut Cursor, n: usize) -> Result<Vec<Option<u32>>, Fault> {
    let all = c.byte()? != 0;
    let mut defined = Vec::with_capacity(n);
    let mut bits = 0;
    for i in 0..n {
        if all {
            defined.push(true);
            continue;
        }
        if i % 8 == 0 {
            bits = c.byte()?;
        }
        defined.push(bits & (0x80 >> (i % 8)) != 0);
    }
    defined
        .into_iter()
        .map(|d| if d { c.u32().map(Some) } else { Ok(None) })
        .collect()
}

fn folder<'h>(c: &mut Cursor<'h>) -> Result<Folder<'h>, Fault> {
    let count = c.number()?;
    if count > CODERS_MAX {
        return damaged(&format!(
            "a block chains {count} coders, more than the {CODERS_MAX} read"
        ));
    }
    let mut coders = Vec::with_capacity(count as usize);
    let (mut ins, mut outs) = (0u64, 0u64);
    for _ in 0..count {
        let flags = c.byte()?;
        let id = c.take(u64::from(flags & 0x0F))?;
        let simple = flags & 0x10 == 0;
        if simple {
            ins += 1;
        } else {
            ins = ins.saturating_add(c.number()?);
            if c.number()? != 1 {
                return damaged("a coder has other than one output");
            }
        }
        outs += 1;
        if ins > IN_STREAMS_MAX {
            return damaged(&format!(
                "a block takes more than the {IN_STREAMS_MAX} input streams read"
            ));
        }
        let props = if flags & 0x20 != 0 {
            let size = c.number()?;
            c.take(size)?
        } else {
            &[]
        };
        if flags & 0x80 != 0 {
            return damaged("a coder lists alternative methods");
        }
        coders.push(Coder { id, props, simple });
    }
    if outs == 0 {
        return damaged("a block has no coders");
    }
    let bind_pairs = outs - 1;
    for _ in 0..bind_pairs {
        c.number()?;
        c.number()?;
    }
    let packed = ins
        .checked_sub(bind_pairs)
        .filter(|&p| p > 0)
        .ok_or_else(|| Fault::Damaged("a block has no packed streams".into()))?;
    if packed > 1 {
        for _ in 0..packed {
            c.number()?;
        }
    }
    Ok(Folder {
        coders,
        unpack: Vec::with_capacity(outs as usize),
        crc: None,
    })
}

/// Reads a streams info, after its own marker, to its end, refusing any count past `limit`.
pub(super) fn streams<'h>(c: &mut Cursor<'h>, limit: usize) -> Result<Streams<'h>, Fault> {
    let mut s = Streams::default();
    let mut nid = c.byte()?;
    if nid == K_PACK_INFO {
        s.pack_pos = c.number()?;
        let n = counted(c.number()?, limit)?;
        nid = c.byte()?;
        if nid == K_SIZE {
            for _ in 0..n {
                s.pack_sizes.push(c.number()?);
            }
            nid = c.byte()?;
        }
        if nid == K_CRC {
            digests(c, n)?;
            nid = c.byte()?;
        }
        if nid != K_END {
            return damaged("its pack info is not terminated");
        }
        nid = c.byte()?;
    }
    if nid == K_UNPACK_INFO {
        if c.byte()? != K_FOLDER {
            return damaged("its unpack info lists no blocks");
        }
        let n = counted(c.number()?, limit)?;
        if c.byte()? != 0 {
            return damaged("its blocks are stored elsewhere");
        }
        for _ in 0..n {
            s.folders.push(folder(c)?);
        }
        if c.byte()? != K_CODERS_UNPACK_SIZE {
            return damaged("its blocks' sizes are missing");
        }
        for f in &mut s.folders {
            for _ in 0..f.coders.len() {
                f.unpack.push(c.number()?);
            }
        }
        nid = c.byte()?;
        if nid == K_CRC {
            for (f, crc) in s.folders.iter_mut().zip(digests(c, n)?) {
                f.crc = crc;
            }
            nid = c.byte()?;
        }
        if nid != K_END {
            return damaged("its unpack info is not terminated");
        }
        nid = c.byte()?;
    }
    if nid == K_SUB_STREAMS_INFO {
        let mut streams = vec![1u64; s.folders.len()];
        nid = c.byte()?;
        if nid == K_NUM_UNPACK_STREAM {
            let mut total = 0u64;
            for k in &mut streams {
                *k = c.number()?;
                total = total.saturating_add(*k);
                counted(total, limit)?;
            }
            nid = c.byte()?;
        }
        if nid == K_SIZE {
            for &k in &streams {
                for _ in 1..k {
                    c.number()?;
                }
            }
            nid = c.byte()?;
        }
        let num_digests: u64 = s
            .folders
            .iter()
            .zip(&streams)
            .filter(|(f, k)| **k != 1 || f.crc.is_none())
            .map(|(_, k)| k)
            .sum();
        if nid == K_CRC {
            digests(c, num_digests as usize)?;
            nid = c.byte()?;
        }
        if nid != K_END {
            return damaged("its substreams info is not terminated");
        }
        nid = c.byte()?;
    }
    if nid != K_END {
        return damaged("its streams info is not terminated");
    }
    Ok(s)
}

/// Walks a plain header, from its marker, as far as its count of files, refusing any count past `limit`.
pub(super) fn counts(header: &[u8], limit: usize) -> Result<Counts, Fault> {
    let mut c = Cursor::new(header);
    if c.byte()? != K_HEADER {
        return damaged("it has no header");
    }
    let mut nid = c.byte()?;
    if nid == K_ARCHIVE_PROPERTIES {
        nid = c.byte()?;
        while nid != K_END {
            let size = c.number()?;
            c.take(size)?;
            nid = c.byte()?;
        }
        nid = c.byte()?;
    }
    if nid == K_ADDITIONAL_STREAMS_INFO {
        return damaged("it has additional streams");
    }
    let mut folders = 0;
    if nid == K_MAIN_STREAMS_INFO {
        folders = streams(&mut c, limit)?.folders.len();
        nid = c.byte()?;
    }
    let files = if nid == K_FILES_INFO {
        counted(c.number()?, limit)?
    } else {
        0
    };
    Ok(Counts { files, folders })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_past_the_limit_is_refused_wherever_the_header_lists_it() {
        let pack_streams = [K_HEADER, K_MAIN_STREAMS_INFO, K_PACK_INFO, 0x00, 0x83, 0xE8];
        assert_eq!(counts(&pack_streams, 999), Err(Fault::TooMany(1000)));
        let folders = [K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER, 0x02];
        assert_eq!(counts(&folders, 1), Err(Fault::TooMany(2)));
        // One block of one COPY coder, unpacking to 5 bytes, split into 5000 substreams.
        let substreams = [
            K_HEADER,
            K_MAIN_STREAMS_INFO,
            K_UNPACK_INFO,
            K_FOLDER,
            0x01,
            0x00,
            0x01,
            0x01,
            0x00,
            K_CODERS_UNPACK_SIZE,
            0x05,
            K_END,
            K_SUB_STREAMS_INFO,
            K_NUM_UNPACK_STREAM,
            0x93,
            0x88,
        ];
        assert_eq!(counts(&substreams, 4096), Err(Fault::TooMany(5000)));
        let files = [K_HEADER, K_FILES_INFO, 0x05, 0x00, 0x00];
        assert_eq!(counts(&files, 4), Err(Fault::TooMany(5)));
        assert_eq!(
            counts(&files, 5),
            Ok(Counts {
                files: 5,
                folders: 0
            })
        );
    }
}
