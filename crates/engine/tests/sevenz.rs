//! 7z archives, read only in Ingest Folders (ADR 0018): probed member by member within a dictionary limit and a
//! global decode gate, and their members written out as TorrentZip or loose files.

mod common;
#[path = "common/sevenz.rs"]
mod sevenz_fixtures;

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::*;
use sevenz_fixtures::*;
use sha2::{Digest, Sha256};
use title_select_api::build::ExpectedRom;
use title_select_engine::build::{Probe, Probed, ZipLimits, verify, verify_loose};
use title_select_engine::payload::Hasher;
use title_select_engine::sevenz::{self, Decoding, Gate};

fn probed(path: &Path, max_dictionary_mib: u32) -> Probed {
    let gate = Gate::new(1);
    let decoding = Decoding {
        gate: &gate,
        max_dictionary_mib,
    };
    sevenz::probe(&File::open(path).unwrap(), &ZipLimits::default(), &decoding).unwrap()
}

fn read(path: &Path) -> Probe {
    match probed(path, 128) {
        Probed::Read(probe) => probe,
        Probed::Refused(why) => panic!("refused: {why}"),
    }
}

fn refusal(path: &Path, max_dictionary_mib: u32) -> String {
    match probed(path, max_dictionary_mib) {
        Probed::Refused(why) => why,
        Probed::Read(probe) => panic!("read: {probe:?}"),
    }
}

fn expected(name: &str, bytes: &[u8]) -> ExpectedRom {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    let sums = hasher.finish();
    ExpectedRom {
        name: name.into(),
        size: sums.size,
        crc: Some(sums.crc),
        md5: Some(sums.md5),
        sha1: Some(sums.sha1),
        sha256: Some(sums.sha256),
        header: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn created(path: &Path) -> File {
    File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
}

fn disc() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("Game (USA)/Disc 1/Track 02.bin", noise(2, 5_000)),
        ("Game (USA)/Disc 1/Track 01.bin", noise(1, 70_000)),
        (
            "Game (USA)/Game (USA).cue",
            b"FILE \"Track 01.bin\" BINARY".to_vec(),
        ),
    ]
}

fn borrowed<'a>(members: &'a [(&'static str, Vec<u8>)]) -> Vec<(&'static str, &'a [u8])> {
    members.iter().map(|(n, b)| (*n, b.as_slice())).collect()
}

#[test]
fn every_member_of_a_solid_or_non_solid_archive_of_each_method_is_read_in_nested_folders() {
    let dir = tempfile::tempdir().unwrap();
    let members = disc();
    for method in Method::ALL {
        for solid in [true, false] {
            let path = dir.path().join(format!("{method:?}-{solid}.7z"));
            write_7z(&path, &borrowed(&members), method, solid);

            let probe = read(&path);

            let bytes = std::fs::read(&path).unwrap();
            assert!(probe.sevenz && !probe.zip && !probe.torrentzip, "{probe:?}");
            assert_eq!(probe.error, None, "{method:?} solid {solid}");
            assert_eq!(
                (probe.sha256.as_str(), probe.size),
                (sha256_hex(&bytes).as_str(), bytes.len() as u64)
            );
            let read: Vec<_> = probe
                .payloads
                .iter()
                .map(|p| (p.name.clone().unwrap(), p.sha1.clone(), p.size))
                .collect();
            let want: Vec<_> = members
                .iter()
                .map(|(name, b)| {
                    (
                        name.to_string(),
                        expected(name, b).sha1.unwrap(),
                        b.len() as u64,
                    )
                })
                .collect();
            assert_eq!(read, want, "{method:?} solid {solid}");
        }
    }
}

#[test]
fn an_archive_whose_dictionary_passes_the_limit_is_refused_before_it_is_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let message = |n: u64, m: u32| {
        format!(
            "this 7z needs a {n} MiB dictionary, more than max_dictionary_mib ({m}); unpack it yourself or raise \
             the limit"
        )
    };
    let lzma2 = dir.path().join("lzma2.7z");
    crafted_7z(
        &lzma2,
        &[0x21],
        &[LZMA2_1536_MIB],
        "Game.bin",
        &[0; 64],
        1 << 20,
    );
    assert_eq!(refusal(&lzma2, 1024), message(1536, 1024));

    let lzma = dir.path().join("lzma.7z");
    let mut props = vec![0x5D];
    props.extend((256u32 << 20).to_le_bytes());
    crafted_7z(&lzma, &[3, 1, 1], &props, "Game.bin", &[0; 64], 1 << 20);
    assert_eq!(refusal(&lzma, 128), message(256, 128));

    let zstd = dir.path().join("zstd.7z");
    crafted_7z(
        &zstd,
        &[4, 0xF7, 0x11, 1],
        &[],
        "Game.bin",
        &zstd_frame_with_window(30),
        1 << 20,
    );
    assert_eq!(refusal(&zstd, 128), message(1024, 128));

    // A real archive's dictionary, read from its header, is refused under a limit below it.
    let real = dir.path().join("real.7z");
    write_7z(&real, &[("Game.bin", &noise(3, 4096))], Method::Lzma2, true);
    assert!(refusal(&real, 1).starts_with("this 7z needs a "));
}

