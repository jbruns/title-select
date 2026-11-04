//! Shadow runs in a child process: the other engine runs in its own process group under a memory limit and a
//! deadline, and is killed and reaped when it overruns, so a runaway shadow run can't take the Service down with it.
//! Whatever goes wrong becomes the shadow run's error; it never reaches the Selection it is recorded beside.
//!
//! The parent reads what the child returns through bounded readers, keeps the deadline until the comparison is
//! done, and kills and reaps every child it still runs when the Service stops (`Engine::stop_shadows`). A child whose
//! parent dies unexpectedly goes too: on Linux the kernel kills it (`PR_SET_PDEATHSIG`); everywhere, it watches the
//! pipe on its stdin, whose far end only the parent holds, and kills its own process group at end of file.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use title_select_api::selection::{Selection, SelectionEngine, SelectionProfile, ShadowRun};

use crate::compare::{Budget, compare_within};
use crate::{Engine, Error, RunOptions};

/// The hidden subcommand of `title-select` that runs one engine for a shadow run.
pub const SHADOW_SUBCOMMAND: &str = "selection-shadow";

/// The most address space a shadow run may use, by default.
pub const DEFAULT_SHADOW_MEMORY: u64 = 8 << 30;

/// The largest result a shadow run's child may return, by default. The largest fixture DAT's Selection, Sony -
/// PlayStation 2, serializes to 2.9 MB for 11,374 releases, about 260 bytes each; 64 MiB holds about 250,000
/// releases, several times the largest DATs the Service imports.
pub const DEFAULT_SHADOW_RESULT: u64 = 64 << 20;

/// How much of the end of a child's stderr a failed shadow run reads for its error.
const DIAGNOSTICS: u64 = 4 << 20;

/// The most steps a shadow run's comparison may take: one per group and one per release either engine placed, so
/// several times a result of `DEFAULT_SHADOW_RESULT`.
const COMPARE_WORK: u64 = 4_000_000;

/// Why a shadow run that the Service stopped has no comparison.
const STOPPING: &str = "it was stopped because the service is stopping";

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
    watch_parent();
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
        // The child stops itself at its timeout; the grace is for one that can't, and for reading and comparing.
        let allowed = options.timeout + GRACE;
        let deadline = Instant::now() + allowed;
        let (comparison, error) = match self.run_child(other, dat, profile, options, deadline) {
            Ok((theirs, their_warnings)) => {
                let (bundled, builtin) = match options.engine {
                    SelectionEngine::Bundled => {
                        ((selection, warnings), (&theirs, &their_warnings[..]))
                    }
                    SelectionEngine::Builtin => {
                        ((&theirs, &their_warnings[..]), (selection, warnings))
                    }
                };
                let budget = Budget {
                    deadline: Some(deadline),
                    work: COMPARE_WORK,
                };
                match compare_within(bundled.0, builtin.0, bundled.1, builtin.1, budget) {
                    Ok(comparison) => (Some(comparison), None),
                    Err(message) => (None, Some(message)),
                }
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
        deadline: Instant,
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
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(
                std::fs::File::create(&stderr).map_err(|e| format!("{}: {e}", stderr.display()))?,
            );
        limit(&mut child, options.shadow_memory);
        let (child, parent_alive) = self.shadows.spawn(&mut child).map_err(|e| match e {
            Spawn::Stopping => STOPPING.to_owned(),
            Spawn::Failed(e) => format!("cannot start {}: {e}", command.program.display()),
        })?;
        let status = self.shadows.wait(child, deadline, options.timeout + GRACE);
        drop(parent_alive);
        let status = status?;
        let bytes = match read_capped(&response, options.shadow_result)? {
            Some(bytes) => bytes,
            None => {
                let said = tail(&stderr, DIAGNOSTICS);
                let last = said.lines().rev().find(|l| !l.trim().is_empty());
                return Err(match last {
                    Some(line) => {
                        format!("it stopped without a result ({status}): {}", line.trim())
                    }
                    None => format!("it stopped without a result ({status})"),
                });
            }
        };
        if Instant::now() >= deadline {
            return Err("reading its result didn't finish in time".to_owned());
        }
        match serde_json::from_slice(&bytes) {
            Ok(Response::Ran {
                selection,
                warnings,
            }) => Ok((*selection, warnings)),
            Ok(Response::Failed(message)) => Err(message),
            Err(e) => Err(format!("its result is unreadable: {e}")),
        }
    }

    /// Kills every shadow run's child, with what it started, and waits up to `within` for them to be reaped. No
    /// shadow run starts after. Returns whether every child was reaped in time.
    pub fn stop_shadows(&self, within: Duration) -> bool {
        self.shadows.stop(within)
    }
}

/// The children shadow runs have started and not yet reaped. A child's id stays here until it is reaped, under the
/// same lock, so killing the process group of an id read here never reaches a process that reused it.
#[derive(Debug, Default)]
pub(crate) struct Shadows {
    running: Mutex<Running>,
    reaped: Condvar,
}

