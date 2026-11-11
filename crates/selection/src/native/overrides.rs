//! The user's overrides: excludes, includes and post filters (`modules/title_selection/excludes.py`,
//! `includes.py`, `overrides_post_filters.py`).

use super::clone_lists::{Lookup, key_error};
use super::json::py_repr;
use super::node::{Groups, Id, REMOVES, Run, Titles};
use super::py::PyResult;
use super::pyre::Re;
use super::pyset::{PyHash, PySet, tuple_hash};

/// `match_title_name`: `/regex` searches without case, `|name` is the whole name without case, anything else
/// a part of the name without case.
fn match_title_name(run: &mut Run, pattern: &str, name: &str) -> PyResult<bool> {
    if let Some(regex) = pattern.strip_prefix('/') {
        run.search_i(regex, name)
    } else if let Some(exact) = pattern.strip_prefix('|') {
        Ok(exact.to_lowercase() == name.to_lowercase())
    } else {
        Ok(name.to_lowercase().contains(&pattern.to_lowercase()))
    }
}

/// `user_override_post_filter_match`. A `<...>` entry also marks the title's group as related.
pub(crate) fn override_match(
    run: &mut Run,
    title: Id,
    list: &[String],
    is_post_filter: bool,
    cloneof_check: bool,
) -> PyResult<bool> {
    let mut matched = false;
    for entry in list {
        if cloneof_check {
            let cloneof = run.n(title).cloneof.clone();
            if !cloneof.is_empty() {
                matched = match_title_name(run, entry, &cloneof)?;
            }
            if matched {
                break;
            }
        } else if entry.starts_with('<') && entry.ends_with('>') && !is_post_filter {
            let inner: String = {
                let mut chars = entry.chars();
                chars.next();
                chars.next_back();
                chars.collect()
            };
            let name = run.n(title).full_name.clone();
            matched = match_title_name(run, &inner, &name)?;
            if matched {
                run.m(title).exclude_include_related = true;
                break;
            }
        } else {
            let name = run.n(title).full_name.clone();
            matched = match_title_name(run, entry, &name)?;
            if matched {
                break;
            }
        }
    }
    Ok(matched)
}

/// A `(pattern, category)` pair in the set of exclusion patterns, with the pattern's CPython hash.
#[derive(Clone, PartialEq, Eq)]
struct ExcludeRegex {
    hash: i64,
    index: usize,
    category: &'static str,
}

impl PyHash for ExcludeRegex {
    fn py_hash(&self) -> i64 {
        tuple_hash(&[self.hash, self.category.py_hash()])
    }
}

/// CPython 3.11's `hash()` of the compiled exclusion patterns under `PYTHONHASHSEED=0`. A compiled pattern
/// hashes its source, flags and compiled code, so Python computed these, from the patched Retool's patterns.
pub(crate) const PATTERN_HASHES: [(&str, i64); 29] = [
    (r"\(Aftermarket\)", -1527780529567862980),
    (
        r"\((?:Test )?Program\)|(Check|Sample) Program",
        6884385613080568561,
    ),
    (r"\[b\]", -2967871312222758192),
    (r"\[BIOS\]|\(Enhancement Chip\)", -2651752067714088007),
    (r"\(Manual\)", 5901385456363045079),
    (r"\(Magazine\)", 4776065235205278194),
    (r"\(Pirate\)", 3952869458646580619),
    (r"EPK|Press Kit|\(Promo\)", -5447584174571711810),
    (r"\(Unl\)", -5411375134185336183),
    (
        r"\((?:\w[-.]?\s*)*Demo(?:(?:,?\s|-)[\w0-9\.]*)*\)",
        8621702019127332852,
    ),
    (r"Cheheompan", 3934961188316632231),
    (r"Taikenban", -5678744530049316434),
    (r"\(@barai\)", -1273464575736454607),
    (r"\(GameCube Preview\)", 965194290472641758),
    (r"\(Preview\)", -8471712575163877327),
    (
        r"\(Sample(?:\s[0-9]*|\s\d{4}-\d{2}-\d{2})?\)",
        8004911786196057012,
    ),
    (r"Trial (Disc|Edition|Version|ver\.)", -5524910502086880059),
    (r"\((?:Full )?Trial\)", -6748760623783897395),
    (
        r"\((?:\w-?\s*)*?Kiosk,?(?:\s\w*?)*\)|Kiosk Demo Disc|(PSP System|PS2) Kiosk",
        2865651020979943771,
    ),
    (r"\((?:\w*?\s)*Alpha(?:\s\d+)?\)", -8726183323881092777),
    (r"\((?:\w*?\s)*Beta(?:\s\d+)?\)", 229648578011984261),
    (
        r"\((?:\w*?\s)*Proto(?:type)?(?:\s\d+)?\)",
        -5367692860619291654,
    ),
    (r"\((?:Pre-production|Prerelease)\)", 6571286085304218418),
    (r"\((?:DEV|DEBUG|Debug Build)\)", -1200179127653802190),
    (r"Game Boy Advance Video", -3593284307657121459),
    (r"- (Preview|Movie) Trailer", 5505491355686771945),
    (
        r"\(Nintendo (?:3DS )?(?:Direct|Conference).*?\)",
        -8080500405440680180,
    ),
    (
        r"\((?:\w*\s)*Trailer(?:s|\sDisc)?(?:\s\w*)*\)",
        5786313148608359709,
    ),
    (r"\((?:E3.*)?Video\)", 7213392027033624653),
];

