//! The only HTTP client in Title Select. Every request, and every redirect it would follow, is checked first against
//! the hosts Title Select must never address: No-Intro's (ADR 0012).

use std::time::Duration;

use ureq::http::Uri;

/// The User-Agent every request sends, naming Title Select and how it fetches.
pub const USER_AGENT: &str = concat!(
    "title-select/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/jbruns/title-select)"
);

/// The domains no request may address, nor any of their subdomains. No-Intro sanctions no automated access.
const FORBIDDEN_DOMAINS: &[&str] = &["no-intro.org"];
/// The most redirects one request follows.
const MAX_REDIRECTS: usize = 5;
/// The longest one request may take, body included.
const TIMEOUT: Duration = Duration::from_secs(600);

/// Why a request failed. A failure is never retried in the same fetch.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The URL addresses a host Title Select never contacts, or isn't one it may request.
    #[error("{0}")]
    Forbidden(String),
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: the response is larger than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("{url}: {message}")]
    Transport { url: String, message: String },
}

/// A response body, with the URL that served it and the file name it gave, if any.
#[derive(Debug)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub url: String,
    pub file_name: Option<String>,
}

/// Why Title Select refuses to request `url`, if it does: it is not plain `http` or `https` to a named host, carries
/// credentials, or addresses a No-Intro host.
pub fn refusal(url: &str) -> Option<String> {
    let Ok(uri) = url.parse::<Uri>() else {
        return Some(format!("{url} is not a URL Title Select can request"));
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return Some(format!("{url} is not an http or https URL"));
    }
    let Some(authority) = uri.authority() else {
        return Some(format!("{url} names no host"));
    };
    if authority.as_str().contains('@') {
        return Some(format!("{url} carries credentials"));
    }
    let host = authority.host().trim_end_matches('.').to_ascii_lowercase();
    let forbidden = FORBIDDEN_DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    forbidden.then(|| {
        format!(
            "{host} is a No-Intro host, and Title Select never sends it any request. Drop No-Intro DATs into an \
             ingest folder instead"
        )
    })
}

/// A blocking HTTP client that follows redirects itself, so that it checks each hop with [`refusal`].
pub struct Http {
    agent: ureq::Agent,
}

impl Default for Http {
    fn default() -> Self {
        Http::new()
    }
}

impl Http {
    pub fn new() -> Http {
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .timeout_global(Some(TIMEOUT))
            .build();
        Http {
            agent: config.into(),
        }
    }

    /// Gets `url`, with `accept` as its Accept header if given, reading at most `limit` bytes of body.
    pub fn get(&self, url: &str, accept: Option<&str>, limit: u64) -> Result<Fetched, FetchError> {
        self.get_paced(url, accept, limit, &mut || {})
    }

    /// As [`Http::get`], running `pace` before each request it sends, redirects included.
    pub fn get_paced(
        &self,
        url: &str,
        accept: Option<&str>,
        limit: u64,
        pace: &mut dyn FnMut(),
    ) -> Result<Fetched, FetchError> {
        let mut url = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            if let Some(why) = refusal(&url) {
                return Err(FetchError::Forbidden(why));
            }
            pace();
            let transport = |e: ureq::Error| FetchError::Transport {
                url: url.clone(),
                message: e.to_string(),
            };
            let mut request = self.agent.get(&url);
            if let Some(accept) = accept {
                request = request.header("Accept", accept);
            }
            let mut response = request.call().map_err(transport)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| FetchError::Status {
                        url: url.clone(),
                        status,
                    })?;
                url = resolve(&url, location).ok_or_else(|| FetchError::Transport {
                    url: url.clone(),
                    message: format!("it redirects to {location}, which is not a URL"),
                })?;
                continue;
            }
            if status != 200 {
                return Err(FetchError::Status { url, status });
            }
            let file_name = response
                .headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok())
                .and_then(disposition_file_name);
            let read = response.body_mut().with_config().limit(limit).read_to_vec();
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(ureq::Error::BodyExceedsLimit(_)) => {
                    return Err(FetchError::TooLarge { url, limit });
                }
                Err(e) => return Err(transport(e)),
            };
            return Ok(Fetched {
                bytes,
                url,
                file_name,
            });
        }
        Err(FetchError::Transport {
            url,
            message: format!("more than {MAX_REDIRECTS} redirects"),
        })
    }
}

/// The URL a redirect's `location` names, relative to `from`.
fn resolve(from: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_owned());
    }
    let from = from.parse::<Uri>().ok()?;
    let base = format!("{}://{}", from.scheme_str()?, from.authority()?);
    location
        .starts_with('/')
        .then(|| format!("{base}{location}"))
}

/// The file name a `Content-Disposition` header gives, without any directory.
fn disposition_file_name(header: &str) -> Option<String> {
    let value = header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("filename="))?;
    let name = value.trim_matches('"');
    let name = name.rsplit(['/', '\\']).next()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}
