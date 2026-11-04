//! The clone list stages: MIA and RetroAchievements labels (`modules/clone_lists/mias.py`,
//! `retroachievements.py`), and the clone list's variants (`variants_orchestrator.py`, `variants.py`,
//! `local_names.py`).

use indexmap::IndexMap;

use super::json::{J, py_repr};
use super::node::{Contained, Groups, Id, Run, Titles};
use super::process::CloneList;
use super::py::{Crash, PyResult};
use super::pyre::Re;
use super::pyset::PySet;

/// Retool's `quick_lookup` indexes over the titles `process_dat` returns.
#[derive(Default)]
pub(crate) struct Lookup {
    pub crc: IndexMap<String, Titles>,
    pub md5: IndexMap<String, Titles>,
    pub sha1: IndexMap<String, Titles>,
    pub sha256: IndexMap<String, Titles>,
    pub full_name: IndexMap<String, Titles>,
    pub short_name: IndexMap<String, Titles>,
    pub region_free_name: IndexMap<String, Titles>,
}

impl Lookup {
    pub fn new(run: &Run, groups: &Groups) -> Lookup {
        let mut l = Lookup::default();
        for titles in groups.values() {
            for &id in titles {
                let n = run.n(id);
                l.short_name
                    .entry(n.short_name.clone())
                    .or_default()
                    .insert(id);
                l.full_name
                    .entry(n.full_name.clone())
                    .or_default()
                    .insert(id);
                l.region_free_name
                    .entry(n.region_free_name.clone())
                    .or_default()
                    .insert(id);
                for rom in &n.roms {
                    l.crc.entry(rom.crc.clone()).or_default().insert(id);
                    l.md5.entry(rom.md5.clone()).or_default().insert(id);
                    l.sha1.entry(rom.sha1.clone()).or_default().insert(id);
                    l.sha256.entry(rom.sha256.clone()).or_default().insert(id);
                }
            }
        }
        l
    }
}

/// `repr` of a list of strings.
pub(crate) fn repr_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| py_repr(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn key_error(key: &str) -> Crash {
    Crash(format!("KeyError: {}", py_repr(key)))
}

/// `mias`.
pub(crate) fn mias(run: &mut Run, lookup: &Lookup, clone_list: &CloneList) -> PyResult<()> {
    if clone_list.mias.is_empty() {
        return Ok(());
    }
    run.note("• Applying MIA attributes... ");
    for mia in &clone_list.mias {
        let J::Dict(mia) = mia else {
            continue;
        };
        let found: Titles = match mia.get("crc") {
            Some(J::Str(crc)) => lookup.crc.get(crc).cloned().unwrap_or_default(),
            Some(_) => Titles::new(),
            None => lookup.crc.get("").cloned().unwrap_or_default(),
        };
        for id in found {
            let roms = run.n(id).roms.clone();
            for (i, rom) in roms.iter().enumerate() {
                let crc_match =
                    mia.get("crc").ok_or_else(|| key_error("crc"))?.as_str() == Some(&rom.crc);
                if !crc_match {
                    continue;
                }
                let name_match =
                    mia.get("name").ok_or_else(|| key_error("name"))?.as_str() == Some(&rom.name);
                let full_name = run.n(id).full_name.clone();
                let node = run.m(id);
                node.roms[i].mia = "yes".into();
                node.is_mia = true;
                if name_match {
                    run.trace_ref("REF0135", &[full_name, rom.crc.clone(), rom.name.clone()]);
                } else {
                    run.trace_ref("REF0134", &[full_name, rom.crc.clone()]);
                }
                run.note(&format!("• {}", rom.name));
            }
        }
    }
    run.note("• Applying MIA attributes... done");
    Ok(())
}

/// `retroachievements`.
pub(crate) fn retroachievements(run: &mut Run, lookup: &Lookup, clone_list: &CloneList) {
    if clone_list.retroachievements.is_empty() {
        return;
    }
    run.note("• Applying RetroAchievements attributes... ");
    for entry in &clone_list.retroachievements {
        let J::Dict(entry) = entry else {
            continue;
        };
        let mut found = Titles::new();
        for (key, index) in [
            ("sha256", &lookup.sha256),
            ("sha1", &lookup.sha1),
            ("md5", &lookup.md5),
            ("crc", &lookup.crc),
        ] {
            if let Some(value) = entry.get(key) {
                if let Some(titles) = value.as_str().and_then(|v| index.get(v)) {
                    found = titles.clone();
                }
                break;
            }
        }
        for id in found {
            run.m(id).is_retroachievement = true;
            let name = run.n(id).full_name.clone();
            run.trace_ref("REF0136", &[name]);
        }
    }
    run.note("• Applying RetroAchievements attributes... done");
}

/// What the variants stage threads through each list of variant titles.
struct Variants<'a> {
    lookup: &'a Lookup,
    missing: PySet<String>,
    /// `deleted_titles`, a set of `(title, group)` pairs. Retool iterates it in hash order, which only decides the
    /// order empty groups go in, so insertion order stands in for it.
    deleted: IndexMap<(Id, String), ()>,
    is_includes: bool,
}