fn pattern_hash(source: &str) -> PyResult<i64> {
    PATTERN_HASHES
        .iter()
        .find(|(s, _)| *s == source)
        .map(|(_, h)| *h)
        .ok_or_else(|| {
            super::py::Crash(format!(
                "no recorded hash for the exclusion pattern {source}"
            ))
        })
}

/// `f'{category.lower()}_removes'` with spaces and hyphens as underscores.
fn removes_list(category: &str) -> &'static str {
    let name = format!("{}_removes", category.to_lowercase()).replace([' ', '-'], "_");
    REMOVES
        .iter()
        .find(|r| **r == name)
        .copied()
        .unwrap_or_else(|| panic!("no removes list {name}"))
}

/// Whether `re.search('.*', name)` finds a non-empty match, which Retool's trace filter asks.
fn traced(name: &str) -> bool {
    !name.starts_with('\n') && !name.is_empty()
}

/// `excludes`.
pub(crate) fn excludes(run: &mut Run, groups: &mut Groups) -> PyResult<()> {
    run.note("• Applying category and user excludes... ");
    let e = run.s.options.exclude;
    let mut categories: Vec<&'static str> = Vec::new();
    let mut patterns: Vec<(Re, &'static str)> = Vec::new();
    {
        let r = &run.s.regex;
        let mut add = |re: &Re, category: &'static str| patterns.push((re.clone(), category));
        if e.add_ons {
            categories.push("Add-Ons");
        }
        if e.aftermarket {
            add(&r.aftermarket, "Aftermarket");
        }
        if e.applications {
            categories.push("Applications");
            add(&r.programs, "Applications");
        }
        if e.audio {
            categories.push("Audio");
        }
        if e.bad_dumps {
            add(&r.bad, "Bad Dumps");
        }
        if e.bios {
            categories.push("BIOS");
            add(&r.bios, "BIOS");
        }
        if e.bonus_discs {
            categories.push("Bonus Discs");
        }
        if e.coverdiscs {
            categories.push("Coverdiscs");
        }
        if e.demos {
            categories.push("Demos");
            for demo in &r.demos {
                add(demo, "Demos");
            }
        }
        if e.educational {
            categories.push("Educational");
        }
        if e.games {
            categories.push("Games");
        }
        if e.manuals {
            categories.push("Manuals");
            add(&r.manuals, "Manuals");
        }
        if e.multimedia {
            categories.push("Multimedia");
            add(&r.multimedia, "Multimedia");
        }
        if e.pirate {
            categories.push("Pirate");
            add(&r.pirate, "Pirate");
        }
        if e.preproduction {
            categories.push("Preproduction");
            for p in r.preproduction() {
                add(p, "Preproduction");
            }
        }
        if e.promotional {
            categories.push("Promotional");
            add(&r.promotional, "Promotional");
        }
        if e.unlicensed {
            categories.push("Unlicensed");
            add(&r.unlicensed, "Unlicensed");
        }
        if e.video {
            categories.push("Video");
            for v in &r.video {
                add(v, "Video");
            }
        }
    }
    // Retool keeps the patterns in a set, so the first that matches a title is the first in the set's order.
    let mut set: PySet<ExcludeRegex> = PySet::new();
    for (index, (re, category)) in patterns.iter().enumerate() {
        set.add(ExcludeRegex {
            hash: pattern_hash(&re.source)?,
            index,
            category,
        });
    }
    let ordered: Vec<(Re, &'static str)> = set.iter().map(|x| patterns[x.index].clone()).collect();

    let mut exclude_titles = Titles::new();
    let flattened: Titles = groups.values().flatten().copied().collect();
    // Title Select never passes `--no-overrides`.
    let no_overrides = false;
    for &title in &flattened {
        run.limits.check()?;
        let full_name = run.n(title).full_name.clone();
        let mut found = false;
        for category in &categories {
            let has = {
                let c = &run.n(title).categories;
                c.iter().any(|x| x == category) || c.iter().any(|x| *x == category.to_lowercase())
            };
            if has {
                if traced(&full_name) {
                    let joined = run.n(title).categories.join(", ");
                    run.trace_title("REF0042", &[joined], [&title], false);
                }
                exclude_titles.insert(title);
                run.m(title).exclude_reason = format!("Category exclude, {category}");
                run.remove_into(removes_list(category), title);
                found = true;
                break;
            }
        }
        if found {
            continue;
        }
        for (re, category) in &ordered {
            if !re.find(&full_name).is_empty() {
                if traced(&full_name) {
                    run.trace_title("REF0044", &[py_repr(&re.source)], [&title], false);
                }
                exclude_titles.insert(title);
                run.m(title).exclude_reason = format!("Category exclude, {category}");
                run.remove_into(removes_list(category), title);
                found = true;
                break;
            }
        }
        if found {
            continue;
        }
        if e.mia && run.n(title).is_mia {
            run.trace_title("REF0062", &[], [&title], false);
            exclude_titles.insert(title);
            run.m(title).exclude_reason = "Known MIA title".into();
            run.remove_into("mia_removes", title);
            continue;
        }
        if !no_overrides {
            let system_exclude = run.s.system_exclude.clone();
            let system_include = run.s.system_include.clone();
            let global_exclude = run.s.global_exclude.clone();
            let global_include = run.s.global_include.clone();
            if !system_exclude.is_empty() {
                if override_match(run, title, &system_exclude, false, false)?
                    && !override_match(run, title, &system_include, false, false)?
                {
                    run.trace_title("REF0045", &[], [&title], false);
                    exclude_titles.insert(title);
                    run.m(title).exclude_reason = "User system exclude".into();
                    run.remove_into("system_excludes", title);
                } else if override_match(run, title, &system_exclude, false, false)?
                    && override_match(run, title, &system_include, false, false)?
                {
                    run.trace_title("REF0050", &[], [&title], false);
                }
            }
            if !global_exclude.is_empty() {
                if override_match(run, title, &global_exclude, false, false)?
                    && !override_match(run, title, &global_include, false, false)?
                    && !override_match(run, title, &system_include, false, false)?
                {
                    run.trace_title("REF0046", &[], [&title], false);
                    exclude_titles.insert(title);
                    run.m(title).exclude_reason = "User global exclude".into();
                    run.remove_into("global_excludes", title);
                } else if override_match(run, title, &global_exclude, false, false)?
                    && override_match(run, title, &global_include, false, false)?
                {
                    run.trace_title("REF0051", &[], [&title], false);
                } else if override_match(run, title, &global_exclude, false, false)?
                    && override_match(run, title, &system_include, false, false)?
                {
                    run.trace_title("REF0052", &[], [&title], false);
                }
            }
        }
    }
    let mut related = Titles::new();
    if !no_overrides {
        for &title in &exclude_titles {
            if !run.n(title).exclude_include_related {
                continue;
            }
            let group = run.n(title).group_name.clone();
            let reason = run.n(title).exclude_reason.clone();
            let members: Vec<Id> = groups
                .get(&group)
                .ok_or_else(|| key_error(&group))?
                .iter()
                .copied()
                .collect();
            for member in members {
                if reason == "User system exclude" {
                    run.m(member).exclude_reason = "Related to user system excludes".into();
                    run.remove_into("system_excludes", member);
                } else if reason == "User global exclude" {
                    run.m(member).exclude_reason = "Related to user global excludes".into();
                    run.remove_into("global_excludes", member);
                }
                related.insert(member);
            }
        }
    }
    exclude_titles.extend(related);
    for title in exclude_titles {
        let group = run.n(title).group_name.clone();
        let titles = groups.get_mut(&group).ok_or_else(|| key_error(&group))?;
        titles.shift_remove(&title);
        if titles.is_empty() {
            groups.shift_remove(&group);
        }
    }
    run.note("• Applying category and user excludes... done.");
    Ok(())
}

/// Whether the system's post filters list is truthy in Retool, where it holds its override marker.
fn system_filter_set(run: &Run) -> bool {
    !run.s.system_filter.is_empty() || run.s.system_filter_override.is_some()
}

/// `post_filters`.
pub(crate) fn post_filters(run: &mut Run, groups: &mut Groups) -> PyResult<()> {
    let system_set = system_filter_set(run);
    let override_true = run.s.system_filter_override == Some(true);
    let override_false = run.s.system_filter_override == Some(false);
    let global_set = !run.s.global_filter.is_empty();
    let progress = (system_set && override_true) || (global_set && override_false);
    if system_set && override_true {
        run.note("• Applying post filters... ");
    }
    if global_set && override_false {
        run.note("• Applying post filters... ");
    }
    let mut dupe_check = Titles::new();
    let mut filter_titles: Vec<(String, Id)> = Vec::new();
    let system_filter = run.s.system_filter.clone();
    let global_filter = run.s.global_filter.clone();
    // Retool's list holds the override marker too, which only counts when it isn't `true`.
    let system_entries = !system_filter.is_empty() || override_false;
    let group_names: Vec<String> = groups.keys().cloned().collect();
    for group_name in group_names {
        run.limits.check()?;
        let titles: Vec<Id> = groups[&group_name].iter().copied().collect();
        for title in titles {
            if dupe_check.contains(&title) {
                continue;
            }
            let mut add = |run: &mut Run, list: &'static str, reason: &str, dupe: &mut Titles| {
                if !filter_titles.contains(&(group_name.clone(), title)) {
                    filter_titles.push((group_name.clone(), title));
                }
                let name = run.n(title).full_name.clone();
                if !dupe.iter().any(|&d| run.n(d).full_name == name) {
                    run.m(title).exclude_reason = reason.into();
                    dupe.insert(title);
                    run.remove_into(list, title);
                }
            };
            if system_set && system_entries && override_true {
                let has_clone = !run.n(title).cloneof.is_empty();
                if has_clone {
                    if !override_match(run, title, &system_filter, true, true)? {
                        add(
                            run,
                            "system_filter_removes",
                            "System post filter exclude",
                            &mut dupe_check,
                        );
                    }
                } else if !override_match(run, title, &system_filter, true, false)? {
                    add(
                        run,
                        "system_filter_removes",
                        "System post filter exclude",
                        &mut dupe_check,
                    );
                } else {
                    run.trace_title("REF0099", &[], [&title], false);
                }
            }
            if global_set && (!system_set || override_false) {
                let has_clone = !run.n(title).cloneof.is_empty();
                if has_clone {
                    if !override_match(run, title, &global_filter, true, true)? {
                        add(
                            run,
                            "global_filter_removes",
                            "Global post filter exclude",
                            &mut dupe_check,
                        );
                    }
                } else if !override_match(run, title, &global_filter, true, false)? {
                    add(
                        run,
                        "global_filter_removes",
                        "Global post filter exclude",
                        &mut dupe_check,
                    );
                } else {
                    run.trace_title("REF0100", &[], [&title], false);
                }
            }
        }
    }
    for (group, title) in &filter_titles {
        let titles = groups.get_mut(group).ok_or_else(|| key_error(group))?;
        if !titles.shift_remove(title) {
            return Err(super::py::Crash("KeyError: a post filtered title".into()));
        }
    }
    let winners = "retool_compilations_winners";
    let discards = "retool_compilations_discards";
    if groups.contains_key(winners) && groups.contains_key(discards) {
        let mut removes = Titles::new();
        for group in [winners, discards] {
            for &compilation in &groups[group] {
                for (_, filtered) in &filter_titles {
                    if run.n(*filtered).full_name == run.n(compilation).full_name {
                        removes.insert(compilation);
                    }
                }
            }
        }
        for remove in removes {
            for group in [winners, discards] {
                groups
                    .get_mut(group)
                    .expect("checked")
                    .shift_remove(&remove);
            }
        }
    }
    if progress {
        run.note("• Applying post filters... done.");
    }
    Ok(())
}

/// Every title in Retool's removed lists, in the lists' order.
fn removed(run: &Run) -> Vec<Id> {
    let mut out: Titles = Titles::new();
    for titles in run.removes.values() {
        out.extend(titles.iter().copied());
    }
    out.into_iter().collect()
}

/// `includes`.
pub(crate) fn includes(run: &mut Run, groups: &mut Groups, lookup: &Lookup) -> PyResult<()> {
    run.note("• Applying user includes... ");
    if run.removes.values().next().is_some() {
        run.trace.warn("\n\nStage: Inclusions");
    }
    let mut include_titles = Titles::new();
    let system_include = run.s.system_include.clone();
    let global_include = run.s.global_include.clone();
    let system_exclude = run.s.system_exclude.clone();
    let include = |run: &mut Run,
                   titles: Vec<Id>,
                   by_name: bool,
                   include_titles: &mut Titles|
     -> PyResult<()> {
        for title in titles {
            run.limits.check()?;
            for (system, reason) in [
                (true, "User system include"),
                (false, "User global include"),
            ] {
                let hit = if system {
                    override_match(run, title, &system_include, false, false)?
                } else {
                    override_match(run, title, &global_include, false, false)?
                        && !override_match(run, title, &system_exclude, false, false)?
                };
                if !hit {
                    continue;
                }
                let name = run.n(title).full_name.clone();
                if include_titles.iter().any(|&d| run.n(d).full_name == name) {
                    continue;
                }
                let property = if by_name {
                    run.n(title).full_name.clone()
                } else {
                    run.n(title).cloneof.clone()
                };
                if !property.is_empty() {
                    let node = run.m(title);
                    node.include_reason = reason.into();
                    node.cloneof = String::new();
                    node.exclude_reason = String::new();
                    include_titles.insert(title);
                }
            }
        }
        Ok(())
    };
    let lists: Vec<Vec<Id>> = run
        .removes
        .values()
        .map(|t| t.iter().copied().collect())
        .collect();
    for titles in lists {
        include(run, titles, true, &mut include_titles)?;
    }
    let current: Vec<Vec<Id>> = groups
        .values()
        .map(|t| t.iter().copied().collect())
        .collect();
    for titles in current {
        include(run, titles, false, &mut include_titles)?;
    }
    let system_includes: Vec<Id> = include_titles
        .iter()
        .copied()
        .filter(|&x| run.n(x).include_reason == "User system include")
        .collect();
    let global_includes: Vec<Id> = include_titles
        .iter()
        .copied()
        .filter(|&x| run.n(x).include_reason == "User global include")
        .collect();
    if !system_includes.is_empty() {
        run.trace_title("REF0047", &[], &system_includes, false);
    }
    if !global_includes.is_empty() {
        run.trace_title("REF0048", &[], &global_includes, false);
    }
    if include_titles
        .iter()
        .any(|&x| run.n(x).exclude_include_related)
    {
        let mut related = Titles::new();
        let mut group_names: Vec<String> = Vec::new();
        for &title in &include_titles {
            if run.n(title).exclude_include_related {
                let group = run.n(title).group_name.clone();
                if !group_names.contains(&group) {
                    group_names.push(group);
                }
            }
        }
        let names: Vec<String> = include_titles
            .iter()
            .map(|&x| run.n(x).full_name.clone())
            .collect();
        let mut related_of = |run: &mut Run, titles: Vec<Id>, group: &str, by_name: bool| {
            for title in titles {
                if names.contains(&run.n(title).full_name) || run.n(title).group_name != group {
                    continue;
                }
                let property = if by_name {
                    &run.n(title).full_name
                } else {
                    &run.n(title).cloneof
                };
                if !property.is_empty() {
                    let node = run.m(title);
                    node.cloneof = String::new();
                    node.exclude_reason = String::new();
                    related.insert(title);
                }
            }
        };
        for group in &group_names {
            let all = removed(run);
            related_of(run, all, group, true);
        }
        for group in &group_names {
            if let Some(titles) = groups.get(group) {
                let titles: Vec<Id> = titles.iter().copied().collect();
                related_of(run, titles, group, false);
            }
        }
        run.trace_title("REF0066", &[], &related, false);
        include_titles.extend(related);
    }
    for include_title in include_titles {
        let name = run.n(include_title).full_name.clone();
        if let Some(found) = lookup.full_name.get(&name) {
            for &current in found {
                let group = run.n(current).group_name.clone();
                if let Some(titles) = groups.get_mut(&group) {
                    titles.shift_remove(&current);
                }
            }
        }
        let group = run.n(include_title).group_name.clone();
        groups.entry(group).or_default().insert(include_title);
        let all = removed(run);
        for title in all {
            if run.n(title).full_name == name {
                for titles in run.removes.values_mut() {
                    titles.shift_remove(&title);
                }
            }
        }
    }
    run.note("• Applying user includes... done.");
    Ok(())
}
