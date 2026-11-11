//! The built-in Selection engine: a port of the pinned, patched Retool's Selection to Rust.
#![allow(dead_code)]

mod choose;
mod clone_lists;
mod dat;
mod filters;
mod json;
mod node;
mod overrides;
mod pipeline;
mod process;
mod py;
mod pyre;
mod pyset;
mod refs;
mod regexes;
mod select;
mod settings;
mod titletools;
mod trace;
mod userre;

pub(crate) use pipeline::run;

/// Reads one of the Operator's `/pattern` lines as Python's `re` would, or says why the engine can't.
pub(crate) fn user_regex(pattern: &str) -> Result<(), String> {
    pyre::Re::user(pattern).map(|_| ())
}

/// The system `dat` is for, as its header names it, or `None` for a DAT the engine can't read.
pub(crate) fn system_name(dat: &std::path::Path, internal_config: &[u8]) -> Option<String> {
    let text = process::read_text(dat).ok()?;
    let settings = settings::Settings::new(
        internal_config,
        &title_select_api::selection::SelectionProfile::default(),
        "",
    )
    .ok()?;
    let header = dat::read_header(dat, &text, &settings).ok()?;
    Some(header.search_name).filter(|name| !name.is_empty())
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// When a run of the built-in engine must stop: the caller cancelled it, or it ran out of time.
#[derive(Debug, Clone, Default)]
pub(crate) struct Limits {
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<(Instant, Duration)>,
}

/// The `Crash` messages that stand for a stop rather than an error.
const CANCELLED: &str = "\0cancelled";
const TIMED_OUT: &str = "\0timed out";

impl Limits {
    /// Fails when the run must stop.
    pub fn check(&self) -> py::PyResult<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(py::Crash(CANCELLED.into()));
        }
        if self.deadline.is_some_and(|(at, _)| Instant::now() >= at) {
            return Err(py::Crash(TIMED_OUT.into()));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
