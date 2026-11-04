mod client;

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use title_select_api::JobState;
use title_select_api::ingest::Proposal;
use title_select_api::lifecycle::{Holds, RegistrationKind, Unregistration};
use title_select_api::selection::SelectionProfile;
use title_select_engine::{AuditEvidence, EvidenceLevel, Finding, GameStatus, Report};
use title_select_server::Settings;
use title_select_store::config::Config;
use title_select_store::{ImportOutcome, Store};

/// Audits your source sets against DAT revisions.
#[derive(Parser)]
#[command(version = version())]
struct Cli {
    /// The store directory: the database and the DAT blobs.
    #[arg(long, global = true, env = "TITLE_SELECT_STORE")]
    store: Option<PathBuf>,
    /// The configuration file (`title-select.toml`) that declares your roots.
    #[arg(long, global = true, env = "TITLE_SELECT_CONFIG")]
    config: Option<PathBuf>,
    /// The file holding your operator token: the service accepts it, and `audit --server` sends it.
    #[arg(long, global = true, env = "TITLE_SELECT_OPERATOR_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit a source set against a DAT revision and print a JSON report.
    /// Reads only; never changes the source set.
    ///
    /// With `--dat <file> <dir>` the audit is stateless. With `--set <name>` it audits a registered source set
    /// and records the audit in the store. With `--server <url> --set <name>` the service at `<url>` runs it as a
    /// job, which this follows to completion, showing progress and a summary on stderr.
    ///
    /// An incremental audit, the default, reuses the payload read recorded for a file whose device, inode, size,
    /// modification time and status change time are all unchanged since it was read. That change test is a
    /// heuristic: a file rewritten in place with its times restored passes it. Use `--evidence full` to read every
    /// payload again.
    Audit {
        /// The Logiqx XML DAT file to audit against.
        #[arg(long, required_unless_present = "set", conflicts_with = "set")]
        dat: Option<PathBuf>,
        /// The registered source set to audit, by name.
        #[arg(long)]
        set: Option<String>,
        /// With `--set`, the DAT revision to audit against, by a prefix of its SHA-256. Defaults to the latest
        /// audit's DAT revision, else the newest imported revision of the source set's DAT.
        #[arg(long, requires = "set")]
        revision: Option<String>,
        /// The service to run the audit, such as `http://127.0.0.1:8080`. Sends your operator token.
        #[arg(long, requires = "set")]
        server: Option<String>,
        /// How the audit gets each entry's checksums: `quick` takes a ZIP member's size and CRC32 from the
        /// archive's directory as claim evidence, without reading its payload; `incremental` reads only payloads
        /// of files that look changed (a heuristic); `full` reads every payload. A stateless audit has no earlier
        /// reads to reuse, so `incremental` there reads every payload and reports `full`.
        #[arg(long, default_value = "incremental", value_parser = parse_evidence)]
        evidence: AuditEvidence,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// The source set's directory.
        #[arg(required_unless_present = "set", conflicts_with = "set")]
        source_set: Option<PathBuf>,
    },
    /// Manage DATs and their DAT revisions.
    Dat {
        #[command(subcommand)]
        command: DatCommand,
    },
    /// Manage Redump cue packs: the `.cue` sheets a repair fills or replaces a disc's cue sheet from.
    CuePack {
        #[command(subcommand)]
        command: CuePackCommand,
    },
    /// Manage the source set registry.
    Set {
        #[command(subcommand)]
        command: SetCommand,
    },
    /// Manage Ingest Folders: roots of kind `ingest` where new files arrive.
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Manage play library folders.
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Manage additions sets.
    Additions {
        #[command(subcommand)]
        command: AdditionsCommand,
    },
    /// See what is registered in a root.
    Root {
        #[command(subcommand)]
        command: RootCommand,
    },
    /// Retire or restore selection profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// List what you unregistered, whose history stays readable, and forget it.
    Unregistered {
        #[command(subcommand)]
        command: UnregisteredCommand,
    },
    /// Run the service: the `/api/v1` JSON API and the audit and selection jobs. It holds the store until it stops.
    Serve {
        /// The address to listen on.
        #[arg(long, env = "TITLE_SELECT_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// The workbench's built bundle (`target/site` after `cargo leptos build`), served at `/`.
        #[arg(long, env = "TITLE_SELECT_UI")]
        ui: Option<PathBuf>,
        /// The selection engine install that `tools/retool/setup` builds, for selections and previews. Defaults
        /// to `/opt/retool`. Without one the service still runs audits.
        #[arg(long, env = title_select_selection::RETOOL_ENV)]
        retool: Option<PathBuf>,
    },
    /// Select from a DAT with a selection profile, running the pinned selection engine, and print the explained
    /// selection as JSON: every group's winner, alternatives and exclusions with the rule responsible for each.
    ///
    /// The selection engine runs in a scratch folder in the store's `tmp` when `--store` is given, else in the
    /// system's temporary folder, and the folder is deleted afterwards.
    Select {
        /// The Logiqx XML DAT file to select from.
        #[arg(long)]
        dat: PathBuf,
        /// The selection profile, as JSON.
        #[arg(long)]
        profile: PathBuf,
        /// The selection engine install that `tools/retool/setup` builds. Defaults to `/opt/retool`.
        #[arg(long, env = title_select_selection::RETOOL_ENV)]
        retool: Option<PathBuf>,
        /// Seconds the selection engine may run before it is stopped.
        #[arg(long, default_value_t = title_select_selection::DEFAULT_TIMEOUT.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// Write the selection to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Check that the service is up, for a container healthcheck. Prints nothing on success; exits 1 with the
    /// reason on stderr otherwise.
    Health {
        /// The service's health endpoint.
        #[arg(long, default_value = "http://127.0.0.1:8080/api/v1/health")]
        url: String,
        /// Seconds to wait for the whole check, from 1 to 300.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum DatCommand {
    /// Import an unmodified DAT file as a DAT revision. Importing the same bytes again changes nothing,
    /// except to restore the file if the store had dropped it.
    Import {
        file: PathBuf,
        /// Who publishes the DAT, for example `No-Intro`.
        #[arg(long)]
        provider: String,
        /// Where the file was downloaded from.
        #[arg(long)]
        source_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum CuePackCommand {
    /// Import an unmodified cue pack, a ZIP of `.cue` sheets, for a DAT you imported. A repair then fills a
    /// missing cue sheet, or replaces one that doesn't match, from it. Importing the same bytes again changes
    /// nothing.
    Import {
        file: PathBuf,
        /// The DAT its cue sheets are for, as `<provider>/<name>`, for example `Redump/Sony - PlayStation`.
        #[arg(long)]
        dat: String,
        /// Where the file was downloaded from.
        #[arg(long)]
        source_url: Option<String>,
    },
    /// List the cue packs you imported.
    List,
}

#[derive(Subcommand)]
enum SetCommand {
    /// Register a folder in a root as a source set, bound to a DAT. With no path, the path template proposes
    /// one from the DAT: by default `{provider}/{dat}`, such as `No-Intro/Atari - Atari 2600`.
    Add {
        /// The root's name in the configuration.
        root: String,
        /// The source set's folder, relative to the root. Leave it out to use the path the template proposes.
        path: Option<String>,
        /// The DAT to audit it against, as `<provider>/<name>`.
        #[arg(long)]
        dat: String,
        /// The path template to propose the path from, instead of `path_templates.source_set` in your
        /// configuration. Its tokens are `{provider}`, `{dat}` and `{platform}`.
        #[arg(long, conflicts_with = "path")]
        template: Option<String>,
    },
    /// List the source sets with the DAT revision each was last verified against.
    List,
    /// List a source set's audits, newest first, with time, DAT revision, counts and evidence.
    History { name: String },
    /// Show what holds a source set registered: each reason it can't be unregistered yet.
    Holds { name: String },
    /// Unregister a source set. No game file is touched; its audits and selections stay readable as unregistered.
    /// Refused while anything holds it.
    Unregister { name: String },
}

#[derive(Subcommand)]
enum LibraryCommand {
    /// Show what holds a play library folder registered.
    Holds { id: i64 },
    /// Unregister a play library folder. What was built in it stays as files Title Select no longer manages.
    Unregister { id: i64 },
}

#[derive(Subcommand)]
enum AdditionsCommand {
    /// Show what holds the additions set of a system registered.
    Holds { system: String },
    /// Unregister the additions set of a system. No addition's file is touched.
    Unregister { system: String },
}

#[derive(Subcommand)]
enum RootCommand {
    /// Show what is registered in a root: each registration that holds it in your configuration.
    Holds { name: String },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Retire a selection profile: it is hidden from new choices, and its versions stay for the selections that
    /// used them. Refused while a play library folder uses it.
    Retire { name: String },
    /// Restore a retired selection profile.
    Restore { name: String },
}

#[derive(Subcommand)]
enum UnregisteredCommand {
    /// List what you unregistered, newest first, with the history the store still holds for each.
    List,
    /// Show the history an unregistered source set, play library folder, additions set or ingest folder left: its
    /// audits, selections, builds and jobs, newest first. Read by its id, as its name may be registered again.
    History {
        /// `source_set`, `library_folder`, `additions_set` or `ingest_folder`.
        #[arg(value_parser = parse_kind)]
        kind: RegistrationKind,
        id: i64,
    },
    /// Forget an unregistered source set, play library folder, additions set or ingest folder: its audits,
    /// selections, builds and jobs are removed from the store. No file is touched.
    Forget {
        /// `source_set`, `library_folder`, `additions_set` or `ingest_folder`.
        #[arg(value_parser = parse_kind)]
        kind: RegistrationKind,
        id: i64,
    },
}

#[derive(Subcommand)]
enum IngestCommand {
    /// Register an ingest root as an ingest folder of this store, writing its marker. Title Select scans and
    /// changes an ingest folder only once it is registered.
    Register {
        /// The root's name in the configuration; it must have `kind = "ingest"`.
        root: String,
    },
    /// Show what holds an ingest folder registered.
    Holds { root: String },
    /// Unregister an ingest folder. No file in it is touched.
    Unregister { root: String },
    /// Scan an ingest folder now: note what arrived, changed or went, and assess each item that has settled.
    /// Changes nothing in the folder.
    Scan {
        /// The ingest root's name.
        root: String,
    },
    /// List an ingest folder's items with their states and what is proposed for each.
    Items {
        /// The ingest root's name.
        root: String,
    },
}

/// The package version, then the upstream commits selection is pinned to.
fn version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let package = env!("CARGO_PKG_VERSION");
        match title_select_selection::upstream() {
            Ok(up) => format!(
                "{package}\nselection engine: {} at {}, patch set {}\nclone lists and metadata: {} at {}",
                up.retool.repository,
                up.retool.commit,
                up.patch_set_sha256,
                up.clone_lists.repository,
                up.clone_lists.commit,
            ),
            Err(err) => format!("{package}\n{err}"),
        }
    });
    &VERSION
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("title-select: {message}");
            ExitCode::FAILURE
        }
    }
}

