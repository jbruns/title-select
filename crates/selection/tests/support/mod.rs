//! Shared helpers for the Selection tests, which run the Retool install that `tools/retool/setup` builds.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use title_select_api::selection::SelectionProfile;
use title_select_selection::{Engine, RunOptions};

/// Retool's vendored tree, whose `tests/` folder holds the synthetic DATs, configs and goldens.
pub fn upstream() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/retool/upstream")
}

/// The install at `TITLE_SELECT_RETOOL`. A missing install fails the test: it never skips.
pub fn engine() -> Engine {
    Engine::locate(None).unwrap_or_else(|e| {
        panic!("{e}\nthe Selection tests need the Retool install: run tools/retool/setup")
    })
}

/// The install, reading Retool's test clone lists, metadata, MIAs and RetroAchievements.
pub fn fixture_engine() -> Engine {
    engine().with_data_dir(&upstream().join("tests"))
}

pub fn options(work_dir: &Path) -> RunOptions {
    RunOptions {
        timeout: Duration::from_secs(300),
        ..RunOptions::new(work_dir)
    }
}

/// A Selection Profile with the orders, overrides and post filters of one of Retool's test configs.
pub fn profile_from_upstream_config(name: &str) -> SelectionProfile {
    let path = upstream().join("tests/configs").join(name);
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

/// The titles in a DAT that Retool wrote in its legacy parent/clone form, each with the parent it is a clone of,
/// if any.
pub fn parent_clone(dat: &Path) -> Vec<(String, Option<String>)> {
    let text = std::fs::read_to_string(dat).unwrap();
    let mut titles = Vec::new();
    for tag in ["<game ", "<machine "] {
        for (start, _) in text.match_indices(tag) {
            let element = &text[start..start + text[start..].find('>').unwrap()];
            titles.push((
                attribute(element, "name").unwrap(),
                attribute(element, "cloneof"),
            ));
        }
    }
    titles.sort();
    titles
}

fn attribute(element: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let start = element.find(&needle)? + needle.len();
    let value = &element[start..start + element[start..].find('"')?];
    Some(
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
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
