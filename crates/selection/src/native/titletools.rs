//! Retool's `TitleTools`: how it reads names, regions, languages, versions and dates out of a title's name.

use std::sync::LazyLock;

use indexmap::IndexMap;

use super::py::{self, Crash, PyResult};
use super::pyre::Re;
use super::pyset::PySet;
use super::settings::{Settings, Tag};

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Re> = LazyLock::new(|| Re::of($pattern));
    };
    ($name:ident, $pattern:expr, i) => {
        static $name: LazyLock<Re> = LazyLock::new(|| Re::of_i($pattern));
    };
}

re!(
    MONTHS,
    "january|february|march|april|may|june|july|august|september|october|november|december",
    i
);
re!(SIX_DIGITS, r"\(\d{6}\)");
re!(UTC, r"\(\d{8}T\d{6}\)");
re!(DIGITS, r"\d+");
re!(TWO_VERSIONS, r"v[\d+\.\-]+");
re!(VERSION_AND_DATE, r"v[\d+\.]+(?:, )\d{4}-\d{2}-\d{2}");
re!(VERSION_OR_DATE, r"(v[\d+\.]+|\d{4}-\d{2}-\d{2})");
re!(VERSION_PART, r"[\d+\.\-]+");
re!(VER_DOT, r"ver\.", i);
re!(
    VERSION_WORDS,
    r"version|^(v|Rev|Version|Beta|Alpha|Prototype|Proto|Build|Alt|FW)|\s",
    i
);
re!(LEADING_DOT, r"^\.");
re!(FMTOWNS_1, r" Version ((\d\.?)+)");
re!(FMTOWNS_2, r" (v(\d\.?)+)");
re!(FMTOWNS_3, r" \((v(\d\.?)+)beta\)");
re!(COLON, r"(\S):\s");

const MONTH_NAMES: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// `TitleTools.get_date`: the date in a name as `YYYYMMDDhhmmss`, or 0.
pub(crate) fn get_date(name: &str, s: &Settings) -> PyResult<i64> {
    let dates = &s.regex.dates;
    let mut name = name.to_owned();
    if dates[5].search(&name) {
        name = dates[5].sub(r"(\1-01)", &name);
    }
    if dates[6].search(&name) {
        name = dates[6].sub(r"(\1-01-01)", &name);
    }
    if dates[8].search(&name) {
        name = dates[8].sub(r"(1970-\1)", &name);
    }
    let mut time = "000000".to_owned();
    if MONTHS.search(&name) {
        for (i, month) in MONTH_NAMES.iter().enumerate() {
            let pattern = Re::of_i(&format!("{month}, "));
            name = pattern.sub(&format!("{:02}-01-", i + 1), &name);
        }
    }
    let us_date = dates[2].search(&name);
    let short_date = dates[3].search(&name);
    let utc_date = dates[4].search(&name);
    for regex in dates {
        if !regex.search(&name) {
            continue;
        }
        let found = regex.find(&name);
        name = name.replace(&found, &found.replace('-', ""));
        let mut found = dates[0].find(&name);
        let (year, month, day);
        let sl = |t: &str, a: Option<isize>, b: Option<isize>| py::slice(t, a, b);
        if short_date {
            found = SIX_DIGITS.find(&name);
            let yy = py::int(&sl(&found, Some(1), Some(-5)))?;
            year = (if yy < 70 { 2000 + yy } else { 1900 + yy }).to_string();
            month = sl(&found, Some(3), Some(-3));
            day = sl(&found, Some(5), Some(-1));
        } else if us_date {
            year = sl(&found, Some(5), Some(-1));
            month = sl(&found, Some(1), Some(-7));
            day = sl(&found, Some(3), Some(-5));
        } else if utc_date {
            found = UTC.find(&name);
            year = sl(&found, Some(1), Some(5));
            month = sl(&found, Some(5), Some(7));
            day = sl(&found, Some(7), Some(9));
            time = sl(&found, Some(10), Some(16));
        } else {
            year = sl(&found, Some(1), Some(-5));
            month = sl(&found, Some(5), Some(-3));
            day = sl(&found, Some(7), Some(-1));
        }
        // Python's `and` stops at the first false comparison, before the next `int()` can raise.
        let valid = py::int(&year)? >= 1970
            && py::int(&month)? >= 1
            && py::int(&month)? <= 12
            && py::int(&day)? >= 1
            && py::int(&day)? <= 31;
        if valid {
            return py::int(&format!("{year}{month}{day}{time}"));
        }
    }
    Ok(0)
}

