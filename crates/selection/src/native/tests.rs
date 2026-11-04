//! Differential tests: the built-in engine's pieces against the bundled Retool's, on Retool's fixture names and
//! generated ones.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use indexmap::IndexMap;
use title_select_api::selection::SelectionProfile;

use super::settings::Settings;

pub(crate) fn engine() -> crate::Engine {
    crate::Engine::locate(None).unwrap_or_else(|e| {
        panic!("{e}\nthe Selection tests need the Retool install: run tools/retool/setup")
    })
}

pub(crate) fn upstream() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/retool/upstream")
}

pub(crate) fn internal_config() -> Vec<u8> {
    std::fs::read(engine().root.join("retool/config/internal-config.json")).unwrap()
}

/// Runs `oracle.py COMMAND` with `input` on stdin, under `profile`'s global settings.
pub(crate) fn oracle(command: &str, input: &str, profile: &SelectionProfile) -> String {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let vocabulary = engine.vocabulary().unwrap();
    let user_config = dir.path().join("user-config.yaml");
    std::fs::write(
        &user_config,
        crate::config::user_config(profile, &vocabulary),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("systems")).unwrap();
    let script = dir.path().join("oracle.py");
    std::fs::write(&script, include_str!("oracle.py")).unwrap();
    let mut child = Command::new(engine.python())
        .arg(&script)
        .arg(engine.root.join("retool"))
        .arg(&user_config)
        .arg(command)
        .current_dir(dir.path())
        .env("PYTHONHASHSEED", "0")
        .env("PYTHONIOENCODING", "utf-8")
        .env("HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Every game name in Retool's fixture DATs.
pub(crate) fn fixture_names() -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![upstream().join("tests")];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "dat") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for line in text.lines() {
                    let line = line.trim_start();
                    if (line.starts_with("<game ") || line.starts_with("<machine "))
                        && let Some(start) = line.find("name=\"")
                    {
                        let rest = &line[start + 6..];
                        let name = &rest[..rest.find('"').unwrap_or(rest.len())];
                        names.push(
                            name.replace("&amp;", "&")
                                .replace("&quot;", "\"")
                                .replace("&apos;", "'")
                                .replace("&lt;", "<")
                                .replace("&gt;", ">"),
                        );
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// A small deterministic generator, so a failing case can be reproduced from its seed.
pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

pub(crate) const BASES: &[&str] = &[
    "Super Game",
    "Super Game II",
    "The Legend of Things",
    "Legend of Things, The",
    "Adventure - The Quest",
    "Adventure: The Quest",
    "Racing Version 2",
    "Puzzle V1.2",
    "Puzzle Ver. 1.2",
    "Doom",
    "Doom v666",
    "Sports 2000 - Version Vol. 1",
    "Clash King V201",
    "Game Boy Advance Video - Cartoon",
    "Trial Version Fun",
    "Kiosk Demo Disc",
    "Mega Pack",
    "Mega Pack + Bonus",
    "Shooter X - Movie Trailer",
    "Fighter EPK",
    ".hack",
    "Ab*c?",
    "Quote \"Me\"",
    "Path/To\\Game",
    "Ünïcödé Spiel",
    "Spiel Ⅱ",
    "Sample Program",
    "Dreamcast V01 L02",
    "Towns Game V1.1 L10",
    "Towns Game Version 2.0 L1",
    "Taikenban Game",
];

pub(crate) const TAGS: &[&str] = &[
    "(USA)",
    "(Europe)",
    "(Japan)",
    "(USA, Europe)",
    "(Japan, Korea)",
    "(World)",
    "(Export)",
    "(UK)",
    "(United Kingdom)",
    "(Unknown)",
    "(Asia)",
    "(Germany, Austria)",
    "(Hong Kong)",
    "(Taiwan)",
    "(China)",
    "(Brazil)",
    "(En)",
    "(En,Fr,De)",
    "(En,Fr-CA)",
    "(Fr-CA)",
    "(En+Ja)",
    "(En+En+En)",
    "(Zh)",
    "(Zh-Hant)",
    "(Es-XL)",
    "(Pt-BR)",
    "(En-US,En-GB)",
    "(Ja, En)",
    "(v1.0)",
    "(v1.1)",
    "(v1.0, v2.0)",
    "(v1.0.2)",
    "(v.95)",
    "(Rev 1)",
    "(Rev A)",
    "(Rev)",
    "(Beta)",
    "(Beta 2)",
    "(Alpha)",
    "(Proto)",
    "(Proto 3)",
    "(Prototype)",
    "(Pre-production)",
    "(DEBUG)",
    "(Alt)",
    "(Alt 2)",
    "(Build 1234)",
    "(1999-12-31)",
    "(12-31-1999)",
    "(99-12-31)",
    "(19991231)",
    "(2000-xx-xx)",
    "(~2001-xx-xx)",
    "(2000-12-xx)",
    "(January, 2000)",
    "(March,2001)",
    "(1-2)",
    "(13-40)",
    "(2000-12-31T235959)",
    "(Disc 1)",
    "(Disc 2)",
    "(Disk A)",
    "(Disc II)",
    "(Disc One)",
    "(CD 2)",
    "(Side A)",
    "(Disc-1)",
    "[BIOS]",
    "(Enhancement Chip)",
    "(Demo)",
    "(Kiosk)",
    "(Sample)",
    "(Trial)",
    "(Unl)",
    "(Pirate)",
    "(Aftermarket)",
    "(SGB Enhanced)",
    "(Made in Japan)",
    "(PAL)",
    "(NTSC)",
    "(PAL 60Hz)",
    "(MPAL)",
    "(SECAM)",
    "[b]",
    "(FW1.50)",
    "(SLUS-01234)",
    "(BLES-01234)",
    "(NPUB-12345)",
    "(PCSE-00001)",
    "(DV 1)",
    "(DV 12)",
    "(1A, 2B)",
    "(1M)",
    "(RE-1)",
    "(DVR-1234)",
    "(FAAA, SAAB)",
    "(USE1)",
    "(ABJE)",
    "(Switch Online)",
    "(Covermount)",
    "(Rerelease)",
    "(OEM)",
    "(Review Code)",
    "(Magazine)",
    "(Promo)",
    "(Not for Resale)",
    "(EDC)",
    "(Sega CD 32X)",
    "(FM Towns Marty)",
    "(Addon)",
    "(DLC)",
    "(Soundtrack)",
    "(Manual)",
    "(Program)",
    "(Video)",
    "(1FBC123)",
    "(v1.0beta)",
    "(Dupe 1)",
    "[MIA]",
    "(Virtual Console)",
    "(Classic Mini)",
    "(Collector's Edition)",
];

/// A generated title name.
pub(crate) fn synthetic_name(rng: &mut Rng) -> String {
    let mut name = (*rng.pick(BASES)).to_owned();
    let tags = rng.below(5);
    for _ in 0..tags {
        name.push(' ');
        name.push_str(rng.pick(TAGS));
    }
    name
}

#[derive(serde::Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum Versions {
    Map(IndexMap<String, serde_json::Value>),
    Crash(String),
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct TitleToolsCase {
    date: serde_json::Value,
    group_name: String,
    versions: Versions,
    short_name: String,
    region_free_name: String,
    languages: String,
    languages_untagged: String,
    regions: String,
    remove_tags: String,
    sanitized: String,
    sanitized_header: String,
}

fn titletools_case(name: &str, s: &Settings) -> TitleToolsCase {
    use super::pyset::PySet;
    use super::titletools::{self as t, Method, VersionValue};
    let tags: PySet<String> = name.split(" (").skip(1).map(|x| format!("({x}")).collect();
    let crash = |c: super::py::Crash| {
        format!(
            "crash: {}",
            c.0.split(':').next().unwrap_or_default().to_owned()
        )
    };
    TitleToolsCase {
        date: match t::get_date(name, s) {
            Ok(d) => serde_json::json!(d),
            Err(c) => serde_json::json!(crash(c)),
        },
        group_name: t::get_group_name(name, s),
        versions: match t::get_normalized_version(name, s) {
            Ok(v) => Versions::Map(
                v.into_iter()
                    .map(|(k, v)| {
                        (
                            k.to_owned(),
                            match v {
                                VersionValue::Str(s) => serde_json::json!(s),
                                VersionValue::Flag => serde_json::json!(true),
                            },
                        )
                    })
                    .collect(),
            ),
            Err(c) => Versions::Crash(crash(c)),
        },
        short_name: t::get_short_name(name, &tags, s),
        region_free_name: t::get_region_free_name(name, &tags, s),
        languages: t::languages(name, &tags, s, Method::Get),
        languages_untagged: t::languages(name, &PySet::new(), s, Method::Get),
        regions: t::regions(name, s, Method::Get),
        remove_tags: t::remove_tags(name, s),
        sanitized: t::replace_invalid_characters(name, false),
        sanitized_header: t::replace_invalid_characters(name, true),
    }
}

#[test]
fn titletools_match_the_bundled_engine() {
    let mut names = fixture_names();
    let mut rng = Rng::new(94);
    for _ in 0..3000 {
        names.push(synthetic_name(&mut rng));
    }
    let profile = SelectionProfile::default();
    let expected: Vec<TitleToolsCase> = serde_json::from_str(&oracle(
        "titletools",
        &serde_json::to_string(&names).unwrap(),
        &profile,
    ))
    .unwrap();
    let settings = Settings::new(&internal_config(), &profile, "").unwrap();
    let mut differences = Vec::new();
    for (name, expected) in names.iter().zip(expected) {
        let actual = titletools_case(name, &settings);
        if actual != expected {
            differences.push(format!(
                "{name}\n  python: {expected:?}\n  rust:   {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} of {} names differ:\n{}",
        differences.len(),
        names.len(),
        differences[..differences.len().min(15)].join("\n")
    );
}