#[test]
fn encrypted_damaged_or_unsupported_archives_are_read_as_unreadable_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let error = |path: &Path| read(path).error.unwrap_or_else(|| panic!("{path:?} read"));

    let encrypted = dir.path().join("encrypted.7z");
    crafted_7z(
        &encrypted,
        &[6, 0xF1, 7, 1],
        &[0; 2],
        "Game.bin",
        &[0; 64],
        64,
    );
    assert!(
        error(&encrypted).contains("encrypted"),
        "{}",
        error(&encrypted)
    );

    let ppmd = dir.path().join("ppmd.7z");
    crafted_7z(
        &ppmd,
        &[3, 4, 1],
        &[6, 0, 0, 0, 1],
        "Game.bin",
        &[0; 64],
        64,
    );
    assert!(error(&ppmd).contains("PPMD"), "{}", error(&ppmd));

    // A zstd window past what the decoder accepts is unreadable, even under a limit above it.
    let zstd = dir.path().join("zstd.7z");
    crafted_7z(
        &zstd,
        &[4, 0xF7, 0x11, 1],
        &[],
        "Game.bin",
        &zstd_frame_with_window(28),
        64,
    );
    let gate = Gate::new(1);
    let roomy = Decoding {
        gate: &gate,
        max_dictionary_mib: 1024,
    };
    let probed = sevenz::probe(&File::open(&zstd).unwrap(), &ZipLimits::default(), &roomy).unwrap();
    assert!(
        matches!(&probed, Probed::Read(p) if p.error.as_deref().is_some_and(|e| e.contains("256 MiB zstd window"))),
        "{probed:?}"
    );

    let damaged = dir.path().join("damaged.7z");
    write_7z(
        &damaged,
        &[("Game.bin", &noise(4, 8192))],
        Method::Lzma2,
        true,
    );
    let mut bytes = std::fs::read(&damaged).unwrap();
    bytes[32 + 100] ^= 0xFF;
    std::fs::write(&damaged, &bytes).unwrap();
    assert!(error(&damaged).contains("damaged"), "{}", error(&damaged));

    let truncated = dir.path().join("truncated.7z");
    std::fs::write(&truncated, &bytes[..200]).unwrap();
    assert!(!error(&truncated).is_empty());
}

#[test]
fn an_archive_holding_another_archive_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let by_name = dir.path().join("by-name.7z");
    write_7z(&by_name, &[("Inner.zip", b"anything")], Method::Lzma2, true);
    assert!(refusal(&by_name, 128).contains("Inner.zip"));

    let by_content = dir.path().join("by-content.7z");
    write_7z(
        &by_content,
        &[("Inner.bin", b"PK\x03\x04 a zip")],
        Method::Lzma2,
        true,
    );
    assert!(refusal(&by_content, 128).contains("Inner.bin"));
}

#[test]
fn a_decode_waits_until_the_gate_has_a_free_permit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Game.7z");
    write_7z(&path, &[("Game.bin", &noise(5, 4096))], Method::Lzma2, true);
    let gate = Arc::new(Gate::new(1));
    let done = Arc::new(AtomicBool::new(false));

    let held = gate.enter();
    let decoding = {
        let (gate, done) = (gate.clone(), done.clone());
        std::thread::spawn(move || {
            let decoding = Decoding {
                gate: &gate,
                max_dictionary_mib: 128,
            };
            let file = File::open(&path).unwrap();
            let probed = sevenz::probe(&file, &ZipLimits::default(), &decoding).unwrap();
            done.store(true, Ordering::SeqCst);
            probed
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !done.load(Ordering::SeqCst),
        "decoded while the only permit was held"
    );
    drop(held);

    assert!(matches!(decoding.join().unwrap(), Probed::Read(p) if p.error.is_none()));
}

#[test]
fn no_more_archives_decode_at_once_than_the_gate_allows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Game.7z");
    write_7z(
        &path,
        &[("Game.bin", &noise(6, 2 << 20))],
        Method::Lzma,
        true,
    );
    let gate = Arc::new(Gate::new(2));
    let start = Arc::new(Barrier::new(6));

    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (gate, start, path) = (gate.clone(), start.clone(), path.clone());
            std::thread::spawn(move || {
                let decoding = Decoding {
                    gate: &gate,
                    max_dictionary_mib: 128,
                };
                start.wait();
                sevenz::probe(
                    &File::open(&path).unwrap(),
                    &ZipLimits::default(),
                    &decoding,
                )
                .unwrap()
            })
        })
        .collect();
    for thread in threads {
        assert!(matches!(thread.join().unwrap(), Probed::Read(p) if p.error.is_none()));
    }

    assert_eq!(gate.most_at_once(), 2);
}

