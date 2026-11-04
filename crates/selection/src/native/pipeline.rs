//! A whole run, in `retool.py`'s order, and the trace records it ends with (`TraceTools.json_final`).

use std::path::Path;

use indexmap::IndexMap;
use title_select_api::selection::SelectionProfile;

use super::Limits;
use super::clone_lists::{self, Lookup};
use super::dat;
use super::filters;
use super::json::{J, dict};
use super::node::{Groups, Run};
use super::overrides;
use super::process::{self, crash};
use super::py::PyResult;
use super::select;
use super::settings::Settings;
use crate::Error;

/// What a run of the built-in engine leaves: the trace, and the warnings it gave.
pub(crate) struct Ran {
    pub trace: String,
    pub warnings: Vec<String>,
}

/// Runs the Selection over `dat_path` with `profile`. `internal_config` is Retool's `internal-config.json`, and
/// `data` the folder with the `clonelists`, `metadata`, `mias` and `retroachievements` folders.
pub(crate) fn run(
    dat_path: &Path,
    profile: &SelectionProfile,
    internal_config: &[u8],
    data: &Path,
    limits: &Limits,
) -> Result<Ran, Error> {
    let stop = |e: Error| match e {
        Error::Timeout(_) => Error::Timeout(limits.deadline.map(|(_, t)| t).unwrap_or_default()),
        e => e,
    };
    run_within(dat_path, profile, internal_config, data, limits).map_err(stop)
}

fn run_within(
    dat_path: &Path,
    profile: &SelectionProfile,
    internal_config: &[u8],
    data: &Path,
    limits: &Limits,
) -> Result<Ran, Error> {
    let text = process::read_text(dat_path)?;
    let header = dat::read_header(
        dat_path,
        &text,
        &Settings::new(internal_config, profile, "")?,
    )?;
    let settings = Settings::new(internal_config, profile, &header.search_name)?;
    let dat = dat::read_header(dat_path, &text, &settings)?;
    let mut run = Run::new(settings);
    run.limits = limits.clone();
    let processed = process::process_dat(&mut run, dat_path, dat, data, false)?;
    run.limits.check().map_err(crash)?;
    let mut clone_list = processed.clone_list;
    let mut groups = processed.titles;
    let lookup = Lookup::new(&run, &groups);
    record_titles(&mut run, &groups);

    run.trace
        .context(Some("MIAs and RetroAchievements"), "", "");
    if run.s.options.label_mia {
        clone_lists::mias(&mut run, &lookup, &clone_list).map_err(crash)?;
    }
    if run.s.options.label_retro {
        clone_lists::retroachievements(&mut run, &lookup, &clone_list).map_err(crash)?;
    }
    run.trace.context(Some("Clone lists"), "", "");
    if !run.s.options.no_1g1r {
        clone_lists::variants_orchestrator(&mut run, &mut groups, &lookup, &mut clone_list, false)
            .map_err(crash)?;
    }
    run.trace.context(Some("Exclusions"), "", "");
    overrides::excludes(&mut run, &mut groups).map_err(crash)?;
    run.trace.context(Some("Language filter"), "", "");
    if run.s.options.filter_languages {
        filters::filter_languages(&mut run, &mut groups).map_err(crash)?;
    }
    filters::filter_regions(&mut run, &mut groups).map_err(crash)?;
    run.trace.context(Some("Parent selection"), "", "");
    if !run.s.options.no_1g1r {
        groups = select::orchestrator(&mut run, groups).map_err(crash)?;
    }
    run.trace.context(Some("Includes"), "", "");
    if !run.s.global_include.is_empty() || !run.s.system_include.is_empty() {
        overrides::includes(&mut run, &mut groups, &lookup).map_err(crash)?;
    }
    run.trace.context(Some("Post filters"), "", "");
    if !run.s.global_filter.is_empty()
        || !run.s.system_filter.is_empty()
        || run.s.system_filter_override.is_some()
    {
        overrides::post_filters(&mut run, &mut groups).map_err(crash)?;
    }
    json_final(&mut run, &groups).map_err(crash)?;
    if groups.values().all(|titles| titles.is_empty()) {
        run.trace.warn_error(
            "• No titles in the input DAT match your preferences. No DAT file has been created.",
        );
    }
    run.limits.check().map_err(crash)?;
    let (trace, warnings) = run.trace.finish();
    Ok(Ran { trace, warnings })
}

