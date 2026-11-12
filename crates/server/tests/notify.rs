//! Webhook notifications and `/metrics`, against a fake webhook and a fake DAT Provider on local ports. No test sends a
//! request anywhere else.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};
use sha2::Digest;
use title_select_api::{Job, JobState};
use title_select_server::{Handle, Settings};
use title_select_store::config::Config;
use title_select_store::fetch::Endpoints;

const TOKEN: &str = "correct horse battery staple";
const SECRET: &str = "webhook secret";
const SET: &str = "Test - System";
const ESCAPED: &str = "Test%20-%20System";
const WORKBENCH: &str = "https://workbench.example";

/// A request a [`Fake`] server saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// An HTTP/1.1 server on a local port that answers every request with `status`, after `delay`, and records each.
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn start(status: u16, delay: Duration) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let recorded = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let recorded = recorded.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let mut headers = BTreeMap::new();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':') {
                            headers
                                .insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                        }
                    }
                    let length = headers
                        .get("content-length")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    if reader.read_exact(&mut body).is_err() {
                        return;
                    }
                    recorded.lock().unwrap().push(Seen {
                        path,
                        headers,
                        body,
                    });
                    std::thread::sleep(delay);
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(head.as_bytes());
                });
            }
        });
        Fake { url, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// Waits until `done` holds of what it has seen, failing after 30 seconds.
    fn wait_for(&self, what: &str, done: impl Fn(&[Seen]) -> bool) -> Vec<Seen> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let seen = self.seen();
            if done(&seen) {
                return seen;
            }
            assert!(Instant::now() < deadline, "never saw {what}: {seen:#?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The webhook payloads in `seen`, by event.
fn events(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .map(|s| {
            let body: Value = serde_json::from_slice(&s.body).unwrap();
            body["event"].as_str().unwrap().to_owned()
        })
        .collect()
}

/// HMAC-SHA256 (RFC 2104) of `body` under `key`, as lowercase hex.
fn hmac_hex(key: &[u8], body: &[u8]) -> String {
    let mut block = [0u8; 64];
    block[..key.len()].copy_from_slice(key);
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = sha2::Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(body)
        .finalize();
    let outer = sha2::Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    outer.iter().map(|b| format!("{b:02x}")).collect()
}

/// A store directory, roots `sets` and `play`, the token and webhook secret files.
struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let set = tmp.path().join("roots/sets").join(SET);
        std::fs::create_dir_all(&set).unwrap();
        write_file(&set.join("Pong (USA).a26"), b"pong");
        std::fs::create_dir_all(tmp.path().join("roots/sets/PSX")).unwrap();
        std::fs::create_dir_all(tmp.path().join("roots/play")).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        std::fs::write(tmp.path().join("token"), format!("{TOKEN}\n")).unwrap();
        std::fs::write(tmp.path().join("secret"), format!("{SECRET}\n")).unwrap();
        Env { tmp }
    }

    /// A Service posting to `hook`, with `notify` added to its `[notify]` table, fetching from `provider`.
    fn start(&self, hook: &Fake, notify: &str, provider: &Fake) -> Handle {
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n[roots.play]\npath = {:?}\nwritable = true\n[fetch]\nredump = true\n\
             [notify]\nurl = \"{}/hook\"\n{notify}",
            self.tmp.path().join("roots/sets").display().to_string(),
            self.tmp.path().join("roots/play").display().to_string(),
            hook.url,
        ))
        .unwrap();
        title_select_server::spawn(Settings {
            store: self.tmp.path().join("store"),
            config,
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: Some(self.tmp.path().join("token")),
            webhook_secret_file: Some(self.tmp.path().join("secret")),
            ui: None,
            clone_lists: None,
            hold_jobs: false,
            fetch_endpoints: Some(Endpoints::at(&provider.url)),
        })
        .unwrap()
    }

    fn dat(&self, version: &str) -> Vec<u8> {
        let path = write_dat_version(
            &self.tmp.path().join("in"),
            &format!("Test - System ({version}).dat"),
            version,
            &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
        );
        std::fs::read(path).unwrap()
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

/// An authenticated client of one Service.
struct Client {
    base: String,
}

impl Client {
    fn new(service: &Handle) -> Client {
        Client {
            base: format!("{}/api/v1", service.url()),
        }
    }

    fn auth(&self) -> String {
        format!("Bearer {TOKEN}")
    }

    fn send(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let request = match method {
            "PUT" => agent().put(url),
            _ => agent().post(url),
        };
        let mut response = request
            .header("Authorization", self.auth())
            .send_json(body)
            .unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_json().unwrap())
    }

    fn get(&self, path: &str) -> Value {
        agent()
            .get(format!("{}{path}", self.base))
            .header("Authorization", self.auth())
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    }

    fn upload(&self, provider: &str, file: &str, bytes: &[u8]) {
        let boundary = "title-select-test-boundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"provider\"\r\n\r\n{provider}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/xml\r\n\r\n"
        )
        .into_bytes();
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").bytes());
        let response = agent()
            .post(format!("{}/dat-revisions", self.base))
            .header("Authorization", self.auth())
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .unwrap();
        assert_eq!(response.status(), 201, "uploading {file}");
    }

    /// Waits for the Job to end, and returns it then.
    fn ended(&self, job: &Value) -> Job {
        let id = job["id"].as_i64().unwrap_or_else(|| panic!("{job}"));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let job: Job = serde_json::from_value(self.get(&format!("/jobs/{id}"))).unwrap();
            if job.state.is_final() {
                return job;
            }
            assert!(Instant::now() < deadline, "job {id} never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn register_and_audit(&self, name: &str, dat: &str) -> Job {
        let (status, body) = self.send(
            "POST",
            "/source-sets",
            json!({"root": "sets", "path": name, "dat": dat}),
        );
        assert_eq!(status, 201, "{body}");
        let (status, job) = self.send("POST", "/audits", json!({"source_set": name}));
        assert_eq!(status, 202, "{job}");
        self.ended(&job)
    }

    fn check(&self, folder: i64) -> Job {
        let (status, job) = self.send("POST", &format!("/libraries/{folder}/checks"), json!(null));
        assert_eq!(status, 202, "{job}");
        self.ended(&job)
    }
}