#[test]
fn members_of_a_7z_are_repacked_as_verified_torrentzip_and_the_source_hashed_as_read() {
    let dir = tempfile::tempdir().unwrap();
    let members = disc();
    for method in Method::ALL {
        for solid in [true, false] {
            let source = dir.path().join(format!("{method:?}-{solid}.7z"));
            write_7z(&source, &borrowed(&members), method, solid);
            // The ROMs are named apart from the members, and TorrentZip writes them in another order.
            let roms: Vec<ExpectedRom> = members
                .iter()
                .map(|(name, b)| expected(name.rsplit('/').next().unwrap(), b))
                .collect();
            let named: Vec<(&ExpectedRom, &str)> = roms
                .iter()
                .zip(&members)
                .map(|(r, (m, _))| (r, *m))
                .collect();
            let gate = Gate::new(1);
            let decoding = Decoding {
                gate: &gate,
                max_dictionary_mib: 128,
            };
            let dest = created(&dir.path().join(format!("{method:?}-{solid}.zip")));

            let scratch = tempfile::tempdir_in(dir.path()).unwrap();

            let read = sevenz::repack(
                &dest,
                &named,
                &File::open(&source).unwrap(),
                &decoding,
                scratch.path(),
            )
            .unwrap();

            verify(&dest, &roms).unwrap();
            let bytes = std::fs::read(&source).unwrap();
            assert_eq!(read, (sha256_hex(&bytes), bytes.len() as u64));
        }
    }
}

#[test]
fn a_member_of_a_7z_is_written_as_a_verified_loose_file() {
    let dir = tempfile::tempdir().unwrap();
    let members = disc();
    let source = dir.path().join("Game.7z");
    write_7z(&source, &borrowed(&members), Method::Lzma2, true);
    let (member, bytes) = &members[1];
    let rom = expected("Track 01.bin", bytes);
    let gate = Gate::new(1);
    let decoding = Decoding {
        gate: &gate,
        max_dictionary_mib: 128,
    };
    let path = dir.path().join("Track 01.bin");

    let read = sevenz::write_loose(
        &created(&path),
        &rom,
        member,
        &File::open(&source).unwrap(),
        &decoding,
    )
    .unwrap();

    verify_loose(&File::open(&path).unwrap(), &rom).unwrap();
    assert_eq!(&std::fs::read(&path).unwrap(), bytes);
    let whole = std::fs::read(&source).unwrap();
    assert_eq!(read, (sha256_hex(&whole), whole.len() as u64));
}

/// Each block an archive's requested members are in is decoded once, solid or not, whatever order TorrentZip writes
/// them in: members that come before their turn wait in scratch, no more of them than their declared sizes, and are
/// gone once written.
#[test]
fn each_block_is_decoded_once_however_many_of_its_members_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut members = disc();
    members.push(("Game (USA)/Disc 1/Track 03.bin", noise(3, 9_000)));
    members.push(("Game (USA)/Unwanted.txt", noise(4, 100)));
    let wanted = &members[..4];
    let roms: Vec<ExpectedRom> = wanted
        .iter()
        .map(|(name, b)| expected(name.rsplit('/').next().unwrap(), b))
        .collect();
    let named: Vec<(&ExpectedRom, &str)> =
        roms.iter().zip(wanted).map(|(r, (m, _))| (r, *m)).collect();
    for solid in [true, false] {
        let source = dir.path().join(format!("{solid}.7z"));
        write_7z(&source, &borrowed(&members), Method::Lzma2, solid);
        let gate = Gate::new(1);
        let decoding = Decoding {
            gate: &gate,
            max_dictionary_mib: 128,
        };
        let dest = created(&dir.path().join(format!("{solid}.zip")));
        let scratch = tempfile::tempdir_in(dir.path()).unwrap();

        sevenz::repack(
            &dest,
            &named,
            &File::open(&source).unwrap(),
            &decoding,
            scratch.path(),
        )
        .unwrap();

        verify(&dest, &roms).unwrap();
        let blocks = if solid { 1 } else { wanted.len() };
        assert_eq!(gate.blocks_decoded(), blocks, "solid: {solid}");
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}

