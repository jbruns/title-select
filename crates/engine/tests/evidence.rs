//! Evidence Levels: Claim from a quick inventory, and Payload Evidence reused under the change test.

mod common;

use std::path::Path;
use std::sync::Mutex;

use common::*;
use title_select_engine::payload::Checksums;
use title_select_engine::{
    AuditEvidence, EvidenceLevel, FileStamp, Finding, GameStatus, ONE_WORKER_EACH, Options,
    PayloadRead, PayloadReads, Report, Reuse, audit_with,
};

fn run(
    dat: &Path,
    set: &Path,
    evidence: AuditEvidence,
    reads: Option<&dyn PayloadReads>,
) -> Report {
    audit_with(
        dat,
        set,
        &Options {
            file: "test.dat".into(),
            confine: None,
            workers: ONE_WORKER_EACH,
            progress: None,
            evidence,
            reads,
        },
    )
    .unwrap()
}

#[test]
fn a_quick_audit_claims_zip_members_from_the_directory_and_never_reports_payload_evidence() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]);
    write_zip(
        &f.set_path("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    write_file(&f.set_path("Combat (USA).a26"), b"combat");

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None);

    assert_eq!(report.evidence, AuditEvidence::Quick);
    let zipped = report
        .entries
        .iter()
        .find(|e| e.path == "Pong (USA).zip")
        .unwrap();
    assert_eq!(zipped.finding, Finding::Matched);
    let claim = zipped.evidence.as_ref().unwrap();
    assert_eq!(claim.level, EvidenceLevel::Claim);
    assert_eq!(claim.algorithms, ["size", "crc32"]);
    assert_eq!(claim.read_at, None, "a claim reads no payload");
    let loose = report
        .entries
        .iter()
        .find(|e| e.path == "Combat (USA).a26")
        .unwrap();
    assert_eq!(
        loose.evidence.as_ref().unwrap().level,
        EvidenceLevel::Payload,
        "a loose file has no directory to claim from, so its payload is read"
    );
}

#[test]
fn a_claim_whose_directory_crc_disagrees_with_the_dat_does_not_match() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_zip(
        &f.set_path("Pong (USA).zip"),
        &[("Pong (USA).a26", b"gnop")],
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None);

    assert_eq!(report.entries[0].finding, Finding::Mismatched);
    assert_eq!(
        report.entries[0].evidence.as_ref().unwrap().level,
        EvidenceLevel::Claim
    );
}

/// Offers one earlier read of every file and records the reads the Audit makes.
#[derive(Default)]
struct Reads {
    prior: Vec<(String, PayloadRead)>,
    recorded: Mutex<Vec<(String, PayloadRead)>>,
}

impl PayloadReads for Reads {
    fn prior(&self, path: &str) -> Option<PayloadRead> {
        self.prior
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, r)| r.clone())
    }

    fn record(&self, path: &str, read: &PayloadRead) {
        self.recorded
            .lock()
            .unwrap()
            .push((path.to_owned(), read.clone()));
    }
}

/// The checksums of `payload`, as an earlier read would have recorded them.
fn sums_of(payload: &[u8]) -> Checksums {
    let mut hasher = title_select_engine::payload::Hasher::new();
    hasher.update(payload);
    hasher.finish()
}

fn earlier() -> jiff::Timestamp {
    "2026-01-02T03:04:05Z".parse().unwrap()
}

/// A Source Set holding `Pong (USA).a26` with Pong's payload, and an earlier read of it recorded as holding Combat's,
/// so that reusing that read is told apart from reading the file again.
fn set_with_a_stale_read(
    alter: impl FnOnce(&mut FileStamp),
) -> (Fixture, std::path::PathBuf, Reads) {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]);
    let path = f.set_path("Pong (USA).a26");
    write_file(&path, b"pong");
    let mut stamp = FileStamp::of(&path).unwrap();
    alter(&mut stamp);
    let reads = Reads {
        prior: vec![(
            "Pong (USA).a26".into(),
            PayloadRead {
                stamp,
                read_at: earlier(),
                members: vec![(None, sums_of(b"combat"))],
                header_rule: None,
            },
        )],
        ..Reads::default()
    };
    (f, dat, reads)
}

