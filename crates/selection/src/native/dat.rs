//! Reading a DAT file as Retool's `parse_dat` does: its header details and its titles, from Logiqx XML or
//! ClrMamePro.

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::py;
use super::pyre::Re;
use super::pyset::PySet;
use super::settings::Settings;
use super::titletools;
use crate::Error;

#[derive(Debug, Clone, Default)]
pub(crate) struct Rom {
    pub name: String,
    pub size: String,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
    pub kind: String,
    pub mia: String,
    pub header: String,
}

impl PartialEq for Rom {
    fn eq(&self, other: &Rom) -> bool {
        (
            &self.name,
            &self.size,
            &self.crc,
            &self.md5,
            &self.sha1,
            &self.sha256,
            &self.kind,
            &self.mia,
            &self.header,
        ) == (
            &other.name,
            &other.size,
            &other.crc,
            &other.md5,
            &other.sha1,
            &other.sha256,
            &other.kind,
            &other.mia,
            &other.header,
        )
    }
}

/// A title as the DAT holds it (`TitleData`).
#[derive(Debug, Clone, Default)]
pub(crate) struct TitleData {
    pub name: String,
    /// A Python set of strings: Retool copies it into the title's categories in the set's order.
    pub categories: PySet<String>,
    pub description: String,
    pub files: Vec<Rom>,
}

/// The DAT's header (`Dat`).
#[derive(Debug, Clone)]
pub(crate) struct Dat {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub homepage: String,
    pub url: String,
    pub comment: String,
    pub is_clrmamepro: bool,
    pub clrmamepro_category: String,
    pub search_name: String,
    pub numbered: bool,
}

impl Default for Dat {
    fn default() -> Dat {
        let unknown = || "Unknown".to_owned();
        Dat {
            name: unknown(),
            description: unknown(),
            version: unknown(),
            author: unknown(),
            homepage: unknown(),
            url: unknown(),
            comment: unknown(),
            is_clrmamepro: false,
            clrmamepro_category: String::new(),
            search_name: String::new(),
            numbered: false,
        }
    }
}

fn incompatible(path: &Path) -> Error {
    Error::Builtin(format!("{} isn't a compatible DAT file", path.display()))
}

/// `replace_quotes`.
fn replace_quotes(s: &str) -> String {
    match s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) if !s.contains('\n') => inner.to_owned(),
        _ => s.to_owned(),
    }
}

/// `format_system_name`.
pub(crate) fn format_system_name(name: &str, s: &Settings, dat: &Dat) -> String {
    let remove = Re::of(&format!(" \\(({})\\)", s.dat_file_tags.join("|")));
    let mut search_name = remove.sub("", name);
    let (url, author) = (&dat.url, &dat.author);
    if url.contains("no-intro") {
        search_name = format!("{search_name} (No-Intro)");
    } else if url.contains("MAMERedump") || author.contains("MAME Redump") {
        search_name = format!("{search_name} (MAME Redump)");
    } else if url.contains("redump")
        || dat.comment.contains("Redump.org ISOs converted")
        || author.contains("Redump")
        || author.contains("redump")
    {
        search_name = format!("{search_name} (Redump)");
    } else if dat.homepage.contains("TOSEC") || url.contains("tosecdev.org") {
        search_name = format!("{search_name} (TOSEC)");
    }
    let nkit = |n: &str| {
        n.contains("NKit GCZ")
            || n.contains("NKit ISO")
            || n.contains("NKit RVZ")
            || n.contains("NASOS")
    };
    if search_name.contains("GameCube") && nkit(&search_name) {
        search_name = "Nintendo - GameCube (Redump)".into();
    }
    if search_name.contains("Wii") && nkit(&search_name) {
        search_name = "Nintendo - Wii (Redump)".into();
    }
    if search_name.contains("Wii U") && search_name.contains("WUX") {
        search_name = "Nintendo - Wii U (Redump)".into();
    }
    search_name
}