/// Every operation on a 7z, the scan's probe and both writes, waits for a permit before it reads its header: an
/// encoded header is not decoded while the only permit is held elsewhere.
#[test]
fn a_header_is_decoded_only_once_the_gate_has_a_free_permit() {
    let dir = tempfile::tempdir().unwrap();
    let members: Vec<(String, Vec<u8>)> = (0..40)
        .map(|i| (format!("Game (USA)/Track {i:02}.bin"), noise(i, 64)))
        .collect();
    let listed: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let source = dir.path().join("Game.7z");
    write_7z(&source, &listed, Method::Lzma2, true);
    assert!(has_encoded_header(&source));
    let rom = expected("Track 00.bin", &members[0].1);

    for op in ["probe", "repack", "write_loose"] {
        let gate = Arc::new(Gate::new(1));
        let held = gate.enter();
        let running = {
            let (gate, source, rom, dir) = (
                gate.clone(),
                source.clone(),
                rom.clone(),
                dir.path().to_owned(),
            );
            let member = members[0].0.clone();
            std::thread::spawn(move || {
                let decoding = Decoding {
                    gate: &gate,
                    max_dictionary_mib: 128,
                };
                let file = File::open(&source).unwrap();
                match op {
                    "probe" => matches!(
                        sevenz::probe(&file, &ZipLimits::default(), &decoding).unwrap(),
                        Probed::Read(p) if p.error.is_none()
                    ),
                    "repack" => {
                        let scratch = tempfile::tempdir_in(&dir).unwrap();
                        let dest = created(&dir.join("Game.zip"));
                        sevenz::repack(&dest, &[(&rom, &member)], &file, &decoding, scratch.path())
                            .is_ok()
                    }
                    _ => {
                        let dest = created(&dir.join("Track 00.bin"));
                        sevenz::write_loose(&dest, &rom, &member, &file, &decoding).is_ok()
                    }
                }
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            gate.headers_decoded(),
            0,
            "{op} decoded a header while the only permit was held"
        );
        assert!(
            !running.is_finished(),
            "{op} ran while the only permit was held"
        );
        drop(held);

        assert!(running.join().unwrap(), "{op}");
        assert_eq!(gate.headers_decoded(), 1, "{op}");
    }
}

/// Each zstd frame is held to the limit as it streams, not only the first: a later frame needing a larger window is
/// refused before the decoder reads its header, by its exact window, whether or not the limit is a power of two.
#[test]
fn every_zstd_frame_is_held_to_the_limit_as_it_streams() {
    let dir = tempfile::tempdir().unwrap();
    let frames = |log| {
        let mut packed = zstd_frame(b"first ", 10);
        packed.extend(zstd_frame(b"second", log));
        packed
    };
    let path = dir.path().join("Game.7z");
    crafted_7z(&path, &[4, 0xF7, 0x11, 1], &[], "Game.bin", &frames(24), 12);
    let rom = expected("Game.bin", b"first second");

    let probe = read(&path);
    assert_eq!(probe.error, None);
    assert_eq!(probe.payloads[0].sha256, rom.sha256.clone().unwrap());
    assert_eq!(
        refusal(&path, 1),
        "this 7z needs a 16 MiB dictionary, more than max_dictionary_mib (1); unpack it yourself or raise the limit"
    );
    let gate = Gate::new(1);
    let tight = Decoding {
        gate: &gate,
        max_dictionary_mib: 1,
    };
    let scratch = tempfile::tempdir_in(dir.path()).unwrap();
    let written = sevenz::repack(
        &created(&dir.path().join("Game.zip")),
        &[(&rom, "Game.bin")],
        &File::open(&path).unwrap(),
        &tight,
        scratch.path(),
    );
    assert!(
        matches!(&written, Err(why) if why.contains("this 7z needs a 16 MiB dictionary")),
        "{written:?}"
    );

    // A limit of 3 MiB lets a 2 MiB window through and refuses a 4 MiB one.
    for (log, refused) in [(21, false), (22, true)] {
        let path = dir.path().join(format!("{log}.7z"));
        crafted_7z(
            &path,
            &[4, 0xF7, 0x11, 1],
            &[],
            "Game.bin",
            &frames(log),
            12,
        );
        match probed(&path, 3) {
            Probed::Refused(why) => {
                assert!(refused && why.starts_with("this 7z needs a 4 MiB"), "{why}")
            }
            Probed::Read(probe) => assert!(!refused && probe.error.is_none(), "{probe:?}"),
        }
    }

    // Past the largest window a zstd 7z is read with, a later frame is unreadable whatever the limit.
    let path = dir.path().join("28.7z");
    crafted_7z(&path, &[4, 0xF7, 0x11, 1], &[], "Game.bin", &frames(28), 12);
    let probe = match probed(&path, 1024) {
        Probed::Read(probe) => probe,
        Probed::Refused(why) => panic!("refused: {why}"),
    };
    assert!(
        probe
            .error
            .as_deref()
            .is_some_and(|e| e.contains("256 MiB zstd window")),
        "{probe:?}"
    );
}

/// Outside an Ingest Folder a 7z is read as a library or source set file is: one opaque payload, the whole file.
#[test]
fn a_7z_probed_as_a_library_file_is_one_opaque_payload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Game.7z");
    write_7z(&path, &[("Game.bin", b"game")], Method::Lzma2, true);

    let probe = title_select_engine::build::probe(&File::open(&path).unwrap()).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert!(!probe.zip && !probe.sevenz, "{probe:?}");
    assert_eq!(probe.payloads.len(), 1);
    assert_eq!(probe.payloads[0].name, None);
    assert_eq!(probe.payloads[0].sha256, sha256_hex(&bytes));
}

/// Title Select never writes 7z: no production source names the writer, and the production dependency leaves out
/// the feature that compiles it.
#[test]
fn no_code_path_writes_7z() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for entry in walkdir::WalkDir::new(crates) {
        let entry = entry.unwrap();
        let path = entry.path();
        let in_src = path.strip_prefix(crates).is_ok_and(|p| {
            p.components()
                .nth(1)
                .is_some_and(|c| c.as_os_str() == "src")
        });
        if !in_src || path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for writer in ["ArchiveWriter", "push_archive_entr", "EncoderConfiguration"] {
            assert!(!text.contains(writer), "{} names {writer}", path.display());
        }
    }
    let manifest = std::fs::read_to_string(crates.parent().unwrap().join("Cargo.toml")).unwrap();
    let line = manifest
        .lines()
        .find(|l| l.starts_with("sevenz-rust2"))
        .unwrap();
    assert!(
        line.contains("default-features = false") && !line.contains("compress"),
        "{line}"
    );
}

