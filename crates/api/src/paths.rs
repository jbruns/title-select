//! Path Templates: the tokens that propose the path of a Source Set or Library Folder at registration, and the
//! platform table that gives the `{platform}` token its value.
//!
//! A template only proposes a path; a registered path never moves when a template changes. Shared by the Service,
//! the CLI and the workbench, which renders its live preview with it.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The template a Source Set's path is proposed from when the configuration names none.
pub const DEFAULT_SOURCE_SET_TEMPLATE: &str = "{provider}/{dat}";
/// The template a Library Folder's path is proposed from when the configuration names none.
pub const DEFAULT_LIBRARY_FOLDER_TEMPLATE: &str = "{platform}";

/// The platform table, as committed: see the file's header for its sources.
pub const PLATFORMS_TSV: &str = include_str!("../data/platforms.tsv");

/// Dump-format qualifiers a DAT header name may end with, such as `Nintendo - Nintendo 64 (BigEndian)`. They don't
/// change the system, so a platform lookup ignores them.
const FORMAT_QUALIFIERS: &[&str] = &[
    "Headered",
    "Headerless",
    "BigEndian",
    "ByteSwapped",
    "LittleEndian",
    "Decrypted",
    "Encrypted",
    "Parent-Clone",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    /// The DAT's provider, such as `No-Intro`.
    Provider,
    /// The DAT's header name, such as `Atari - Atari 2600`.
    Dat,
    /// The front-end folder name of the DAT's system, such as `atari2600`, from the platform table.
    Platform,
}

impl Token {
    pub const ALL: [Token; 3] = [Token::Provider, Token::Dat, Token::Platform];

    pub fn as_str(self) -> &'static str {
        match self {
            Token::Provider => "provider",
            Token::Dat => "dat",
            Token::Platform => "platform",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Token(Token),
}

/// A parsed Path Template: literal text and `{token}`s. Literal text and token values are kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate {
    text: String,
    parts: Vec<Part>,
}

/// Why a path is not a valid relative path inside a Root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    Absolute,
    ParentDir,
    /// A `\`, which Windows reads as a separator: refused everywhere, so a path means the same on every Host.
    Backslash,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathError::Empty => "must name a folder inside its root",
            PathError::Absolute => "must be relative to its root",
            PathError::ParentDir => "must not contain `..`",
            PathError::Backslash => "must separate folders with /, not \\",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// A `{name}` that is not one of the tokens.
    UnknownToken(String),
    /// A `{` with no `}`, or a `}` with no `{`.
    UnmatchedBrace,
    /// The template uses `{platform}`, and the platform table has no folder name for this DAT.
    NoPlatform { provider: String, dat: String },
    /// The rendered path is not a valid relative path.
    Path { path: String, error: PathError },
    /// The template's literal text makes every path it could render invalid, whatever the DAT.
    Unusable(PathError),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::UnknownToken(name) => write!(
                f,
                "{{{name}}} is not a template token; use {{provider}}, {{dat}} or {{platform}}"
            ),
            TemplateError::UnmatchedBrace => {
                f.write_str("the template has a brace with no partner; tokens look like {dat}")
            }
            TemplateError::NoPlatform { provider, dat } => write!(
                f,
                "no platform folder name is known for {provider}/{dat}, so {{platform}} has no value; type the path"
            ),
            TemplateError::Path { path, error } => write!(f, "{path:?}: the path {error}"),
            TemplateError::Unusable(error) => write!(f, "the template {error}"),
        }
    }
}

impl std::error::Error for TemplateError {}
impl std::error::Error for PathError {}

