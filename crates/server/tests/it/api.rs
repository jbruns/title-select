use std::io::BufRead;
use std::path::PathBuf;

use crate::common::*;
use serde::de::DeserializeOwned;
use title_select_api::{
    AuditEvidence, AuditSummary, EntryRow, ErrorBody, Finding, GameRow, GameStatus, ImportOutcome,
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
            clone_lists: None,
            hold_jobs: false,
            fetch_endpoints: None,
            romm_scan_timing: None,
            calibration_limits: None,
            notify_backoff: None,
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

    /// Changes the settings as `body` says, and returns the status and the settings or refusal.
    fn change_settings(&self, body: serde_json::Value) -> (u16, serde_json::Value) {
        let (name, value) = self.auth.clone().unwrap();
        let mut response = agent()
            .patch(format!("{}/settings", self.base))
            .header(name, value)
            .send_json(body)
            .unwrap();
        (
            response.status().as_u16(),
            response.body_mut().read_json().unwrap(),
        )
    }

    /// Has the ingest folder of Root `root` settle after `secs`.
    fn settle_after(&self, root: &str, secs: u64) {
        let (status, body) = self
            .change_settings(serde_json::json!({"ingest_folders": {root: {"settle_secs": secs}}}));
        assert_eq!(status, 200, "{body}");
    }

    fn json<T: DeserializeOwned>(&self, path: &str) -> T {
        let mut response = self.get(path).call().unwrap();
        assert_eq!(response.status(), 200, "GET {path}");
        response.body_mut().read_json().unwrap()
    }

    fn upload(&self, file: &str, bytes: &[u8]) -> (u16, ImportedDatRevision) {
        let (status, body) = self.upload_form("/dat-revisions", &[("source_url", "")], file, bytes);
        (status, serde_json::from_value(body).unwrap())
    }

    /// Posts a multipart form of `fields` and the file `file`, returning the status and the JSON body.
    fn upload_form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        file: &str,
        bytes: &[u8],
    ) -> (u16, serde_json::Value) {
        let boundary = "title-select-test-boundary";
        let mut body = Vec::new();
        for (name, value) in fields {
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
            .post(path)
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
fn the_api_uploads_a_cue_pack_for_a_dat_and_lists_it() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    let pack = env.tmp.path().join("cues.zip");
    write_zip(
        &pack,
        &[("Pong (USA).cue", b"FILE \"Pong (USA).bin\" BINARY\n")],
    );
    let bytes = std::fs::read(&pack).unwrap();

    let (status, body) = client.upload_form(
        "/cue-packs",
        &[("dat", "No-Intro/Test - System"), ("source_url", "")],
        "Test - System (cues).zip",
        &bytes,
    );
    assert_eq!(status, 201, "{body}");
    let imported: title_select_api::CuePackImport = serde_json::from_value(body).unwrap();
    assert_eq!(imported.outcome, ImportOutcome::Added);
    assert_eq!(imported.pack.cues, 1);
    assert_eq!(
        imported.pack.origin,
        title_select_api::CuePackOrigin::Import
    );
    let packs: Vec<title_select_api::CuePackRecord> = client.json("/cue-packs");
    assert_eq!(packs, [imported.pack]);

    let (status, _) = client.upload_form(
        "/cue-packs",
        &[("dat", "No-Intro/Missing")],
        "cues.zip",
        &bytes,
    );
    assert_eq!(status, 400);
}

#[test]
fn the_api_uploads_a_support_file_and_lists_it() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let ini = b";; catver.ini 0.289 / 21-Aug-26 / MAME 0.289 ;;\r\n[Category]\r\npong=Sports / Tennis\r\n";

    let (status, body) = client.upload_form("/support-files", &[], "catver.ini", ini);
    assert_eq!(status, 201, "{body}");
    let imported: title_select_api::support::SupportFileImport =
        serde_json::from_value(body).unwrap();
    assert_eq!(imported.outcome, ImportOutcome::Added);
    assert_eq!(
        imported.file.kind,
        title_select_api::support::SupportFileKind::Catver
    );
    assert_eq!(imported.file.mame_version, "0.289");
    let files: Vec<title_select_api::support::SupportFileRecord> = client.json("/support-files");
    assert_eq!(files, [imported.file]);

    let (status, _) = client.upload_form("/support-files", &[], "catver.ini", ini);
    assert_eq!(status, 200, "the same bytes again");
    let (status, _) = client.upload_form("/support-files", &[], "notes.ini", b"[Category]\r\n");
    assert_eq!(status, 400);
}

#[test]
fn an_uploaded_dat_of_no_known_provider_is_kept_by_its_format_and_name_and_a_provider_field_is_refused()
 {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let dat = br#"<?xml version="1.0"?>
<datafile><header><name>Homebrew</name><version>1</version></header>
<game name="Pong"><rom name="Pong.a26" size="4" crc="00000000"/></game></datafile>"#;

    let (status, uploaded) = client.upload("homebrew.dat", dat);
    assert_eq!(status, 201);
    assert_eq!(uploaded.dat_revision.dat, "unknown-logiqx/Homebrew");
    assert_eq!(uploaded.dat_revision.provider, None);
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert_eq!(
        (dats[0].dat.as_str(), dats[0].provider.as_deref()),
        ("unknown-logiqx/Homebrew", None)
    );

    let (status, body) = client.upload_form(
        "/dat-revisions",
        &[("provider", "Mine")],
        "homebrew.dat",
        dat,
    );
    assert_eq!(status, 400, "{body}");
}

#[test]
fn the_api_downloads_a_dat_revisions_kept_file_byte_for_byte_under_its_recorded_name() {
    use sha2::Digest;

    let env = Env::new();
    let dat = env.dat();
    let homebrew = br#"<?xml version="1.0"?>
<datafile><header><name>Homebrew</name><version>1</version></header>
<game name="Pong"><rom name="Pong.a26" size="4" crc="00000000"/></game></datafile>"#;
    // The upload form can't carry these names, so they are recorded through the store before the Service starts.
    let (hostile, nameless) = {
        let store = title_select_store::Store::open(&env.store()).unwrap();
        let hostile = store
            .import_dat_bytes(&dat, "..\\up/\"evil\";\r\nname é.dat", None)
            .unwrap()
            .0;
        let nameless = store.import_dat_bytes(homebrew, "../", None).unwrap().0;
        (hostile, nameless)
    };
    let service = env.start();
    let client = Client::new(&service);

    let file = |sha: &str| format!("/dat-revisions/{sha}/file");
    let mut response = client.get(&file(&hostile.sha256)).call().unwrap();
    assert_eq!(response.status(), 200);
    let header = |response: &ureq::http::Response<ureq::Body>, name: &str| {
        response.headers()[name].to_str().unwrap().to_owned()
    };
    assert_eq!(
        header(&response, "content-disposition"),
        "attachment; filename=\"_evil_;name _.dat\"; filename*=UTF-8''%22evil%22%3Bname%20%C3%A9.dat"
    );
    let bytes = response.body_mut().read_to_vec().unwrap();
    assert_eq!(bytes, dat);
    assert_eq!(
        sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        hostile.sha256
    );

    let response = client.get(&file(&nameless.sha256)).call().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        header(&response, "content-disposition"),
        "attachment; filename=\"Homebrew.dat\"; filename*=UTF-8''Homebrew.dat",
        "a recorded name with nothing left once sanitised falls back to the DAT's"
    );

    // An older revision is not the newest, and nothing holds it, so its file is dropped as it is imported.
    let older = write_dat_version(
        &env.tmp.path().join("in"),
        "older.dat",
        "20250101-000000",
        &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
    );
    let (status, older) = client.upload("older.dat", &std::fs::read(older).unwrap());
    assert_eq!(status, 201);
    let revisions: Vec<title_select_api::DatRevisionRecord> = client.json("/dat-revisions");
    let kept = |sha: &str| {
        revisions
            .iter()
            .find(|r| r.sha256 == sha)
            .unwrap()
            .blob_present
    };
    assert!(!kept(&older.dat_revision.sha256));
    let mut response = client
        .get(&file(&older.dat_revision.sha256))
        .call()
        .unwrap();
    assert_eq!(response.status(), 410);
    let body: ErrorBody = response.body_mut().read_json().unwrap();
    assert!(body.error.contains("isn't kept"), "{}", body.error);

    let unknown = "0".repeat(64);
    assert_eq!(status_of(client.get(&file(&unknown)).call()), 404);
    for bad in [
        hostile.sha256.to_uppercase(),
        hostile.sha256[..63].to_owned(),
        "%2E%2E%2Ftoken".to_owned(),
        format!("{}g", &hostile.sha256[..63]),
    ] {
        assert_eq!(status_of(client.get(&file(&bad)).call()), 400, "{bad}");
    }

    let missing = Client {
        auth: None,
        ..Client::new(&service)
    };
    assert_eq!(status_of(missing.get(&file(&hostile.sha256)).call()), 401);
}

#[test]
fn the_api_refuses_to_delete_a_dat_revision_in_use_saying_why_and_deletes_it_once_nothing_is() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let (_, uploaded) = client.upload("Test - System (20260101).dat", &env.dat());
    let sha = uploaded.dat_revision.sha256;
    client.add_set();
    let auth = client.auth.clone().unwrap();
    let delete = || {
        agent()
            .delete(format!("{}/dat-revisions/{sha}", client.base))
            .header(&auth.0, &auth.1)
            .call()
            .unwrap()
    };

    let mut response = delete();
    assert_eq!(response.status(), 409);
    let refused: ErrorBody = response.body_mut().read_json().unwrap();
    let blockers = refused.blockers.unwrap();
    assert_eq!(
        (blockers[0].kind, blockers[0].names.clone()),
        (
            title_select_api::DeletionBlockerKind::SourceSet,
            vec![SET.to_string()]
        )
    );
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert_eq!(dats[0].revisions[0].blockers.as_ref(), Some(&blockers));

    let response = client
        .post(&format!(
            "/source-sets/{}/unregistration",
            "Test%20-%20System"
        ))
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut response = delete();
    assert_eq!(response.status(), 200);
    let deleted: title_select_api::DeletedDatRevision = response.body_mut().read_json().unwrap();
    assert!(deleted.dat_removed);
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert!(dats.is_empty());
    assert_eq!(delete().status(), 404);
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
    assert_eq!(
        uploaded.dat_revision.provider.as_deref(),
        Some("No-Intro"),
        "read from the DAT's header"
    );
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert_eq!(dats.len(), 1);
    assert_eq!(dats[0].dat, "No-Intro/Test - System");
    assert_eq!(
        (dats[0].format, dats[0].provider.as_deref()),
        (title_select_api::DatFormat::Logiqx, Some("No-Intro"))
    );
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
    assert_eq!(history, vec![summary.clone()]);
    let summaries: Vec<title_select_api::SourceSetSummary> = client.json("/source-set-summaries");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].set.name, set.name);
    assert_eq!(summaries[0].latest.as_ref(), Some(&summary));
    assert_eq!(summaries[0].repair, None);

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
            defaults.library_folder.as_str()
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
fn the_api_exports_a_fixdat_and_a_have_miss_csv_of_what_the_latest_audit_found_missing() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let base = "/source-sets/Test%20-%20System";
    client.upload("Test - System (20260101).dat", &env.dat());
    client.add_set();
    assert_eq!(status_of(client.get(&format!("{base}/fixdat")).call()), 400);
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    let (_, done) = client.follow(job.id);
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");

    let response = client.get(&format!("{base}/fixdat")).call().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Content-Type"], "application/xml");
    let disposition = response.headers()["Content-Disposition"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        disposition.starts_with("attachment; filename=\"fixdat_Test - System ("),
        "{disposition}"
    );
    let xml = response.into_body().read_to_string().unwrap();
    let fixdat =
        title_select_engine::dat::Dat::parse(xml.as_bytes(), std::path::Path::new("x")).unwrap();
    assert_eq!(fixdat.games, ["Breakout (USA)"]);

    let response = client.get(&format!("{base}/have-miss.csv")).call().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["Content-Type"],
        "text/csv; charset=utf-8"
    );
    let csv = response.into_body().read_to_string().unwrap();
    assert_eq!(
        csv.lines().collect::<Vec<_>>(),
        [
            "game,status,present_roms,missing_roms,available_elsewhere_roms",
            "Pong (USA),complete,Pong (USA).a26,,",
            "Combat (USA),complete,Combat (USA).a26,,",
            "Breakout (USA),missing,,Breakout (USA).a26,",
        ]
    );

    assert_eq!(
        status_of(client.get(&format!("{base}/fixdat?selection=999")).call()),
        404
    );
    assert_eq!(
        status_of(
            client
                .get(&format!("{base}/have-miss.csv?selection=x"))
                .call()
        ),
        400
    );
}

