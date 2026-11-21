use std::process::Command;

use crate::common::*;

fn title_select() -> Command {
    Command::new(env!("CARGO_BIN_EXE_title-select"))
}

fn pong_fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    (f, dat)
}

#[test]
fn audit_prints_a_json_report_to_stdout() {
    let (f, dat) = pong_fixture();

    let out = title_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 6);
    assert_eq!(report["dat_revision"]["name"], "Test - System");
    assert_eq!(report["summary"]["findings"]["matched"], 1);
    assert_eq!(report["entries"][0]["finding"], "matched");
    assert_eq!(report["entries"][0]["evidence"]["level"], "payload");
    assert_eq!(report["games"][0]["status"], "complete");
}

#[test]
fn a_stateless_audit_says_it_ran_in_full() {
    let (f, dat) = pong_fixture();

    let out = title_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .output()
        .unwrap();

    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["evidence"], "full");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no earlier payload reads to reuse: it ran as a full audit"),
        "{stderr}"
    );
}

#[test]
fn audit_writes_the_report_to_the_output_file() {
    let (f, dat) = pong_fixture();
    let output = f.dat_dir.path().join("report.json");

    let out = title_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(report["summary"]["findings"]["matched"], 1);
}

#[test]
fn audit_fails_with_a_message_for_an_unreadable_dat_or_a_missing_source_set() {
    let (f, dat) = pong_fixture();
    let not_xml = f.dat_dir.path().join("not.dat");
    std::fs::write(&not_xml, "<datafile><game name=\"x\"></datafile>").unwrap();
    let missing_set = f.dat_dir.path().join("no-such-set");

    for (dat, set) in [
        (not_xml.as_path(), f.set.path()),
        (dat.as_path(), missing_set.as_path()),
    ] {
        let out = title_select()
            .arg("audit")
            .arg("--dat")
            .arg(dat)
            .arg(set)
            .output()
            .unwrap();

        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_headerless_nes_match_is_warned_of_in_the_report_and_counted_in_the_history() {
    let f = Fixture::new();
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", b"zelda")])],
    );
    write_file(&f.set_path("Zelda (USA).nes"), b"zelda");
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));

    let report: serde_json::Value =
        serde_json::from_str(&run_ok(ts().args(["audit", "--set", rel]))).unwrap();

    assert_eq!(report["games"][0]["warnings"][0], "header_missing");
    assert_eq!(report["summary"]["warnings"]["header_missing"], 1);
    let history = run_ok(ts().args(["set", "history", rel]));
    assert!(history.contains(" header_missing=1\t"), "{history}");
}

#[test]
fn a_registered_source_set_is_audited_into_the_store_and_listed_in_its_history() {
    let (f, dat) = pong_fixture();
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };

    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));
    assert!(imported.contains("No-Intro/Test - System"), "{imported}");
    let pack = f.dat_dir.path().join("cues.zip");
    write_zip(
        &pack,
        &[("Pong (USA).cue", b"FILE \"Pong (USA).bin\" BINARY\n")],
    );
    let cues = run_ok(
        ts().args(["cue-pack", "import"])
            .arg(&pack)
            .args(["--dat", "No-Intro/Test - System"]),
    );
    assert!(cues.contains("(1 cue sheet) imported"), "{cues}");
    let listed = run_ok(ts().args(["cue-pack", "list"]));
    assert!(
        listed.starts_with("No-Intro/Test - System\tcues.zip\t1 cue sheet"),
        "{listed}"
    );
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    let report: serde_json::Value =
        serde_json::from_str(&run_ok(ts().args(["audit", "--set", rel]))).unwrap();
    assert_eq!(report["schema"], 6);
    assert_eq!(report["summary"]["findings"]["matched"], 1);

    let sets = run_ok(ts().args(["set", "list"]));
    let sha = report["dat_revision"]["sha256"].as_str().unwrap();
    assert!(sets.starts_with(rel), "{sets}");
    assert!(
        sets.contains(&format!("last verified against {sha}")),
        "{sets}"
    );
    let history = run_ok(ts().args(["set", "history", rel]));
    assert_eq!(history.lines().count(), 1);
    assert!(
        history.contains("matched=1") && history.contains("complete=1"),
        "{history}"
    );

    let again = ts().args(["audit", "--set", rel]).output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert_eq!(report["evidence"], "incremental");
    assert!(
        stderr.contains("0 entries read, 1 reused") && stderr.contains("heuristic"),
        "{stderr}"
    );
    let full: serde_json::Value = serde_json::from_str(&run_ok(ts().args([
        "audit",
        "--set",
        rel,
        "--evidence",
        "full",
    ])))
    .unwrap();
    assert_eq!(full["evidence"], "full");
    assert!(full["entries"][0]["evidence"].get("reused").is_none());
    let history = run_ok(ts().args(["set", "history", rel]));
    assert!(
        history.lines().next().unwrap().contains("evidence=full"),
        "{history}"
    );
}

