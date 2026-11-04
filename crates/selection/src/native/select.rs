//! Parent selection: Retool's `choose_1g1r_orchestrator`, `choose_1g1r` and `choose_compilation`.
//!
//! Retool's sets of titles iterate in address order, which no port can reproduce; these iterate in insertion
//! order. Where Retool's result depends on that order, the port's may differ.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;

use super::choose::{self, Pat, TopLanguage, equivalent, pairs, regex_string, remove_all};
use super::json::{J, py_repr};
use super::node::{Groups, Id, Run, Titles};
use super::py::{Crash, PyResult};
use super::pyre::Re;
use super::pyset::PySet;
use super::regexes::Regexes;
use super::settings::Tag;
use super::titletools;

const WINNERS: &str = "retool_compilations_winners";
const DISCARDS: &str = "retool_compilations_discards";

/// Retool's `primary_region == 'USA' or 'Europe' or 'Japan'`.
fn usa_europe_japan(region: &str) -> bool {
    matches!(region, "USA" | "Europe" | "Japan")
}

/// `choose_string` with one of Retool's own patterns.
fn string_re(
    run: &mut Run,
    titles: &mut Titles,
    pattern: fn(&Regexes) -> &Re,
    with: bool,
) -> PyResult<()> {
    let re = pattern(&run.s.regex).clone();
    choose::string(run, titles, Pat::Re(&re), with)
}

/// `choose_string` for each of a list of edition tags.
fn string_tags(run: &mut Run, titles: &mut Titles, tags: &[Tag], with: bool) -> PyResult<()> {
    for tag in tags {
        choose::string(run, titles, Pat::of(tag), with)?;
    }
    Ok(())
}

/// The name a clone points at its parent by.
fn parent_name(run: &Run, id: Id) -> String {
    let n = run.n(id);
    if run.numbered {
        n.numbered_name.clone()
    } else {
        n.full_name.clone()
    }
}

/// `production_retail`, nested in `choose_1g1r`: of two equivalent titles, drops the one only it matches one of
/// `patterns`. It removes as it goes.
fn production_retail(run: &mut Run, titles: &mut Titles, patterns: &[Re]) -> PyResult<()> {
    for pair in pairs(&run.limits, titles) {
        let (a, b) = pair?;
        if !equivalent(run, a, b, None) {
            continue;
        }
        let (na, nb) = (run.n(a).full_name.clone(), run.n(b).full_name.clone());
        let (mut found_1, mut found_2) = (false, false);
        let mut found: Vec<&Re> = Vec::new();
        for pattern in patterns {
            let (m1, m2) = (!pattern.find(&na).is_empty(), !pattern.find(&nb).is_empty());
            if m1 && m2 {
                break;
            }
            if m1 && !m2 {
                found_1 = true;
                found.push(pattern);
            }
            if m2 && !m1 {
                found_2 = true;
                found.push(pattern);
            }
        }
        let Some(first) = found.first() else {
            continue;
        };
        let shown = regex_string(&first.py_str());
        if found_1 && !found_2 && titles.contains(&a) {
            run.trace_keep(
                "REF0117",
                &format!("{shown} {nb}"),
                &format!("{shown} {na}"),
            );
            titles.shift_remove(&a);
        }
        if found_2 && !found_1 && titles.contains(&b) {
            run.trace_keep(
                "REF0118",
                &format!("{shown} {na}"),
                &format!("{shown} {nb}"),
            );
            titles.shift_remove(&b);
        }
    }
    Ok(())
}

/// The patterns a modern edition tag compiles to when `production_retail` looks for it.
fn modern_patterns(tags: &[Tag]) -> Vec<Re> {
    tags.iter()
        .map(|tag| match tag {
            Tag::String(s) => Re::of(
                &s.replace('(', "\\(")
                    .replace(')', "\\)")
                    .replace('[', "\\[")
                    .replace(']', "\\]"),
            ),
            Tag::Regex(re) => re.clone(),
        })
        .collect()
}