fn metrics(service: &Handle) -> (String, String) {
    let mut response = agent()
        .get(format!("{}/metrics", service.url()))
        .call()
        .unwrap();
    assert_eq!(response.status(), 200, "no token is needed");
    let kind = response.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_owned();
    (kind, response.body_mut().read_to_string().unwrap())
}

/// Each sample in Prometheus text exposition `text`, by its name and labels, checked as it is read: each sample's
/// metric has a TYPE, and each value is a number.
fn samples(text: &str) -> BTreeMap<String, f64> {
    let mut typed = Vec::new();
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap();
            assert!(["counter", "gauge"].contains(&kind), "{line}");
            typed.push(name.to_owned());
            continue;
        }
        if line.starts_with("# HELP ") || line.is_empty() {
            continue;
        }
        let (series, value) = line.rsplit_once(' ').unwrap();
        let name = series.split('{').next().unwrap();
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'),
            "{line}"
        );
        assert!(typed.iter().any(|t| t == name), "no TYPE for {line}");
        let value: f64 = value
            .parse()
            .unwrap_or_else(|_| panic!("not a value: {line}"));
        assert!(out.insert(series.to_owned(), value).is_none(), "{line}");
    }
    assert!(text.ends_with('\n'));
    out
}

/// Runs a Service through one of each event: two new DAT revisions, a finished audit, a source set gone stale, a
/// failed fetch, drift in a library folder and a failed check. Returns the Service and its client once all are done.
fn every_event(env: &Env, hook: &Fake, notify: &str) -> (Handle, Client) {
    let provider = Fake::start(503, Duration::ZERO);
    let service = env.start(hook, notify, &provider);
    let client = Client::new(&service);
    client.upload(
        "No-Intro",
        "Test - System (20260101).dat",
        &env.dat("20260101"),
    );
    assert_eq!(
        client
            .register_and_audit(SET, "No-Intro/Test - System")
            .state,
        JobState::Succeeded
    );
    let (status, _) = client.send(
        "PUT",
        &format!("/source-sets/{ESCAPED}/follow"),
        json!({"follow": true}),
    );
    assert_eq!(status, 200);
    client.upload(
        "No-Intro",
        "Test - System (20260201).dat",
        &env.dat("20260201"),
    );

    let psx = br#"<?xml version="1.0"?>
<datafile>
  <header><name>Sony - PlayStation</name><version>1</version></header>
  <game name="Game (USA)"><rom name="Game (USA).bin" size="4" crc="d87f7e0c"/></game>
</datafile>
"#;
    client.upload("Redump", "Sony - PlayStation.dat", psx);
    client.register_and_audit("PSX", "Redump/Sony - PlayStation");
    let (status, _) = client.send("PUT", "/source-sets/PSX/follow", json!({"follow": true}));
    assert_eq!(status, 200);
    let (status, body) = client.send("POST", "/fetch/redump", json!(null));
    assert!(status == 200 || status == 202, "{body}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while provider.seen().is_empty() {
        assert!(Instant::now() < deadline, "the fetch sent no request");
        std::thread::sleep(Duration::from_millis(20));
    }

    let (status, body) = client.send("POST", "/profiles", json!({"name": "Everyday"}));
    assert_eq!(status, 201, "{body}");
    let mut folders = Vec::new();
    for path in ["drifted", "gone"] {
        let (status, folder) = client.send(
            "POST",
            "/libraries",
            json!({"root": "play", "path": path, "source_set": SET, "profile": "Everyday"}),
        );
        assert_eq!(status, 201, "{folder}");
        folders.push(folder["id"].as_i64().unwrap());
    }
    let play = env.tmp.path().join("roots/play");
    write_file(&play.join("drifted/notes.txt"), b"mine");
    assert_eq!(client.check(folders[0]).state, JobState::Succeeded);
    std::fs::remove_dir_all(play.join("gone")).unwrap();
    assert_eq!(client.check(folders[1]).state, JobState::Failed);
    (service, client)
}

