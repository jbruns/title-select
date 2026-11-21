//! The Selection engine (ADR 0015): selects from one DAT Revision with a Selection Profile, and explains its
//! Selection from the trace.
//!
//! The engine (`native`) is a port of Retool to Rust. It reads the pinned clone lists, metadata, MIA and
//! RetroAchievements files that `tools/clone-lists/fetch` unpacks (`.clone-lists/`, or `/opt/clone-lists` in the
//! image), and never reaches the network.
//!
//! An Arcade DAT, a MAME ListXML, goes to the arcade selector (`arcade`, ADR 0027) instead, and a MAME software list
//! is refused: `Engine::run` chooses by the DAT's kind. The arcade selector reads the Support Files the caller gives,
//! and the pinned curated list that `tools/curated-list/fetch` unpacks (`.curated-list/`, or `/opt/curated-list` in
//! the image) when its settings ask for it.

pub mod arcade;
pub mod choices;
mod config;
mod native;
mod patterns;
mod trace;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use title_select_api::dat::DatFormat;
use title_select_api::selection::{
    ARCADE_COVERAGE_REFUSAL, SELECTION_TRACE_SCHEMA, SOFTWARE_LIST_REFUSAL, Selection,
    SelectionPins, SelectionProfile,
};

pub use trace::stable_trace;

/// The environment variable that points at the clone lists, for `Engine::locate`.
pub const CLONE_LISTS_ENV: &str = "TITLE_SELECT_CLONE_LISTS";
/// Where the image installs the clone lists.
pub const DEFAULT_CLONE_LISTS: &str = "/opt/clone-lists";
/// The longest a Selection may run unless the caller says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

const PINS: &str = include_str!("../../../third_party/clone-lists/pins.toml");

/// The environment variable that points at the curated list, for `Engine::locate`.
pub const CURATED_LIST_ENV: &str = "TITLE_SELECT_CURATED_LIST";
/// Where the image installs the curated list.
pub const DEFAULT_CURATED_LIST: &str = "/opt/curated-list";

const CURATED_PINS: &str = include_str!("../../../third_party/curated-list/pins.toml");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the clone lists: {0} (fetch them with tools/clone-lists/fetch)")]
    Install(String),
    #[error("the curated list: {0} (fetch it with tools/curated-list/fetch)")]
    CuratedList(String),
    #[error("selection profile: {0}")]
    Profile(String),
    /// Regular expressions in the Profile that the engine cannot read, one message each.
    #[error("{}", .0.join("\n"))]
    Patterns(Vec<String>),
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
    #[error("selection did not finish within {} s", .0.as_secs())]
    Timeout(Duration),
    #[error("selection was cancelled")]
    Cancelled,
    #[error("the selection engine's trace: {0}")]
    Trace(String),
    #[error("the selection engine: {0}")]
    Builtin(String),
    /// Something the engine doesn't support, such as a regular expression it can't read as Python would.
    #[error("the selection engine doesn't support {0}")]
    Unsupported(String),
    /// A DAT the engine can't read.
    #[error("the DAT: {0}")]
    Dat(String),
    /// A Selection the engine won't make, such as of a software list, and why, as the Operator reads it.
    #[error("{0}")]
    Refused(String),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Error {
    let context = context.into();
    move |source| Error::Io { context, source }
}

/// How to run one Selection.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub timeout: Duration,
    /// Set to stop the run: it fails with `Error::Cancelled`.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The Source Set's Coverage, as engine region names: Games it doesn't cover are excluded before the engine
    /// chooses (ADR 0021). `None` covers every Game.
    pub coverage: Option<Vec<String>>,
    /// The Support Files the arcade selector reads, at most one of each kind, which their headers name; the Retool
    /// port reads none.
    pub support_files: Vec<PathBuf>,
}

impl RunOptions {
    pub fn new() -> RunOptions {
        RunOptions {
            timeout: DEFAULT_TIMEOUT,
            cancel: None,
            coverage: None,
            support_files: Vec::new(),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions::new()
    }
}

/// A Selection and the trace it was derived from.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub selection: Selection,
    /// The engine's trace, as JSON Lines.
    pub trace: String,
    /// What the engine warned about on a run that succeeded, one line each.
    pub warnings: Vec<String>,
}

/// The engine with a verified copy of the clone lists.
#[derive(Debug, Clone)]
pub struct Engine {
    /// Holds `config/internal-config.json`.
    root: PathBuf,
    /// Holds the clone lists, metadata, MIAs and RetroAchievements.
    data: PathBuf,
    /// Holds the curated list's `ROMSET` and the `commit` it was fetched at; read only when a run asks for it.
    curated_list: PathBuf,
    pins: SelectionPins,
}

