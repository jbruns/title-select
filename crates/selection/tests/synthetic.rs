//! Differential tests on synthetic DATs: both engines run on DATs generated from a seed, with profiles generated
//! from the same seed, and must conform (`Engine::conformance`). There is no written specification of Retool's
//! selection, so many small, varied DATs stand in for one.
//!
//! Two kinds of DAT are generated: invented titles that mix regions, languages and the tags selection weighs
//! (revisions, versions, prereleases, unlicensed and modern releases, discs, compilations, duplicates), and
//! samples of Retool's fixture DATs, whose titles its fixture clone lists, metadata and MIAs name.
//!
//! `SYNTHETIC_DATS` sets how many DATs to generate (default 48) and `SYNTHETIC_SEED` the first seed (default 1).

mod support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use title_select_api::selection::{Category, Compilations, SelectionProfile};

/// The most other set orders to try: rarely, a difference shows under only one order in eight or fewer.
const SET_ORDERS: u32 = 128;

/// xorshift64*: a small, seedable generator, so a failing seed can be rerun alone.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn hex(&mut self, digits: usize) -> String {
        let mut out = String::new();
        while out.len() < digits {
            write!(out, "{:016x}", self.next()).unwrap();
        }
        out.truncate(digits);
        out
    }
}

const BASES: &[&str] = &[
    "Alpha Quest",
    "Beta Racer",
    "Gamma Force",
    "Delta Kart",
    "Epsilon",
    "Zeta Saga",
    "Eta Golf",
    "Legend of Theta, The",
    "Iota Ball - Championship Edition",
    "Kappa Puzzle",
    "Lambda Wars II",
    "Mu Fighters '98",
    "Nu Soccer 2000",
    "Xi - The Lost Cave",
    "Omicron Pinball",
];

const REGIONS: &[&str] = &[
    "USA",
    "Europe",
    "Japan",
    "World",
    "USA, Europe",
    "Japan, USA",
    "Japan, Europe",
    "Germany",
    "France",
    "Spain",
    "Italy",
    "Korea",
    "Brazil",
    "Australia",
    "Asia",
    "Canada",
    "Netherlands",
    "Sweden",
    "China",
    "Taiwan",
    "Russia",
    "UK",
    "Unknown",
];

const LANGUAGES: &[&str] = &[
    "En", "Fr", "De", "Es", "It", "Ja", "Nl", "Sv", "Pt", "Zh", "Ko", "Ru", "En-GB", "Pt-BR",
];

const TAGS: &[&str] = &[
    "(Rev 1)",
    "(Rev 2)",
    "(Rev A)",
    "(v1.1)",
    "(v1.02)",
    "(v2.0)",
    "(Beta)",
    "(Beta 2)",
    "(Proto)",
    "(Demo)",
    "(Sample)",
    "(Kiosk)",
    "(Alt)",
    "(Alt 2)",
    "(Unl)",
    "(Virtual Console)",
    "(Switch Online)",
    "(Aftermarket)",
    "(Pirate)",
    "(Program)",
    "(Made in Japan)",
    "(Collector's Edition)",
    "(Promo)",
    "(Rumble Version)",
    "(SGB Enhanced)",
    "(GB Compatible)",
    "(2001-03-14)",
    "(Debug)",
    "[b]",
    "(Bonus Disc)",
    "(Limited Edition)",
    "(NTSC)",
    "(PAL)",
    "(Steam)",
    "(Evercade)",
    "(Classic Mini)",
];

