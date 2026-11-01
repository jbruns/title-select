//! The headless client: starts an Audit Job on a running Service and follows it to completion.

use std::io::BufRead;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use title_select_api::{
    AuditEvidence, ErrorBody, Finding, GameStatus, Health, Job, JobEvent, JobProgress, JobState,
    PREFIX, Report, SSE_KEEP_ALIVE, StartAudit,
};

/// Succeeds only when `url` answers 200 with `{"status": "ok"}` within `timeout`. The reason for a failure is one
/// line, whatever the URL or the answer holds.
pub fn health(url: &str, timeout: Duration) -> Result<(), String> {
    check_health(url, timeout).map_err(|e| {
        let mut line = String::new();
        for c in e.chars() {
            if c.is_control() {
                line.extend(c.escape_default());
            } else {
                line.push(c);
            }
        }
        line
    })
}

fn check_health(url: &str, timeout: Duration) -> Result<(), String> {
    const SHOWN: usize = 200;
    // A container-local check: never follow a redirect elsewhere or go through the environment's proxy.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Timeout(_) => format!(
            "the service at {url} did not answer within {} s",
            timeout.as_secs()
        ),
        e => format!("cannot reach the service at {url}: {e}"),
    })?;
    let status = response.status();
    if status != ureq::http::StatusCode::OK {
        return Err(format!("the service at {url} answered {status}"));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|e| format!("cannot read the service's answer at {url}: {e}"))?;
    match serde_json::from_str::<Health>(&body) {
        Ok(health) if health.status == "ok" => Ok(()),
        _ => {
            let shown: String = body.trim().chars().take(SHOWN).collect();
            let more = if body.trim().chars().count() > SHOWN {
                "…"
            } else {
                ""
            };
            Err(format!(
                "the service at {url} is not healthy: {shown:?}{more}"
            ))
        }
    }
}

pub struct Request<'a> {
    pub server: &'a str,
    pub set: String,
    pub revision: Option<String>,
    pub evidence: AuditEvidence,
    pub token_file: Option<&'a Path>,
}

struct Client {
    agent: ureq::Agent,
    base: String,
    authorization: String,
    timing: Timing,
}

impl Client {
    fn new(server: &str, token: &str, timing: Timing) -> Client {
        Client {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(timing.connect))
                .timeout_recv_response(Some(timing.response))
                .build()
                .into(),
            base: server.trim_end_matches('/').to_owned(),
            authorization: format!("Bearer {token}"),
            timing,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{PREFIX}{path}", self.base)
    }

    fn unreachable(&self, err: ureq::Error) -> String {
        format!("cannot reach the service at {}: {err}", self.base)
    }

    /// The body of a successful response, else the error the Service gave.
    fn check(
        &self,
        mut response: ureq::http::Response<ureq::Body>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let message = response
            .body_mut()
            .read_json::<ErrorBody>()
            .map(|b| b.error)
            .unwrap_or_else(|_| status.to_string());
        if status == ureq::http::StatusCode::UNAUTHORIZED {
            return Err(format!("the service refused the operator token: {message}"));
        }
        Err(message)
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self
            .agent
            .get(self.url(path))
            .header("Authorization", &self.authorization)
            .call()
            .map_err(|e| self.unreachable(e))?;
        self.check(response)?
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .map_err(|e| format!("{path}: {e}"))
    }

    /// Follows the Job's events until a final state, reconnecting if the stream ends early, and polling the Job's
    /// state instead once the stream stalls.
    fn follow(&self, mut job: Job) -> Result<Job, String> {
        let mut shown = None;
        let mut stalled = false;
        while !job.state.is_final() {
            if stalled {
                std::thread::sleep(self.timing.poll);
            } else if self.stream(&mut job, &mut shown)? == Stream::Stalled {
                eprintln!(
                    "job {}: no events from the service for {}s; polling its state instead",
                    job.id,
                    self.timing.stall.as_secs()
                );
                stalled = true;
            }
            if !job.state.is_final() {
                let next = self.get(&format!("/jobs/{}", job.id))?;
                update(&mut job, next);
            }
        }
        Ok(job)
    }