impl Engine {
    /// The clone lists at `explicit`, else at `$TITLE_SELECT_CLONE_LISTS`, else at `/opt/clone-lists`, and the
    /// curated list at `$TITLE_SELECT_CURATED_LIST`, else at `/opt/curated-list`.
    pub fn locate(explicit: Option<&Path>) -> Result<Self, Error> {
        let root = match explicit {
            Some(path) => path.to_path_buf(),
            None => std::env::var_os(CLONE_LISTS_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CLONE_LISTS)),
        };
        let curated = std::env::var_os(CURATED_LIST_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CURATED_LIST));
        Ok(Self::open(&root)?.with_curated_list(&curated))
    }

    /// The clone lists at `root`, if they are the commit this binary was built with.
    pub fn open(root: &Path) -> Result<Self, Error> {
        let expected = expected_pins()?;
        let path = root.join("commit");
        let commit = std::fs::read_to_string(&path)
            .map_err(|e| Error::Install(format!("{}: {e}", path.display())))?;
        if commit.trim() != expected.clone_list_commit {
            return Err(Error::Install(format!(
                "{} holds commit {}, not the commit third_party/clone-lists/pins.toml pins",
                root.display(),
                commit.trim()
            )));
        }
        Self::unverified(root)
    }

    /// The clone lists and `config/internal-config.json` at `root`, whatever their commit. Tests use it to run
    /// Retool's own fixtures.
    pub fn unverified(root: &Path) -> Result<Self, Error> {
        let root = std::path::absolute(root).map_err(io(root.display().to_string()))?;
        let required = root.join("config").join("internal-config.json");
        if !required.is_file() {
            return Err(Error::Install(format!(
                "{} has no config/internal-config.json",
                root.display()
            )));
        }
        Ok(Engine {
            data: root.clone(),
            root,
            curated_list: PathBuf::from(DEFAULT_CURATED_LIST),
            pins: expected_pins()?,
        })
    }

    /// Reads the curated list from `dir`, which holds its `ROMSET` and the `commit` it was fetched at.
    pub fn with_curated_list(mut self, dir: &Path) -> Self {
        self.curated_list = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        self
    }

    /// The curated list's pin and shortnames, if it is the commit this binary was built with.
    fn read_curated_list(&self) -> Result<CuratedList, Error> {
        let pinned = curated_list()?;
        let read = |name: &str| {
            let path = self.curated_list.join(name);
            std::fs::read_to_string(&path)
                .map_err(|e| Error::CuratedList(format!("{}: {e}", path.display())))
        };
        let commit = read("commit")?;
        if commit.trim() != pinned.commit {
            return Err(Error::CuratedList(format!(
                "{} holds commit {}, not the commit third_party/curated-list/pins.toml pins",
                self.curated_list.display(),
                commit.trim()
            )));
        }
        let names = read("ROMSET")?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_owned)
            .collect();
        Ok(CuratedList { pin: pinned, names })
    }

    /// Reads clone lists, metadata, MIAs and RetroAchievements from `dir` instead of the pinned ones. Tests use
    /// it to run Retool's own fixtures.
    pub fn with_data_dir(mut self, dir: &Path) -> Self {
        self.data = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        self
    }

    pub fn pins(&self) -> &SelectionPins {
        &self.pins
    }

    pub fn select(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
    ) -> Result<Selection, Error> {
        self.run(dat, profile, options)
            .map(|outcome| outcome.selection)
    }

    fn internal_config(&self) -> Result<Vec<u8>, Error> {
        let path = self.root.join("config").join("internal-config.json");
        std::fs::read(&path).map_err(io(path.display().to_string()))
    }

    fn vocabulary(&self) -> Result<config::Vocabulary, Error> {
        config::Vocabulary::parse(&self.internal_config()?)
    }

    /// The region, language and video names a Profile may use.
    pub fn names(&self) -> Result<Names, Error> {
        let vocabulary = self.vocabulary()?;
        Ok(Names {
            default_region_order: vocabulary.region_order(&[]).to_vec(),
            default_video_order: vocabulary.video_order(&[]).to_vec(),
            regions: vocabulary.regions,
            languages: vocabulary.languages.into_iter().collect(),
            videos: vocabulary.videos,
        })
    }

    /// Reads Games' regions from their names, as a Selection does.
    pub fn regions(&self) -> Result<Regions, Error> {
        Ok(Regions(native::GameRegions::new(&self.internal_config()?)?))
    }

    /// Checks that the engine can read `profile`: every name is known and every override and post filter is
    /// well formed.
    pub fn check(&self, profile: &SelectionProfile) -> Result<(), Error> {
        self.vocabulary()?.check(profile)?;
        patterns::check(profile)
    }

    /// The system each of `dats` is for, as its header names it and as a Profile files its system settings and
    /// choices under, or `None` for a DAT the engine can't read or select. Every ListXML is `MAME`.
    pub fn systems(&self, dats: &[&Path]) -> Result<Vec<Option<String>>, Error> {
        let internal = self.internal_config()?;
        Ok(dats
            .iter()
            .map(|dat| match format_of(dat) {
                Ok(Some(DatFormat::Listxml)) => Some(title_select_api::dat::MAME.to_owned()),
                Ok(Some(DatFormat::SoftwareList)) => None,
                _ => native::system_name(dat, &internal),
            })
            .collect())
    }

    /// Selects from `dat` with `profile`, applies the Profile's choices for the DAT's system, and keeps the trace. A
    /// ListXML goes to the arcade selector with its default settings; a software list is refused.
    pub fn run(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
    ) -> Result<Outcome, Error> {
        match format_of(dat)? {
            Some(DatFormat::Listxml) => {
                return self.run_arcade(dat, profile, &arcade::ArcadeSettings::default(), options);
            }
            Some(DatFormat::SoftwareList) => {
                return Err(Error::Refused(SOFTWARE_LIST_REFUSAL.to_owned()));
            }
            Some(DatFormat::Logiqx) | None => {}
        }
        self.vocabulary()?.check(profile)?;
        patterns::check(profile)?;
        let dat = std::path::absolute(dat).map_err(io(dat.display().to_string()))?;
        let dat_sha256 = sha256_file(&dat)?;
        if options.cancelled() {
            return Err(Error::Cancelled);
        }
        let limits = native::Limits {
            cancel: options.cancel.clone(),
            deadline: Some((Instant::now() + options.timeout, options.timeout)),
        };
        let ran = native::run(
            &dat,
            profile,
            options.coverage.as_deref(),
            &self.internal_config()?,
            &self.data,
            &limits,
        )?;
        self.outcome(
            ran.trace,
            ran.warnings,
            dat_sha256,
            profile,
            options.coverage.as_deref(),
        )
    }

    /// Selects from the ListXML `dat` with the arcade selector under `settings`, applies the Profile's choices for
    /// `MAME`, and lists the winners' Dependencies. An arcade Selection takes no Coverage.
    pub fn run_arcade(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        settings: &arcade::ArcadeSettings,
        options: &RunOptions,
    ) -> Result<Outcome, Error> {
        if options.coverage.is_some() {
            return Err(Error::Refused(ARCADE_COVERAGE_REFUSAL.to_owned()));
        }
        arcade::check(settings)?;
        let support = arcade::SupportFiles::read(&options.support_files)?;
        let curated = match settings.curated {
            true => Some(self.read_curated_list()?),
            false => None,
        };
        if options.cancelled() {
            return Err(Error::Cancelled);
        }
        let deadline = Instant::now() + options.timeout;
        let stop = || {
            if options.cancelled() {
                Err(Error::Cancelled)
            } else if Instant::now() >= deadline {
                Err(Error::Timeout(options.timeout))
            } else {
                Ok(())
            }
        };
        let parsed =
            title_select_engine::dat::Dat::read(dat).map_err(|e| Error::Dat(e.to_string()))?;
        if parsed.format != DatFormat::Listxml {
            return Err(Error::Dat(format!(
                "{} is not a MAME ListXML",
                dat.display()
            )));
        }
        let elected = arcade::elect(&parsed, settings, &support, curated, &stop)?;
        let mut trace = elected.trace;
        let system = parsed.name.clone();
        let mut selection = Selection {
            pins: SelectionPins {
                arcade: Some(elected.pins),
                ..self.pins.clone()
            },
            dat_sha256: parsed.sha256.clone(),
            system_settings: profile.systems.contains_key(&system),
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups: elected.groups,
            stale_choices: Default::default(),
            system,
        };
        choices::apply(&mut selection, profile.choices.get(&parsed.name));
        arcade::list_dependencies(&mut selection, &parsed, &mut trace);
        choices::recount(&mut selection);
        selection.summary.curated_unmatched = selection
            .pins
            .arcade
            .as_ref()
            .and_then(|a| a.curated.as_ref())
            .map_or(0, |c| c.unmatched.len());
        let mut lines = String::new();
        for record in trace {
            lines.push_str(&serde_json::to_string(&record).expect("a JSON value serializes"));
            lines.push('\n');
        }
        Ok(Outcome {
            selection,
            trace: lines,
            warnings: Vec::new(),
        })
    }

    fn outcome(
        &self,
        trace: String,
        warnings: Vec<String>,
        dat_sha256: String,
        profile: &SelectionProfile,
        coverage: Option<&[String]>,
    ) -> Result<Outcome, Error> {
        let derived = trace::derive(&trace)?;
        let mut selection = Selection {
            pins: SelectionPins {
                coverage: coverage.map(<[String]>::to_vec),
                ..self.pins.clone()
            },
            dat_sha256,
            system_settings: profile.systems.contains_key(&derived.system),
            summary: derived.summary,
            coverage_drift: derived.coverage_drift,
            groups: derived.groups,
            stale_choices: Default::default(),
            system: derived.system,
        };
        // Separate passes over the engine's result: before choices, so that a choice settles what the first flags;
        // after, so that a choice that leaves a disc uncovered is flagged again.
        title_select_api::discs::mark_unresolved(&mut selection);
        let choices = profile.choices.get(&selection.system);
        choices::apply(&mut selection, choices);
        title_select_api::discs::mark_uncovered_choices(&mut selection);
        Ok(Outcome {
            selection,
            trace,
            warnings,
        })
    }
}

