//! A machine's category: from catver's `Top / Sub` category where catver lists the machine, else from ListXML alone
//! (research 0105 §4): its driver's source directory, whether it names a software list, and its control types. The
//! heuristic agrees with catver about 91% of the time on MAME 0.289, and misses quiz, mature and most tabletop machines.

use title_select_engine::dat::Machine;

/// A kind of machine the category filter can drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Computers, consoles, handhelds, calculators, chess computers, instruments and other machines that aren't arcade
    /// games.
    NonArcade,
    /// Gambling, redemption and electromechanical machines.
    Gambling,
    /// Mahjong, hanafuda and other tabletop machines.
    Tabletop,
    /// Board games and chess computers.
    BoardGame,
    /// Quiz and trivia machines.
    Quiz,
    /// What catver marks `* Mature *`, whatever its category.
    Mature,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::NonArcade => "non-arcade",
            Category::Gambling => "gambling",
            Category::Tabletop => "tabletop",
            Category::BoardGame => "board game",
            Category::Quiz => "quiz",
            Category::Mature => "mature",
        }
    }
}

/// catver's top-level categories of machines that aren't arcade games.
const NON_ARCADE_CATEGORIES: &[&str] = &[
    "System",
    "Computer",
    "Game Console",
    "Handheld",
    "Calculator",
    "Telephone",
    "Printer",
    "Watch",
    "Utilities",
    "Misc.",
    "Medical Equipment",
    "Musical Instrument",
    "Music Player",
];

/// catver's top-level categories of gambling, redemption and electromechanical machines.
const GAMBLING_CATEGORIES: &[&str] = &[
    "Electromechanical",
    "Slot Machine",
    "Gambling",
    "Casino",
    "Medal Game",
    "Redemption Game",
    "Luna Park",
    "Whac-A-Mole",
];

/// The kinds catver's `category`, such as `Tabletop / Mahjong * Mature *`, makes a machine: by its top-level category,
/// and Mature if it is marked so. An arcade game's is empty.
pub fn from_catver(category: &str) -> Vec<Category> {
    let (category, mature) = match category.strip_suffix("* Mature *") {
        Some(rest) => (rest.trim(), true),
        None => (category.trim(), false),
    };
    let top = category.split(" / ").next().unwrap_or_default().trim();
    let mut kinds = Vec::new();
    if NON_ARCADE_CATEGORIES.contains(&top) {
        kinds.push(Category::NonArcade);
    } else if GAMBLING_CATEGORIES.contains(&top) {
        kinds.push(Category::Gambling);
    } else if top == "Tabletop" {
        kinds.push(Category::Tabletop);
    } else if top == "Board Game" {
        kinds.push(Category::BoardGame);
    } else if top == "Quiz" {
        kinds.push(Category::Quiz);
    }
    if mature {
        kinds.push(Category::Mature);
    }
    kinds
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catver_categories_are_read_by_their_top_level_and_the_mature_mark() {
        assert_eq!(
            from_catver("Slot Machine / Video Slot"),
            [Category::Gambling]
        );
        assert_eq!(
            from_catver("Tabletop / Mahjong * Mature *"),
            [Category::Tabletop, Category::Mature]
        );
        assert_eq!(from_catver("Puzzle / Drop * Mature *"), [Category::Mature]);
        assert_eq!(from_catver("Misc. / Laserdisc"), [Category::NonArcade]);
        assert!(from_catver("Shooter / Flying Vertical").is_empty());
        // A top-level category is matched whole, never by its start.
        assert!(from_catver("Quizzical / Trivia").is_empty());
    }
}