#[test]
fn the_api_exports_a_fixdat_of_the_coverage_by_default_and_of_the_whole_dat_when_asked() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let base = "/source-sets/Test%20-%20System";
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    let fixdat = |query: &str| -> (u16, String) {
        let mut response = client.get(&format!("{base}/fixdat{query}")).call().unwrap();
        (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap(),
        )
    };
    assert_eq!(
        status_of(client.get(&format!("{base}/fixdat?scope=coverage")).call()),
        400,
        "no coverage to limit it to"
    );
    let (name, value) = client.auth.clone().unwrap();
    let response = agent()
        .put(format!("{}{base}/coverage", client.base))
        .header(name, value)
        .send_json(serde_json::json!({ "coverage": ["Japan"] }))
        .unwrap();
    assert_eq!(response.status(), 200);

    let (status, covered) = fixdat("");
    assert_eq!(status, 200);
    assert!(!covered.contains("Breakout"), "{covered}");
    assert!(
        covered.contains("limited to the games in its coverage: Japan"),
        "{covered}"
    );
    assert_eq!(fixdat("?scope=coverage").1, covered);
    let (status, whole) = fixdat("?scope=dat");
    assert_eq!(status, 200);
    assert!(whole.contains("Breakout (USA)"), "{whole}");
    assert!(!whole.contains("coverage"), "{whole}");
    for query in ["?scope=coverage&selection=1", "?scope=region"] {
        assert_eq!(
            status_of(client.get(&format!("{base}/fixdat{query}")).call()),
            400,
            "{query}"
        );
    }
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
        store.import_dat(&dat, None).unwrap();
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

    let service = env.start();
    let client = Client::new(&service);

    let jobs: Vec<Job> = client.json("/jobs");
    let (again, first) = (&jobs[0], &jobs[1]);
    assert_eq!(first.state, JobState::Interrupted);
    assert_eq!((again.attempt, again.requeued_from), (2, Some(first.id)));
    // It records the ceilings at its start, not the threads it started, which the shared pools decide as it runs.
    let cpus = std::thread::available_parallelism().unwrap().get();
    assert_eq!(again.workers.readers, cpus);
    let (_, done) = client.follow(again.id);
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn every_watching_client_hears_that_the_settings_changed_without_their_secrets() {
    let env = Env::new();
    let service = title_select_server::spawn(env.settings("")).unwrap();
    let client = Client::new(&service);
    let (mut events, _) = client.watch();

    let (status, _) = client.change_settings(serde_json::json!({
        "pace": "unthrottled",
        "notify": {
            "url": {"set": "https://hooks.example/very-secret-path"},
            "secret": {"set": "s3cret-value"},
        },
    }));

    assert_eq!(status, 200);
    let (name, data) = events.next_event();
    assert_eq!(name, "settings");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap(),
        serde_json::json!({"pace": "unthrottled"})
    );
    assert!(!data.contains("very-secret-path") && !data.contains("s3cret-value"));
}

/// A stub RomM 5.4.1 on a local port: a heartbeat for anyone, and to the bearer of `rmm_good` its user `svc`, every
/// scope, and a platform gba with no ROMs. No test sends a request to a real RomM.
fn stub_romm() -> String {
    stub_romm_with("finished")
}

