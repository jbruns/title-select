//! Checksums computed from an Entry's payload.

use sha1::Digest;

use crate::dat::Rom;
use crate::hex;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Checksums {
    pub size: u64,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

/// Computes every checksum in one pass over a payload, fed in chunks.
#[derive(Default)]
pub struct Hasher {
    crc: crc32fast::Hasher,
    md5: md5::Md5,
    sha1: sha1::Sha1,
    sha256: sha2::Sha256,
    size: u64,
}

impl Hasher {
    pub fn new() -> Hasher {
        Hasher::default()
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.crc.update(chunk);
        self.md5.update(chunk);
        self.sha1.update(chunk);
        self.sha256.update(chunk);
        self.size += chunk.len() as u64;
    }

    pub fn finish(self) -> Checksums {
        Checksums {
            size: self.size,
            crc: format!("{:08x}", self.crc.finalize()),
            md5: hex(&self.md5.finalize()),
            sha1: hex(&self.sha1.finalize()),
            sha256: hex(&self.sha256.finalize()),
        }
    }
}

impl Checksums {
    pub fn matches(&self, rom: &Rom) -> bool {
        rom.size == self.size
            && rom.crc.as_deref().is_none_or(|v| v == self.crc)
            && rom.md5.as_deref().is_none_or(|v| v == self.md5)
            && rom.sha1.as_deref().is_none_or(|v| v == self.sha1)
            && rom.sha256.as_deref().is_none_or(|v| v == self.sha256)
    }
}
