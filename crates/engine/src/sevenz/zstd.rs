//! zstd frames, checked as they stream out of a 7z towards the decoder.
//!
//! The zstd decoder `sevenz-rust2` builds reads frame after frame and takes each frame's window from its header,
//! with no limit of Title Select's. [`Guarded`] sits between the archive and that decoder: it follows the frames of
//! each zstd packed stream as their bytes pass, and fails the read holding a frame header whose window passes the
//! limit, so the decoder never sees that header and never allocates its window.

use std::io::{self, Read, Seek, SeekFrom};

const MAGIC: u32 = 0xFD2F_B528;
const SKIPPABLE: u32 = 0x184D_2A50;

/// Why a zstd stream is not decoded further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stop {
    /// A frame needs a window of `window` bytes, larger than the limit, beside `others` bytes of the block's other
    /// dictionaries.
    Window { window: u64, others: u64 },
    /// The stream is not zstd frames that are read.
    Bad(&'static str),
}

#[derive(Debug, Clone, Copy)]
enum State {
    /// Reading a frame's magic number.
    Magic,
    /// Reading a skippable frame's length.
    SkippableLength,
    /// Passing over bytes: a skippable frame's, a block's or a checksum's.
    Skip(u64, After),
    /// Reading a frame header's descriptor.
    Descriptor,
    /// Reading the rest of a frame header, `needs` bytes after its descriptor.
    Header { descriptor: u8, needs: usize },
    /// Reading a block header.
    Block { checksum: bool },
}

/// Where a skip leads.
#[derive(Debug, Clone, Copy)]
enum After {
    Magic,
    Block { checksum: bool },
}

/// Follows the frames of one zstd stream as its bytes pass, refusing any whose window passes `limit` bytes.
#[derive(Debug)]
pub(super) struct Frames {
    limit: u64,
    /// The block's other dictionaries, live beside each frame's window, to report what a refused frame needs.
    others: u64,
    state: State,
    held: [u8; 14],
    got: usize,
}

impl Frames {
    pub fn new(limit: u64, others: u64) -> Frames {
        Frames {
            limit,
            others,
            state: State::Magic,
            held: [0; 14],
            got: 0,
        }
    }

    /// Follows `bytes`, the next of the stream.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<(), Stop> {
        while !bytes.is_empty() {
            if let State::Skip(left, after) = self.state {
                let n = left.min(bytes.len() as u64);
                bytes = &bytes[n as usize..];
                self.state = if n == left {
                    match after {
                        After::Magic => State::Magic,
                        After::Block { checksum } => State::Block { checksum },
                    }
                } else {
                    State::Skip(left - n, after)
                };
                continue;
            }
            let needs = match self.state {
                State::Magic | State::SkippableLength => 4,
                State::Descriptor => 1,
                State::Header { needs, .. } => needs,
                State::Block { .. } => 3,
                State::Skip(..) => unreachable!(),
            };
            let n = (needs - self.got).min(bytes.len());
            self.held[self.got..self.got + n].copy_from_slice(&bytes[..n]);
            self.got += n;
            bytes = &bytes[n..];
            if self.got == needs {
                self.got = 0;
                self.state = self.next()?;
            }
        }
        Ok(())
    }

    /// The state after the field just held in full.
    fn next(&self) -> Result<State, Stop> {
        let held = &self.held;
        Ok(match self.state {
            State::Magic => match u32::from_le_bytes(held[..4].try_into().unwrap()) {
                MAGIC => State::Descriptor,
                m if m & 0xFFFF_FFF0 == SKIPPABLE => State::SkippableLength,
                _ => return Err(Stop::Bad("it is not a zstd frame")),
            },
            State::SkippableLength => State::Skip(
                u64::from(u32::from_le_bytes(held[..4].try_into().unwrap())),
                After::Magic,
            ),
            State::Descriptor => {
                let descriptor = held[0];
                if descriptor & 0x08 != 0 {
                    return Err(Stop::Bad("its frame header sets a reserved bit"));
                }
                if descriptor & 0x03 != 0 {
                    return Err(Stop::Bad("its frame needs a zstd dictionary"));
                }
                let single = descriptor & 0x20 != 0;
                let content = match descriptor >> 6 {
                    0 => usize::from(single),
                    1 => 2,
                    2 => 4,
                    _ => 8,
                };
                let needs = usize::from(!single) + content;
                if needs == 0 {
                    self.window(descriptor, &[])?
                } else {
                    State::Header { descriptor, needs }
                }
            }
            State::Header { descriptor, needs } => self.window(descriptor, &held[..needs])?,
            State::Block { checksum } => {
                let header = u32::from_le_bytes([held[0], held[1], held[2], 0]);
                let last = header & 1 != 0;
                let size = u64::from(header >> 3);
                let body = match (header >> 1) & 3 {
                    0 | 2 => size,
                    1 => 1,
                    _ => return Err(Stop::Bad("it has a reserved block type")),
                };
                match (last, checksum) {
                    (false, _) => State::Skip(body, After::Block { checksum }),
                    (true, true) => State::Skip(body + 4, After::Magic),
                    (true, false) => State::Skip(body, After::Magic),
                }
            }
            State::Skip(..) => unreachable!(),
        })
    }

    /// Checks the window a frame header declares, its descriptor and the fields after it, and goes on to its blocks.
    fn window(&self, descriptor: u8, fields: &[u8]) -> Result<State, Stop> {
        let single = descriptor & 0x20 != 0;
        let window = if single {
            let mut size = [0; 8];
            size[..fields.len()].copy_from_slice(fields);
            let size = u64::from_le_bytes(size);
            if fields.len() == 2 { size + 256 } else { size }
        } else {
            let wd = fields[0];
            let base = 1u64 << (10 + u32::from(wd >> 3));
            base + base / 8 * u64::from(wd & 7)
        };
        if window > self.limit {
            return Err(Stop::Window {
                window,
                others: self.others,
            });
        }
        Ok(State::Block {
            checksum: descriptor & 0x04 != 0,
        })
    }
}