/// `clone_list_variants_orchestrator`.
pub(crate) fn variants_orchestrator(
    run: &mut Run,
    groups: &mut Groups,
    lookup: &Lookup,
    clone_list: &mut CloneList,
    is_includes: bool,
) -> PyResult<()> {
    if clone_list.variants.is_empty() {
        return Ok(());
    }
    run.note("• Processing clone list variants... ");
    let mut state = Variants {
        lookup,
        missing: PySet::new(),
        deleted: IndexMap::new(),
        is_includes,
    };
    for value in clone_list.variants.iter_mut() {
        let J::Dict(value) = value else {
            continue;
        };
        if !(value.contains_key("titles")
            || value.contains_key("compilations")
            || value.contains_key("supersets"))
        {
            continue;
        }
        for title_type in ["titles", "compilations", "supersets"] {
            if value.contains_key("ignore")
                && let Some(entries) = value.get_mut(title_type)
            {
                for entry in iter_mut(entries)? {
                    let entry = dict_mut(entry)?;
                    if !entry.contains_key("ignore") {
                        entry.insert("ignore".into(), J::Bool(true));
                    }
                }
            }
            if let Some(categories @ J::List(_)) = value.get("categories").cloned()
                && let Some(entries) = value.get_mut(title_type)
            {
                for entry in iter_mut(entries)? {
                    let entry = dict_mut(entry)?;
                    if !entry.contains_key("categories") {
                        entry.insert("categories".into(), categories.clone());
                    }
                }
            }
            if let Some(mut entries) = value.get(title_type).cloned() {
                let kind = &title_type[..title_type.len() - 1];
                clone_list_variants(run, groups, &mut state, &mut entries, kind, value)?;
                value.insert(title_type.into(), entries);
            }
        }
    }
    for ((id, old_group), ()) in std::mem::take(&mut state.deleted) {
        let group = groups
            .get_mut(&old_group)
            .ok_or_else(|| key_error(&old_group))?;
        group.shift_remove(&id);
        if group.is_empty() {
            groups.shift_remove(&old_group);
        }
    }
    if !is_includes {
        run.trace.clone_list_unmatched = state.missing.to_vec();
    }
    run.note("• Processing clone list variants... done");
    Ok(())
}

fn iter_mut(value: &mut J) -> PyResult<std::slice::IterMut<'_, J>> {
    match value {
        J::List(l) => Ok(l.iter_mut()),
        _ => Err(Crash(format!(
            "TypeError: a clone list variants list that isn't an array: {}",
            value.py_repr()
        ))),
    }
}

