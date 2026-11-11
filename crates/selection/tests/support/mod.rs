//! Shared helpers for the Selection tests, which read the clone lists that `tools/clone-lists/fetch` unpacks.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use title_select_api::selection::SelectionProfile;
use title_select_selection::{Engine, RunOptions};

/// Retool's test inputs: DATs under `source/`, configs, clone lists, metadata, MIAs and RetroAchievements, and the
/// internal config they run under (`config/`).
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/retool/fixtures")
}

/// The engine with the clone lists at `TITLE_SELECT_CLONE_LISTS`. Missing clone lists fail the test: it never
/// skips.
pub fn engine() -> Engine {
    Engine::locate(None).unwrap_or_else(|e| {
        panic!("{e}\nthe Selection tests need the clone lists: run tools/clone-lists/fetch")
    })
}

/// The engine reading Retool's test clone lists, metadata, MIAs and RetroAchievements, under the internal config
/// frozen beside them, so that a clone-list bump doesn't change what the fixtures select.
pub fn fixture_engine() -> Engine {
    Engine::unverified(&fixtures()).unwrap()
}

pub fn options() -> RunOptions {
    RunOptions {
        timeout: Duration::from_secs(300),
        ..RunOptions::new()
    }
}

/// A Selection Profile with the orders, overrides and post filters of one of Retool's test configs.
pub fn profile_from_upstream_config(name: &str) -> SelectionProfile {
    let path = fixtures().join("configs").join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut profile = SelectionProfile::default();
    let mut key = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') || line == "---" {
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            let item = if item.starts_with('"') {
                serde_json::from_str::<String>(item).unwrap()
            } else {
                item.to_owned()
            };
            let list = match key.as_str() {
                "language order" => &mut profile.language_order,
                "region order" => &mut profile.region_order,
                "localization order" => &mut profile.localization_order,
                "video order" => &mut profile.video_order,
                "exclude" => &mut profile.overrides.exclude,
                "include" => &mut profile.overrides.include,
                "filters" => &mut profile.post_filters,
                _ => continue,
            };
            list.push(item);
        } else if let Some((name, _)) = line.split_once(':') {
            key = name.to_owned();
        }
    }
    profile
}

/// Whether to rewrite goldens instead of comparing against them.
pub fn regenerating() -> bool {
    std::env::var_os("TITLE_SELECT_REGENERATE_GOLDENS").is_some()
}

/// Compares `actual` with the committed golden at `tests/goldens/<name>`, or rewrites it when regenerating.
pub fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name);
    if regenerating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nregenerate with TITLE_SELECT_REGENERATE_GOLDENS=1",
            path.display()
        )
    });
    if expected != actual {
        let first = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .unwrap_or(expected.lines().count().min(actual.lines().count()));
        panic!(
            "{} differs from the golden from line {}:\n  golden: {:?}\n  actual: {:?}\n\
             if the change is intended, regenerate with TITLE_SELECT_REGENERATE_GOLDENS=1",
            path.display(),
            first + 1,
            expected.lines().nth(first),
            actual.lines().nth(first),
        );
    }
}
