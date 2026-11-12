//! Webhook notifications: one signed JSON POST per event, sent by one delivery task from a bounded queue, so that no
//! Job ever waits on the webhook. Job events come from [`crate::jobs::Jobs::changed`]; the rest from the store as each
//! write that makes one commits ([`title_select_store::Store::set_observer`]).

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use title_select_api::{Job, JobKind, JobState, SourceSetStale};
use title_select_store::config::{Notify as NotifyConfig, NotifyEvent};
use title_select_store::fetch::Http;
use title_select_store::observe::Observed;
use tokio::sync::mpsc;

use crate::StartError;

/// The environment variable naming the file that holds the webhook's HMAC secret.
pub const SECRET_FILE_ENV: &str = "TITLE_SELECT_WEBHOOK_SECRET_FILE";
/// The most events waiting for delivery; one more is dropped.
const QUEUE: usize = 256;
/// How many times one event is sent before it is dropped.
const ATTEMPTS: usize = 3;
/// The wait before each attempt after the first.
const BACKOFF: [Duration; ATTEMPTS - 1] = [Duration::from_secs(1), Duration::from_secs(4)];
/// The longest one attempt may take.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
/// How many ended Jobs are remembered, so that each is notified once.
const JOBS_REMEMBERED: usize = 1024;

/// What one POST carries.
#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    pub event: &'static str,
    pub at: String,
    pub subject: String,
    pub summary: String,
    pub link: Option<String>,
}

/// Turns what happens into webhook events, and queues them for delivery. Inert while no webhook is configured.
pub struct Notifier {
    inner: Option<Inner>,
}

struct Inner {
    events: HashSet<NotifyEvent>,
    workbench: Option<String>,
    queue: mpsc::Sender<Payload>,
    /// The ended Jobs already notified, oldest first.
    jobs: Mutex<(HashSet<i64>, VecDeque<i64>)>,
}

/// What the delivery task needs, until the Service starts it.
pub struct Delivery {
    url: String,
    secret: Vec<u8>,
    queue: mpsc::Receiver<Payload>,
}

impl Notifier {
    /// A Notifier that sends nothing.
    pub fn off() -> Notifier {
        Notifier { inner: None }
    }

    /// A Notifier for `config`, with the secret read from `secret_file`, and its delivery; inert without a URL. A URL
    /// with no secret file refuses to start.
    pub fn new(
        config: &NotifyConfig,
        secret_file: Option<&Path>,
    ) -> Result<(Notifier, Option<Delivery>), StartError> {
        let Some(url) = &config.url else {
            return Ok((Notifier::off(), None));
        };
        let secret_file = secret_file.ok_or_else(|| {
            StartError::Notify(format!(
                "[notify] url is set, so the webhook secret is needed: name its file with {SECRET_FILE_ENV} or \
                 --webhook-secret-file"
            ))
        })?;
        let secret = read_secret(secret_file)?;
        let (send, receive) = mpsc::channel(QUEUE);
        let notifier = Notifier {
            inner: Some(Inner {
                events: config.events.iter().copied().collect(),
                workbench: config.workbench_url.clone(),
                queue: send,
                jobs: Mutex::default(),
            }),
        };
        let delivery = Delivery {
            url: url.clone(),
            secret,
            queue: receive,
        };
        Ok((notifier, Some(delivery)))
    }

    /// Whether a webhook is configured.
    pub fn is_on(&self) -> bool {
        self.inner.is_some()
    }

    /// Notifies a Job's end, once.
    pub fn job_changed(&self, job: &Job) {
        let Some(inner) = &self.inner else { return };
        if !job.state.is_final() {
            return;
        }
        {
            let mut jobs = inner.jobs.lock().unwrap();
            if !jobs.0.insert(job.id) {
                return;
            }
            jobs.1.push_back(job.id);
            if jobs.1.len() > JOBS_REMEMBERED
                && let Some(old) = jobs.1.pop_front()
            {
                jobs.0.remove(&old);
            }
        }
        self.send(job_payload(job, inner.workbench.as_deref()));
    }

    /// Notifies what the store reports as it commits.
    pub fn observed(&self, observed: Observed) {
        let Some(inner) = &self.inner else { return };
        self.send(observed_payload(observed, inner.workbench.as_deref()));
    }

    /// Queues `payload` if its event was chosen, dropping it with a log line if the queue is full.
    fn send(&self, payload: Payload) {
        let Some(inner) = &self.inner else { return };
        let chosen = inner.events.iter().any(|e| e.as_str() == payload.event);
        if !chosen {
            return;
        }
        let event = payload.event;
        if inner.queue.try_send(payload).is_err() {
            eprintln!(
                "title-select: dropped a {event} notification: {QUEUE} are already waiting for your webhook"
            );
        }
    }
}

fn read_secret(path: &Path) -> Result<Vec<u8>, StartError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| StartError::Notify(format!("webhook secret file {}: {e}", path.display())))?;
    let secret = text.trim();
    if secret.is_empty() {
        return Err(StartError::Notify(format!(
            "webhook secret file {} is empty",
            path.display()
        )));
    }
    Ok(secret.as_bytes().to_vec())
}