/// `TraceTools.json_record_titles`.
fn record_titles(run: &mut Run, groups: &Groups) {
    for (group, titles) in groups {
        for &title in titles {
            let name = run.n(title).full_name.clone();
            run.trace.seen.entry(name).or_insert_with(|| group.clone());
        }
    }
}

/// `TraceTools.json_final`: each group's parents and clones, every title the run didn't keep and why, and
/// what the clone list and metadata didn't cover.
fn json_final(run: &mut Run, groups: &Groups) -> PyResult<()> {
    let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut sorted_groups: Vec<(&String, _)> = groups.iter().collect();
    sorted_groups.sort_by(|a, b| a.0.cmp(b.0));
    for (group, titles) in sorted_groups {
        run.limits.check()?;
        let mut numbered: IndexMap<String, String> = IndexMap::new();
        for &t in titles {
            let n = run.n(t);
            numbered.insert(n.numbered_name.clone(), n.full_name.clone());
        }
        kept.extend(titles.iter().map(|&t| run.n(t).full_name.clone()));
        let mut parents: Vec<String> = titles
            .iter()
            .filter(|&&t| run.n(t).cloneof.is_empty())
            .map(|&t| run.n(t).full_name.clone())
            .collect();
        parents.sort();
        let mut by_name: Vec<usize> = titles.iter().copied().collect();
        by_name.sort_by(|&a, &b| run.n(a).full_name.cmp(&run.n(b).full_name));
        let mut clones: IndexMap<String, J> = IndexMap::new();
        for t in by_name {
            let n = run.n(t);
            if n.cloneof.is_empty() {
                continue;
            }
            let parent = numbered
                .get(&n.cloneof)
                .cloned()
                .unwrap_or_else(|| n.cloneof.clone());
            clones.insert(n.full_name.clone(), J::Str(parent));
        }
        let record = dict([
            ("type", "group".into()),
            ("group", group.as_str().into()),
            ("parents", parents.into()),
            ("clones", J::Dict(clones)),
        ]);
        run.trace.write(record);
    }
    let mut excluded: IndexMap<String, J> = IndexMap::new();
    let mut lists: Vec<(&str, Vec<usize>)> = run
        .removes
        .iter()
        .map(|(k, v)| (*k, v.iter().copied().collect()))
        .collect();
    lists.sort_by(|a, b| a.0.cmp(b.0));
    for (list, mut titles) in lists {
        run.limits.check()?;
        titles.sort_by(|&a, &b| run.n(a).full_name.cmp(&run.n(b).full_name));
        for t in titles {
            let n = run.n(t);
            if kept.contains(&n.full_name) || excluded.contains_key(&n.full_name) {
                continue;
            }
            excluded.insert(
                n.full_name.clone(),
                dict([
                    ("type", "excluded".into()),
                    ("title", n.full_name.as_str().into()),
                    ("group", n.group_name.as_str().into()),
                    ("list", list.into()),
                    ("reason", n.exclude_reason.as_str().into()),
                ]),
            );
        }
    }
    let mut seen: Vec<(String, String)> = run
        .trace
        .seen
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    seen.sort();
    for (name, group) in seen {
        run.limits.check()?;
        if kept.contains(&name) || excluded.contains_key(&name) {
            continue;
        }
        excluded.insert(
            name.clone(),
            dict([
                ("type", "excluded".into()),
                ("title", name.into()),
                ("group", group.into()),
                ("list", J::Null),
                ("reason", "".into()),
            ]),
        );
    }
    for (_, record) in excluded {
        run.limits.check()?;
        run.trace.write(record);
    }
    let mut unmatched = run.trace.clone_list_unmatched.clone();
    unmatched.sort();
    let mut missing: Vec<String> = run
        .trace
        .seen
        .keys()
        .filter(|x| !run.trace.metadata.contains(*x))
        .cloned()
        .collect();
    missing.sort();
    run.trace.write(dict([
        ("type", "coverage".into()),
        ("clone_list_unmatched", unmatched.into()),
        ("metadata_missing", missing.into()),
    ]));
    run.trace.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::native::py;
    use crate::native::tests::{internal_config, upstream};

    fn fixture(name: &str) -> std::path::PathBuf {
        upstream().join("tests/source/features").join(name)
    }

    fn run_with(profile: &SelectionProfile, limits: &Limits) -> Result<Ran, Error> {
        run(
            &fixture("Retool - Prefer regions.dat"),
            profile,
            &internal_config(),
            &upstream().join("tests"),
            limits,
        )
    }

    #[test]
    fn a_cancelled_run_stops_without_parent_selection_too() {
        let mut profile = SelectionProfile::default();
        profile.options.disable_1g1r = true;
        let limits = Limits {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            deadline: None,
        };
        assert!(matches!(run_with(&profile, &limits), Err(Error::Cancelled)));
        let expired = Limits {
            cancel: None,
            deadline: Some((std::time::Instant::now(), std::time::Duration::from_secs(1))),
        };
        assert!(matches!(
            run_with(&profile, &expired),
            Err(Error::Timeout(_))
        ));
    }

    #[test]
    fn a_pattern_the_built_in_engine_cannot_compile_fails_the_run_naming_it() {
        let mut profile = SelectionProfile::default();
        profile.overrides.exclude = vec!["/\\N{LATIN SMALL LETTER A}".into()];
        match run_with(&profile, &Limits::default()) {
            Err(Error::Unsupported(message)) => {
                assert!(message.contains("\\N{LATIN SMALL LETTER A}"), "{message}")
            }
            other => panic!("{:?}", other.map(|r| r.warnings)),
        }
    }

    #[test]
    fn an_ascii_only_pattern_is_supported() {
        let mut profile = SelectionProfile::default();
        profile.overrides.exclude = vec!["/(?a)^Pong".into()];
        let ran = run_with(&profile, &Limits::default()).unwrap();
        assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
    }

    /// A DAT of one group of `n` releases, all equivalent, so every chooser compares every pair.
    fn one_large_group(dir: &std::path::Path, n: usize) -> std::path::PathBuf {
        let mut dat = String::from(
            "<?xml version=\"1.0\"?>\n<datafile>\n<header><name>Synthetic - Large group</name>\
             <description>Synthetic - Large group</description></header>\n",
        );
        for i in 0..n {
            let name = format!("Pong (USA) (Rev {i})");
            dat.push_str(&format!(
                "<game name=\"{name}\"><category>Games</category><description>{name}</description>\
                 <rom name=\"{name}.bin\" size=\"1\" crc=\"{i:08x}\" sha1=\"{i:040x}\"/></game>\n"
            ));
        }
        dat.push_str("</datafile>\n");
        let path = dir.join("Large group.dat");
        std::fs::write(&path, dat).unwrap();
        path
    }

    #[test]
    fn a_cancelled_run_stops_inside_a_large_group() {
        let work = tempfile::tempdir().unwrap();
        let path = one_large_group(work.path(), 3000);
        let profile = SelectionProfile::default();
        let text = process::read_text(&path).unwrap();
        let settings = Settings::new(&internal_config(), &profile, "").unwrap();
        let dat = dat::read_header(&path, &text, &settings).unwrap();
        let mut run = Run::new(settings);
        let processed =
            process::process_dat(&mut run, &path, dat, &upstream().join("tests"), false).unwrap();
        let (_, titles) = processed.titles.first().unwrap();
        assert_eq!(titles.len(), 3000);
        // Cancelled once the group's choosing has begun: only the choosers' own checks can see it.
        run.limits.cancel = Some(Arc::new(AtomicBool::new(true)));
        let started = std::time::Instant::now();
        let chosen = select::choose_1g1r(&mut run, &mut Default::default(), false, false, titles);
        assert!(
            matches!(&chosen, Err(py::Crash(m)) if m == super::super::CANCELLED),
            "{:?}",
            chosen.map(|(g, _)| g)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
