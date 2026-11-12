#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::*;
use title_select_server::{Handle, Settings};
use title_select_store::Store;
use title_select_store::config::Config;

const SET: &str = "Test - System";
const TOKEN: &str = "headless test token";

/// A Service with one registered Source Set, run in this process, and the files the headless client is given.
struct Env {
    fx: Fixture,
    service: Handle,
}

impl Env {
    fn new() -> Env {
        let fx = Fixture::new();
        let set = fx.set_path(SET);
        write_file(&set.join("Pong (USA).a26"), b"pong");
        write_file(&set.join("stray.bin"), b"stray");
        let dat = fx.dat(&[
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ]);
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            fx.set.path().display().to_string()
        ))
        .unwrap();
        let store = fx.dat_dir.path().join("store");
        {
            let store = Store::open(&store).unwrap();
            store.import_dat(&dat, "No-Intro", None).unwrap();
            store
                .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
                .unwrap();
        }
        std::fs::write(fx.dat_dir.path().join("token"), TOKEN).unwrap();
        std::fs::write(fx.dat_dir.path().join("wrong"), "not the token").unwrap();
        let service = title_select_server::spawn(Settings {
            store,
            config,
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: Some(fx.dat_dir.path().join("token")),
            webhook_secret_file: None,
            ui: None,
            clone_lists: None,
            hold_jobs: false,
            fetch_endpoints: None,
        })
        .unwrap();
        Env { fx, service }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.fx.dat_dir.path().join(name)
    }

    fn audit(&self, server: &str, token: &Path, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_title-select"))
            .args(["audit", "--server", server, "--set", SET])
            .arg("--token-file")
            .arg(token)
            .args(extra)
            .env_remove("TITLE_SELECT_STORE")
            .output()
            .unwrap()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn the_headless_client_runs_an_audit_job_and_prints_the_report() {
    let env = Env::new();

    let out = env.audit(&env.service.url(), &env.file("token"), &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 5);
    assert!(report["source_set"].as_str().unwrap().ends_with(SET));
    assert_eq!(report["summary"]["findings"]["matched"], 1);
    assert_eq!(report["summary"]["findings"]["unmatched"], 1);
    let err = stderr(&out);
    assert!(err.contains("succeeded"), "{err}");
    assert!(err.contains("matched=1"), "{err}");

    let output = env.file("report.json");
    let out = env.audit(
        &env.service.url(),
        &env.file("token"),
        &["--output", output.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(written["summary"], report["summary"]);
}

#[test]
fn the_headless_client_asks_for_the_evidence_you_choose() {
    let env = Env::new();
    let token = env.file("token");
    assert!(env.audit(&env.service.url(), &token, &[]).status.success());

    let full = env.audit(&env.service.url(), &token, &["--evidence", "full"]);
    let quick = env.audit(&env.service.url(), &token, &["--evidence", "quick"]);

    assert!(full.status.success(), "{}", stderr(&full));
    let report: serde_json::Value = serde_json::from_slice(&full.stdout).unwrap();
    assert_eq!(report["evidence"], "full");
    assert!(
        stderr(&full).contains("2 entries read, 0 reused"),
        "{}",
        stderr(&full)
    );
    let report: serde_json::Value = serde_json::from_slice(&quick.stdout).unwrap();
    assert_eq!(report["evidence"], "quick");
    assert!(
        stderr(&quick).contains("0 entries read, 2 reused"),
        "{}",
        stderr(&quick)
    );
}

#[test]
fn the_headless_client_fails_on_a_wrong_token_an_unreachable_server_or_a_failed_job() {
    let env = Env::new();

    let wrong = env.audit(&env.service.url(), &env.file("wrong"), &[]);
    assert!(!wrong.status.success());
    assert!(
        stderr(&wrong).contains("operator token"),
        "{}",
        stderr(&wrong)
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let unreachable = env.audit(&closed, &env.file("token"), &[]);
    assert!(!unreachable.status.success());
    assert!(
        stderr(&unreachable).contains("cannot reach"),
        "{}",
        stderr(&unreachable)
    );

    std::fs::remove_dir_all(env.fx.set_path(SET)).unwrap();
    let failed = env.audit(&env.service.url(), &env.file("token"), &[]);
    assert!(!failed.status.success());
    assert!(stderr(&failed).contains("failed"), "{}", stderr(&failed));
    assert!(failed.stdout.is_empty());
}

fn health(url: &str) -> Output {
    health_with(url, &[])
}

fn health_with(url: &str, env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_title-select"))
        .args(["health", "--timeout", "1", "--url", url])
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

/// A server that answers its first request with `response`, then closes.
fn answering(response: String) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/api/v1/health", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.read(&mut [0; 4096]);
        stream.write_all(response.as_bytes()).unwrap();
    });
    url
}

fn ok_with_body(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn closed_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[test]
fn health_exits_0_quietly_against_a_running_service_ignoring_proxy_settings() {
    let env = Env::new();
    let url = format!("{}/api/v1/health", env.service.url());
    let proxy = closed_port();

    for out in [
        health(&url),
        health_with(
            &url,
            &[
                ("HTTP_PROXY", &proxy),
                ("http_proxy", &proxy),
                ("ALL_PROXY", &proxy),
                ("all_proxy", &proxy),
            ],
        ),
    ] {
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn health_exits_1_with_a_one_line_reason_when_the_service_is_not_healthy() {
    let closed_url = format!("{}/api/v1/health", closed_port());
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_url = format!("http://{}/api/v1/health", silent.local_addr().unwrap());
    let not_found = answering("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into());
    let healthy = answering(ok_with_body(r#"{"status": "ok"}"#));
    let redirect = answering(format!(
        "HTTP/1.1 302 Found\r\nLocation: {healthy}\r\nContent-Length: 0\r\n\r\n"
    ));
    let wrong_body = answering(ok_with_body(r#"{"status": "failed"}"#));
    let long_body = answering(ok_with_body(&format!("failed\n{}", "x".repeat(10_000))));
    let newline_url = format!("{closed_url}\nsecond line");

    for (url, reason) in [
        (&closed_url, "cannot reach"),
        (&silent_url, "did not answer"),
        (&not_found, "404"),
        (&redirect, "302"),
        (&wrong_body, "failed"),
        (&long_body, "failed"),
        (&newline_url, "second line"),
    ] {
        let out = health(url);
        assert_eq!(out.status.code(), Some(1), "{url}: {}", stderr(&out));
        assert!(out.stdout.is_empty());
        let err = stderr(&out);
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(err.len() < 500, "{err}");
        assert!(err.contains(reason), "{url}: {err}");
    }
}

#[test]
fn health_refuses_a_timeout_out_of_range() {
    let out = Command::new(env!("CARGO_BIN_EXE_title-select"))
        .args(["health", "--timeout", "18446744073709551615"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}
