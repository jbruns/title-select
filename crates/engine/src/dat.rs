//! Reading a DAT into a DAT Revision: Logiqx XML, MAME's ListXML (`<mame>`, named `MAME` and versioned by its build),
//! or a MAME software list (`<softwarelist>`, named by its list name, unversioned). A Logiqx or ListXML Game also
//! gives the machine model the arcade selector and family counting read (ADRs 0025 to 0027), and its Disks.

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use sha2::Digest;
use title_select_api::HeaderRule;
use title_select_api::dat::{DatFormat, HeaderClaims, recognise_provider};

use crate::Error;

pub struct Rom {
    pub game: usize,
    pub name: String,
    /// Its size in bytes; 0 for a Disk, which a DAT gives no size.
    pub size: u64,
    pub crc: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    /// The name of the ROM it shares with the Game it is `romof` (`merge=`), as that Game lists it.
    pub merge: Option<String>,
    pub status: DumpStatus,
    /// Whether the Game may run without it (`optional="yes"`).
    pub optional: bool,
    /// Whether it is a Disk (`<disk>`): a CHD named `<disk>.chd`, listed by SHA-1 alone and with no size, which only
    /// its name finds.
    pub disk: bool,
}

impl Rom {
    /// Whether its Game needs it: it must be present, as it isn't optional, and a file can match it, as it isn't
    /// nodump (ADR 0025). A Game that lists none such is Nothing to Check.
    pub fn needed(&self) -> bool {
        !self.optional && self.status != DumpStatus::NoDump
    }

    /// Its name as the DAT lists it: a Disk's less `.chd`.
    pub fn listed_name(&self) -> &str {
        match self.disk {
            true => self.name.strip_suffix(".chd").unwrap_or(&self.name),
            false => &self.name,
        }
    }
}

/// Whether a ROM's dump is known good, as a DAT's `status` says; good when it says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpStatus {
    /// `good`, or Logiqx's `verified`.
    Good,
    /// `baddump`: its checksums are of a dump known to be bad.
    BadDump,
    /// `nodump`: no good dump is known, and its checksums are left out.
    NoDump,
}

impl DumpStatus {
    fn parse(text: Option<&str>) -> DumpStatus {
        match text {
            Some("baddump") => DumpStatus::BadDump,
            Some("nodump") => DumpStatus::NoDump,
            _ => DumpStatus::Good,
        }
    }
}
/// What a Game's entry says of the machine it is (ADR 0027). Every field is optional, machine by machine: none means
/// the entry doesn't say, and a filter on it keeps the machine, while some is what the entry says, an empty list
/// included. Nothing is filled in that the DAT doesn't give.
///
/// A ListXML machine is MAME's complete output, so its DTD's defaults apply and it knows every flag, list and count:
/// a flag it leaves out is the default (`runnable` yes, the rest no), and an element it leaves out is an empty list or
/// a count of 0. So no `<display>` there means screenless, and no `<input>`, a device's, means no players, coins or
/// controls. A Logiqx game knows only what its own entry writes: another game's `<display>` says nothing of its
/// screens. A software item knows nothing.
///
/// A name (`romof`, `sampleof`, `sourcefile`, `<year>`, `<manufacturer>`) and the `<driver>` are none where the entry
/// gives none, in either format: MAME leaves them out where there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Machine {
    /// The Game whose ROMs it shares (`romof`): its parent, or its BIOS for a parent.
    pub romof: Option<String>,
    pub is_bios: Option<bool>,
    pub is_device: Option<bool>,
    pub is_mechanical: Option<bool>,
    pub runnable: Option<bool>,
    /// The Game whose samples it uses (`sampleof`).
    pub sample_of: Option<String>,
    /// The driver source it is defined in (`sourcefile`), such as `atari/pong.cpp`.
    pub source_file: Option<String>,
    /// The devices it names (`<device_ref>`), in DAT order.
    pub device_refs: Option<Vec<String>>,
    /// The samples it plays (`<sample>`), in DAT order.
    pub samples: Option<Vec<String>>,
    pub year: Option<String>,
    pub manufacturer: Option<String>,
    pub driver: Option<Driver>,
    /// Its screens (`<display>`), in DAT order; empty for a screenless machine.
    pub displays: Option<Vec<Display>>,
    /// How many players its `<input>` takes (`players`).
    pub players: Option<u32>,
    /// How many coin slots its `<input>` has (`coins`).
    pub coins: Option<u32>,
    /// Each `<control>`'s type, such as `joy` or `dial`, in DAT order.
    pub controls: Option<Vec<String>>,
    /// Whether it names a software list (`<softwarelist>`).
    pub has_software_list: Option<bool>,
    /// The parts it emulates imperfectly or not at all (`<feature>`), in DAT order.
    pub features: Option<Vec<Feature>>,
}

