//! Reading a DAT into a DAT Revision: Logiqx XML, MAME's ListXML (`<mame>`, named `MAME` and versioned by its build),
//! or a MAME software list (`<softwarelist>`, named by its list name, unversioned).

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use sha2::Digest;
use title_select_api::HeaderRule;

use crate::Error;

pub struct Rom {
    pub game: usize,
    pub name: String,
    pub size: u64,
    pub crc: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

/// The root element a DAT is read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    Logiqx,
    ListXml,
    SoftwareList,
}

pub struct Dat {
    pub sha256: String,
    pub name: String,
    pub version: String,
    /// The header's date, as the provider writes it; empty if it gives none.
    pub date: String,
    /// The Header Rule the header names (`<clrmamepro header="…"/>`), as it names it; none if it names none.
    pub header_rule: Option<String>,
    pub games: Vec<String>,
    /// Each Game's description, by the Game's index; empty if the DAT gives none.
    pub descriptions: Vec<String>,
    /// The parent each Game is a clone of (`cloneof`), by the Game's index; none for a parent or a DAT without them.
    pub cloneof: Vec<Option<String>>,
    pub roms: Vec<Rom>,
    by_name: HashMap<String, Vec<usize>>,
    by_size_crc: HashMap<(u64, String), Vec<usize>>,
    /// ROMs the DAT lists without a CRC, which the size-and-CRC index cannot find.
    by_size_without_crc: HashMap<u64, Vec<usize>>,
    /// Where a software list's last ROM is read to, and the furthest any read of it reached, while unnamed ROMs that
    /// follow may still read on in its file.
    extent: Option<(u64, u64)>,
}

/// A size as DATs write it: decimal, or hexadecimal after `0x`.
fn parse_size(text: &str) -> Option<u64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn attrs(e: &BytesStart) -> Result<HashMap<String, String>, Error> {
    let mut out = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|err| Error::Dat(err.to_string()))?;
        let key = a.key.as_ref().to_string();
        let value = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| Error::Dat(err.to_string()))?;
        out.insert(key, value.into_owned());
    }
    Ok(out)
}

impl Dat {
    pub fn read(path: &Path) -> Result<Dat, Error> {
        let bytes =
            std::fs::read(path).map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
        Dat::parse(&bytes, path)
    }

