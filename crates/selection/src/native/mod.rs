//! The built-in Selection engine: a port of the pinned, patched Retool's Selection to Rust.
#![allow(dead_code)]

mod py;
mod pyre;
mod pyset;
mod regexes;
mod settings;
mod titletools;

#[cfg(test)]
mod tests;
