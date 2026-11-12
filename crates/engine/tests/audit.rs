mod common;

use common::*;
use sha2::Digest;
use title_select_engine::{AvailableElsewhere, EvidenceLevel, Finding, GameStatus, audit};

#[test]
fn loose_file_whose_payload_matches_its_named_rom_is_matched() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].finding, Finding::Matched);
    assert_eq!(report.games[0].status, GameStatus::Complete);
}

fn finding_of(report: &title_select_engine::Report, path: &str) -> Finding {
    report
        .entries
        .iter()
        .find(|e| e.path == path)
        .unwrap()
        .finding
}

fn status_of(report: &title_select_engine::Report, game: &str) -> GameStatus {
    report.games.iter().find(|g| g.name == game).unwrap().status
}

#[test]
fn payload_of_a_rom_under_a_name_the_dat_lacks_is_misnamed_and_completes_that_game() {
    let f = Fixture::new();
    let dat = f.dat(&[(
        "Asteroids (USA)",
        vec![rom("Asteroids (USA).a26", b"rocks")],
    )]);
    write_file(&f.set_path("Asteroids (USA) (Anthology).a26"), b"rocks");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(
        finding_of(&report, "Asteroids (USA) (Anthology).a26"),
        Finding::Misnamed
    );
    assert_eq!(status_of(&report, "Asteroids (USA)"), GameStatus::Complete);
}

#[test]
fn payload_of_one_rom_under_another_roms_name_is_conflicting_and_completes_the_payloads_game() {
    let f = Fixture::new();
    let dat = f.dat(&[
        (
            "Breakout (Europe)",
            vec![rom("Breakout (Europe).a26", b"rev0")],
        ),
        (
            "Breakout (Europe) (Rev 1)",
            vec![rom("Breakout (Europe) (Rev 1).a26", b"rev1")],
        ),
    ]);
    write_file(&f.set_path("Breakout (Europe).a26"), b"rev1");

    let report = audit(&dat, f.set.path()).unwrap();

    let entry = &report.entries[0];
    assert_eq!(entry.finding, Finding::Conflicting);
    assert_eq!(entry.roms[0].game, "Breakout (Europe) (Rev 1)");
    assert_eq!(entry.named_rom.as_ref().unwrap().game, "Breakout (Europe)");
    assert_eq!(
        status_of(&report, "Breakout (Europe) (Rev 1)"),
        GameStatus::Complete
    );
    assert_eq!(status_of(&report, "Breakout (Europe)"), GameStatus::Missing);
}

#[test]
fn named_rom_whose_payload_matches_no_rom_is_mismatched() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong, corrupted");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries[0].finding, Finding::Mismatched);
    assert_eq!(
        report.entries[0].named_rom.as_ref().unwrap().name,
        "Pong (USA).a26"
    );
    assert_eq!(status_of(&report, "Pong (USA)"), GameStatus::Missing);
}

#[test]
fn file_matching_no_rom_by_name_or_payload_is_unmatched() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Homebrew.a26"), b"homebrew");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries[0].finding, Finding::Unmatched);
    assert!(report.entries[0].roms.is_empty());
}