fn dict_mut(value: &mut J) -> PyResult<&mut IndexMap<String, J>> {
    match value {
        J::Dict(d) => Ok(d),
        _ => Err(Crash(format!(
            "TypeError: a clone list variant title that isn't an object: {}",
            value.py_repr()
        ))),
    }
}

/// The items `for x in value` meets.
fn items(value: &J) -> PyResult<Vec<J>> {
    match value {
        J::List(l) => Ok(l.clone()),
        J::Dict(d) => Ok(d.keys().map(|k| J::Str(k.clone())).collect()),
        J::Str(s) => Ok(s.chars().map(|c| J::Str(c.into())).collect()),
        _ => Err(Crash(format!(
            "TypeError: {} isn't iterable",
            value.py_repr()
        ))),
    }
}

fn strings(value: &J) -> PyResult<Vec<String>> {
    Ok(items(value)?.iter().map(J::py_str).collect())
}

/// A region list the clone list names; Retool appends to it, which only this run sees.
fn region_list(value: &J) -> PyResult<Vec<String>> {
    match value {
        J::List(l) => Ok(l.iter().map(J::py_str).collect()),
        _ => Err(Crash(format!(
            "AttributeError: a region list that isn't an array: {}",
            value.py_repr()
        ))),
    }
}

/// A priority Retool stores as it reads it. Title Select's clone lists only hold numbers here.
fn int(value: &J) -> PyResult<i64> {
    match value {
        J::Int(i) => Ok(*i),
        J::Bool(b) => Ok(i64::from(*b)),
        _ => Err(Crash(format!(
            "TypeError: a priority that isn't a whole number: {}",
            value.py_repr()
        ))),
    }
}

/// `x in container` for a JSON container and a string.
fn contains(container: &J, item: &str) -> PyResult<bool> {
    match container {
        J::List(l) => Ok(l.iter().any(|x| x.as_str() == Some(item))),
        J::Dict(d) => Ok(d.contains_key(item)),
        J::Str(s) => Ok(s.contains(item)),
        _ => Err(Crash(format!(
            "TypeError: {} isn't a container",
            container.py_repr()
        ))),
    }
}

/// `title in processed_titles[title.group_name]`.
fn in_own_group(run: &Run, groups: &Groups, title: Id) -> PyResult<bool> {
    let group = &run.n(title).group_name;
    Ok(groups
        .get(group)
        .ok_or_else(|| key_error(group))?
        .contains(&title))
}

/// `variables` that outlive each variant title in `clone_list_variants`: a filter's region lists, which a later
/// filter without them reuses.
#[derive(Default)]
struct RegionLists {
    higher: Option<Vec<String>>,
    lower: Option<Vec<String>>,
}