/// `choose_1g1r`: chooses the parents in one group, and points the rest at them. It returns the group under the
/// name of its first title's group, as Retool does.
pub(crate) fn choose_1g1r(
    run: &mut Run,
    potential_parents: &mut IndexMap<String, Titles>,
    is_superset: bool,
    is_compilations: bool,
    title_set: &Titles,
) -> PyResult<(String, Titles)> {
    let language_order = if run.s.languages_filter.is_empty() {
        run.s.region_order_languages_user.clone()
    } else {
        run.language_order()
    };
    let region_order = run.region_order();
    let first = *title_set
        .first()
        .ok_or_else(|| Crash("StopIteration".into()))?;
    let group_name = run.n(first).group_name.clone();
    let saved = run.trace.current();
    let stage = if is_superset {
        "Superset parent selection"
    } else if is_compilations {
        "Compilation parent selection"
    } else {
        "Parent selection"
    };
    run.trace.context(Some(stage), &group_name, "");

    // The titles of each short name to read the top language from, preferring good, production titles.
    let short_names: PySet<String> = title_set
        .iter()
        .map(|&t| run.n(t).short_name.clone())
        .collect();
    let mut short_name_titles: IndexMap<String, Vec<Id>> = IndexMap::new();
    let mut short_name_groups: Vec<(String, Vec<Id>)> = Vec::new();
    let add = |map: &mut IndexMap<String, Vec<Id>>, short_name: &str, title: Id| {
        let list = map.entry(short_name.to_owned()).or_default();
        if !list.contains(&title) {
            list.push(title);
        }
    };
    for short_name in short_names.iter() {
        let r = &run.s.regex;
        for &title in title_set {
            let n = run.n(title);
            if n.short_name == *short_name && !r.bad.search(&n.full_name) {
                let preproduction = r.preproduction().iter().any(|p| p.search(&n.full_name));
                if !preproduction && !r.pirate.search(&n.full_name) {
                    add(&mut short_name_titles, short_name, title);
                }
            }
        }
        if short_name_titles.is_empty() {
            for &title in title_set {
                let n = run.n(title);
                if n.short_name == *short_name {
                    for p in r.preproduction() {
                        if p.search(&n.full_name) {
                            add(&mut short_name_titles, short_name, title);
                        }
                    }
                }
            }
        }
        if short_name_titles.is_empty() {
            for &title in title_set {
                let n = run.n(title);
                if n.short_name == *short_name && r.pirate.search(&n.full_name) {
                    add(&mut short_name_titles, short_name, title);
                }
            }
        }
        if short_name_titles.is_empty() {
            for &title in title_set {
                let n = run.n(title);
                if n.short_name == *short_name && r.bad.search(&n.full_name) {
                    add(&mut short_name_titles, short_name, title);
                }
            }
        }
        for (key, values) in &short_name_titles {
            short_name_groups.push((key.clone(), values.clone()));
        }
    }
    // Retool doesn't reset the top language between short names: once one is found, it carries over.
    let mut top_language = String::new();
    let mut top_languages: PySet<TopLanguage> = PySet::new();
    for (short_name, titles) in &short_name_groups {
        let highest = titles
            .iter()
            .map(|&t| run.n(t).language_priority)
            .min()
            .expect("a short name has titles");
        let mut by_region = titles.clone();
        by_region.sort_by_key(|&t| run.n(t).region_priority);
        for title in by_region {
            if run.n(title).language_priority != highest {
                continue;
            }
            let joined = run.n(title).languages.concat();
            for language in &language_order {
                if run.search(language, &joined) {
                    top_language = language.clone();
                    break;
                }
            }
            if !top_language.is_empty() {
                break;
            }
        }
        top_languages.add(TopLanguage(
            short_name.clone(),
            highest,
            top_language.clone(),
        ));
    }

    let oldest = run.s.options.oldest;
    let modern = run.s.options.modern;
    let modern_tags = run.s.tags_modern_editions.clone();
    let budget_tags = run.s.tags_budget_editions.clone();
    let promote_tags = run.s.tags_promote_editions.clone();
    let demote_tags = run.s.tags_demote_editions.clone();
    let video_order = run
        .s
        .system_video_order_user
        .clone()
        .unwrap_or_else(|| run.s.video_order_user.clone());

    let mut cross = Titles::new();
    // The last region's titles, which a later step of Retool's reads by mistake.
    let mut last_titles: Option<Titles> = None;
    for region in &region_order {
        let mut by_short_name: IndexMap<String, Titles> = IndexMap::new();
        for &title in title_set {
            let n = run.n(title);
            if n.primary_region.contains(region.as_str()) {
                by_short_name
                    .entry(n.short_name.clone())
                    .or_default()
                    .insert(title);
            }
        }
        for (short_name, mut titles) in by_short_name {
            if titles.len() > 1 {
                run.trace
                    .context(None, &group_name, &format!("{region} | {short_name}"));
                run.trace_title("REF0001", &[], &titles, false);
                if titles.len() > 1 && run.s.options.retroachievements {
                    choose::retroachievements(run, &mut titles)?;
                }
                if titles.len() > 1 {
                    choose::good(run, &mut titles)?;
                }
                run.trace_title("REF0003", &[], &titles, false);
                if titles.len() > 1 {
                    choose::language(run, &mut titles, true)?;
                }
                run.trace_title("REF0005", &[], &titles, false);
                if titles.len() > 1 {
                    choose::superset(run, &mut titles)?;
                }
                run.trace_title("REF0076", &[], &titles, false);
                if titles.len() > 1 {
                    choose::priority(run, &mut titles)?;
                }
                run.trace_title("REF0002", &[], &titles, false);
                if titles.len() > 1 {
                    string_tags(run, &mut titles, &modern_tags, modern)?;
                }
                run.trace_title("REF0004", &[], &titles, false);
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.unlicensed, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.aftermarket, false)?;
                }
                run.trace_title("REF0060", &[], &titles, false);
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.sega32x, true)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.fmtowns_marty, true)?;
                }
                run.trace_title("REF0006", &[], &titles, false);
                if titles.len() > 1 {
                    string_tags(run, &mut titles, &budget_tags, !oldest)?;
                    run.trace_title("REF0113", &[], &titles, false);
                }
                for key in [
                    "version",
                    "revision",
                    "firmware",
                    "beta",
                    "alpha",
                    "proto",
                    "sega",
                    "playstation",
                    "nec",
                    "benesse",
                ] {
                    if titles.len() > 1 {
                        choose::version_revision(run, &mut titles, key)?;
                    }
                }
                run.trace_title("REF0007", &[], &titles, false);
                if titles.len() > 1 {
                    for standard in &video_order {
                        choose::video_standard(run, &mut titles, &standard.to_lowercase())?;
                    }
                    run.trace_title("REF0096", &[], &titles, false);
                }
                if titles.len() > 1 {
                    choose::language(run, &mut titles, false)?;
                    run.trace_title("REF0043", &[], &titles, false);
                }
                if titles.len() > 1 {
                    choose::region(run, &mut titles, &region_order, false)?;
                    run.trace_title("REF0008", &[], &titles, false);
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.alt, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.oem, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.not_for_resale, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.covermount, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.rerelease, false)?;
                }
                if titles.len() > 1 {
                    string_re(run, &mut titles, |r| &r.edc, true)?;
                }
                run.trace_title("REF0010", &[], &titles, false);
                if titles.len() > 1 {
                    string_tags(run, &mut titles, &promote_tags, true)?;
                    string_tags(run, &mut titles, &demote_tags, false)?;
                    run.trace_title("REF0011", &[], &titles, false);
                }
                if titles.len() > 1 {
                    choose::date(run, &mut titles)?;
                    run.trace_title("REF0009", &[], &titles, false);
                }
                if titles.len() > 1 {
                    choose::version_revision(run, &mut titles, "build")?;
                }
                if titles.len() > 1 {
                    choose::made_in(run, &mut titles)?;
                    run.trace_title("REF0012", &[], &titles, false);
                }
                if titles.len() > 1 {
                    choose::version_revision(run, &mut titles, "alt")?;
                    run.trace_title("REF0061", &[], &titles, false);
                }
                if !is_compilations && titles.len() > 1 {
                    choose::highest_string(run, &mut titles)?;
                }
                run.trace_title("REF0059", &[], &titles, false);
            } else if titles.len() == 1 {
                run.trace
                    .context(None, &group_name, &format!("{region} | {short_name}"));
                run.trace_title("REF0074", &[], &titles, false);
            }
            cross.extend(titles.iter().copied());
            last_titles = Some(titles);
        }
    }
    run.trace.context(None, &group_name, "All regions");
    run.trace_title("REF0013", &[], &cross, false);

    if cross.len() > 1 {
        if run.s.options.retroachievements {
            choose::retroachievements(run, &mut cross)?;
        }
        if !run.s.options.region_bias {
            choose::language_top(run, &mut cross, &top_languages)?;
        }
        let r = &run.s.regex;
        let preproduction: Vec<Re> = r.preproduction().into_iter().cloned().collect();
        let bad = [r.bad.clone()];
        let pirate = [r.pirate.clone()];
        let unl: Vec<Re> = r.unl_group().into_iter().cloned().collect();
        production_retail(run, &mut cross, &preproduction)?;
        production_retail(run, &mut cross, &bad)?;
        production_retail(run, &mut cross, &pirate)?;
        if !modern {
            production_retail(run, &mut cross, &modern_patterns(&modern_tags))?;
        }
        if run.s.options.demote_unl {
            production_retail(run, &mut cross, &unl)?;
        }
        run.trace_title("REF0119", &[], &cross, false);

        let region_bias = run.s.options.region_bias;
        for pair in pairs(&run.limits, &cross) {
            let (a, b) = pair?;
            if !equivalent(run, a, b, Some(&cross)) {
                continue;
            }
            let (x, y) = (run.n(a), run.n(b));
            if !region_bias && (x.is_superset || y.is_superset) {
                continue;
            }
            if x.region_priority < y.region_priority {
                if !(usa_europe_japan(&x.primary_region) && y.primary_region == "World") {
                    cross.shift_remove(&b);
                }
            } else if y.region_priority < x.region_priority
                && !(usa_europe_japan(&y.primary_region) && x.primary_region == "World")
            {
                cross.shift_remove(&a);
            }
        }
        run.trace_title("REF0014", &[], &cross, false);

        if cross.len() > 1 {
            for pair in pairs(&run.limits, &cross) {
                let (a, b) = pair?;
                if !equivalent(run, a, b, Some(&cross)) {
                    continue;
                }
                let (sa, sb) = (run.n(a).is_superset, run.n(b).is_superset);
                if sa && !sb {
                    cross.shift_remove(&b);
                } else if sb && !sa {
                    cross.shift_remove(&a);
                }
            }
            run.trace_title("REF0103", &[], &cross, false);
        }
        if cross.len() > 1 {
            for pair in pairs(&run.limits, &cross) {
                let (a, b) = pair?;
                let (x, y) = (run.n(a), run.n(b));
                if !(x.is_superset && y.is_superset) {
                    continue;
                }
                let (na, nb) = (x.full_name.clone(), y.full_name.clone());
                if x.clonelist_priority < y.clonelist_priority {
                    run.trace_keep("REF0106", &na, &nb);
                    cross.shift_remove(&b);
                } else if y.clonelist_priority < x.clonelist_priority {
                    run.trace_keep("REF0107", &nb, &na);
                    cross.shift_remove(&a);
                }
            }
        }
        if cross.len() > 1 {
            let mut remove_names: HashSet<String> = HashSet::new();
            for region in &region_order {
                let in_region: Titles = cross
                    .iter()
                    .copied()
                    .filter(|&t| run.n(t).regions.contains(region))
                    .collect();
                for pair in pairs(&run.limits, &in_region) {
                    let (a, b) = pair?;
                    if !equivalent(run, a, b, None) {
                        continue;
                    }
                    let mut winner: Titles = [a, b].into_iter().collect();
                    choose::language(run, &mut winner, true)?;
                    if winner.len() == 1 {
                        let left = *winner.first().expect("one title");
                        if run.n(a).full_name == run.n(left).full_name {
                            remove_names.insert(run.n(b).full_name.clone());
                        } else {
                            remove_names.insert(run.n(a).full_name.clone());
                        }
                    }
                }
            }
            cross.retain(|&t| !remove_names.contains(&run.n(t).full_name));
            run.trace_title("REF0104", &[], &cross, false);
        }
        for pair in pairs(&run.limits, &cross) {
            let (a, b) = pair?;
            let (ra, rb) = (&run.n(a).primary_region, &run.n(b).primary_region);
            let usa_and_world =
                (usa_europe_japan(ra) && rb == "World") || (usa_europe_japan(rb) && ra == "World");
            if !usa_and_world {
                continue;
            }
            if cross.len() > 1 {
                // Retool applies the modern editions to the last region's titles here, not to these.
                if !modern_tags.is_empty() {
                    let titles = last_titles.as_mut().ok_or_else(|| {
                        Crash(
                            "UnboundLocalError: cannot access local variable 'titles' where it is not \
                             associated with a value"
                                .into(),
                        )
                    })?;
                    string_tags(run, titles, &modern_tags, modern)?;
                }
                string_tags(run, &mut cross, &promote_tags, true)?;
                string_tags(run, &mut cross, &demote_tags, false)?;
            }
            run.trace_title("REF0140", &[], &cross, false);
        }
        if cross.len() > 1 {
            choose::region(run, &mut cross, &region_order, false)?;
            run.trace_title("REF0102", &[], &cross, false);
        }
        if cross.len() > 1 {
            choose::language(run, &mut cross, true)?;
            run.trace_title("REF0101", &[], &cross, false);
        }
        let supersets: Titles = cross
            .iter()
            .copied()
            .filter(|&t| run.n(t).is_superset)
            .collect();
        if supersets.is_empty() {
            cross.retain(|&t| !run.n(t).is_superset);
        } else {
            let mut by_short_name: IndexMap<String, Titles> = IndexMap::new();
            for &title in &supersets {
                by_short_name
                    .entry(run.n(title).short_name.clone())
                    .or_default()
                    .insert(title);
            }
            let mut superset_final = Titles::new();
            for (short_name, set) in &by_short_name {
                let top = set
                    .iter()
                    .map(|&t| run.n(t).clonelist_priority)
                    .min()
                    .expect("a short name has titles");
                run.trace_title(
                    "REF0016",
                    &[top.to_string(), short_name.clone()],
                    set,
                    false,
                );
                let trimmed: Vec<Id> = set
                    .iter()
                    .copied()
                    .filter(|&t| run.n(t).clonelist_priority == top)
                    .collect();
                for region in &region_order {
                    let mut region_found = false;
                    for &title in &trimmed {
                        if run.n(title).regions.contains(region) {
                            region_found = true;
                            superset_final.insert(title);
                        }
                    }
                    if region_found {
                        break;
                    }
                }
                run.trace_title("REF0079", &[], &superset_final, false);
                cross.retain(|&t| !run.n(t).is_superset);
                let mut superset_removes = Titles::new();
                for title in cross.clone() {
                    run.limits.check()?;
                    for superset in superset_final.clone() {
                        if !equivalent(run, title, superset, None) || !cross.contains(&title) {
                            continue;
                        }
                        let (tn, sn) = (
                            run.n(title).full_name.clone(),
                            run.n(superset).full_name.clone(),
                        );
                        if region_bias {
                            if run.n(title).region_priority < run.n(superset).region_priority {
                                superset_removes.insert(superset);
                                run.trace_keep("REF0086", &tn, &sn);
                            } else {
                                cross.shift_remove(&title);
                                run.trace_keep("REF0088", &sn, &tn);
                            }
                        } else {
                            cross.shift_remove(&title);
                            run.trace_keep("REF0087", &sn, &tn);
                        }
                    }
                }
                remove_all(&mut superset_final, &superset_removes);
                cross.extend(superset_final.iter().copied());
            }
            run.trace_title("REF0017", &[], &superset_final, false);
        }
    }

    let mut clone_report: IndexMap<String, BTreeSet<String>> = IndexMap::new();
    for &parent in &cross {
        for &title in title_set {
            if !equivalent(run, title, parent, None)
                || run.n(title).full_name == run.n(parent).full_name
            {
                continue;
            }
            let title_name = run.n(title).full_name.clone();
            if is_superset && !potential_parents.contains_key(&title_name) {
                potential_parents.insert(title_name.clone(), Titles::new());
            }
            let parent_full = run.n(parent).full_name.clone();
            clone_report.entry(parent_full.clone()).or_default();
            if !clone_report.values().any(|c| c.contains(&title_name)) {
                clone_report
                    .get_mut(&parent_full)
                    .expect("entry")
                    .insert(title_name.clone());
            }
            if !is_superset {
                let name = parent_name(run, parent);
                run.m(title).cloneof = name;
            } else {
                let known = potential_parents[&title_name]
                    .iter()
                    .any(|&x| run.n(x).full_name == parent_full);
                if !known {
                    potential_parents
                        .get_mut(&title_name)
                        .expect("entry")
                        .insert(parent);
                }
            }
        }
    }
    if !clone_report.is_empty() {
        run.trace_ref("REF0019", &[]);
        clone_report.sort_keys();
        for (parent, clones) in &clone_report {
            run.note(&format!("+ [1G1R title]\t{parent}"));
            let last = clones.iter().next_back();
            for clone in clones {
                let symbol = if Some(clone) == last { '└' } else { '├' };
                run.note(&format!("{symbol} [clone]\t{clone}"));
            }
        }
    }
    run.trace.context(Some(&saved.0), &saved.1, &saved.2);
    Ok((group_name, title_set.clone()))
}

