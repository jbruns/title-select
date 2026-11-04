//! Fetching newer DAT Revisions for Following Source Sets (ADR 0012), against a fake DAT Provider on a local port.
//! No test sends a request anywhere else.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use title_select_api::AuditEvidence;
use title_select_api::fetch::{
    FetchArtifact, FetchOutcome, FetchProvider, FetchReport, FetchTrigger,
};
use title_select_store::config::Config;
use title_select_store::fetch::{Endpoints, FetchError, Fetcher, Http};
use title_select_store::{Error, Store};

/// What the fake provider answers a path with.
#[derive(Clone)]
enum Reply {
    Body(Vec<u8>),
    Status(u16),
    Redirect(String),
}

/// A request the fake provider saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    user_agent: Option<String>,
    at: Instant,
}

/// A fake DAT Provider: an HTTP/1.1 server on a local port that answers each path as told and records each request.
struct FakeProvider {
    url: String,
    replies: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeProvider {
    fn start() -> FakeProvider {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let replies: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (r, s) = (replies.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut user_agent = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("user-agent")
                    {
                        user_agent = Some(value.trim().to_owned());
                    }
                }
                s.lock().unwrap().push(Seen {
                    path: path.clone(),
                    user_agent,
                    at: Instant::now(),
                });
                let reply = r.lock().unwrap().get(&path).cloned();
                let (status, extra, body) = match reply {
                    Some(Reply::Body(body)) => (200, String::new(), body),
                    Some(Reply::Status(status)) => (status, String::new(), Vec::new()),
                    Some(Reply::Redirect(to)) => (302, format!("Location: {to}\r\n"), Vec::new()),
                    None => (404, String::new(), Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        FakeProvider { url, replies, seen }
    }

    fn reply(&self, path: &str, reply: Reply) {
        self.replies.lock().unwrap().insert(path.to_owned(), reply);
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[test]
fn no_request_can_address_a_no_intro_host() {
    let http = Http::new();
    for url in [
        "https://datomatic.no-intro.org/index.php?page=download",
        "http://no-intro.org/",
        "https://NO-INTRO.ORG./x",
        "https://forum.No-Intro.org:443/",
        "http://user@datomatic.no-intro.org/",
    ] {
        match http.get(url, None, 1 << 20) {
            Err(FetchError::Forbidden(_)) => {}
            other => panic!("{url}: {other:?}"),
        }
    }
}

#[test]
fn a_redirect_to_a_no_intro_host_is_refused_before_it_is_followed() {
    let fake = FakeProvider::start();
    fake.reply(
        "/datfile/psx/",
        Reply::Redirect("https://datomatic.no-intro.org/index.php".into()),
    );
    match Http::new().get(&format!("{}/datfile/psx/", fake.url), None, 1 << 20) {
        Err(FetchError::Forbidden(_)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.seen().len(), 1, "only the first request was sent");
}

#[test]
fn a_request_names_title_select_and_follows_an_allowed_redirect() {
    let fake = FakeProvider::start();
    fake.reply("/a", Reply::Redirect(format!("{}/b", fake.url)));
    fake.reply("/b", Reply::Body(b"payload".to_vec()));
    let got = Http::new()
        .get(&format!("{}/a", fake.url), None, 1 << 20)
        .unwrap();
    assert_eq!(got.bytes, b"payload");
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    let agent = seen[0].user_agent.clone().unwrap();
    assert!(agent.starts_with("title-select/"), "{agent}");
}

#[test]
fn a_response_larger_than_its_limit_or_not_ok_fails() {
    let fake = FakeProvider::start();
    fake.reply("/big", Reply::Body(vec![0; 2048]));
    fake.reply("/gone", Reply::Status(503));
    let http = Http::new();
    assert!(matches!(
        http.get(&format!("{}/big", fake.url), None, 1024),
        Err(FetchError::TooLarge { .. })
    ));
    assert!(matches!(
        http.get(&format!("{}/gone", fake.url), None, 1024),
        Err(FetchError::Status { status: 503, .. })
    ));
}

/// A Logiqx DAT named `name` at `version`, with one Game.
fn dat_xml(name: &str, version: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<datafile>
  <header><name>{name}</name><version>{version}</version></header>
  <game name="Game (USA)"><rom name="Game (USA).bin" size="4" crc="d87f7e0c"/></game>
</datafile>
"#
    )
    .into_bytes()
}

/// A ZIP of `members`, in memory.
fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write as _;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, payload) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(payload).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// A store with the Source Set `PSX`, bound to `Redump/Sony - PlayStation` at version 1, audited and Following, and a
/// fake provider standing in for every official endpoint.
struct Env {
    _tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    fake: FakeProvider,
    fetcher: Fetcher,
}

impl Env {
    fn new(fetch: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sets/PSX")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[fetch]\n{fetch}\n",
            tmp.path().join("sets").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let fake = FakeProvider::start();
        let fetcher = Fetcher::new(Endpoints::at(&fake.url));
        let env = Env {
            _tmp: tmp,
            store,
            config,
            fake,
            fetcher,
        };
        env.follow("PSX", "Redump", "Sony - PlayStation");
        env
    }

    /// Imports version 1 of `<provider>/<dat>` and registers, audits and follows the Source Set `name` bound to it.
    fn follow(&self, name: &str, provider: &str, dat: &str) {
        std::fs::create_dir_all(self._tmp.path().join("sets").join(name)).unwrap();
        self.store
            .import_dat_bytes(&dat_xml(dat, "1"), "v1.dat", provider, None)
            .unwrap();
        self.store
            .add_source_set(&self.config, "sets", name, &format!("{provider}/{dat}"))
            .unwrap();
        self.store
            .audit_source_set(&self.config, name, None, AuditEvidence::Incremental)
            .unwrap();
        self.store.set_follow(name, true).unwrap();
    }

    fn fetch(&self, provider: FetchProvider, trigger: FetchTrigger) -> Result<FetchReport, Error> {
        self.store
            .fetch(&self.config, &self.fetcher, provider, trigger, "Operator")
    }

    fn serve_psx(&self, version: &str) {
        let dat = zip_of(&[(
            "Sony - PlayStation (2026).dat",
            &dat_xml("Sony - PlayStation", version),
        )]);
        self.fake.reply("/datfile/psx/", Reply::Body(dat));
        self.fake.reply(
            "/cues/psx/",
            Reply::Body(zip_of(&[("Game (USA).cue", b"FILE")])),
        );
        self.fake.reply(
            "/sbi/psx/",
            Reply::Body(zip_of(&[("Game (USA).sbi", b"SBI\0")])),
        );
    }

    fn audits(&self) -> i64 {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        db.query_row("SELECT count(*) FROM audits", [], |r| r.get(0))
            .unwrap()
    }

    /// Moves every recorded request back by `hours`.
    fn backdate(&self, hours: i64) {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        let mut rows = db.prepare("SELECT id, started_at FROM fetches").unwrap();
        let rows: Vec<(i64, String)> = rows
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (id, at) in rows {
            let at: jiff::Timestamp = at.parse().unwrap();
            let at = at - jiff::SignedDuration::from_hours(hours);
            db.execute(
                "UPDATE fetches SET started_at = ?2, finished_at = ?2 WHERE id = ?1",
                rusqlite::params![id, at.to_string()],
            )
            .unwrap();
        }
    }
}

fn outcomes(report: &FetchReport) -> Vec<(String, FetchArtifact, Option<FetchOutcome>)> {
    report
        .attempts
        .iter()
        .map(|a| (a.system.clone(), a.artifact, a.outcome))
        .collect()
}

#[test]
fn a_redump_fetch_brings_a_newer_dat_and_its_packs_a_second_apart_and_starts_no_audit() {
    let env = Env::new("redump = true");
    env.serve_psx("2");
    let (jobs, audits) = (env.store.jobs(100).unwrap(), env.audits());

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None);
    let added = Some(FetchOutcome::Added);
    assert_eq!(
        outcomes(&report),
        [
            ("psx".into(), FetchArtifact::Dat, added),
            ("psx".into(), FetchArtifact::Cues, added),
            ("psx".into(), FetchArtifact::Sbi, added),
        ]
    );
    let seen = env.fake.seen();
    let paths: Vec<_> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["/datfile/psx/", "/cues/psx/", "/sbi/psx/"]);
    for pair in seen.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= Duration::from_secs(1),
            "{seen:?}"
        );
    }
    for s in &seen {
        assert!(
            s.user_agent
                .as_deref()
                .unwrap()
                .starts_with("title-select/")
        );
    }

