//! Refusing a 7z over the dictionary limit allocates nothing for its dictionary: the most memory held while it is
//! refused stays small, and the same for a dictionary twice as large. A test binary of its own, so that its counting
//! allocator sees no other test's allocations.

#[path = "common/sevenz.rs"]
mod sevenz_fixtures;

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use sevenz_fixtures::*;
use title_select_engine::build::{Probed, ZipLimits};
use title_select_engine::sevenz::{self, Decoding, Gate};

struct Counting;

static HELD: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let held = HELD.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(held, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        HELD.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most bytes held at once, above those held before, while `path` is refused under a 1 MiB limit, for a reason
/// starting `why`.
fn peak_while_refused_for(path: &Path, why: &str) -> usize {
    let file = File::open(path).unwrap();
    let gate = Gate::new(1);
    let decoding = Decoding {
        gate: &gate,
        max_dictionary_mib: 1,
    };
    let before = HELD.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let probed = sevenz::probe(&file, &ZipLimits::default(), &decoding).unwrap();
    let peak = PEAK.load(Ordering::SeqCst) - before;
    assert!(
        matches!(&probed, Probed::Refused(refused) if refused.starts_with(why)),
        "{probed:?}"
    );
    peak
}

fn peak_while_refused(path: &Path) -> usize {
    peak_while_refused_for(path, "this 7z needs a ")
}

#[test]
fn peak_memory_does_not_grow_with_a_refused_archives_dictionary() {
    let dir = tempfile::tempdir().unwrap();
    let packed = vec![0; 4096];
    let mut peaks = Vec::new();
    // 768 MiB, then 1.5 GiB.
    for (prop, name) in [(35, "768.7z"), (LZMA2_1536_MIB, "1536.7z")] {
        let path = dir.path().join(name);
        crafted_7z(&path, &[0x21], &[prop], "Game.bin", &packed, 1 << 30);
        peaks.push(peak_while_refused(&path));
    }

    for peak in &peaks {
        assert!(*peak < 1 << 20, "{peak} bytes held while refusing");
    }
    assert!(peaks[1] <= peaks[0] + (64 << 10), "{peaks:?}");
}

/// A header may list no more entries than a game's archive is read with: one listing millions, plain or encoded, is
/// refused once its count is read, before any entry is allocated. Without that, a few MiB of header would allocate
/// hundreds of MiB of entries.
#[test]
fn a_header_listing_millions_of_entries_is_refused_before_they_are_allocated() {
    let dir = tempfile::tempdir().unwrap();
    let header = header_of_many_files(3_000_000);
    let plain = dir.path().join("plain.7z");
    assemble(&plain, &[], &header);
    let encoded = dir.path().join("encoded.7z");
    encoded_7z(&encoded, &[], &header);
    assert!(has_encoded_header(&encoded));
    let why = "it lists 3000000 entries, more than the 4096 a game's archive is read with";

    let plain_peak = peak_while_refused_for(&plain, why);
    let encoded_peak = peak_while_refused_for(&encoded, why);

    // The header itself, and for the encoded one the 1 MiB dictionary it is decoded with.
    assert!(
        plain_peak < header.len() + (1 << 20),
        "{plain_peak} bytes held"
    );
    assert!(
        encoded_peak < header.len() + (4 << 20),
        "{encoded_peak} bytes held"
    );
}

/// A header larger than any game's archive needs is refused from the size the archive's start declares, unread.
#[test]
fn a_header_too_large_to_be_a_games_is_refused_unread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.7z");
    assemble(&path, &[], &header_of_many_files(9 << 20));

    let peak = peak_while_refused_for(
        &path,
        "its header is 10 MiB, more than the 8 MiB a 7z's header is read to",
    );

    assert!(peak < 64 << 10, "{peak} bytes held");
}
