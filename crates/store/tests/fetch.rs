//! Fetching newer DAT Revisions for Following Source Sets (ADR 0012), against a fake DAT Provider on a local port.
//! No test sends a request anywhere else.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use title_select_store::fetch::{FetchError, Http};

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