/// `clone_list_variants`.
fn clone_list_variants(
    run: &mut Run,
    groups: &mut Groups,
    state: &mut Variants<'_>,
    entries: &mut J,
    kind: &str,
    contents: &IndexMap<String, J>,
) -> PyResult<()> {
    let mut variants: Vec<Id> = Vec::new();
    let mut report_on_match = false;
    let mut new_group_name = String::new();
    let mut lists = RegionLists::default();
    for entry in iter_mut(entries)? {
        let variant = dict_mut(entry)?;
        let Some(search_term) = variant.get("searchTerm").cloned() else {
            run.trace.warn(
                "• The following variants entry is missing a searchTerm key and will be skipped:",
            );
            run.trace
                .warn(&format!("\n  {}", J::Dict(variant.clone()).py_repr()));
            continue;
        };
        let name = search_term.py_str();
        let name_type = match variant.get("nameType") {
            None => "short",
            Some(t) => match t.as_str() {
                Some(t @ ("full" | "short" | "regionFree" | "regex")) => t,
                _ => "short",
            },
        }
        .to_owned();
        let lookup = state.lookup;
        let mut found = Titles::new();
        match name_type.as_str() {
            "regex" => {
                if Re::new(&name).is_err() {
                    run.trace.warn(
                        "• The following regex in the clone list's variants object is invalid and will be skipped:\n",
                    );
                    run.trace.warn(&format!("  • {name}"));
                    continue;
                }
                let pattern =
                    Re::new(&name.to_lowercase()).map_err(|e| Crash(format!("re.error: {e}")))?;
                for (full_name, titles) in &lookup.full_name {
                    if !pattern.find(&full_name.to_lowercase()).is_empty() {
                        found.extend(titles.iter().copied());
                    }
                }
            }
            "full" => found = lookup.full_name.get(&name).cloned().unwrap_or_default(),
            "short" => {
                found = lookup
                    .short_name
                    .get(&name.to_lowercase())
                    .cloned()
                    .unwrap_or_default()
            }
            _ => {
                found = lookup
                    .region_free_name
                    .get(&name)
                    .cloned()
                    .unwrap_or_default()
            }
        }
        let mut old_group_names: PySet<String> = PySet::new();
        for &id in &found {
            old_group_names.add(run.n(id).group_name.clone());
        }
        new_group_name = group_of(contents)?.to_lowercase();
        groups.entry(new_group_name.clone()).or_default();
        if found.is_empty() {
            state.missing.add(name.clone());
            continue;
        }
        report_on_match = true;
        for &title in &found {
            if let Some(J::Bool(true)) = variant.get("ignore")
                && in_own_group(run, groups, title)?
            {
                run.trace_ref("REF0054", &[]);
                let full_name = run.n(title).full_name.clone();
                run.note(&format!("- Remove: {full_name}"));
                let group = run.n(title).group_name.clone();
                groups
                    .get_mut(&group)
                    .expect("checked")
                    .shift_remove(&title);
                run.m(title).exclude_reason = "Clone list remove".into();
                run.remove_into("clone_list_ignores", title);
            }
            if let Some(categories @ J::List(_)) = variant.get("categories")
                && in_own_group(run, groups, title)?
            {
                run.trace_ref("REF0053", &[]);
                let full_name = run.n(title).full_name.clone();
                let old = repr_list(&run.n(title).categories);
                run.note(&format!("• {full_name}"));
                run.note(&format!(
                    "  New categories: {}\n  Old categories: {old}",
                    categories.py_repr()
                ));
                run.m(title).categories = strings(categories)?;
            }
            for old_group_name in old_group_names.to_vec() {
                let in_old = groups
                    .get(&old_group_name)
                    .ok_or_else(|| key_error(&old_group_name))?
                    .contains(&title);
                if !in_old {
                    continue;
                }
                if kind == "title" || kind == "superset" {
                    let new_title = run.copy(title);
                    if let Some(priority) = variant.get("priority")
                        && run.n(new_title).clonelist_priority == 1
                    {
                        run.m(new_title).clonelist_priority = int(priority)?;
                    }
                    if kind == "superset" {
                        run.m(new_title).is_superset = true;
                    }
                    for group in old_group_names.to_vec() {
                        let held = groups
                            .get(&group)
                            .ok_or_else(|| key_error(&group))?
                            .contains(&title);
                        if held {
                            state.deleted.insert((title, group), ());
                        }
                    }
                    if variant.contains_key("localNames") {
                        let local_names = variant["localNames"].clone();
                        local_names_apply(run, &local_names, new_title)?;
                    }
                    if let Some(J::Bool(true)) = variant.get("englishFriendly")
                        && in_own_group(run, groups, title)?
                    {
                        run.trace_ref("REF0112", &[]);
                        let full_name = run.n(new_title).full_name.clone();
                        run.note(&format!("• {full_name}"));
                        if !run.n(new_title).languages.iter().any(|l| l == "En") {
                            run.m(new_title).languages.push("En".into());
                        }
                    }
                    if let Some(J::Bool(true)) = variant.get("isOldest")
                        && in_own_group(run, groups, title)?
                    {
                        run.trace_ref("REF0129", &[]);
                        let full_name = run.n(new_title).full_name.clone();
                        run.note(&format!("• {full_name}"));
                        run.m(new_title).is_oldest = true;
                    }
                    if let Some(filters) = variant.get("filters").cloned() {
                        apply_filters(
                            run,
                            &filters,
                            &search_term,
                            title,
                            new_title,
                            contents,
                            &mut lists,
                            state.is_includes,
                        )?;
                    }
                    variants.push(new_title);
                } else if kind == "compilation" {
                    let members: Vec<Id> = groups[&old_group_name].iter().copied().collect();
                    for compilation in members {
                        if run.n(compilation).full_name != run.n(title).full_name {
                            continue;
                        }
                        let position = variant.get("titlePosition").cloned().unwrap_or(J::Int(1));
                        let priority = variant.get("priority").cloned().unwrap_or(J::Int(1));
                        let group = group_of(contents)?.to_owned();
                        run.m(compilation)
                            .contains_titles
                            .insert(group, Contained { position, priority });
                        if variant.contains_key("localNames") {
                            let local_names = variant["localNames"].clone();
                            local_names_apply(run, &local_names, compilation)?;
                        }
                    }
                }
            }
        }
    }
    for variant in variants {
        let conditional = run.n(variant).group_name_conditional.clone();
        if !conditional.is_empty() {
            let node = run.m(variant);
            node.group_name = conditional.clone();
            node.short_name = conditional.to_lowercase();
            groups.entry(conditional).or_default().insert(variant);
        } else {
            if report_on_match && run.n(variant).group_name != new_group_name && !state.is_includes
            {
                run.trace_ref("REF0055", &[]);
                let n = run.n(variant);
                let lines = format!(
                    "• {}\n  New group: {new_group_name}\n  Old group: {}",
                    n.full_name, n.group_name
                );
                run.note(&lines);
            }
            let short_name = group_of(contents)?.to_lowercase();
            let node = run.m(variant);
            node.group_name = new_group_name.clone();
            node.short_name = short_name;
            groups
                .get_mut(&new_group_name)
                .ok_or_else(|| key_error(&new_group_name))?
                .insert(variant);
        }
    }
    Ok(())
}