impl PathTemplate {
    pub fn parse(text: &str) -> Result<PathTemplate, TemplateError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        while let Some(i) = rest.find(['{', '}']) {
            literal.push_str(&rest[..i]);
            if rest[i..].starts_with('}') {
                return Err(TemplateError::UnmatchedBrace);
            }
            let after = &rest[i + 1..];
            let end = after.find('}').ok_or(TemplateError::UnmatchedBrace)?;
            let name = &after[..end];
            if name.contains('{') {
                return Err(TemplateError::UnmatchedBrace);
            }
            let token = Token::ALL
                .into_iter()
                .find(|t| t.as_str() == name)
                .ok_or_else(|| TemplateError::UnknownToken(name.to_owned()))?;
            if !literal.is_empty() {
                parts.push(Part::Literal(std::mem::take(&mut literal)));
            }
            parts.push(Part::Token(token));
            rest = &after[end + 1..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        let template = PathTemplate {
            text: text.to_owned(),
            parts,
        };
        template.check_literals()?;
        Ok(template)
    }

    /// Refuses a template whose literal text alone makes every rendered path invalid: empty, absolute, a `..`
    /// segment, a backslash or a drive prefix. Each token is stood in for by a plain folder name, so only the
    /// literal text can fail; token values are checked when the template renders.
    fn check_literals(&self) -> Result<(), TemplateError> {
        let stand_in = TokenValues {
            provider: "provider".into(),
            dat: "dat".into(),
            platform: Some("platform".into()),
        };
        match self.render(&stand_in) {
            Err(TemplateError::Path { error, .. }) => Err(TemplateError::Unusable(error)),
            _ => Ok(()),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn uses(&self, token: Token) -> bool {
        self.parts.contains(&Part::Token(token))
    }

    /// The path this template proposes for a DAT, checked to be a valid relative path and returned with `/`
    /// separators. Whether it stays inside its Root once symlinks are followed is checked at registration.
    pub fn render(&self, values: &TokenValues) -> Result<String, TemplateError> {
        let mut path = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => path.push_str(text),
                Part::Token(Token::Provider) => path.push_str(&values.provider),
                Part::Token(Token::Dat) => path.push_str(&values.dat),
                Part::Token(Token::Platform) => match &values.platform {
                    Some(platform) => path.push_str(platform),
                    None => {
                        return Err(TemplateError::NoPlatform {
                            provider: values.provider.clone(),
                            dat: values.dat.clone(),
                        });
                    }
                },
            }
        }
        relative_path(&path).map_err(|error| TemplateError::Path { path, error })
    }
}

impl FromStr for PathTemplate {
    type Err = TemplateError;

    fn from_str(text: &str) -> Result<PathTemplate, TemplateError> {
        PathTemplate::parse(text)
    }
}

impl fmt::Display for PathTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for PathTemplate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for PathTemplate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<PathTemplate, D::Error> {
        let text = String::deserialize(d)?;
        PathTemplate::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The values a template's tokens take for one DAT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenValues {
    pub provider: String,
    pub dat: String,
    /// `None` when the platform table has no folder name for the DAT.
    pub platform: Option<String>,
}

impl TokenValues {
    /// The values for the DAT `provider`/`dat`, with `{platform}` from the platform table.
    pub fn for_dat(provider: &str, dat: &str) -> TokenValues {
        TokenValues {
            provider: provider.to_owned(),
            dat: dat.to_owned(),
            platform: platform(provider, dat).map(str::to_owned),
        }
    }
}

/// One line of the platform table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub provider: &'static str,
    pub dat: &'static str,
    /// The front-end folder name, such as `atari2600`.
    pub folder: &'static str,
}

/// Every line of the platform table, in file order.
pub fn platforms() -> &'static [Platform] {
    static TABLE: LazyLock<Vec<Platform>> = LazyLock::new(|| {
        PLATFORMS_TSV
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|line| {
                let mut fields = line.split('\t');
                match (fields.next(), fields.next(), fields.next(), fields.next()) {
                    (Some(provider), Some(dat), Some(folder), None) => Platform {
                        provider,
                        dat,
                        folder,
                    },
                    _ => panic!("platforms.tsv: {line:?} is not provider, DAT name and folder"),
                }
            })
            .collect()
    });
    &TABLE
}

/// The front-end folder name of the DAT `provider`/`dat`, ignoring dump-format qualifiers at the end of its name.
/// `None` when the platform table has no entry for it.
pub fn platform(provider: &str, dat: &str) -> Option<&'static str> {
    let mut name = dat;
    loop {
        if let Some(p) = platforms()
            .iter()
            .find(|p| p.provider == provider && p.dat == name)
        {
            return Some(p.folder);
        }
        name = FORMAT_QUALIFIERS.iter().find_map(|q| {
            name.strip_suffix(')')
                .and_then(|n| n.strip_suffix(q))
                .and_then(|n| n.strip_suffix(" ("))
        })?;
    }
}