fn open_store(cli_store: Option<&Path>) -> Result<Store, String> {
    let dir = cli_store.ok_or("no store: pass --store <dir> or set TITLE_SELECT_STORE")?;
    Store::open(dir).map_err(|e| e.to_string())
}

/// The configuration file, or no roots when none is given.
fn load_config(path: Option<&Path>) -> Result<Config, String> {
    match path {
        Some(path) => Config::load(path).map_err(|e| e.to_string()),
        None => Ok(Config::default()),
    }
}

fn write_report(report: &impl serde::Serialize, output: Option<PathBuf>) -> Result<(), String> {
    let mut json = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
    json.push(b'\n');
    match output {
        Some(path) => std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display())),
        None => std::io::stdout()
            .write_all(&json)
            .map_err(|e| e.to_string()),
    }
}

fn parse_kind(s: &str) -> Result<RegistrationKind, String> {
    serde_json::from_value(serde_json::Value::String(s.replace('-', "_"))).map_err(|_| {
        "expected source_set, library_folder, additions_set or ingest_folder".to_owned()
    })
}

/// Prints what holds a registration, one reason a line.
fn print_holds(holds: Holds) {
    if holds.holds.is_empty() {
        println!("nothing holds it; you can unregister it");
    }
    for hold in holds.holds {
        println!("{hold}");
    }
}

