//! Shadow runs in a child process: the other engine runs in its own process group under a memory limit and a
//! deadline, and is killed and reaped when it overruns, so a runaway shadow run can't take the Service down with it.
//! Whatever goes wrong becomes the shadow run's error; it never reaches the Selection it is recorded beside.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use title_select_api::selection::{Selection, SelectionEngine, SelectionProfile, ShadowRun};

use crate::{Engine, Error, RunOptions, compare};

/// The hidden subcommand of `title-select` that runs one engine for a shadow run.
pub const SHADOW_SUBCOMMAND: &str = "selection-shadow";

/// The most address space a shadow run may use, by default.
pub const DEFAULT_SHADOW_MEMORY: u64 = 8 << 30;

/// How long past its own deadline a shadow run's child has to finish before it's killed.
const GRACE: Duration = Duration::from_secs(5);

/// How to start the child that runs a shadow run: `program` with `args`, then the request and response paths.
#[derive(Debug, Clone)]
pub struct ShadowCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
}

impl ShadowCommand {
    /// This binary's hidden `selection-shadow` subcommand.
    pub fn current_exe() -> Result<ShadowCommand, Error> {
        let program = std::env::current_exe().map_err(|source| Error::Io {
            context: "the running binary".into(),
            source,
        })?;
        Ok(ShadowCommand {
            program,
            args: vec![SHADOW_SUBCOMMAND.into()],
            envs: Vec::new(),
        })
    }
}

/// What the child runs.
#[derive(Serialize, Deserialize)]
struct Request {
    root: PathBuf,
    data: PathBuf,
    dat: PathBuf,
    profile: SelectionProfile,
    engine: SelectionEngine,
    work_dir: PathBuf,
    timeout_ms: u64,
}

/// What the child made.
#[derive(Serialize, Deserialize)]
enum Response {
    Ran {
        selection: Box<Selection>,
        warnings: Vec<String>,
    },
    Failed(String),
}

/// The child's side: runs the request at `request` and writes the response to `response`. Fails only when it can't
/// read the request or write the response.
pub fn serve_shadow(request: &Path, response: &Path) -> Result<(), Error> {
    let io = |path: &Path| {
        let context = path.display().to_string();
        move |source| Error::Io { context, source }
    };
    let bytes = std::fs::read(request).map_err(io(request))?;
    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Install(format!("{}: {e}", request.display())))?;
    let ran = Engine::open(&request.root).and_then(|engine| {
        let engine = engine.with_data_dir(&request.data);
        let mut options = RunOptions::new(&request.work_dir);
        options.engine = request.engine;
        options.timeout = Duration::from_millis(request.timeout_ms);
        engine.run(&request.dat, &request.profile, &options)
    });
    let answer = match ran {
        Ok(outcome) => Response::Ran {
            selection: Box::new(outcome.selection),
            warnings: outcome.warnings,
        },
        Err(e) => Response::Failed(e.to_string()),
    };
    let bytes = serde_json::to_vec(&answer).expect("serializable");
    std::fs::write(response, bytes).map_err(io(response))
}

impl Engine {
    /// Runs the engine other than `options.engine` on `dat` with `profile` in a child process, and compares its
    /// Selection with `selection`, which `options.engine` made with `warnings`. The child gets `options.timeout`
    /// and, where the system enforces it, `options.shadow_memory` bytes of address space.
    pub fn shadow(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
        selection: &Selection,
        warnings: &[String],
    ) -> ShadowRun {
        let other = options.engine.other();
        let (comparison, error) = match self.run_child(other, dat, profile, options) {
            Ok((theirs, their_warnings)) => {
                let (bundled, builtin) = match options.engine {
                    SelectionEngine::Bundled => {
                        ((selection, warnings), (&theirs, &their_warnings[..]))
                    }
                    SelectionEngine::Builtin => {
                        ((&theirs, &their_warnings[..]), (selection, warnings))
                    }
                };
                (
                    Some(compare(bundled.0, builtin.0, bundled.1, builtin.1)),
                    None,
                )
            }
            Err(message) => (None, Some(message)),
        };
        ShadowRun::new(options.engine, other, comparison, error)
    }