    /// Parses the bytes of the DAT file at `path`; the path only labels errors.
    pub fn parse(bytes: &[u8], path: &Path) -> Result<Dat, Error> {
        let sha256 = crate::hex(&sha2::Sha256::digest(bytes));
        let mut reader = quick_xml::Reader::from_reader(bytes);
        let mut buf = Vec::new();
        let mut dat = Dat {
            sha256,
            name: String::new(),
            version: String::new(),
            date: String::new(),
            header_rule: None,
            games: Vec::new(),
            descriptions: Vec::new(),
            cloneof: Vec::new(),
            roms: Vec::new(),
            by_name: HashMap::new(),
            by_size_crc: HashMap::new(),
            by_size_without_crc: HashMap::new(),
            extent: None,
        };
        let mut stack: Vec<String> = Vec::new();
        let mut root = None;
        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
            match event {
                Event::Start(e) if stack.is_empty() => {
                    let tag = e.local_name().as_ref().to_string();
                    let a = attrs(&e)?;
                    root = match tag.as_str() {
                        "datafile" => Some(Root::Logiqx),
                        "mame" => {
                            dat.name = "MAME".into();
                            dat.version = a.get("build").cloned().unwrap_or_default();
                            Some(Root::ListXml)
                        }
                        "softwarelist" => {
                            dat.name = a.get("name").cloned().unwrap_or_default();
                            Some(Root::SoftwareList)
                        }
                        _ => None,
                    };
                    stack.push(tag);
                }
                Event::Start(e) => {
                    let tag = dat.open(&e, root)?;
                    stack.push(tag);
                }
                Event::Empty(e) => {
                    dat.open(&e, root)?;
                }
                Event::Text(_) | Event::GeneralRef(_) => {
                    let text = match &event {
                        Event::Text(t) => t.xml10_content().into_owned(),
                        Event::GeneralRef(r) => match r.resolve_char_ref() {
                            Ok(Some(c)) => c.to_string(),
                            _ => quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
                                .unwrap_or_default()
                                .to_string(),
                        },
                        _ => unreachable!("matched above"),
                    };
                    let in_header = root == Some(Root::Logiqx)
                        && stack.len() >= 2
                        && stack[stack.len() - 2] == "header";
                    let in_game = stack.len() >= 2
                        && matches!(
                            stack[stack.len() - 2].as_str(),
                            "game" | "machine" | "software"
                        );
                    match stack.last().map(String::as_str) {
                        Some("description") if in_game => {
                            if let Some(description) = dat.descriptions.last_mut() {
                                description.push_str(&text);
                            }
                        }
                        Some("name") if in_header => dat.name.push_str(&text),
                        Some("version") if in_header => dat.version.push_str(&text),
                        Some("date") if in_header => dat.date.push_str(&text),
                        _ => {}
                    }
                }
                Event::End(_) => {
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        if root.is_none() {
            return Err(Error::Dat(format!(
                "{}: not a Logiqx XML DAT, MAME ListXML or MAME software list",
                path.display()
            )));
        }
        Ok(dat)
    }

    /// The date in the Logiqx header at the start of `bytes`, read no further than the header's end: empty if there is
    /// none, or the header is not whole in `bytes`.
    pub fn header_date(bytes: &[u8]) -> String {
        let mut reader = quick_xml::Reader::from_reader(bytes);
        let mut buf = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        let mut date = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let tag = e.local_name().as_ref().to_string();
                    if stack.is_empty() && tag != "datafile" {
                        return String::new();
                    }
                    stack.push(tag);
                }
                Ok(Event::Text(t))
                    if stack.len() == 3 && stack[1] == "header" && stack[2] == "date" =>
                {
                    date.push_str(&t.xml10_content());
                }
                Ok(Event::End(_)) => {
                    if stack.pop().is_some_and(|t| t == "header") {
                        return date;
                    }
                }
                Ok(Event::Eof) | Err(_) => return String::new(),
                _ => {}
            }
            buf.clear();
        }
    }

    /// Handles an opening or empty tag inside the root and returns its name.
    fn open(&mut self, e: &BytesStart, root: Option<Root>) -> Result<String, Error> {
        let tag = e.local_name().as_ref().to_string();
        match (root, tag.as_str()) {
            (Some(Root::Logiqx | Root::ListXml), "game" | "machine") => self.start_game(e)?,
            (Some(Root::SoftwareList), "software") => self.start_game(e)?,
            // A software list's unnamed ROM has no file of its own: it reads on in the file of the named ROM before
            // it. As MAME's rom_file_size measures that file, one that continues or skips reads on from where the
            // last read ended, and one that reloads reads again from the start; the file is as large as the
            // furthest any read reaches. Any other, such as a fill, reads no file, and ends the run.
            (Some(Root::SoftwareList), "rom") if !attrs(e)?.contains_key("name") => {
                let a = attrs(e)?;
                let read = |pos: u64| -> Result<u64, Error> {
                    a.get("size")
                        .and_then(|s| parse_size(s))
                        .map(|size| pos + size)
                        .ok_or_else(|| {
                            Error::Dat("a rom read in its file has no valid size".into())
                        })
                };
                self.extent = match (self.extent, a.get("loadflag").map(String::as_str)) {
                    (Some((pos, max)), Some("continue" | "ignore")) => Some((read(pos)?, max)),
                    (Some((_, max)), Some("reload" | "reload_plain")) => Some((read(0)?, max)),
                    _ => None,
                };
                if let Some((pos, max)) = self.extent
                    && pos > max
                {
                    self.extent = Some((pos, pos));
                    self.resize_last_rom(pos);
                }
            }
            (Some(_), "rom") => self.add_rom(e)?,
            (Some(Root::Logiqx), "clrmamepro") if self.games.is_empty() => {
                if let Some(rule) = attrs(e)?.remove("header") {
                    self.header_rule = Some(rule);
                }
            }
            _ => {}
        }
        Ok(tag)
    }

    fn start_game(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a game has no name".into()))?;
        self.games.push(name.clone());
        self.descriptions.push(String::new());
        self.cloneof.push(a.get("cloneof").cloned());
        self.extent = None;
        Ok(())
    }

    fn add_rom(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let game = self
            .games
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::Dat("a rom is outside any game".into()))?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a rom has no name".into()))?
            .clone();
        let size = a
            .get("size")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Dat(format!("rom {name} has no valid size")))?;
        let lower = |k: &str| a.get(k).map(|v| v.to_ascii_lowercase());
        let rom = Rom {
            game,
            name,
            size,
            crc: lower("crc"),
            md5: lower("md5"),
            sha1: lower("sha1"),
            sha256: lower("sha256"),
        };
        let idx = self.roms.len();
        self.extent = Some((rom.size, rom.size));
        self.by_name.entry(rom.name.clone()).or_default().push(idx);
        match &rom.crc {
            Some(crc) => self
                .by_size_crc
                .entry((rom.size, crc.clone()))
                .or_default()
                .push(idx),
            None => self
                .by_size_without_crc
                .entry(rom.size)
                .or_default()
                .push(idx),
        }
        self.roms.push(rom);
        Ok(())
    }

    /// Makes the last ROM read `size` bytes.
    fn resize_last_rom(&mut self, size: u64) {
        let Some(idx) = self.roms.len().checked_sub(1) else {
            return;
        };
        let rom = &self.roms[idx];
        let bucket = match &rom.crc {
            Some(crc) => self.by_size_crc.get_mut(&(rom.size, crc.clone())),
            None => self.by_size_without_crc.get_mut(&rom.size),
        };
        if let Some(bucket) = bucket {
            bucket.retain(|&i| i != idx);
        }
        let rom = &mut self.roms[idx];
        rom.size = size;
        match &rom.crc {
            Some(crc) => self
                .by_size_crc
                .entry((rom.size, crc.clone()))
                .or_default()
                .push(idx),
            None => self
                .by_size_without_crc
                .entry(rom.size)
                .or_default()
                .push(idx),
        }
    }

    /// The built-in Header Rule the header names; none if it names none, or one Title Select does not know.
    pub fn header(&self) -> Option<HeaderRule> {
        self.header_rule.as_deref().and_then(HeaderRule::named)
    }

    /// What a Build expects of `rom`, one of this DAT's ROMs: its size and checksums, and the Header Rule they leave
    /// a Header out under.
    pub fn expected(&self, rom: &Rom) -> title_select_api::build::ExpectedRom {
        title_select_api::build::ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
            header: self.header(),
        }
    }

    pub fn roms_named(&self, name: &str) -> &[usize] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// The ROMs a payload of this size and CRC32 could match; each must still be compared in full.
    pub fn candidates(&self, size: u64, crc: &str) -> impl Iterator<Item = usize> + '_ {
        let with_crc = self.by_size_crc.get(&(size, crc.to_string()));
        let without_crc = self.by_size_without_crc.get(&size);
        with_crc.into_iter().chain(without_crc).flatten().copied()
    }
}
