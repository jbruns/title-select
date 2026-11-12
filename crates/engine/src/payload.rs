//! Checksums computed from an Entry's payload.

use sha1::Digest;

use title_select_api::HeaderRule;

use crate::dat::Rom;
use crate::hex;

/// A payload's checksums. When a Header Rule found a Header at its start, they are of the bytes after it, and
/// `header` is its size.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Checksums {
    pub size: u64,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<u64>,
}

/// Computes every checksum in one pass over a payload, fed in chunks. Under a Header Rule it holds back the first
/// bytes until it can tell whether they are a Header, then leaves a Header out: each byte is still hashed at most once.
#[derive(Default)]
pub struct Hasher {
    crc: crc32fast::Hasher,
    md5: md5::Md5,
    sha1: sha1::Sha1,
    sha256: sha2::Sha256,
    size: u64,
    /// The rule, and the bytes held back while it is undecided whether they start with its Header.
    undecided: Option<(HeaderRule, Vec<u8>)>,
    header: Option<u64>,
}

impl Hasher {
    pub fn new() -> Hasher {
        Hasher::default()
    }

    /// A hasher that leaves out the Header `rule` finds at the payload's start, if any.
    pub fn with_header(rule: Option<HeaderRule>) -> Hasher {
        Hasher {
            undecided: rule.map(|r| (r, Vec::new())),
            ..Hasher::default()
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        let Some((rule, held)) = &mut self.undecided else {
            self.hash(chunk);
            return;
        };
        held.extend_from_slice(chunk);
        // A payload with more bytes than the Header is long enough to tell.
        if held.len() as u64 > rule.size() {
            self.decide();
        }
    }

    /// Hashes the bytes held back, leaving out the Header if they start with one.
    fn decide(&mut self) {
        let Some((rule, held)) = self.undecided.take() else {
            return;
        };
        if rule.starts(&held, held.len() as u64) {
            self.header = Some(rule.size());
            self.hash(&held[rule.size() as usize..]);
        } else {
            self.hash(&held);
        }
    }

    fn hash(&mut self, chunk: &[u8]) {
        self.crc.update(chunk);
        self.md5.update(chunk);
        self.sha1.update(chunk);
        self.sha256.update(chunk);
        self.size += chunk.len() as u64;
    }

    pub fn finish(mut self) -> Checksums {
        self.decide();
        Checksums {
            size: self.size,
            crc: format!("{:08x}", self.crc.finalize()),
            md5: hex(&self.md5.finalize()),
            sha1: hex(&self.sha1.finalize()),
            sha256: hex(&self.sha256.finalize()),
            header: self.header,
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