impl Machine {
    /// A ListXML machine before its entry is read: every flag, list and count at its DTD default.
    fn listxml() -> Machine {
        Machine {
            is_bios: Some(false),
            is_device: Some(false),
            is_mechanical: Some(false),
            runnable: Some(true),
            device_refs: Some(Vec::new()),
            samples: Some(Vec::new()),
            displays: Some(Vec::new()),
            players: Some(0),
            coins: Some(0),
            controls: Some(Vec::new()),
            has_software_list: Some(false),
            features: Some(Vec::new()),
            ..Machine::default()
        }
    }

    /// Whether it says anything of `signal`.
    pub fn knows(&self, signal: Signal) -> bool {
        match signal {
            Signal::Romof => self.romof.is_some(),
            Signal::Bios => self.is_bios.is_some(),
            Signal::Device => self.is_device.is_some(),
            Signal::Mechanical => self.is_mechanical.is_some(),
            Signal::Runnable => self.runnable.is_some(),
            Signal::SampleOf => self.sample_of.is_some(),
            Signal::SourceFile => self.source_file.is_some(),
            Signal::DeviceRefs => self.device_refs.is_some(),
            Signal::Samples => self.samples.is_some(),
            Signal::Year => self.year.is_some(),
            Signal::Manufacturer => self.manufacturer.is_some(),
            Signal::Driver => self.driver.is_some(),
            Signal::Displays => self.displays.is_some(),
            Signal::Players => self.players.is_some(),
            Signal::Coins => self.coins.is_some(),
            Signal::Controls => self.controls.is_some(),
            Signal::SoftwareLists => self.has_software_list.is_some(),
            Signal::Features => self.features.is_some(),
        }
    }
}

/// What a DAT may say of its machines, one per field of [`Machine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Romof,
    Bios,
    Device,
    Mechanical,
    Runnable,
    SampleOf,
    SourceFile,
    DeviceRefs,
    Samples,
    Year,
    Manufacturer,
    Driver,
    Displays,
    Players,
    Coins,
    Controls,
    SoftwareLists,
    Features,
}

impl Signal {
    pub const ALL: [Signal; 18] = [
        Signal::Romof,
        Signal::Bios,
        Signal::Device,
        Signal::Mechanical,
        Signal::Runnable,
        Signal::SampleOf,
        Signal::SourceFile,
        Signal::DeviceRefs,
        Signal::Samples,
        Signal::Year,
        Signal::Manufacturer,
        Signal::Driver,
        Signal::Displays,
        Signal::Players,
        Signal::Coins,
        Signal::Controls,
        Signal::SoftwareLists,
        Signal::Features,
    ];
}

/// How well a driver works, as MAME rates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DriverStatus {
    Preliminary,
    Imperfect,
    Good,
}

impl DriverStatus {
    fn parse(text: &str) -> Option<DriverStatus> {
        match text {
            "good" => Some(DriverStatus::Good),
            "imperfect" => Some(DriverStatus::Imperfect),
            "preliminary" => Some(DriverStatus::Preliminary),
            _ => None,
        }
    }
}