/// `choose_1g1r_orchestrator`: chooses the parents of every group, then of the groups with supersets, then
/// among compilations.
pub(crate) fn orchestrator(run: &mut Run, mut processed: Groups) -> PyResult<Groups> {
    let mut superset_groups = Groups::new();
    let mut compilations = Titles::new();
    let names: Vec<String> = processed.keys().cloned().collect();
    for group in &names {
        let titles = processed[group].clone();
        for &title in &titles {
            if run.n(title).is_superset {
                let copy = run.copy_set(&titles);
                superset_groups.insert(group.clone(), copy);
            }
            if !run.n(title).contains_titles.is_empty() {
                compilations.insert(title);
            }
        }
    }
    for group in superset_groups.keys() {
        processed.shift_remove(group);
    }
    for &compilation in &compilations {
        let group = run.n(compilation).group_name.clone();
        if let Some(titles) = processed.get_mut(&group) {
            titles.shift_remove(&compilation);
        }
        match processed.get(&group) {
            None => return Err(Crash(format!("KeyError: {}", py_repr(&group)))),
            Some(titles) if titles.is_empty() => {
                processed.shift_remove(&group);
            }
            Some(_) => {}
        }
    }
    run.note("• Selecting 1G1R titles...");
    let mut unused = IndexMap::new();
    let mut parent_titles: Vec<(String, Titles)> = Vec::new();
    for titles in processed.values() {
        run.limits.check()?;
        parent_titles.push(choose_1g1r(run, &mut unused, false, false, titles)?);
    }
    let mut potential_parents: IndexMap<String, Titles> = IndexMap::new();
    let mut superset_parent_titles: Vec<(String, Titles)> = Vec::new();
    for titles in superset_groups.values() {
        run.limits.check()?;
        superset_parent_titles.push(choose_1g1r(
            run,
            &mut potential_parents,
            true,
            false,
            titles,
        )?);
    }
    let mut all = Groups::new();
    for (key, values) in parent_titles {
        all.insert(key, values);
    }
    for (key, values) in superset_parent_titles {
        for &value in &values {
            let name = run.n(value).full_name.clone();
            let Some(potential) = potential_parents.get(&name) else {
                continue;
            };
            let candidates: Vec<Id> = potential.iter().copied().collect();
            let key_of = |run: &Run, x: Id| (run.n(x).clonelist_priority, parent_name(run, x));
            let parent = if candidates.len() > 1 {
                let mut sorted = candidates.clone();
                if candidates.iter().any(|&x| run.n(x).is_superset) {
                    sorted.sort_by_key(|&a| std::cmp::Reverse(key_of(run, a)));
                    *sorted
                        .iter()
                        .find(|&&x| run.n(x).is_superset)
                        .expect("a superset")
                } else {
                    sorted.sort_by_key(|&a| key_of(run, a));
                    *sorted
                        .iter()
                        .find(|&&x| !run.n(x).is_superset)
                        .expect("a title")
                }
            } else {
                *candidates
                    .first()
                    .ok_or_else(|| Crash("IndexError: list index out of range".into()))?
            };
            let cloneof = parent_name(run, parent);
            run.m(value).cloneof = cloneof;
        }
        all.insert(key, values);
    }

    run.trace.context(Some("Superset clone check"), "", "");
    let mut superset_clones: HashMap<String, String> = HashMap::new();
    for titles in all.values() {
        for &title in titles {
            let n = run.n(title);
            if n.is_superset && !n.cloneof.is_empty() {
                superset_clones.insert(parent_name(run, title), n.cloneof.clone());
            }
        }
    }
    let ids: Vec<Id> = all.values().flatten().copied().collect();
    for title in ids {
        let n = run.n(title);
        if n.is_superset {
            continue;
        }
        let Some(new) = superset_clones.get(&n.cloneof).cloned() else {
            continue;
        };
        let old = std::mem::replace(&mut run.m(title).cloneof, new.clone());
        let name = run.n(title).full_name.clone();
        run.trace_ref("REF0114", &[]);
        run.note(&format!("\n• {name}"));
        run.note(&format!("  New clone: {new}\n  Old clone: {old}"));
    }
    let all = compilation(run, &compilations, all)?;
    run.note("• Selecting 1G1R titles... done.");
    Ok(all)
}