#[test]
fn each_event_reaches_the_webhook_signed_with_the_documented_payload() {
    let env = Env::new();
    let hook = Fake::start(204, Duration::ZERO);
    let (_service, _client) =
        every_event(&env, &hook, &format!("workbench_url = \"{WORKBENCH}/\"\n"));

    let wanted = [
        "dat_revision.new",
        "job.finished",
        "source_set.stale",
        "fetch.failed",
        "drift.found",
        "job.failed",
    ];
    let seen = hook.wait_for("every event", |seen| {
        let events = events(seen);
        wanted.iter().all(|w| events.iter().any(|e| e == w))
    });

    let mut payloads = BTreeMap::<String, Vec<Value>>::new();
    for s in &seen {
        assert_eq!(s.path, "/hook");
        assert_eq!(s.headers["content-type"], "application/json");
        assert_eq!(
            s.headers["x-title-select-signature"],
            format!("sha256={}", hmac_hex(SECRET.as_bytes(), &s.body)),
            "signed with the secret, trimmed"
        );
        let body: Value = serde_json::from_slice(&s.body).unwrap();
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["at", "event", "link", "subject", "summary"],
            "{body}"
        );
        assert!(
            body["at"]
                .as_str()
                .unwrap()
                .parse::<jiff::Timestamp>()
                .is_ok(),
            "{body}"
        );
        assert!(!body["summary"].as_str().unwrap().is_empty(), "{body}");
        payloads
            .entry(body["event"].as_str().unwrap().to_owned())
            .or_default()
            .push(body);
    }
    let subjects = |event: &str| -> Vec<(String, String)> {
        payloads[event]
            .iter()
            .map(|b| {
                (
                    b["subject"].as_str().unwrap().to_owned(),
                    b["link"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    assert_eq!(
        subjects("dat_revision.new"),
        [
            ("No-Intro/Test - System".into(), format!("{WORKBENCH}/dats")),
            ("No-Intro/Test - System".into(), format!("{WORKBENCH}/dats")),
            (
                "Redump/Sony - PlayStation".into(),
                format!("{WORKBENCH}/dats")
            ),
        ],
        "once per revision"
    );
    assert_eq!(
        subjects("source_set.stale"),
        [(SET.into(), format!("{WORKBENCH}/source-sets/{ESCAPED}"))],
        "once, as it went stale"
    );
    assert!(
        payloads["source_set.stale"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("20260201"),
        "{:?}",
        payloads["source_set.stale"]
    );
    assert_eq!(
        subjects("fetch.failed"),
        [("redump/psx".into(), format!("{WORKBENCH}/dats"))]
    );
    assert!(
        payloads["fetch.failed"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("503")
    );
    assert_eq!(
        subjects("drift.found"),
        [("play/drifted".into(), format!("{WORKBENCH}/libraries/1"))]
    );
    let jobs = |event: &str| -> Vec<String> {
        payloads[event]
            .iter()
            .map(|b| b["subject"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(jobs("job.finished").len(), 3, "two audits and a check");
    assert_eq!(jobs("job.failed").len(), 1);
    let failed = &payloads["job.failed"][0];
    assert!(
        failed["subject"].as_str().unwrap().starts_with("job "),
        "{failed}"
    );
    assert_eq!(failed["link"], format!("{WORKBENCH}/activity?state=failed"));
}

#[test]
fn only_the_events_you_choose_are_sent_and_a_link_needs_the_workbench_url() {
    let env = Env::new();
    let hook = Fake::start(200, Duration::ZERO);
    let (_service, _client) = every_event(&env, &hook, "events = [\"job.failed\"]\n");

    let seen = hook.wait_for("the failed job", |seen| !seen.is_empty());
    std::thread::sleep(Duration::from_millis(500));
    let seen = [seen, hook.seen()].concat();
    assert_eq!(events(&hook.seen()), ["job.failed"], "{seen:#?}");
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["link"], Value::Null);
}

#[test]
fn a_failing_webhook_is_tried_three_times_then_dropped_and_jobs_run_on() {
    let env = Env::new();
    let hook = Fake::start(500, Duration::from_millis(500));
    let provider = Fake::start(503, Duration::ZERO);
    let service = env.start(&hook, "events = [\"job.finished\"]\n", &provider);
    let client = Client::new(&service);
    client.upload(
        "No-Intro",
        "Test - System (20260101).dat",
        &env.dat("20260101"),
    );
    let started = Instant::now();
    assert_eq!(
        client
            .register_and_audit(SET, "No-Intro/Test - System")
            .state,
        JobState::Succeeded
    );
    let (_, again) = client.send("POST", "/audits", json!({"source_set": SET}));
    assert_eq!(client.ended(&again).state, JobState::Succeeded);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the jobs waited for the webhook: {:?}",
        started.elapsed()
    );

    let seen = hook.wait_for("six attempts", |seen| seen.len() >= 6);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(hook.seen().len(), 6, "three attempts each, then dropped");
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(seen[1].body, seen[2].body);
    assert_ne!(seen[2].body, seen[3].body);
}

#[test]
fn metrics_are_served_without_a_token_and_follow_jobs_stale_drift_and_fetches() {
    let env = Env::new();
    let hook = Fake::start(204, Duration::ZERO);
    let provider = Fake::start(503, Duration::ZERO);
    let service = env.start(&hook, "", &provider);
    let (kind, text) = metrics(&service);
    assert_eq!(kind, "text/plain; version=0.0.4; charset=utf-8");
    let before = samples(&text);
    assert_eq!(
        before[&format!(
            "title_select_build_info{{version=\"{}\"}}",
            env!("CARGO_PKG_VERSION")
        )],
        1.0
    );
    assert_eq!(before["title_select_jobs_running"], 0.0);
    assert_eq!(before["title_select_source_sets_stale"], 0.0);
    assert_eq!(
        before["title_select_jobs_total{kind=\"audit\",outcome=\"succeeded\"}"],
        0.0
    );
    drop(service);

    let (service, client) = every_event(&env, &hook, "");
    // The fetch runs on after the request that started it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let after = loop {
        let after = samples(&metrics(&service).1);
        if after
            .keys()
            .any(|k| k.starts_with("title_select_fetch_last_result"))
        {
            break after;
        }
        assert!(Instant::now() < deadline, "no fetch recorded");
        std::thread::sleep(Duration::from_millis(50));
    };
    let text = metrics(&service).1;
    assert_eq!(
        after["title_select_jobs_total{kind=\"audit\",outcome=\"succeeded\"}"],
        2.0
    );
    assert_eq!(
        after["title_select_jobs_total{kind=\"check\",outcome=\"succeeded\"}"],
        1.0
    );
    assert_eq!(
        after["title_select_jobs_total{kind=\"check\",outcome=\"failed\"}"],
        1.0
    );
    assert_eq!(after["title_select_jobs_running"], 0.0);
    assert_eq!(after["title_select_source_sets_stale"], 1.0);
    assert_eq!(
        after[&format!("title_select_source_set_stale{{source_set=\"{SET}\"}}")],
        1.0
    );
    assert_eq!(
        after["title_select_source_set_stale{source_set=\"PSX\"}"],
        0.0
    );
    let audited: Job = serde_json::from_value(
        client
            .get("/jobs")
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["kind"] == "audit" && j["source_set"] == SET)
            .unwrap()
            .clone(),
    )
    .unwrap();
    let last_audit = after
        [&format!("title_select_source_set_last_audit_timestamp_seconds{{source_set=\"{SET}\"}}")];
    let finished: jiff::Timestamp = audited.finished_at.unwrap().parse().unwrap();
    assert!(
        (last_audit - finished.as_second() as f64).abs() < 2.0,
        "{last_audit}"
    );
    assert_eq!(
        after["title_select_fetch_last_result{provider=\"redump\",system=\"psx\",outcome=\"failed\"}"],
        1.0
    );
    assert!(
        after["title_select_fetch_last_timestamp_seconds{provider=\"redump\",system=\"psx\"}"]
            > 1.7e9
    );
    assert_eq!(
        after[&format!(
            "title_select_library_folder_drift{{library_folder=\"1\",root=\"play\",source_set=\"{SET}\"}}"
        )],
        1.0
    );
    for private in [
        env.tmp.path().display().to_string(),
        "drifted".into(),
        "Pong".into(),
        "notes.txt".into(),
    ] {
        assert!(!text.contains(&private), "{private} in {text}");
    }
}

#[test]
fn a_webhook_without_its_secret_refuses_to_start() {
    let env = Env::new();
    let config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n[notify]\nurl = \"http://127.0.0.1:9/hook\"\n",
        env.tmp.path().join("roots/sets").display().to_string(),
    ))
    .unwrap();
    let started = title_select_server::spawn(Settings {
        store: env.tmp.path().join("store"),
        config,
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: Some(env.tmp.path().join("token")),
        webhook_secret_file: None,
        ui: None,
        clone_lists: None,
        hold_jobs: false,
        fetch_endpoints: None,
    });
    let Err(err) = started else {
        panic!("it started")
    };
    assert!(
        err.to_string().contains("TITLE_SELECT_WEBHOOK_SECRET_FILE"),
        "{err}"
    );
}