/// An invented release name, sometimes a compilation of two bases, sometimes a disc of a set.
fn invented(rng: &mut Rng) -> String {
    let mut name = if rng.chance(8) {
        format!("{} + {}", rng.pick(BASES), rng.pick(BASES))
    } else {
        rng.pick(BASES).to_string()
    };
    if rng.chance(5) {
        name = format!("[BIOS] {name}");
    }
    write!(name, " ({})", rng.pick(REGIONS)).unwrap();
    if rng.chance(30) {
        let count = 1 + rng.below(4);
        let mut languages: Vec<&str> = Vec::new();
        for _ in 0..count {
            let language = *rng.pick(LANGUAGES);
            if !languages.contains(&language) {
                languages.push(language);
            }
        }
        write!(name, " ({})", languages.join(",")).unwrap();
    }
    for _ in 0..rng.below(3) {
        write!(name, " {}", rng.pick(TAGS)).unwrap();
    }
    if rng.chance(10) {
        write!(name, " (Disc {})", 1 + rng.below(3)).unwrap();
    }
    name
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn game(rng: &mut Rng, name: &str, roms: Option<&str>) -> String {
    let rom = match roms {
        Some(roms) => roms.to_owned(),
        None => format!(
            "\t\t<rom name=\"{}.bin\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\"/>\n",
            escape(name),
            1024 * (1 + rng.below(64)),
            rng.hex(8),
            rng.hex(32),
            rng.hex(40)
        ),
    };
    format!(
        "\t<game name=\"{0}\">\n\t\t<description>{0}</description>\n{rom}\t</game>\n",
        escape(name)
    )
}

/// The `<game>` elements of a fixture DAT, and its header.
fn fixture_games(dat: &Path) -> (String, Vec<String>) {
    let text = std::fs::read_to_string(dat).unwrap();
    let start = text.find("<game ").unwrap_or(text.len());
    let header = text[..start].to_owned();
    let mut games = Vec::new();
    let mut rest = &text[start..];
    while let Some(at) = rest.find("<game ") {
        let end = rest[at..].find("</game>").map(|e| at + e + "</game>".len());
        let Some(end) = end else { break };
        games.push(format!("\t{}\n", &rest[at..end]));
        rest = &rest[end..];
    }
    (header, games)
}

fn fixture_dats() -> Vec<PathBuf> {
    let dir = support::upstream().join("tests/source/systems");
    let mut dats: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "dat"))
        .filter(|p| !fixture_games(p).1.is_empty())
        .collect();
    dats.sort();
    dats
}

/// A DAT generated from `seed`: invented titles under a header with no clone list, or a sample of a fixture DAT
/// with invented titles mixed in.
fn synthetic_dat(seed: u64, fixtures: &[PathBuf]) -> (String, String) {
    let mut rng = Rng::new(seed);
    let mut games = Vec::new();
    let (label, header) = if rng.chance(50) {
        let fixture = rng.pick(fixtures);
        let (header, all) = fixture_games(fixture);
        let take = (all.len() * (5 + rng.below(25)) / 100).clamp(1, 250);
        let mut indexes: Vec<usize> = (0..all.len()).collect();
        for i in 0..take {
            let j = i + rng.below(indexes.len() - i);
            indexes.swap(i, j);
        }
        let mut chosen = indexes[..take].to_vec();
        chosen.sort_unstable();
        games.extend(chosen.into_iter().map(|i| all[i].clone()));
        let name = fixture.file_name().unwrap().to_string_lossy().into_owned();
        (format!("sample of {name}"), header)
    } else {
        (
            "invented".to_owned(),
            "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>Synthetic - System</name>\n\t\t\
             <description>Synthetic - System</description>\n\t\t<version>20240101-000000</version>\n\t\t\
             <homepage>No-Intro</homepage>\n\t\t<url>https://www.no-intro.org</url>\n\t</header>\n"
                .to_owned(),
        )
    };
    let mut names: Vec<String> = Vec::new();
    for _ in 0..(10 + rng.below(70)) {
        let name = invented(&mut rng);
        // Now and then the same name twice, with the same or other ROMs, as DATs sometimes have.
        if let Some(previous) = names.last().filter(|_| rng.chance(2)).cloned() {
            games.push(game(&mut rng, &previous, None));
        }
        games.push(game(&mut rng, &name, None));
        names.push(name);
    }
    let mut text = header;
    for g in games {
        text.push_str(&g);
    }
    text.push_str("</datafile>\n");
    (format!("seed {seed} ({label})"), text)
}

/// A profile generated from `seed`: one of Retool's test configs' orders, with random options and exclusions.
fn synthetic_profile(seed: u64) -> SelectionProfile {
    let mut rng = Rng::new(seed ^ 0x5eed);
    let config = rng.pick(&[
        "user-config-regions-1.yaml",
        "user-config-regions-4.yaml",
        "user-config-languages-1.yaml",
        "user-config-languages-2.yaml",
        "user-config-clone-list-filters-1.yaml",
        "user-config-post-filters.yaml",
    ]);
    let mut profile = support::profile_from_upstream_config(config);
    let options = &mut profile.options;
    options.prefer_retroachievements = rng.chance(20);
    options.prefer_oldest = rng.chance(20);
    options.prefer_regions = rng.chance(30);
    options.prefer_licensed = rng.chance(30);
    options.prefer_modern_rips = rng.chance(30);
    options.filter_languages = rng.chance(30);
    options.disable_1g1r = rng.chance(15);
    options.compilations = *rng.pick(&[
        Compilations::Default,
        Compilations::Individual,
        Compilations::Keep,
        Compilations::Optimize,
    ]);
    profile.exclude = Category::ALL
        .into_iter()
        .filter(|_| rng.chance(15))
        .collect();
    profile
}