/// Reads a Game's regions from the region tags in its name, exactly as a Selection does, so that Coverage and
/// Selection agree on them (ADR 0021).
pub struct Regions(native::GameRegions);

impl Regions {
    /// The regions of the Game named `name` (its name in the DAT), in the engine's default region order: `World`
    /// for an `Export` tag, and `Unknown` alone when no tag names a region.
    pub fn of(&self, name: &str) -> Vec<String> {
        self.0.of(name)
    }
}

/// The names a Profile may use, from the engine's internal configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub regions: Vec<String>,
    pub languages: Vec<String>,
    pub videos: Vec<String>,
    /// The region order a Selection uses when a Profile leaves it empty.
    pub default_region_order: Vec<String>,
    /// The video order a Selection uses when a Profile leaves it empty.
    pub default_video_order: Vec<String>,
}

/// The clone lists this binary's Selection is pinned to, from `third_party/clone-lists/pins.toml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Pinned {
    pub repository: String,
    pub commit: String,
}

/// The curated list as a run reads it: its pin and the shortnames its `ROMSET` names.
pub(crate) struct CuratedList {
    pub pin: Pinned,
    pub names: Vec<String>,
}

/// The curated-list repository and commit this binary was built with.
pub fn curated_list() -> Result<Pinned, Error> {
    #[derive(serde::Deserialize)]
    struct Pins {
        curated_list: Pinned,
    }
    let pins: Pins =
        toml::from_str(CURATED_PINS).map_err(|e| Error::CuratedList(format!("pins.toml: {e}")))?;
    Ok(pins.curated_list)
}