/// Each request each stub RomM saw, as `METHOD /path`, by its address.
static ROMM_SEEN: std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// `stub_romm`, whose scan task always answers `status`, and which makes the collection 7 when asked for one.
fn stub_romm_with(status: &'static str) -> String {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = format!(r#"{{"task_id": "t1", "status": "{status}"}}"#);
    let at = url.clone();
    std::thread::spawn(move || {
        let url = at;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let (mut line, mut authorized, mut length) = (String::new(), false, 0usize);
            reader.read_line(&mut line).unwrap();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                authorized |= lower.trim() == "authorization: bearer rmm_good";
                if let Some(n) = lower.strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
            }
            std::io::Read::read_exact(&mut reader, &mut vec![0; length]).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("");
            let method = line.split_whitespace().next().unwrap_or("");
            ROMM_SEEN
                .lock()
                .unwrap()
                .entry(url.clone())
                .or_default()
                .push(format!("{method} {path}"));
            let collection = r#"{"id": 7, "name": "Additions", "rom_ids": [], "is_public": true}"#;
            let (status, body) = match path {
                "/api/heartbeat" => (200, r#"{"SYSTEM": {"VERSION": "5.4.1"}}"#),
                _ if !authorized => (401, r#"{"detail": "Not authenticated"}"#),
                "/api/users/me" => (200, r#"{"username": "svc", "role": "editor"}"#),
                "/api/platforms" => (
                    200,
                    r#"[{"id": 3, "slug": "gba", "fs_slug": "gba", "name": "GBA"}]"#,
                ),
                p if p.starts_with("/api/roms") => (200, r#"{"items": []}"#),
                p if p.ends_with("/identifiers") => (200, "[]"),
                "/api/tasks/scan" => (202, r#"{"task_id": "t1", "status": "queued"}"#),
                "/api/tasks/t1" => (200, task.as_str()),
                "/api/collections?is_public=true" | "/api/collections/7" => (200, collection),
                _ => (404, r#"{"detail": "Not found"}"#),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn romm_is_off_until_you_turn_it_on_and_its_token_is_tested_but_never_shown() {
    use title_select_api::romm::{RommCheck, RommUser};
    use title_select_api::settings::SettingsView;
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let romm = stub_romm();
    let settings: SettingsView = client.json("/settings");
    assert!(!settings.romm.enabled && !settings.romm.token_set);
    let test = |body: serde_json::Value| {
        let mut response = client.post("/romm/test").send_json(body).unwrap();
        let status = response.status().as_u16();
        (
            status,
            response
                .body_mut()
                .read_json::<serde_json::Value>()
                .unwrap(),
        )
    };
    let (status, check) = test(serde_json::json!({"base_url": romm, "token": {"set": "rmm_good"}}));
    assert_eq!(
        status, 200,
        "a connection is tested before it is saved: {check}"
    );
    assert_eq!(check["failure"], serde_json::Value::Null, "{check}");
    let (mut events, _) = client.watch();

    let (status, body) = client.change_settings(serde_json::json!({"romm": {
        "enabled": true, "base_url": romm, "token": {"set": "rmm_good"}, "root": "sets",
    }}));

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["romm"]["token_set"], true);
    assert!(!body.to_string().contains("rmm_good"));
    let (name, data) = events.next_event();
    assert_eq!(name, "settings");
    assert!(!data.contains("rmm_good"));
    let settings: SettingsView = client.json("/settings");
    assert!(settings.romm.enabled && settings.romm.token_set);
    assert!(
        !serde_json::to_string(&settings)
            .unwrap()
            .contains("rmm_good")
    );
    let (status, check) = test(serde_json::json!({}));
    assert_eq!(status, 200, "{check}");
    let check: RommCheck = serde_json::from_value(check).unwrap();
    assert_eq!(
        check,
        RommCheck {
            version: Some("5.4.1".into()),
            version_tested: true,
            user: Some(RommUser {
                username: "svc".into(),
                admin: false
            }),
            missing_scopes: vec![],
            failure: None,
        }
    );
    let (status, refused) = test(serde_json::json!({"base_url": "http://192.0.2.1:8080"}));
    assert_eq!(
        status, 400,
        "the saved token goes only to the saved address: {refused}"
    );
    let (status, check) = test(serde_json::json!({"token": {"set": "rmm_wrong"}}));
    assert_eq!(status, 200);
    assert!(
        check["failure"]
            .as_str()
            .unwrap()
            .contains("refused the token"),
        "{check}"
    );
}

#[test]
fn a_pace_you_choose_applies_without_a_restart_as_a_fraction_of_the_shared_ceilings() {
    use title_select_api::settings::{CeilingSource, Pace, SettingsView};
    let env = Env::new();
    let service = title_select_server::spawn(
        env.settings("[jobs]\nconcurrent = 2\nreaders = 8\nhashers = 5\n"),
    )
    .unwrap();
    let client = Client::new(&service);
    let settings: SettingsView = client.json("/settings");
    let host = &settings.jobs.host;
    assert_eq!(settings.jobs.pace, Pace::Default);
    assert_eq!(
        (host.concurrent.ceiling, host.concurrent.in_force),
        (Some(2), Some(1))
    );
    assert_eq!(
        (host.hashers.ceiling, host.hashers.in_force),
        (Some(5), Some(2))
    );
    assert_eq!(host.hashers.source, Some(CeilingSource::Config));
    assert_eq!(host.hashers.locked_by.as_deref(), Some("[jobs] hashers"));
    let sets = settings
        .jobs
        .roots
        .iter()
        .find(|r| r.root == "sets")
        .unwrap();
    assert_eq!(
        (sets.readers.ceiling, sets.readers.in_force),
        (Some(8), Some(4))
    );

    let (status, body) = client.change_settings(serde_json::json!({"pace": "unthrottled"}));
    assert_eq!(status, 200, "{body}");
    let settings: SettingsView = client.json("/settings");
    assert_eq!(settings.jobs.host.hashers.in_force, Some(5));
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(
        (job.workers.readers, job.workers.hashers),
        (8, 5),
        "a job records the ceilings it started with"
    );
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);

    let (status, _) = client.change_settings(
        serde_json::json!({"retention": {"audits_keep": 0, "selections_keep": 1}}),
    );
    assert_eq!(status, 400);
    let (status, _) = client.change_settings(serde_json::json!({"pace": "fast"}));
    assert_eq!(status, 422);
}

#[test]
fn a_ceiling_you_set_applies_at_once_and_one_the_file_locks_or_out_of_range_is_refused() {
    use title_select_api::settings::{CeilingSource, SettingsView};
    let env = Env::new();
    let service = title_select_server::spawn(env.settings("[jobs]\nhashers = 4\n")).unwrap();
    let client = Client::new(&service);

    let (status, body) =
        client.change_settings(serde_json::json!({"ceilings": {"host": {"hashers": 6}}}));
    assert_eq!(status, 400, "{body}");
    let why = body["error"].as_str().unwrap();
    assert!(
        why.contains("configuration file sets the hash workers ([jobs] hashers)"),
        "{why}"
    );
    for bad in [
        serde_json::json!(0),
        serde_json::json!(-2),
        serde_json::json!(2.5),
        serde_json::json!("lots"),
    ] {
        let (status, body) = client.change_settings(
            serde_json::json!({"ceilings": {"roots": {"sets": {"readers": bad}}}}),
        );
        assert_eq!(status, 400, "{bad}: {body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("the readers on sets to a whole number"),
            "{body}"
        );
    }

    let (status, body) = client.change_settings(serde_json::json!({"ceilings": {
        "host": {"concurrent": 3, "archive_workers": 6},
        "roots": {"sets": {"readers": 6, "write_mib_per_s": 120}}
    }}));
    assert_eq!(status, 200, "{body}");
    let settings: SettingsView = serde_json::from_value(body).unwrap();
    let host = &settings.jobs.host;
    assert_eq!(host.archive_workers.source, Some(CeilingSource::Yours));
    assert_eq!(
        (host.archive_workers.ceiling, host.archive_workers.in_force),
        (Some(6), Some(3))
    );
    let sets = settings
        .jobs
        .roots
        .iter()
        .find(|r| r.root == "sets")
        .unwrap();
    assert_eq!(
        (sets.readers.ceiling, sets.readers.in_force),
        (Some(6), Some(3))
    );
    assert_eq!(sets.write_mib_per_s.in_force, Some(60));
    assert_eq!(settings, client.json::<SettingsView>("/settings"));
}

#[test]
fn the_api_lists_every_failed_job() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let ok: Job = client.start_audit().body_mut().read_json().unwrap();
    client.follow(ok.id);
    std::fs::remove_dir_all(env.tmp.path().join("roots/sets").join(SET)).unwrap();
    let broken: Job = client.start_audit().body_mut().read_json().unwrap();
    let (_, broken) = client.follow(broken.id);
    assert_eq!(broken.state, JobState::Failed);

    let failed: Vec<Job> = client.json("/failed-jobs");

    assert_eq!(failed, vec![broken]);
}

/// A Job's throughput is recorded in the write that ends it, so its final state event carries it, and every read
/// after agrees.
#[test]
fn a_jobs_final_state_event_carries_its_throughput_as_every_later_read_does() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let (mut events, _) = client.watch();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();

    let (_, followed) = client.follow(job.id);
    let (_, published) = events.follow(job.id);

    assert_eq!(followed.state, JobState::Succeeded);
    let throughput = followed.throughput.expect("its throughput");
    assert!(throughput.bytes_read > 0, "{throughput:?}");
    assert_eq!(published, followed);
    let read: Job = client.json(&format!("/jobs/{}", job.id));
    assert_eq!(read, followed);
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
        "[roots.drop]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
        drop.display().to_string()
    )))
    .unwrap();
    let client = Client::new(&service);
    client.settle_after("drop", 0);
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
        Some(Proposal::Addition { .. })
    ));

    let mut response = client
        .post("/ingest-folders/drop/dismissals")
        .send_json(serde_json::json!({"items": [breakout.id]}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let dismissed: Vec<IngestItem> = response.body_mut().read_json().unwrap();
    assert_eq!(dismissed.len(), 1);
    assert_eq!(dismissed[0].state, ItemState::Dismissed);
    assert_eq!(
        status_of(
            client
                .post("/ingest-folders/drop/dismissals")
                .send_json(serde_json::json!({"items": []}))
        ),
        400
    );

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
    let moved = item.last_move.expect("its move is listed");
    assert_eq!((moved.plan, moved.under_way), (Some(plan.id), false));
    assert_eq!(
        status_of(client.get("/ingest-folders/sets/items").call()),
        404
    );
}

/// Starts a Service with the ingest root `drop` settling after `settle_secs`, registered, and returns its folder.
fn ingest_service(env: &Env, settle_secs: u64) -> (Handle, std::path::PathBuf) {
    let drop = env.tmp.path().join("drop");
    std::fs::create_dir_all(&drop).unwrap();
    let service = title_select_server::spawn(env.settings(&format!(
        "[roots.drop]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
        drop.display().to_string()
    )))
    .unwrap();
    Client::new(&service).settle_after("drop", settle_secs);
    let response = Client::new(&service)
        .post("/ingest-folders")
        .send_json(serde_json::json!({"root": "drop"}))
        .unwrap();
    assert_eq!(response.status(), 201);
    (service, drop)
}

/// When the folder `drop` is next checked for its arriving items, once `ready` says it is the follow-up expected. A
/// scan's Job ends before the Service sets the follow-up from it, so the test waits for that.
fn next_check_once(
    client: &Client,
    ready: impl Fn(Option<jiff::Timestamp>) -> bool,
) -> Option<jiff::Timestamp> {
    use title_select_api::ingest::IngestFolder;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let folder: IngestFolder = client.json("/ingest-folders/drop");
        let next = folder.next_check_at.map(|at| at.parse().unwrap());
        if ready(next) {
            return next;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "next check stayed {next:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Scans `drop` by hand, and returns the scan as it ended.
fn scan_drop(client: &Client) -> Job {
    let mut response = client
        .post("/ingest-folders/drop/scans")
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 202);
    let job: Job = response.body_mut().read_json().unwrap();
    client.follow(job.id).1
}

/// A scan that leaves an item arriving is followed by a scan of that folder once it could have settled, which
/// assesses it with no scan started by hand. Only the outcome is awaited, so a slow machine only makes it wait.
#[test]
fn the_service_scans_an_ingest_folder_again_once_arriving_items_could_have_settled() {
    use title_select_api::ingest::{IngestFolder, IngestItem, ItemState};
    let env = Env::new();
    let (service, drop) = ingest_service(&env, 1);
    let client = Client::new(&service);
    write_file(&drop.join("mystery.bin"), b"nobody knows");
    assert_eq!(scan_drop(&client).state, JobState::Succeeded);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let items: Vec<IngestItem> = client.json("/ingest-folders/drop/items");
        let folder: IngestFolder = client.json("/ingest-folders/drop");
        if items[0].state == ItemState::Assessed && folder.next_check_at.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never assessed: {items:?} {folder:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let jobs: Vec<Job> = client.json("/jobs");
    assert!(
        jobs.iter()
            .any(|j| j.ingest_folder.as_deref() == Some("drop")
                && j.actor == "follow-up"
                && j.state == JobState::Succeeded),
        "{jobs:?}"
    );
}

/// The folder says when it is next checked: once its arriving item could have settled, later when the item changes
/// again, and, while the folder can't be scanned, after a wait that doubles. The settle interval is far longer than
/// the test, so no follow-up runs during it.
#[test]
fn an_ingest_folder_says_when_its_arriving_items_are_next_checked_and_retries_a_failed_scan() {
    use title_select_api::ingest::{IngestFolder, IngestItem};
    let env = Env::new();
    let (service, drop) = ingest_service(&env, 600);
    let client = Client::new(&service);
    let minutes = |m: i64| jiff::SignedDuration::from_mins(m);
    let folder: IngestFolder = client.json("/ingest-folders/drop");
    assert_eq!(folder.next_check_at, None, "nothing has arrived");

    write_file(&drop.join("mystery.bin"), b"nobody");
    assert_eq!(scan_drop(&client).state, JobState::Succeeded);
    let items: Vec<IngestItem> = client.json("/ingest-folders/drop/items");
    let seen: jiff::Timestamp = items[0].first_seen_at.parse().unwrap();
    next_check_once(&client, |next| next == Some(seen + minutes(10)));

    write_file(&drop.join("mystery.bin"), b"nobody knows");
    let before = jiff::Timestamp::now();
    assert_eq!(scan_drop(&client).state, JobState::Succeeded);
    let changed = next_check_once(&client, |next| {
        next.is_some_and(|at| at >= before + minutes(10))
    })
    .unwrap();

    // While the folder can't be read, each failed scan is retried: the settle interval after the first, twice that
    // after the next, up to 15 minutes. Each retry is due later than any follow-up set before its scan started.
    let away = env.tmp.path().join("away");
    std::fs::rename(&drop, &away).unwrap();
    for wait in [10, 15] {
        let before = jiff::Timestamp::now();
        assert_eq!(scan_drop(&client).state, JobState::Failed);
        let retry = next_check_once(&client, |next| {
            next.is_some_and(|at| at >= before + minutes(wait))
        })
        .unwrap();
        let after = jiff::Timestamp::now();
        assert!(
            retry <= after + minutes(wait),
            "retry {retry} after {wait} minutes"
        );
    }

    std::fs::rename(&away, &drop).unwrap();
    assert_eq!(scan_drop(&client).state, JobState::Succeeded);
    next_check_once(&client, |next| next == Some(changed));
}

/// Unregistering a folder drops its follow-up, so it is neither scanned again nor said to be.
#[test]
fn an_unregistered_ingest_folder_is_not_checked_again() {
    let env = Env::new();
    let (service, drop) = ingest_service(&env, 600);
    let client = Client::new(&service);
    write_file(&drop.join("mystery.bin"), b"nobody knows");
    assert_eq!(scan_drop(&client).state, JobState::Succeeded);
    next_check_once(&client, |next| next.is_some());

    let response = client
        .post("/ingest-folders/drop/unregistration")
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(next_check_once(&client, |_| true), None);
}

#[test]
fn the_api_adds_an_ingest_item_as_an_addition_lists_edits_checks_and_retires_it() {
    use title_select_api::JobKind;
    use title_select_api::additions::{Addition, AdditionState, AdditionsCheck, AdditionsSet};
    use title_select_api::build::PlanKeptDeletion;
    use title_select_api::ingest::{IngestItem, IngestPlan, Proposal};
    use title_select_api::repair::{SourceKeptDeletion, SourceKeptFile};
    let env = Env::new();
    let drop = env.tmp.path().join("drop");
    let additions = env.tmp.path().join("additions");
    std::fs::create_dir_all(&drop).unwrap();
    std::fs::create_dir_all(&additions).unwrap();
    let service = title_select_server::spawn(env.settings(&format!(
        "[roots.drop]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\n\
         [roots.additions]\npath = {:?}\nwritable = true\n",
        drop.display().to_string(),
        additions.display().to_string(),
    )))
    .unwrap();
    let client = Client::new(&service);
    client.settle_after("drop", 0);
    let (status, _) = client.upload("Test - System (20260101).dat", &env.dat());
    assert_eq!(status, 201);
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    let response = client
        .post("/ingest-folders")
        .send_json(serde_json::json!({"root": "drop"}))
        .unwrap();
    assert_eq!(response.status(), 201);
    write_file(&drop.join("My Hack.a26"), b"hack");
    for _ in 0..2 {
        let mut response = client
            .post("/ingest-folders/drop/scans")
            .send_empty()
            .unwrap();
        let job: Job = response.body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    }
    let items: Vec<IngestItem> = client.json("/ingest-folders/drop/items");
    let item = items.iter().find(|i| i.name == "My Hack.a26").unwrap();
    let system = "No-Intro/Test - System";
    assert_eq!(
        item.assessment.proposal,
        Some(Proposal::Addition {
            system: Some(system.into())
        })
    );

    let mut response = client
        .post("/ingest-folders/drop/plans")
        .send_json(serde_json::json!({
            "action": "addition",
            "items": [item.id],
            "additions": {item.id.to_string(): {"system": system, "name": "My Hack"}},
        }))
        .unwrap();
    assert_eq!(response.status(), 201);
    let plan: IngestPlan = response.body_mut().read_json().unwrap();
    assert!(
        client
            .json::<Vec<AdditionsSet>>("/additions-sets")
            .is_empty(),
        "planning adds nothing"
    );
    let mut response = client
        .post(&format!("/ingest-plans/{}/approval", plan.id))
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let approved: IngestPlan = response.body_mut().read_json().unwrap();
    assert!(
        approved.finished_at.is_none(),
        "an additions job publishes it"
    );
    // Waits for the latest Additions Job to end.
    let settle = || {
        let jobs: Vec<Job> = client.json("/jobs");
        let job = jobs
            .iter()
            .filter(|j| j.kind == JobKind::Additions)
            .max_by_key(|j| j.id)
            .expect("an additions job is queued");
        assert_eq!(job.additions_set.as_deref(), Some("No-Intro/Test - System"));
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    };
    settle();
    let done: IngestPlan = client.json(&format!("/ingest-plans/{}", plan.id));
    assert!(done.outcomes.iter().all(|o| o.moved), "{done:?}");
    let moved: IngestItem = client.json(&format!("/ingest-items/{}", item.id));
    let moved = moved.last_move.expect("its move is listed");
    let jobs: Vec<Job> = client.json("/jobs");
    let job = jobs
        .iter()
        .filter(|j| j.kind == JobKind::Additions)
        .max_by_key(|j| j.id);
    assert_eq!(
        moved.job,
        job.map(|j| j.id),
        "it links to its additions job"
    );
    assert!(!moved.under_way);
    let ingests: Vec<IngestPlan> = client.json("/source-sets/Test%20-%20System/ingests");
    assert_eq!(
        ingests.iter().map(|p| p.id).collect::<Vec<_>>(),
        [plan.id],
        "an addition to its DAT's additions set is in the source set's ingest history"
    );
    assert!(!drop.join("My Hack.a26").exists());
    assert!(additions.join(SET).join("My Hack.zip").exists());

    let sets: Vec<AdditionsSet> = client.json("/additions-sets");
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].system, system);
    let query = "system=No-Intro%2FTest%20-%20System";
    let set: AdditionsSet = client.json(&format!("/additions-set?{query}"));
    let added = &set.additions[0];
    assert_eq!(added.state, AdditionState::Active);
    assert!(added.rom.as_ref().is_some_and(|r| r.sha1.is_some()));

    let path = format!("{}/additions/{}", client.base, added.id);
    let patch = |body: serde_json::Value| {
        let (name, value) = client.auth.clone().unwrap();
        agent().patch(&path).header(name, value).send_json(body)
    };
    let mut response =
        patch(serde_json::json!({"name": "Renamed", "parent": "Pong (USA)"})).unwrap();
    assert_eq!(response.status(), 200);
    let edited: Addition = response.body_mut().read_json().unwrap();
    assert_eq!(
        (edited.name.as_str(), edited.parent.as_deref()),
        ("Renamed", Some("Pong (USA)"))
    );
    let mut response = patch(serde_json::json!({"parent": null})).unwrap();
    let cleared: Addition = response.body_mut().read_json().unwrap();
    assert_eq!((cleared.name.as_str(), cleared.parent), ("Renamed", None));
    assert_eq!(status_of(patch(serde_json::json!({"name": "a/b"}))), 400);

    let mut response = client
        .post(&format!("/additions-set/checks?{query}"))
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    let check: AdditionsCheck = response.body_mut().read_json().unwrap();
    assert!(!check.drifted(), "{check:?}");

    let mut response = client
        .post(&format!("/additions/{}/retirement", added.id))
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    let retiring: Addition = response.body_mut().read_json().unwrap();
    assert_eq!(retiring.state, AdditionState::Retiring);
    settle();
    let set: AdditionsSet = client.json(&format!("/additions-set?{query}"));
    assert_eq!(set.additions[0].state, AdditionState::Retired);
    assert!(!additions.join(SET).join("My Hack.zip").exists());
    let kept: Vec<SourceKeptFile> = client.json(&format!("/additions-set/kept-files?{query}"));
    assert_eq!(kept.len(), 1);
    assert_eq!(set.kept, kept);

    let response = client
        .post(&format!("/additions/{}/restoration", added.id))
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    settle();
    let set: AdditionsSet = client.json(&format!("/additions-set?{query}"));
    assert_eq!(set.additions[0].state, AdditionState::Active);
    assert!(additions.join(SET).join("My Hack.zip").exists());

    client
        .post(&format!("/additions/{}/retirement", added.id))
        .send_empty()
        .unwrap();
    settle();
    let kept: Vec<SourceKeptFile> = client.json(&format!("/additions-set/kept-files?{query}"));
    let mut response = client
        .post(&format!("/additions-set/kept-deletions?{query}"))
        .send_json(PlanKeptDeletion {
            ids: vec![kept[0].id],
            all: false,
        })
        .unwrap();
    assert_eq!(response.status(), 201);
    let deletion: SourceKeptDeletion = response.body_mut().read_json().unwrap();
    let mut response = client
        .post(&format!(
            "/additions-kept-deletions/{}/approval",
            deletion.id
        ))
        .send_json(serde_json::json!({"digest": deletion.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let deleted: SourceKeptDeletion = response.body_mut().read_json().unwrap();
    assert!(deleted.outcomes.iter().all(|o| o.deleted), "{deleted:?}");
    assert!(
        client
            .json::<Vec<SourceKeptFile>>(&format!("/additions-set/kept-files?{query}"))
            .is_empty()
    );
    assert_eq!(
        status_of(client.get("/additions-set?system=No-Intro%2FNope").call()),
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
    let summaries: Vec<title_select_api::SourceSetSummary> = client.json("/source-set-summaries");
    assert_eq!(
        summaries[0].repair.as_ref().map(|r| (r.id, r.approved)),
        Some((plan.id, false)),
        "the list shows the repair open"
    );

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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

#[test]
fn the_api_sets_a_source_set_aside_unregisters_it_and_forgets_it_once_its_kept_files_are_deleted() {
    use title_select_api::lifecycle::{Forgotten, Holds, UnregisteredRecord};
    use title_select_api::repair::{RepairPlan, SourceKeptDeletion, SourceKeptFile};
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    let service =
        title_select_server::spawn(env.settings(&format!("writable_paths = [{SET:?}]\n"))).unwrap();
    let client = Client::new(&service);
    let auth = client.auth.clone().unwrap();
    client.upload("Test - System (20260101).dat", &env.dat());
    client.add_set();
    let job: Job = client
        .post("/audits")
        .send_json(serde_json::json!({"source_set": SET, "evidence": "full"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    let holds: Holds = client.json("/source-sets/Test%20-%20System/holds");
    assert!(holds.holds.is_empty(), "{holds:?}");
    let holds: Holds = client.json("/roots/sets/holds");
    assert_eq!(
        holds.holds.len(),
        1,
        "the source set holds its root: {holds:?}"
    );
    assert_eq!(
        status_of(client.post("/profiles/Nobody/retirement").send_empty()),
        404
    );

    let mut response = client
        .post("/source-sets/Test%20-%20System/set-aside")
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 201);
    let plan: RepairPlan = response.body_mut().read_json().unwrap();
    assert!(plan.content.set_aside);
    let approved: RepairPlan = client
        .post(&format!("/repairs/{}/approval", plan.id))
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let (_, ended) = client.follow(approved.job.expect("a repair job"));
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert!(
        set.read_dir()
            .unwrap()
            .all(|e| e.unwrap().file_name().to_string_lossy() == ".title-select"),
        "every file is kept in place"
    );
    let gone = client.get("/source-sets/Test%20-%20System").call();
    assert_eq!(status_of(gone), 404);
    let unregistered: Vec<UnregisteredRecord> = client.json("/unregistered");
    assert_eq!(unregistered.len(), 1, "{unregistered:?}");
    let id = unregistered[0].id;
    assert_eq!(unregistered[0].kept_files, 3);
    let history: title_select_api::lifecycle::RegistrationHistory =
        client.json(&format!("/unregistered/source_set/{id}/history"));
    assert_eq!(
        (history.record.id, history.audits.len()),
        (id, 1),
        "its audits stay readable by its id"
    );
    let refused = agent()
        .delete(format!("{}/unregistered/source_set/{id}", client.base))
        .header(&auth.0, &auth.1)
        .call();
    assert_eq!(status_of(refused), 400, "kept files hold its history");

    let kept: Vec<SourceKeptFile> =
        client.json(&format!("/unregistered/source_set/{id}/kept-files"));
    assert_eq!(kept.len(), 3);
    let deletion: SourceKeptDeletion = client
        .post(&format!("/unregistered/source_set/{id}/kept-deletions"))
        .send_json(serde_json::json!({"all": true}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let response = client
        .post(&format!("/source-kept-deletions/{}/approval", deletion.id))
        .send_json(serde_json::json!({"digest": deletion.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let left: Vec<_> = set
        .read_dir()
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "the marker goes too: {left:?}");

    let mut response = agent()
        .delete(format!("{}/unregistered/source_set/{id}", client.base))
        .header(&auth.0, &auth.1)
        .call()
        .unwrap();
    assert_eq!(response.status(), 200);
    let forgotten: Forgotten = response.body_mut().read_json().unwrap();
    assert_eq!(
        (forgotten.audits, forgotten.dat_revisions),
        (1, 0),
        "the DAT keeps its latest revision"
    );
    assert!(
        client
            .json::<Vec<UnregisteredRecord>>("/unregistered")
            .is_empty()
    );
}

#[test]
fn the_api_sets_the_storage_policy_and_a_repair_stores_the_source_set_as_it_says() {
    use title_select_api::repair::{RepairKind, RepairPlan};
    use title_select_api::storage::{StorageFormat, StoragePolicy, SystemStorage};
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    let service =
        title_select_server::spawn(env.settings(&format!("writable_paths = [{SET:?}]\n"))).unwrap();
    let client = Client::new(&service);
    client.upload("Test - System (20260101).dat", &env.dat());
    client.add_set();

    let mut policy: StoragePolicy = client.json("/storage-policy");
    assert_eq!(
        policy,
        StoragePolicy::default(),
        "TorrentZip everywhere by default"
    );
    policy.source_sets.systems.insert(
        "No-Intro/Test - System".into(),
        SystemStorage {
            format: Some(StorageFormat::Loose),
            delete_originals: None,
            ..Default::default()
        },
    );
    let put = |body: serde_json::Value| {
        let (name, value) = client.auth.clone().unwrap();
        agent()
            .put(format!("{}/storage-policy", client.base))
            .header(name, value)
            .send_json(body)
    };
    let mut response = put(serde_json::to_value(&policy).unwrap()).unwrap();
    assert_eq!(response.status(), 200);
    let saved: StoragePolicy = response.body_mut().read_json().unwrap();
    assert_eq!(saved, policy);
    assert_eq!(client.json::<StoragePolicy>("/storage-policy"), policy);
    let refused =
        put(serde_json::json!({"source_sets": {"systems": {"no system": {"format": "loose"}}}}));
    assert_eq!(status_of(refused), 400);
    let layout_of_no_disc_system = put(serde_json::json!({"play_libraries": {
        "systems": {"No-Intro/Test - System": {"layout": "flat"}}
    }}));
    assert_eq!(
        status_of(layout_of_no_disc_system),
        400,
        "only a disc system has a disc layout"
    );

    let job: Job = client
        .post("/audits")
        .send_json(serde_json::json!({"source_set": SET, "evidence": "full"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    let mut response = client
        .post("/source-sets/Test%20-%20System/repairs")
        .send_json(serde_json::json!({}))
        .unwrap();
    assert_eq!(response.status(), 201);
    let plan: RepairPlan = response.body_mut().read_json().unwrap();
    let combat = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "game:Combat (USA)")
        .expect("Combat is rewritten loose");
    // The fixture's ZIP is not TorrentZip, so it is repacked rather than converted; either way it ends loose.
    assert_eq!(combat.kind, RepairKind::Repack);
    assert!(combat.summary.ends_with("stored loose"), "{combat:?}");
    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.game.as_deref() != Some("Pong (USA)")),
        "Pong is already loose"
    );
    let mut response = client
        .post(&format!("/repairs/{}/approval", plan.id))
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap();
    assert_eq!(response.status(), 200);
    let approved: RepairPlan = response.body_mut().read_json().unwrap();
    let (_, ended) = client.follow(approved.job.unwrap());
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    assert_eq!(
        std::fs::read(set.join("Combat (USA).a26")).unwrap(),
        b"combat"
    );
    assert!(!set.join("Combat (USA).zip").exists());
}

/// Selection Profiles and Selections, run by the selection engine on the pinned clone lists (`tools/clone-lists/fetch`
/// fetches them).
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

    /// A Selection Job chooses only among the Games its Source Set's Coverage covers, and records that Coverage.
    #[test]
    fn a_selection_job_chooses_within_its_source_sets_coverage() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let (name, value) = client.auth.clone().unwrap();
        let response = agent()
            .put(format!(
                "{}/source-sets/Test%20-%20System/coverage",
                client.base
            ))
            .header(name, value)
            .send_json(json!({ "coverage": ["Europe"] }))
            .unwrap();
        assert_eq!(response.status(), 200);

        let (_, record) = save(&client, json!({"region_order": ["USA", "Europe"]}));

        let made = selection(&client, record.id);
        assert_eq!(made.pins.coverage, Some(vec!["Europe".to_owned()]));
        assert_eq!(winners(&made, "pong"), ["Pong (Europe)"]);
        let usa = &group(&made, "pong").exclusions[0];
        assert_eq!(usa.title, "Pong (USA)");
        assert!(usa.rule.is_coverage(), "{usa:?}");
    }

    /// A client that goes away mid-preview leaves no run folder under the store's `tmp`. Whether the run was cancelled
    /// or finished isn't seen here: that a dropped request raises its cancel flag is the previews' unit test, and that
    /// the engine stops when it's raised, the selection engine's.
    #[test]
    fn a_preview_whose_client_goes_away_leaves_no_run_folder() {
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
        // Each run stages its DAT in a scratch folder that lives as long as the run.
        let runs = || -> Vec<PathBuf> {
            std::fs::read_dir(env.store().join("tmp"))
                .into_iter()
                .flatten()
                .map(|e| e.unwrap().path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("dat-"))
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
        wait_for(
            "the engine to start",
            &|| !runs().is_empty(),
            std::time::Duration::from_secs(60),
        );
        drop(connection);
        wait_for(
            "the engine to stop",
            &|| runs().is_empty(),
            std::time::Duration::from_secs(60),
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
    fn a_source_sets_system_is_named_before_any_selection_and_selections_list_by_profile() {
        let env = Env::new();
        let (_service, client) = ready(&env);
        let systems: Vec<title_select_api::SourceSetSystem> = client.json("/systems");
        assert_eq!(
            systems,
            [title_select_api::SourceSetSystem {
                source_set: SET.into(),
                system: Some("Test - System".into()),
            }]
        );

        let (_, made) = save(&client, json!({}));
        assert_eq!(
            made.system, "Test - System",
            "a Profile files settings under it"
        );
        assert!(made.library_folders.is_empty(), "no Library Folder uses it");
        let by_profile: Vec<SelectionRecord> = client.json("/selections?profile=Everyday");
        assert_eq!(by_profile, std::slice::from_ref(&made));
        let both: Vec<SelectionRecord> =
            client.json("/selections?profile=Everyday&source_set=Test%20-%20System");
        assert_eq!(both, [made]);
        let response = client.get("/selections?profile=Nobody").call().unwrap();
        assert_eq!(response.status(), 404);
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
        let diff: Option<SelectionDiff> = client.json(&format!("/selections/{second}/diff"));
        let diff = diff.expect("the second selection compares with the first");
        assert_eq!((diff.from, diff.changes.len()), (first.id, 0));
        let none: Option<SelectionDiff> = client.json(&format!("/selections/{}/diff", first.id));
        assert_eq!(
            none, None,
            "the first selection has nothing earlier to compare with"
        );
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
    fn without_clone_lists_selections_are_unavailable_and_audits_still_run() {
        let env = Env::new();
        let service = title_select_server::spawn(Settings {
            clone_lists: Some(env.tmp.path().join("no-clone-lists")),
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

    /// Each Library Folder maps to the RomM platform named by its folder directly under the RomM root, even where
    /// the RomM root lies inside another Root, and to none when it is outside or nested deeper.
    #[test]
    fn each_library_folder_maps_to_the_romm_platform_its_folder_names_under_the_romm_root() {
        use title_select_api::romm::{RommFolderPlatform, RommPlatform};
        let env = Env::new();
        let play = env.tmp.path().join("roots/play");
        let other = env.tmp.path().join("roots/other");
        for dir in [play.join("library"), other.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let service = title_select_server::spawn(env.settings(&format!(
            "\n[roots.play]\npath = {:?}\nwritable = true\n\
             [roots.romm]\npath = {:?}\nwritable = true\n\
             [roots.other]\npath = {:?}\nwritable = true\n",
            play.display().to_string(),
            play.join("library").display().to_string(),
            other.display().to_string(),
        )))
        .unwrap();
        let client = Client::new(&service);
        client.upload("Test - System (20260101).dat", &dat(&env, "20260101", true));
        client.add_set();
        let job: Job = client.start_audit().body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        save(&client, json!({}));
        let mut ids = vec![];
        for (root, path) in [
            ("romm", "gba"),
            ("play", "library/snes"),
            ("play", "library/n64/hacks"),
            ("other", "gb"),
        ] {
            let (status, folder) = send(
                client.post("/libraries"),
                json!({"root": root, "path": path, "source_set": SET, "profile": "Everyday"}),
            );
            assert_eq!(status, 201, "{folder}");
            ids.push(folder["id"].as_i64().unwrap());
        }
        let mapped = |client: &Client| {
            client
                .json::<Vec<RommFolderPlatform>>("/romm/platforms")
                .into_iter()
                .map(|f| (f.folder_id, f.platform))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert!(
            mapped(&client).values().all(|p| *p == RommPlatform::NoRoot),
            "with no RomM root, no folder maps"
        );

        let (status, body) = client.change_settings(json!({"romm": {"root": "romm"}}));

        assert_eq!(status, 200, "{body}");
        let slug = |s: &str| RommPlatform::Mapped { fs_slug: s.into() };
        assert_eq!(
            mapped(&client),
            [
                (ids[0], slug("gba")),
                (ids[1], slug("snes")),
                (ids[2], RommPlatform::Nested),
                (ids[3], RommPlatform::OutsideRoot),
            ]
            .into()
        );
    }

    /// A Service whose RomM, a stub whose scan task answers `status`, maps the library folder play/gba, and the
    /// succeeded build job that changed it, whose scan is then followed at `timing`; and its events, from before the
    /// build.
    fn built_where_romm_scans(
        env: &Env,
        status: &'static str,
        timing: title_select_store::romm::ScanTiming,
    ) -> (Handle, i64, Events) {
        use title_select_api::build::{LibraryFolder, Plan};
        let play = env.tmp.path().join("roots/play");
        std::fs::create_dir_all(&play).unwrap();
        let service = title_select_server::spawn(Settings {
            romm_scan_timing: Some(timing),
            calibration_limits: None,
            notify_backoff: None,
            ..env.settings(&format!(
                "\n[roots.play]\npath = {:?}\nwritable = true\n",
                play.display().to_string()
            ))
        })
        .unwrap();
        let client = Client::new(&service);
        client.upload("Test - System (20260101).dat", &dat(env, "20260101", true));
        client.add_set();
        let job: Job = client.start_audit().body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        let (_, selection) = save(&client, json!({}));
        let (status, body) = client.change_settings(json!({"romm": {
            "enabled": true, "base_url": stub_romm_with(status), "token": {"set": "rmm_good"}, "root": "play",
        }}));
        assert_eq!(status, 200, "{body}");
        let (_, folder) = send(
            client.post("/libraries"),
            json!({"root": "play", "path": "gba", "source_set": SET, "profile": "Everyday"}),
        );
        let folder: LibraryFolder = serde_json::from_value(folder).unwrap();
        let (_, plan) = send(
            client.post(&format!("/libraries/{}/plans", folder.id)),
            json!({"selection": selection.id}),
        );
        let plan: Plan = serde_json::from_value(plan).unwrap();
        let (events, _) = client.watch();
        let (status, job) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        (service, job.id, events)
    }

    /// A build that changes a library folder RomM maps to a platform asks RomM to scan it once the build ends, and
    /// the job, which succeeded, shows how the scan ended.
    #[test]
    fn a_build_into_a_folder_romm_maps_asks_romm_to_scan_it_and_shows_how_the_scan_ended() {
        use std::time::{Duration, Instant};
        use title_select_api::romm::{RommPlatform, RommScanOutcome};
        let env = Env::new();
        let (service, id, _) = built_where_romm_scans(
            &env,
            "finished",
            title_select_store::romm::ScanTiming {
                poll: Duration::from_millis(10),
                backoff: Duration::from_millis(10),
                backoff_max: Duration::from_millis(10),
                window: Duration::from_secs(5),
                request: Duration::from_secs(5),
            },
        );
        let client = Client::new(&service);

        let deadline = Instant::now() + Duration::from_secs(30);
        let job = loop {
            let job: Job = client.json(&format!("/jobs/{id}"));
            match &job.romm_scan {
                Some(scan) if scan.outcome != RommScanOutcome::Requested => break job,
                _ if Instant::now() > deadline => panic!("the scan never ended: {job:?}"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        assert_eq!(job.state, JobState::Succeeded);
        let scan = job.romm_scan.unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::Finished, "{scan:?}");
        assert_eq!(
            scan.platform,
            RommPlatform::Mapped {
                fs_slug: "gba".into()
            }
        );
        assert_eq!(scan.task_id.as_deref(), Some("t1"));
    }

    /// Once RomM's scan finishes, its "Additions" collection is made and tracked; turning RomM off saves at once and
    /// deletes the collection in the background.
    #[test]
    fn turning_romm_off_deletes_the_additions_collection_a_build_made() {
        use std::time::{Duration, Instant};
        use title_select_api::romm::RommAdditionsOutcome;
        let env = Env::new();
        let (service, id, _) = built_where_romm_scans(
            &env,
            "finished",
            title_select_store::romm::ScanTiming {
                poll: Duration::from_millis(10),
                backoff: Duration::from_millis(10),
                backoff_max: Duration::from_millis(10),
                window: Duration::from_secs(5),
                request: Duration::from_secs(5),
            },
        );
        let client = Client::new(&service);
        let deadline = Instant::now() + Duration::from_secs(30);
        let additions = loop {
            let job: Job = client.json(&format!("/jobs/{id}"));
            match job.romm_scan.and_then(|scan| scan.additions) {
                Some(additions) => break additions,
                None if Instant::now() > deadline => panic!("the collection was never synced"),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        assert_eq!(
            additions.outcome,
            RommAdditionsOutcome::Synced,
            "{additions:?}"
        );
        assert_eq!(additions.collection_id, Some(7));
        let settings: serde_json::Value = client.json("/settings");
        let url = settings["romm"]["base_url"].as_str().unwrap().to_owned();

        let (status, body) = client.change_settings(json!({"romm": {
            "enabled": false, "base_url": url, "token": "clear", "root": "play",
        }}));
        assert_eq!(status, 200, "{body}");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ROMM_SEEN.lock().unwrap()[&url].contains(&"DELETE /api/collections/7".into()) {
            assert!(
                Instant::now() < deadline,
                "the collection was never deleted"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Once the scan a build requested finishes, RomM's view of the library folder is compared with what the build
    /// made. You can compare it again, and ask RomM to scan again a ROM it flagged, but only while RomM is on and only
    /// with your credential.
    #[test]
    fn a_finished_scan_leads_to_a_comparison_with_romm_you_can_repeat_and_act_on() {
        use std::time::{Duration, Instant};
        use title_select_api::romm::RommReconciliation;
        let env = Env::new();
        let (service, id, mut events) = built_where_romm_scans(
            &env,
            "finished",
            title_select_store::romm::ScanTiming {
                poll: Duration::from_millis(10),
                backoff: Duration::from_millis(10),
                backoff_max: Duration::from_millis(10),
                window: Duration::from_secs(5),
                request: Duration::from_secs(5),
            },
        );
        let client = Client::new(&service);
        let folder = client
            .json::<Job>(&format!("/jobs/{id}"))
            .library_folder
            .unwrap();
        let path = format!("/libraries/{folder}/romm");
        let mut compared = move || loop {
            let (name, data) = events.next_event();
            if name == "romm_reconciled" {
                break serde_json::from_str::<serde_json::Value>(&data).unwrap();
            }
        };
        assert_eq!(
            compared(),
            json!({"folder_id": folder}),
            "you hear that the comparison after the scan is ready"
        );

        let deadline = Instant::now() + Duration::from_secs(30);
        let latest = loop {
            let mut response = client.get(&path).call().unwrap();
            match response.status().as_u16() {
                200 => {
                    break response
                        .body_mut()
                        .read_json::<RommReconciliation>()
                        .unwrap();
                }
                404 if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                status => panic!("RomM's view was never compared: {status}"),
            }
        };
        assert_eq!((latest.job_id, latest.error.as_deref()), (Some(id), None));
        assert!(latest.counts.files > 0, "{latest:?}");
        assert_eq!(
            latest.counts.hash_mismatch, latest.counts.files,
            "RomM lists no file the build made: {latest:?}"
        );

        let missing = Client {
            auth: None,
            ..Client::new(&service)
        };
        assert_eq!(status_of(missing.get(&path).call()), 401);
        for post in ["reconciliations", "rescans"] {
            let response = missing
                .post(&format!("{path}/{post}"))
                .send_json(json!({"rom_id": 1}));
            assert_eq!(status_of(response), 401, "{post}");
        }

        let (status, again) = send(client.post(&format!("{path}/reconciliations")), json!({}));
        assert_eq!(status, 200, "{again}");
        let again: RommReconciliation = serde_json::from_value(again).unwrap();
        assert_eq!((again.job_id, again.counts), (None, latest.counts));
        assert_eq!(
            compared(),
            json!({"folder_id": folder}),
            "and when you compare again"
        );
        let (status, body) = send(
            client.post(&format!("{path}/rescans")),
            json!({"rom_id": 9}),
        );
        assert_eq!(
            status, 400,
            "a ROM no comparison flagged isn't scanned again: {body}"
        );

        let settings: title_select_api::settings::SettingsView = client.json("/settings");
        let (status, body) = client.change_settings(
            json!({"romm": {"enabled": false, "base_url": settings.romm.base_url}}),
        );
        assert_eq!(status, 200, "{body}");
        for post in ["reconciliations", "rescans"] {
            let (status, body) = send(client.post(&format!("{path}/{post}")), json!({"rom_id": 9}));
            assert_eq!(status, 400, "{post} while RomM is off: {body}");
        }
        assert_eq!(
            client.json::<RommReconciliation>(&path),
            again,
            "the latest is still shown"
        );
    }

    /// Stopping the Service stops following a RomM scan at once, records it as no longer followed, and frees the
    /// store to open again.
    #[test]
    fn stopping_the_service_stops_following_a_scan_at_once_and_frees_the_store() {
        use std::time::{Duration, Instant};
        use title_select_api::romm::RommScanOutcome;
        let env = Env::new();
        let (service, id, _) = built_where_romm_scans(
            &env,
            "started",
            title_select_store::romm::ScanTiming {
                poll: Duration::from_millis(10),
                backoff: Duration::from_millis(10),
                backoff_max: Duration::from_millis(10),
                window: Duration::from_secs(600),
                request: Duration::from_secs(600),
            },
        );
        let client = Client::new(&service);
        let deadline = Instant::now() + Duration::from_secs(30);
        while client
            .json::<Job>(&format!("/jobs/{id}"))
            .romm_scan
            .is_none()
        {
            assert!(Instant::now() < deadline, "the scan was never requested");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Following the scan alone would wait ten minutes on RomM.
        let (stopped, stop) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(service);
            let _ = stopped.send(());
        });
        stop.recv_timeout(Duration::from_secs(30))
            .expect("the service stopped without waiting on RomM");
        let store = title_select_store::Store::open(&env.store()).unwrap();
        let scan = store.job(id).unwrap().romm_scan.unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::Stopped, "{scan:?}");
        assert!(scan.ended_at.is_some() && scan.detail.is_some(), "{scan:?}");
    }

    /// A preview of a coverage change counts the covered missing games before and after, and names the library
    /// folders built from the source set that would go stale, saving nothing.
    #[test]
    fn a_coverage_preview_counts_missing_games_and_names_the_library_folders_it_would_make_stale() {
        use title_select_api::build::{LibraryFolder, Plan};
        use title_select_api::{CoveragePreview, CoverageStaleFolder};
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
        save(&client, json!({}));
        let mut folders = vec![];
        for path in ["built", "unbuilt", "usa"] {
            let (status, folder) = send(
                client.post("/libraries"),
                json!({"root": "play", "path": path, "source_set": SET, "profile": "Everyday"}),
            );
            assert_eq!(status, 201, "{folder}");
            folders.push(serde_json::from_value::<LibraryFolder>(folder).unwrap());
        }
        let build = |folder: &LibraryFolder| {
            let (_, selection) = save(&client, json!({}));
            let (_, plan) = send(
                client.post(&format!("/libraries/{}/plans", folder.id)),
                json!({"selection": selection.id}),
            );
            let plan: Plan = serde_json::from_value(plan).unwrap();
            let (_, job) = send(
                client.post(&format!("/plans/{}/approval", plan.id)),
                json!({"digest": plan.digest}),
            );
            let job: Job = serde_json::from_value(job).unwrap();
            assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        };
        build(&folders[0]);
        assert_eq!(put_coverage(&client, json!(["USA"])).0, 200);
        build(&folders[2]);
        assert_eq!(put_coverage(&client, Value::Null).0, 200);
        let listed = |preview: &CoveragePreview| -> Vec<String> {
            preview
                .stale_library_folders
                .iter()
                .map(|f| f.path.clone())
                .collect()
        };
        let preview = |coverage: Value| {
            let (status, body) = send(
                client.post("/source-sets/Test%20-%20System/coverage/preview"),
                json!({ "coverage": coverage }),
            );
            assert_eq!(status, 200, "{body}");
            serde_json::from_value::<CoveragePreview>(body).unwrap()
        };

        let europe = preview(json!(["Europe"]));

        assert_eq!(
            (europe.missing_before, europe.missing_after),
            (Some(2), Some(1)),
            "Breakout (USA) (Demo) and Pong (Europe), then Pong (Europe) alone"
        );
        assert_eq!(
            europe.stale_library_folders[0],
            CoverageStaleFolder {
                id: folders[0].id,
                root: "play".into(),
                path: "built".into(),
            }
        );
        assert_eq!(
            listed(&europe),
            ["built", "usa"],
            "the folders built under another coverage, not the unbuilt one"
        );
        assert_eq!(
            listed(&preview(json!(["USA"]))),
            ["built"],
            "not the folder built under the proposed coverage"
        );
        assert_eq!(europe.coverage, Some(vec!["Europe".to_owned()]));
        let set: SourceSetRecord = client.json("/source-sets/Test%20-%20System");
        assert_eq!(set.coverage, None, "a preview saves nothing");
        let unchanged = preview(Value::Null);
        assert_eq!(
            (unchanged.missing_before, unchanged.missing_after),
            (Some(2), Some(2))
        );
        assert_eq!(
            listed(&unchanged),
            ["usa"],
            "already stale for its coverage"
        );
        let (status, _) = send(
            client.post("/source-sets/Test%20-%20System/coverage/preview"),
            json!({"coverage": ["Atlantis"]}),
        );
        assert_eq!(status, 400);
    }

    /// A library folder in a writable root: a plan of the selection, an approval bound to its digest, and the build
    /// job that writes TorrentZips under the DAT's names.
    #[test]
    fn a_selection_is_planned_approved_by_digest_and_built_into_a_library_folder() {
        use title_select_api::JobKind;
        use title_select_api::build::{ItemKind, LibraryFolder, Plan};
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
            client.post("/libraries"),
            json!({"root": "sets", "path": "Play", "source_set": SET, "profile": "Everyday"}),
        );
        assert_eq!(status, 400, "a read-only root is refused: {body}");

        let (status, folder) = send(
            client.post("/libraries"),
            json!({"root": "play", "path": "test", "source_set": SET, "profile": "Everyday"}),
        );
        assert_eq!(status, 201, "{folder}");
        let folder: LibraryFolder = serde_json::from_value(folder).unwrap();
        assert!(play.join("test/.title-select/marker").is_file());
        let (status, plan) = send(
            client.post(&format!("/libraries/{}/plans", folder.id)),
            json!({"selection": selection.id}),
        );
        assert_eq!(status, 201, "{plan}");
        let plan: Plan = serde_json::from_value(plan).unwrap();
        assert!(plan.approvable(), "{:?}", plan.content.blockers);
        let read = |id: i64| client.json::<Value>(&format!("/libraries/{id}"));
        assert_eq!(read(folder.id)["plan_open"], true, "the plan waits on you");
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
            (job.kind, job.library_folder, job.plan),
            (JobKind::Build, Some(folder.id), Some(plan.id))
        );
        let (_, job) = client.follow(job.id);
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");

        for name in ["Pong (USA).zip", "Combat (USA).zip"] {
            let mut file = std::fs::File::open(play.join("test").join(name)).unwrap();
            title_select_engine::torrentzip::validate(&mut file).unwrap();
        }
        assert!(!play.join("test/.title-select/staging").exists());
        let folder: LibraryFolder = client.json(&format!("/libraries/{}", folder.id));
        let build = folder.build.unwrap();
        assert_eq!((folder.generation, build.applied, build.failed), (1, 2, 0));
        assert!(!folder.plan_open);
        assert!(folder.stale.is_empty());
        let files: Vec<title_select_api::build::BuiltFile> =
            client.json(&format!("/libraries/{}/files", folder.id));
        assert_eq!(files.len(), 2);
        let (status, _) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_ne!(status, 202, "a plan is approved once");

        // A check job reads the folder and records what it found; the folder is left as it is.
        std::fs::write(play.join("test/notes.txt"), b"mine").unwrap();
        let planned: title_select_api::build::FolderCheck =
            client.json(&format!("/libraries/{}/check", folder.id));
        assert_eq!(planned.summary.job, None, "planning checked the folder");
        let (status, job) = send(
            client.post(&format!("/libraries/{}/checks", folder.id)),
            json!(null),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(
            (job.kind, job.library_folder),
            (JobKind::Check, Some(folder.id))
        );
        let (_, job) = client.follow(job.id);
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        let check: title_select_api::build::FolderCheck =
            client.json(&format!("/libraries/{}/check", folder.id));
        assert_eq!(check.summary.job, Some(job.id));
        assert_eq!((check.files.len(), check.drift.len()), (2, 1));
        assert_eq!(std::fs::read(play.join("test/notes.txt")).unwrap(), b"mine");
        let incomplete: title_select_api::build::Incomplete =
            client.json(&format!("/libraries/{}/incomplete", folder.id));
        let reasons: Vec<_> = incomplete.items.iter().map(|i| i.reason).collect();
        assert_eq!(
            reasons,
            [title_select_api::build::IncompleteReason::NoSource],
            "drift alone leaves the folder as complete as its build"
        );

        // A cleanup keeps the drift aside once you approve it.
        let (status, body) = send(
            client.post(&format!("/libraries/{}/cleanups", folder.id)),
            json!({}),
        );
        assert_eq!(status, 400, "a cleanup that chooses nothing: {body}");
        let (status, plan) = send(
            client.post(&format!("/libraries/{}/cleanups", folder.id)),
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
            client.json(&format!("/libraries/{}/kept-files", folder.id));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "notes.txt");

        // Saving the profile again leaves the build behind it.
        save(&client, json!({"region_order": ["Japan"]}));
        assert_eq!(
            read(folder.id)["stale"],
            json!([{"kind": "profile_saved", "built": 1, "latest": 2}])
        );
    }

    /// Single copy over HTTP: naming the folder that holds the source set's placed games when several are bound, and
    /// how many games each folder holds once a build places them.
    #[test]
    fn the_api_names_the_folder_holding_placed_games_and_counts_the_games_placed_in_it() {
        use title_select_api::build::{BuiltFile, LibraryFolder, PlacedFile, Plan};
        use title_select_api::storage::StoragePolicy;
        use title_select_api::{PlacedIn, Placements};
        let env = Env::new();
        let play = env.tmp.path().join("roots/play");
        std::fs::create_dir_all(&play).unwrap();
        let service = title_select_server::spawn(env.settings(&format!(
            "writable_paths = [{SET:?}]\n\n[roots.play]\npath = {:?}\nwritable = true\n",
            play.display().to_string()
        )))
        .unwrap();
        let client = Client::new(&service);
        client.upload("Test - System (20260101).dat", &dat(&env, "20260101", true));
        client.add_set();
        let job: Job = client.start_audit().body_mut().read_json().unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        let (_, selection) = save(&client, json!({"region_order": ["USA", "Europe"]}));
        let folder = |path: &str| -> LibraryFolder {
            let (status, folder) = send(
                client.post("/libraries"),
                json!({"root": "play", "path": path, "source_set": SET, "profile": "Everyday"}),
            );
            assert_eq!(status, 201, "{folder}");
            serde_json::from_value(folder).unwrap()
        };
        let (a, b) = (folder("a"), folder("b"));
        const PLACEMENTS: &str = "/source-sets/Test%20-%20System/placements";
        let put = |body: Value| {
            let (name, value) = client.auth.clone().unwrap();
            let mut response = agent()
                .put(format!(
                    "{}/source-sets/Test%20-%20System/holding-folder",
                    client.base
                ))
                .header(name, value)
                .send_json(body)
                .unwrap();
            let status = response.status().as_u16();
            (status, response.body_mut().read_json::<Value>().unwrap())
        };

        assert_eq!(
            client.json::<Placements>(PLACEMENTS),
            Placements::default(),
            "with two folders bound and none named, none holds its games"
        );
        let (status, named) = put(json!({"folder": a.id}));
        assert_eq!(status, 200, "{named}");
        assert_eq!(
            serde_json::from_value::<Placements>(named)
                .unwrap()
                .holding_folder,
            Some(a.id)
        );
        let (status, body) = put(json!({"folder": a.id + b.id + 1}));
        assert_eq!(status, 404, "no such folder: {body}");

        let mut policy: StoragePolicy = client.json("/storage-policy");
        policy.play_libraries.single_copy = true;
        let (name, value) = client.auth.clone().unwrap();
        let response = agent()
            .put(format!("{}/storage-policy", client.base))
            .header(name, value)
            .send_json(serde_json::to_value(&policy).unwrap())
            .unwrap();
        assert_eq!(response.status(), 200);
        let (status, plan) = send(
            client.post(&format!("/libraries/{}/plans", a.id)),
            json!({"selection": selection.id}),
        );
        assert_eq!(status, 201, "{plan}");
        let plan: Plan = serde_json::from_value(plan).unwrap();
        assert!(plan.summary.place > 0, "{:?}", plan.content.notes);
        let (status, job) = send(
            client.post(&format!("/plans/{}/approval", plan.id)),
            json!({"digest": plan.digest}),
        );
        assert_eq!(status, 202, "{job}");
        let job: Job = serde_json::from_value(job).unwrap();
        assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
        let placed = client.json::<Placements>(PLACEMENTS);
        assert_eq!(
            placed.placed,
            [PlacedIn {
                library_folder: a.id,
                games: plan.summary.place,
            }],
            "one count per game, however many files it has"
        );
        assert!(placed.review_flags.is_empty());
        let only_copies: Vec<PlacedFile> =
            client.json(&format!("/libraries/{}/placed-files", a.id));
        let built: Vec<BuiltFile> = client.json(&format!("/libraries/{}/files", a.id));
        assert_eq!(only_copies.len() as u64, plan.summary.place);
        assert!(
            only_copies
                .iter()
                .all(|p| p.library_folder == a.id && built.iter().any(|f| f.target == p.path)),
            "each placed file is a built file of the folder, by its path: {only_copies:?}"
        );
        assert!(
            client
                .json::<Vec<PlacedFile>>(&format!("/libraries/{}/placed-files", b.id))
                .is_empty()
        );
        assert_eq!(
            status_of(
                client
                    .get(&format!("/libraries/{}/placed-files", a.id + b.id + 1))
                    .call()
            ),
            404
        );

        let (status, moved) = put(json!({"folder": b.id}));
        assert_eq!(status, 200, "{moved}");
        let moved: Placements = serde_json::from_value(moved).unwrap();
        assert_eq!(moved.holding_folder, Some(b.id));
        assert_eq!(
            moved.placed, placed.placed,
            "naming another folder moves nothing until a build returns them"
        );
        let (status, cleared) = put(json!({"folder": null}));
        assert_eq!(status, 200, "{cleared}");
        assert_eq!(
            serde_json::from_value::<Placements>(cleared)
                .unwrap()
                .holding_folder,
            None
        );
    }
}

#[test]
fn the_api_registers_changes_and_clears_a_source_sets_coverage() {
    const ESCAPED: &str = "Test%20-%20System";
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    let set_coverage = |coverage: serde_json::Value| -> (u16, serde_json::Value) {
        let (name, value) = client.auth.clone().unwrap();
        let mut response = agent()
            .put(format!("{}/source-sets/{ESCAPED}/coverage", client.base))
            .header(name, value)
            .send_json(serde_json::json!({ "coverage": coverage }))
            .unwrap();
        (
            response.status().as_u16(),
            response.body_mut().read_json().unwrap(),
        )
    };

    let mut response = client
        .post("/source-sets")
        .send_json(serde_json::json!({
            "root": "sets", "path": SET, "dat": "No-Intro/Test - System",
            "coverage": ["Europe", "USA", "Europe"],
        }))
        .unwrap();
    assert_eq!(response.status(), 201);
    let set: SourceSetRecord = response.body_mut().read_json().unwrap();
    assert_eq!(
        set.coverage.as_deref(),
        Some(&["USA".to_owned(), "Europe".to_owned()][..]),
        "stored once each, in the engine's region order"
    );

    let (status, refused) = set_coverage(serde_json::json!(["Japan", "Atlantis"]));
    assert_eq!(status, 400);
    let refused: ErrorBody = serde_json::from_value(refused).unwrap();
    assert!(refused.error.contains("Atlantis"), "{}", refused.error);
    let (status, changed) = set_coverage(serde_json::json!(["Japan", "Unknown"]));
    assert_eq!(status, 200, "{changed}");
    assert_eq!(changed["coverage"], serde_json::json!(["Japan", "Unknown"]));
    let fetched: SourceSetRecord = client.json(&format!("/source-sets/{ESCAPED}"));
    assert_eq!(serde_json::to_value(&fetched).unwrap(), changed);

    let (status, cleared) = set_coverage(serde_json::Value::Null);
    assert_eq!(status, 200, "{cleared}");
    assert!(
        cleared.get("coverage").is_none(),
        "a source set without a coverage reads as before: {cleared}"
    );

    let vocabulary: title_select_api::Vocabulary = client.json("/vocabulary");
    let europe = vocabulary
        .coverage_presets
        .iter()
        .find(|p| p.name == "Europe + Australia")
        .unwrap();
    assert!(europe.regions.iter().any(|r| r == "UK"), "{europe:?}");
}

/// A Source Set of mixed regions, audited: present are Pong (USA), Combat (USA), Golf (USA, Europe), the untagged
/// Chess, and one of Racing (Europe)'s two ROMs; Tennis (Europe), Soccer (World), Boxing (Japan) and Breakout (USA)
/// are missing.
fn audited_mixed_regions(env: &Env) -> (Handle, Client) {
    let set = env.tmp.path().join("roots/sets").join(SET);
    write_file(&set.join("Golf (USA, Europe).a26"), b"golf");
    write_file(&set.join("Chess.a26"), b"chess");
    write_file(&set.join("Racing (Europe).a26"), b"race-1");
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (20260101).dat",
        "20260101-000000",
        &[
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
            ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
            (
                "Golf (USA, Europe)",
                vec![rom("Golf (USA, Europe).a26", b"golf")],
            ),
            ("Chess", vec![rom("Chess.a26", b"chess")]),
            (
                "Racing (Europe)",
                vec![
                    rom("Racing (Europe).a26", b"race-1"),
                    rom("Racing (Europe) (Track 2).a26", b"race-2"),
                ],
            ),
            (
                "Tennis (Europe)",
                vec![rom("Tennis (Europe).a26", b"tennis")],
            ),
            ("Soccer (World)", vec![rom("Soccer (World).a26", b"soccer")]),
            ("Boxing (Japan)", vec![rom("Boxing (Japan).a26", b"boxing")]),
        ],
    );
    let service = env.start();
    let client = Client::new(&service);
    let (status, _) = client.upload("Test - System (20260101).dat", &std::fs::read(dat).unwrap());
    assert_eq!(status, 201);
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);
    (service, client)
}

/// Sets the Source Set's Coverage and returns the status and the body.
fn put_coverage(client: &Client, coverage: serde_json::Value) -> (u16, serde_json::Value) {
    let (name, value) = client.auth.clone().unwrap();
    let mut response = agent()
        .put(format!(
            "{}/source-sets/Test%20-%20System/coverage",
            client.base
        ))
        .header(name, value)
        .send_json(serde_json::json!({ "coverage": coverage }))
        .unwrap();
    (
        response.status().as_u16(),
        response.body_mut().read_json().unwrap(),
    )
}

/// A whole Audit's counts, which name every Game Status, less those of no Game.
fn nonzero(
    games: &std::collections::BTreeMap<GameStatus, usize>,
) -> std::collections::BTreeMap<GameStatus, usize> {
    games
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(g, n)| (*g, *n))
        .collect()
}

fn statuses(
    complete: usize,
    incomplete: usize,
    missing: usize,
) -> std::collections::BTreeMap<GameStatus, usize> {
    [
        (GameStatus::Complete, complete),
        (GameStatus::Incomplete, incomplete),
        (GameStatus::Missing, missing),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .collect()
}

#[test]
fn a_coverage_counts_only_covered_games_and_reports_the_rest_beside_them_without_a_new_audit() {
    let env = Env::new();
    let (_service, client) = audited_mixed_regions(&env);
    let summary = || {
        let mut summaries: Vec<title_select_api::SourceSetSummary> =
            client.json("/source-set-summaries");
        summaries.remove(0)
    };
    let whole = summary();
    assert_eq!(
        nonzero(&whole.latest.as_ref().unwrap().games),
        statuses(4, 1, 4)
    );
    assert_eq!(
        whole.outside_coverage, None,
        "without a coverage, nothing is outside it"
    );
    assert_eq!(whole.covered_entries, None);
    assert!(!whole.coverage_unavailable);

    let (status, body) = put_coverage(&client, serde_json::json!(["Europe"]));
    assert_eq!(status, 200, "{body}");

    let covered = summary();
    let latest = covered.latest.as_ref().unwrap();
    assert_eq!(latest.id, whole.latest.as_ref().unwrap().id, "no new audit");
    assert_eq!(
        latest.games,
        statuses(1, 1, 2),
        "Golf (USA, Europe), Racing (Europe), Tennis (Europe) and Soccer (World)"
    );
    assert_eq!(
        covered.outside_coverage,
        Some(title_select_api::OutsideCoverage {
            games: statuses(3, 0, 2),
            unknown_region: 1,
        }),
        "Pong, Combat and Chess present; Boxing and Breakout missing; Chess of no known region"
    );
    assert_eq!(
        covered.covered_entries,
        Some(title_select_api::CoveredEntries {
            findings: [(Finding::Matched, 2), (Finding::Unmatched, 1)].into(),
            claimed: 0,
        }),
        "Golf's and Racing's entries, and notes.txt, which belongs to no game"
    );
    assert!(covered.set.stale.is_empty(), "{:?}", covered.set.stale);
    assert_eq!(latest.findings, whole.latest.as_ref().unwrap().findings);

    put_coverage(&client, serde_json::Value::Null);
    assert_eq!(summary(), whole, "clearing the coverage reads as before");
}

#[test]
fn a_forgotten_source_set_registered_and_audited_again_is_counted_from_its_new_audit() {
    let env = Env::new();
    let (_service, client) = audited_mixed_regions(&env);
    let auth = client.auth.clone().unwrap();
    put_coverage(&client, serde_json::json!(["Europe"]));
    let summary = || {
        let mut summaries: Vec<title_select_api::SourceSetSummary> =
            client.json("/source-set-summaries");
        summaries.remove(0)
    };
    let before = summary();
    assert_eq!(before.latest.as_ref().unwrap().games, statuses(1, 1, 2));
    let response = client
        .post("/source-sets/Test%20-%20System/unregistration")
        .send_empty()
        .unwrap();
    assert_eq!(response.status(), 200);
    let unregistered: Vec<title_select_api::lifecycle::UnregisteredRecord> =
        client.json("/unregistered");
    let forget = agent()
        .delete(format!(
            "{}/unregistered/source_set/{}",
            client.base, unregistered[0].id
        ))
        .header(&auth.0, &auth.1)
        .call();
    assert_eq!(status_of(forget), 200);

    std::fs::remove_file(
        env.tmp
            .path()
            .join("roots/sets")
            .join(SET)
            .join("Golf (USA, Europe).a26"),
    )
    .unwrap();
    client.add_set();
    put_coverage(&client, serde_json::json!(["Europe"]));
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);

    let after = summary();
    assert_eq!(
        after.latest.as_ref().unwrap().id,
        before.latest.as_ref().unwrap().id,
        "the store gives the new audit the forgotten one's id"
    );
    assert_eq!(
        after.latest.as_ref().unwrap().games,
        statuses(0, 1, 3),
        "Golf (USA, Europe) is missing now"
    );
    let regions: title_select_api::RegionCounts =
        client.json("/source-sets/Test%20-%20System/regions");
    assert_eq!(regions.total, statuses(3, 1, 5));
}

#[test]
fn entries_of_games_outside_the_coverage_leave_out_their_claims_and_findings() {
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    std::fs::remove_file(set.join("notes.txt")).unwrap();
    write_file(&set.join("Tennis (Europe).a26"), b"tennis");
    write_file(&set.join("Boxing.a26"), b"boxing");
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (20260101).dat",
        "20260101-000000",
        &[
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
            (
                "Tennis (Europe)",
                vec![rom("Tennis (Europe).a26", b"tennis")],
            ),
            ("Boxing (Japan)", vec![rom("Boxing (Japan).a26", b"boxing")]),
        ],
    );
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System (20260101).dat", &std::fs::read(dat).unwrap());
    client.add_set();
    put_coverage(&client, serde_json::json!(["Europe"]));
    let job: Job = client
        .post("/audits")
        .send_json(serde_json::json!({"source_set": SET, "evidence": "quick"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(client.follow(job.id).1.state, JobState::Succeeded);

    let summaries: Vec<title_select_api::SourceSetSummary> = client.json("/source-set-summaries");

    let latest = summaries[0].latest.as_ref().unwrap();
    assert_eq!(latest.findings[&Finding::Misnamed], 1, "Boxing.a26");
    assert_eq!(latest.claimed, Some(1), "Combat (USA).zip, read on claims");
    assert_eq!(
        summaries[0].covered_entries,
        Some(title_select_api::CoveredEntries {
            findings: [(Finding::Matched, 1)].into(),
            claimed: 0,
        }),
        "only Tennis (Europe), read in full"
    );
}

#[test]
fn without_clone_lists_a_coverage_is_not_applied_and_the_summary_says_so() {
    let env = Env::new();
    let (service, client) = audited_mixed_regions(&env);
    let (status, body) = put_coverage(&client, serde_json::json!(["Europe"]));
    assert_eq!(status, 200, "{body}");
    drop(service);
    let service = title_select_server::spawn(Settings {
        clone_lists: Some(env.tmp.path().join("no-clone-lists")),
        ..env.settings("")
    })
    .unwrap();
    let client = Client::new(&service);

    let summaries: Vec<title_select_api::SourceSetSummary> = client.json("/source-set-summaries");

    assert!(summaries[0].coverage_unavailable);
    assert_eq!(
        nonzero(&summaries[0].latest.as_ref().unwrap().games),
        statuses(4, 1, 4),
        "the whole audit"
    );
    assert_eq!(summaries[0].outside_coverage, None);
    assert_eq!(summaries[0].covered_entries, None);
    let status = |response: Result<ureq::http::Response<ureq::Body>, ureq::Error>| {
        response.unwrap().status().as_u16()
    };
    assert_eq!(
        status(client.get("/source-sets/Test%20-%20System/regions").call()),
        503
    );
    assert_eq!(
        status(
            client
                .post("/source-sets/Test%20-%20System/coverage/preview")
                .send_json(serde_json::json!({ "coverage": null }))
        ),
        503
    );
}

#[test]
fn the_api_counts_the_latest_audit_by_region_overlapping_for_games_of_several_regions() {
    use title_select_api::{RegionCount, RegionCounts};
    let env = Env::new();
    let (_service, client) = audited_mixed_regions(&env);
    put_coverage(&client, serde_json::json!(["Europe"]));

    let counts: RegionCounts = client.json("/source-sets/Test%20-%20System/regions");

    let row = |region: &str, covered: bool, games| RegionCount {
        region: region.into(),
        covered,
        games,
    };
    assert_eq!(
        counts.regions,
        [
            row("USA", false, statuses(3, 0, 1)),
            row("World", true, statuses(0, 0, 1)),
            row("Europe", true, statuses(1, 1, 1)),
            row("Japan", false, statuses(0, 0, 1)),
            row("Unknown", false, statuses(1, 0, 0)),
        ],
        "Golf (USA, Europe) counts under both USA and Europe"
    );
    assert_eq!(counts.total, statuses(4, 1, 4), "each game once");
    let summary: Vec<title_select_api::SourceSetSummary> = client.json("/source-set-summaries");
    assert_eq!(counts.audit, summary[0].latest.as_ref().unwrap().id);
}

#[test]
fn the_api_checks_a_source_set_for_changes_and_a_followed_one_goes_stale_on_a_newer_revision() {
    use title_select_api::{SourceSetStale, StatWalk};
    const ESCAPED: &str = "Test%20-%20System";
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let refused = client
        .post(&format!("/source-sets/{ESCAPED}/stat-walks"))
        .send_empty();
    assert_eq!(
        status_of(refused),
        400,
        "nothing to compare with before an audit"
    );
    let audit: Job = client.start_audit().body_mut().read_json().unwrap();
    client.follow(audit.id);
    let check = || -> Job {
        let mut response = client
            .post(&format!("/source-sets/{ESCAPED}/stat-walks"))
            .send_empty()
            .unwrap();
        assert_eq!(response.status(), 202);
        let job: Job = response.body_mut().read_json().unwrap();
        client.follow(job.id).1
    };

    assert_eq!(check().state, JobState::Succeeded);
    let set: SourceSetRecord = client.json(&format!("/source-sets/{ESCAPED}"));
    assert_eq!(set.stale, []);
    write_file(
        &env.tmp.path().join("roots/sets").join(SET).join("new.bin"),
        b"new",
    );
    assert_eq!(check().state, JobState::Succeeded);
    let set: SourceSetRecord = client.json(&format!("/source-sets/{ESCAPED}"));
    assert!(
        matches!(
            &set.stale[..],
            [SourceSetStale::FilesChanged { added: 1, .. }]
        ),
        "{:?}",
        set.stale
    );
    let walks: Vec<StatWalk> = client.json(&format!("/source-sets/{ESCAPED}/stat-walks"));
    assert_eq!(walks.len(), 1);

    let follow = |follow: bool| -> SourceSetRecord {
        let mut request = agent().put(format!("{}/source-sets/{ESCAPED}/follow", client.base));
        if let Some((name, value)) = &client.auth {
            request = request.header(name, value);
        }
        let mut response = request
            .send_json(serde_json::json!({ "follow": follow }))
            .unwrap();
        assert_eq!(response.status(), 200);
        response.body_mut().read_json().unwrap()
    };
    assert!(follow(true).follow);
    let newer = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (20260201).dat",
        "20260201-000000",
        &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
    );
    client.upload(
        "Test - System (20260201).dat",
        &std::fs::read(newer).unwrap(),
    );
    let set: SourceSetRecord = client.json(&format!("/source-sets/{ESCAPED}"));
    assert!(set.follow);
    assert!(
        set.stale
            .iter()
            .any(|s| matches!(s, SourceSetStale::NewerRevision { version, .. } if version == "20260201-000000")),
        "{:?}",
        set.stale
    );
    assert_eq!(
        follow(false).stale.len(),
        1,
        "only the changed files remain"
    );
    let followed: Vec<title_select_api::FollowedRevision> =
        client.json(&format!("/source-sets/{ESCAPED}/followed-revisions"));
    assert_eq!(
        followed
            .iter()
            .map(|r| r.version.as_str())
            .collect::<Vec<_>>(),
        ["20260201-000000"],
        "its history keeps what made it stale"
    );
}

/// With a free job slot, a check for changes queued behind a repair of its source set waits for it, and the runner
/// claims it as soon as the repair ends.
#[test]
fn the_runner_starts_a_check_waiting_on_a_repair_once_it_ends() {
    const ESCAPED: &str = "Test%20-%20System";
    let env = Env::new();
    let service = title_select_server::spawn(env.settings(&format!(
        "writable_paths = [{SET:?}]\n[jobs]\nconcurrent = 2\n"
    )))
    .unwrap();
    let client = Client::new(&service);
    client.upload("Test - System (20260101).dat", &env.dat());
    client.add_set();
    let audit: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(client.follow(audit.id).1.state, JobState::Succeeded);
    let plan: title_select_api::repair::RepairPlan = client
        .post(&format!("/source-sets/{ESCAPED}/repairs"))
        .send_json(serde_json::json!({}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();

    let approved: title_select_api::repair::RepairPlan = client
        .post(&format!("/repairs/{}/approval", plan.id))
        .send_json(serde_json::json!({"digest": plan.digest}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let walk: Job = client
        .post(&format!("/source-sets/{ESCAPED}/stat-walks"))
        .send_empty()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let repair = approved.job.expect("approval queues a repair job");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let ended = |id: i64| client.json::<Job>(&format!("/jobs/{id}")).state.is_final();
    while !(ended(repair) && ended(walk.id)) {
        assert!(
            std::time::Instant::now() < deadline,
            "the check never ran: {:?}",
            client.json::<Job>(&format!("/jobs/{}", walk.id))
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Fetching is off until you turn it on, and refuses before any request when nothing can be fetched. No test sends a
/// request to a DAT Provider.
#[test]
fn fetching_reports_its_state_and_refuses_before_any_request() {
    use title_select_api::fetch::{FetchProvider, FetchStatus};
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let status: FetchStatus = client.json("/fetch");
    let providers: Vec<_> = status
        .providers
        .iter()
        .map(|p| (p.provider, p.enabled, p.running))
        .collect();
    assert_eq!(
        providers,
        [
            (FetchProvider::Redump, false, false),
            (FetchProvider::Mame, false, false)
        ]
    );
    let mut off = client.post("/fetch/redump").send_empty().unwrap();
    assert_eq!(off.status(), 400);
    let body: ErrorBody = off.body_mut().read_json().unwrap();
    assert!(
        body.error.contains("turn on fetching from Redump"),
        "{}",
        body.error
    );
    let no_intro = client.post("/fetch/no-intro").send_empty().unwrap();
    assert_eq!(no_intro.status(), 404);

    let (status, _) =
        client.change_settings(serde_json::json!({"fetch": {"redump": true, "mame": false}}));
    assert_eq!(status, 200);
    let mut nothing = client.post("/fetch/redump").send_empty().unwrap();
    assert_eq!(nothing.status(), 400);
    let body: ErrorBody = nothing.body_mut().read_json().unwrap();
    assert!(
        body.error.contains("no source set follows"),
        "{}",
        body.error
    );
}

#[test]
fn a_calibration_is_queued_and_a_second_is_refused() {
    let env = Env::new();
    let service = title_select_server::spawn(Settings {
        hold_jobs: true,
        ..env.settings("")
    })
    .unwrap();
    let client = Client::new(&service);

    let mut first = client.post("/calibrations").send_empty().unwrap();
    assert_eq!(first.status(), 202);
    let first: Job = first.body_mut().read_json().unwrap();
    assert_eq!(first.kind, title_select_api::JobKind::Calibration);
    assert_eq!(first.state, JobState::Queued);

    let mut second = client.post("/calibrations").send_empty().unwrap();
    assert_eq!(second.status(), 409);
    let body: ErrorBody = second.body_mut().read_json().unwrap();
    assert!(
        body.error.contains(&format!("job {}", first.id)),
        "{}",
        body.error
    );

    let cancelled = client
        .post(&format!("/jobs/{}/cancellation", first.id))
        .send_empty()
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let job: Job = client.json(&format!("/jobs/{}", first.id));
    assert!(job.state.is_final(), "{:?}", job.state);
}

/// What a calibration measures is the store's to test: here each step ends at its first byte, never at its time.
#[test]
fn a_calibration_measures_the_host_and_each_root_and_reports_them() {
    let env = Env::new();
    let service = title_select_server::spawn(Settings {
        calibration_limits: Some(title_select_server::CalibrationLimits {
            step_bytes: 1,
            min_bytes: 1,
            ..title_select_server::CalibrationLimits::default()
        }),
        ..env.settings("")
    })
    .unwrap();
    let client = Client::new(&service);
    let empty: title_select_api::calibration::Calibration = client.json("/calibration");
    assert!(empty.host.is_none() && empty.roots.is_empty() && empty.stale.is_empty());

    let (mut events, _) = client.watch();
    let mut started = client.post("/calibrations").send_empty().unwrap();
    assert_eq!(started.status(), 202);
    let started: Job = started.body_mut().read_json().unwrap();
    // Every watching client hears that the settings changed, with the ceilings measured, by the time it ends.
    let mut heard_settings = false;
    let ended = loop {
        let (name, data) = events.next_event();
        heard_settings |= name == "settings";
        if let JobEvent::State(job) = serde_json::from_str(&data).unwrap()
            && job.id == started.id
            && job.state.is_final()
        {
            break *job;
        }
    };
    assert!(heard_settings, "no settings event as the calibration ended");

    assert_eq!(ended.state, JobState::Succeeded, "{:?}", ended.error);
    let measured: title_select_api::calibration::Calibration =
        client.json(&format!("/jobs/{}/calibration", started.id));
    let host = measured.host.expect("the host's sweep");
    assert_eq!(host.job, started.id);
    assert!(!host.curve.is_empty() && host.knee.is_some());
    assert_eq!(measured.roots.len(), 1);
    assert_eq!(measured.roots[0].root, "sets");
    let now: title_select_api::calibration::Calibration = client.json("/calibration");
    assert_eq!(now.host.map(|h| h.job), Some(started.id));
    assert!(now.stale.is_empty(), "{:?}", now.stale);
}
