//! A machine's category from ListXML alone (research 0105 §4): its driver's source directory, whether it names a
//! software list, and its control types. It agrees with catver about 91% of the time on MAME 0.289, misses quiz,
//! mature and most tabletop machines, and stands in until the support files are read (#251).

use title_select_api::arcade::Category;
use title_select_engine::dat::Machine;

/// Directories of MAME 0.289's `src/mame` that hold only machines that aren't arcade games: handhelds, TV games,
/// skeleton drivers, pinball, chess computers, and makers of computers, calculators and instruments.
const NON_ARCADE_DIRECTORIES: &[&str] = &[
    "acorn",
    "akai",
    "alesis",
    "amiga",
    "amstrad",
    "apollo",
    "apple",
    "casio",
    "chess",
    "chessking",
    "commodore",
    "dec",
    "ensoniq",
    "fairlight",
    "fidelity",
    "handheld",
    "hegenerglaser",
    "hp",
    "ibm",
    "kaypro",
    "korg",
    "kurzweil",
    "leapfrog",
    "linn",
    "moog",
    "msx",
    "novag",
    "novation",
    "oberheim",
    "osborne",
    "palm",
    "pc",
    "pinball",
    "psion",
    "roland",
    "saitek",
    "sequential",
    "sgi",
    "sinclair",
    "skeleton",
    "sun",
    "ti",
    "trs",
    "tvgames",
    "vtech",
    "wang",
    "xerox",
    "yamaha",
];

/// Directories of MAME 0.289's `src/mame` that hold gambling machines.
const GAMBLING_DIRECTORIES: &[&str] = &[
    "aristocrat",
    "astrocorp",
    "barcrest",
    "bfm",
    "cirsa",
    "igt",
    "jpm",
    "maygay",
];

/// The category the heuristic gives `machine`, and why, or `None` for an arcade game or a machine that says too little
/// to tell.
pub fn heuristic(machine: &Machine) -> Option<(Category, String)> {
    let controls = machine.controls.as_deref().unwrap_or_default();
    let has = |kind: &str| controls.iter().any(|c| c == kind);
    if has("gambling") {
        return Some((Category::Gambling, "it has gambling controls".into()));
    }
    for kind in ["mahjong", "hanafuda"] {
        if has(kind) {
            return Some((Category::Tabletop, format!("it has {kind} controls")));
        }
    }
    if has("keyboard") {
        return Some((Category::NonArcade, "it has a keyboard".into()));
    }
    if machine.has_software_list == Some(true) {
        return Some((Category::NonArcade, "it names a software list".into()));
    }
    let directory = machine
        .source_file
        .as_deref()
        .and_then(|file| file.split_once('/'))
        .map(|(directory, _)| directory)?;
    let because = || format!("its driver is in {directory}/");
    if NON_ARCADE_DIRECTORIES.contains(&directory) {
        return Some((Category::NonArcade, because()));
    }
    if GAMBLING_DIRECTORIES.contains(&directory) {
        return Some((Category::Gambling, because()));
    }
    None
}