/// A compilation's grouping of the titles it holds, as `choose_compilation` compares them: their names, and
/// the compilation's region.
type Grouping = (Vec<String>, String);

/// Python's `repr` of a tuple from its items' reprs.
fn tuple_repr(items: &[String]) -> String {
    if items.len() == 1 {
        format!("({},)", items[0])
    } else {
        format!("({})", items.join(", "))
    }
}

fn list_repr(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

fn grouping_repr(grouping: &Grouping) -> String {
    let names: Vec<String> = grouping.0.iter().map(|x| py_repr(x)).collect();
    tuple_repr(&[tuple_repr(&names), py_repr(&grouping.1)])
}

fn candidate_repr(candidate: &[Grouping]) -> String {
    let items: Vec<String> = candidate.iter().map(grouping_repr).collect();
    tuple_repr(&items)
}

fn candidates_repr(candidates: &[Vec<Grouping>]) -> String {
    let items: Vec<String> = candidates.iter().map(|c| candidate_repr(c)).collect();
    list_repr(&items)
}

/// How much work the built-in engine spends comparing combinations of compilations for one group. Retool lists
/// every combination in its trace; past this it runs for hours and exhausts memory instead.
#[derive(Clone, Copy)]
struct Budget {
    /// The most combinations it compares.
    combinations: u128,
    /// The most bytes of candidate combinations it keeps.
    bytes: usize,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            combinations: 2_000_000,
            bytes: 64 << 20,
        }
    }
}

fn too_many(what: String) -> Crash {
    Crash(format!(
        "too many combinations of compilations to compare: {what}"
    ))
}

/// Roughly how much memory a grouping holds.
fn grouping_bytes((names, region): &Grouping) -> usize {
    names.iter().map(|n| n.len() + 24).sum::<usize>() + region.len() + 48
}