    fn run_child(
        &self,
        engine: SelectionEngine,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
    ) -> Result<(Selection, Vec<String>), String> {
        let command = match &self.shadow_command {
            Some(command) => command.clone(),
            None => ShadowCommand::current_exe().map_err(|e| e.to_string())?,
        };
        let work_dir = std::path::absolute(&options.work_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&work_dir).map_err(|e| format!("{}: {e}", work_dir.display()))?;
        let scratch = tempfile::Builder::new()
            .prefix("shadow-")
            .tempdir_in(&work_dir)
            .map_err(|e| format!("{}: {e}", work_dir.display()))?;
        let request = scratch.path().join("request.json");
        let response = scratch.path().join("response.json");
        let stderr = scratch.path().join("stderr.txt");
        let dat = std::path::absolute(dat).map_err(|e| e.to_string())?;
        let body = Request {
            root: self.root.clone(),
            data: self.data.clone(),
            dat,
            profile: profile.clone(),
            engine,
            work_dir,
            timeout_ms: options.timeout.as_millis().try_into().unwrap_or(u64::MAX),
        };
        std::fs::write(&request, serde_json::to_vec(&body).expect("serializable"))
            .map_err(|e| format!("{}: {e}", request.display()))?;
        let mut child = Command::new(&command.program);
        child
            .args(&command.args)
            .arg(&request)
            .arg(&response)
            .envs(command.envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(
                std::fs::File::create(&stderr).map_err(|e| format!("{}: {e}", stderr.display()))?,
            );
        limit(&mut child, options.shadow_memory);
        let child = child
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", command.program.display()))?;
        // The child stops itself at the deadline; this one is for a child that can't.
        let status = wait(child, options.timeout + GRACE)?;
        match std::fs::read(&response) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(Response::Ran {
                    selection,
                    warnings,
                }) => Ok((*selection, warnings)),
                Ok(Response::Failed(message)) => Err(message),
                Err(e) => Err(format!("its result is unreadable: {e}")),
            },
            Err(_) => {
                let said = std::fs::read_to_string(&stderr).unwrap_or_default();
                let last = said.lines().rev().find(|l| !l.trim().is_empty());
                Err(match last {
                    Some(line) => {
                        format!("it stopped without a result ({status}): {}", line.trim())
                    }
                    None => format!("it stopped without a result ({status})"),
                })
            }
        }
    }
}

/// Waits for `child` until `deadline` has passed, then kills its process group, while the unreaped child still holds
/// the group's id, and reaps it.
fn wait(mut child: Child, deadline: Duration) -> Result<std::process::ExitStatus, String> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() >= deadline => {
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "it didn't finish within {} s, so it was stopped",
                    deadline.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot wait for it: {e}"));
            }
        }
    }
}

