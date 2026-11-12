//! Headers: a DAT that names a Header Rule matches a dump with or without its Header (ADR 0016).

mod common;

use common::*;
use std::sync::Mutex;

use title_select_engine::payload::Hasher;
use title_select_engine::{
    AuditEvidence, EvidenceLevel, FileStamp, Finding, GameStatus, GameWarning, HeaderRule,
    ONE_WORKER_EACH, Options, PayloadRead, PayloadReads, Report, audit, audit_with,
};

const NES: &str = "No-Intro_NES.xml";

/// A 16-byte iNES Header.
fn ines() -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.resize(16, 0x01);
    h
}

fn with_header(header: &[u8], rom: &[u8]) -> Vec<u8> {
    [header, rom].concat()
}

fn entry<'r>(report: &'r Report, path: &str) -> &'r title_select_engine::Entry {
    report.entries.iter().find(|e| e.path == path).unwrap()
}

fn header_bytes(report: &Report, path: &str) -> Option<u64> {
    entry(report, path)
        .evidence
        .as_ref()
        .unwrap()
        .header
        .as_ref()
        .map(|h| h.bytes)
}

#[test]
fn a_headered_loose_dump_matches_a_headerless_dat_naming_its_rule() {
    let f = Fixture::new();
    let prg = noise(1, 64);
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        NES,
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)])],
    );
    write_file(&f.set_path("Zelda (USA).nes"), &with_header(&ines(), &prg));

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(entry(&report, "Zelda (USA).nes").finding, Finding::Matched);
    assert_eq!(header_bytes(&report, "Zelda (USA).nes"), Some(16));
}

/// Each built-in rule's DAT name, and a Header it recognises.
fn rules() -> Vec<(&'static str, Vec<u8>)> {
    let pad = |mut h: Vec<u8>, len: usize| {
        h.resize(len, 0x02);
        h
    };
    vec![
        (NES, ines()),
        ("No-Intro_FDS.xml", pad(b"FDS\x1a".to_vec(), 16)),
        ("No-Intro_A7800.xml", pad(b"\x01ATARI7800".to_vec(), 128)),
        ("No-Intro_LNX.xml", pad(b"LYNX".to_vec(), 64)),
    ]
}

#[test]
fn every_rule_matches_headered_and_headerless_dumps_as_loose_files_and_zip_members() {
    for (rule, header) in rules() {
        let f = Fixture::new();
        let rom_data = noise(2, 256);
        let dat = write_dat_with_header(
            f.dat_dir.path(),
            rule,
            &[("Game", vec![rom("Game.bin", &rom_data)])],
        );
        let headered = with_header(&header, &rom_data);
        write_file(&f.set_path("a/Game.bin"), &headered);
        write_file(&f.set_path("b/Game.bin"), &rom_data);
        write_zip(&f.set_path("c.zip"), &[("Game.bin", &headered)]);
        write_zip(&f.set_path("d.zip"), &[("Game.bin", &rom_data)]);

        let report = audit(&dat, f.set.path()).unwrap();

        for path in ["a/Game.bin", "b/Game.bin", "c.zip", "d.zip"] {
            assert_eq!(
                entry(&report, path).finding,
                Finding::Matched,
                "{rule}: {path}"
            );
        }
        let size = header.len() as u64;
        assert_eq!(header_bytes(&report, "a/Game.bin"), Some(size), "{rule}");
        assert_eq!(header_bytes(&report, "c.zip"), Some(size), "{rule}");
        assert_eq!(header_bytes(&report, "b/Game.bin"), None, "{rule}");
        assert_eq!(header_bytes(&report, "d.zip"), None, "{rule}");
    }
}

