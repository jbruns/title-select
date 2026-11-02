//! Requests to the Service's `/api/v1`, same-origin, so the trusted proxy's identity header or the loopback
//! `dev_identity` authenticates them and no token is kept in the browser.

use gloo_net::http::{Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use title_select_api::{ErrorBody, GameRow, PREFIX, Page};

pub fn url(path: &str) -> String {
    format!("{PREFIX}{path}")
}

/// Escapes a name, such as a Source Set's, for use as one path segment.
pub fn segment(name: &str) -> String {
    String::from(js_sys::encode_uri_component(name))
}

async fn checked(response: Response) -> Result<Response, String> {
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    match response.json::<ErrorBody>().await {
        Ok(body) => Err(body.error),
        Err(_) => Err(format!("The service answered {status}.")),
    }
}

fn failed(err: gloo_net::Error) -> String {
    format!("The service could not be reached: {err}")
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let response = Request::get(&url(path)).send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn text(path: &str) -> Result<String, String> {
    let response = Request::get(&url(path)).send().await.map_err(failed)?;
    checked(response)
        .await?
        .text()
        .await
        .map_err(|e| e.to_string())
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    post_until(path, body, None).await
}

/// Like `post`, but `signal` aborts the request, which closes the connection so the Service stops the work.
pub async fn post_until<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<T, String> {
    let request = Request::post(&url(path))
        .abort_signal(signal)
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn patch<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    let request = Request::patch(&url(path))
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    let request = Request::put(&url(path))
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn post_form<T: DeserializeOwned>(
    path: &str,
    form: web_sys::FormData,
) -> Result<T, String> {
    let request = Request::post(&url(path))
        .body(form)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Every Game Status of an Audit, a page of 1000 at a time.
pub async fn games(audit: i64) -> Result<Vec<GameRow>, String> {
    let mut games = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut path = format!("/audits/{audit}/games?limit=1000");
        if let Some(cursor) = &cursor {
            path.push_str("&cursor=");
            path.push_str(&segment(cursor));
        }
        let page: Page<GameRow> = get(&path).await?;
        games.extend(page.items);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(games),
        }
    }
}