/// The REF0122 step of `choose_compilations`: every combination of fewer groupings than the groups in contention
/// that covers the required titles, in `itertools.combinations` order.
fn compilation_candidates(
    run: &mut Run,
    groupings: &[Grouping],
    required: &[(String, String)],
    contention: usize,
    budget: Budget,
) -> PyResult<Vec<Vec<Grouping>>> {
    // Retool prints each list of combinations whole. A list shown on one line changes nothing in the trace, so
    // it's only built when a name spans lines.
    let multiline = groupings
        .iter()
        .any(|(names, region)| region.contains('\n') || names.iter().any(|n| n.contains('\n')));
    let mut work: u128 = 0;
    let mut kept_bytes = 0;
    let mut candidates: Vec<Vec<Grouping>> = Vec::new();
    for i in 1..contention {
        run.limits.check()?;
        work = work.saturating_add(binomial(groupings.len(), i));
        if work > budget.combinations {
            return Err(too_many(format!("{} choose {i}", groupings.len())));
        }
        run.trace_ref("REF0122", &[i.to_string()]);
        let mut shown: Vec<String> = Vec::new();
        let mut shown_bytes = 0;
        for (n, combo) in combinations(groupings.len(), i).enumerate() {
            if n % 4096 == 0 {
                run.limits.check()?;
            }
            let combo: Vec<Grouping> = combo.iter().map(|&g| groupings[g].clone()).collect();
            if multiline {
                let repr = candidate_repr(&combo);
                shown_bytes += repr.len();
                if shown_bytes > budget.bytes {
                    return Err(too_many(format!("{shown_bytes} bytes to show")));
                }
                shown.push(repr);
            }
            let mut full: HashSet<(&str, &str)> = HashSet::new();
            for (names, region) in &combo {
                for name in names {
                    full.insert((name, region));
                }
            }
            if required
                .iter()
                .all(|(name, region)| full.contains(&(name.as_str(), region.as_str())))
            {
                kept_bytes += combo.iter().map(grouping_bytes).sum::<usize>();
                if kept_bytes > budget.bytes {
                    return Err(too_many(format!("{kept_bytes} bytes of candidates")));
                }
                candidates.push(combo);
            }
        }
        if multiline {
            run.note(&list_repr(&shown));
        }
    }
    Ok(candidates)
}

/// `n` choose `k`, saturating.
fn binomial(n: usize, k: usize) -> u128 {
    if k > n {
        return 0;
    }
    let k = k.min(n - k) as u128;
    let mut out: u128 = 1;
    for i in 0..k {
        out = match out.checked_mul(n as u128 - i) {
            Some(product) => product / (i + 1),
            None => return u128::MAX,
        };
    }
    out
}

/// `itertools.combinations(range(n), k)`, one combination of indices at a time.
struct Combinations {
    n: usize,
    indices: Option<Vec<usize>>,
}

fn combinations(n: usize, k: usize) -> Combinations {
    Combinations {
        n,
        indices: (k <= n).then(|| (0..k).collect()),
    }
}

impl Iterator for Combinations {
    type Item = Vec<usize>;

    fn next(&mut self) -> Option<Vec<usize>> {
        let current = self.indices.clone()?;
        let indices = self.indices.as_mut().expect("current");
        let k = indices.len();
        match (0..k).rev().find(|&i| indices[i] != i + self.n - k) {
            Some(i) => {
                indices[i] += 1;
                for j in i + 1..k {
                    indices[j] = indices[j - 1] + 1;
                }
            }
            None => self.indices = None,
        }
        Some(current)
    }
}

/// A title's `contains_titles` names, lowercased.
fn contained_lower(run: &Run, id: Id) -> Vec<String> {
    run.n(id)
        .contains_titles
        .keys()
        .map(|k| k.to_lowercase())
        .collect()
}

/// A compilation's grouping, or a title's.
fn grouping(run: &Run, id: Id) -> Grouping {
    let n = run.n(id);
    if n.contains_titles.is_empty() {
        (vec![n.group_name.clone()], n.primary_region.clone())
    } else {
        (contained_lower(run, id), n.primary_region.clone())
    }
}

/// An `int` from a clone list's compilation entry.
fn int_of(value: &J, what: &str) -> PyResult<i64> {
    match value {
        J::Int(n) => Ok(*n),
        _ => Err(Crash(format!("TypeError: {what} isn't an int"))),
    }
}

/// `TitleTools.convert_to_virtual_titles`: adds to `groups[name]` a copy of each compilation that holds a
/// title called `name`, named and prioritized as that title.
fn convert_to_virtual_titles(
    run: &mut Run,
    compilations: &Titles,
    groups: &mut IndexMap<String, Titles>,
    name: &str,
) -> PyResult<()> {
    for &compilation in compilations {
        if !contained_lower(run, compilation).join(",").contains(name) {
            continue;
        }
        let n = run.n(compilation);
        let mut preproduction = String::new();
        for pattern in run.s.regex.preproduction() {
            preproduction = pattern.find(&n.full_name);
            if !preproduction.is_empty() {
                preproduction = format!(" {preproduction}");
                break;
            }
        }
        let mut virtual_languages = String::new();
        let entries: Vec<(String, J, J)> = n
            .contains_titles
            .iter()
            .map(|(k, v)| (k.clone(), v.position.clone(), v.priority.clone()))
            .collect();
        for (title, position, priority) in entries {
            let n = run.n(compilation);
            if n.languages_title_orig.contains('+') {
                let parts: Vec<&str> = n.languages_title_orig.split('+').collect();
                let index = int_of(&position, "position")? - 1;
                let len = parts.len() as i64;
                let resolved = if index < 0 { index + len } else { index };
                if resolved < 0 || resolved >= len {
                    return Err(Crash("IndexError: list index out of range".into()));
                }
                virtual_languages = format!(" ({})", parts[resolved as usize]);
            } else if !n.languages.is_empty() {
                virtual_languages = format!(" ({})", n.languages.join(","));
            }
            let full_name = format!(
                ":V: {title} ({}){virtual_languages}{preproduction} • {}",
                n.regions.join(", "),
                n.full_name
            );
            let group = groups.get(name).expect("the group exists");
            if title.to_lowercase() != name
                || group.iter().any(|&x| run.n(x).full_name == full_name)
            {
                continue;
            }
            let priority = int_of(&priority, "priority")?;
            let languages: Vec<String> = virtual_languages
                .trim()
                .replace(['(', ')'], "")
                .split(',')
                .map(str::to_owned)
                .collect();
            let copy = run.copy(compilation);
            let v = run.m(copy);
            v.full_name = full_name;
            v.short_name = title.to_lowercase();
            v.languages = languages;
            v.clonelist_priority = priority;
            groups.get_mut(name).expect("the group exists").insert(copy);
        }
    }
    Ok(())
}

/// Finds the first title in `all` called `name`, and removes it from its own group.
fn remove_from_group(run: &Run, all: &mut Groups, name: &str) -> PyResult<()> {
    let found = all
        .values()
        .flat_map(|titles| titles.iter())
        .copied()
        .find(|&t| run.n(t).full_name == name);
    if let Some(remove) = found {
        let group = &run.n(remove).group_name;
        let titles = all
            .get_mut(group)
            .ok_or_else(|| Crash(format!("KeyError: {}", py_repr(group))))?;
        if !titles.shift_remove(&remove) {
            return Err(Crash(format!("KeyError: <DatNode {}>", py_repr(name))));
        }
    }
    Ok(())
}

