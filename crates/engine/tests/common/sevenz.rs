//! Synthetic 7z archives for tests, written at test time: by `sevenz-rust2`'s writer, which only tests enable, or by
//! hand, for headers no writer makes, such as a huge dictionary or an encrypted coder.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use sevenz_rust2::{
    ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, SourceReader,
};

/// The compression method a test archive is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Lzma,
    Lzma2,
    Zstd,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::Lzma, Method::Lzma2, Method::Zstd];

    fn encoder(self) -> EncoderMethod {
        match self {
            Method::Lzma => EncoderMethod::LZMA,
            Method::Lzma2 => EncoderMethod::LZMA2,
            Method::Zstd => EncoderMethod::ZSTD,
        }
    }
}

/// Writes a 7z at `path` holding `members`, by name, `/` separating folders: one block for all of them when `solid`,
/// else one block each.
pub fn write_7z(path: &Path, members: &[(&str, &[u8])], method: Method, solid: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = ArchiveWriter::new(fs::File::create(path).unwrap()).unwrap();
    writer.set_content_methods(vec![EncoderConfiguration::new(method.encoder())]);
    if solid {
        let entries = members
            .iter()
            .map(|(name, _)| ArchiveEntry::new_file(name))
            .collect();
        let readers = members
            .iter()
            .map(|(_, payload)| SourceReader::new(*payload))
            .collect();
        writer.push_archive_entries(entries, readers).unwrap();
    } else {
        for (name, payload) in members {
            writer
                .push_archive_entry(ArchiveEntry::new_file(name), Some(*payload))
                .unwrap();
        }
    }
    writer.finish().unwrap();
}

/// LZMA2's dictionary property for a dictionary of `2 | (p & 1)` << (`p / 2` + 11) bytes: 37 is 1.5 GiB.
pub const LZMA2_1536_MIB: u8 = 37;

/// Writes a 7z at `path`, its header written by hand, holding one member `name` of `size` bytes in one block of one
/// coder, `method` with `props`, its packed stream `packed`. Nothing checks that `packed` decodes to anything.
pub fn crafted_7z(path: &Path, method: &[u8], props: &[u8], name: &str, packed: &[u8], size: u64) {
    let mut header = vec![0x01, 0x04];
    // Pack info: one packed stream at the start.
    header.extend([0x06, 0x00, 0x01, 0x09]);
    number(&mut header, packed.len() as u64);
    header.push(0x00);
    // Unpack info: one folder of one coder.
    header.extend([0x07, 0x0B, 0x01, 0x00, 0x01]);
    let flags = method.len() as u8 | if props.is_empty() { 0 } else { 0x20 };
    header.push(flags);
    header.extend(method);
    if !props.is_empty() {
        number(&mut header, props.len() as u64);
        header.extend(props);
    }
    header.push(0x0C);
    number(&mut header, size);
    // The end of the unpack info, a substreams info of one stream per folder, and the end of the streams info.
    header.extend([0x00, 0x08, 0x00, 0x00]);
    // Files info: the one member's name, UTF-16LE and terminated.
    let mut utf16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    utf16.extend([0, 0]);
    header.extend([0x05, 0x01, 0x11]);
    number(&mut header, utf16.len() as u64 + 1);
    header.push(0x00);
    header.extend(utf16);
    header.extend([0x00, 0x00]);

    let mut start = Vec::with_capacity(20);
    start.extend((packed.len() as u64).to_le_bytes());
    start.extend((header.len() as u64).to_le_bytes());
    start.extend(crc32fast::hash(&header).to_le_bytes());
    let mut file = b"7z\xbc\xaf\x27\x1c\x00\x04".to_vec();
    file.extend(crc32fast::hash(&start).to_le_bytes());
    file.extend(start);
    file.extend(packed);
    file.extend(header);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, file).unwrap();
}

/// A 7z number: the leading one bits of the first byte count the bytes after it, little-endian, and the rest of the
/// first byte holds the value's highest bits.
fn number(out: &mut Vec<u8>, value: u64) {
    for extra in 0..8u32 {
        if value < 1u64 << (7 * (extra + 1)) {
            let mask = !(0xFFu16 >> extra) as u8;
            out.push(mask | (value >> (8 * extra)) as u8);
            out.extend(&value.to_le_bytes()[..extra as usize]);
            return;
        }
    }
    out.push(0xFF);
    out.extend(value.to_le_bytes());
}

/// The first bytes of a zstd frame whose window is 2^`log` bytes, followed by noise.
pub fn zstd_frame_with_window(log: u8) -> Vec<u8> {
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, (log - 10) << 3];
    frame.extend([0x55; 32]);
    frame
}