/// Where a zstd packed stream lies in the archive, `start..end`, and how many bytes of dictionary the other coders of
/// its block hold while it is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub start: u64,
    pub end: u64,
    pub others: u64,
}

/// A zstd packed stream: its bytes in the archive, `start..end`, and the frames followed through it.
#[derive(Debug)]
struct Stream {
    start: u64,
    end: u64,
    /// How many of its bytes have been read, in order.
    read: u64,
    frames: Frames,
}

/// Reads an archive, following the frames of each of its zstd packed streams as they pass.
///
/// A zstd packed stream is read only in order, from its start: a read anywhere else in it fails, as its frames
/// could not be followed. A read never crosses a stream's start or end, so each read is followed whole or not at all.
pub(super) struct Guarded<R> {
    inner: R,
    pos: u64,
    streams: Vec<Stream>,
    /// Why a read was failed, once one was.
    pub stopped: Option<Stop>,
}

impl<R: Read + Seek> Guarded<R> {
    /// Guards `inner`, following the zstd packed streams at `streams`, refusing any frame whose window, beside its
    /// block's other dictionaries, passes `budget` bytes, or alone passes `cap` bytes.
    pub fn new(mut inner: R, streams: &[Span], budget: u64, cap: u64) -> io::Result<Guarded<R>> {
        let pos = inner.stream_position()?;
        Ok(Guarded {
            inner,
            pos,
            streams: streams
                .iter()
                .map(|span| Stream {
                    start: span.start,
                    end: span.end,
                    read: 0,
                    frames: Frames::new(budget.saturating_sub(span.others).min(cap), span.others),
                })
                .collect(),
            stopped: None,
        })
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for Guarded<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(stop) = &self.stopped {
            return Err(failed(stop));
        }
        let pos = self.pos;
        let within = self
            .streams
            .iter()
            .position(|s| s.start <= pos && pos < s.end);
        let room = match within {
            Some(i) => {
                let stream = &self.streams[i];
                if pos != stream.start + stream.read {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a zstd stream was read out of order",
                    ));
                }
                stream.end - pos
            }
            None => self
                .streams
                .iter()
                .filter(|s| s.start > pos)
                .map(|s| s.start - pos)
                .min()
                .unwrap_or(u64::MAX),
        };
        let len = (buf.len() as u64).min(room) as usize;
        let n = self.inner.read(&mut buf[..len])?;
        if let Some(i) = within {
            let stream = &mut self.streams[i];
            if let Err(stop) = stream.frames.feed(&buf[..n]) {
                let err = failed(&stop);
                self.stopped = Some(stop);
                return Err(err);
            }
            stream.read += n as u64;
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for Guarded<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        Ok(self.pos)
    }
}

fn failed(stop: &Stop) -> io::Error {
    let why = match stop {
        Stop::Window { window, .. } => format!("a zstd frame needs a {window} byte window"),
        Stop::Bad(why) => why.to_string(),
    };
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8], log: u32, checksum: bool) -> Vec<u8> {
        let mut encoder = ::zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        encoder
            .set_parameter(::zstd::zstd_safe::CParameter::WindowLog(log))
            .unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.include_checksum(checksum).unwrap();
        std::io::Write::write_all(&mut encoder, payload).unwrap();
        encoder.finish().unwrap()
    }

    /// Frames are followed however their bytes are split, through skippable frames, checksums and blocks of every
    /// kind, and the first frame past the limit stops them at its header.
    #[test]
    fn each_frame_is_followed_to_its_header_however_its_bytes_arrive() {
        let noise: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        let mut stream = frame(&noise, 18, true);
        stream.extend([0x5E, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3]);
        stream.extend(frame(&[7; 500_000], 19, false));
        let second = stream.len();
        stream.extend(frame(b"late", 21, false));

        for split in [1, 7, 4096, stream.len()] {
            let mut frames = Frames::new(1 << 20, 0);
            let mut fed = 0;
            let mut stop = None;
            for chunk in stream.chunks(split) {
                if let Err(s) = frames.feed(chunk) {
                    stop = Some(s);
                    break;
                }
                fed += chunk.len();
            }
            assert_eq!(
                stop,
                Some(Stop::Window {
                    window: 2 << 20,
                    others: 0
                }),
                "split {split}"
            );
            assert!(fed <= second + 6, "split {split}: {fed} bytes passed");
            assert!(
                fed + split > second + 5,
                "split {split}: stopped early, at {fed}"
            );
        }
    }
}