/// Whether one of the version-ignore patterns finds a problem title in `name`.
fn problem_title(name: &str, s: &Settings) -> bool {
    s.version_ignore.iter().any(|p| !p.find(name).is_empty())
}

/// `TitleTools.get_group_name`.
pub(crate) fn get_group_name(name: &str, s: &Settings) -> String {
    let mut name = py::rstrip(name).to_owned();
    let paren = py::find(&name, "(");
    if paren != -1 {
        name = py::rstrip(&py::slice(&name, None, Some(paren - 1))).to_owned();
    }
    let r = &s.regex;
    let mut patterns: Vec<&Re> = vec![&r.dreamcast_version, &r.fmtowns_version, &r.long_version];
    patterns.extend(r.mpal());
    patterns.extend(r.ntsc());
    patterns.extend(r.pal_60hz());
    patterns.extend(r.pal());
    patterns.extend(r.secam());
    let mut problem = false;
    for pattern in patterns {
        if std::ptr::eq(pattern, &r.long_version) && problem_title(&name, s) {
            problem = true;
        }
        if problem {
            continue;
        }
        name = py::strip(&pattern.sub("", &name)).to_owned();
    }
    py::strip(&name.to_lowercase().replace("  ", " ")).to_owned()
}

/// A value in a normalized version: a version string, or a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VersionValue {
    Str(String),
    Flag,
}

pub(crate) type Versions = IndexMap<&'static str, VersionValue>;