#[test]
fn fixdat_and_have_miss_export_what_the_latest_audit_found_missing() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
    ]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    let unaudited = ts().args(["fixdat", rel]).output().unwrap();
    assert!(!unaudited.status.success());
    assert!(
        String::from_utf8_lossy(&unaudited.stderr).contains("audit it"),
        "{}",
        String::from_utf8_lossy(&unaudited.stderr)
    );
    run_ok(ts().args(["audit", "--set", rel]));

    let xml = run_ok(ts().args(["fixdat", rel]));
    let fixdat =
        title_select_engine::dat::Dat::parse(xml.as_bytes(), std::path::Path::new("x")).unwrap();
    assert_eq!(fixdat.games, ["Breakout (USA)"]);

    let out = f.dat_dir.path().join("exports");
    std::fs::create_dir(&out).unwrap();
    let said = ts()
        .args(["have-miss", rel, "--output"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(said.status.success());
    let written: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [csv] = written.as_slice() else {
        panic!("one file is written into the folder: {written:?}");
    };
    let name = csv.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("have-miss_Test - System (") && name.ends_with(").csv"),
        "{name}"
    );
    assert!(
        String::from_utf8_lossy(&said.stderr).contains(name),
        "it says where it wrote"
    );
    assert_eq!(
        std::fs::read_to_string(csv).unwrap().lines().nth(2),
        Some("Breakout (USA),missing,,Breakout (USA).a26,")
    );

    let unknown = ts()
        .args(["fixdat", rel, "--selection", "999"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());

    run_ok(ts().args(["set", "coverage", rel, "--region", "Japan"]));
    let covered = run_ok(ts().args(["fixdat", rel]));
    assert!(!covered.contains("Breakout"), "{covered}");
    assert!(
        covered.contains("limited to the games in its coverage: Japan"),
        "{covered}"
    );
    let whole = run_ok(ts().args(["fixdat", rel, "--scope", "dat"]));
    assert!(whole.contains("Breakout (USA)"), "{whole}");
}

#[test]
fn a_source_set_is_unregistered_listed_as_unregistered_and_forgotten() {
    let (f, dat) = pong_fixture();
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    run_ok(ts().args(["audit", "--set", rel]));
    let holds = run_ok(ts().args(["root", "holds", "sets"]));
    assert!(holds.contains(rel), "{holds}");
    let holds = run_ok(ts().args(["set", "holds", rel]));
    assert!(holds.contains("nothing holds it"), "{holds}");

    let done = run_ok(ts().args(["set", "unregister", rel]));
    assert!(
        done.contains("unregistered; no game file was touched"),
        "{done}"
    );
    assert!(f.set.path().join("Pong (USA).a26").exists());
    assert!(run_ok(ts().args(["set", "list"])).is_empty());
    let listed = run_ok(ts().args(["unregistered", "list"]));
    assert!(
        listed.starts_with("source_set\t1\t") && listed.contains("audits=1"),
        "{listed}"
    );

    let history = run_ok(ts().args(["unregistered", "history", "source-set", "1"]));
    assert!(
        history.starts_with(&format!("source set {rel} "))
            && history.lines().any(|l| l.starts_with("audit\t")),
        "{history}"
    );

    let forgotten = run_ok(ts().args(["unregistered", "forget", "source-set", "1"]));
    assert!(forgotten.contains("forgotten: 1 audits"), "{forgotten}");
    assert!(run_ok(ts().args(["unregistered", "list"])).is_empty());
    let unknown = ts()
        .args(["unregistered", "forget", "shelf", "1"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
}

#[test]
fn set_add_with_no_path_registers_the_path_the_template_proposes() {
    let (f, dat) = pong_fixture();
    let root = f.dat_dir.path().join("sets");
    std::fs::create_dir_all(root.join("No-Intro/Test - System")).unwrap();
    std::fs::create_dir_all(root.join("Mine/Test - System")).unwrap();
    let config = f.dat_dir.path().join("title-select.toml");
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    run_ok(ts().args(["dat", "import"]).arg(&dat));

    let out = ts()
        .args([
            "set",
            "add",
            "sets",
            "--dat",
            "No-Intro/Test - System",
            "--template",
            "{platform}",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("type the path"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let added = run_ok(ts().args(["set", "add", "sets", "--dat", "No-Intro/Test - System"]));

    assert!(added.contains("sets:No-Intro/Test - System"), "{added}");
    let both = ts()
        .args([
            "set",
            "add",
            "sets",
            "Mine/Test - System",
            "--dat",
            "No-Intro/Test - System",
        ])
        .args(["--template", "Mine/{dat}"])
        .output()
        .unwrap();
    assert!(!both.status.success(), "a path and a template conflict");
}

#[test]
fn set_coverage_sets_shows_and_clears_a_source_sets_coverage() {
    let (f, dat) = pong_fixture();
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    run_ok(ts().args(["dat", "import"]).arg(&dat));

    let added = run_ok(
        ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"])
            .args([
                "--coverage-preset",
                "USA + Canada",
                "--coverage-region",
                "Unknown",
            ]),
    );
    assert!(added.contains("covers USA, Canada, Unknown"), "{added}");
    let shown = run_ok(ts().args(["set", "coverage", rel]));
    assert!(shown.contains("covers USA, Canada, Unknown"), "{shown}");

    let unknown = ts()
        .args(["set", "coverage", rel, "--region", "Atlantis"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown regions: Atlantis"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    let changed = run_ok(ts().args(["set", "coverage", rel, "--region", "Japan"]));
    assert!(changed.contains("covers Japan"), "{changed}");

    let cleared = run_ok(ts().args(["set", "coverage", rel, "--clear"]));
    assert!(cleared.contains("covers every game"), "{cleared}");
    let shown = run_ok(ts().args(["set", "coverage", rel]));
    assert!(shown.contains("covers every game"), "{shown}");
}

#[test]
fn version_prints_the_pinned_clone_list_commit() {
    let pins = include_str!("../../../../third_party/clone-lists/pins.toml");
    let commits: Vec<&str> = pins
        .lines()
        .filter_map(|l| l.strip_prefix("commit = \""))
        .map(|l| l.trim_end_matches('"'))
        .collect();
    assert_eq!(commits.len(), 1, "the clone-list commit");

    let out = title_select().arg("--version").output().unwrap();

    assert!(out.status.success());
    let version = String::from_utf8_lossy(&out.stdout);
    assert!(
        version.starts_with(&format!("title-select {}\n", env!("CARGO_PKG_VERSION"))),
        "{version}"
    );
    for commit in commits {
        assert!(version.contains(commit), "{commit} in {version}");
    }
}

#[test]
fn a_stored_audit_without_a_store_fails_with_a_message() {
    let out = title_select()
        .env_remove("TITLE_SELECT_STORE")
        .args(["audit", "--set", "Anything"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--store"));
}

#[test]
fn select_prints_the_selection_and_refuses_a_profile_it_cannot_read() {
    let (f, dat) = pong_fixture();
    let profile = f.dat_dir.path().join("profile.json");
    std::fs::write(&profile, r#"{"region_order": ["USA"]}"#).unwrap();

    let stdout = run_ok(
        title_select()
            .arg("select")
            .arg("--dat")
            .arg(&dat)
            .arg("--profile")
            .arg(&profile),
    );
    let selection: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(selection["system"], "Test - System");
    assert_eq!(selection["groups"][0]["winners"][0]["title"], "Pong (USA)");

    std::fs::write(&profile, r#"{"region_ordr": ["USA"]}"#).unwrap();
    let out = title_select()
        .arg("select")
        .arg("--dat")
        .arg(&dat)
        .arg("--profile")
        .arg(&profile)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("region_ordr"));
}

#[test]
fn the_engine_choice_and_the_conformance_command_are_gone() {
    let (f, dat) = pong_fixture();
    let profile = f.dat_dir.path().join("profile.json");
    std::fs::write(&profile, "{}").unwrap();
    for args in [
        &["select", "--engine", "builtin", "--profile"][..],
        &["conformance", "--profile"][..],
    ] {
        let out = title_select()
            .args(args)
            .arg(&profile)
            .arg("--dat")
            .arg(&dat)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
    }
}

#[test]
fn an_ingest_folder_is_registered_scanned_and_its_items_listed() {
    let f = Fixture::new();
    let inbox = f.dat_dir.path().join("in");
    std::fs::create_dir_all(&inbox).unwrap();
    write_file(&inbox.join("mystery.bin"), b"nobody knows");
    let config = f.dat_dir.path().join("title-select.toml");
    std::fs::write(
        &config,
        format!(
            "[roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            inbox.display().to_string()
        ),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    title_select_store::Store::open(&store)
        .unwrap()
        .update_settings(
            &title_select_store::config::Config::load(&config).unwrap(),
            &title_select_api::settings::SettingsUpdate {
                ingest_folders: Some(
                    [(
                        "in".to_owned(),
                        title_select_api::settings::IngestCadence {
                            settle_secs: 0,
                            scan_every_minutes: None,
                        },
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            "test",
        )
        .unwrap();
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };

    let unregistered = ts().args(["ingest", "scan", "in"]).output().unwrap();
    assert!(!unregistered.status.success());
    run_ok(ts().args(["ingest", "register", "in"]));
    run_ok(ts().args(["ingest", "scan", "in"]));
    let arriving = run_ok(ts().args(["ingest", "items", "in"]));
    assert!(arriving.contains("\tarriving\tmystery.bin"), "{arriving}");
    run_ok(ts().args(["ingest", "scan", "in"]));
    let assessed = run_ok(ts().args(["ingest", "items", "in"]));
    assert!(
        assessed.contains("\tassessed\tmystery.bin\taddition"),
        "{assessed}"
    );
}

#[test]
fn dat_import_takes_no_provider_and_says_when_the_header_names_none() {
    let f = Fixture::new();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store);
        cmd
    };
    let help = run_ok(ts().args(["dat", "import", "--help"]));
    assert!(!help.contains("--provider"), "{help}");
    let dat = write_dat_from(
        f.dat_dir.path(),
        "homebrew.dat",
        "1",
        "Mine",
        &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
    );

    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));

    assert!(
        imported.contains("unknown-logiqx/Test - System")
            && imported.contains("doesn't say which provider"),
        "{imported}"
    );
    let refused = ts()
        .args(["dat", "import", "--provider", "Mine"])
        .arg(&dat)
        .output()
        .unwrap();
    assert!(!refused.status.success());
}

#[test]
fn dat_delete_refuses_while_a_source_set_uses_the_dat_and_deletes_once_none_does() {
    let (f, dat) = pong_fixture();
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };
    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));
    let sha = imported
        .split_whitespace()
        .find(|w| w.len() == 64)
        .unwrap()
        .to_string();
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));

    let refused = ts().args(["dat", "delete", &sha]).output().unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("1 source set uses its DAT") && stderr.contains("Nothing was changed"),
        "{stderr}"
    );

    run_ok(ts().args(["set", "unregister", rel]));
    let deleted = run_ok(ts().args(["dat", "delete", &sha]));
    assert!(deleted.contains("deleted"), "{deleted}");
    assert!(deleted.contains("no longer listed"), "{deleted}");
}

#[test]
fn support_file_import_keeps_a_support_file_and_list_prints_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store);
        cmd
    };
    let ini = dir.path().join("languages.ini");
    std::fs::write(
        &ini,
        ";; languages.ini 0.289 / 21-Aug-26 / MAME 0.289 ;;\r\n[English]\r\npong\r\n",
    )
    .unwrap();

    let imported = run_ok(ts().args(["support-file", "import"]).arg(&ini));
    assert!(
        imported.starts_with("languages.ini for MAME 0.289: support file ")
            && imported.ends_with(" imported\n"),
        "{imported}"
    );
    let again = run_ok(ts().args(["support-file", "import"]).arg(&ini));
    assert!(again.ends_with(" already imported\n"), "{again}");
    let listed = run_ok(ts().args(["support-file", "list"]));
    assert!(
        listed.starts_with("languages.ini\tMAME 0.289\tlanguages.ini\timported\t"),
        "{listed}"
    );

    let other = dir.path().join("notes.ini");
    std::fs::write(&other, "[English]\r\npong\r\n").unwrap();
    let out = ts()
        .args(["support-file", "import"])
        .arg(&other)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a MAME support file"), "{err}");
}