    let dat = &report.attempts[0];
    assert_eq!(dat.version.as_deref(), Some("2"));
    let stale = env.store.followed_revisions("PSX").unwrap();
    assert_eq!(stale.len(), 1, "the following source set is stale");
    assert_eq!(Some(&stale[0].sha256), dat.dat_revision.as_ref());
    for pack in &report.attempts[1..] {
        let sha = pack.pack.as_ref().unwrap();
        assert!(env.store.dir().join("packs").join(sha).is_file());
    }
    assert_eq!(env.store.jobs(100).unwrap(), jobs, "fetching queues no job");
    assert_eq!(env.audits(), audits, "fetching starts no audit");
}

#[test]
fn each_system_is_fetched_at_most_once_a_day_even_by_hand() {
    let env = Env::new("redump = true");
    env.serve_psx("2");
    env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();

    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("once a day"), "{why}"),
        other => panic!("{other:?}"),
    }
    let scheduled = env
        .fetch(FetchProvider::Redump, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(scheduled.attempts, []);
    assert_eq!(env.fake.seen().len(), 3, "nothing more was requested");

    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Redump, FetchTrigger::Schedule)
        .unwrap();
    let unchanged = Some(FetchOutcome::Unchanged);
    assert_eq!(
        outcomes(&again),
        [
            ("psx".into(), FetchArtifact::Dat, unchanged),
            ("psx".into(), FetchArtifact::Cues, unchanged),
            ("psx".into(), FetchArtifact::Sbi, unchanged),
        ]
    );
}

