//! Fetching newer DAT Revisions for Following Source Sets from the DAT Providers that permit it (ADR 0012).

mod http;

pub use http::{FetchError, Fetched, Http, USER_AGENT, refusal};