/// A zstd frame shares its block's budget with the block's other dictionaries: a later frame whose window fits the
/// limit alone, but not beside an LZMA2 dictionary decoding its output, is refused as it streams.
#[test]
fn a_later_zstd_frame_is_held_to_what_the_blocks_other_dictionaries_leave() {
    let dir = tempfile::tempdir().unwrap();
    let content = b"a game chained through zstd and LZMA2";
    let lzma2 = lzma2_stored(content);
    let mut packed = zstd_frame(&lzma2[..4], 10);
    packed.extend(zstd_frame(&lzma2[4..], 21));
    let path = dir.path().join("Game.7z");
    // LZMA2 with a 3 MiB dictionary, fed by zstd.
    chained_7z(
        &path,
        &[
            (&[0x21], &[19], content.len() as u64),
            (&[4, 0xF7, 0x11, 1], &[], lzma2.len() as u64),
        ],
        "Game.bin",
        &packed,
    );
    let rom = expected("Game.bin", content);

    let probe = match probed(&path, 5) {
        Probed::Read(probe) => probe,
        Probed::Refused(why) => panic!("refused: {why}"),
    };
    assert_eq!(probe.error, None);
    assert_eq!(probe.payloads[0].sha256, rom.sha256.clone().unwrap());

    // 3 MiB and the second frame's 2 MiB window are 5 MiB at once, past a 4 MiB limit.
    assert_eq!(
        refusal(&path, 4),
        "this 7z needs a 5 MiB dictionary, more than max_dictionary_mib (4); unpack it yourself or raise the limit"
    );
    let gate = Gate::new(1);
    let tight = Decoding {
        gate: &gate,
        max_dictionary_mib: 4,
    };
    let scratch = tempfile::tempdir_in(dir.path()).unwrap();
    let written = sevenz::repack(
        &created(&dir.path().join("Game.zip")),
        &[(&rom, "Game.bin")],
        &File::open(&path).unwrap(),
        &tight,
        scratch.path(),
    );
    assert!(
        matches!(&written, Err(why) if why.contains("this 7z needs a 5 MiB dictionary")),
        "{written:?}"
    );
}