#[test]
fn a_dat_that_names_no_rule_never_skips_a_header_even_for_nes() {
    let f = Fixture::new();
    let prg = noise(3, 64);
    let dat = f.dat(&[("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)])]);
    write_file(&f.set_path("Zelda (USA).nes"), &with_header(&ines(), &prg));

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(
        entry(&report, "Zelda (USA).nes").finding,
        Finding::Mismatched
    );
    assert_eq!(header_bytes(&report, "Zelda (USA).nes"), None);
    assert_eq!(report.dat_revision.header_rule, None);
}

#[test]
fn a_rule_title_select_does_not_know_is_recorded_and_skips_nothing() {
    let f = Fixture::new();
    let prg = noise(4, 64);
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        "No-Intro_Unknown.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)])],
    );
    write_file(&f.set_path("Zelda (USA).nes"), &with_header(&ines(), &prg));

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(
        entry(&report, "Zelda (USA).nes").finding,
        Finding::Mismatched
    );
    let named = report.dat_revision.header_rule.as_ref().unwrap();
    assert_eq!(named.name, "No-Intro_Unknown.xml");
    assert_eq!(named.rule, None);
}

#[test]
fn a_headerless_match_warns_for_nes_and_7800_but_not_fds_or_lynx() {
    for (rule, header) in rules() {
        let f = Fixture::new();
        let rom_data = noise(5, 256);
        let other = noise(6, 256);
        let dat = write_dat_with_header(
            f.dat_dir.path(),
            rule,
            &[
                ("Bare", vec![rom("Bare.bin", &rom_data)]),
                ("Headered", vec![rom("Headered.bin", &other)]),
            ],
        );
        write_file(&f.set_path("Bare.bin"), &rom_data);
        write_file(&f.set_path("Headered.bin"), &with_header(&header, &other));

        let report = audit(&dat, f.set.path()).unwrap();

        let warnings = |name: &str| {
            report
                .games
                .iter()
                .find(|g| g.name == name)
                .unwrap()
                .warnings
                .clone()
        };
        let warns = rule == NES || rule == "No-Intro_A7800.xml";
        let expected = if warns {
            vec![GameWarning::HeaderMissing]
        } else {
            vec![]
        };
        assert_eq!(warnings("Bare"), expected, "{rule}");
        assert_eq!(warnings("Headered"), vec![], "{rule}");
        assert_eq!(
            report.summary.warnings.get(&GameWarning::HeaderMissing),
            warns.then_some(&1),
            "{rule}"
        );
        let game = report.games.iter().find(|g| g.name == "Bare").unwrap();
        assert_eq!(
            game.status,
            GameStatus::Complete,
            "a warning is not a status"
        );
    }
}

#[test]
fn the_json_report_carries_the_header_size_and_the_warning() {
    let f = Fixture::new();
    let prg = noise(7, 64);
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        NES,
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)]),
            ("Metroid (USA)", vec![rom("Metroid (USA).nes", &prg[..32])]),
        ],
    );
    write_file(&f.set_path("Zelda (USA).nes"), &with_header(&ines(), &prg));
    write_file(&f.set_path("Metroid (USA).nes"), &prg[..32]);

    let json = serde_json::to_value(audit(&dat, f.set.path()).unwrap()).unwrap();

    assert_eq!(json["schema"], 4);
    assert_eq!(
        json["dat_revision"]["header_rule"],
        serde_json::json!({"name": NES, "rule": "nes"})
    );
    let zelda = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "Zelda (USA).nes")
        .unwrap();
    assert_eq!(
        zelda["evidence"]["header"],
        serde_json::json!({"bytes": 16})
    );
    let metroid = json["games"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "Metroid (USA)")
        .unwrap();
    assert_eq!(metroid["warnings"], serde_json::json!(["header_missing"]));
    assert_eq!(
        json["summary"]["warnings"],
        serde_json::json!({"header_missing": 1})
    );
}

fn run(
    dat: &std::path::Path,
    set: &std::path::Path,
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
fn a_quick_inventory_reads_a_member_that_might_be_headered_and_claims_the_rest() {
    let f = Fixture::new();
    let prg = noise(8, 64);
    let other = noise(9, 96);
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        NES,
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)]),
            ("Metroid (USA)", vec![rom("Metroid (USA).nes", &other)]),
        ],
    );
    write_zip(
        &f.set_path("Zelda (USA).zip"),
        &[("Zelda (USA).nes", &with_header(&ines(), &prg))],
    );
    write_zip(
        &f.set_path("Metroid (USA).zip"),
        &[("Metroid (USA).nes", &other)],
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None);

    let zelda = entry(&report, "Zelda (USA).zip");
    assert_eq!(zelda.finding, Finding::Matched);
    assert_eq!(
        zelda.evidence.as_ref().unwrap().level,
        EvidenceLevel::Payload
    );
    assert_eq!(header_bytes(&report, "Zelda (USA).zip"), Some(16));
    let metroid = entry(&report, "Metroid (USA).zip");
    assert_eq!(metroid.finding, Finding::Matched);
    assert_eq!(
        metroid.evidence.as_ref().unwrap().level,
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

#[test]
fn a_read_made_under_another_header_rule_is_read_again() {
    let f = Fixture::new();
    let prg = noise(10, 64);
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        NES,
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &prg)])],
    );
    let path = f.set_path("Zelda (USA).nes");
    let dump = with_header(&ines(), &prg);
    write_file(&path, &dump);
    let mut whole = Hasher::new();
    whole.update(&dump);
    let reads = Reads {
        prior: vec![(
            "Zelda (USA).nes".into(),
            PayloadRead {
                stamp: FileStamp::of(&path).unwrap(),
                read_at: "2026-01-02T03:04:05Z".parse().unwrap(),
                members: vec![(None, whole.finish())],
                header_rule: None,
            },
        )],
        ..Reads::default()
    };

    let report = run(&dat, f.set.path(), AuditEvidence::Incremental, Some(&reads));

    assert_eq!(entry(&report, "Zelda (USA).nes").finding, Finding::Matched);
    assert_eq!(
        entry(&report, "Zelda (USA).nes")
            .evidence
            .as_ref()
            .unwrap()
            .reused,
        None
    );
    let recorded = reads.recorded.lock().unwrap();
    assert_eq!(recorded[0].1.header_rule, Some(HeaderRule::Nes));
    assert_eq!(recorded[0].1.members[0].1.header, Some(16));
}
