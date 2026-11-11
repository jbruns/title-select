//! `process_dat` and the data files Retool reads for a DAT: its clone list, metadata, MIAs and
//! RetroAchievements lists.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;

use super::dat::{self, Dat, TitleData};
use super::json::J;
use super::node::{Groups, Run, Titles};
use super::py::Crash;
use super::pyre::Re;
use crate::Error;

/// A clone list's contents (`CloneList`).
#[derive(Debug, Clone, Default)]
pub(crate) struct CloneList {
    pub mias: Vec<J>,
    pub retroachievements: Vec<J>,
    pub variants: Vec<J>,
}

/// Whether `path` is a file under exactly its name. A case-insensitive file system (macOS, Windows) would also
/// open a file whose name differs only in case, which Linux, where the service runs, doesn't: so a Selection
/// reads the same data files everywhere.
fn exactly_a_file(path: &Path) -> bool {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    path.is_file()
        && std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .any(|entry| entry.is_ok_and(|e| e.file_name() == name))
}

/// `load_data`: a JSON file's contents, or an empty object when there's no file.
fn load_data(path: &Path, kind: &str) -> Result<J, Error> {
    if !exactly_a_file(path) {
        return Ok(J::Dict(IndexMap::new()));
    }
    let bytes =
        std::fs::read(path).map_err(|e| Error::Builtin(format!("{}: {e}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|_| {
        Error::Builtin(format!(
            "the {kind} {} contains invalid JSON",
            path.display()
        ))
    })
}

fn list(data: &J, key: &str) -> Vec<J> {
    match data {
        J::Dict(d) => match d.get(key) {
            Some(J::List(l)) => l.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Retool's version, which a clone list's `minimumVersion` is checked against.
const VERSION: (i64, i64, i64) = (2, 4, 9);

/// `import_clone_list_mia_ra`.
pub(crate) fn import_clone_list(dat: &Dat, data: &Path) -> Result<CloneList, Error> {
    let name = dat.search_name.replace(" (MAME Redump)", " (Redump)");
    let clone_file = data.join("clonelists").join(format!("{name}.json"));
    let clone_data = load_data(&clone_file, "clone list")?;
    let variants = list(&clone_data, "variants");
    if let Some(min) = clone_data
        .get("description")
        .and_then(|d| d.get("minimumVersion"))
    {
        let mut min = min.py_str();
        if min.matches('.').count() < 2 {
            min = format!("{min}.0");
        }
        let parts: Vec<i64> = min
            .split('.')
            .take(3)
            .map(|p| super::py::int(p).unwrap_or(0))
            .collect();
        if parts.len() == 3 && (parts[0], parts[1], parts[2]) > VERSION {
            return Err(Error::Builtin(format!(
                "the clone list {} needs a newer selection engine ({min})",
                clone_file.display()
            )));
        }
    }
    let mia_file = data.join("mias").join(format!("{}.json", dat.search_name));
    let mias = list(&load_data(&mia_file, "MIA file")?, "mias");
    let ra_name = dat
        .search_name
        .replace("Non-Redump - ", "")
        .replace(" (No-Intro)", "")
        .replace(" (Redump)", "")
        .replace(" (MAME Redump)", "");
    let ra_dir = data.join("retroachievements");
    let mut retroachievements = list(
        &load_data(
            &ra_dir.join(format!("{ra_name}.json")),
            "RetroAchievements file",
        )?,
        "retroachievements",
    );
    let chd = ra_dir.join(format!("{ra_name} (CHD).json"));
    if exactly_a_file(&chd) {
        retroachievements.extend(list(
            &load_data(&chd, "RetroAchievements CHD file")?,
            "retroachievements",
        ));
    }
    Ok(CloneList {
        mias,
        retroachievements,
        variants,
    })
}

/// `import_metadata`.
pub(crate) fn import_metadata(dat: &Dat, data: &Path) -> Result<IndexMap<String, J>, Error> {
    let name = dat.search_name.replace(" (MAME Redump)", " (Redump)");
    let file = data.join("metadata").join(format!("{name}.json"));
    Ok(match load_data(&file, "metadata file")? {
        J::Dict(d) => d,
        _ => IndexMap::new(),
    })
}

/// `regex_test` for the Operator's filters: drops the `/regex` entries that don't compile, with a warning.
fn regex_test(list: Vec<String>, origin: &str, post_filter: bool) -> Result<Vec<String>, Error> {
    // `patterns::check` refuses a Profile whose patterns don't compile before the engine runs, so a pattern
    // here compiles; one the engine can't read as Python's `re` does stops the run rather than being
    // skipped, which would change the rules.
    for item in &list {
        let wrapped = item
            .strip_prefix('<')
            .and_then(|i| i.strip_suffix('>'))
            .filter(|_| !post_filter);
        if let Some(pattern) = wrapped.unwrap_or(item).strip_prefix('/')
            && let Err(reason) = Re::user(pattern)
        {
            return Err(Error::Unsupported(format!(
                "the regular expression {item:?} in your {origin}s ({reason})"
            )));
        }
    }
    Ok(list)
}

/// What `process_dat` hands the rest of the run.
pub(crate) struct Processed {
    pub dat: Dat,
    pub clone_list: CloneList,
    pub titles: Groups,
}

/// `process_dat`.
pub(crate) fn process_dat(
    run: &mut Run,
    path: &Path,
    dat: Dat,
    data: &Path,
    no_overrides: bool,
) -> Result<Processed, Error> {
    let mut dat = dat;
    let text = read_text(path)?;
    let titles: Vec<TitleData> = if dat.is_clrmamepro {
        dat::clrmamepro_titles(&text, &dat)
    } else {
        dat::logiqx_titles(&text)
    };
    if dat.url.to_lowercase().contains("no-intro") {
        let numbered = Re::of("^([0-9]|x|z)([0-9]|B)[0-9]{2,2} - ");
        dat.numbered = titles.iter().all(|t| numbered.search(&t.name));
    }
    run.numbered = dat.numbered;
    if titles.is_empty() {
        return Err(Error::Builtin(format!(
            "{}: the DAT has no titles with a size or a digest",
            path.display()
        )));
    }
    if !no_overrides {
        let s = &run.s;
        let (ge, gi, se, si) = (
            s.global_exclude.clone(),
            s.global_include.clone(),
            s.system_exclude.clone(),
            s.system_include.clone(),
        );
        run.s.global_exclude = regex_test(ge, "global exclude", false)?;
        run.s.global_include = regex_test(gi, "global include", false)?;
        run.s.system_exclude = regex_test(se, "system exclude", false)?;
        run.s.system_include = regex_test(si, "system include", false)?;
    }
    if !run.s.global_filter.is_empty() {
        let list = run.s.global_filter.clone();
        run.s.global_filter = regex_test(list, "global post filter", true)?;
    }
    if !run.s.system_filter.is_empty() {
        let list = run.s.system_filter.clone();
        run.s.system_filter = regex_test(list, "system post filter", true)?;
    }
    let clone_list = import_clone_list(&dat, data)?;
    let metadata = import_metadata(&dat, data)?;
    run.trace.run(
        &dat.search_name,
        !clone_list.variants.is_empty(),
        metadata.keys().cloned().collect::<HashSet<String>>(),
    );
    let mut process_list = Vec::with_capacity(titles.len());
    for title in &titles {
        run.limits.check().map_err(crash)?;
        let node = run.node(dat.numbered, &metadata, title).map_err(crash)?;
        process_list.push(run.add(node));
    }
    let mut names: Vec<(String, String)> = process_list
        .iter()
        .zip(&titles)
        .map(|(&id, t)| (run.n(id).full_name.clone(), t.name.clone()))
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let names: IndexMap<String, J> = names
        .into_iter()
        .filter(|(full, name)| full != name)
        .map(|(full, name)| (full, J::Str(name)))
        .collect();
    run.trace.write(super::json::dict([
        ("type", "dat_names".into()),
        ("names", J::Dict(names)),
    ]));

    let mut groups = Groups::new();
    let mut duplicates = Titles::new();
    let mut duplicate_names: Vec<String> = Vec::new();
    let dupe = Re::of(" \\(Dupe \\d+\\)");
    let digits = Re::of("\\d+");
    for &id in &process_list {
        run.limits.check().map_err(crash)?;
        let group = run.n(id).group_name.clone();
        let existing: Vec<usize> = groups
            .entry(group.clone())
            .or_default()
            .iter()
            .copied()
            .collect();
        for other in existing {
            if run.n(other).full_name != run.n(id).full_name {
                continue;
            }
            if run.n(other).roms == run.n(id).roms {
                duplicates.insert(id);
                continue;
            }
            let mut number: i64 = 0;
            let node = run.m(id);
            if dupe.search(&node.full_name) {
                let found = dupe.find(&node.full_name);
                number = super::py::int(&digits.find(&found)).map_err(crash)? + 1;
                node.full_name = dupe.sub("", &node.full_name);
                node.description = dupe.sub("", &node.description);
                node.short_name = dupe.sub("", &node.short_name);
                node.region_free_name = dupe.sub("", &node.region_free_name);
            } else {
                number += 1;
            }
            let before = node.full_name.clone();
            node.full_name = format!("{} (Dupe {number})", node.full_name);
            node.description = format!("{} (Dupe {number})", node.description);
            node.short_name = format!("{} (Dupe {number})", node.short_name);
            node.region_free_name = format!("{} (Dupe {number})", node.region_free_name);
            // With Retool's colours, `Font.warning_bold` and `Font.warning`, which sort before any printable text.
            let entry = format!(
                "\x1b[1m\x1b[93m{before}\x1b[0m\x1b[93m -> \x1b[1m\x1b[93m{}\x1b[0m\x1b[93m",
                node.full_name
            );
            if !duplicate_names.contains(&entry) {
                duplicate_names.push(entry);
            }
        }
        groups.get_mut(&group).expect("group").insert(id);
    }
    for id in duplicates {
        run.limits.check().map_err(crash)?;
        let group = run.n(id).group_name.clone();
        if let Some(titles) = groups.get_mut(&group) {
            titles.shift_remove(&id);
        }
    }
    if !duplicate_names.is_empty() {
        duplicate_names.sort();
        run.trace.warn("• The following titles with identical names were found in the input DAT file, and will be renamed. They won't be processed as clones:\n");
        for name in duplicate_names {
            run.trace.warn_unwrapped(&format!("  •  {name}"));
        }
    }
    Ok(Processed {
        dat,
        clone_list,
        titles: groups,
    })
}

pub(crate) fn crash(c: Crash) -> Error {
    match c.0.as_str() {
        super::CANCELLED => Error::Cancelled,
        super::TIMED_OUT => Error::Timeout(Default::default()),
        _ => Error::Builtin(format!("stopped: {}", c.0)),
    }
}

pub(crate) fn read_text(path: &Path) -> Result<String, Error> {
    let bytes =
        std::fs::read(path).map_err(|e| Error::Builtin(format!("{}: {e}", path.display())))?;
    String::from_utf8(bytes).map_err(|_| Error::Builtin(format!("{} isn't UTF-8", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a case-insensitive file system `Retool - Post filters.json` would open `Retool - Post Filters.json`;
    /// on Linux, where the service runs, it wouldn't. Every platform reads only the exact name.
    #[test]
    fn a_data_file_is_found_only_under_its_exact_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Some System.json"), r#"{"a": 1}"#).unwrap();
        assert!(matches!(
            load_data(&dir.path().join("Some System.json"), "clone list").unwrap(),
            J::Dict(d) if d.len() == 1
        ));
        assert!(matches!(
            load_data(&dir.path().join("Some system.json"), "clone list").unwrap(),
            J::Dict(d) if d.is_empty()
        ));
        assert!(!exactly_a_file(&dir.path().join("some system.json")));
    }
}
