//! Exporting what a Source Set lacks, from an Audit against a DAT Revision: the Fixdat, a Logiqx XML DAT of its
//! Missing Games and of its Incomplete Games' missing ROMs, and the have/miss CSV, one row per Game.
//!
//! Both are deterministic: Games come in DAT order, and every value is taken from the Audit and its DAT Revision,
//! never from the time of the export.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::dat::{Dat, Rom};
use crate::{Game, GameStatus, Report};

/// The DOCTYPE of a Logiqx XML DAT.
const DOCTYPE: &str = r#"datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd""#;

/// Separates ROM names within a have/miss cell. No ROM name holds it: it can't be in a file name on Windows.
const ROM_SEPARATOR: &str = "|";

/// The Games an export is limited to: a Selection's winners.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub selection: i64,
    pub games: &'a HashSet<String>,
}

/// What a Source Set lacks, by its Audit `audit`, whose report is `report`, against `dat`, the DAT Revision that
/// Audit used.
pub struct Lacking<'a> {
    pub dat: &'a Dat,
    pub report: &'a Report,
    pub audit: i64,
    /// Every Game of the DAT when none.
    pub scope: Option<Scope<'a>>,
}

impl Lacking<'_> {
    /// The Fixdat, as Logiqx XML.
    pub fn fixdat(&self) -> String {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        let mut put = |event: Event| writer.write_event(event).expect("writing to memory");
        put(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)));
        put(Event::DocType(BytesText::from_escaped(DOCTYPE)));
        put(Event::Start(BytesStart::new("datafile")));
        put(Event::Start(BytesStart::new("header")));
        let name = format!("fixdat_{}", self.dat.name);
        let version = match self.dat.version.as_str() {
            "" => self.date(),
            version => version.to_string(),
        };
        let mut description = format!(
            "{name} ({}): what source set {} lacks against {} version {}, DAT revision {}, by audit {} finished {}",
            self.date(),
            self.report.source_set,
            self.dat.name,
            self.dat.version,
            self.dat.sha256,
            self.audit,
            self.report.finished_at,
        );
        if let Some(scope) = self.scope {
            let _ = write!(
                description,
                ", limited to the winners of selection {}",
                scope.selection
            );
        }
        for (tag, text) in [
            ("name", name.as_str()),
            ("description", description.as_str()),
            ("version", version.as_str()),
            ("date", self.date().as_str()),
            ("author", "Title Select"),
        ] {
            put(Event::Start(BytesStart::new(tag)));
            put(Event::Text(BytesText::new(text)));
            put(Event::End(BytesEnd::new(tag)));
        }
        // Its ROMs leave out the Header as the DAT's do, so it names the same Header Rule.
        if let Some(rule) = &self.dat.header_rule {
            put(Event::Empty(
                BytesStart::new("clrmamepro").with_attributes([("header", rule.as_str())]),
            ));
        }
        put(Event::End(BytesEnd::new("header")));
        let roms_of = self.roms_by_game();
        for (index, game, status) in self.games() {
            let missing: HashSet<&str> = game.missing_roms.iter().map(String::as_str).collect();
            let roms: Vec<&Rom> = roms_of[index]
                .iter()
                .map(|r| &self.dat.roms[*r])
                .filter(|r| status == GameStatus::Missing || missing.contains(r.name.as_str()))
                .collect();
            if status == GameStatus::Complete || roms.is_empty() {
                continue;
            }
            put(Event::Start(
                BytesStart::new("game").with_attributes([("name", game.name.as_str())]),
            ));
            put(Event::Start(BytesStart::new("description")));
            let description = match self.dat.descriptions[index].as_str() {
                "" => game.name.as_str(),
                description => description,
            };
            put(Event::Text(BytesText::new(description)));
            put(Event::End(BytesEnd::new("description")));
            for rom in roms {
                let size = rom.size.to_string();
                let mut attrs = vec![("name", rom.name.as_str()), ("size", size.as_str())];
                for (key, hash) in [
                    ("crc", &rom.crc),
                    ("md5", &rom.md5),
                    ("sha1", &rom.sha1),
                    ("sha256", &rom.sha256),
                ] {
                    if let Some(hash) = hash {
                        attrs.push((key, hash.as_str()));
                    }
                }
                put(Event::Empty(BytesStart::new("rom").with_attributes(attrs)));
            }
            put(Event::End(BytesEnd::new("game")));
        }
        put(Event::End(BytesEnd::new("datafile")));
        let mut xml = String::from_utf8(writer.into_inner()).expect("written from strings");
        xml.push('\n');
        xml
    }

    /// `fixdat_<DAT name> (<date>).dat`.
    pub fn fixdat_file_name(&self) -> String {
        file_name(&format!("fixdat_{} ({}).dat", self.dat.name, self.date()))
    }

    /// The have/miss CSV: a header, then a row per Game in scope, in DAT order.
    pub fn have_miss(&self) -> String {
        let mut csv =
            String::from("game,status,present_roms,missing_roms,available_elsewhere_roms\r\n");
        let roms_of = self.roms_by_game();
        for (index, game, status) in self.games() {
            let missing: HashSet<&str> = game.missing_roms.iter().map(String::as_str).collect();
            let present: Vec<&str> = roms_of[index]
                .iter()
                .map(|r| self.dat.roms[*r].name.as_str())
                .filter(|name| !missing.contains(name))
                .collect();
            // Available Elsewhere ROMs are not reported until #125 makes a Game's status count only its own files;
            // until then the column is empty.
            let available_elsewhere = String::new();
            let row = [
                game.name.clone(),
                status.as_str().to_string(),
                present.join(ROM_SEPARATOR),
                game.missing_roms.join(ROM_SEPARATOR),
                available_elsewhere,
            ];
            let row: Vec<String> = row.iter().map(|cell| csv_cell(cell)).collect();
            csv.push_str(&row.join(","));
            csv.push_str("\r\n");
        }
        csv
    }

    /// `have-miss_<DAT name> (<date>).csv`.
    pub fn have_miss_file_name(&self) -> String {
        file_name(&format!(
            "have-miss_{} ({}).csv",
            self.dat.name,
            self.date()
        ))
    }

    /// The day the Audit finished, in UTC.
    fn date(&self) -> String {
        self.report.finished_at.strftime("%Y-%m-%d").to_string()
    }

    /// The Games in scope, in DAT order, each with its index in the DAT and its status in the Audit.
    fn games(&self) -> impl Iterator<Item = (usize, &Game, GameStatus)> {
        let by_name: HashMap<&str, &Game> = self
            .report
            .games
            .iter()
            .map(|g| (g.name.as_str(), g))
            .collect();
        self.dat
            .games
            .iter()
            .enumerate()
            .filter(|(_, name)| self.scope.is_none_or(|s| s.games.contains(*name)))
            .filter_map(move |(index, name)| {
                by_name
                    .get(name.as_str())
                    .map(|game| (index, *game, game.status))
            })
    }

    /// The indexes of each Game's ROMs, by the Game's index.
    fn roms_by_game(&self) -> Vec<Vec<usize>> {
        let mut roms = vec![Vec::new(); self.dat.games.len()];
        for (index, rom) in self.dat.roms.iter().enumerate() {
            roms[rom.game].push(index);
        }
        roms
    }
}

/// A CSV cell, quoted when it holds a comma, a quote or a line break.
fn csv_cell(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// A file name with any character a file system may refuse replaced by `_`.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}