#[test]
fn an_unchanged_file_reuses_its_earlier_read_and_shows_when_that_payload_was_read() {
    let (f, dat, reads) = set_with_a_stale_read(|_| {});

    let report = run(&dat, f.set.path(), AuditEvidence::Incremental, Some(&reads));

    let entry = &report.entries[0];
    assert_eq!(
        entry.finding,
        Finding::Conflicting,
        "the earlier read's checksums are matched again, not the file's bytes"
    );
    let evidence = entry.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.read_at, Some(earlier()));
    assert_eq!(evidence.reused, Some(Reuse::UnchangedFileHeuristic));
    assert!(reads.recorded.lock().unwrap().is_empty());
}

type Alteration = (&'static str, fn(&mut FileStamp));

#[test]
fn a_file_is_read_again_when_any_field_of_the_change_test_differs() {
    let alterations: [Alteration; 5] = [
        ("dev", |s| s.dev += 1),
        ("ino", |s| s.ino += 1),
        ("size", |s| s.size += 1),
        ("mtime", |s| s.mtime_ns += 1),
        ("ctime", |s| s.ctime_ns += 1),
    ];
    for (field, alter) in alterations {
        let (f, dat, reads) = set_with_a_stale_read(alter);

        let report = run(&dat, f.set.path(), AuditEvidence::Incremental, Some(&reads));

        let entry = &report.entries[0];
        assert_eq!(entry.finding, Finding::Matched, "{field} changed");
        let evidence = entry.evidence.as_ref().unwrap();
        assert_eq!(evidence.reused, None, "{field} changed");
        assert!(evidence.read_at.unwrap() > earlier(), "{field} changed");
        let recorded = reads.recorded.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "{field} changed: the new read is recorded"
        );
        assert_eq!(
            recorded[0].1.stamp,
            FileStamp::of(&f.set_path("Pong (USA).a26")).unwrap()
        );
    }
}

#[test]
fn a_quick_audit_reuses_an_unchanged_zips_payload_read_rather_than_claiming_it() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    let path = f.set_path("Pong (USA).zip");
    write_zip(&path, &[("Pong (USA).a26", b"pong")]);
    let reads = Reads {
        prior: vec![(
            "Pong (USA).zip".into(),
            PayloadRead {
                stamp: FileStamp::of(&path).unwrap(),
                read_at: earlier(),
                members: vec![(Some("Pong (USA).a26".into()), sums_of(b"pong"))],
                header_rule: None,
            },
        )],
        ..Reads::default()
    };

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, Some(&reads));

    let evidence = report.entries[0].evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.read_at, Some(earlier()));
}

#[test]
fn a_game_present_only_on_claim_evidence_names_its_claimed_roms() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        (
            "Breakout (USA)",
            vec![
                rom("Breakout (USA).a26", b"break"),
                rom("Breakout (USA).txt", b"manual"),
            ],
        ),
    ]);
    write_zip(
        &f.set_path("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    write_zip(
        &f.set_path("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    write_file(&f.set_path("Combat (USA).a26"), b"combat");
    write_zip(
        &f.set_path("Breakout (USA).zip"),
        &[("Breakout (USA).a26", b"break")],
    );
    write_file(&f.set_path("Breakout (USA).txt"), b"manual");

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None);

    let game = |name: &str| report.games.iter().find(|g| g.name == name).unwrap();
    assert_eq!(game("Pong (USA)").status, GameStatus::Complete);
    assert_eq!(game("Pong (USA)").claimed_roms, ["Pong (USA).a26"]);
    assert!(
        game("Combat (USA)").claimed_roms.is_empty(),
        "a payload read of the same ROM outweighs a claim"
    );
    assert_eq!(game("Breakout (USA)").status, GameStatus::Complete);
    assert_eq!(game("Breakout (USA)").claimed_roms, ["Breakout (USA).a26"]);
    let full = run(&dat, f.set.path(), AuditEvidence::Full, None);
    assert!(full.games.iter().all(|g| g.claimed_roms.is_empty()));
}
