#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::io::BufRead;
use std::path::PathBuf;

use common::*;
use serde::de::DeserializeOwned;
use title_select_api::{
    AuditEvidence, AuditSummary, EntryRow, ErrorBody, Finding, GameRow, ImportOutcome,
    ImportedDatRevision, Job, JobEvent, JobState, Page, Report, Reuse, SourceSetRecord,
};
use title_select_server::{Handle, Settings, StartError, TOKEN_ACTOR};
use title_select_store::config::Config;

const TOKEN: &str = "correct horse battery staple";
const SET: &str = "Test - System";

/// A store directory, a Root called `sets` holding one Source Set folder, a DAT to upload and a token file.
struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let set = tmp.path().join("roots/sets").join(SET);
        std::fs::create_dir_all(&set).unwrap();
        write_file(&set.join("Pong (USA).a26"), b"pong");
        write_zip(
            &set.join("Combat (USA).zip"),
            &[("Combat (USA).a26", b"combat")],
        );
        write_file(&set.join("notes.txt"), b"not a rom");
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        std::fs::write(tmp.path().join("token"), format!("{TOKEN}\n")).unwrap();
        Env { tmp }
    }

    fn store(&self) -> PathBuf {
        self.tmp.path().join("store")
    }

    fn config(&self, extra: &str) -> Config {
        let root = self.tmp.path().join("roots/sets");
        Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n{extra}",
            root.display().to_string()
        ))
        .unwrap()
    }

    fn settings(&self, extra: &str) -> Settings {
        Settings {
            store: self.store(),
            config: self.config(extra),
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: Some(self.tmp.path().join("token")),
            ui: None,
            retool: None,
            hold_jobs: false,
        }
    }

    fn start(&self) -> Handle {
        title_select_server::spawn(self.settings("")).unwrap()
    }

    fn dat(&self) -> Vec<u8> {
        let path = write_dat_version(
            &self.tmp.path().join("in"),
            "Test - System (20260101).dat",
            "20260101-000000",
            &[
                ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
                ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
            ],
        );
        std::fs::read(path).unwrap()
    }
}

fn bearer(token: &str) -> Option<(String, String)> {
    Some(("Authorization".into(), format!("Bearer {token}")))
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
    /// A header to send with every request, and its value.
    auth: Option<(String, String)>,
}

impl Client {
    fn new(service: &Handle) -> Client {
        Client {
            base: format!("{}/api/v1", service.url()),
            auth: bearer(TOKEN),
        }
    }

    fn get(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let request = agent().get(format!("{}{path}", self.base));
        match &self.auth {
            Some((name, value)) => request.header(name, value),
            None => request,
        }
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let request = agent().post(format!("{}{path}", self.base));
        match &self.auth {
            Some((name, value)) => request.header(name, value),
            None => request,
        }
    }

    fn json<T: DeserializeOwned>(&self, path: &str) -> T {
        let mut response = self.get(path).call().unwrap();
        assert_eq!(response.status(), 200, "GET {path}");
        response.body_mut().read_json().unwrap()
    }

    fn upload(&self, file: &str, bytes: &[u8]) -> (u16, ImportedDatRevision) {
        let boundary = "title-select-test-boundary";
        let mut body = Vec::new();
        for (name, value) in [("provider", "No-Intro"), ("source_url", "")] {
            body.extend(format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ).bytes());
        }
        body.extend(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/xml\r\n\r\n"
        ).bytes());
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").bytes());
        let mut response = self
            .post("/dat-revisions")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_json().unwrap())
    }

    fn add_set(&self) -> SourceSetRecord {
        let mut response = self
            .post("/source-sets")
            .send_json(
                serde_json::json!({"root": "sets", "path": SET, "dat": "No-Intro/Test - System"}),
            )
            .unwrap();
        assert_eq!(response.status(), 201);
        response.body_mut().read_json().unwrap()
    }

    fn start_audit(&self) -> ureq::http::Response<ureq::Body> {
        self.post("/audits")
            .send_json(serde_json::json!({"source_set": SET}))
            .unwrap()
    }

    /// Subscribes to every Job's events, returning once the `resync` sent on connecting has arrived, with its Jobs.
    fn watch(&self) -> (Events, title_select_api::JobSnapshot) {
        let response = self.get("/events").call().unwrap();
        assert_eq!(response.status(), 200);
        let mut events = Events(Box::new(
            std::io::BufReader::new(response.into_body().into_reader()).lines(),
        ));
        let (name, data) = events.next_event();
        assert_eq!(name, "resync");
        (events, serde_json::from_str(&data).unwrap())
    }

    /// Follows the Job's events to its final state, returning the event names and the last Job.
    fn follow(&self, id: i64) -> (Vec<String>, Job) {
        let response = self.get(&format!("/jobs/{id}/events")).call().unwrap();
        assert_eq!(response.status(), 200);
        let reader = std::io::BufReader::new(response.into_body().into_reader());
        let (mut names, mut name, mut last) = (Vec::new(), String::new(), None);
        for line in reader.lines() {
            let line = line.unwrap();
            if let Some(n) = line.strip_prefix("event: ") {
                name = n.to_owned();
            } else if let Some(data) = line.strip_prefix("data: ") {
                names.push(name.clone());
                if name == "state" {
                    last = Some(serde_json::from_str::<Job>(data).unwrap());
                }
            }
        }
        (names, last.expect("a state event"))
    }
}

/// A stream of server-sent events.
struct Events(Box<dyn Iterator<Item = std::io::Result<String>>>);

impl Events {
    /// The next event's name and data.
    fn next_event(&mut self) -> (String, String) {
        let mut name = String::new();
        for line in &mut self.0 {
            let line = line.unwrap();
            if let Some(n) = line.strip_prefix("event: ") {
                name = n.to_owned();
            } else if let Some(data) = line.strip_prefix("data: ") {
                return (name, data.to_owned());
            }
        }
        panic!("the event stream ended");
    }

    /// The names of the Job `id`'s events until its final state, and the Job then.
    fn follow(&mut self, id: i64) -> (Vec<String>, Job) {
        let mut names = Vec::new();
        loop {
            let (name, data) = self.next_event();
            let event: JobEvent = serde_json::from_str(&data).unwrap();
            if event.job_id() != Some(id) {
                continue;
            }
            names.push(name);
            if let JobEvent::State(job) = event
                && job.state.is_final()
            {
                return (names, *job);
            }
        }
    }
}

