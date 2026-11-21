//! The Support Files the arcade selector reads (ADR 0027): progetto-SNAPS's `catver.ini`, `languages.ini` and
//! `bestgames.ini`. Each is an INI file whose header names it and its MAME version. catver lists `shortname=Category`
//! lines under `[Category]`; languages and bestgames list shortnames, one a line, under a section named for a language
//! or a rating tier, such as `[English]` or `[70 to 80 (Good)]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use title_select_api::selection::SupportFilePin;
use title_select_api::support::{SupportFileKind, read_header};

use crate::Error;

/// Sections that hold no machines.
const SETTINGS: &[&str] = &["FOLDER_SETTINGS", "ROOT_FOLDER", "VerAdded"];

/// A rating tier of bestgames, such as `70 to 80 (Good)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    pub min: u32,
    pub max: u32,
    pub label: String,
}

impl Tier {
    fn parse(section: &str) -> Option<Tier> {
        let (range, label) = section.split_once('(')?;
        let (min, max) = range.trim().split_once(" to ")?;
        Some(Tier {
            min: min.trim().parse().ok()?,
            max: max.trim().parse().ok()?,
            label: label.trim_end_matches(')').trim().to_owned(),
        })
    }

    /// "70–80 (Good)".
    pub fn describe(&self) -> String {
        format!("{}–{} ({})", self.min, self.max, self.label)
    }
}

/// The Support Files a run reads, each optional.
#[derive(Debug, Default)]
pub struct SupportFiles {
    /// Each machine's catver category, such as `Maze / Shooter Small * Mature *`.
    pub catver: Option<HashMap<String, String>>,
    /// The languages each machine is in, in the file's order.
    pub languages: Option<HashMap<String, Vec<String>>>,
    /// Each rated machine's tier.
    pub bestgames: Option<HashMap<String, Tier>>,
    /// What was read, in kind order.
    pub pins: Vec<SupportFilePin>,
}

impl SupportFiles {
    /// Reads each of `paths`, which its header says which Support File it is. Two files of one kind are refused.
    pub fn read(paths: &[PathBuf]) -> Result<SupportFiles, Error> {
        let mut files = SupportFiles::default();
        for path in paths {
            files.add(path)?;
        }
        files.pins.sort_by_key(|p| p.kind);
        Ok(files)
    }

    fn add(&mut self, path: &Path) -> Result<(), Error> {
        let bytes = std::fs::read(path).map_err(crate::io(path.display().to_string()))?;
        let (kind, mame_version) =
            read_header(&bytes, &path.display().to_string()).map_err(Error::Refused)?;
        if self.pins.iter().any(|p| p.kind == kind) {
            return Err(Error::Refused(format!(
                "{}: a selection reads one {}, and this is the second",
                path.display(),
                kind.file_name()
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let sections = sections(&text);
        match kind {
            SupportFileKind::Catver => {
                let mut categories = HashMap::new();
                for (section, line) in sections {
                    if section == "Category"
                        && let Some((name, category)) = line.split_once('=')
                    {
                        categories.insert(name.trim().to_owned(), category.trim().to_owned());
                    }
                }
                self.catver = Some(categories);
            }
            SupportFileKind::Languages => {
                let mut languages: HashMap<String, Vec<String>> = HashMap::new();
                for (section, line) in sections {
                    languages
                        .entry(line.to_owned())
                        .or_default()
                        .push(section.to_owned());
                }
                self.languages = Some(languages);
            }
            SupportFileKind::Bestgames => {
                let mut tiers = HashMap::new();
                for (section, line) in sections {
                    if let Some(tier) = Tier::parse(section) {
                        tiers.insert(line.to_owned(), tier);
                    }
                }
                self.bestgames = Some(tiers);
            }
        }
        self.pins.push(SupportFilePin {
            kind,
            sha256: crate::hex(&Sha256::digest(&bytes)),
            mame_version,
            drift: false,
        });
        Ok(())
    }

    /// bestgames' tiers and the MAME version it names, if it was read.
    pub fn bestgames(&self) -> Option<(&HashMap<String, Tier>, &str)> {
        Some((
            self.bestgames.as_ref()?,
            self.version(SupportFileKind::Bestgames)?,
        ))
    }

    /// The MAME version the Support File of `kind` names, if one was read.
    pub fn version(&self, kind: SupportFileKind) -> Option<&str> {
        self.pins
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.mame_version.as_str())
    }
}

/// Each line that names a machine, with the section it is under: not blank, not a comment, and not in a settings
/// section.
fn sections(text: &str) -> Vec<(&str, &str)> {
    let mut section = "";
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        if !section.is_empty() && !SETTINGS.contains(&section) {
            out.push((section, line));
        }
    }
    out
}

/// A MAME version, such as `0.289` or the `0.289 (mame0289)` a ListXML's build names, as numbers to compare.
pub fn version_number(version: &str) -> Option<(u32, u32)> {
    let (major, minor) = version.split_whitespace().next()?.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_reads_its_bounds_and_label() {
        let tier = Tier::parse("90 to 100 (Best Games)").unwrap();
        assert_eq!((tier.min, tier.max), (90, 100));
        assert_eq!(tier.describe(), "90–100 (Best Games)");
        assert_eq!(Tier::parse("English"), None);
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version_number("0.99") < version_number("0.289 (mame0289)"));
        assert_eq!(version_number("mame0289"), None);
    }

    #[test]
    fn settings_sections_and_comments_name_no_machine() {
        let text = "[FOLDER_SETTINGS]\r\nRootFolderIcon mame\r\n;; x ;;\r\n[ROOT_FOLDER]\r\n\r\n[English]\r\n1941\r\n";
        assert_eq!(sections(text), [("English", "1941")]);
    }
}