#[derive(Debug, Default)]
struct Running {
    stopping: bool,
    children: BTreeSet<u32>,
}

enum Spawn {
    Stopping,
    Failed(std::io::Error),
}

impl Shadows {
    fn lock(&self) -> std::sync::MutexGuard<'_, Running> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts `command` unless the Service is stopping, and takes the far end of its stdin pipe, which the child
    /// watches to learn that its parent has gone.
    fn spawn(
        &self,
        command: &mut Command,
    ) -> Result<(Child, Option<std::process::ChildStdin>), Spawn> {
        let mut running = self.lock();
        if running.stopping {
            return Err(Spawn::Stopping);
        }
        let mut child = command.spawn().map_err(Spawn::Failed)?;
        running.children.insert(child.id());
        let stdin = child.stdin.take();
        Ok((child, stdin))
    }

    /// Waits for `child` until `deadline` has passed, then kills its process group, while the unreaped child still
    /// holds the group's id, and reaps it. `allowed` is how long it had, for the error.
    fn wait(
        &self,
        mut child: Child,
        deadline: Instant,
        allowed: Duration,
    ) -> Result<std::process::ExitStatus, String> {
        loop {
            let mut running = self.lock();
            let ended = match child.try_wait() {
                Ok(Some(status)) => Some(Ok(status)),
                Ok(None) if Instant::now() >= deadline => {
                    stop(&mut child);
                    Some(Err(format!(
                        "it didn't finish within {} s, so it was stopped",
                        allowed.as_secs()
                    )))
                }
                Ok(None) => None,
                Err(e) => {
                    stop(&mut child);
                    Some(Err(format!("cannot wait for it: {e}")))
                }
            };
            if let Some(ended) = ended {
                running.children.remove(&child.id());
                let stopping = running.stopping;
                drop(running);
                self.reaped.notify_all();
                return if stopping {
                    Err(STOPPING.to_owned())
                } else {
                    ended
                };
            }
            drop(running);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&self, within: Duration) -> bool {
        let mut running = self.lock();
        running.stopping = true;
        for &id in &running.children {
            kill_group(id);
        }
        let (running, _) = self
            .reaped
            .wait_timeout_while(running, within, |r| !r.children.is_empty())
            .unwrap_or_else(|p| p.into_inner());
        running.children.is_empty()
    }
}

/// Kills `child`'s process group and `child`, and reaps it.
fn stop(child: &mut Child) {
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// The bytes of the file at `path`, if there is one, refused unread when it holds more than `cap`.
fn read_capped(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, String> {
    let too_large = |len: u64| {
        format!(
            "its result is larger than the {} MiB a shadow run may return ({len} bytes or more)",
            cap >> 20
        )
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("its result is unreadable: {e}")),
    };
    let len = file
        .metadata()
        .map_err(|e| format!("its result is unreadable: {e}"))?
        .len();
    if len > cap {
        return Err(too_large(len));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("its result is unreadable: {e}"))?;
    if bytes.len() as u64 > cap {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(Some(bytes))
}

/// At most the last `cap` bytes of the file at `path`, as text.
fn tail(path: &Path, cap: u64) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(len.saturating_sub(cap))).is_ok() {
        let _ = file.take(cap).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// In a shadow run's child: once the pipe on stdin reaches end of file, because the parent closed it or died, kills
/// this process's group, which holds whatever it started, and exits. Does nothing unless stdin is a pipe and this
/// process leads its group, as `Engine::shadow` starts it.
fn watch_parent() {
    #[cfg(unix)]
    {
        let piped = rustix::fs::fstat(std::io::stdin()).is_ok_and(|s| {
            rustix::fs::FileType::from_raw_mode(s.st_mode) == rustix::fs::FileType::Fifo
        });
        if !piped || rustix::process::getpgrp() != rustix::process::getpid() {
            return;
        }
        std::thread::spawn(|| {
            let mut buf = [0; 64];
            while matches!(std::io::stdin().read(&mut buf), Ok(n) if n > 0) {}
            let _ = rustix::process::kill_current_process_group(rustix::process::Signal::KILL);
            std::process::exit(1);
        });
    }
}

#[cfg(unix)]
fn limit(command: &mut Command, memory: Option<u64>) {
    use std::os::unix::process::CommandExt;
    die_with_parent(command);
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

/// On Linux, has the kernel kill `command`'s process when the thread that started it ends.
pub(crate) fn die_with_parent(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = rustix::process::getpid();
        // SAFETY: the closure runs in the forked child before exec, and only makes system calls, which are
        // async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ))?;
                if rustix::process::getppid() != Some(parent) {
                    return Err(std::io::Error::other("the parent has gone"));
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

#[cfg(not(unix))]
fn limit(_: &mut Command, _: Option<u64>) {}

#[cfg(unix)]
fn kill_group(id: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(id as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::tests::{engine, upstream};

    const CHILD: &str = "TITLE_SELECT_TEST_SHADOW_CHILD";
    const WATCHING: &str = "TITLE_SELECT_TEST_SHADOW_WATCHING";

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
        shadow_on(&engine().with_shadow_command(command), options)
    }

    fn shadow_on(engine: &Engine, options: &RunOptions) -> ShadowRun {
        let selection = Selection {
            pins: engine.pins.clone(),
            dat_sha256: String::new(),
            system_settings: false,
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups: Default::default(),
            stale_choices: Default::default(),
            system: String::new(),
        };
        engine.shadow(
            &fixture(),
            &SelectionProfile::default(),
            options,
            &selection,
            &[],
        )
    }

    /// Waits up to 20 s for `path` to hold a process id, and returns it.
    fn pid_in(path: &Path) -> rustix::process::Pid {
        for _ in 0..1000 {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse()
            {
                return rustix::process::Pid::from_raw(pid).unwrap();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{} never held a process id", path.display())
    }

    /// Whether `pid` is gone within 5 s.
    fn gone(pid: rustix::process::Pid) -> bool {
        (0..250).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            rustix::process::test_kill_process(pid).is_err()
        })
    }

    /// The child side of `a_shadow_run_compares_the_other_engine_run_in_a_child_process`: this test binary, re-run.
    #[test]
    fn shadow_child() {
        if std::env::var_os(WATCHING).is_some() {
            watch_parent();
            let args: Vec<String> = std::env::args().collect();
            let pid_file = &args[args.len() - 1];
            let mut grandchild = Command::new("sleep").arg("300").spawn().unwrap();
            std::fs::write(pid_file, grandchild.id().to_string()).unwrap();
            let _ = grandchild.wait();
            panic!("the child outlived its parent");
        }
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

    #[test]
    fn a_shadow_result_larger_than_its_limit_is_refused_unread() {
        let work = tempfile::tempdir().unwrap();
        let mut options = RunOptions::new(work.path());
        options.shadow_result = 1 << 20;
        let shadow = shadow_with(shell("head -c 2000000 /dev/zero > \"$2\""), &options);
        let error = shadow.error.unwrap();
        assert!(
            error.contains("larger than the 1 MiB a shadow run may return"),
            "{error}"
        );
        assert_eq!(shadow.comparison, None);
    }

    #[test]
    fn a_failed_shadow_run_reads_only_the_end_of_what_it_said() {
        let work = tempfile::tempdir().unwrap();
        let said = work.path().join("said");
        std::fs::write(&said, format!("{}\nthe last word\n", "x".repeat(100))).unwrap();
        assert_eq!(tail(&said, 14), "the last word\n");
        let options = RunOptions::new(work.path());
        let shadow = shadow_with(
            shell(
                "head -c 20000000 /dev/zero | tr '\\0' 'x' >&2; echo >&2; echo 'out of luck' >&2; exit 3",
            ),
            &options,
        );
        let error = shadow.error.unwrap();
        assert!(error.ends_with("out of luck"), "{}", &error[..100]);
    }

    #[test]
    fn stopping_shadow_runs_kills_and_reaps_them_and_refuses_more() {
        let work = tempfile::tempdir().unwrap();
        let pid_file = work.path().join("grandchild");
        let engine = engine().with_shadow_command(shell(&format!(
            "sleep 300 & echo $! > '{}'; wait",
            pid_file.display()
        )));
        let options = RunOptions::new(work.path());
        let running = std::thread::spawn({
            let (engine, options) = (engine.clone(), options.clone());
            move || shadow_on(&engine, &options)
        });
        let grandchild = pid_in(&pid_file);
        let started = Instant::now();
        assert!(engine.stop_shadows(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_secs(10));
        let shadow = running.join().unwrap();
        assert_eq!(shadow.error.as_deref(), Some(STOPPING));
        assert!(
            gone(grandchild),
            "the shadow run's own child is still running"
        );
        assert_eq!(
            shadow_on(&engine, &options).error.as_deref(),
            Some(STOPPING)
        );
    }

    #[test]
    fn a_shadow_child_goes_with_what_it_started_when_its_parent_goes() {
        use std::os::unix::process::CommandExt;
        let work = tempfile::tempdir().unwrap();
        let pid_file = work.path().join("grandchild");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "isolated::tests::shadow_child", "--quiet"])
            .arg(&pid_file)
            .env(WATCHING, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        // SAFETY: only makes a system call in the forked child.
        unsafe {
            command.pre_exec(|| rustix::process::setsid().map(drop).map_err(Into::into));
        }
        let mut child = command.spawn().unwrap();
        let grandchild = pid_in(&pid_file);
        // The parent going closes the far end of the pipe, as this does.
        drop(child.stdin.take());
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the child is still running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(grandchild), "what the child started is still running");
    }
}