fn status_of(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> u16 {
    response.unwrap().status().as_u16()
}

#[test]
fn startup_is_refused_with_no_auth_configured() {
    let env = Env::new();
    let settings = Settings {
        token_file: None,
        ..env.settings("")
    };

    let refused = title_select_server::spawn(settings);

    assert!(
        matches!(refused, Err(StartError::Auth(_))),
        "{:?}",
        refused.err()
    );
    assert!(
        !env.store().exists(),
        "nothing is opened before auth is checked"
    );
}

#[test]
fn startup_is_refused_with_dev_identity_off_loopback() {
    let env = Env::new();
    let settings = Settings {
        listen: "0.0.0.0:0".parse().unwrap(),
        ..env.settings("[auth]\ndev_identity = \"Developer\"\n")
    };

    let refused = title_select_server::spawn(settings);

    let Err(StartError::Auth(message)) = refused else {
        panic!("expected a refusal");
    };
    assert!(message.contains("loopback"), "{message}");
}

#[test]
fn dev_identity_on_loopback_authenticates_requests_with_no_credential() {
    let env = Env::new();
    let settings = Settings {
        token_file: None,
        ..env.settings("[auth]\ndev_identity = \"Developer\"\n")
    };
    let service = title_select_server::spawn(settings).unwrap();
    let client = Client {
        auth: None,
        ..Client::new(&service)
    };

    assert_eq!(status_of(client.get("/jobs").call()), 200);
    let wrong = Client {
        auth: bearer("nope"),
        ..Client::new(&service)
    };
    assert_eq!(status_of(wrong.get("/jobs").call()), 401);
}

#[test]
fn the_workbench_is_served_beside_the_api_without_hiding_its_errors() {
    let env = Env::new();
    let ui = env.tmp.path().join("site");
    std::fs::create_dir_all(ui.join("pkg")).unwrap();
    std::fs::write(
        ui.join("workbench.html"),
        "<!doctype html><title>workbench</title>",
    )
    .unwrap();
    std::fs::write(ui.join("pkg/title-select.js"), "export default 1;").unwrap();
    let service = title_select_server::spawn(Settings {
        ui: Some(ui),
        ..env.settings("")
    })
    .unwrap();
    let page = |path: &str| {
        let mut response = agent()
            .get(format!("{}{path}", service.url()))
            .call()
            .unwrap();
        let csp = response.headers().get("content-security-policy").cloned();
        (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap(),
            csp,
        )
    };

    let (status, body, csp) = page("/");
    assert_eq!(status, 200);
    assert!(body.contains("workbench"));
    assert!(
        csp.unwrap()
            .to_str()
            .unwrap()
            .starts_with("default-src 'self'")
    );
    let (status, body, _) = page("/audits/7");
    assert_eq!(
        (status, body.contains("workbench")),
        (200, true),
        "a screen's URL loads the workbench"
    );
    assert_eq!(page("/pkg/title-select.js").1, "export default 1;");

    for path in ["/api/v1/no-such-thing", "/api/v2/jobs", "/api"] {
        let (status, body, _) = page(path);
        assert_eq!(status, 404, "{path}");
        let error: ErrorBody = serde_json::from_str(&body).unwrap();
        assert_eq!(error.error, "no such API path");
    }
    assert_eq!(
        page("/api/v1/jobs").0,
        401,
        "the API still needs a credential"
    );
}

#[test]
fn a_missing_or_wrong_token_is_rejected_and_the_correct_one_accepted() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);

    let missing = Client {
        auth: None,
        ..Client::new(&service)
    };
    let response = missing.get("/source-sets").call().unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["WWW-Authenticate"], "Bearer");
    for wrong in [
        "Bearer correct horse",
        "Bearer ",
        "Basic Zm9vOmJhcg==",
        TOKEN,
    ] {
        let wrong = Client {
            auth: Some(("Authorization".into(), wrong.into())),
            ..Client::new(&service)
        };
        assert_eq!(
            status_of(wrong.get("/source-sets").call()),
            401,
            "{:?}",
            wrong.auth
        );
    }

    assert_eq!(status_of(client.get("/source-sets").call()), 200);
    assert_eq!(
        status_of(missing.get("/health").call()),
        200,
        "health is unauthenticated"
    );
}

#[test]
fn an_identity_header_is_trusted_only_from_a_trusted_proxy() {
    let env = Env::new();
    let header = "[auth]\nidentity_header = \"Remote-User\"\n";
    let untrusted = title_select_server::spawn(Settings {
        token_file: None,
        ..env.settings(&format!("{header}trusted_proxies = [\"192.0.2.0/24\"]\n"))
    })
    .unwrap();
    let client = Client {
        auth: None,
        ..Client::new(&untrusted)
    };

    let response = client.get("/jobs").header("Remote-User", "Operator").call();

    assert_eq!(status_of(response), 401);
    assert_eq!(status_of(client.get("/jobs").call()), 401);
    drop(untrusted);

    let trusted = title_select_server::spawn(Settings {
        token_file: None,
        ..env.settings(&format!(
            "{header}trusted_proxies = [\"127.0.0.0/8\", \"::1/128\"]\n"
        ))
    })
    .unwrap();
    let client = Client {
        auth: Some(("Remote-User".into(), "the Operator".into())),
        ..Client::new(&trusted)
    };
    client.upload("Test - System.dat", &env.dat());
    client.add_set();

    let mut response = client.start_audit();

    assert_eq!(response.status(), 202);
    let job: Job = response.body_mut().read_json().unwrap();
    assert_eq!(job.actor, "the Operator");
}

