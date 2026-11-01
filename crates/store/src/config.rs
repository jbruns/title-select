//! The Operator's configuration: `title-select.toml` (ADR 0006).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ipnet::IpNet;
use serde::Deserialize;
use title_select_api::Workers;
use title_select_api::paths::{
    DEFAULT_MANAGED_FOLDER_TEMPLATE, DEFAULT_SOURCE_SET_TEMPLATE, PathTemplate, relative_path,
};

/// How many full Audits a Source Set keeps when the configuration does not say.
pub const DEFAULT_AUDITS_KEPT: usize = 20;
/// How many full Selections a Source Set keeps when the configuration does not say.
pub const DEFAULT_SELECTIONS_KEPT: usize = 20;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Named Roots: the folders Title Select may read.
    #[serde(default)]
    pub roots: BTreeMap<String, Root>,
    #[serde(default)]
    pub audits: Audits,
    #[serde(default)]
    pub selections: Selections,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub jobs: Jobs,
    #[serde(default)]
    pub path_templates: PathTemplates,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub path: PathBuf,
    /// Whether Title Select may write to this Root: Builds to its Managed Folders, and repairs to any Source Set
    /// in it. A Source Set is registered only in a read-only Root.
    #[serde(default)]
    pub writable: bool,
    /// For a read-only Root: the paths, relative to it, of the Source Sets in it that a repair may write to.
    #[serde(default)]
    pub writable_paths: Vec<String>,
    /// What the Root is for: `ingest` makes it an Ingest Folder, which must be writable and holds no Source Set or
    /// Managed Folder.
    #[serde(default)]
    pub kind: RootKind,
    /// For an Ingest Folder: how long, in seconds, an item's size and times must stay unchanged before it is
    /// assessed.
    pub settle_secs: Option<u64>,
    /// For an Ingest Folder: how often, in minutes, it is scanned on its own. Off when absent.
    pub scan_every_minutes: Option<u64>,
}

/// How long an item in an Ingest Folder must stay unchanged before it is assessed, when the Root does not say.
pub const DEFAULT_SETTLE_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    #[default]
    General,
    /// An Ingest Folder: where new files arrive, assessed, and moved out only by plans you approve.
    Ingest,
}

impl Root {
    pub fn is_ingest(&self) -> bool {
        self.kind == RootKind::Ingest
    }

    /// Whether Title Select may write to the folder `rel` of this Root: the Root is writable, or lists `rel` among
    /// its `writable_paths`.
    pub fn may_write(&self, rel: &str) -> bool {
        self.writable
            || relative_path(rel).is_ok_and(|rel| {
                self.writable_paths
                    .iter()
                    .any(|p| relative_path(p).is_ok_and(|p| p == rel))
            })
    }

    /// For an Ingest Folder, the settle interval in seconds.
    pub fn settle_secs(&self) -> u64 {
        self.settle_secs.unwrap_or(DEFAULT_SETTLE_SECS)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audits {
    /// The latest full Audits kept per Source Set; older ones are pruned to a summary.
    #[serde(default = "default_keep")]
    pub keep: usize,
}

fn default_keep() -> usize {
    DEFAULT_AUDITS_KEPT
}

impl Default for Audits {
    fn default() -> Self {
        Audits {
            keep: DEFAULT_AUDITS_KEPT,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selections {
    /// The latest full Selections kept per Source Set; older ones are pruned to their record.
    #[serde(default = "default_selections_keep")]
    pub keep: usize,
}

fn default_selections_keep() -> usize {
    DEFAULT_SELECTIONS_KEPT
}

impl Default for Selections {
    fn default() -> Self {
        Selections {
            keep: DEFAULT_SELECTIONS_KEPT,
        }
    }
}

/// The default Path Templates that propose a registration's path. Each can be overridden at registration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathTemplates {
    /// For a Source Set, relative to its Root.
    #[serde(default = "default_source_set_template")]
    pub source_set: PathTemplate,
    /// For a Managed Folder, relative to its Play Library Root.
    #[serde(default = "default_managed_folder_template")]
    pub managed_folder: PathTemplate,
}

fn default_source_set_template() -> PathTemplate {
    PathTemplate::parse(DEFAULT_SOURCE_SET_TEMPLATE).expect("the default template parses")
}

fn default_managed_folder_template() -> PathTemplate {
    PathTemplate::parse(DEFAULT_MANAGED_FOLDER_TEMPLATE).expect("the default template parses")
}

impl Default for PathTemplates {
    fn default() -> Self {
        PathTemplates {
            source_set: default_source_set_template(),
            managed_folder: default_managed_folder_template(),
        }
    }
}

/// How the Service knows a request comes from the Operator. The Operator token, read from the file named by
/// `TITLE_SELECT_OPERATOR_TOKEN_FILE`, is the other way.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// The request header carrying the Operator's identity, set by an authenticating reverse proxy.
    pub identity_header: Option<String>,
    /// The only peers, as CIDRs, whose `identity_header` is trusted.
    #[serde(default)]
    pub trusted_proxies: Vec<IpNet>,
    /// An identity given to every request with no other credential, for development. Only allowed while the
    /// Service listens on a loopback address.
    pub dev_identity: Option<String>,
}

/// How the Service runs Jobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Jobs {
    /// How many Jobs run at once.
    #[serde(default = "one")]
    pub concurrent: usize,
    /// How hard an Audit works the disks and CPUs; `readers` and `hashers` override its counts.
    #[serde(default)]
    pub pace: Pace,
    /// Threads reading and decompressing payloads.
    pub readers: Option<usize>,
    /// Threads hashing payloads.
    pub hashers: Option<usize>,
}

fn one() -> usize {
    1
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            concurrent: 1,
            pace: Pace::default(),
            readers: None,
            hashers: None,
        }
    }
}