/// A machine's `<driver>`: its overall `status` and its `emulation`'s; none for a value MAME doesn't write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Driver {
    pub status: Option<DriverStatus>,
    pub emulation: Option<DriverStatus>,
}

/// A machine's screen: its `type`, such as `raster`, `vector`, `lcd` or `svg`, and its rotation in degrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub kind: Option<String>,
    pub rotate: Option<u16>,
}

/// A part a machine emulates imperfectly or not at all: its `type`, such as `sound` or `protection`, and the
/// `status` or `overall` MAME gives it, such as `imperfect` or `unemulated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
    pub kind: String,
    pub status: Option<String>,
    pub overall: Option<String>,
}

pub struct Dat {
    pub sha256: String,
    pub format: DatFormat,
    /// Its native name: the Logiqx header's, a software list's, or `MAME` for ListXML; empty if it gives none.
    pub name: String,
    /// The Logiqx header's or software list's description; none if it gives none.
    pub description: Option<String>,
    /// The Logiqx header's author, homepage and URL, as written; none if it gives none.
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub url: Option<String>,
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
    /// What each Game's entry says of its machine, by the Game's index.
    pub machines: Vec<Machine>,
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

/// How many bytes of XML whitespace `bytes` starts with.
fn space_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .count()
}

/// How many bytes of a UTF-8 BOM `bytes` starts with.
fn bom_len(bytes: &[u8]) -> usize {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    }
}

/// Whether a DAT whose first content opens with `first` can be XML: an XML DAT opens with markup (`<?xml`, a doctype,
/// a comment, a processing instruction or its root), and anything else, such as ClrMamePro's `clrmamepro (`, is no
/// XML, whatever its text names later.
fn opens_markup(first: u8) -> bool {
    first == b'<'
}

/// The format a DAT's root element names, or `None` for one this reader doesn't know.
fn root_format(root: &BytesStart) -> Option<DatFormat> {
    match root.local_name().as_ref() {
        "datafile" => Some(DatFormat::Logiqx),
        "mame" => Some(DatFormat::Listxml),
        "softwarelist" => Some(DatFormat::SoftwareList),
        _ => None,
    }
}

/// A number attribute; none if it is absent or no number.
fn number<T: std::str::FromStr>(a: &HashMap<String, String>, key: &str) -> Option<T> {
    a.get(key).and_then(|v| v.parse().ok())
}