/// `choose_compilation`: compares compilations with the titles they hold, and keeps the fewest titles that
/// cover them all.
fn compilation(run: &mut Run, compilations: &Titles, mut all: Groups) -> PyResult<Groups> {
    let mode = run.s.options.compilations;
    // Retool reads a loop variable here after its loop has ended: the last title name a compilation held.
    let mut last_contains: Option<String> = None;
    let mut individual = Titles::new();
    if mode != "k" {
        for &compilation in compilations {
            let keys: Vec<String> = run.n(compilation).contains_titles.keys().cloned().collect();
            for key in keys {
                let group = key.to_lowercase();
                last_contains = Some(key);
                if let Some(titles) = all.get(&group) {
                    let parent_names: HashSet<&str> = titles
                        .iter()
                        .filter(|&&t| run.n(t).cloneof.is_empty())
                        .map(|&t| run.n(t).full_name.as_str())
                        .collect();
                    for &title in titles {
                        let n = run.n(title);
                        if parent_names.contains(n.full_name.as_str()) && n.group_name == group {
                            individual.insert(title);
                        }
                    }
                }
            }
        }
    }
    let mut comparison: Titles = compilations.clone();
    comparison.extend(individual.iter().copied());
    run.trace.context(Some("Compilations"), "", "");
    let mut names: PySet<String> = PySet::new();
    for &compilation in compilations {
        for key in run.n(compilation).contains_titles.keys() {
            last_contains = Some(key.clone());
            names.add(key.to_lowercase());
        }
    }
    for &title in &individual {
        names.add(run.n(title).short_name.clone());
    }
    let mut grouped: IndexMap<String, Titles> = IndexMap::new();
    for name in names.iter() {
        grouped.entry(name.clone()).or_default();
        convert_to_virtual_titles(run, compilations, &mut grouped, name)?;
        for &title in &individual {
            if *name == run.n(title).short_name {
                grouped.get_mut(name).expect("entry").insert(title);
            }
        }
    }

    let region_order = run.region_order();
    let mut filtered: IndexMap<String, Titles> = IndexMap::new();
    for (key, titles) in &grouped {
        let mut set = run.copy_set(titles);
        let vars = [key.clone()];
        run.trace.context(Some("Compilations"), "", key);
        run.trace_title("REF0067", &vars, &set, false);
        if run.s.options.retroachievements {
            choose::retroachievements(run, &mut set)?;
            run.trace_title("REF0139", &vars, &set, false);
        }
        if set.len() > 1 {
            choose::good(run, &mut set)?;
            run.trace_title("REF0092", &vars, &set, false);
        }
        if set.len() > 1 && mode == "i" {
            if set.iter().any(|&t| run.n(t).contains_titles.is_empty()) {
                set.retain(|&t| run.n(t).contains_titles.is_empty());
            }
            run.trace_title("REF0126", &vars, &set, false);
        }
        if set.len() > 1 {
            choose::superset(run, &mut set)?;
            run.trace_title("REF0127", &vars, &set, false);
        }
        if set.len() > 1 {
            for &title in &set {
                run.m(title).primary_region = "compilation".into();
            }
            choose::language(run, &mut set, true)?;
            run.trace_title("REF0068", &vars, &set, false);
        }
        for &title in &set {
            let n = run.m(title);
            n.languages = n.languages_original.clone();
            n.primary_region = n.primary_region_original.clone();
        }
        if set.len() > 1 {
            choose::priority(run, &mut set)?;
            run.trace_title("REF0080", &vars, &set, false);
        }
        if set.len() > 1 {
            choose::region(run, &mut set, &region_order, true)?;
            run.trace_title("REF0069", &vars, &set, false);
        }
        for &title in &set {
            let n = run.m(title);
            n.full_name = n.full_name_original.clone();
        }
        run.trace_title("REF0070", &vars, &set, false);
        if set.len() > 1 && mode != "o" {
            let mut remove = Titles::new();
            for pair in pairs(&run.limits, &set) {
                let (a, b) = pair?;
                if !equivalent(run, a, b, None) {
                    continue;
                }
                let (ca, cb) = (
                    !run.n(a).contains_titles.is_empty(),
                    !run.n(b).contains_titles.is_empty(),
                );
                let (na, nb) = (run.n(a).full_name.clone(), run.n(b).full_name.clone());
                if ca && !cb {
                    remove.insert(a);
                    run.trace_keep("REF0075", &nb, &na);
                }
                if cb && !ca {
                    remove.insert(b);
                    run.trace_keep("REF0093", &na, &nb);
                }
            }
            remove_all(&mut set, &remove);
        }
        if set.len() > 1 {
            choose::language(run, &mut set, false)?;
        }
        if set.len() > 1 {
            for pair in pairs(&run.limits, &set) {
                let (a, b) = pair?;
                let (x, y) = (run.n(a), run.n(b));
                if x.contains_titles.is_empty() || y.contains_titles.is_empty() {
                    continue;
                }
                let same_region = x.primary_region == y.primary_region;
                let (ca, cb) = (&x.contains_titles, &y.contains_titles);
                let (na, nb) = (x.full_name.clone(), y.full_name.clone());
                if ca.len() < cb.len() && same_region && ca.keys().all(|k| cb.contains_key(k)) {
                    set.shift_remove(&a);
                    run.trace_keep("REF0090", &nb, &na);
                } else if cb.len() < ca.len()
                    && same_region
                    && cb.keys().all(|k| ca.contains_key(k))
                {
                    set.shift_remove(&b);
                    run.trace_keep("REF0091", &na, &nb);
                } else if ca == cb {
                    let (sa, sb) = (x.short_name.clone(), y.short_name.clone());
                    run.m(a).short_name = "compilation".into();
                    run.m(b).short_name = "compilation".into();
                    let pair: Titles = [a, b].into_iter().collect();
                    let (_, chosen) = choose_1g1r(run, &mut IndexMap::new(), false, true, &pair)?;
                    let parents: Titles = chosen
                        .into_iter()
                        .filter(|&t| run.n(t).cloneof.is_empty())
                        .collect();
                    run.m(a).short_name = sa;
                    run.m(b).short_name = sb;
                    if !parents.contains(&a) {
                        set.shift_remove(&a);
                    }
                    if !parents.contains(&b) {
                        set.shift_remove(&b);
                    }
                }
            }
            run.trace_title("REF0128", &vars, &set, false);
        }
        filtered.insert(key.clone(), set);
    }

    // The sets of titles compilations hold that overlap, merged the way Retool merges them.
    let mut related_names: Vec<BTreeSet<String>> = Vec::new();
    for titles in filtered.values() {
        for &title in titles {
            let n = run.n(title);
            if !n.contains_titles.is_empty() {
                related_names.push(n.contains_titles.keys().cloned().collect());
            } else if mode != "o" {
                related_names.push([n.short_name.clone()].into_iter().collect());
            }
        }
    }
    for i in 0..related_names.len() {
        run.limits.check()?;
        for j in 0..related_names.len() {
            if related_names[i]
                .intersection(&related_names[j])
                .next()
                .is_some()
            {
                let union: BTreeSet<String> =
                    related_names[i].union(&related_names[j]).cloned().collect();
                related_names[i] = union.clone();
                related_names[j] = union;
            }
        }
    }
    let mut lists: Vec<Vec<String>> = related_names
        .into_iter()
        .map(|s| s.into_iter().collect())
        .collect();
    lists.sort();
    lists.dedup();
    let mut related: IndexMap<String, Titles> = IndexMap::new();
    for list in lists {
        let list: Vec<String> = list.iter().map(|x| x.to_lowercase()).collect();
        let key = list.join(", ");
        related.entry(key.clone()).or_default();
        for short_name in &list {
            let titles = filtered
                .get(short_name)
                .ok_or_else(|| Crash(format!("KeyError: {}", py_repr(short_name))))?;
            for &title in titles {
                let name = &run.n(title).full_name;
                let set = related.get_mut(&key).expect("entry");
                if !set.iter().any(|&x| run.n(x).full_name == *name) {
                    set.insert(title);
                }
            }
        }
    }

    // Titles a superset replaces: those whose other titles the rest of the set already covers.
    let mut remove_titles = Titles::new();
    for titles in related.values() {
        let supersets: Vec<Id> = titles
            .iter()
            .copied()
            .filter(|&t| run.n(t).contains_titles.is_empty() && run.n(t).is_superset)
            .collect();
        for superset in supersets {
            let superset_short = run.n(superset).short_name.clone();
            let superset_full = run.n(superset).full_name.clone();
            let replace: Vec<Id> = titles
                .iter()
                .copied()
                .filter(|&t| contained_lower(run, t).contains(&superset_short))
                .collect();
            for title in replace {
                run.limits.check()?;
                let mut contained = contained_lower(run, title);
                if let Some(i) = contained.iter().position(|x| *x == superset_short) {
                    contained.remove(i);
                }
                let title_full = run.n(title).full_name.clone();
                let mut satisfied = 0;
                for name in &contained {
                    for &other in titles {
                        let o = run.n(other);
                        if o.full_name == title_full || o.full_name == superset_full {
                            continue;
                        }
                        if !o.contains_titles.is_empty() {
                            if contained_lower(run, other).contains(name) {
                                satisfied += 1;
                            }
                        } else if o.short_name == *name {
                            satisfied += 1;
                        }
                    }
                }
                if satisfied == contained.len() {
                    remove_titles.insert(title);
                }
            }
        }
    }
    for &remove in &remove_titles {
        let name = run.n(remove).full_name.clone();
        for titles in related.values_mut() {
            titles.retain(|&t| run.n(t).full_name != name);
        }
    }

    all.entry(WINNERS.into()).or_default();
    let mut available: IndexMap<String, PySet<Grouping>> = IndexMap::new();
    let keys: Vec<String> = related.keys().cloned().collect();
    for key in keys {
        run.limits.check()?;
        let titles = related[&key].clone();
        if titles.len() <= 1 {
            for &title in &titles {
                let name = run.n(title).full_name.clone();
                if all[WINNERS].iter().any(|&w| run.n(w).full_name == name) {
                    continue;
                }
                remove_from_group(run, &mut all, &name)?;
                all.get_mut(WINNERS).expect("entry").insert(title);
            }
            continue;
        }
        run.trace_ref("REF0071", std::slice::from_ref(&key));
        run.note("Titles in contention:\n");
        for &title in &titles {
            let n = run.n(title);
            let line = format!("• [{}] {}", n.short_name, n.full_name);
            run.note(&line);
        }
        let mut contention: PySet<String> = PySet::new();
        for &title in &titles {
            let n = run.n(title);
            if n.contains_titles.is_empty() {
                contention.add(n.short_name.clone());
            } else {
                for k in n.contains_titles.keys() {
                    last_contains = Some(k.clone());
                    contention.add(k.to_lowercase());
                }
            }
        }
        run.trace_ref("REF0120", &[]);
        let shown: Vec<String> = contention.iter().map(|x| py_repr(x)).collect();
        let shown = if shown.is_empty() {
            "set()".to_owned()
        } else {
            format!("{{{}}}", shown.join(", "))
        };
        run.note(&shown);

        let mut required: IndexMap<String, Vec<Id>> = IndexMap::new();
        let mut compare: Titles = Titles::new();
        for group in contention.iter() {
            let lower = group.to_lowercase();
            if let Some(titles) = filtered.get(&lower) {
                compare = titles.clone();
            } else {
                let name = last_contains.as_deref().ok_or_else(|| {
                    Crash("UnboundLocalError: cannot access local variable 'contains_title'".into())
                })?;
                let group_name = titletools::get_group_name(name, &run.s);
                if let Some(titles) = filtered.get(&group_name) {
                    compare = titles.clone();
                }
            }
            for &title in &compare {
                required.entry(group.clone()).or_default().push(title);
            }
        }
        let required: Vec<(String, String)> = required
            .iter()
            .map(|(k, v)| (k.clone(), run.n(v[0]).primary_region.clone()))
            .collect();
        run.trace_ref("REF0121", &[]);
        let shown: Vec<String> = required
            .iter()
            .map(|(k, r)| tuple_repr(&[py_repr(k), py_repr(r)]))
            .collect();
        run.note(&tuple_repr(&shown));

        let mut unique: HashSet<String> = HashSet::new();
        for &title in &titles {
            let name = run.n(title).full_name.clone();
            if unique.insert(name) {
                let g = grouping(run, title);
                available.entry(key.clone()).or_default().add(g);
            }
        }
        let groupings = available[&key].clone();
        let mut sorted_groupings = groupings.to_vec();
        sorted_groupings.sort();
        let mut candidates = compilation_candidates(
            run,
            &sorted_groupings,
            &required,
            contention.len(),
            Budget::default(),
        )?;
        if candidates.is_empty() || mode != "o" {
            candidates = vec![groupings.to_vec()];
        }
        run.trace_ref("REF0124", &[]);
        run.note(&candidates_repr(&candidates));

        let least = candidates.iter().map(Vec::len).min().unwrap_or(0);
        let flat = |candidate: &[Grouping]| -> Vec<String> {
            candidate.iter().flat_map(|g| g.0.iter().cloned()).collect()
        };
        let mut sorted_candidates = candidates.clone();
        sorted_candidates.sort();
        let mut smallest = 0;
        for candidate in &sorted_candidates {
            let items = flat(candidate);
            let length = candidate.len();
            let shown: Vec<String> = items.iter().map(|x| py_repr(x)).collect();
            run.trace_ref("REF0125", &[]);
            run.note(&format!("Candidate: {}", candidate_repr(candidate)));
            run.note(&format!(
                "Number of titles including dupes: {} (based on {})",
                items.len(),
                list_repr(&shown)
            ));
            run.note(&format!("Candidate length: {length}"));
            run.note(&format!(
                "Least possible elements in potential candidate: {least}"
            ));
            if length != least {
                run.note("Bad candidate, doesn't go to next stage");
            }
            if length == least && (items.len() < smallest || smallest == 0) {
                smallest = items.len();
            }
        }
        let stage_1: Vec<Vec<Grouping>> = sorted_candidates
            .iter()
            .filter(|c| flat(c).len() == smallest)
            .cloned()
            .collect();
        let empty_max = || Crash("ValueError: max() arg is an empty sequence".into());
        let longest = stage_1
            .iter()
            .flatten()
            .map(|g| g.0.len())
            .max()
            .ok_or_else(empty_max)?;
        let longest_overall = candidates
            .iter()
            .flatten()
            .map(|g| g.0.len())
            .max()
            .ok_or_else(empty_max)?;
        let stage_2: Vec<Vec<Grouping>> = if longest_overall == longest {
            stage_1.clone()
        } else {
            Vec::new()
        };
        let mut ideal_options: Vec<Vec<Grouping>> = stage_2
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.sort();
                c
            })
            .collect();
        ideal_options.sort();
        let ideal = ideal_options
            .into_iter()
            .next()
            .ok_or_else(|| Crash("IndexError: list index out of range".into()))?;
        let mut final_titles = Titles::new();
        if !ideal.is_empty() {
            for &title in &related[&key] {
                let g = grouping(run, title);
                if !ideal.contains(&g) {
                    continue;
                }
                final_titles.insert(title);
                if run.n(title).contains_titles.is_empty() {
                    let name = run.n(title).full_name.clone();
                    remove_from_group(run, &mut all, &name)?;
                }
                all.get_mut(WINNERS).expect("entry").insert(title);
            }
        }
        run.trace_ref("REF0123", &[]);
        run.note(&format!(
            "Stage 1 candidates (least number of duplicate titles):\t{}",
            candidates_repr(&stage_1)
        ));
        run.note(&format!(
            "Stage 2 candidates (longest grouping of titles):\t{}",
            candidates_repr(&stage_2)
        ));
        let shown: Vec<String> = ideal.iter().map(grouping_repr).collect();
        run.trace_title("REF0072", &shown, &final_titles, false);
    }

    let winner_names: HashSet<String> = all[WINNERS]
        .iter()
        .map(|&w| run.n(w).full_name.clone())
        .collect();
    let discards: Titles = comparison
        .iter()
        .copied()
        .filter(|&t| !winner_names.contains(&run.n(t).full_name))
        .collect();
    all.insert(DISCARDS.into(), discards);

    let by_name = |run: &Run, set: &Titles| {
        let mut list: Vec<Id> = set.iter().copied().collect();
        list.sort_by(|&a, &b| run.n(a).full_name.cmp(&run.n(b).full_name));
        list
    };
    let sorted_discards = by_name(run, &all[DISCARDS]);
    let sorted_winners = by_name(run, &all[WINNERS]);
    for &title in &sorted_discards {
        run.limits.check()?;
        for &winner in &sorted_winners {
            if !run.n(title).cloneof.is_empty() {
                continue;
            }
            let winner_name = parent_name(run, winner);
            let t = run.n(title);
            let w = run.n(winner);
            if !w.contains_titles.is_empty() {
                let title_contains = contained_lower(run, title);
                let matched = w.contains_titles.keys().any(|k| {
                    let k = k.to_lowercase();
                    if t.contains_titles.is_empty() {
                        k == t.short_name
                    } else {
                        title_contains.contains(&k)
                    }
                });
                if matched {
                    let old = parent_name(run, title);
                    run.m(title).cloneof = winner_name.clone();
                    let ids: Vec<Id> = all.values().flatten().copied().collect();
                    for id in ids {
                        if run.n(id).cloneof == old {
                            run.m(id).cloneof = winner_name.clone();
                        }
                    }
                }
            } else if t.contains_titles.is_empty() {
                if equivalent(run, winner, title, None) {
                    run.m(title).cloneof = winner_name;
                    break;
                }
            } else if contained_lower(run, title).contains(&w.short_name) {
                run.m(title).cloneof = winner_name;
                break;
            }
        }
    }
    let last_discard = sorted_discards.last().copied();
    for &winner in &sorted_winners {
        let winner_name = parent_name(run, winner);
        let full_name = run.n(winner).full_name.clone();
        run.trace_ref("REF0073", &[]);
        run.note(&format!("+ [1G1R title]\t{full_name}"));
        for &clone in &sorted_discards {
            if run.n(clone).cloneof != winner_name {
                continue;
            }
            let symbol = if Some(clone) == last_discard {
                '└'
            } else {
                '├'
            };
            let line = format!("{symbol} [clone]\t{}", run.n(clone).full_name);
            run.note(&line);
        }
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparing_compilations_stops_at_its_budget() {
        let settings = super::super::settings::Settings::new(
            &super::super::tests::internal_config(),
            &title_select_api::selection::SelectionProfile::default(),
            "",
        )
        .unwrap();
        let mut run = Run::new(settings);
        let groupings: Vec<Grouping> = (0..40)
            .map(|i| (vec![format!("game {i}")], "USA".to_owned()))
            .collect();
        let required = [("game 0".to_owned(), "USA".to_owned())];
        let mut within = |combinations, bytes| {
            compilation_candidates(
                &mut run,
                &groupings,
                &required,
                4,
                Budget {
                    combinations,
                    bytes,
                },
            )
        };
        // 40 + 780 + 9,880 combinations, of which 1 + 39 + 741 hold game 0.
        assert_eq!(within(10_700, 1 << 30).unwrap().len(), 781);
        let work = within(10_000, 1 << 30).err().unwrap();
        assert!(work.0.contains("40 choose 3"), "{}", work.0);
        let bytes = within(10_700, 10_000).err().unwrap();
        assert!(bytes.0.contains("bytes of candidates"), "{}", bytes.0);
    }

    #[test]
    fn combinations_follow_itertools() {
        let items = ["a", "b", "c"];
        let pick = |k| -> Vec<Vec<&str>> {
            combinations(items.len(), k)
                .map(|c| c.iter().map(|&i| items[i]).collect())
                .collect()
        };
        assert_eq!(
            pick(2),
            vec![vec!["a", "b"], vec!["a", "c"], vec!["b", "c"]]
        );
        assert_eq!(pick(0), vec![Vec::<&str>::new()]);
        assert_eq!(pick(4), Vec::<Vec<&str>>::new());
        // About 1.7e14 combinations, which only fit if they come one at a time.
        assert_eq!(combinations(100_000, 3).nth(2), Some(vec![0, 1, 4]));
        assert_eq!(binomial(5, 2), 10);
        assert_eq!(binomial(3, 4), 0);
        assert_eq!(binomial(200, 100), u128::MAX);
    }

    #[test]
    fn groupings_show_as_python_shows_them() {
        let one: Grouping = (vec!["a".into()], "USA".into());
        let two: Grouping = (vec!["a".into(), "b".into()], "Europe".into());
        assert_eq!(grouping_repr(&one), "(('a',), 'USA')");
        assert_eq!(
            candidates_repr(&[vec![one.clone()], vec![one, two]]),
            "[((('a',), 'USA'),), ((('a',), 'USA'), (('a', 'b'), 'Europe'))]"
        );
    }
}