/// Decodes the entities an HTML parser decodes in header text.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(end) = after.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `get_dat_header` (with `get_logiqx_header`).
pub(crate) fn read_header(path: &Path, text: &str, s: &Settings) -> Result<Dat, Error> {
    let mut dat = Dat::default();
    let first_line = text.split('\n').next().unwrap_or("");
    if py::strip(first_line) == "clrmamepro (" {
        dat.is_clrmamepro = true;
        let header = clrmamepro_header(text);
        for detail in header.iter().skip(1).take(header.len().saturating_sub(2)) {
            for (key, field) in [
                ("author ", &mut dat.author),
                ("category ", &mut dat.clrmamepro_category),
                ("description ", &mut dat.description),
                ("name ", &mut dat.name),
                ("version ", &mut dat.version),
            ] {
                if let Some(value) = detail.strip_prefix(key) {
                    *field = replace_quotes(value);
                }
            }
        }
    } else {
        if !(first_line.contains("<?xml version")
            || first_line.contains("<!DOCTYPE datafile")
            || first_line.contains("<datafile>\""))
        {
            return Err(incompatible(path));
        }
        // Retool scans for `</header>` forever when there isn't one.
        let end = text.find("</header>").ok_or_else(|| incompatible(path))?;
        let mut header = &text[..end + 9];
        let start = Re::of("\\s*?<header");
        let finish = Re::of("\n*\\s*?</header");
        if start.search(header) && finish.search(header) {
            let a = start.span(header).map_or(0, |(a, _)| a);
            let b = finish.span(header).map_or(header.len(), |(b, _)| b);
            header = if a <= b { &header[a..b] } else { "" };
        }
        let lines: Vec<&str> = header.split('\n').filter(|l| *l != "\r").collect();
        let Some(position) = lines.iter().position(|l| l.contains("<header>")) else {
            return Err(Error::Builtin(format!(
                "{}: the DAT's header has no <header> line",
                path.display()
            )));
        };
        let element =
            Re::of(r"(?s)^\s*<([A-Za-z_][\w.-]*)(?:\s[^>]*)?(?:/>|>(.*?)(?:</\1\s*>.*)?)\s*$");
        for line in &lines[position + 1..] {
            let line = line.replace('\r', "\n");
            let Some(caps) = element.captures_all(py::strip(&line)) else {
                continue;
            };
            let tag = caps[1].clone().unwrap_or_default().to_lowercase();
            let text = caps.get(2).cloned().flatten().unwrap_or_default();
            let text = text.split('<').next().unwrap_or("").to_owned();
            let value = if text.is_empty() {
                "Unknown".to_owned()
            } else {
                unescape(&text)
            };
            match tag.as_str() {
                "name" => dat.name = value,
                "description" => dat.description = value,
                "version" => dat.version = value,
                "author" => dat.author = value,
                "homepage" => dat.homepage = value,
                "url" => dat.url = value,
                "comment" => dat.comment = value,
                _ => {}
            }
        }
    }
    dat.name = Re::of(" \\(Retool.*?\\)")
        .sub("", &dat.name)
        .replace("&amp;", "&");
    dat.name = titletools::replace_invalid_characters(&dat.name, true);
    dat.version = titletools::replace_invalid_characters(&dat.version, true);
    let reserved = Re::of("^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])$");
    if reserved.search(&dat.name) {
        dat.name = "Unknown".into();
    }
    if reserved.search(&dat.version) {
        dat.version = "Unknown".into();
    }
    dat.search_name = format_system_name(&dat.name, s, &dat);
    Ok(dat)
}

fn clrmamepro_header(text: &str) -> Vec<String> {
    let mut header = Vec::new();
    for line in text.split_inclusive('\n') {
        header.push(py::strip(line).to_owned());
        if header.len() > 1 && header.last().is_some_and(|l| l == ")") {
            return header;
        }
    }
    Vec::new()
}