#[test]
fn zip_members_are_entries_and_a_partly_present_multi_rom_game_is_incomplete() {
    let f = Fixture::new();
    let dat = f.dat(&[(
        "Music Game (USA)",
        vec![
            rom("Music Game (USA).a26", b"code"),
            rom("Track 1.wav", b"track one"),
            rom("Track 2.wav", b"track two"),
        ],
    )]);
    write_zip(
        &f.set_path("Music Game (USA).zip"),
        &[
            ("Music Game (USA).a26", b"code"),
            ("Track 1.wav", b"track one"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let members: Vec<_> = report
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.member.as_deref(), e.finding))
        .collect();
    assert_eq!(
        members,
        [
            (
                "Music Game (USA).zip",
                Some("Music Game (USA).a26"),
                Finding::Matched
            ),
            (
                "Music Game (USA).zip",
                Some("Track 1.wav"),
                Finding::Matched
            ),
        ]
    );
    let game = &report.games[0];
    assert_eq!(game.status, GameStatus::Incomplete);
    assert_eq!(game.missing_roms, ["Track 2.wav"]);
}

#[test]
fn archive_that_cannot_be_read_is_one_unreadable_entry_and_the_audit_continues() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Broken.zip"), b"PK\x03\x04 not really a zip");
    write_file(&f.set_path("Pong (USA).a26"), b"pong");

    let report = audit(&dat, f.set.path()).unwrap();

    let broken = &report.entries[0];
    assert_eq!(
        (broken.path.as_str(), broken.finding),
        ("Broken.zip", Finding::Unreadable)
    );
    assert!(broken.reason.as_deref().is_some_and(|r| !r.is_empty()));
    assert_eq!(finding_of(&report, "Pong (USA).a26"), Finding::Matched);
}

#[test]
fn zip_member_failing_its_crc_is_unreadable() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    let zip = f.set_path("Pong (USA).zip");
    write_stored_zip(&zip, &[("Pong (USA).a26", b"pong")]);
    // Stored member: flip its payload bytes in place so the stored CRC no longer agrees.
    let mut bytes = std::fs::read(&zip).unwrap();
    let at = bytes.windows(4).position(|w| w == b"pong").unwrap();
    bytes[at..at + 4].copy_from_slice(b"PONG");
    std::fs::write(&zip, bytes).unwrap();

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries[0].member.as_deref(), Some("Pong (USA).a26"));
    assert_eq!(report.entries[0].finding, Finding::Unreadable);
}

#[test]
fn evidence_records_the_algorithms_each_rom_supplies() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("New (USA)", vec![rom("New (USA).a26", b"new")]),
        (
            "Old (USA)",
            vec![rom_without_sha256("Old (USA).a26", b"old")],
        ),
    ]);
    write_file(&f.set_path("New (USA).a26"), b"new");
    write_file(&f.set_path("Old (USA).a26"), b"old");

    let report = audit(&dat, f.set.path()).unwrap();

    let algorithms = |path| {
        let e = report.entries.iter().find(|e| e.path == path).unwrap();
        let evidence = e.evidence.as_ref().unwrap();
        assert_eq!(evidence.level, EvidenceLevel::Payload);
        evidence.algorithms.clone()
    };
    assert_eq!(
        algorithms("New (USA).a26"),
        ["size", "crc32", "md5", "sha1", "sha256"]
    );
    assert_eq!(
        algorithms("Old (USA).a26"),
        ["size", "crc32", "md5", "sha1"]
    );
}

#[test]
fn report_pins_the_dat_revision_and_summarises_findings_and_game_statuses() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    write_file(&f.set_path("Homebrew.a26"), b"homebrew");

    let report = audit(&dat, f.set.path()).unwrap();

    let expected_sha256: String = sha2::Sha256::digest(std::fs::read(&dat).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(report.dat_revision.sha256, expected_sha256);
    assert_eq!(report.dat_revision.name, "Test - System");
    assert_eq!(report.dat_revision.version, "20260101-000000");
    assert_eq!(report.summary.findings.get(&Finding::Matched), Some(&1));
    assert_eq!(report.summary.findings.get(&Finding::Unmatched), Some(&1));
    assert_eq!(report.summary.games.get(&GameStatus::Complete), Some(&1));
    assert_eq!(report.summary.games.get(&GameStatus::Missing), Some(&1));
    assert!(report.started_at <= report.finished_at);
}

fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(|e| e.unwrap())
        .map(|e| {
            let bytes = if e.file_type().is_file() {
                std::fs::read(e.path()).unwrap()
            } else {
                Vec::new()
            };
            (
                e.path().to_path_buf(),
                bytes,
                e.metadata().unwrap().modified().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn audit_leaves_the_source_set_unchanged() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_zip(
        &f.set_path("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    write_file(&f.set_path("sub/Homebrew.a26"), b"homebrew");
    write_file(&f.set_path("Broken.zip"), b"not a zip");
    let before = snapshot(f.set.path());

    audit(&dat, f.set.path()).unwrap();

    assert_eq!(snapshot(f.set.path()), before);
}

#[test]
fn rom_listed_without_a_crc_is_matched_by_its_other_checksums() {
    let f = Fixture::new();
    let dat = f.dat(&[(
        "Prototype (USA)",
        vec![rom_without_crc("Prototype (USA).a26", b"proto")],
    )]);
    write_file(&f.set_path("Prototype (USA).a26"), b"proto");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries[0].finding, Finding::Matched);
    assert_eq!(
        report.entries[0].evidence.as_ref().unwrap().algorithms,
        ["size", "sha1"]
    );
    assert_eq!(status_of(&report, "Prototype (USA)"), GameStatus::Complete);
}

#[test]
fn identical_rom_shared_by_two_games_completes_both() {
    let f = Fixture::new();
    let dat = f.dat(&[
        (
            "Game A (USA)",
            vec![rom("bios.bin", b"bios"), rom("a.bin", b"a")],
        ),
        (
            "Game B (USA)",
            vec![rom("bios.bin", b"bios"), rom("b.bin", b"b")],
        ),
    ]);
    write_zip(
        &f.set_path("Game A (USA).zip"),
        &[("bios.bin", b"bios"), ("a.bin", b"a")],
    );
    write_zip(
        &f.set_path("Game B (USA).zip"),
        &[("bios.bin", b"bios"), ("b.bin", b"b")],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(status_of(&report, "Game A (USA)"), GameStatus::Complete);
    assert_eq!(status_of(&report, "Game B (USA)"), GameStatus::Complete);
}

#[test]
fn mismatched_entry_evidence_is_only_the_size_and_crc32_lookup() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"gnop");

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries[0].finding, Finding::Mismatched);
    assert_eq!(
        report.entries[0].evidence.as_ref().unwrap().algorithms,
        ["size", "crc32"]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_files_are_audited_and_an_unreadable_directory_is_unreadable_without_stopping_the_audit()
 {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.dat_dir.path().join("elsewhere.a26"), b"pong");
    std::os::unix::fs::symlink(
        f.dat_dir.path().join("elsewhere.a26"),
        f.set_path("Pong (USA).a26"),
    )
    .unwrap();
    write_file(&f.set_path("locked/Homebrew.a26"), b"homebrew");
    std::fs::set_permissions(f.set_path("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let report = audit(&dat, f.set.path());
    std::fs::set_permissions(f.set_path("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();

    assert_eq!(finding_of(&report, "Pong (USA).a26"), Finding::Matched);
    assert_eq!(finding_of(&report, "locked"), Finding::Unreadable);
}

#[test]
fn many_workers_produce_the_same_report_and_report_progress_for_every_file() {
    let fx = Fixture::new();
    let mut games = Vec::new();
    for i in 0..12 {
        let name: &'static str = Box::leak(format!("Game {i:02}.bin").into_boxed_str());
        let payload = format!("payload {i}").repeat(10_000 * (i + 1));
        games.push((name, vec![rom(name, payload.as_bytes())]));
        if i % 2 == 0 {
            write_file(&fx.set_path(name), payload.as_bytes());
        } else {
            let zip_name = format!("Game {i:02}.zip");
            write_zip(&fx.set_path(&zip_name), &[(name, payload.as_bytes())]);
        }
    }
    let games: Vec<(&str, Vec<RomSpec>)> = games;
    let dat = fx.dat(&games);
    let run = |workers: title_select_engine::Workers| {
        let seen = std::sync::Mutex::new(Vec::new());
        let progress = |p: title_select_engine::Progress| seen.lock().unwrap().push(p);
        let options = title_select_engine::Options {
            file: "test.dat".into(),
            confine: None,
            workers,
            progress: Some(&progress),
            evidence: title_select_engine::AuditEvidence::Full,
            reads: None,
        };
        let report = title_select_engine::audit_with(&dat, fx.set.path(), &options).unwrap();
        (report, seen.into_inner().unwrap())
    };

    let (one, one_progress) = run(title_select_engine::ONE_WORKER_EACH);
    let (many, many_progress) = run(title_select_engine::Workers {
        readers: 4,
        hashers: 3,
    });
    assert_eq!(many.summary.findings.get(&Finding::Matched), Some(&12));

    // Each payload's read time is its own.
    let unread = |report: title_select_engine::Report| {
        let mut entries = report.entries;
        for e in &mut entries {
            e.evidence.as_mut().unwrap().read_at = None;
        }
        entries
    };
    assert_eq!(one.games, many.games);
    assert_eq!(unread(one), unread(many));

    for progress in [one_progress, many_progress] {
        assert_eq!(progress.len(), 12);
        assert!(progress.iter().all(|p| p.files_total == 12));
        let mut done: Vec<_> = progress.iter().map(|p| p.files_done).collect();
        done.sort();
        assert_eq!(done, (1..=12).collect::<Vec<_>>());
    }
}

#[test]
fn title_selects_own_entries_in_a_source_set_are_not_audited() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    write_file(&f.set_path(".title-select"), b"{}");
    write_file(&f.set_path(".title-select-retained/1/old.a26"), b"old");
    write_file(&f.set_path(".title-select-retained/.deleting/7"), b"gone");
    write_file(
        &f.set_path("sub/.Pong (USA).zip.title-select-new-1-2-3"),
        b"partial",
    );
    write_file(&f.set_path(".deleting/9"), b"x");
    write_file(&f.set_path(".hidden.a26"), b"pong");

    let report = audit(&dat, f.set.path()).unwrap();

    let paths: Vec<&str> = report.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, [".hidden.a26", "Pong (USA).a26"]);
}

#[test]
fn a_disc_set_of_loose_folders_beside_zips_audits_every_track_verified() {
    let f = Fixture::new();
    // A first track larger than any read buffer, so each payload is hashed across many reads.
    let alpha = disc("Alpha (USA)", 3, (1 << 20) + 7, 1);
    let beta = disc("Beta (Europe)", 1, 4096, 2);
    let gamma = disc("Gamma (Japan)", 2, 8192, 3);
    let dat = f.dat(&[
        ("Alpha (USA)", disc("Alpha (USA)", 3, (1 << 20) + 7, 1)),
        ("Beta (Europe)", disc("Beta (Europe)", 1, 4096, 2)),
        ("Gamma (Japan)", disc("Gamma (Japan)", 2, 8192, 3)),
    ]);
    write_loose_folder(f.set.path(), "Alpha (USA)", &alpha);
    write_loose_folder(f.set.path(), "Beta (Europe)", &beta);
    let members: Vec<(&str, &[u8])> = gamma.iter().map(|r| (r.name, &r.payload[..])).collect();
    write_zip(&f.set_path("Gamma (Japan).zip"), &members);

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(report.entries.len(), 4 + 2 + 3, "{:#?}", report.entries);
    for entry in &report.entries {
        assert_eq!(entry.finding, Finding::Matched, "{entry:?}");
        let evidence = entry.evidence.as_ref().unwrap();
        assert_eq!(evidence.level, EvidenceLevel::Payload, "{entry:?}");
        assert!(
            evidence.algorithms.iter().any(|a| a == "sha256"),
            "{entry:?}"
        );
    }
    assert!(
        report
            .entries
            .iter()
            .any(|e| e.path == "Alpha (USA)/Alpha (USA) (Track 3).bin"),
        "{:#?}",
        report.entries
    );
    for game in ["Alpha (USA)", "Beta (Europe)", "Gamma (Japan)"] {
        assert_eq!(status_of(&report, game), GameStatus::Complete, "{game}");
    }
}

/// A Redump-style DAT where Game B's second track is byte-identical to Game A's.
fn shared_track_dat(f: &Fixture) -> std::path::PathBuf {
    f.dat(&[
        (
            "Game A (USA)",
            vec![
                rom("Game A (USA) (Track 1).bin", b"a data"),
                rom("Game A (USA) (Track 2).bin", b"shared audio"),
            ],
        ),
        (
            "Game B (Europe)",
            vec![
                rom("Game B (Europe) (Track 1).bin", b"b data"),
                rom("Game B (Europe) (Track 2).bin", b"shared audio"),
            ],
        ),
    ])
}

fn game_of<'r>(
    report: &'r title_select_engine::Report,
    name: &str,
) -> &'r title_select_engine::Game {
    report.games.iter().find(|g| g.name == name).unwrap()
}

#[test]
fn a_track_inside_another_games_zip_leaves_the_game_missing_with_that_rom_available_elsewhere() {
    let f = Fixture::new();
    let dat = shared_track_dat(&f);
    // Game A's ZIP from before a rename: its members match by payload only, so each counts towards every Game its
    // payload matches.
    write_zip(
        &f.set_path("Game A (USA).zip"),
        &[
            ("Old Title (USA) (Track 1).bin", b"a data"),
            ("Old Title (USA) (Track 2).bin", b"shared audio"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let a = game_of(&report, "Game A (USA)");
    assert_eq!(a.status, GameStatus::Complete);
    assert!(a.available_elsewhere.is_empty());
    let b = game_of(&report, "Game B (Europe)");
    assert_eq!(b.status, GameStatus::Missing);
    assert_eq!(
        b.missing_roms,
        [
            "Game B (Europe) (Track 1).bin",
            "Game B (Europe) (Track 2).bin"
        ]
    );
    assert_eq!(
        b.available_elsewhere,
        [AvailableElsewhere {
            rom: "Game B (Europe) (Track 2).bin".into(),
            games: vec!["Game A (USA)".into()],
        }]
    );
    assert_eq!(report.summary.games[&GameStatus::Missing], 1);
    // Per-Entry Findings are unchanged: the shared member still matches both ROMs.
    let shared = report
        .entries
        .iter()
        .find(|e| e.member.as_deref() == Some("Old Title (USA) (Track 2).bin"))
        .unwrap();
    assert_eq!(shared.finding, Finding::Misnamed);
    assert_eq!(shared.roms.len(), 2);
}

#[test]
fn a_misnamed_track_in_another_games_folder_is_available_elsewhere() {
    let f = Fixture::new();
    let dat = shared_track_dat(&f);
    write_file(
        &f.set_path("Game A (USA)/Game A (USA) (Track 1).bin"),
        b"a data",
    );
    write_file(&f.set_path("Game A (USA)/Track 02.bin"), b"shared audio");
    write_file(
        &f.set_path("Game B (Europe)/Game B (Europe) (Track 1).bin"),
        b"b data",
    );

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(status_of(&report, "Game A (USA)"), GameStatus::Complete);
    let b = game_of(&report, "Game B (Europe)");
    assert_eq!(b.status, GameStatus::Incomplete);
    assert_eq!(b.missing_roms, ["Game B (Europe) (Track 2).bin"]);
    assert_eq!(
        b.available_elsewhere,
        [AvailableElsewhere {
            rom: "Game B (Europe) (Track 2).bin".into(),
            games: vec!["Game A (USA)".into()],
        }]
    );
}

#[test]
fn a_misnamed_loose_file_or_a_member_of_the_games_own_zip_still_counts_towards_it() {
    let f = Fixture::new();
    let dat = shared_track_dat(&f);
    write_zip(
        &f.set_path("Game B (Europe).zip"),
        &[("Track 01.bin", b"b data")],
    );
    write_file(&f.set_path("shared.bin"), b"shared audio");

    let report = audit(&dat, f.set.path()).unwrap();

    let b = game_of(&report, "Game B (Europe)");
    assert_eq!(b.status, GameStatus::Complete);
    assert!(b.available_elsewhere.is_empty());
    assert_eq!(status_of(&report, "Game A (USA)"), GameStatus::Incomplete);
}