/// A `yes`/`no` attribute; none if it is absent or neither.
fn yes(a: &HashMap<String, String>, key: &str) -> Option<bool> {
    match a.get(key).map(String::as_str) {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    }
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
            format: DatFormat::Logiqx,
            name: String::new(),
            description: None,
            author: None,
            homepage: None,
            url: None,
            version: String::new(),
            date: String::new(),
            header_rule: None,
            games: Vec::new(),
            descriptions: Vec::new(),
            cloneof: Vec::new(),
            machines: Vec::new(),
            roms: Vec::new(),
            by_name: HashMap::new(),
            by_size_crc: HashMap::new(),
            by_size_without_crc: HashMap::new(),
            extent: None,
        };
        let mut stack: Vec<String> = Vec::new();
        let mut root = None;
        let unknown = || {
            Error::Dat(format!(
                "{}: not a Logiqx XML DAT, MAME ListXML or MAME software list",
                path.display()
            ))
        };
        let content = &bytes[bom_len(bytes)..];
        if !content
            .get(space_len(content))
            .copied()
            .is_some_and(opens_markup)
        {
            return Err(unknown());
        }
        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
            match event {
                Event::Start(e) if stack.is_empty() => {
                    let tag = e.local_name().as_ref().to_string();
                    let a = attrs(&e)?;
                    root = root_format(&e);
                    match root {
                        Some(DatFormat::Listxml) => {
                            dat.name = "MAME".into();
                            dat.version = a.get("build").cloned().unwrap_or_default();
                        }
                        Some(DatFormat::SoftwareList) => {
                            dat.name = a.get("name").cloned().unwrap_or_default();
                            dat.description = a.get("description").cloned();
                        }
                        Some(DatFormat::Logiqx) | None => {}
                    }
                    stack.push(tag);
                }
                Event::Start(e) => {
                    let tag = dat.open(&e, root)?;
                    stack.push(tag);
                }
                Event::Empty(e) => {
                    dat.open(&e, root)?;
                }
                Event::Text(_) | Event::GeneralRef(_) | Event::CData(_) => {
                    let text = match &event {
                        Event::Text(t) => t.xml10_content().into_owned(),
                        Event::CData(c) => AsRef::<str>::as_ref(c).to_owned(),
                        Event::GeneralRef(r) => match r.resolve_char_ref() {
                            Ok(Some(c)) => c.to_string(),
                            _ => quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
                                .unwrap_or_default()
                                .to_string(),
                        },
                        _ => unreachable!("matched above"),
                    };
                    let in_header = root == Some(DatFormat::Logiqx)
                        && stack.len() >= 2
                        && stack[stack.len() - 2] == "header";
                    let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str());
                    let in_game = matches!(parent, Some("game" | "machine" | "software"));
                    let in_machine = root != Some(DatFormat::SoftwareList)
                        && matches!(parent, Some("game" | "machine"));
                    let machine = dat.machines.last_mut().filter(|_| in_machine);
                    match (stack.last().map(String::as_str), machine) {
                        (Some("year"), Some(machine)) => {
                            machine.year.get_or_insert_default().push_str(&text)
                        }
                        (Some("manufacturer"), Some(machine)) => {
                            machine.manufacturer.get_or_insert_default().push_str(&text)
                        }
                        _ => {}
                    }
                    match stack.last().map(String::as_str) {
                        Some("description") if in_game => {
                            if let Some(description) = dat.descriptions.last_mut() {
                                description.push_str(&text);
                            }
                        }
                        Some("name") if in_header => dat.name.push_str(&text),
                        Some("version") if in_header => dat.version.push_str(&text),
                        Some("date") if in_header => dat.date.push_str(&text),
                        Some("description") if in_header => {
                            dat.description.get_or_insert_default().push_str(&text)
                        }
                        Some("author") if in_header => {
                            dat.author.get_or_insert_default().push_str(&text)
                        }
                        Some("homepage") if in_header => {
                            dat.homepage.get_or_insert_default().push_str(&text)
                        }
                        Some("url") if in_header => dat.url.get_or_insert_default().push_str(&text),
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
        let Some(format) = root else {
            return Err(unknown());
        };
        dat.format = format;
        for claim in [
            &mut dat.description,
            &mut dat.author,
            &mut dat.homepage,
            &mut dat.url,
        ] {
            if claim.as_ref().is_some_and(|c| c.trim().is_empty()) {
                *claim = None;
            }
        }
        Ok(dat)
    }

    /// The format of the DAT `reader` reads, by its root element as [`Dat::parse`] finds it: the first element at the
    /// top level, however long the prologue before it. `None` when that root is no element this reader knows, when
    /// there is none, when the DAT doesn't open with markup, as a ClrMamePro DAT doesn't, or when the XML breaks
    /// before it. A DAT that doesn't open with markup is read no further than its first buffer. Only an I/O error is an
    /// error.
    pub fn format_of(mut reader: impl std::io::BufRead) -> Result<Option<DatFormat>, Error> {
        let io = |err: std::io::Error| Error::Dat(err.to_string());
        let skip = bom_len(reader.fill_buf().map_err(io)?);
        reader.consume(skip);
        loop {
            let buf = reader.fill_buf().map_err(io)?;
            let skip = space_len(buf);
            match buf.get(skip) {
                Some(&first) if opens_markup(first) => break,
                Some(_) => return Ok(None),
                None if buf.is_empty() => return Ok(None),
                None => reader.consume(skip),
            }
        }
        let mut reader = quick_xml::Reader::from_reader(reader);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => return Ok(root_format(&e)),
                Ok(Event::Eof) => return Ok(None),
                Err(quick_xml::Error::Io(err)) => return Err(Error::Dat(err.to_string())),
                Err(_) => return Ok(None),
                Ok(_) => {}
            }
            buf.clear();
        }
    }

    /// The DAT Provider its format and header signal, or `None` when unknown.
    pub fn provider(&self) -> Option<&'static str> {
        recognise_provider(
            self.format,
            HeaderClaims {
                homepage: self.homepage.as_deref(),
                url: self.url.as_deref(),
                author: self.author.as_deref(),
            },
        )
    }

    /// Whether any of its machines says anything of `signal`: if none does, a filter on it is unavailable for this
    /// DAT (ADR 0027). Where it is carried, a machine that doesn't know it still says nothing, and a filter on it
    /// keeps that machine.
    pub fn carries(&self, signal: Signal) -> bool {
        self.machines.iter().any(|m| m.knows(signal))
    }

    /// Refuses a DAT whose format gives it no native name, naming the file at `path` and the missing field.
    pub fn check_name(&self, path: &Path) -> Result<(), Error> {
        if !self.name.trim().is_empty() {
            return Ok(());
        }
        let missing = match self.format {
            DatFormat::Logiqx => "its header has no <name>",
            DatFormat::Listxml => "it has no name",
            DatFormat::SoftwareList => "its <softwarelist> has no name attribute",
        };
        Err(Error::Dat(format!(
            "{}: the DAT has no name to know it by: {missing}",
            path.display()
        )))
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
    fn open(&mut self, e: &BytesStart, root: Option<DatFormat>) -> Result<String, Error> {
        let tag = e.local_name().as_ref().to_string();
        match (root, tag.as_str()) {
            (Some(format @ (DatFormat::Logiqx | DatFormat::Listxml)), "game" | "machine") => {
                self.start_game(e)?;
                let a = attrs(e)?;
                let machine = match format {
                    DatFormat::Listxml => Machine::listxml(),
                    _ => Machine::default(),
                };
                let flag = |key: &str, default: Option<bool>| yes(&a, key).or(default);
                self.machines.push(Machine {
                    romof: a.get("romof").cloned(),
                    is_bios: flag("isbios", machine.is_bios),
                    is_device: flag("isdevice", machine.is_device),
                    is_mechanical: flag("ismechanical", machine.is_mechanical),
                    runnable: flag("runnable", machine.runnable),
                    sample_of: a.get("sampleof").cloned(),
                    source_file: a.get("sourcefile").cloned(),
                    ..machine
                });
            }
            (Some(DatFormat::SoftwareList), "software") => {
                self.start_game(e)?;
                self.machines.push(Machine::default());
            }
            // A software list's unnamed ROM has no file of its own: it reads on in the file of the named ROM before
            // it. As MAME's rom_file_size measures that file, one that continues or skips reads on from where the
            // last read ended, and one that reloads reads again from the start; the file is as large as the
            // furthest any read reaches. Any other, such as a fill, reads no file, and ends the run.
            (Some(DatFormat::SoftwareList), "rom") if !attrs(e)?.contains_key("name") => {
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
            // Disks come only from ListXML machines (ADR 0026's amendment).
            (Some(DatFormat::Listxml), "disk") => self.add_disk(e)?,
            (
                Some(DatFormat::Logiqx | DatFormat::Listxml),
                "device_ref" | "sample" | "driver" | "display" | "input" | "control"
                | "softwarelist" | "feature",
            ) => {
                let Some(machine) = self.machines.last_mut() else {
                    return Ok(tag);
                };
                let mut a = attrs(e)?;
                match tag.as_str() {
                    "device_ref" => machine
                        .device_refs
                        .get_or_insert_default()
                        .extend(a.remove("name")),
                    "sample" => machine
                        .samples
                        .get_or_insert_default()
                        .extend(a.remove("name")),
                    "driver" => {
                        let status = |key: &str| a.get(key).and_then(|v| DriverStatus::parse(v));
                        machine.driver = Some(Driver {
                            status: status("status"),
                            emulation: status("emulation"),
                        });
                    }
                    "display" => machine.displays.get_or_insert_default().push(Display {
                        kind: a.remove("type"),
                        rotate: number(&a, "rotate"),
                    }),
                    // A count left out keeps ListXML's 0, where MAME leaves out no coins; in Logiqx it stays unknown.
                    "input" => {
                        machine.players = number(&a, "players").or(machine.players);
                        machine.coins = number(&a, "coins").or(machine.coins);
                    }
                    "control" => machine
                        .controls
                        .get_or_insert_default()
                        .extend(a.remove("type")),
                    "softwarelist" => machine.has_software_list = Some(true),
                    "feature" => {
                        if let Some(kind) = a.remove("type") {
                            machine.features.get_or_insert_default().push(Feature {
                                kind,
                                status: a.remove("status"),
                                overall: a.remove("overall"),
                            });
                        }
                    }
                    _ => unreachable!("matched above"),
                }
            }
            (Some(DatFormat::Logiqx), "clrmamepro") if self.games.is_empty() => {
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
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a rom has no name".into()))?
            .clone();
        let size = a
            .get("size")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Dat(format!("rom {name} has no valid size")))?;
        let lower = |k: &str| a.get(k).map(|v| v.to_ascii_lowercase());
        let rom = self.listed(&a, "rom", name, a.get("merge").cloned())?;
        let rom = Rom {
            size,
            crc: lower("crc"),
            md5: lower("md5"),
            sha1: lower("sha1"),
            sha256: lower("sha256"),
            ..rom
        };
        let idx = self.roms.len();
        self.extent = Some((rom.size, rom.size));
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
        self.push_rom(rom);
        Ok(())
    }

    /// Adds a `<disk>` as a Disk of the last Game: a ROM named for its CHD, which a DAT gives no size. Only its name
    /// finds it here; matching it by its SHA-1 is #228's.
    fn add_disk(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let chd = |name: &String| format!("{name}.chd");
        let name = chd(a
            .get("name")
            .ok_or_else(|| Error::Dat("a disk has no name".into()))?);
        let rom = self.listed(&a, "disk", name, a.get("merge").map(chd))?;
        self.extent = None;
        self.push_rom(Rom {
            sha1: a.get("sha1").map(|v| v.to_ascii_lowercase()),
            disk: true,
            ..rom
        });
        Ok(())
    }

    /// A ROM of the last Game named `name`, with what its `element`'s attributes `a` say of it beside its size and
    /// checksums, which it leaves out.
    fn listed(
        &self,
        a: &HashMap<String, String>,
        element: &str,
        name: String,
        merge: Option<String>,
    ) -> Result<Rom, Error> {
        let game = self
            .games
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::Dat(format!("a {element} is outside any game")))?;
        Ok(Rom {
            game,
            name,
            size: 0,
            crc: None,
            md5: None,
            sha1: None,
            sha256: None,
            merge,
            status: DumpStatus::parse(a.get("status").map(String::as_str)),
            optional: yes(a, "optional").unwrap_or(false),
            disk: false,
        })
    }

    /// Adds `rom` as the next ROM, found by its name.
    fn push_rom(&mut self, rom: Rom) {
        self.by_name
            .entry(rom.name.clone())
            .or_default()
            .push(self.roms.len());
        self.roms.push(rom);
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