fn print_unregistration(done: Unregistration) {
    println!(
        "{} {}: unregistered; no game file was touched, and its history stays readable as unregistered",
        done.kind.label(),
        done.name
    );
    for note in done.notes {
        println!("{note}");
    }
}

fn parse_evidence(s: &str) -> Result<AuditEvidence, String> {
    AuditEvidence::parse(s).ok_or_else(|| "expected quick, incremental or full".to_owned())
}

/// How many Entries' checksums the Audit read, reused or took as claims.
fn payloads_line(report: &Report) -> String {
    let (mut read, mut reused, mut claimed) = (0, 0, 0);
    for evidence in report.entries.iter().filter_map(|e| e.evidence.as_ref()) {
        match (evidence.level, evidence.reused) {
            (EvidenceLevel::Claim, _) => claimed += 1,
            (_, Some(_)) => reused += 1,
            (_, None) => read += 1,
        }
    }
    format!(
        "{} audit: {read} entries read, {reused} reused because their files looked unchanged (a heuristic), \
         {claimed} claimed from ZIP directories without reading payloads",
        report.evidence.as_str()
    )
}

fn run(cli: Cli) -> Result<(), String> {
    let store = cli.store.as_deref();
    let config = cli.config.as_deref();
    match cli.command {
        Command::Audit {
            set: Some(set),
            revision,
            server: Some(server),
            evidence,
            output,
            ..
        } => {
            let report = client::audit(client::Request {
                server: &server,
                set,
                revision,
                evidence,
                token_file: cli.token_file.as_deref(),
            })?;
            write_report(&report, output)
        }
        Command::Audit {
            dat: Some(dat),
            source_set: Some(source_set),
            evidence,
            output,
            ..
        } => {
            let file = dat
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let evidence = match evidence {
                AuditEvidence::Quick => AuditEvidence::Quick,
                AuditEvidence::Incremental => {
                    eprintln!(
                        "this audit has no store and so no earlier payload reads to reuse: it ran as a full \
                         audit"
                    );
                    AuditEvidence::Full
                }
                AuditEvidence::Full => AuditEvidence::Full,
            };
            let report = title_select_engine::audit_with(
                &dat,
                &source_set,
                &title_select_engine::Options {
                    file,
                    confine: None,
                    workers: title_select_engine::ONE_WORKER_EACH,
                    progress: None,
                    evidence,
                    reads: None,
                },
            )
            .map_err(|e| e.to_string())?;
            write_report(&report, output)
        }
        Command::Audit {
            set: Some(set),
            revision,
            evidence,
            output,
            ..
        } => {
            let config = load_config(config)?;
            let audit = open_store(store)?
                .audit_source_set(&config, &set, revision.as_deref(), evidence)
                .map_err(|e| e.to_string())?;
            eprintln!("{}", payloads_line(&audit.report));
            write_report(&audit.report, output)
        }
        Command::Audit { .. } => unreachable!("clap requires --dat with a directory, or --set"),
        Command::Serve { listen, ui, retool } => {
            let store = store.ok_or("no store: pass --store <dir> or set TITLE_SELECT_STORE")?;
            title_select_server::run(Settings {
                store: store.to_path_buf(),
                config: load_config(config)?,
                listen,
                token_file: cli.token_file,
                ui,
                retool,
                hold_jobs: false,
            })
            .map_err(|e| e.to_string())
        }
        Command::Select {
            dat,
            profile,
            retool,
            timeout,
            output,
        } => {
            let profile: SelectionProfile = serde_json::from_slice(
                &std::fs::read(&profile).map_err(|e| format!("{}: {e}", profile.display()))?,
            )
            .map_err(|e| format!("{}: {e}", profile.display()))?;
            let work_dir = match store {
                Some(store) => store.join(title_select_store::TMP),
                None => std::env::temp_dir(),
            };
            let outcome = title_select_selection::Engine::locate(retool.as_deref())
                .and_then(|engine| {
                    engine.run(
                        &dat,
                        &profile,
                        &title_select_selection::RunOptions {
                            work_dir,
                            timeout: std::time::Duration::from_secs(timeout),
                            cancel: None,
                        },
                    )
                })
                .map_err(|e| e.to_string())?;
            for warning in &outcome.warnings {
                eprintln!("title-select: selection engine warning: {warning}");
            }
            write_report(&outcome.selection, output)
        }
        Command::Health { url, timeout } => {
            client::health(&url, std::time::Duration::from_secs(timeout))
        }
        Command::Dat {
            command:
                DatCommand::Import {
                    file,
                    provider,
                    source_url,
                },
        } => {
            let (revision, outcome) = open_store(store)?
                .import_dat(&file, &provider, source_url.as_deref())
                .map_err(|e| e.to_string())?;
            let what = match outcome {
                ImportOutcome::Added => "imported",
                ImportOutcome::AlreadyImported => "already imported",
                ImportOutcome::Restored => "already imported; its file is restored",
            };
            println!(
                "{}: DAT revision {} (version {}) {what}",
                revision.dat(),
                revision.sha256,
                revision.version
            );
            Ok(())
        }
        Command::CuePack {
            command:
                CuePackCommand::Import {
                    file,
                    dat,
                    source_url,
                },
        } => {
            let (pack, outcome) = open_store(store)?
                .import_cue_pack(&file, &dat, source_url.as_deref())
                .map_err(|e| e.to_string())?;
            let what = match outcome {
                ImportOutcome::Added => "imported",
                ImportOutcome::AlreadyImported => "already imported",
                ImportOutcome::Restored => "already imported; its file is restored",
            };
            println!(
                "{}: cue pack {} ({}) {what}",
                pack.dat,
                pack.sha256,
                cue_sheets(pack.cues)
            );
            Ok(())
        }
        Command::CuePack {
            command: CuePackCommand::List,
        } => {
            for pack in open_store(store)?.cue_packs().map_err(|e| e.to_string())? {
                println!(
                    "{}\t{}\t{}\t{}\t{}{}",
                    pack.dat,
                    pack.file,
                    cue_sheets(pack.cues),
                    pack.imported_at,
                    pack.sha256,
                    if pack.blob_present {
                        ""
                    } else {
                        "\tfile deleted; import it again to use it"
                    }
                );
            }
            Ok(())
        }
        Command::Set {
            command:
                SetCommand::Add {
                    root,
                    path,
                    dat,
                    template,
                },
        } => {
            let config = load_config(config)?;
            let store = open_store(store)?;
            let path = match path {
                Some(path) => path,
                None => store
                    .propose_source_set_path(&config, &dat, template.as_deref())
                    .map_err(|e| e.to_string())?,
            };
            let set = store
                .add_source_set(&config, &root, &path, &dat)
                .map_err(|e| e.to_string())?;
            println!(
                "{}: added from {}:{}, bound to {}",
                set.name, set.root, set.path, set.dat
            );
            Ok(())
        }
        Command::Ingest {
            command: IngestCommand::Register { root },
        } => {
            let config = load_config(config)?;
            let folder = open_store(store)?
                .register_ingest_folder(&config, &root)
                .map_err(|e| e.to_string())?;
            println!(
                "{}: registered as an ingest folder at {}",
                folder.root, folder.path
            );
            Ok(())
        }
        Command::Ingest {
            command: IngestCommand::Scan { root },
        } => {
            let config = load_config(config)?;
            let job = open_store(store)?
                .scan_ingest_folder(&config, &root, "the Operator")
                .map_err(|e| e.to_string())?;
            match job.error {
                Some(error) if job.state != JobState::Succeeded => {
                    Err(format!("the scan of {root} failed: {error}"))
                }
                _ => {
                    println!("{root}: scanned (job {})", job.id);
                    Ok(())
                }
            }
        }
        Command::Ingest {
            command: IngestCommand::Items { root },
        } => {
            for item in open_store(store)?
                .ingest_items(&root)
                .map_err(|e| e.to_string())?
            {
                let proposal = match &item.assessment.proposal {
                    Some(Proposal::SourceSet {
                        source_set,
                        game,
                        need,
                    }) => format!("{source_set}: {game} ({})", need.label()),
                    Some(Proposal::Addition {
                        system: Some(system),
                    }) => {
                        format!("addition for {system}")
                    }
                    Some(Proposal::Addition { system: None }) => "addition".to_owned(),
                    Some(Proposal::DatImport) => "DAT import".to_owned(),
                    None => item.reason.clone().unwrap_or_default(),
                };
                println!(
                    "{}\t{}\t{}\t{proposal}",
                    item.id,
                    item.state.as_str(),
                    item.name
                );
            }
            Ok(())
        }
        Command::Set {
            command: SetCommand::Holds { name },
        } => {
            print_holds(
                open_store(store)?
                    .source_set_holds(&name)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Set {
            command: SetCommand::Unregister { name },
        } => {
            let config = load_config(config)?;
            print_unregistration(
                open_store(store)?
                    .unregister_source_set(&config, &name)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Ingest {
            command: IngestCommand::Holds { root },
        } => {
            print_holds(
                open_store(store)?
                    .ingest_folder_holds(&root)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Ingest {
            command: IngestCommand::Unregister { root },
        } => {
            let config = load_config(config)?;
            print_unregistration(
                open_store(store)?
                    .unregister_ingest_folder(&config, &root)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Library {
            command: LibraryCommand::Holds { id },
        } => {
            print_holds(
                open_store(store)?
                    .library_folder_holds(id)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Library {
            command: LibraryCommand::Unregister { id },
        } => {
            let config = load_config(config)?;
            print_unregistration(
                open_store(store)?
                    .unregister_library_folder(&config, id)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Additions {
            command: AdditionsCommand::Holds { system },
        } => {
            print_holds(
                open_store(store)?
                    .additions_set_holds(&system)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Additions {
            command: AdditionsCommand::Unregister { system },
        } => {
            let config = load_config(config)?;
            print_unregistration(
                open_store(store)?
                    .unregister_additions_set(&config, &system)
                    .map_err(|e| e.to_string())?,
            );
            Ok(())
        }
        Command::Root {
            command: RootCommand::Holds { name },
        } => {
            let config = load_config(config)?;
            let holds = open_store(store)?
                .root_holds(&config, &name)
                .map_err(|e| e.to_string())?;
            if holds.holds.is_empty() {
                println!(
                    "nothing is registered in {name}; you can remove it from your configuration"
                );
            }
            for hold in holds.holds {
                println!("{hold}");
            }
            Ok(())
        }
        Command::Profile {
            command: ProfileCommand::Retire { name },
        } => {
            open_store(store)?
                .retire_profile(&name)
                .map_err(|e| e.to_string())?;
            println!("{name}: retired; it is hidden from new choices until you restore it");
            Ok(())
        }
        Command::Profile {
            command: ProfileCommand::Restore { name },
        } => {
            open_store(store)?
                .restore_profile(&name)
                .map_err(|e| e.to_string())?;
            println!("{name}: restored");
            Ok(())
        }
        Command::Unregistered {
            command: UnregisteredCommand::List,
        } => {
            for record in open_store(store)?
                .unregistered()
                .map_err(|e| e.to_string())?
            {
                println!(
                    "{}\t{}\t{}\t{}:{}\tunregistered {}\taudits={} selections={} builds={} jobs={} kept_files={}",
                    record.kind.as_str(),
                    record.id,
                    record.name,
                    record.root,
                    record.path,
                    record.unregistered_at,
                    record.audits,
                    record.selections,
                    record.builds,
                    record.jobs,
                    record.kept_files
                );
            }
            Ok(())
        }
        Command::Unregistered {
            command: UnregisteredCommand::History { kind, id },
        } => {
            let history = open_store(store)?
                .unregistered_history(kind, id)
                .map_err(|e| e.to_string())?;
            let record = &history.record;
            println!(
                "{} {} ({}:{}): registered {}, unregistered {}",
                record.kind.label(),
                record.name,
                record.root,
                record.path,
                record.registered_at,
                record.unregistered_at
            );
            for audit in &history.audits {
                println!(
                    "audit\t{}\t{}\t{}",
                    audit.id, audit.finished_at, audit.dat_revision_version
                );
            }
            for selection in &history.selections {
                println!(
                    "selection\t{}\t{}\t{} v{}",
                    selection.id,
                    selection.created_at,
                    selection.profile.name,
                    selection.profile.version
                );
            }
            for build in &history.builds {
                println!(
                    "build\t{}\t{}\t{}",
                    build.id,
                    build.state.as_str(),
                    build.started_at
                );
            }
            for job in &history.jobs {
                println!(
                    "job\t{}\t{}\t{}\t{}",
                    job.id,
                    job.kind.as_str(),
                    job.state.as_str(),
                    job.created_at
                );
            }
            Ok(())
        }
        Command::Unregistered {
            command: UnregisteredCommand::Forget { kind, id },
        } => {
            let forgotten = open_store(store)?
                .forget(kind, id)
                .map_err(|e| e.to_string())?;
            println!(
                "{} {}: forgotten: {} audits, {} selections, {} builds, {} jobs and {} DAT revisions nothing else \
                 used were removed from the store; no file was touched",
                forgotten.kind.label(),
                forgotten.name,
                forgotten.audits,
                forgotten.selections,
                forgotten.builds,
                forgotten.jobs,
                forgotten.dat_revisions
            );
            Ok(())
        }
        Command::Set {
            command: SetCommand::List,
        } => {
            for set in open_store(store)?
                .source_sets()
                .map_err(|e| e.to_string())?
            {
                let last = match &set.last_verified_against {
                    Some(l) => format!("last verified against {} ({})", l.sha256, l.version),
                    None => "never audited".to_owned(),
                };
                println!(
                    "{}\t{}:{}\t{}\t{last}",
                    set.name, set.root, set.path, set.dat
                );
            }
            Ok(())
        }
        Command::Set {
            command: SetCommand::History { name },
        } => {
            for audit in open_store(store)?
                .history(&name)
                .map_err(|e| e.to_string())?
            {
                let findings = Finding::ALL
                    .iter()
                    .map(|f| format!("{}={}", f.as_str(), audit.findings[f]));
                let games = GameStatus::ALL
                    .iter()
                    .map(|g| format!("{}={}", g.as_str(), audit.games[g]));
                let counts: Vec<String> = findings.chain(games).collect();
                println!(
                    "{}\t{}\t{} ({})\t{}\tevidence={}\t{}",
                    audit.id,
                    audit.finished_at,
                    audit.dat_revision_sha256,
                    audit.dat_revision_version,
                    counts.join(" "),
                    audit.evidence.as_str(),
                    if audit.pruned { "summary" } else { "full" }
                );
            }
            Ok(())
        }
    }
}

/// "1 cue sheet", "2 cue sheets".
fn cue_sheets(n: u64) -> String {
    match n {
        1 => "1 cue sheet".into(),
        n => format!("{n} cue sheets"),
    }
}