/// `clone_list_group_contents['group']`.
fn group_of(contents: &IndexMap<String, J>) -> PyResult<&str> {
    contents
        .get("group")
        .ok_or_else(|| key_error("group"))?
        .as_str()
        .ok_or_else(|| Crash("AttributeError: a group that isn't a string".into()))
}

/// The warning Retool gives for a region order condition it can't read, which then fails to name the variant
/// title.
fn region_order_warning(run: &mut Run, text: &str, contents: &IndexMap<String, J>) -> PyResult<()> {
    run.trace.warn(text);
    let term = contents
        .get("searchTerm")
        .ok_or_else(|| key_error("searchTerm"))?;
    run.trace.warn(&format!("\n  {}", term.py_str()));
    Ok(())
}

/// The variant title's `filters`: conditions, and the results they bring when they hold.
#[allow(clippy::too_many_arguments)]
fn apply_filters(
    run: &mut Run,
    filters: &J,
    search_term: &J,
    title: Id,
    new_title: Id,
    contents: &IndexMap<String, J>,
    lists: &mut RegionLists,
    is_includes: bool,
) -> PyResult<()> {
    let first = match filters {
        J::List(l) => l.first(),
        _ => None,
    };
    let conditions_found = first.and_then(|f| f.get("conditions")).is_some();
    let results_found = first.and_then(|f| f.get("results")).is_some();
    let missing = match (conditions_found, results_found) {
        (false, false) => "conditions and results object",
        (false, true) => "conditions object",
        (true, false) => "results object",
        (true, true) => "",
    };
    if !missing.is_empty() {
        run.trace.warn(&format!(
            "• The following variant title's filters array is missing a {missing}, so no changes have been made:"
        ));
        run.trace.warn(&format!("\n  {}", search_term.py_str()));
        return Ok(());
    }
    for filter in items(filters)? {
        let conditions = filter
            .get("conditions")
            .ok_or_else(|| key_error("conditions"))?;
        let mut ok = true;
        if let Some(languages) = conditions.get("matchLanguages")
            && !strings(languages)?
                .iter()
                .all(|x| run.n(title).languages.contains(x))
        {
            ok = false;
        }
        if let Some(regions) = conditions.get("matchRegions")
            && !strings(regions)?
                .iter()
                .all(|x| run.n(title).regions.contains(x))
        {
            ok = false;
        }
        if let Some(pattern) = conditions.get("matchString") {
            let pattern = pattern
                .as_str()
                .ok_or_else(|| Crash("TypeError: matchString isn't a string".into()))?;
            let pattern = Re::new(pattern).map_err(|e| Crash(format!("re.error: {e}")))?;
            if pattern.find(&run.n(title).full_name).is_empty() {
                ok = false;
            }
        }
        if let Some(order) = conditions.get("regionOrder") {
            match order.get("higherRegions") {
                Some(value) => lists.higher = Some(region_list(value)?),
                None => region_order_warning(
                    run,
                    "• The following exception's higherRegions key isn't an array and will be skipped:",
                    contents,
                )?,
            }
            match order.get("lowerRegions") {
                Some(value) => lists.lower = Some(region_list(value)?),
                None => region_order_warning(
                    run,
                    "• The following exception's lowerRegions key isn't an array and will be skipped:",
                    contents,
                )?,
            }
            let unbound = || Crash("UnboundLocalError: a region order condition".into());
            let other = "All other regions";
            let has = |list: &Vec<String>, x: &str| list.iter().any(|r| r == x);
            if has(lists.higher.as_ref().ok_or_else(unbound)?, other)
                && has(lists.lower.as_ref().ok_or_else(unbound)?, other)
            {
                region_order_warning(
                    run,
                    "• The following exception has All other regions in both the higherRegions and lowerRegions keys, and will be skipped:",
                    contents,
                )?;
                continue;
            }
            for list in [lists.higher.as_mut(), lists.lower.as_mut()]
                .into_iter()
                .flatten()
            {
                if has(list, "UK") && !has(list, "United Kingdom") {
                    list.push("United Kingdom".into());
                }
                if has(list, "United Kingdom") && !has(list, "UK") {
                    list.push("UK".into());
                }
            }
            let default = &run.s.region_order_default;
            if lists.higher.as_deref() == Some(&[other.to_owned()][..]) {
                let lower = lists.lower.as_ref().ok_or_else(unbound)?;
                lists.higher = Some(
                    default
                        .iter()
                        .filter(|x| !lower.contains(x))
                        .cloned()
                        .collect(),
                );
            }
            if lists.lower.as_deref() == Some(&[other.to_owned()][..]) {
                let higher = lists.higher.as_ref().ok_or_else(unbound)?;
                lists.lower = Some(
                    default
                        .iter()
                        .filter(|x| !higher.contains(x))
                        .cloned()
                        .collect(),
                );
            }
            let region_order = run.s.region_order();
            let higher_list = lists.higher.as_ref().ok_or_else(unbound)?;
            let lower_list = lists.lower.as_ref().ok_or_else(unbound)?;
            let higher: Vec<usize> = (0..region_order.len())
                .filter(|&i| higher_list.contains(&region_order[i]))
                .collect();
            let lowest = (0..region_order.len()).find(|&i| lower_list.contains(&region_order[i]));
            let region_ok = match lowest {
                Some(lowest) if !higher.is_empty() => higher.iter().any(|&h| h < lowest),
                _ => false,
            };
            if !region_ok {
                ok = false;
            }
        }
        if !ok {
            continue;
        }
        let results = filter.get("results").ok_or_else(|| key_error("results"))?;
        if let Some(categories) = results.get("categories") {
            run.trace_ref("REF0108", &[]);
            let full_name = run.n(title).full_name.clone();
            let old = repr_list(&run.n(title).categories);
            run.note(&format!("• {full_name}"));
            run.note(&format!(
                "  New categories: {}\n  Old categories: {old}",
                categories.py_repr()
            ));
            run.m(new_title).categories = strings(categories)?;
        }
        if let Some(group) = results.get("group")
            && !run.n(new_title).group_moved_by_condition
        {
            let group = group
                .as_str()
                .ok_or_else(|| Crash("AttributeError: a group that isn't a string".into()))?
                .to_lowercase();
            if run.n(new_title).group_name != group && !is_includes {
                run.trace_ref("REF0109", &[]);
                let n = run.n(new_title);
                let text = format!(
                    "• {}\n  New group: {group}\n  Old group: {}",
                    n.full_name, n.group_name
                );
                run.note(&text);
            }
            let node = run.m(new_title);
            node.group_name_conditional = group;
            node.group_moved_by_condition = true;
        }
        if let Some(local_names) = results.get("localNames") {
            run.trace_ref("REF0115", &[]);
            let n = run.n(new_title);
            let text = format!("  New name: {}\n  Old name: {}", n.local_name, n.full_name);
            run.note(&text);
            local_names_apply(run, local_names, new_title)?;
        }
        if let Some(J::Bool(true)) = results.get("englishFriendly") {
            run.trace_ref("REF0113", &[]);
            let full_name = run.n(new_title).full_name.clone();
            run.note(&format!("• {full_name}"));
            if !run.n(title).languages.iter().any(|l| l == "En") {
                run.m(new_title).languages.push("En".into());
            }
        }
        if let Some(J::Bool(true)) = results.get("isOldest") {
            run.trace_ref("REF0132", &[]);
            let full_name = run.n(new_title).full_name.clone();
            run.note(&format!("• {full_name}"));
            run.m(new_title).is_oldest = true;
        }
        if let Some(priority) = results.get("priority") {
            run.trace_ref("REF0110", &[]);
            let n = run.n(new_title);
            let text = format!(
                "• {}\n  New priority: {}\n  Old priority: {}",
                n.full_name,
                priority.py_str(),
                n.clonelist_priority
            );
            run.note(&text);
            run.m(new_title).clonelist_priority = int(priority)?;
        }
        if let Some(superset) = results.get("superset")
            && superset.truthy()
        {
            run.trace_ref("REF0116", &[]);
            let full_name = run.n(title).full_name.clone();
            run.note(&format!("• {full_name}"));
            run.m(new_title).is_superset = true;
        }
    }
    Ok(())
}