#[test]
fn the_api_uploads_a_dat_registers_a_source_set_and_audits_it_end_to_end() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let dat = env.dat();

    let (status, uploaded) = client.upload("Test - System (20260101).dat", &dat);
    assert_eq!((status, uploaded.outcome), (201, ImportOutcome::Added));
    assert_eq!(uploaded.dat_revision.version, "20260101-000000");
    assert_eq!(uploaded.dat_revision.file, "Test - System (20260101).dat");
    let (status, again) = client.upload("renamed.dat", &dat);
    assert_eq!(
        (status, again.outcome),
        (200, ImportOutcome::AlreadyImported)
    );
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert_eq!(dats.len(), 1);
    assert_eq!(dats[0].dat, "No-Intro/Test - System");
    let roots: Vec<title_select_api::Root> = client.json("/roots");
    assert_eq!(roots[0].name, "sets");

    let set = client.add_set();
    assert_eq!(set.name, SET);
    let fetched: SourceSetRecord = client.json(&format!("/source-sets/{}", "Test%20-%20System"));
    assert_eq!(fetched, set);

    let (mut watching, snapshot) = client.watch();
    assert!(snapshot.jobs.is_empty());
    let mut response = client.start_audit();
    assert_eq!(response.status(), 202);
    let job: Job = response.body_mut().read_json().unwrap();
    assert_eq!(
        (job.actor.as_str(), job.dat_revision.as_str()),
        (TOKEN_ACTOR, uploaded.dat_revision.sha256.as_str())
    );

    let (events, done) = watching.follow(job.id);
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert!(events.contains(&"progress".to_string()), "{events:?}");
    let (_, followed) = client.follow(job.id);
    assert_eq!(
        followed, done,
        "a finished Job's stream sends its final state"
    );
    let (_, snapshot) = client.watch();
    assert_eq!(
        snapshot.jobs,
        vec![done.clone()],
        "the snapshot has recently finished Jobs"
    );

    let job: Job = client.json(&format!("/jobs/{}", job.id));
    let audit_id = job.audit.unwrap();
    let summary: AuditSummary = client.json(&format!("/audits/{audit_id}"));
    assert_eq!(summary.findings[&Finding::Matched], 2);
    let history: Vec<AuditSummary> = client.json("/source-sets/Test%20-%20System/history");
    assert_eq!(history, vec![summary]);

    let report: Report = client.json(&format!("/audits/{audit_id}/report"));
    assert_eq!(report.schema, title_select_api::SCHEMA);
    assert_eq!(report.entries.len(), 3);

    let first: Page<EntryRow> = client.json(&format!("/audits/{audit_id}/entries?limit=2"));
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.unwrap();
    let rest: Page<EntryRow> = client.json(&format!(
        "/audits/{audit_id}/entries?limit=2&cursor={cursor}"
    ));
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next_cursor.is_none());
    let paged: Vec<_> = first
        .items
        .into_iter()
        .chain(rest.items)
        .map(|r| r.entry)
        .collect();
    assert_eq!(paged, report.entries);
    let unmatched: Page<EntryRow> =
        client.json(&format!("/audits/{audit_id}/entries?finding=unmatched"));
    assert_eq!(unmatched.items.len(), 1);
    let missing: Page<GameRow> = client.json(&format!("/audits/{audit_id}/games?status=missing"));
    assert_eq!(missing.items.len(), 1);
    assert_eq!(missing.items[0].game.name, "Breakout (USA)");

    assert_eq!(status_of(client.get("/audits/999").call()), 404);
    assert_eq!(
        status_of(client.get("/audits/1/entries?cursor=x").call()),
        400
    );
    assert_eq!(status_of(client.get("/nowhere").call()), 404);
}

#[test]
fn a_source_set_registered_without_a_path_takes_the_one_its_template_proposes() {
    let env = Env::new();
    std::fs::create_dir_all(env.tmp.path().join("roots/sets/No-Intro/Test - System")).unwrap();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System (20260101).dat", &env.dat());
    let send = |body: serde_json::Value| {
        let mut response = client.post("/source-sets").send_json(body).unwrap();
        let status = response.status().as_u16();
        (
            status,
            response
                .body_mut()
                .read_json::<serde_json::Value>()
                .unwrap(),
        )
    };

    let defaults: title_select_api::PathTemplateDefaults = client.json("/path-templates");
    assert_eq!(
        (
            defaults.source_set.as_str(),
            defaults.managed_folder.as_str()
        ),
        ("{provider}/{dat}", "{platform}")
    );
    let (status, both) = send(serde_json::json!(
        {"root": "sets", "path": SET, "template": "{dat}", "dat": "No-Intro/Test - System"}
    ));
    assert_eq!(status, 400, "{both}");
    let (status, unmapped) = send(serde_json::json!(
        {"root": "sets", "template": "{platform}", "dat": "No-Intro/Test - System"}
    ));
    assert_eq!(status, 400);
    assert!(
        unmapped["error"]
            .as_str()
            .unwrap()
            .contains("type the path"),
        "{unmapped}"
    );

    let (status, proposed) =
        send(serde_json::json!({"root": "sets", "dat": "No-Intro/Test - System"}));
    assert_eq!(status, 201, "{proposed}");
    assert_eq!(proposed["path"], "No-Intro/Test - System");
    let (status, absent) = send(serde_json::json!(
        {"root": "sets", "template": "Absent/{dat}", "dat": "No-Intro/Test - System"}
    ));
    assert_eq!(status, 400);
    assert!(
        absent["error"]
            .as_str()
            .unwrap()
            .contains("Absent/Test - System"),
        "the override proposed the path: {absent}"
    );
}

#[test]
fn the_bulk_ndjson_endpoint_returns_every_entry() {
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    let extra: Vec<String> = (0..250).map(|i| format!("extra {i:03}.bin")).collect();
    for name in &extra {
        write_file(&set.join(name), name.as_bytes());
    }
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    let (_, done) = client.follow(job.id);
    let audit_id = done.audit.unwrap();

    let response = client
        .get(&format!("/audits/{audit_id}/entries.ndjson"))
        .call()
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Content-Type"], "application/x-ndjson");
    let text = response.into_body().read_to_string().unwrap();
    let rows: Vec<EntryRow> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let report: Report = client.json(&format!("/audits/{audit_id}/report"));
    assert_eq!(rows.len(), 253);
    assert_eq!(
        rows.into_iter().map(|r| r.entry).collect::<Vec<_>>(),
        report.entries
    );
}

