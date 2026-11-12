//! Exporting what a Source Set lacks from its Audit: the Fixdat and the have/miss CSV.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common::*;
use title_select_engine::dat::Dat;
use title_select_engine::fixdat::{Lacking, Scope};
use title_select_engine::{Report, audit};

/// A Source Set with one Complete, one Missing and one Incomplete Game, and a Missing Game whose name needs escaping
/// and whose ROM the DAT lists with SHA-1 only.
fn audited() -> (Fixture, PathBuf, Report) {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
        (
            "Combat (USA)",
            vec![
                rom("Combat (USA) (Disk 1).a26", b"combat1"),
                rom_without_sha256("Combat (USA) (Disk 2).a26", b"combat2"),
            ],
        ),
        (
            "Tom &amp; Jerry, &quot;Live&quot; (USA)",
            vec![rom_without_crc("Tom &amp; Jerry &lt;1&gt;.a26", b"tom")],
        ),
    ]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    write_file(&f.set_path("Combat (USA) (Disk 1).a26"), b"combat1");
    let report = audit(&dat, f.set.path()).unwrap();
    (f, dat, report)
}

fn parse(xml: &str) -> Dat {
    Dat::parse(xml.as_bytes(), Path::new("fixdat.dat")).unwrap()
}

/// Each ROM as `(game, name, size, crc, md5, sha1, sha256)`.
type RomRow = (
    String,
    String,
    u64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn roms(dat: &Dat) -> Vec<RomRow> {
    dat.roms
        .iter()
        .map(|r| {
            (
                dat.games[r.game].clone(),
                r.name.clone(),
                r.size,
                r.crc.clone(),
                r.md5.clone(),
                r.sha1.clone(),
                r.sha256.clone(),
            )
        })
        .collect()
}

#[test]
fn the_fixdat_lists_missing_games_and_only_the_missing_roms_of_incomplete_ones_as_the_dat_gives_them()
 {
    let (_f, dat_path, report) = audited();
    let dat = Dat::read(&dat_path).unwrap();
    let lacking = Lacking {
        dat: &dat,
        report: &report,
        audit: 7,
        scope: None,
    };

    let fixdat = parse(&lacking.fixdat());

    assert_eq!(
        fixdat.games,
        [
            "Breakout (USA)",
            "Combat (USA)",
            "Tom & Jerry, \"Live\" (USA)"
        ]
    );
    let source = roms(&dat);
    let expected: Vec<RomRow> = source
        .into_iter()
        .filter(|r| {
            [
                "Breakout (USA).a26",
                "Combat (USA) (Disk 2).a26",
                "Tom & Jerry <1>.a26",
            ]
            .contains(&r.1.as_str())
        })
        .collect();
    assert_eq!(roms(&fixdat), expected);
    let tom = fixdat.roms.last().unwrap();
    assert_eq!(
        (
            tom.crc.as_deref(),
            tom.md5.as_deref(),
            tom.sha256.as_deref()
        ),
        (None, None, None),
        "a hash the DAT doesn't give is left out"
    );
}

#[test]
fn the_fixdat_header_names_the_source_dat_its_revision_the_audit_and_the_date() {
    let (_f, dat_path, report) = audited();
    let dat = Dat::read(&dat_path).unwrap();
    let lacking = Lacking {
        dat: &dat,
        report: &report,
        audit: 7,
        scope: None,
    };
    let xml = lacking.fixdat();
    let fixdat = parse(&xml);
    let date = report.finished_at.to_string()[..10].to_string();

    assert_eq!(fixdat.name, "fixdat_Test - System");
    assert_eq!(fixdat.date, date);
    assert_eq!(fixdat.version, "20260101-000000");
    for fact in [
        "Test - System",
        dat.sha256.as_str(),
        "audit 7",
        report.finished_at.to_string().as_str(),
    ] {
        assert!(xml.contains(fact), "the header names {fact}: {xml}");
    }
    assert_eq!(
        lacking.fixdat_file_name(),
        format!("fixdat_Test - System ({date}).dat")
    );
    assert_eq!(xml, lacking.fixdat(), "the same Audit gives the same bytes");
}

#[test]
fn have_miss_has_a_row_per_game_with_its_audit_status_quoted_as_csv_needs() {
    let (_f, dat_path, report) = audited();
    let dat = Dat::read(&dat_path).unwrap();
    let lacking = Lacking {
        dat: &dat,
        report: &report,
        audit: 7,
        scope: None,
    };

    assert_eq!(
        lacking.have_miss(),
        "game,status,present_roms,missing_roms,available_elsewhere_roms\r\n\
         Pong (USA),complete,Pong (USA).a26,,\r\n\
         Breakout (USA),missing,,Breakout (USA).a26,\r\n\
         Combat (USA),incomplete,Combat (USA) (Disk 1).a26,Combat (USA) (Disk 2).a26,\r\n\
         \"Tom & Jerry, \"\"Live\"\" (USA)\",missing,,Tom & Jerry <1>.a26,\r\n"
    );
    let date = &report.finished_at.to_string()[..10];
    assert_eq!(
        lacking.have_miss_file_name(),
        format!("have-miss_Test - System ({date}).csv")
    );
}

#[test]
fn a_scope_limits_both_exports_to_its_games() {
    let (_f, dat_path, report) = audited();
    let dat = Dat::read(&dat_path).unwrap();
    let scope: HashSet<String> = ["Combat (USA)".to_string(), "Pong (USA)".to_string()].into();
    let lacking = Lacking {
        dat: &dat,
        report: &report,
        audit: 7,
        scope: Some(Scope {
            selection: 3,
            games: &scope,
        }),
    };

    let xml = lacking.fixdat();
    assert_eq!(parse(&xml).games, ["Combat (USA)"]);
    assert!(xml.contains("winners of selection 3"), "{xml}");
    let games: Vec<String> = lacking
        .have_miss()
        .lines()
        .skip(1)
        .map(|l| l.split(',').next().unwrap().to_string())
        .collect();
    assert_eq!(games, ["Pong (USA)", "Combat (USA)"]);
}

#[test]
fn each_fixdat_game_keeps_the_description_the_dat_gives_it() {
    let f = Fixture::new();
    let dat_path = f.dat_dir.path().join("described.dat");
    std::fs::write(
        &dat_path,
        r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test - System</name><version>1</version></header>
  <game name="pong"><description>Pong &amp; Friends (USA)</description><rom name="pong.a26" size="4" crc="00000001"/></game>
  <game name="combat"><rom name="combat.a26" size="6" crc="00000002"/></game>
</datafile>
"#,
    )
    .unwrap();
    let report = audit(&dat_path, f.set.path()).unwrap();
    let dat = Dat::read(&dat_path).unwrap();
    let lacking = Lacking {
        dat: &dat,
        report: &report,
        audit: 1,
        scope: None,
    };

    let fixdat = parse(&lacking.fixdat());

    assert_eq!(fixdat.games, ["pong", "combat"]);
    assert_eq!(
        fixdat.descriptions,
        ["Pong & Friends (USA)", "combat"],
        "a game the DAT doesn't describe is described by its name"
    );
}

/// A Fixdat of a DAT that names a Header Rule names it too, and lists its ROMs as the DAT does, without the Header: a
/// headered dump the Audit matched is not in it.
#[test]
fn a_fixdat_names_the_dats_header_rule_and_keeps_its_headerless_roms() {
    let f = Fixture::new();
    let zelda = noise(1, 64);
    let mario = noise(2, 64);
    let dat_path = write_dat_with_header(
        f.dat_dir.path(),
        "No-Intro_NES.xml",
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
            ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
        ],
    );
    let mut ines = b"NES\x1a".to_vec();
    ines.resize(16, 0);
    write_file(&f.set_path("Zelda (USA).nes"), &[ines, zelda].concat());
    let report = audit(&dat_path, f.set.path()).unwrap();
    let dat = Dat::read(&dat_path).unwrap();

    let fixdat = parse(
        &Lacking {
            dat: &dat,
            report: &report,
            audit: 1,
            scope: None,
        }
        .fixdat(),
    );

    assert_eq!(fixdat.header_rule.as_deref(), Some("No-Intro_NES.xml"));
    let mario = dat
        .roms
        .iter()
        .find(|r| r.name == "Mario (USA).nes")
        .unwrap();
    assert_eq!(
        roms(&fixdat),
        vec![(
            "Mario (USA)".to_owned(),
            mario.name.clone(),
            64,
            mario.crc.clone(),
            mario.md5.clone(),
            mario.sha1.clone(),
            mario.sha256.clone(),
        )]
    );
}