#[test]
fn a_failed_request_stops_the_fetch_and_keeps_the_last_good_revision() {
    let env = Env::new("redump = true");
    env.follow("PS2", "Redump", "Sony - PlayStation 2");
    env.serve_psx("2");
    env.fake.reply("/datfile/psx/", Reply::Status(503));

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [("psx".into(), FetchArtifact::Dat, Some(FetchOutcome::Failed))]
    );
    assert!(report.stopped.is_some());
    assert!(report.attempts[0].error.as_deref().unwrap().contains("503"));
    assert_eq!(env.fake.seen().len(), 1, "nothing after the first error");
    assert_eq!(env.store.followed_revisions("PSX").unwrap(), []);
    assert_eq!(
        env.store.dat_revisions().unwrap().len(),
        2,
        "version 1 of each DAT"
    );

    let status = env.store.fetch_status(&env.config, &env.fetcher).unwrap();
    let redump = &status.providers[0];
    assert_eq!(redump.provider, FetchProvider::Redump);
    let psx = redump
        .systems
        .iter()
        .find(|s| s.system.as_deref() == Some("psx"))
        .unwrap();
    assert_eq!(psx.source_sets, ["PSX"]);
    let dat = &psx.artifacts[0];
    assert_eq!(dat.artifact, FetchArtifact::Dat);
    assert_eq!(
        dat.last_attempt.as_ref().unwrap().outcome,
        Some(FetchOutcome::Failed)
    );
    assert_eq!(dat.last_success, None);
    assert!(dat.next_at.is_some(), "a failure also waits a day");
}

#[test]
fn a_dat_for_another_system_fails_the_fetch() {
    let env = Env::new("redump = true");
    let other = zip_of(&[("x.dat", &dat_xml("Sega - Saturn", "2"))]);
    env.fake.reply("/datfile/psx/", Reply::Body(other));

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.attempts[0].outcome, Some(FetchOutcome::Failed));
    assert_eq!(env.store.dat_revisions().unwrap().len(), 1);
}

#[test]
fn fetching_is_off_until_you_turn_it_on_and_only_for_following_systems() {
    let env = Env::new("");
    env.serve_psx("2");
    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("redump = true"), "{why}"),
        other => panic!("{other:?}"),
    }
    let status = env.store.fetch_status(&env.config, &env.fetcher).unwrap();
    assert!(status.providers.iter().all(|p| !p.enabled));

    let env = Env::new("redump = true");
    env.serve_psx("2");
    env.store.set_follow("PSX", false).unwrap();
    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("follow"), "{why}"),
        other => panic!("{other:?}"),
    }
    env.store.set_follow("PSX", true).unwrap();
    env.store.unregister_source_set(&env.config, "PSX").unwrap();
    assert!(
        env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
            .is_err()
    );
    assert_eq!(env.fake.seen().len(), 0, "nothing was requested");
}

/// A MAME ListXML at build `build`.
fn listxml(build: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<mame build="{build}">
  <machine name="pong"><rom name="pong.bin" size="4" crc="d87f7e0c"/></machine>
</mame>
"#
    )
    .into_bytes()
}

