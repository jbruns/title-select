//! Differential tests: the built-in engine's pieces against the answers the bundled Retool gave on generated
//! inputs, frozen in `tests/oracle/` when the bundled engine was removed.

use std::path::PathBuf;

use indexmap::IndexMap;
use title_select_api::selection::SelectionProfile;

use super::settings::Settings;

/// Retool's test inputs, kept for the fixture tests.
pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/retool/fixtures")
}

/// The internal config the fixture tests run under, frozen beside the fixtures.
pub(crate) fn internal_config() -> Vec<u8> {
    std::fs::read(fixtures().join("config/internal-config.json")).unwrap()
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

/// What the bundled Retool's `TitleTools` returned for 3000 generated names under the default Profile: one JSON
/// array per name, the name then each field of [`TitleToolsCase`] in order.
const TITLETOOLS: &str = include_str!("../../tests/oracle/titletools.jsonl");

#[test]
fn titletools_match_the_bundled_engine() {
    let mut rng = Rng::new(94);
    let settings = Settings::new(&internal_config(), &SelectionProfile::default(), "").unwrap();
    let mut differences = Vec::new();
    let mut count = 0;
    for line in TITLETOOLS.lines() {
        let (name, expected): (String, TitleToolsCase) = {
            let mut row: Vec<serde_json::Value> = serde_json::from_str(line).unwrap();
            let name = serde_json::from_value(row.remove(0)).unwrap();
            let keys = [
                "date",
                "group_name",
                "versions",
                "short_name",
                "region_free_name",
                "languages",
                "languages_untagged",
                "regions",
                "remove_tags",
                "sanitized",
                "sanitized_header",
            ];
            let object: serde_json::Map<_, _> = keys
                .iter()
                .map(|k| (k.to_string(), row.remove(0)))
                .collect();
            (name, serde_json::from_value(object.into()).unwrap())
        };
        assert_eq!(name, synthetic_name(&mut rng), "the names are generated");
        count += 1;
        let actual = titletools_case(&name, &settings);
        if actual != expected {
            differences.push(format!(
                "{name}\n  python: {expected:?}\n  rust:   {actual:?}"
            ));
        }
    }
    assert_eq!(count, 3000);
    assert!(
        differences.is_empty(),
        "{} of {count} names differ:\n{}",
        differences.len(),
        differences[..differences.len().min(15)].join("\n")
    );
}