/// The clone-list repository and commit this binary was built with.
pub fn clone_lists() -> Result<Pinned, Error> {
    #[derive(serde::Deserialize)]
    struct Pins {
        clone_lists: Pinned,
    }
    let pins: Pins = toml::from_str(PINS).map_err(|e| Error::Install(format!("pins.toml: {e}")))?;
    Ok(pins.clone_lists)
}

/// The pins every Selection this binary makes records.
fn expected_pins() -> Result<SelectionPins, Error> {
    Ok(SelectionPins {
        arcade: None,
        retool_commit: None,
        patch_set_sha256: None,
        title_select_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        clone_list_commit: clone_lists()?.commit,
        trace_schema: SELECTION_TRACE_SCHEMA,
        coverage: None,
    })
}

/// The format of the DAT at `path`, by its root element as `Dat::read` finds it, or `None` for one this reader doesn't
/// know, such as a ClrMamePro DAT, which the Retool port reads.
fn format_of(path: &Path) -> Result<Option<DatFormat>, Error> {
    let file = File::open(path).map_err(io(path.display().to_string()))?;
    title_select_engine::dat::Dat::format_of(std::io::BufReader::new(file))
        .map_err(|e| Error::Dat(format!("{}: {e}", path.display())))
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path).map_err(io(path.display().to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(io(path.display().to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