#[test]
fn an_audit_is_incremental_unless_you_ask_the_api_for_a_full_one() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let first: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(first.evidence, Some(AuditEvidence::Incremental));
    client.follow(first.id);

    let second: Job = client.start_audit().body_mut().read_json().unwrap();
    let (_, second) = client.follow(second.id);
    let full: Job = client
        .post("/audits")
        .send_json(serde_json::json!({"source_set": SET, "evidence": "full"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(full.evidence, Some(AuditEvidence::Full));
    let (_, full) = client.follow(full.id);

    let reused: Report = client.json(&format!("/audits/{}/report", second.audit.unwrap()));
    assert_eq!(reused.evidence, AuditEvidence::Incremental);
    let payload = reused.entries.iter().filter_map(|e| e.evidence.as_ref());
    assert!(payload.clone().count() > 0);
    assert!(
        payload
            .clone()
            .all(|e| e.reused == Some(Reuse::UnchangedFileHeuristic))
    );
    let read: Report = client.json(&format!("/audits/{}/report", full.audit.unwrap()));
    assert_eq!(read.evidence, AuditEvidence::Full);
    assert!(
        read.entries
            .iter()
            .filter_map(|e| e.evidence.as_ref())
            .all(|e| e.reused.is_none() && e.read_at.is_some())
    );
}

#[test]
fn a_second_audit_of_the_same_source_set_is_refused_naming_the_active_job() {
    let env = Env::new();
    let service = title_select_server::spawn(Settings {
        hold_jobs: true,
        ..env.settings("")
    })
    .unwrap();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let first: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(first.state, JobState::Queued);

    let mut second = client.start_audit();

    assert_eq!(second.status(), 409);
    let body: ErrorBody = second.body_mut().read_json().unwrap();
    assert_eq!(body.job, Some(first.id));
    assert!(
        body.error.contains(&format!("job {}", first.id)),
        "{}",
        body.error
    );
}

#[test]
fn starting_the_service_interrupts_a_job_left_running_and_runs_it_again() {
    let env = Env::new();
    {
        let store = title_select_store::Store::open(&env.store()).unwrap();
        let dat = env.tmp.path().join("in/Test - System.dat");
        std::fs::write(&dat, env.dat()).unwrap();
        store.import_dat(&dat, "No-Intro", None).unwrap();
        store
            .add_source_set(&env.config(""), "sets", SET, "No-Intro/Test - System")
            .unwrap();
        let workers = title_select_store::Workers {
            readers: 1,
            hashers: 1,
        };
        store
            .enqueue_audit(
                SET,
                None,
                "Operator",
                workers,
                title_select_api::AuditEvidence::Incremental,
            )
            .unwrap();
        store.claim_job().unwrap();
    }

    let service =
        title_select_server::spawn(env.settings("[jobs]\npace = \"unthrottled\"\n")).unwrap();
    let client = Client::new(&service);

    let jobs: Vec<Job> = client.json("/jobs");
    let (again, first) = (&jobs[0], &jobs[1]);
    assert_eq!(first.state, JobState::Interrupted);
    assert_eq!((again.attempt, again.requeued_from), (2, Some(first.id)));
    let cpus = std::thread::available_parallelism().unwrap().get();
    assert_eq!(again.workers.readers, cpus);
    let (_, done) = client.follow(again.id);
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn the_service_holds_the_store_lock() {
    let env = Env::new();
    let service = env.start();

    let refused = title_select_store::Store::open(&env.store());

    assert!(refused.is_err());
    drop(service);
    title_select_store::Store::open(&env.store()).unwrap();
}

/// An Ingest Folder registered, scanned by a Job, its items assessed against the audited Source Set, and an item
/// discarded only through an approved plan.
#[test]
fn the_api_scans_an_ingest_folder_and_discards_an_item_only_through_an_approved_plan() {
    use title_select_api::ingest::{
        IngestFolder, IngestItem, IngestKeptFile, IngestPlan, ItemState, Need, Proposal,
    };
    let env = Env::new();
    let drop = env.tmp.path().join("drop");
    std::fs::create_dir_all(&drop).unwrap();
    let service = title_select_server::spawn(env.settings(&format!(
        "[roots.drop]\npath = {:?}\nwritable = true\nkind = \"ingest\"\nsettle_secs = 0\n",
        drop.display().to_string()
    )))
    .unwrap();
    let client = Client::new(&service);
    let (status, _) = client.upload("Test - System (20260101).dat", &env.dat());
    assert_eq!(status, 201);
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    let roots: Vec<title_select_api::Root> = client.json("/roots");
    assert!(roots.iter().any(|r| r.name == "drop" && r.ingest));

    let mut response = client
        .post("/ingest-folders")
        .send_json(serde_json::json!({"root": "drop"}))
        .unwrap();
    assert_eq!(response.status(), 201);
    let folder: IngestFolder = response.body_mut().read_json().unwrap();
    assert!(folder.id.is_some());
    write_file(&drop.join("Breakout (USA).a26"), b"break");
    write_file(&drop.join("mystery.bin"), b"nobody knows");
    let scan = || {
        let mut response = client
            .post("/ingest-folders/drop/scans")
            .send_empty()
            .unwrap();
        assert_eq!(response.status(), 202);
        let job: Job = response.body_mut().read_json().unwrap();
        assert_eq!(job.ingest_folder.as_deref(), Some("drop"));
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    };
    scan();
    scan();

    let items: Vec<IngestItem> = client.json("/ingest-folders/drop/items");
    let named = |name: &str| items.iter().find(|i| i.name == name).unwrap().clone();
    let breakout = named("Breakout (USA).a26");
    assert_eq!(breakout.state, ItemState::Assessed);
    assert_eq!(
        breakout.assessment.proposal,
        Some(Proposal::SourceSet {
            source_set: SET.into(),
            game: "Breakout (USA)".into(),
            need: Need::Missing,
        })
    );
    let mystery = named("mystery.bin");
    assert!(matches!(
        mystery.assessment.proposal,
        Some(Proposal::OperatorAddition { .. })
    ));

    let mut response = client
        .post("/ingest-folders/drop/plans")
        .send_json(serde_json::json!({"action": "discard", "items": [mystery.id]}))
        .unwrap();
    assert_eq!(response.status(), 201);
    let plan: IngestPlan = response.body_mut().read_json().unwrap();
    assert!(
        drop.join("mystery.bin").exists(),
        "planning changes nothing"
    );
    let approval = format!("/ingest-plans/{}/approval", plan.id);
    let wrong = client
        .post(&approval)
        .send_json(serde_json::json!({"digest": "0".repeat(64)}));
    assert_eq!(status_of(wrong), 409);

    let mut response = client
        .post(&approval)
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let done: IngestPlan = response.body_mut().read_json().unwrap();
    assert!(done.outcomes.iter().all(|o| o.moved), "{done:?}");
    assert!(!drop.join("mystery.bin").exists());
    let kept: Vec<IngestKeptFile> = client.json("/ingest-folders/drop/kept-files");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "mystery.bin");
    let item: IngestItem = client.json(&format!("/ingest-items/{}", mystery.id));
    assert_eq!(item.state, ItemState::Moved);
    assert_eq!(
        status_of(client.get("/ingest-folders/sets/items").call()),
        404
    );
}

#[test]
fn the_api_repairs_a_source_set_only_through_an_approved_plan_and_keeps_what_it_sets_aside() {
    use title_select_api::repair::{RepairPlan, SourceKeptDeletion, SourceKeptFile};
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    let full = |client: &Client| {
        let job: Job = client
            .post("/audits")
            .send_json(serde_json::json!({"source_set": SET, "evidence": "full"}))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    };
    let repairs = "/source-sets/Test%20-%20System/repairs";

    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System (20260101).dat", &env.dat());
    client.add_set();
    full(&client);
    let refused = client.post(repairs).send_json(serde_json::json!({}));
    assert_eq!(status_of(refused), 400, "the root is not writable");
    drop(service);

    let service =
        title_select_server::spawn(env.settings(&format!("writable_paths = [{SET:?}]\n"))).unwrap();
    let client = Client::new(&service);
    let roots: Vec<title_select_api::Root> = client.json("/roots");
    assert!(
        roots
            .iter()
            .any(|r| r.name == "sets" && r.writable_paths == [SET])
    );
    let mut response = client
        .post(repairs)
        .send_json(serde_json::json!({}))
        .unwrap();
    assert_eq!(response.status(), 201);
    let plan: RepairPlan = response.body_mut().read_json().unwrap();
    assert!(plan.content.creates_marker);
    assert!(set.join("notes.txt").exists(), "planning changes nothing");
    let listed: Vec<RepairPlan> = client.json(repairs);
    assert_eq!(listed.iter().map(|p| p.id).collect::<Vec<_>>(), [plan.id]);

    let approval = format!("/repairs/{}/approval", plan.id);
    let wrong = client
        .post(&approval)
        .send_json(serde_json::json!({"digest": "0".repeat(64)}));
    assert_eq!(status_of(wrong), 409);
    let mut response = client
        .post(&approval)
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let approved: RepairPlan = response.body_mut().read_json().unwrap();
    let repair_job = approved.job.expect("approval queues a repair job");
    let (_, ended) = client.follow(repair_job);
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let jobs: Vec<Job> = client.json("/jobs");
    assert!(
        jobs.iter()
            .any(|j| j.id == repair_job && j.repair == Some(plan.id))
    );
    let refused = client
        .post(&format!("/jobs/{repair_job}/cancellation"))
        .send_empty();
    assert_eq!(
        status_of(refused),
        409,
        "a repair job that ended cannot be cancelled"
    );
    assert!(set.join("Pong (USA).zip").exists());
    assert!(!set.join("notes.txt").exists());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let done = loop {
        let fetched: RepairPlan = client.json(&format!("/repairs/{}", plan.id));
        if fetched.audit_job.is_some() || std::time::Instant::now() > deadline {
            break fetched;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(
        done.finished_at.is_some() && done.outcomes.iter().all(|o| o.done),
        "{done:?}"
    );
    assert_eq!(done.digest, plan.digest);
    let job = done.audit_job.expect("a repair queues an audit");
    assert_eq!(client.follow(job).1.state, JobState::Succeeded);

    let kept: Vec<SourceKeptFile> = client.json("/source-sets/Test%20-%20System/kept-files");
    let notes = kept
        .iter()
        .find(|k| k.name == "notes.txt")
        .expect("notes.txt is kept");
    let mut response = client
        .post("/source-sets/Test%20-%20System/kept-deletions")
        .send_json(serde_json::json!({"ids": [notes.id]}))
        .unwrap();
    assert_eq!(response.status(), 201);
    let deletion: SourceKeptDeletion = response.body_mut().read_json().unwrap();
    let mut response = client
        .post(&format!("/source-kept-deletions/{}/approval", deletion.id))
        .send_json(serde_json::json!({"digest": deletion.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let deleted: SourceKeptDeletion = response.body_mut().read_json().unwrap();
    assert!(deleted.outcomes.iter().all(|o| o.deleted), "{deleted:?}");
    let kept: Vec<SourceKeptFile> = client.json("/source-sets/Test%20-%20System/kept-files");
    assert!(kept.iter().all(|k| k.id != notes.id));
}

/// Selection Profiles and Selections, run by the pinned selection engine (`tools/retool/setup` builds it).
mod selections {
    use super::*;
    use serde_json::{Value, json};
    use title_select_api::selection::{ChoiceStatus, Role, Selection};
    use title_select_api::{
        GameStatus, Preview, ProfileRecord, SavedProfile, SelectionDiff, SelectionRecord,
        SelectionReleaseRow,
    };

    const PROFILE: &str = "/profiles/Everyday/versions";

    /// The Test - System DAT with a Pong clone and a demo, as revision `version`; `pong_europe` leaves the clone
    /// out.
    fn dat(env: &Env, version: &str, pong_europe: bool) -> Vec<u8> {
        let mut games = vec![
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
            (
                "Breakout (USA) (Demo)",
                vec![rom("Breakout (USA) (Demo).a26", b"break")],
            ),
        ];
        if pong_europe {
            games.push(("Pong (Europe)", vec![rom("Pong (Europe).a26", b"pong-eu")]));
        }
        let path = write_dat_version(
            &env.tmp.path().join("in"),
            &format!("Test - System ({version}).dat"),
            version,
            &games,
        );
        std::fs::read(path).unwrap()
    }

    /// A Service with the DAT uploaded, the Source Set registered and audited.
    fn ready(env: &Env) -> (Handle, Client) {
        let service = env.start();
        let client = Client::new(&service);
        let (status, _) =
            client.upload("Test - System (20260101).dat", &dat(env, "20260101", true));
        assert_eq!(status, 201);
        client.add_set();
        let mut response = client.start_audit();
        assert_eq!(response.status(), 202);
        let job: Job = response.body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        (service, client)
    }

    fn send(request: ureq::RequestBuilder<ureq::typestate::WithBody>, body: Value) -> (u16, Value) {
        let mut response = request.send_json(body).unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_json().unwrap())
    }

    /// The Profile's newest version, if it has one.
    fn latest(client: &Client) -> Option<u32> {
        let response = client.get("/profiles/Everyday").call().unwrap();
        if response.status() == 404 {
            return None;
        }
        let record: ProfileRecord = response.into_body().read_json().unwrap();
        record.latest.map(|v| v.version)
    }

    /// Saves `profile` as the next version, queueing a Selection, and returns the save and the Selection made.
    fn save(client: &Client, profile: Value) -> (SavedProfile, SelectionRecord) {
        save_on(client, profile, None)
    }

    /// Saves `profile`, queueing a Selection on the DAT Revision `revision`, by default the one last audited.
    fn save_on(
        client: &Client,
        profile: Value,
        revision: Option<&str>,
    ) -> (SavedProfile, SelectionRecord) {
        let (status, saved) = send(
            client.post(PROFILE),
            json!({
                "profile": profile,
                "base_version": latest(client),
                "queue": [{"source_set": SET, "revision": revision}],
            }),
        );
        assert!(status == 201 || status == 200, "{status}: {saved}");
        let saved: SavedProfile = serde_json::from_value(saved).unwrap();
        let (_, job) = client.follow(saved.jobs[0].id);
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        let record = client.json(&format!("/selections/{}", job.selection.unwrap()));
        (saved, record)
    }

    fn selection(client: &Client, id: i64) -> Selection {
        client.json(&format!("/selections/{id}/selection"))
    }

    fn group<'a>(
        selection: &'a Selection,
        name: &str,
    ) -> &'a title_select_api::selection::SelectionGroup {
        selection.groups.iter().find(|g| g.group == name).unwrap()
    }

    fn winners(selection: &Selection, name: &str) -> Vec<String> {
        group(selection, name)
            .winners
            .iter()
            .map(|w| w.title.clone())
            .collect()
    }

    /// A client that goes away mid-preview stops the engine run: its run folder under the store's `tmp` goes at
    /// once instead of when the run would have finished.
    #[test]
    fn a_preview_whose_client_goes_away_stops_the_engine() {
        use std::io::Write as _;
        let env = Env::new();
        let names: Vec<String> = (0..10_000)
            .flat_map(|i| {
                [
                    format!("Game {i:05} (USA)"),
                    format!("Game {i:05} (Europe)"),
                ]
            })
            .collect();
        let games: Vec<(&str, Vec<RomSpec>)> = names
            .iter()
            .map(|name| {
                let file: &'static str = Box::leak(format!("{name}.a26").into_boxed_str());
                (name.as_str(), vec![rom(file, name.as_bytes())])
            })
            .collect();
        let path = write_dat_version(
            &env.tmp.path().join("in"),
            "Test - System (20260101).dat",
            "20260101",
            &games,
        );
        let service = env.start();
        let client = Client::new(&service);
        let (status, _) = client.upload(
            "Test - System (20260101).dat",
            &std::fs::read(path).unwrap(),
        );
        assert_eq!(status, 201);
        client.add_set();
        let job: Job = client.start_audit().body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        let runs = || -> Vec<PathBuf> {
            std::fs::read_dir(env.store().join("tmp"))
                .into_iter()
                .flatten()
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("selection-")
                })
                .collect()
        };
        let wait_for = |what: &str, done: &dyn Fn() -> bool, within: std::time::Duration| {
            let started = std::time::Instant::now();
            while !done() {
                assert!(started.elapsed() < within, "{what} within {within:?}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };

        let body = json!({"source_set": SET, "profile": {}}).to_string();
        let address = service.url().trim_start_matches("http://").to_owned();
        let mut connection = std::net::TcpStream::connect(address).unwrap();
        write!(
            connection,
            "POST /api/v1/preview HTTP/1.1\r\nhost: test\r\nauthorization: Bearer {TOKEN}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let started = std::time::Instant::now();
        wait_for(
            "the engine to start",
            &|| !runs().is_empty(),
            std::time::Duration::from_secs(60),
        );
        drop(connection);
        wait_for(
            "the engine to stop",
            &|| runs().is_empty(),
            std::time::Duration::from_secs(5),
        );
        let stopped = started.elapsed();

        // Done before the run could have finished: a whole preview of this DAT takes several seconds.
        let (_, whole) = send(
            client.post("/preview"),
            json!({"source_set": SET, "profile": {}}),
        );
        let elapsed = std::time::Duration::from_millis(whole["elapsed_ms"].as_u64().unwrap());
        assert!(
            stopped < elapsed,
            "stopped after {stopped:?}, a whole run takes {elapsed:?}"
        );
    }

    #[test]
    fn a_first_run_previews_a_draft_and_saves_it_only_when_confirmed() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let draft = json!({"region_order": ["USA", "Europe"]});

        let (status, preview) = send(
            client.post("/preview"),
            json!({"source_set": SET, "profile": draft}),
        );
        assert_eq!(status, 200, "{preview}");
        let preview: Preview = serde_json::from_value(preview).unwrap();
        assert_eq!(winners(&preview.selection, "pong"), ["Pong (USA)"]);
        assert_eq!(preview.against, None);
        let profiles: Vec<ProfileRecord> = client.json("/profiles");
        assert!(profiles.is_empty(), "a preview saves nothing");
        assert!(
            client
                .json::<Vec<SelectionRecord>>("/selections?source_set=Test%20-%20System")
                .is_empty()
        );

        let (saved, record) = save(&client, draft.clone());
        assert!(saved.created);
        assert_eq!(saved.version.version, 1);
        assert_eq!(
            (record.profile.name.as_str(), record.profile.version),
            ("Everyday", 1)
        );
        assert_eq!(selection(&client, record.id), preview.selection);
        let rows: Page<SelectionReleaseRow> =
            client.json(&format!("/selections/{}/releases?role=winner", record.id));
        let breakout = rows
            .items
            .iter()
            .find(|r| r.release.group == "breakout")
            .unwrap();
        assert_eq!(
            breakout.availability,
            Some(GameStatus::Missing),
            "a preferred release that is missing stays missing without a choice"
        );

        let (status, again) = send(
            client.post(PROFILE),
            json!({"profile": draft, "base_version": 1}),
        );
        assert_eq!(
            status, 200,
            "a save with nothing changed creates no version"
        );
        assert_eq!(again["created"], json!(false));
        assert_eq!(again["version"]["version"], json!(1));
    }

    #[test]
    fn a_selection_is_explained_joined_to_availability_and_reproduced_by_a_rerun() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let (_, first) = save(
            &client,
            json!({"region_order": ["USA", "Europe"], "exclude": ["demos"]}),
        );
        assert!(!first.system_settings);

        let rows: Page<SelectionReleaseRow> =
            client.json(&format!("/selections/{}/releases", first.id));
        let find = |title: &str| {
            rows.items
                .iter()
                .find(|r| r.release.title == title)
                .unwrap()
        };
        assert_eq!(find("Pong (USA)").release.role, Role::Winner);
        assert_eq!(find("Pong (USA)").availability, Some(GameStatus::Complete));
        assert_eq!(find("Pong (Europe)").release.role, Role::Alternative);
        assert_eq!(
            find("Pong (Europe)").availability,
            Some(GameStatus::Missing)
        );
        let demo = find("Breakout (USA) (Demo)");
        assert_eq!(demo.release.role, Role::Exclusion);
        assert!(
            !demo.release.rule.message.is_empty(),
            "every release names its rule"
        );
        let excluded: Page<SelectionReleaseRow> =
            client.json(&format!("/selections/{}/releases?role=exclusion", first.id));
        assert_eq!(excluded.items.len(), 1);

        let mut response = client
            .post("/selections")
            .send_json(json!({"source_set": SET, "profile": "Everyday", "version": 1}))
            .unwrap();
        assert_eq!(response.status(), 202);
        let job: Job = response.body_mut().read_json().unwrap();
        let (_, job) = client.follow(job.id);
        let second = job.selection.unwrap();
        assert_eq!(selection(&client, second), selection(&client, first.id));
        let diff: SelectionDiff = client.json(&format!("/selections/{second}/diff"));
        assert_eq!((diff.from, diff.changes.len()), (first.id, 0));
    }

    #[test]
    fn a_choice_is_recorded_as_the_rule_and_flagged_as_the_profile_and_dat_change() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let choice = json!({"Test - System": {"pong": ["Pong (Europe)"]}});

        let (_, chosen) = save(
            &client,
            json!({"region_order": ["USA", "Europe"], "choices": choice}),
        );
        let made = selection(&client, chosen.id);
        let pong = group(&made, "pong");
        assert_eq!(winners(&made, "pong"), ["Pong (Europe)"]);
        assert_eq!(pong.winners[0].rule.stage, "choice");
        assert_eq!(pong.winners[0].rule.message, "your choice");
        let choice_state = pong.choice.as_ref().unwrap();
        assert_eq!(choice_state.status, ChoiceStatus::Applied);
        assert_eq!(choice_state.picks, ["Pong (USA)"]);
        assert_eq!(made.summary.choices, 1);

        let (_, now_default) = save(
            &client,
            json!({"region_order": ["Europe", "USA"], "choices": choice}),
        );
        let made = selection(&client, now_default.id);
        assert_eq!(
            group(&made, "pong").choice.as_ref().unwrap().status,
            ChoiceStatus::MatchesDefault
        );

        let (_, conflicting) = save(
            &client,
            json!({
                "region_order": ["USA", "Europe"],
                "exclude": ["demos"],
                "choices": {"Test - System": {"breakout": ["Breakout (USA) (Demo)"]}},
            }),
        );
        let made = selection(&client, conflicting.id);
        let breakout = group(&made, "breakout");
        assert_eq!(
            breakout.choice.as_ref().unwrap().status,
            ChoiceStatus::Conflicting
        );
        assert!(
            breakout.winners.is_empty(),
            "a conflicting choice is not applied"
        );
        assert_eq!(made.summary.conflicting, 1);

        let (_, upload) = client.upload(
            "Test - System (20260201).dat",
            &dat(&env, "20260201", false),
        );
        assert_eq!(upload.dat_revision.version, "20260201");
        let (_, stale) = save_on(
            &client,
            json!({"region_order": ["USA", "Europe"], "choices": choice}),
            Some(&upload.dat_revision.sha256),
        );
        assert_eq!(stale.dat_revision_version, "20260201");
        let made = selection(&client, stale.id);
        let pong = group(&made, "pong");
        assert_eq!(pong.choice.as_ref().unwrap().status, ChoiceStatus::Stale);
        assert_eq!(winners(&made, "pong"), ["Pong (USA)"]);
        assert_eq!(made.summary.stale, 1);
        let availability: title_select_api::Availability =
            client.json(&format!("/selections/{}/availability", stale.id));
        assert_eq!(
            availability.audit, None,
            "no audit against the newer revision"
        );
    }

    #[test]
    fn system_settings_override_the_global_ones_until_removed() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let (_, own) = save(
            &client,
            json!({
                "region_order": ["USA", "Europe"],
                "systems": {"Test - System": {"region_order": ["Europe", "USA"]}},
            }),
        );
        assert!(own.system_settings);
        assert_eq!(
            winners(&selection(&client, own.id), "pong"),
            ["Pong (Europe)"]
        );

        let (saved, global) = save(&client, json!({"region_order": ["USA", "Europe"]}));
        assert_eq!(saved.version.version, 2);
        assert!(!global.system_settings);
        assert_eq!(
            winners(&selection(&client, global.id), "pong"),
            ["Pong (USA)"]
        );
        let diff: SelectionDiff = client.json(&format!("/selections/{}/diff", global.id));
        assert!(
            diff.changes
                .iter()
                .any(|c| c.title == "Pong (USA)" && c.after == Some(Role::Winner))
        );
    }

    #[test]
    fn a_save_from_an_older_version_and_an_unreadable_profile_are_refused() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        save(&client, json!({"region_order": ["USA"]}));
        save(&client, json!({"region_order": ["Europe"]}));

        let (status, body) = send(
            client.post(PROFILE),
            json!({"profile": {"region_order": ["Japan"]}, "base_version": 1}),
        );
        assert_eq!(status, 409, "{body}");
        let (status, body) = send(
            client.post(PROFILE),
            json!({"profile": {"region_order": ["Atlantis"]}, "base_version": 2}),
        );
        assert_eq!(status, 400, "{body}");
        let (status, check) = send(client.post("/check"), json!({"region_order": ["Atlantis"]}));
        assert_eq!(status, 200);
        assert!(
            check["problems"][0].as_str().unwrap().contains("Atlantis"),
            "{check}"
        );
        let vocabulary: title_select_api::Vocabulary = client.json("/vocabulary");
        assert!(vocabulary.regions.iter().any(|r| r == "USA"));
        assert_eq!(
            vocabulary.default_region_order[..3],
            ["USA", "World", "Canada"]
        );
        assert_eq!(vocabulary.default_video_order[0], "NTSC");
    }

    #[test]
    fn without_an_engine_selections_are_unavailable_and_audits_still_run() {
        let env = Env::new();
        let service = title_select_server::spawn(Settings {
            retool: Some(env.tmp.path().join("no-engine")),
            ..env.settings("")
        })
        .unwrap();
        let client = Client::new(&service);
        client.upload("Test - System (20260101).dat", &env.dat());
        client.add_set();

        let (status, body) = send(
            client.post("/preview"),
            json!({"source_set": SET, "profile": {}}),
        );
        assert_eq!(status, 503, "{body}");
        assert_eq!(client.start_audit().status(), 202);
    }

    /// A managed folder in a writable root: a plan of the selection, an approval bound to its digest, and the build
    /// job that writes TorrentZips under the DAT's names.
    #[test]
    fn a_selection_is_planned_approved_by_digest_and_built_into_a_managed_folder() {
        use title_select_api::JobKind;
        use title_select_api::build::{ItemKind, ManagedFolder, Plan};
        let env = Env::new();
        let play = env.tmp.path().join("roots/play");
        std::fs::create_dir_all(&play).unwrap();
        let service = title_select_server::spawn(env.settings(&format!(
            "\n[roots.play]\npath = {:?}\nwritable = true\n",
            play.display().to_string()
        )))
        .unwrap();
        let client = Client::new(&service);
        client.upload("Test - System (20260101).dat", &dat(&env, "20260101", true));
        client.add_set();
        let job: Job = client.start_audit().body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        let (_, selection) = save(&client, json!({"region_order": ["USA", "Europe"]}));
        let (status, body) = send(
            client.post("/managed-folders"),
            json!({"root": "sets", "path": "Play", "source_set": SET, "profile": "Everyday"}),
        );
        assert_eq!(status, 400, "a read-only root is refused: {body}");

        let (status, folder) = send(
            client.post("/managed-folders"),
            json!({"root": "play", "path": "test", "source_set": SET, "profile": "Everyday"}),
        );
        assert_eq!(status, 201, "{folder}");
        let folder: ManagedFolder = serde_json::from_value(folder).unwrap();
        assert!(play.join("test/.title-select").is_file());
        let (status, plan) = send(
            client.post(&format!("/managed-folders/{}/plans", folder.id)),
            json!({"selection": selection.id}),
        );
        assert_eq!(status, 201, "{plan}");
        let plan: Plan = serde_json::from_value(plan).unwrap();
        assert!(plan.approvable(), "{:?}", plan.content.blockers);
        let kind = |game: &str| {
            plan.content
                .items
                .iter()
                .find(|i| i.game == game)
                .map(|i| i.kind)
        };
        assert_eq!(kind("Pong (USA)"), Some(ItemKind::Repack));
        assert_eq!(kind("Combat (USA)"), Some(ItemKind::Repack));
        assert_eq!(kind("Pong (Europe)"), None, "an alternative is not built");
        let (status, _) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": "0".repeat(64)}),
        );
        assert_eq!(status, 409, "an approval of another digest");
        assert!(!play.join("test/Pong (USA).zip").exists());

        let (status, job) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(
            (job.kind, job.managed_folder, job.plan),
            (JobKind::Build, Some(folder.id), Some(plan.id))
        );
        let (_, job) = client.follow(job.id);
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");

        for name in ["Pong (USA).zip", "Combat (USA).zip"] {
            let mut file = std::fs::File::open(play.join("test").join(name)).unwrap();
            title_select_engine::torrentzip::validate(&mut file).unwrap();
        }
        assert!(!play.join("test/.title-select-staging").exists());
        let folder: ManagedFolder = client.json(&format!("/managed-folders/{}", folder.id));
        let build = folder.build.unwrap();
        assert_eq!((folder.generation, build.applied, build.failed), (1, 2, 0));
        let files: Vec<title_select_api::build::BuiltFile> =
            client.json(&format!("/managed-folders/{}/files", folder.id));
        assert_eq!(files.len(), 2);
        let (status, _) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_ne!(status, 202, "a plan is approved once");

        // A check job reads the folder and records what it found; the folder is left as it is.
        std::fs::write(play.join("test/notes.txt"), b"mine").unwrap();
        let planned: title_select_api::build::FolderCheck =
            client.json(&format!("/managed-folders/{}/check", folder.id));
        assert_eq!(planned.summary.job, None, "planning checked the folder");
        let (status, job) = send(
            client.post(&format!("/managed-folders/{}/checks", folder.id)),
            json!(null),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(
            (job.kind, job.managed_folder),
            (JobKind::Check, Some(folder.id))
        );
        let (_, job) = client.follow(job.id);
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        let check: title_select_api::build::FolderCheck =
            client.json(&format!("/managed-folders/{}/check", folder.id));
        assert_eq!(check.summary.job, Some(job.id));
        assert_eq!((check.files.len(), check.drift.len()), (2, 1));
        assert_eq!(std::fs::read(play.join("test/notes.txt")).unwrap(), b"mine");
        let incomplete: title_select_api::build::Incomplete =
            client.json(&format!("/managed-folders/{}/incomplete", folder.id));
        let reasons: Vec<_> = incomplete.items.iter().map(|i| i.reason).collect();
        assert_eq!(
            reasons,
            [title_select_api::build::IncompleteReason::NoSource],
            "drift alone leaves the folder as complete as its build"
        );

        // A cleanup keeps the drift aside once you approve it.
        let (status, body) = send(
            client.post(&format!("/managed-folders/{}/cleanups", folder.id)),
            json!({}),
        );
        assert_eq!(status, 400, "a cleanup that chooses nothing: {body}");
        let (status, plan) = send(
            client.post(&format!("/managed-folders/{}/cleanups", folder.id)),
            json!({"entries": {"notes.txt": "keep_aside"}}),
        );
        assert_eq!(status, 201, "{plan}");
        let plan: Plan = serde_json::from_value(plan).unwrap();
        assert!(plan.approvable(), "{:?}", plan.content.blockers);
        assert!(
            play.join("test/notes.txt").exists(),
            "planning moves nothing"
        );
        let (status, job) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        assert!(!play.join("test/notes.txt").exists());
        let kept: Vec<title_select_api::build::KeptFile> =
            client.json(&format!("/managed-folders/{}/kept-files", folder.id));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "notes.txt");
    }
}
