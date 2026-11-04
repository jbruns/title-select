//! Retool's `choose_*` helpers (`modules/title_selection`), which each narrow a set of candidate titles.
//!
//! Each helper takes the set it narrows and changes it in place, as Retool's do. Retool iterates the pairs of a
//! set with `itertools.combinations` in the set's (address) order; these iterate in insertion order.

use std::cmp::Ordering;

use super::json::py_repr;
use super::node::{Id, Run, Titles};
use super::py::{self, Crash, PyResult};
use super::pyre::Re;
use super::pyset::{PyHash, PySet, tuple_hash};
use super::settings::{Settings, Tag};
use super::titletools::{self, VersionValue};

/// `itertools.combinations(titles, 2)`.
pub(crate) fn pairs(titles: &Titles) -> Vec<(Id, Id)> {
    let list: Vec<Id> = titles.iter().copied().collect();
    let mut out = Vec::with_capacity(list.len() * list.len().saturating_sub(1) / 2);
    for (i, &a) in list.iter().enumerate() {
        for &b in &list[i + 1..] {
            out.push((a, b));
        }
    }
    out
}

/// `TitleTools.check_title_equivalence`.
pub(crate) fn equivalent(run: &Run, a: Id, b: Id, within: Option<&Titles>) -> bool {
    if let Some(set) = within
        && !set.is_empty()
        && !(set.contains(&a) && set.contains(&b))
    {
        return false;
    }
    let (x, y) = (run.n(a), run.n(b));
    x.short_name == y.short_name
        && x.is_demo == y.is_demo
        && !x.categories.iter().any(|c| c == "BIOS")
        && !y.categories.iter().any(|c| c == "BIOS")
}

/// Removes `remove` from `titles`, keeping the order of the rest.
pub(crate) fn remove_all(titles: &mut Titles, remove: &Titles) {
    for id in remove {
        titles.shift_remove(id);
    }
}

