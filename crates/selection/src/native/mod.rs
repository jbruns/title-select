//! The built-in Selection engine: a port of the pinned, patched Retool's Selection to Rust.
#![allow(dead_code)]

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
mod settings;
mod titletools;
mod trace;

pub(crate) use pipeline::run;

#[cfg(test)]
mod tests;