/// Retool's version patterns, by name, in its `versions` order.
fn version_patterns(s: &Settings) -> [(&'static str, &Re); 28] {
    let r = &s.regex;
    [
        ("nintendo_mastering_code", &r.nintendo_mastering_code),
        ("nintendo_3ds_product_code", &r.nintendo_3ds_product_code),
        ("fmtowns_version", &r.fmtowns_version),
        ("dreamcast_version", &r.dreamcast_version),
        ("version", &r.version),
        ("long_version", &r.long_version),
        (
            "famicom_disk_system_version",
            &r.famicom_disk_system_version,
        ),
        ("hyperscan_version", &r.hyperscan_version),
        ("revision", &r.revision),
        ("sega_panasonic_ring_code", &r.sega_panasonic_ring_code),
        ("sega_ringedge_serial", &r.sega_ringedge_serial),
        ("nec_mastering_code", &r.nec_mastering_code),
        ("ps_firmware", &r.ps_firmware),
        ("ps1_2_id", &r.ps1_2_id),
        ("ps3_id", &r.ps3_id),
        ("ps3_digital_id", &r.ps3_digital_id),
        ("ps4_id", &r.ps4_id),
        ("ps5_id", &r.ps5_id),
        ("psp_id", &r.psp_id),
        ("ps_vita_id", &r.ps_vita_id),
        ("benesse_id", &r.benesse_id),
        ("beta", &r.beta),
        ("alpha", &r.alpha),
        ("proto", &r.proto),
        ("preprod", &r.preprod),
        ("dev", &r.dev),
        ("build", &r.build),
        ("alt", &r.alt),
    ]
}

/// `TitleTools.get_normalized_version`.
pub(crate) fn get_normalized_version(full_name: &str, s: &Settings) -> PyResult<Versions> {
    let mut title = full_name.to_owned();
    let mut versions = Versions::new();
    let problem = problem_title(&title, s);
    let mut fm_towns_pattern_used = false;
    for (kind, pattern) in version_patterns(s) {
        // `versions['dreamcast'] and pattern == fmtowns or pattern == long_version`, as Python groups it.
        if versions.contains_key("dreamcast")
            && (kind == "fmtowns_version" || kind == "long_version")
        {
            continue;
        }
        if versions.contains_key("fmtowns") && (kind == "version" || kind == "long_version") {
            continue;
        }
        if kind == "long_version" && problem {
            continue;
        }
        if fm_towns_pattern_used && !problem {
            title = FMTOWNS_1.sub(r" (v\1)", &title);
            title = FMTOWNS_2.sub(r" (\1)", &title);
            title = FMTOWNS_3.sub(r" (\1) (Beta)", &title);
        }
        if kind == "fmtowns_version" {
            fm_towns_pattern_used = true;
        }
        let found = pattern.find(&title);
        if found.is_empty() {
            continue;
        }
        let mut version = found.replace(['(', ')'], "");
        match kind {
            "famicom_disk_system_version" => {
                version =
                    DIGITS.findall(&version).into_iter().max().ok_or_else(|| {
                        Crash("ValueError: max() arg is an empty sequence".into())
                    })?;
            }
            "nec_mastering_code" => {
                version = version.split(", ").max().unwrap_or_default().to_owned();
            }
            "sega_panasonic_ring_code" => {
                version = if DIGITS.find(&version).is_empty() {
                    "0".into()
                } else {
                    let mut max = None;
                    for d in DIGITS.findall(&version) {
                        let n = py::int(&d)?;
                        max = Some(max.map_or(n, |m: i64| m.max(n)));
                    }
                    max.unwrap_or(0).to_string()
                };
            }
            "benesse_id" => {
                version = if s.regex.benesse_id_singular.find(&version).is_empty() {
                    "0".into()
                } else {
                    s.regex
                        .benesse_id_singular
                        .findall(&version)
                        .pop()
                        .unwrap_or_default()
                };
            }
            "beta" if version == "Beta" => version = "0".into(),
            "alpha" if version == "Alpha" => version = "0".into(),
            "proto" if version == "Proto" || version == "Prototype" => version = "0".into(),
            "alt" if version == "Alt" => version = "0".into(),
            _ => {}
        }
        version = version
            .replace("PS3 ", "")
            .replace("-to-", ", ")
            .replace(" - AGI", ",")
            .replace("rev", "");
        let mut num_versions = TWO_VERSIONS.findall(&version).len();
        if VERSION_AND_DATE.search(&version) {
            num_versions = VERSION_OR_DATE.findall(&version).len();
        }
        if num_versions == 2 {
            let parts = VERSION_PART.findall(&version);
            let index_error = || Crash("IndexError: list index out of range".into());
            let version_a = parts.first().cloned().ok_or_else(index_error)?;
            let version_b = parts.get(1).ok_or_else(index_error)?.replace('-', ".");
            // `'.'.join(...)` of each dotted part's first run raises, and is skipped, when a part is empty.
            if !version_a.split('.').any(str::is_empty) {
                version = version_a;
            }
            version = format!("{version}.{version_b}");
        }
        version = py::strip(&version).to_owned();
        if kind == "long_version" {
            version = VER_DOT.sub("", &version);
        }
        if kind != "nintendo_mastering_code" && kind != "ps_vita_id" {
            version = VERSION_WORDS.sub("", &version);
        }
        if version.contains("666") && title.contains("Doom") {
            version = version.replace("666", "6.6.6");
        }
        if version.contains('.') && LEADING_DOT.search(&version) {
            version = LEADING_DOT.sub("0.", &version);
        }
        let value = VersionValue::Str(version);
        match kind {
            "revision" => {
                versions.insert("revision", value);
            }
            "sega_panasonic_ring_code" => {
                versions.insert("sega", value);
            }
            "nec_mastering_code" => {
                versions.insert("nec", value);
            }
            "beta" | "alpha" | "proto" => {
                versions.insert(kind, value);
                versions.insert("preproduction", VersionValue::Flag);
            }
            "build" => {
                versions.insert("build", value);
            }
            "preprod" | "dev" => {
                versions.insert("preproduction", VersionValue::Flag);
            }
            "alt" => {
                versions.insert("alt", value);
            }
            "ps_firmware" => {
                versions.insert("firmware", value);
            }
            "ps1_2_id" | "ps3_id" | "ps3_digital_id" | "ps4_id" | "ps5_id" | "psp_id"
            | "ps_vita_id" => {
                versions.insert("playstation", value);
            }
            "benesse_id" => {
                versions.insert("benesse", value);
            }
            _ => {
                versions.insert("version", value);
            }
        }
        if kind == "dreamcast_version" {
            versions.insert("dreamcast", VersionValue::Flag);
        } else if kind == "fmtowns_version" {
            versions.insert("fmtowns", VersionValue::Flag);
        }
    }
    if versions.contains_key("dreamcast") {
        versions.shift_remove("dreamcast");
    } else if versions.contains_key("fmtowns") {
        versions.shift_remove("fmtowns");
    }
    Ok(versions)
}

/// `TitleTools.get_region_free_name`.
pub(crate) fn get_region_free_name(name: &str, tags: &PySet<String>, s: &Settings) -> String {
    let mut out = name.to_owned();
    let removed = regions(&out, s, Method::Remove);
    if !removed.is_empty() {
        out = removed;
    }
    let removed = languages(&out, tags, s, Method::Remove);
    if !removed.is_empty() {
        out = removed;
    }
    if out.is_empty() {
        out = name.to_owned();
    }
    out
}

/// `TitleTools.get_short_name`.
pub(crate) fn get_short_name(name: &str, tags: &PySet<String>, s: &Settings) -> String {
    let name = normalize_discs(name, s);
    let name = remove_tags(&name, s);
    py::strip(&get_region_free_name(&name, tags, s)).to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Remove,
}

/// `TitleTools.languages`.
pub(crate) fn languages(name: &str, tags: &PySet<String>, s: &Settings, method: Method) -> String {
    let mut found: Vec<String> = Vec::new();
    if tags.is_empty() {
        found.push(s.regex.languages.find(name));
    } else {
        for tag in tags.iter() {
            let f = s.regex.languages.find(tag);
            if !f.is_empty() {
                found.push(f);
            }
        }
    }
    if found.is_empty() {
        return String::new();
    }
    match method {
        Method::Remove => {
            let mut result = name.to_owned();
            for f in &found {
                result = py::strip(&result.replace(f.as_str(), "").replace("  ", " ")).to_owned();
            }
            result
        }
        Method::Get => py::slice(py::strip(&found[0]), Some(1), Some(-1)),
    }
}

/// `TitleTools.normalize_discs`.
pub(crate) fn normalize_discs(name: &str, s: &Settings) -> String {
    let mut name = name.to_owned();
    for (key, value) in &s.tags_disc_rename {
        if name.contains(key.as_str()) {
            name = name.replace(key.as_str(), value);
        }
    }
    name
}

/// `TitleTools.regions`.
pub(crate) fn regions(name: &str, s: &Settings, method: Method) -> String {
    let found = s.regex.region_order_default.find(name);
    if found.is_empty() {
        return String::new();
    }
    let found = format!(" {found}");
    match method {
        Method::Remove => py::strip(&name.replace(&found, "")).to_owned(),
        Method::Get => py::slice(py::strip(&found), Some(1), Some(-1)).replace("Export", "World"),
    }
}

/// `TitleTools.remove_tags`.
pub(crate) fn remove_tags(name: &str, s: &Settings) -> String {
    let mut name = name.to_owned();
    for tag in &s.tags_ignore {
        match tag {
            Tag::Regex(re) => name = py::strip(&re.sub("", &name)).to_owned(),
            Tag::String(text) => {
                if name.contains(text.as_str()) {
                    name =
                        py::strip(&name.replace(text.as_str(), "").replace("  ", " ")).to_owned();
                }
            }
        }
    }
    let r = &s.regex;
    let mut patterns: Vec<&Re> = r.dates.iter().collect();
    patterns.extend(r.mpal());
    patterns.extend(r.ntsc());
    patterns.extend(r.pal_60hz());
    patterns.extend(r.pal());
    patterns.extend(r.preproduction());
    patterns.extend(r.secam());
    patterns.extend(r.unl_group());
    patterns.extend(version_patterns(s).map(|(_, p)| p));
    patterns.extend([&r.multimedia, &r.oem, &r.rerelease, &r.review]);
    let mut problem = false;
    for pattern in patterns {
        if std::ptr::eq(pattern, &r.long_version) && problem_title(&name, s) {
            problem = true;
        }
        if problem {
            continue;
        }
        name = py::strip(&pattern.sub("", &name)).replace("  ", " ");
    }
    name
}

/// `TitleTools.replace_invalid_characters`.
pub(crate) fn replace_invalid_characters(name: &str, is_header_detail: bool) -> String {
    let mut name = name.to_owned();
    for c in [':', '\\', '/', '<', '>', '"', '|', '?', '*'] {
        if !name.contains(c) {
            continue;
        }
        match c {
            ':' => {
                if COLON.search(&name) {
                    name = COLON.sub(r"\1 - ", &name);
                } else {
                    name = name.replace(c, "-");
                }
            }
            '"' => name = name.replace(c, "'"),
            '\\' | '/' => {
                if is_header_detail {
                    name = name.replace(c, "-");
                }
            }
            _ => name = name.replace(c, "-"),
        }
    }
    if !is_header_detail && name.starts_with('.') {
        name = format!("．{}", &name[1..]);
    }
    name
}
