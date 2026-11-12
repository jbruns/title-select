//! Headers in the store: a stored Audit keeps what a DAT's Header Rule found (ADR 0016).
#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::*;
use title_select_engine::{AuditEvidence, Finding, GameWarning, Reuse};
use title_select_store::Store;
use title_select_store::config::Config;

const SET: &str = "No-Intro/Test - System";

struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("roots/sets");
        std::fs::create_dir_all(root.join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            root.display().to_string()
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        Env { tmp, store, config }
    }

    fn set_dir(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn add(&self, rule: &str, games: &[(&str, Vec<RomSpec>)]) {
        let dat = write_dat_with_header(&self.tmp.path().join("in"), rule, games);
        self.store.import_dat(&dat, "No-Intro", None).unwrap();
        self.store
            .add_source_set(&self.config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
    }

    fn audit(&self, evidence: AuditEvidence) -> title_select_store::StoredAudit {
        self.store
            .audit_source_set(&self.config, "Test - System", None, evidence)
            .unwrap()
    }
}

fn ines() -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.resize(16, 0x01);
    h
}

#[test]
fn a_stored_audit_keeps_the_header_rule_each_header_and_the_warnings() {
    let env = Env::new();
    let zelda = noise(1, 64);
    let mario = noise(2, 64);
    env.add(
        "No-Intro_NES.xml",
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
            ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
        ],
    );
    write_file(
        &env.set_dir().join("Zelda (USA).nes"),
        &[ines(), zelda].concat(),
    );
    write_file(&env.set_dir().join("Mario (USA).nes"), &mario);

    let stored = env.audit(AuditEvidence::Full);
    let report = env.store.report(stored.id).unwrap();

    assert_eq!(report, stored.report, "the stored report is what was found");
    let rule = report.dat_revision.header_rule.as_ref().unwrap();
    assert_eq!(rule.name, "No-Intro_NES.xml");
    assert!(rule.rule.is_some());
    let zelda = report
        .entries
        .iter()
        .find(|e| e.path == "Zelda (USA).nes")
        .unwrap();
    assert_eq!(zelda.finding, Finding::Matched);
    assert_eq!(zelda.evidence.as_ref().unwrap().header.unwrap().bytes, 16);
    let mario = report
        .games
        .iter()
        .find(|g| g.name == "Mario (USA)")
        .unwrap();
    assert_eq!(mario.warnings, vec![GameWarning::HeaderMissing]);
    assert_eq!(report.summary.warnings[&GameWarning::HeaderMissing], 1);

    let summary = env.store.audit(stored.id).unwrap();
    assert_eq!(summary.warnings[&GameWarning::HeaderMissing], 1);
    let (games, _) = env.store.games(stored.id, None, 10, None).unwrap();
    assert!(
        games
            .iter()
            .any(|g| g.game.warnings == vec![GameWarning::HeaderMissing])
    );
}

#[test]
fn a_reused_read_keeps_its_header() {
    let env = Env::new();
    let zelda = noise(1, 64);
    env.add(
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)])],
    );
    write_file(
        &env.set_dir().join("Zelda (USA).nes"),
        &[ines(), zelda].concat(),
    );
    env.audit(AuditEvidence::Incremental);

    let second = env.audit(AuditEvidence::Incremental).report;

    let evidence = second.entries[0].evidence.as_ref().unwrap();
    assert_eq!(second.entries[0].finding, Finding::Matched);
    assert_eq!(evidence.reused, Some(Reuse::UnchangedFileHeuristic));
    assert_eq!(evidence.header.unwrap().bytes, 16);
}

#[test]
fn an_unknown_header_rule_is_kept_by_name() {
    let env = Env::new();
    env.add(
        "No-Intro_SNES.xml",
        &[("Game", vec![rom("Game.sfc", b"game")])],
    );
    write_file(&env.set_dir().join("Game.sfc"), b"game");

    let stored = env.audit(AuditEvidence::Full);

    let rule = env
        .store
        .report(stored.id)
        .unwrap()
        .dat_revision
        .header_rule
        .unwrap();
    assert_eq!(rule.name, "No-Intro_SNES.xml");
    assert_eq!(rule.rule, None);
}