/// `get_clrmamepro_titles`.
pub(crate) fn clrmamepro_titles(text: &str, dat: &Dat) -> Vec<TitleData> {
    let mut titles = Vec::new();
    let mut header_complete = false;
    let mut section: Vec<String> = Vec::new();
    let name_quoted = Re::of("name\\s\".*?\"\\s");
    let name_bare = Re::of("name\\s.*?\\s");
    let field = |key: &str| Re::of(&format!("{key}\\s\"?.*?\"?\\s"));
    let fields = [
        ("size", field("size")),
        ("crc", field("crc")),
        ("md5", field("md5")),
        ("sha1", field("sha1")),
        ("sha256", field("sha256")),
    ];
    let mut lines_seen = 0usize;
    for line in text.split_inclusive('\n') {
        let line = py::strip(line);
        if !header_complete {
            lines_seen += 1;
            if lines_seen > 1 && line == ")" {
                header_complete = true;
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        section.push(line.to_owned());
        let opens = section[0].starts_with("game (") || section[0].starts_with("set (");
        if !(opens && section.len() > 1 && line == ")") {
            if !opens {
                section.clear();
            }
            continue;
        }
        let mut title = TitleData::default();
        if !dat.clrmamepro_category.is_empty() {
            title.categories.add(dat.clrmamepro_category.clone());
        }
        for detail in &section[1..section.len() - 1] {
            if let Some(v) = detail.strip_prefix("name ") {
                title.name = replace_quotes(v);
            } else if let Some(v) = detail.strip_prefix("description ") {
                title.description = replace_quotes(v);
            } else if detail.starts_with("rom ") || detail.starts_with("disk ") {
                let kind = if detail.starts_with("disk ") {
                    "disk"
                } else {
                    "rom"
                };
                let quoted = name_quoted.find(detail);
                let name = if !quoted.is_empty() {
                    replace_quotes(&py::strip(&quoted).replace("name ", ""))
                } else {
                    py::strip(&name_bare.find(detail)).replace("name ", "")
                };
                let mut rom = Rom {
                    name,
                    kind: kind.into(),
                    ..Rom::default()
                };
                for (key, pattern) in &fields {
                    let value = py::strip(&replace_quotes(&pattern.find(detail)))
                        .replace(&format!("{key} "), "");
                    match *key {
                        "size" => rom.size = value,
                        "crc" => rom.crc = value,
                        "md5" => rom.md5 = value,
                        "sha1" => rom.sha1 = value,
                        _ => rom.sha256 = value,
                    }
                }
                title.files.push(rom);
            }
        }
        if !title.files.is_empty() && !title.name.is_empty() {
            titles.push(title);
        }
        section.clear();
    }
    titles
}

/// `get_logiqx_titles` for `game` and `machine` elements, in document order.
pub(crate) fn logiqx_titles(text: &str) -> Vec<TitleData> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    let mut titles = Vec::new();
    // The title being read, and the depth of its element.
    let mut current: Option<(TitleData, usize, bool)> = None;
    let mut depth = 0usize;
    // The text being collected for a description or category directly under the title.
    let mut collecting: Option<(String, String)> = None;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => break,
        };
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(event, Event::Empty(_));
                let tag = e.name().0.to_owned();
                let attribute = |key: &str| -> String {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.0 == key)
                        .map(|a| unescape(&a.value))
                        .unwrap_or_default()
                };
                match &mut current {
                    None if tag == "game" || tag == "machine" => {
                        let title = TitleData {
                            name: attribute("name"),
                            ..TitleData::default()
                        };
                        if is_empty {
                            // A title without files is dropped.
                        } else {
                            current = Some((title, depth + 1, false));
                        }
                    }
                    Some((title, title_depth, has_description)) if depth == *title_depth => {
                        match tag.as_str() {
                            "rom" | "disk" => {
                                let rom = Rom {
                                    name: attribute("name"),
                                    size: attribute("size"),
                                    crc: attribute("crc"),
                                    md5: attribute("md5"),
                                    sha1: attribute("sha1"),
                                    sha256: attribute("sha256"),
                                    kind: tag.clone(),
                                    mia: attribute("mia"),
                                    header: attribute("header"),
                                };
                                if !rom.name.is_empty()
                                    && (!rom.crc.is_empty()
                                        || !rom.md5.is_empty()
                                        || !rom.sha1.is_empty()
                                        || !rom.sha256.is_empty())
                                {
                                    title.files.push(rom);
                                }
                            }
                            "description" if !*has_description => {
                                *has_description = true;
                                if is_empty {
                                    title.description = "None".into();
                                } else {
                                    collecting = Some(("description".into(), String::new()));
                                }
                            }
                            "category" => {
                                if is_empty {
                                    title.categories.add("None".into());
                                } else {
                                    collecting = Some(("category".into(), String::new()));
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                if !is_empty {
                    depth += 1;
                }
            }
            Event::Text(t) => {
                if let Some((_, text)) = &mut collecting {
                    let raw = {
                        let t: &str = t.as_ref();
                        t.to_owned()
                    };
                    text.push_str(&unescape(&raw));
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, text)) = &mut collecting {
                    let raw = {
                        let r: &str = r.as_ref();
                        r.to_owned()
                    };
                    text.push_str(&unescape(&format!("&{raw};")));
                }
            }
            Event::CData(t) => {
                if let Some((_, text)) = &mut collecting {
                    {
                        let t: &str = t.as_ref();
                        text.push_str(t);
                    }
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if let Some((kind, text)) = collecting.take() {
                    if let Some((title, _, _)) = &mut current {
                        let text = if text.is_empty() { "None".into() } else { text };
                        if kind == "description" {
                            title.description = text;
                        } else {
                            title.categories.add(text);
                        }
                    }
                } else if let Some((_, title_depth, _)) = &current
                    && depth + 1 == *title_depth
                {
                    let (title, _, _) = current.take().expect("current");
                    if !title.name.is_empty() && !title.files.is_empty() {
                        titles.push(title);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    titles
}