/// `clone_list_local_names`.
fn local_names_apply(run: &mut Run, local_names: &J, id: Id) -> PyResult<()> {
    if !local_names.truthy() {
        return Ok(());
    }
    let codes: Vec<String> = match &run.s.system_localization_order_user {
        Some(order) => order.clone(),
        None => run.s.localization_order_user.clone(),
    };
    let mut names: Vec<String> = Vec::new();
    for code in &codes {
        if let Some((name, _)) = run.s.languages.iter().find(|(_, value)| *value == code) {
            names.push(name.to_lowercase());
        }
    }
    for language in &names {
        if contains(local_names, language)? {
            let value = match local_names {
                J::Dict(d) => d[language].py_str(),
                _ => {
                    return Err(Crash("TypeError: localNames isn't an object".into()));
                }
            };
            run.m(id).local_name = value;
            break;
        }
    }
    if !run.n(id).local_name.is_empty() {
        let tail = Re::of(" \\(.*").find(&run.n(id).full_name);
        let node = run.m(id);
        node.local_name = format!("{}{tail}", node.local_name);
    }
    run.trace_ref("REF0111", &[]);
    let n = run.n(id);
    let text = format!("  New name: {}\n  Old name: {}", n.local_name, n.full_name);
    run.note(&text);
    Ok(())
}