fn software_list(name: &str, rev: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<softwarelist name="{name}" description="Test">
  <software name="game{rev}"><description>Game</description>
    <part name="cart" interface="cart"><dataarea name="rom" size="4">
      <rom name="game.bin" size="4" crc="d87f7e0c"/>
    </dataarea></part>
  </software>
</softwarelist>
"#
    )
    .into_bytes()
}

impl Env {
    fn serve_mame(&self, tag: &str) {
        let url = &self.fake.url;
        let asset = format!("{url}/mamedev/mame/releases/download/{tag}/{tag}lx.zip");
        let release = format!(
            r#"{{"tag_name":"{tag}","assets":[{{"name":"{tag}s.exe","browser_download_url":"{url}/x.exe"}},
               {{"name":"{tag}lx.zip","browser_download_url":"{asset}"}}]}}"#
        );
        self.fake.reply(
            "/repos/mamedev/mame/releases/latest",
            Reply::Body(release.into_bytes()),
        );
        self.fake.reply(
            &format!("/mamedev/mame/releases/download/{tag}/{tag}lx.zip"),
            Reply::Redirect(format!("{url}/objects/{tag}lx.zip")),
        );
        self.fake.reply(
            &format!("/objects/{tag}lx.zip"),
            Reply::Body(zip_of(&[(&format!("{tag}.xml"), &listxml(tag))])),
        );
        self.fake.reply(
            &format!("/mamedev/mame/{tag}/hash/nes.xml"),
            Reply::Body(software_list("nes", tag)),
        );
    }
}

#[test]
fn a_mame_fetch_finds_the_latest_release_and_brings_its_listxml_and_software_lists() {
    let env = Env::new("mame = true");
    env.store
        .import_dat_bytes(&listxml("mame0001"), "mame.xml", "MAME", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/Arcade")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Arcade", "MAME/MAME")
        .unwrap();
    env.store
        .audit_source_set(&env.config, "Arcade", None, AuditEvidence::Incremental)
        .unwrap();
    env.store.set_follow("Arcade", true).unwrap();
    env.store
        .import_dat_bytes(&software_list("nes", "0"), "nes.xml", "MAME", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/NES")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "NES", "MAME/nes")
        .unwrap();
    env.store
        .audit_source_set(&env.config, "NES", None, AuditEvidence::Incremental)
        .unwrap();
    env.store.set_follow("NES", true).unwrap();
    env.serve_mame("mame0270");

    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None, "{report:?}");
    let added = Some(FetchOutcome::Added);
    assert_eq!(
        outcomes(&report),
        [
            ("mame".into(), FetchArtifact::Release, added),
            ("mame".into(), FetchArtifact::Dat, added),
            ("nes".into(), FetchArtifact::Dat, added),
        ]
    );
    assert_eq!(report.attempts[1].version.as_deref(), Some("mame0270"));
    assert!(
        report
            .attempts
            .iter()
            .all(|a| a.release.as_deref() == Some("mame0270"))
    );
    assert_eq!(env.store.followed_revisions("Arcade").unwrap().len(), 1);
    assert_eq!(env.store.followed_revisions("NES").unwrap().len(), 1);
    let seen = env.fake.seen();
    for pair in seen.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= Duration::from_secs(1),
            "{seen:?}"
        );
    }

    // A day later the release is unchanged, so only the release is checked.
    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(
        outcomes(&again),
        [(
            "mame".into(),
            FetchArtifact::Release,
            Some(FetchOutcome::Unchanged)
        )]
    );
}

#[test]
fn a_mame_release_asset_outside_the_official_release_downloads_is_refused() {
    let env = Env::new("mame = true");
    env.store
        .import_dat_bytes(&listxml("mame0001"), "mame.xml", "MAME", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/Arcade")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Arcade", "MAME/MAME")
        .unwrap();
    env.store.set_follow("Arcade", true).unwrap();
    let release = r#"{"tag_name":"mame0270","assets":[{"name":"mame0270lx.zip",
        "browser_download_url":"https://mirror.example/mame0270lx.zip"}]}"#;
    env.fake.reply(
        "/repos/mamedev/mame/releases/latest",
        Reply::Body(release.as_bytes().to_vec()),
    );
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [(
            "mame".into(),
            FetchArtifact::Release,
            Some(FetchOutcome::Failed)
        )]
    );
    assert_eq!(env.fake.seen().len(), 1);
}