/// Sends each queued event, signed, until the queue closes: up to [`ATTEMPTS`] times, then drops it with a log line.
pub async fn deliver(delivery: Delivery) {
    let Delivery {
        url,
        secret,
        mut queue,
    } = delivery;
    let http = Arc::new(Http::new());
    let url: Arc<str> = url.into();
    while let Some(payload) = queue.recv().await {
        let body: Arc<[u8]> = serde_json::to_vec(&payload)
            .expect("a payload serializes")
            .into();
        let signature: Arc<str> = format!("sha256={}", hex(&hmac_sha256(&secret, &body))).into();
        let mut why = String::new();
        let mut delivered = false;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(BACKOFF[attempt - 1]).await;
            }
            let (http, url, body, signature) =
                (http.clone(), url.clone(), body.clone(), signature.clone());
            let event = payload.event;
            let posted = tokio::task::spawn_blocking(move || {
                http.post(
                    &url,
                    &[
                        ("Content-Type", "application/json"),
                        ("X-Title-Select-Event", event),
                        ("X-Title-Select-Signature", &signature),
                    ],
                    &body,
                    ATTEMPT_TIMEOUT,
                )
            })
            .await;
            match posted {
                Ok(Ok(status)) if (200..300).contains(&status) => {
                    delivered = true;
                    break;
                }
                Ok(Ok(status)) => why = format!("it answered HTTP {status}"),
                Ok(Err(e)) => why = e,
                Err(e) => why = e.to_string(),
            }
        }
        if !delivered {
            eprintln!(
                "title-select: cannot deliver the {} notification to your webhook: {why}. Dropped it after {ATTEMPTS} \
                 attempts",
                payload.event
            );
        }
    }
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn link(workbench: Option<&str>, path: &str) -> Option<String> {
    workbench.map(|base| format!("{base}{path}"))
}

/// `text` percent-encoded for one path segment.
fn segment(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn kind_name(kind: JobKind) -> String {
    kind.as_str().replace('_', " ")
}

fn job_payload(job: &Job, workbench: Option<&str>) -> Payload {
    let of = if job.source_set.is_empty() {
        String::new()
    } else {
        format!(" of {}", job.source_set)
    };
    let what = format!("{} job {}{of}", kind_name(job.kind), job.id);
    let (event, summary, path) = match job.state {
        JobState::Succeeded => {
            let path = match job.audit {
                Some(audit) if job.kind == JobKind::Audit => format!("/audits/{audit}"),
                _ => "/activity".into(),
            };
            (
                NotifyEvent::JobFinished,
                format!("The {what} finished."),
                path,
            )
        }
        _ => {
            let why = job
                .error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            let ended = if job.state == JobState::Interrupted {
                "was interrupted"
            } else {
                "failed"
            };
            (
                NotifyEvent::JobFailed,
                format!("The {what} {ended}{why}"),
                "/activity?state=failed".into(),
            )
        }
    };
    Payload {
        event: event.as_str(),
        at: job.finished_at.clone().unwrap_or_else(now),
        subject: format!("job {}", job.id),
        summary,
        link: link(workbench, &path),
    }
}

fn observed_payload(observed: Observed, workbench: Option<&str>) -> Payload {
    let (event, subject, summary, path) = match observed {
        Observed::DatRevision { dat, version } => (
            NotifyEvent::DatRevisionNew,
            dat.clone(),
            format!("DAT revision {version} of {dat} was imported."),
            "/dats".to_owned(),
        ),
        Observed::FetchFailed {
            provider,
            system,
            artifact,
            error,
        } => (
            NotifyEvent::FetchFailed,
            format!("{provider}/{system}"),
            format!("Fetching the {artifact} of {system} from {provider} failed: {error}"),
            "/dats".to_owned(),
        ),
        Observed::Stale {
            source_set,
            reasons,
        } => {
            let why: Vec<String> = reasons
                .iter()
                .map(|r| match r {
                    SourceSetStale::FilesChanged {
                        changed,
                        added,
                        removed,
                        ..
                    } => format!(
                        "files changed since its latest audit ({changed} changed, {added} added, {removed} removed)"
                    ),
                    SourceSetStale::NewerRevision { version, .. } => {
                        format!("DAT revision {version} is newer than its latest audit's")
                    }
                })
                .collect();
            (
                NotifyEvent::SourceSetStale,
                source_set.clone(),
                format!(
                    "Source set {source_set} is stale: {}. Audit it again.",
                    why.join("; ")
                ),
                format!("/source-sets/{}", segment(&source_set)),
            )
        }
        Observed::Drift {
            folder,
            root,
            path,
            drift,
        } => {
            let entries = if drift == 1 { "entry" } else { "entries" };
            (
                NotifyEvent::DriftFound,
                format!("{root}/{path}"),
                format!(
                    "A check of play library folder {root}/{path} found {drift} {entries} Title Select did not build."
                ),
                format!("/libraries/{folder}"),
            )
        }
    };
    Payload {
        event: event.as_str(),
        at: now(),
        subject,
        summary,
        link: link(workbench, &path),
    }
}

/// HMAC-SHA256 (RFC 2104) of `message` under `key`.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test cases 2 and 6: a short key, and a key longer than a block.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
