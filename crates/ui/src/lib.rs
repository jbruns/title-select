//! Title Select's workbench: DAT Revisions, Source Sets, Audits, Selection Profiles, Selections, Library Folders,
//! Change Plans, Builds and Jobs over the `/api/v1` JSON API (ADR 0005).
//!
//! `model`, `selection_model`, `build_model`, `registration` and `jobs` are pure, as is the logic of `parts`, and
//! tested on the host; the rest renders in the browser.

pub mod api;
pub mod app;
pub mod build_model;
pub mod events;
pub mod jobs;
pub mod model;
pub mod parts;
pub mod registration;
pub mod repair_model;
pub mod screens;
pub mod selection_model;
pub mod shell;
pub mod table;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    leptos::mount::mount_to_body(app::App);
}