    /// Reads the Job's event stream until it ends, a final state arrives, or nothing, not even a keep-alive, has
    /// arrived for the stall time.
    fn stream(&self, job: &mut Job, shown: &mut Option<usize>) -> Result<Stream, String> {
        let response = self
            .agent
            .get(self.url(&format!("/jobs/{}/events", job.id)))
            .header("Authorization", &self.authorization)
            .header("Accept", "text/event-stream")
            .call()
            .map_err(|e| self.unreachable(e))?;
        let reader = std::io::BufReader::new(self.check(response)?.into_body().into_reader());
        // ureq cannot time out an idle read, so the stream is read on its own thread. A stalled reader is left
        // blocked; it ends with the process.
        let (tx, lines) = std::sync::mpsc::sync_channel(64);
        std::thread::spawn(move || {
            for line in reader.lines() {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            let line = match lines.recv_timeout(self.timing.stall) {
                Ok(Ok(line)) => line,
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return Ok(Stream::Ended),
                Err(RecvTimeoutError::Timeout) => return Ok(Stream::Stalled),
            };
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            match serde_json::from_str::<JobEvent>(data.trim_start()) {
                Ok(JobEvent::State(next)) => {
                    update(job, *next);
                    if job.state.is_final() {
                        return Ok(Stream::Ended);
                    }
                }
                Ok(JobEvent::Progress(progress)) => show_progress(progress, shown),
                Ok(JobEvent::Resync(_)) => {}
                Err(err) => return Err(format!("unexpected event from the service: {err}")),
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Stream {
    Ended,
    Stalled,
}

/// How long the client waits on the Service.
#[derive(Debug, Clone, Copy)]
struct Timing {
    connect: Duration,
    /// How long a response's status and headers may take.
    response: Duration,
    /// How long an event stream may go without sending anything before it counts as stalled.
    stall: Duration,
    /// How often a stalled Job's state is polled.
    poll: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            connect: Duration::from_secs(10),
            response: Duration::from_secs(60),
            stall: SSE_KEEP_ALIVE * 3,
            poll: Duration::from_secs(2),
        }
    }
}

/// Takes the Job's new state, showing it if it changed.
fn update(job: &mut Job, next: Job) {
    if next.state != job.state {
        eprintln!("job {}: {}", next.id, next.state.as_str());
    }
    *job = next;
}

/// Shows progress every tenth of the way.
fn show_progress(progress: JobProgress, shown: &mut Option<usize>) {
    let tenth = (progress.files_done * 10)
        .checked_div(progress.files_total)
        .unwrap_or(10);
    if *shown != Some(tenth) {
        *shown = Some(tenth);
        eprintln!(
            "job {}: {}/{} files read",
            progress.job, progress.files_done, progress.files_total
        );
    }
}

fn read_token(path: Option<&Path>) -> Result<String, String> {
    let path = path.ok_or(
        "no operator token: pass --token-file <file> or set TITLE_SELECT_OPERATOR_TOKEN_FILE",
    )?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(format!(
            "{}: the operator token file is empty",
            path.display()
        ));
    }
    Ok(token.to_owned())
}

/// Starts the Audit Job, follows it to completion and returns its report, showing progress and a summary on stderr.
pub fn audit(request: Request) -> Result<Report, String> {
    let token = read_token(request.token_file)?;
    let client = Client::new(request.server, &token, Timing::default());
    let start = StartAudit {
        source_set: request.set,
        revision: request.revision,
        evidence: request.evidence,
    };
    let response = client
        .agent
        .post(client.url("/audits"))
        .header("Authorization", &client.authorization)
        .send_json(&start)
        .map_err(|e| client.unreachable(e))?;
    let job: Job = client
        .check(response)?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    eprintln!(
        "job {}: {}: audit of {} against DAT revision {}",
        job.id,
        job.state.as_str(),
        job.source_set,
        job.dat_revision
    );
    let job = client.follow(job)?;
    if job.state != JobState::Succeeded {
        return Err(format!(
            "job {} {}: {}",
            job.id,
            job.state.as_str(),
            job.error.as_deref().unwrap_or("no reason given")
        ));
    }
    let audit = job.audit.ok_or("the job succeeded without an audit")?;
    let report: Report = client.get(&format!("/audits/{audit}/report"))?;
    let findings = Finding::ALL.iter().map(|f| {
        format!(
            "{}={}",
            f.as_str(),
            report.summary.findings.get(f).unwrap_or(&0)
        )
    });
    let games = GameStatus::ALL.iter().map(|g| {
        format!(
            "{}={}",
            g.as_str(),
            report.summary.games.get(g).unwrap_or(&0)
        )
    });
    let counts: Vec<String> = findings.chain(games).collect();
    eprintln!(
        "audit {audit} of {} against {} ({}): {}",
        job.source_set,
        report.dat_revision.sha256,
        report.dat_revision.version,
        counts.join(" ")
    );
    eprintln!("{}", crate::payloads_line(&report));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use title_select_api::Workers;

    fn job(state: JobState) -> Job {
        Job {
            id: 1,
            kind: Default::default(),
            state,
            source_set: "Test - System".into(),
            dat_revision: "0".repeat(64),
            actor: "Operator token".into(),
            attempt: 1,
            requeued_from: None,
            audit: None,
            profile: None,
            selection: None,
            managed_folder: None,
            ingest_folder: None,
            repair: None,
            cancel_requested_at: None,
            plan: None,
            evidence: None,
            error: None,
            workers: Workers {
                readers: 1,
                hashers: 1,
            },
            created_at: "now".into(),
            started_at: None,
            finished_at: None,
            version: 1,
        }
    }

    /// Answers one request: the Job's event stream sends Running and then stalls, holding the connection open;
    /// the Job itself has Succeeded.
    fn answer(mut stream: TcpStream) {
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
            request.push(byte[0]);
        }
        let request = String::from_utf8(request).unwrap();
        if request.starts_with("GET /api/v1/jobs/1/events ") {
            let running = serde_json::to_string(&job(JobState::Running)).unwrap();
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            write!(stream, "{head}event: state\ndata: {running}\n\n").unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        } else if request.starts_with("GET /api/v1/jobs/1 ") {
            let body = serde_json::to_string(&job(JobState::Succeeded)).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        } else {
            panic!("unexpected request {request}");
        }
    }

    #[test]
    fn a_stalled_event_stream_falls_back_to_polling_the_job() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                std::thread::spawn(move || answer(stream.unwrap()));
            }
        });
        let timing = Timing {
            stall: Duration::from_millis(300),
            poll: Duration::from_millis(10),
            ..Timing::default()
        };
        let client = Client::new(&server, "token", timing);

        let started = std::time::Instant::now();
        let done = client.follow(job(JobState::Queued)).unwrap();

        assert_eq!(done.state, JobState::Succeeded);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