/// Checks that `path` is relative, not empty and free of `..`, and returns it with `/` separators, without empty
/// or `.` components. Every other character is kept verbatim.
///
/// The check is the same on every Host: a backslash is refused, and so is a Windows drive prefix such as `C:`,
/// so neither `C:\root`, `\\server\share` nor `a\..\b` can pass where Windows would read them as a path.
pub fn relative_path(path: &str) -> Result<String, PathError> {
    if path.starts_with('/') || has_drive_prefix(path) {
        return Err(PathError::Absolute);
    }
    if path.contains('\\') {
        return Err(PathError::Backslash);
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(PathError::ParentDir),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(PathError::Empty);
    }
    let path = parts.join("/");
    // Dropping `.` components can bring a drive prefix to the front, as in `./C:/folder`.
    if has_drive_prefix(&path) {
        return Err(PathError::Absolute);
    }
    Ok(path)
}

/// Whether `path` starts with a Windows drive prefix: a letter and a colon, such as `C:` or `c:folder`.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn render(template: &str, provider: &str, dat: &str) -> Result<String, TemplateError> {
        PathTemplate::parse(template)
            .unwrap()
            .render(&TokenValues::for_dat(provider, dat))
    }

    #[test]
    fn each_token_takes_its_value_from_the_dat() {
        let values = TokenValues::for_dat("No-Intro", "Atari - Atari 2600");
        let one = |t: &str| PathTemplate::parse(t).unwrap().render(&values).unwrap();

        assert_eq!(one("{provider}"), "No-Intro");
        assert_eq!(one("{dat}"), "Atari - Atari 2600");
        assert_eq!(one("{platform}"), "atari2600");
        assert_eq!(
            one("roms/{platform} ({provider}) {dat}"),
            "roms/atari2600 (No-Intro) Atari - Atari 2600"
        );
    }

    #[test]
    fn the_defaults_for_a_no_intro_and_a_redump_dat() {
        for (provider, dat, set, folder) in [
            (
                "No-Intro",
                "Atari - Atari 2600",
                "No-Intro/Atari - Atari 2600",
                "atari2600",
            ),
            (
                "Redump",
                "Sony - PlayStation",
                "Redump/Sony - PlayStation",
                "psx",
            ),
        ] {
            assert_eq!(
                render(DEFAULT_SOURCE_SET_TEMPLATE, provider, dat).unwrap(),
                set
            );
            assert_eq!(
                render(DEFAULT_LIBRARY_FOLDER_TEMPLATE, provider, dat).unwrap(),
                folder
            );
        }
    }

    #[test]
    fn a_system_with_no_platform_has_no_platform_token_but_the_other_tokens_work() {
        let unmapped = render("{platform}", "No-Intro", "RCA - Studio II");

        assert_eq!(
            unmapped,
            Err(TemplateError::NoPlatform {
                provider: "No-Intro".into(),
                dat: "RCA - Studio II".into()
            })
        );
        assert!(unmapped.unwrap_err().to_string().contains("type the path"));
        assert_eq!(
            render(DEFAULT_SOURCE_SET_TEMPLATE, "No-Intro", "RCA - Studio II").unwrap(),
            "No-Intro/RCA - Studio II"
        );
        assert_eq!(
            platform("Redump", "Atari - Atari 2600"),
            None,
            "the provider is part of the key"
        );
    }

    #[test]
    fn a_dump_format_qualifier_does_not_change_the_platform() {
        assert_eq!(
            platform(
                "No-Intro",
                "Nintendo - Nintendo Entertainment System (Headered)"
            ),
            Some("nes")
        );
        assert_eq!(
            platform("No-Intro", "Nintendo - Nintendo 64 (BigEndian)"),
            Some("n64")
        );
        assert_eq!(
            platform("No-Intro", "Nintendo - Game Boy Advance (Multiboot)"),
            None,
            "only dump-format qualifiers are ignored"
        );
    }

    #[test]
    fn dot_dot_absolute_and_empty_results_are_rejected() {
        for (dat, error) in [
            ("..", PathError::ParentDir),
            ("a/../..", PathError::ParentDir),
            ("/etc", PathError::Absolute),
            ("", PathError::Empty),
            ("./", PathError::Empty),
        ] {
            let rendered = render("{dat}", "No-Intro", dat);
            assert!(
                matches!(&rendered, Err(TemplateError::Path { error: e, .. }) if *e == error),
                "{dat:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn windows_drive_unc_and_backslash_paths_are_rejected_on_every_host() {
        for (path, error) in [
            (r"C:\root\folder", PathError::Absolute),
            ("C:/root/folder", PathError::Absolute),
            ("c:folder", PathError::Absolute),
            ("./C:/folder", PathError::Absolute),
            (".//./c:folder", PathError::Absolute),
            (r"\\server\share\folder", PathError::Backslash),
            ("//server/share/folder", PathError::Absolute),
            (r"\\?\C:\folder", PathError::Backslash),
            (r"a\..\b", PathError::Backslash),
            (r"\folder", PathError::Backslash),
            (r"No-Intro\Atari", PathError::Backslash),
        ] {
            assert_eq!(relative_path(path), Err(error), "{path}");
        }
        assert_eq!(
            render("{dat}", "No-Intro", r"C:\x").unwrap_err(),
            TemplateError::Path {
                path: r"C:\x".into(),
                error: PathError::Absolute
            }
        );
        assert_eq!(relative_path("No-Intro/a:b").unwrap(), "No-Intro/a:b");
        assert_eq!(relative_path("./No-Intro/C:").unwrap(), "No-Intro/C:");
    }

    #[test]
    fn a_template_that_no_dat_could_make_valid_is_refused_when_parsed() {
        for (template, error) in [
            ("", PathError::Empty),
            ("./", PathError::Empty),
            ("../{dat}", PathError::ParentDir),
            ("{provider}/../../{dat}", PathError::ParentDir),
            ("/{dat}", PathError::Absolute),
            ("C:/{dat}", PathError::Absolute),
            ("./C:/{dat}", PathError::Absolute),
            (r"{provider}\{dat}", PathError::Backslash),
        ] {
            assert_eq!(
                PathTemplate::parse(template),
                Err(TemplateError::Unusable(error)),
                "{template:?}"
            );
        }
        assert_eq!(
            PathTemplate::parse("../{dat}").unwrap_err().to_string(),
            "the template must not contain `..`"
        );
        for template in [
            "{dat}",
            "{dat}..",
            "..{dat}",
            "{provider}:{dat}",
            "{dat}/.",
            "./{provider}/C:/{dat}",
        ] {
            assert!(PathTemplate::parse(template).is_ok(), "{template:?}");
        }
    }

    #[test]
    fn characters_are_kept_verbatim_and_empty_or_dot_components_dropped() {
        assert_eq!(
            render(
                "./{provider}//{dat}/",
                "No-Intro",
                "Nintendo - Game & Watch: \"Ö\""
            )
            .unwrap(),
            "No-Intro/Nintendo - Game & Watch: \"Ö\""
        );
        assert_eq!(render("{dat}", "No-Intro", " a b ").unwrap(), " a b ");
    }

    #[test]
    fn an_override_template_replaces_the_default() {
        assert_eq!(
            render("Sets/{platform}/{dat}", "No-Intro", "Atari - Atari 2600").unwrap(),
            "Sets/atari2600/Atari - Atari 2600"
        );
    }

    #[test]
    fn an_unknown_token_or_an_unmatched_brace_is_refused() {
        assert_eq!(
            PathTemplate::parse("{system}"),
            Err(TemplateError::UnknownToken("system".into()))
        );
        for text in ["{dat", "dat}", "{{dat}}", "{da{t}"] {
            assert!(PathTemplate::parse(text).is_err(), "{text}");
        }
        assert!(
            PathTemplate::parse("{platform}")
                .unwrap()
                .uses(Token::Platform)
        );
        assert!(!PathTemplate::parse("{dat}").unwrap().uses(Token::Platform));
    }

    #[test]
    fn the_platform_table_cites_its_sources_and_every_entry_is_unique() {
        assert!(PLATFORMS_TSV.contains("es_systems.xml"));
        assert!(PLATFORMS_TSV.contains("MIT"));
        assert!(PLATFORMS_TSV.contains("docs.romm.app"));
        let table = platforms();
        assert!(table.len() > 50, "{} entries", table.len());
        let mut keys = BTreeSet::new();
        for p in table {
            assert!(keys.insert((p.provider, p.dat)), "{p:?} is listed twice");
            assert!(
                ["No-Intro", "Redump"].contains(&p.provider),
                "{p:?}: an unexpected provider"
            );
            assert!(!p.dat.is_empty() && p.dat.trim() == p.dat, "{p:?}");
            assert!(
                !p.folder.is_empty()
                    && p.folder
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{p:?}: a folder name is lowercase letters, digits and dashes"
            );
            assert_eq!(
                platform(p.provider, p.dat),
                Some(p.folder),
                "{p:?}: no qualifier hides it"
            );
        }
    }
}