#[cfg(unix)]
fn limit(command: &mut Command, memory: Option<u64>) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs in the forked child before exec, and only makes system calls, which are
    // async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            rustix::process::setsid()?;
            if let Some(bytes) = memory {
                let limit = rustix::process::Rlimit {
                    current: Some(bytes),
                    maximum: Some(bytes),
                };
                let set = rustix::process::setrlimit(rustix::process::Resource::As, limit);
                // macOS doesn't enforce an address-space limit; only Linux must.
                if cfg!(target_os = "linux") {
                    set?;
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn limit(_: &mut Command, _: Option<u64>) {}

#[cfg(unix)]
fn kill_group(child: &Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_: &Child) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::tests::{engine, upstream};

    const CHILD: &str = "TITLE_SELECT_TEST_SHADOW_CHILD";

    fn fixture() -> PathBuf {
        upstream().join("tests/source/features/Retool - Prefer regions.dat")
    }

    /// A child that runs `sh -c script`, with the request path as `$1`.
    fn shell(script: &str) -> ShadowCommand {
        ShadowCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into(), "sh".into()],
            envs: Vec::new(),
        }
    }

    fn shadow_with(command: ShadowCommand, options: &RunOptions) -> ShadowRun {
        let selection = Selection {
            pins: engine().pins.clone(),
            dat_sha256: String::new(),
            system_settings: false,
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups: Default::default(),
            stale_choices: Default::default(),
            system: String::new(),
        };
        engine().with_shadow_command(command).shadow(
            &fixture(),
            &SelectionProfile::default(),
            options,
            &selection,
            &[],
        )
    }

    /// The child side of `a_shadow_run_compares_the_other_engine_run_in_a_child_process`: this test binary, re-run.
    #[test]
    fn shadow_child() {
        if std::env::var_os(CHILD).is_some() {
            let args: Vec<String> = std::env::args().collect();
            let [.., request, response] = &args[..] else {
                panic!("{args:?}")
            };
            serve_shadow(Path::new(request), Path::new(response)).unwrap();
        }
    }

    #[test]
    fn a_shadow_run_compares_the_other_engine_run_in_a_child_process() {
        let work = tempfile::tempdir().unwrap();
        let options = RunOptions::new(work.path());
        let ran = engine()
            .run(&fixture(), &SelectionProfile::default(), &options)
            .unwrap();
        let command = ShadowCommand {
            program: std::env::current_exe().unwrap(),
            args: vec![
                "--exact".into(),
                "isolated::tests::shadow_child".into(),
                "--quiet".into(),
            ],
            envs: vec![(CHILD.into(), "1".into())],
        };
        let shadow = engine().with_shadow_command(command).shadow(
            &fixture(),
            &SelectionProfile::default(),
            &options,
            &ran.selection,
            &ran.warnings,
        );
        assert_eq!(shadow.error, None);
        assert_eq!(shadow.shadow, SelectionEngine::Builtin);
        assert!(shadow.comparison.unwrap().is_identical());
    }

    #[test]
    fn a_shadow_run_that_overruns_is_killed_with_what_it_started() {
        let work = tempfile::tempdir().unwrap();
        let mut options = RunOptions::new(work.path());
        options.timeout = Duration::from_secs(1);
        let pid_file = work.path().join("grandchild");
        let started = Instant::now();
        let shadow = shadow_with(
            shell(&format!(
                "sleep 300 & echo $! > '{}'; wait",
                pid_file.display()
            )),
            &options,
        );
        assert!(started.elapsed() < Duration::from_secs(20));
        let error = shadow.error.unwrap();
        assert!(error.contains("didn't finish within 6 s"), "{error}");
        assert_eq!(shadow.comparison, None);
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let gone = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            rustix::process::test_kill_process(pid).is_err()
        });
        assert!(gone, "the shadow run's own child is still running");
    }

    #[test]
    fn a_shadow_run_that_fails_records_why() {
        let work = tempfile::tempdir().unwrap();
        let options = RunOptions::new(work.path());
        let shadow = shadow_with(shell("echo 'out of luck' >&2; exit 3"), &options);
        let error = shadow.error.unwrap();
        assert!(
            error.contains("without a result") && error.contains("out of luck"),
            "{error}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_shadow_run_that_exhausts_its_memory_records_why() {
        let work = tempfile::tempdir().unwrap();
        let mut options = RunOptions::new(work.path());
        options.shadow_memory = Some(1 << 30);
        let python = engine().python();
        let shadow = shadow_with(
            shell(&format!(
                "exec '{}' -c 'bytearray(8 << 30)'",
                python.display()
            )),
            &options,
        );
        let error = shadow.error.unwrap();
        assert!(error.contains("MemoryError"), "{error}");
    }
}