impl Jobs {
    /// The worker counts for an Audit: the pace's, with each explicit count overriding.
    pub fn workers(&self) -> Workers {
        let pace = self.pace.workers();
        Workers {
            readers: self.readers.unwrap_or(pace.readers),
            hashers: self.hashers.unwrap_or(pace.hashers),
        }
    }
}

/// A preset for an Audit's worker counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pace {
    /// One reader and one hasher: one file at a time, for a disk shared with other work.
    Gentle,
    /// Two readers and two hashers.
    #[default]
    Default,
    /// One reader and one hasher per CPU.
    Unthrottled,
}

impl Pace {
    pub fn workers(self) -> Workers {
        let n = match self {
            Pace::Gentle => 1,
            Pace::Default => 2,
            Pace::Unthrottled => std::thread::available_parallelism().map_or(1, |n| n.get()),
        };
        Workers {
            readers: n,
            hashers: n,
        }
    }
}

impl Default for Config {
    /// No Roots, the default Audit history, Job limits and Path Templates, and no auth.
    fn default() -> Self {
        Config {
            roots: BTreeMap::new(),
            audits: Audits::default(),
            selections: Selections::default(),
            auth: Auth::default(),
            jobs: Jobs::default(),
            path_templates: PathTemplates::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid configuration: {0}")]
pub struct Error(String);

impl Config {
    pub fn load(path: &Path) -> Result<Config, Error> {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Config::parse(&text).map_err(|Error(e)| Error(format!("{}: {e}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Config, Error> {
        let config: Config = toml::from_str(text).map_err(|e| Error(e.to_string()))?;
        if config.audits.keep == 0 {
            return Err(Error("audits.keep must be at least 1".into()));
        }
        if config.selections.keep == 0 {
            return Err(Error("selections.keep must be at least 1".into()));
        }
        if config.jobs.concurrent == 0 {
            return Err(Error("jobs.concurrent must be at least 1".into()));
        }
        if config.jobs.readers == Some(0) || config.jobs.hashers == Some(0) {
            return Err(Error(
                "jobs.readers and jobs.hashers must be at least 1".into(),
            ));
        }
        let auth = &config.auth;
        match (&auth.identity_header, auth.trusted_proxies.is_empty()) {
            (Some(header), false) if is_header_name(header) => {}
            (Some(header), false) => {
                return Err(Error(format!(
                    "auth.identity_header {header:?} is not a header name"
                )));
            }
            (Some(_), true) => {
                return Err(Error(
                    "auth.identity_header needs auth.trusted_proxies".into(),
                ));
            }
            (None, false) => {
                return Err(Error(
                    "auth.trusted_proxies needs auth.identity_header".into(),
                ));
            }
            (None, true) => {}
        }
        if auth
            .dev_identity
            .as_deref()
            .is_some_and(|i| i.trim().is_empty())
        {
            return Err(Error("auth.dev_identity is empty".into()));
        }
        for (name, root) in &config.roots {
            if name.is_empty() {
                return Err(Error("a root has an empty name".into()));
            }
            if root.is_ingest() && !root.writable {
                return Err(Error(format!(
                    "roots.{name} is an ingest folder, which needs writable = true"
                )));
            }
            if !root.is_ingest()
                && (root.settle_secs.is_some() || root.scan_every_minutes.is_some())
            {
                return Err(Error(format!(
                    "roots.{name}: settle_secs and scan_every_minutes are only for a root with kind = \"ingest\""
                )));
            }
            if !root.writable_paths.is_empty() && (root.writable || root.is_ingest()) {
                return Err(Error(format!(
                    "roots.{name}.writable_paths is only for a read-only root; this one is writable already"
                )));
            }
            for path in &root.writable_paths {
                if let Err(e) = relative_path(path) {
                    return Err(Error(format!(
                        "roots.{name}.writable_paths: {path:?}: a source set's path {e}"
                    )));
                }
            }
            if root.scan_every_minutes == Some(0) {
                return Err(Error(format!(
                    "roots.{name}.scan_every_minutes must be at least 1; leave it out to scan only when you ask"
                )));
            }
        }
        Ok(config)
    }

    pub fn root(&self, name: &str) -> Option<&Root> {
        self.roots.get(name)
    }
}

/// Whether `name` is a valid HTTP header name: a non-empty token.
fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pace_preset_sets_the_worker_counts() {
        let config = Config::parse("[jobs]\npace = \"gentle\"").unwrap();

        assert_eq!(
            config.jobs.workers(),
            Workers {
                readers: 1,
                hashers: 1
            }
        );
        assert_eq!(
            Config::default().jobs.workers(),
            Workers {
                readers: 2,
                hashers: 2
            }
        );
        assert_eq!(Config::default().jobs.concurrent, 1);
    }

    #[test]
    fn an_explicit_worker_count_overrides_the_pace() {
        let config = Config::parse("[jobs]\npace = \"gentle\"\nhashers = 6").unwrap();

        assert_eq!(
            config.jobs.workers(),
            Workers {
                readers: 1,
                hashers: 6
            }
        );
    }

    #[test]
    fn unthrottled_uses_every_cpu() {
        let cpus = std::thread::available_parallelism().unwrap().get();

        let config = Config::parse("[jobs]\npace = \"unthrottled\"").unwrap();

        assert_eq!(
            config.jobs.workers(),
            Workers {
                readers: cpus,
                hashers: cpus
            }
        );
    }

    #[test]
    fn zero_workers_or_jobs_are_refused() {
        for text in [
            "[jobs]\nconcurrent = 0",
            "[jobs]\nreaders = 0",
            "[jobs]\nhashers = 0",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn path_templates_default_when_absent_and_are_checked_when_given() {
        let defaults = Config::parse("").unwrap().path_templates;
        assert_eq!(defaults.source_set.as_str(), "{provider}/{dat}");
        assert_eq!(defaults.managed_folder.as_str(), "{platform}");

        let config =
            Config::parse("[path_templates]\nsource_set = \"Sets/{platform}/{dat}\"").unwrap();
        assert_eq!(
            config.path_templates.source_set.as_str(),
            "Sets/{platform}/{dat}"
        );
        assert_eq!(config.path_templates.managed_folder.as_str(), "{platform}");

        for text in [
            "[path_templates]\nsource_set = \"{system}\"",
            "[path_templates]\nmanaged_folder = \"{platform\"",
            "[path_templates]\nother = \"{dat}\"",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
        for (template, reason) in [
            ("", "the template must name a folder inside its root"),
            ("../{dat}", "the template must not contain `..`"),
            ("/{dat}", "the template must be relative to its root"),
            ("C:/{dat}", "the template must be relative to its root"),
            ("./C:/{dat}", "the template must be relative to its root"),
            (
                r"{provider}\{dat}",
                "the template must separate folders with /",
            ),
        ] {
            for key in ["source_set", "managed_folder"] {
                let text = format!("[path_templates]\n{key} = {template:?}");
                let error = Config::parse(&text).unwrap_err().to_string();
                assert!(
                    error.contains(reason) && error.contains(key),
                    "{text}: {error}"
                );
            }
        }
    }

    #[test]
    fn an_ingest_root_is_writable_and_only_it_takes_ingest_settings() {
        let config = Config::parse(
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nscan_every_minutes = 5",
        )
        .unwrap();
        let root = config.root("in").unwrap();
        assert!(root.is_ingest());
        assert_eq!(root.settle_secs(), DEFAULT_SETTLE_SECS);
        assert_eq!(root.scan_every_minutes, Some(5));

        for text in [
            "[roots.in]\npath = \"/in\"\nkind = \"ingest\"",
            "[roots.in]\npath = \"/in\"\nsettle_secs = 5",
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nscan_every_minutes = 0",
            "[roots.in]\npath = \"/in\"\nkind = \"other\"",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn writable_paths_let_a_read_only_root_write_only_those_folders() {
        let config = Config::parse(
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"No-Intro/Atari 2600\", \"Other/\"]",
        )
        .unwrap();
        let root = config.root("sets").unwrap();
        assert!(!root.writable);
        assert!(root.may_write("No-Intro/Atari 2600"));
        assert!(root.may_write("Other"));
        assert!(!root.may_write("No-Intro"));
        assert!(!root.may_write("No-Intro/Atari 2600/sub"));

        for text in [
            "[roots.sets]\npath = \"/sets\"\nwritable = true\nwritable_paths = [\"a\"]",
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"../a\"]",
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"/a\"]",
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nwritable_paths = [\"a\"]",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn an_identity_header_and_trusted_proxies_need_each_other() {
        let both = "[auth]\nidentity_header = \"Remote-User\"\ntrusted_proxies = [\"10.0.0.0/8\", \"::1/128\"]";
        let config = Config::parse(both).unwrap();
        assert_eq!(config.auth.trusted_proxies.len(), 2);

        assert!(Config::parse("[auth]\nidentity_header = \"Remote-User\"").is_err());
        assert!(Config::parse("[auth]\ntrusted_proxies = [\"10.0.0.0/8\"]").is_err());
        assert!(
            Config::parse(
                "[auth]\nidentity_header = \"Remote User\"\ntrusted_proxies = [\"10.0.0.0/8\"]"
            )
            .is_err()
        );
        assert!(
            Config::parse(
                "[auth]\nidentity_header = \"Remote-User\"\ntrusted_proxies = [\"nonsense\"]"
            )
            .is_err()
        );
    }
}