/// What `choose_string` looks for: a pattern, or a tag.
#[derive(Clone, Copy)]
pub(crate) enum Pat<'a> {
    Re(&'a Re),
    Str(&'a str),
}

impl<'a> Pat<'a> {
    pub fn of(tag: &'a Tag) -> Pat<'a> {
        match tag {
            Tag::Regex(re) => Pat::Re(re),
            Tag::String(s) => Pat::Str(s),
        }
    }

    /// `str(search_string)`.
    fn py_str(&self) -> String {
        match self {
            Pat::Re(re) => re.py_str(),
            Pat::Str(s) => (*s).to_owned(),
        }
    }
}

/// The `regex_string` Retool shows for a pattern: `str(pattern)` without `re.compile(` and its closing parenthesis.
pub(crate) fn regex_string(pattern: &str) -> String {
    let s = pattern
        .replace("re.compile(", "")
        .replace(", re.IGNORECASE)", "");
    match s.strip_suffix("')") {
        Some(rest) => format!("{rest}'"),
        None => s,
    }
}

/// `choose_good`: prefers good, production dumps, and a revision over a version.
pub(crate) fn good(run: &mut Run, titles: &mut Titles) {
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let r = &run.s.regex;
        let (na, nb) = (&run.n(a).full_name, &run.n(b).full_name);
        let mut patterns: Vec<&Re> = r.preproduction().to_vec();
        patterns.push(&r.bad);
        patterns.push(&r.pirate);
        let (mut a_check, mut b_check) = (false, false);
        for p in patterns {
            let (ma, mb) = (p.search(na), p.search(nb));
            if ma && !mb {
                a_check = true;
            }
            if mb && !ma {
                b_check = true;
            }
        }
        if a_check && !b_check && titles.contains(&a) {
            remove.insert(a);
            continue;
        }
        if b_check && !a_check && titles.contains(&b) {
            remove.insert(b);
            continue;
        }
        if r.revision.search(na) && r.version.search(nb) {
            if titles.contains(&a) {
                remove.insert(a);
            }
        } else if r.revision.search(nb) && r.version.search(na) && titles.contains(&b) {
            remove.insert(b);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_highest_string`.
pub(crate) fn highest_string(run: &mut Run, titles: &mut Titles) {
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (na, nb) = (run.n(a).full_name.clone(), run.n(b).full_name.clone());
        if na < nb {
            run.trace_keep("REF0057", &nb, &na);
            remove.insert(a);
        } else if nb < na {
            run.trace_keep("REF0058", &na, &nb);
            remove.insert(b);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_made_in`.
pub(crate) fn made_in(run: &mut Run, titles: &mut Titles) {
    let mut remove = Titles::new();
    let shown = run
        .s
        .regex
        .madein
        .py_str()
        .replace("re.compile(", "")
        .replace(", re.IGNORECASE)", "");
    for (a, b) in pairs(titles) {
        let r = &run.s.regex.madein;
        let (x, y) = (run.n(a), run.n(b));
        let found_1 = r
            .find(&x.full_name)
            .replace('.', "")
            .replace("EU", "Europe");
        let found_2 = r
            .find(&y.full_name)
            .replace('.', "")
            .replace("EU", "Europe");
        if !(r.search(&x.full_name) && r.search(&y.full_name)) {
            continue;
        }
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        let (ra, rb) = (x.primary_region.clone(), y.primary_region.clone());
        if found_1.contains(&ra) && !found_2.contains(&ra) && titles.contains(&b) {
            run.trace_keep(
                "REF0036",
                &format!("({shown}) {na}"),
                &format!("({shown}) {nb}"),
            );
            remove.insert(b);
        }
        if found_2.contains(&rb) && !found_1.contains(&rb) && titles.contains(&a) {
            run.trace_keep(
                "REF0037",
                &format!("({shown}) {nb}"),
                &format!("({shown}) {na}"),
            );
            remove.insert(a);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_priority`: prefers the clone list's priority, or its oldest title. It removes as it goes.
pub(crate) fn priority(run: &mut Run, titles: &mut Titles) {
    let oldest = run.s.options.oldest;
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        let (pa, pb) = (x.clonelist_priority, y.clonelist_priority);
        if oldest && (x.is_oldest || y.is_oldest) {
            if x.is_oldest {
                run.trace_ref("REF0130", &[]);
                run.note(&format!("+ Keeping:  ((Oldest) {na}"));
                run.note(&format!("- Removing: {nb}"));
                titles.shift_remove(&b);
            } else {
                run.trace_ref("REF0131", &[]);
                run.note(&format!("+ Keeping:  ((Oldest) {nb}"));
                run.note(&format!("- Removing: {na}"));
                titles.shift_remove(&a);
            }
            continue;
        }
        let both_supersets = x.is_superset && y.is_superset;
        let (first, second) = if both_supersets {
            ("REF0022", "REF0023")
        } else {
            ("REF0020", "REF0021")
        };
        if pa < pb {
            run.trace_ref(first, &[]);
            run.note(&format!("+ Keeping:  ({pa}) {na}"));
            run.note(&format!("- Removing: ({pb}) {nb}"));
            titles.shift_remove(&b);
        } else if pb < pa {
            run.trace_ref(second, &[]);
            run.note(&format!("+ Keeping:  ({pb}) {nb}"));
            run.note(&format!("- Removing: ({pa}) {na}"));
            titles.shift_remove(&a);
        }
    }
}

/// `choose_region`.
pub(crate) fn region(
    run: &mut Run,
    titles: &mut Titles,
    region_order: &[String],
    world_is_usa_europe_japan: bool,
) {
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let has = |regions: &[String], r: &str| regions.iter().any(|x| x == r);
        if world_is_usa_europe_japan {
            let world_1 = x.primary_region == "World";
            let world_2 = y.primary_region == "World";
            if ["USA", "Europe", "Japan"]
                .iter()
                .any(|r| (world_1 && has(&y.regions, r)) || (has(&x.regions, r) && world_2))
            {
                continue;
            }
        }
        if !x.contains_titles.is_empty() != !y.contains_titles.is_empty() {
            let (pa, pb) = (x.region_priority, y.region_priority);
            let (na, nb) = (x.full_name.clone(), y.full_name.clone());
            if pa < pb {
                run.trace_keep("REF0094", &format!("({pa}) {na}"), &format!("({pb}) {nb}"));
                remove.insert(b);
            }
            if pb < pa {
                run.trace_keep("REF0095", &format!("({pb}) {nb}"), &format!("({pa}) {na}"));
                remove.insert(a);
            }
        } else {
            for r in region_order {
                let (x, y) = (run.n(a), run.n(b));
                let shown =
                    |n: &super::node::Node| format!("({}) {}", n.regions.join(", "), n.full_name);
                if has(&x.regions, r) && !has(&y.regions, r) {
                    if titles.contains(&b) {
                        let (k, d) = (shown(x), shown(y));
                        run.trace_keep("REF0030", &k, &d);
                        remove.insert(b);
                        break;
                    }
                } else if has(&y.regions, r) && !has(&x.regions, r) && titles.contains(&a) {
                    let (k, d) = (shown(y), shown(x));
                    run.trace_keep("REF0031", &k, &d);
                    remove.insert(a);
                    break;
                }
            }
        }
    }
    remove_all(titles, &remove);
}

/// `choose_retroachievements`.
pub(crate) fn retroachievements(run: &mut Run, titles: &mut Titles) {
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        if x.is_retroachievement && !y.is_retroachievement && titles.contains(&a) {
            remove.insert(b);
            if !na.is_empty() {
                run.trace_keep("REF0137", &na, &nb);
            }
            continue;
        }
        if y.is_retroachievement && !x.is_retroachievement && titles.contains(&b) {
            remove.insert(a);
            run.trace_keep("REF0138", &nb, &na);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_superset`.
pub(crate) fn superset(run: &mut Run, titles: &mut Titles) {
    let oldest = run.s.options.oldest;
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        if x.is_superset && !y.is_superset && titles.contains(&a) {
            let (keep, drop) = if oldest {
                remove.insert(a);
                (nb, na)
            } else {
                remove.insert(b);
                (na, nb)
            };
            if !keep.is_empty() {
                run.trace_keep("REF0077", &keep, &drop);
            }
            continue;
        }
        if y.is_superset && !x.is_superset && titles.contains(&b) {
            let (keep, drop) = if oldest {
                remove.insert(b);
                (na, nb)
            } else {
                remove.insert(a);
                (nb, na)
            };
            run.trace_keep("REF0078", &keep, &drop);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_string`: prefers the title that matches `pat` (`with` true) or the one that doesn't.
pub(crate) fn string(run: &mut Run, titles: &mut Titles, pat: Pat<'_>, with: bool) {
    let mut remove = Titles::new();
    let shown = regex_string(&pat.py_str());
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let (mut m1, mut m2) = (false, false);
        match pat {
            Pat::Re(re) => {
                let (s1, s2) = (re.search(&x.full_name), re.search(&y.full_name));
                m1 = s1 && !s2;
                m2 = s2 && !s1;
            }
            Pat::Str(tag) => {
                let tag = tag.to_owned();
                let (t1, t2) = (x.tags.contains(&tag), y.tags.contains(&tag));
                if t1 && !t2 {
                    m1 = true;
                } else if t2 && !t1 {
                    m2 = true;
                }
            }
        }
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        let ka = format!("{shown} {na}");
        let kb = format!("{shown} {nb}");
        let dropped = if m1 && !m2 {
            if with {
                run.trace_keep("REF0032", &ka, &kb);
                b
            } else {
                run.trace_keep("REF0033", &kb, &ka);
                a
            }
        } else if m2 && !m1 {
            if with {
                run.trace_keep("REF0034", &kb, &ka);
                a
            } else {
                run.trace_keep("REF0035", &ka, &kb);
                b
            }
        } else {
            continue;
        };
        if titles.contains(&dropped) {
            remove.insert(dropped);
        }
    }
    remove_all(titles, &remove);
}

/// `choose_video_standard`.
pub(crate) fn video_standard(run: &mut Run, titles: &mut Titles, standard: &str) {
    let standard = standard.replace(' ', "_");
    let count = run.s.regex.video_standard(&standard).len();
    for i in 0..count {
        let re = run.s.regex.video_standard(&standard)[i].clone();
        string(run, titles, Pat::Re(&re), true);
    }
}

/// `choose_date`.
pub(crate) fn date(run: &mut Run, titles: &mut Titles) -> PyResult<()> {
    let oldest = run.s.options.oldest;
    let mut remove = Titles::new();
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (na, nb) = (run.n(a).full_name.clone(), run.n(b).full_name.clone());
        let d1 = titletools::get_date(&na, &run.s)?;
        let d2 = titletools::get_date(&nb, &run.s)?;
        // Which title is newer: `Some(true)` for the first.
        let first_newer = if d1 != 0 && d2 == 0 {
            Some(true)
        } else if d2 != 0 && d1 == 0 {
            Some(false)
        } else if d1 > d2 {
            Some(true)
        } else if d2 > d1 {
            Some(false)
        } else {
            None
        };
        let Some(first_newer) = first_newer else {
            continue;
        };
        let (newer, older) = if first_newer { (a, b) } else { (b, a) };
        let (keep, drop) = if oldest {
            (older, newer)
        } else {
            (newer, older)
        };
        if !titles.contains(&older) {
            continue;
        }
        remove.insert(drop);
        let (kn, dn) = (run.n(keep).full_name.clone(), run.n(drop).full_name.clone());
        if !kn.is_empty() {
            run.trace_keep("REF0024", &format!("({d1}) {kn}"), &format!("({d2}) {dn}"));
        }
    }
    remove_all(titles, &remove);
    Ok(())
}

/// A value Retool's version comparison holds: an `int`, or a `str` that isn't one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum V {
    /// Negative, and the digits without leading zeros.
    Int(bool, String),
    Str(String),
}

impl V {
    /// `int(s)` if it parses, else `s`.
    fn of(s: &str) -> V {
        match big_int(s) {
            Some((negative, digits)) => V::Int(negative, digits),
            None => V::Str(s.to_owned()),
        }
    }

    fn repr(&self) -> String {
        match self {
            V::Int(true, d) => format!("-{d}"),
            V::Int(false, d) => d.clone(),
            V::Str(s) => py_repr(s),
        }
    }

    /// `<` between two values, or `None` for Python's `TypeError` between an `int` and a `str`.
    fn cmp(&self, other: &V) -> Option<Ordering> {
        match (self, other) {
            (V::Int(n1, d1), V::Int(n2, d2)) => {
                let magnitude = d1.len().cmp(&d2.len()).then_with(|| d1.cmp(d2));
                Some(match (n1, n2) {
                    (false, false) => magnitude,
                    (true, true) => magnitude.reverse(),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                })
            }
            (V::Str(a), V::Str(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

/// `int(s)` with Python's syntax (sign, surrounding whitespace, single underscores between digits), at any size.
/// Python also reads non-ASCII decimal digits, which this doesn't.
fn big_int(s: &str) -> Option<(bool, String)> {
    let t = py::strip(s);
    let (negative, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut digits = String::new();
    let mut previous_underscore = true;
    for c in body.chars() {
        if c == '_' && !previous_underscore {
            previous_underscore = true;
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        digits.push(c);
        previous_underscore = false;
    }
    if digits.is_empty() || previous_underscore {
        return None;
    }
    let trimmed = digits.trim_start_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    let negative = negative && digits != "0";
    Some((negative, digits.to_owned()))
}

/// A list's `<`: the first differing items decide, else the shorter list is less.
fn cmp_lists(a: &[V], b: &[V]) -> Option<Ordering> {
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return x.cmp(y);
        }
    }
    Some(a.len().cmp(&b.len()))
}

fn repr_list(list: &[V]) -> String {
    format!(
        "[{}]",
        list.iter().map(V::repr).collect::<Vec<_>>().join(", ")
    )
}

/// One side of a pair `process_versions` compares: a version's own segment, or the fill list `zip_longest`
/// shares between every pair it pads.
#[derive(Clone, Copy)]
enum Side {
    Own(usize, usize),
    Fill,
}

/// `process_versions`, then the comparison loop: whether the first version is older (`Less`), newer
/// (`Greater`), or neither decides.
fn compare_versions(v1: &str, v2: &str) -> PyResult<Option<Ordering>> {
    if !v1.contains('.') && !v2.contains('.') {
        let (a, b) = match (big_int(v1), big_int(v2)) {
            (Some(a), Some(b)) => (V::Int(a.0, a.1), V::Int(b.0, b.1)),
            _ => (V::Str(v1.to_owned()), V::Str(v2.to_owned())),
        };
        let order = a.cmp(&b).expect("same types");
        return Ok((order != Ordering::Equal).then_some(order));
    }
    let token = Re::of(r"(\d+|[A-za-z]+)");
    let parse = |v: &str| -> Vec<Vec<String>> {
        if v.contains('.') {
            v.split('.').map(|segment| token.findall(segment)).collect()
        } else {
            vec![vec![v.to_owned()]]
        }
    };
    let mut parsed = [parse(v1), parse(v2)];
    let min_len = parsed[0].len().min(parsed[1].len());
    let first = |p: &Vec<Vec<String>>, i: usize| -> String {
        p[i].first().cloned().unwrap_or_else(|| "0".to_owned())
    };
    for i in 0..min_len {
        let (f1, f2) = (first(&parsed[0], i), first(&parsed[1], i));
        let (l1, l2) = (f1.chars().count(), f2.chars().count());
        let (longer, shorter_side, shorter_first, diff) = if l1 > l2 {
            (&f1, 1, &f2, l1 - l2)
        } else if l2 > l1 {
            (&f2, 0, &f1, l2 - l1)
        } else {
            continue;
        };
        if !longer.starts_with('0') {
            continue;
        }
        let padded = format!("{shorter_first}{}", "0".repeat(diff));
        match parsed[shorter_side][i].first_mut() {
            Some(slot) => *slot = padded,
            None => {
                return Err(Crash(
                    "IndexError: list assignment index out of range".into(),
                ));
            }
        }
    }
    let mut own: [Vec<Vec<V>>; 2] = [
        parsed[0]
            .iter()
            .map(|s| s.iter().map(|t| V::of(t)).collect())
            .collect(),
        parsed[1]
            .iter()
            .map(|s| s.iter().map(|t| V::of(t)).collect())
            .collect(),
    ];
    let mut fill: Vec<V> = vec![V::Int(false, "0".into())];
    let longest = own[0].len().max(own[1].len());
    let pairs: Vec<[Side; 2]> = (0..longest)
        .map(|i| {
            let side = |k: usize| {
                if i < own[k].len() {
                    Side::Own(k, i)
                } else {
                    Side::Fill
                }
            };
            [side(0), side(1)]
        })
        .collect();
    fn get<'a>(own: &'a [Vec<Vec<V>>; 2], fill: &'a [V], side: Side) -> &'a [V] {
        match side {
            Side::Own(k, i) => &own[k][i],
            Side::Fill => fill,
        }
    }
    for pair in &pairs {
        let (l0, l1) = (
            get(&own, &fill, pair[0]).len(),
            get(&own, &fill, pair[1]).len(),
        );
        if l0 == l1 {
            continue;
        }
        let (shorter, longer) = if l0 < l1 { (0, 1) } else { (1, 0) };
        let extra: Vec<V> = get(&own, &fill, pair[longer])
            .iter()
            .skip(1)
            .map(|item| match item {
                V::Str(_) => V::Str("0".into()),
                V::Int(..) => V::Int(false, "0".into()),
            })
            .collect();
        match pair[shorter] {
            Side::Own(k, i) => own[k][i].extend(extra),
            Side::Fill => fill.extend(extra),
        }
    }
    for pair in &pairs {
        let (a, b) = (get(&own, &fill, pair[0]), get(&own, &fill, pair[1]));
        let order = match cmp_lists(a, b) {
            Some(order) => order,
            None => repr_list(a).cmp(&repr_list(b)),
        };
        if order != Ordering::Equal {
            return Ok(Some(order));
        }
    }
    Ok(None)
}

/// `choose_version_revision`.
pub(crate) fn version_revision(run: &mut Run, titles: &mut Titles, key: &str) -> PyResult<()> {
    let oldest = run.s.options.oldest;
    let mut remove = Titles::new();
    let value = |v: Option<&VersionValue>| match v {
        Some(VersionValue::Str(s)) => Some(s.clone()),
        Some(VersionValue::Flag) => Some(String::new()),
        None => None,
    };
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let (x, y) = (run.n(a), run.n(b));
        let (na, nb) = (x.full_name.clone(), y.full_name.clone());
        let (v1, v2) = (
            value(x.normalized_version.get(key)),
            value(y.normalized_version.get(key)),
        );
        let (pre1, pre2) = (
            x.normalized_version.contains_key("preproduction"),
            y.normalized_version.contains_key("preproduction"),
        );
        match (v1, v2) {
            (Some(_), None) | (None, Some(_)) => {
                let first = x.normalized_version.contains_key(key);
                // The title with the version, and the one without.
                let (with, without, with_pre, reference) = if first {
                    (a, b, pre1, "REF0038")
                } else {
                    (b, a, pre2, "REF0039")
                };
                let (keep, drop) = if with_pre || oldest {
                    (without, with)
                } else {
                    (with, without)
                };
                let _ = (&na, &nb);
                if titles.contains(&drop) {
                    remove.insert(drop);
                }
                let (kn, dn) = (run.n(keep).full_name.clone(), run.n(drop).full_name.clone());
                run.trace_keep(reference, &kn, &dn);
            }
            (Some(v1), Some(v2)) => {
                let Some(order) = compare_versions(&v1, &v2)? else {
                    continue;
                };
                // `Less`: the first title's version is lower.
                let (higher, lower, reference) = if order == Ordering::Less {
                    (b, a, "REF0041")
                } else {
                    (a, b, "REF0040")
                };
                let (keep, drop) = if oldest {
                    (lower, higher)
                } else {
                    (higher, lower)
                };
                remove.insert(drop);
                let (kn, dn) = (run.n(keep).full_name.clone(), run.n(drop).full_name.clone());
                run.trace_keep(reference, &kn, &dn);
            }
            (None, None) => {}
        }
    }
    remove_all(titles, &remove);
    Ok(())
}

/// The language order `choose_language` prefers.
fn language_order(s: &Settings) -> Vec<String> {
    if let Some(order) = &s.system_language_order_user {
        return order.clone();
    }
    if let Some(regions) = &s.system_region_order_user {
        let implied: Vec<String> = regions
            .iter()
            .flat_map(|r| s.languages_filter.get(r).cloned().unwrap_or_default())
            .collect();
        if !implied.is_empty() {
            return implied;
        }
    }
    s.language_order_user.clone()
}

/// The order `choose_language` falls back to when the language order doesn't decide.
fn fallback_language_order(s: &Settings) -> Vec<String> {
    match &s.system_region_order_user {
        Some(regions) => regions
            .iter()
            .flat_map(|r| s.languages_filter.get(r).cloned().unwrap_or_default())
            .collect(),
        None => s.region_order_languages_user.clone(),
    }
}

/// The languages Retool's regions imply, without repeats.
fn implied_languages(s: &Settings) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for languages in s.languages_implied.values() {
        if let Some(first) = languages.first()
            && !out.contains(first)
        {
            out.push(first.clone());
        }
    }
    out
}

/// `f"({','.join(languages) + ')':<width}"`.
fn padded_languages(languages: &[String], width: usize) -> String {
    let text = format!("{})", languages.join(","));
    let count = text.chars().count();
    format!("({text}{}", " ".repeat(width.saturating_sub(count)))
}

/// `choose_language`.
pub(crate) fn language(run: &mut Run, titles: &mut Titles, first_time: bool) {
    let order = language_order(&run.s);
    let fallback = fallback_language_order(&run.s);
    let implied = implied_languages(&run.s);
    let mut remove = Titles::new();
    let shown = |run: &Run, id: Id| {
        let n = run.n(id);
        format!(
            "{} [{}] {}",
            padded_languages(&n.languages, 30),
            n.short_name,
            n.full_name
        )
    };
    for (a, b) in pairs(titles) {
        if !equivalent(run, a, b, Some(titles)) {
            continue;
        }
        let l1 = run.n(a).languages.join(",");
        let l2 = run.n(b).languages.join(",");
        let same_region_with_languages = run.n(a).primary_region == run.n(b).primary_region
            && !run.n(a).languages.is_empty()
            && !run.n(b).languages.is_empty();
        let mut found = false;
        // Runs one pass of a language list: `Some(true)` keeps the first title.
        let mut pass =
            |run: &mut Run, list: &[String], refs: (&str, &str), stop_on_both: bool| -> bool {
                for language in list {
                    let (s1, s2) = (run.search(language, &l1), run.search(language, &l2));
                    if s1 && !s2 {
                        if titles.contains(&b) {
                            run.trace_ref(refs.0, &[list.join(", ")]);
                            let (k, d) = (shown(run, a), shown(run, b));
                            run.trace.step("", &[k, d], &[], true);
                            remove.insert(b);
                            return true;
                        }
                    } else if s2 && !s1 {
                        if titles.contains(&a) {
                            run.trace_ref(refs.1, &[list.join(", ")]);
                            let (k, d) = (shown(run, b), shown(run, a));
                            run.trace.step("", &[k, d], &[], true);
                            remove.insert(a);
                            return true;
                        }
                    } else if stop_on_both && s1 && s2 {
                        return true;
                    }
                }
                false
            };
        if same_region_with_languages {
            found = pass(run, &order, ("REF0028", "REF0029"), first_time);
        } else {
            // Retool checks the region and languages inside the loop, so nothing in the order applies.
        }
        let supersets = run.n(a).is_superset || run.n(b).is_superset;
        if !found && !supersets {
            found = pass(run, &fallback, ("REF0097", "REF0098"), false);
        }
        if !found && !supersets {
            found = pass(run, &implied, ("REF0083", "REF0084"), false);
        }
        // Retool's `break` here leaves the loop over pairs, not a loop over languages: the first pair decided by
        // language count ends the comparison.
        if !found && !supersets {
            let (c1, c2) = (run.n(a).languages.len(), run.n(b).languages.len());
            let (na, nb) = (run.n(a).full_name.clone(), run.n(b).full_name.clone());
            if c1 > c2 {
                run.trace_keep("REF0081", &na, &nb);
                if titles.contains(&b) {
                    remove.insert(b);
                    break;
                }
            } else if c2 > c1 {
                run.trace_keep("REF0082", &nb, &na);
                if titles.contains(&a) {
                    remove.insert(a);
                    break;
                }
            }
        }
    }
    remove_all(titles, &remove);
}

/// A `(short name, language priority, language)` tuple in `choose_1g1r`'s `short_name_top_languages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopLanguage(pub String, pub i64, pub String);

/// `hash(int)` for an int that fits a machine word.
pub(crate) fn int_hash(n: i64) -> i64 {
    const MODULUS: i128 = (1 << 61) - 1;
    let magnitude = (i128::from(n).abs() % MODULUS) as i64;
    let h = if n < 0 { -magnitude } else { magnitude };
    if h == -1 { -2 } else { h }
}

impl PyHash for TopLanguage {
    fn py_hash(&self) -> i64 {
        tuple_hash(&[self.0.py_hash(), int_hash(self.1), self.2.py_hash()])
    }
}

/// `choose_language_top`.
pub(crate) fn language_top(run: &mut Run, titles: &mut Titles, top: &PySet<TopLanguage>) {
    run.trace_ref("REF0015", &[]);
    run.note("Highest language priority per short name:\n");
    let mut keep = Titles::new();
    let mut dropped = Titles::new();
    let mut remove = Titles::new();
    let count = titles.len();
    for TopLanguage(short_name, priority, language) in top.iter() {
        run.note(&format!("• {short_name} | ({priority}) {language}"));
        for &title in titles.iter() {
            if run.n(title).short_name != *short_name {
                continue;
            }
            let joined = run.n(title).languages.concat();
            if !run.search(language, &joined) && count != 1 && !keep.contains(&title) {
                remove.insert(title);
                dropped.insert(title);
            } else {
                keep.insert(title);
            }
        }
    }
    run.note("\nTitles that match the highest language priority:\n");
    let by_short_name = |run: &Run, set: &Titles| {
        let mut list: Vec<Id> = set.iter().copied().collect();
        list.sort_by(|&a, &b| run.n(a).short_name.cmp(&run.n(b).short_name));
        list
    };
    for id in by_short_name(run, &keep) {
        let n = run.n(id);
        let line = format!(
            "+ Keeping: {} [{}] {}",
            padded_languages(&n.languages, 20),
            n.short_name,
            n.full_name
        );
        run.note(&line);
    }
    for id in by_short_name(run, &dropped) {
        let n = run.n(id);
        let line = format!(
            "- Removing: {}[{}] {}",
            padded_languages(&n.languages, 20),
            n.short_name,
            n.full_name
        );
        run.note(&line);
    }
    remove_all(titles, &remove);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_retool_s_do() {
        let older = |a: &str, b: &str| compare_versions(a, b).unwrap();
        assert_eq!(older("1", "2"), Some(Ordering::Less));
        assert_eq!(older("10", "9"), Some(Ordering::Greater));
        assert_eq!(older("a", "b"), Some(Ordering::Less));
        assert_eq!(older("1.1", "1.1"), None);
        assert_eq!(older("1.5", "1.05"), Some(Ordering::Greater));
        assert_eq!(older("1.1", "1.1a"), Some(Ordering::Less));
        // `['a'] < [1]` raises, and Retool falls back to comparing their reprs.
        assert_eq!(older("1.a", "1.1"), Some(Ordering::Less));
        assert!(compare_versions("1.05", "1.").is_err());
    }

    #[test]
    fn a_pattern_shows_as_retool_shows_it() {
        let re = Re::of_i(r"\(Alt.*?\)");
        assert_eq!(re.py_str(), r"re.compile('\\(Alt.*?\\)', re.IGNORECASE)");
        assert_eq!(regex_string(&re.py_str()), r"'\\(Alt.*?\\)'");
        assert_eq!(regex_string(&Re::of("x").py_str()), "'x'");
        assert_eq!(int_hash(-1), -2);
    }
}