#[test]
fn the_engines_conform_on_synthetic_dats() {
    let count: u64 = std::env::var("SYNTHETIC_DATS")
        .map(|n| n.parse().unwrap())
        .unwrap_or(48);
    let first: u64 = std::env::var("SYNTHETIC_SEED")
        .map(|n| n.parse().unwrap())
        .unwrap_or(1);
    let fixtures = fixture_dats();
    let engine = support::fixture_engine();
    let seeds: Vec<u64> = (first..first + count).collect();
    let results: Vec<(String, title_select_selection::Conformance)> = std::thread::scope(|scope| {
        let parallel = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunks: Vec<&[u64]> = seeds
            .chunks(seeds.len().div_ceil(parallel).max(1))
            .collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let (engine, fixtures) = (&engine, &fixtures);
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|&seed| {
                            let work = tempfile::tempdir().unwrap();
                            let (name, text) = synthetic_dat(seed, fixtures);
                            let dat = work.path().join("synthetic.dat");
                            std::fs::write(&dat, text).unwrap();
                            let profile = synthetic_profile(seed);
                            let conformance = engine
                                .conformance(
                                    &dat,
                                    &profile,
                                    &support::options(work.path()),
                                    SET_ORDERS,
                                )
                                .unwrap_or_else(|e| panic!("{name}: {e}"));
                            (name, conformance)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });

    let groups: usize = results
        .iter()
        .filter_map(|(_, c)| c.comparison.as_ref())
        .map(|c| c.groups)
        .sum();
    let units: usize = results.iter().map(|(_, c)| c.units).sum();
    let differing: usize = results.iter().map(|(_, c)| c.differing.len()).sum();
    let order_sensitive: usize = results.iter().map(|(_, c)| c.order_sensitive.len()).sum();
    let failed_alike = results
        .iter()
        .filter(|(_, c)| c.failures.iter().all(Option::is_some))
        .count();
    eprintln!(
        "{} synthetic DATs, {groups} groups, {units} units compared: {differing} differ, {order_sensitive} of them \
         order-sensitive; both engines failed on {failed_alike}",
        results.len()
    );
    let mut failures = String::new();
    for (name, c) in &results {
        if c.conforms() {
            continue;
        }
        writeln!(failures, "== {name}: failures {:?}", c.failures).unwrap();
        for unit in c.unexplained().into_iter().take(20) {
            let [a, b] = &c.differing[unit];
            writeln!(failures, "unit {unit}:\n- {a:?}\n+ {b:?}").unwrap();
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Not a test: with `SYNTHETIC_SEED` and `DUMP_TO` set, writes one seed's DAT, profile and both engines' traces.
#[test]
#[ignore]
fn dump() {
    let seed: u64 = std::env::var("SYNTHETIC_SEED").unwrap().parse().unwrap();
    let to = PathBuf::from(std::env::var("DUMP_TO").unwrap());
    let engine = support::fixture_engine();
    let work = tempfile::tempdir().unwrap();
    let (_, text) = synthetic_dat(seed, &fixture_dats());
    let dat = to.join("synthetic.dat");
    std::fs::write(&dat, text).unwrap();
    let profile = synthetic_profile(seed);
    std::fs::write(
        to.join("profile.json"),
        serde_json::to_string_pretty(&profile).unwrap(),
    )
    .unwrap();
    let options = support::options(work.path());
    std::fs::write(
        to.join("bundled.jsonl"),
        engine.run(&dat, &profile, &options).unwrap().trace,
    )
    .unwrap();
    std::fs::write(
        to.join("builtin.jsonl"),
        engine.run_builtin(&dat, &profile, &options).unwrap().trace,
    )
    .unwrap();
}
