//! Runs queued Jobs on blocking threads, at most `[jobs] concurrent` at once, and broadcasts their progress and states.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use title_select_api::fetch::{
    FetchArtifact, FetchOutcome, FetchProvider, FetchReport, FetchTrigger,
};
use title_select_api::{Job, JobEvent, JobKind, JobProgress};
use title_select_engine::Progress;
use title_select_selection::Engine;
use title_select_store::Store;
use title_select_store::config::Config;
use title_select_store::fetch::Fetcher;
use tokio::sync::{Notify, Semaphore, broadcast};

/// The least time between two progress events of one Job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How long the runner waits after the store fails to hand it a Job.
const RETRY: Duration = Duration::from_secs(1);

/// Wakes the runner and carries every Job's events to subscribers.
pub struct Jobs {
    wake: Notify,
    events: broadcast::Sender<JobEvent>,
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs {
            wake: Notify::new(),
            events: broadcast::channel(1024).0,
        }
    }

    /// Announces a Job's new state, and wakes the runner in case it can claim one now: this one, Queued, or one
    /// that waited for this one to end, as a stat walk waits for a Repair or Build of its Source Set.
    pub fn changed(&self, job: Job) {
        let wake = job.state == title_select_api::JobState::Queued || job.state.is_final();
        self.publish(JobEvent::State(Box::new(job)));
        if wake {
            self.wake.notify_one();
        }
    }

    pub fn publish(&self, event: JobEvent) {
        let _ = self.events.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.events.subscribe()
    }
}

/// Claims and runs Queued Jobs forever.
pub async fn run(
    store: Arc<Store>,
    config: Arc<Config>,
    jobs: Arc<Jobs>,
    engine: Option<Arc<Engine>>,
) {
    let slots = Arc::new(Semaphore::new(config.jobs.concurrent));
    loop {
        let slot = slots.clone().acquire_owned().await.expect("never closed");
        let claimed = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.claim_job()).await
        };
        let job = match claimed {
            Ok(Ok(Some(job))) => job,
            Ok(Ok(None)) => {
                drop(slot);
                jobs.wake.notified().await;
                continue;
            }
            Ok(Err(err)) => {
                eprintln!("title-select: cannot claim a job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
            Err(err) => {
                eprintln!("title-select: cannot claim a job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        eprintln!(
            "title-select: job {} started: {} of {}",
            job.id,
            job.kind.as_str(),
            job.ingest_folder.as_deref().unwrap_or(&job.source_set)
        );
        jobs.changed(job.clone());
        let (store, config, jobs, engine) =
            (store.clone(), config.clone(), jobs.clone(), engine.clone());
        tokio::spawn(async move {
            let (id, version, kind) = (job.id, job.version, job.kind);
            let ended = {
                let (store, jobs) = (store.clone(), jobs.clone());
                tokio::task::spawn_blocking(move || match kind {
                    JobKind::Audit | JobKind::Build | JobKind::Check | JobKind::Repair => {
                        run_one(&store, &config, &jobs, kind, id, version)
                    }
                    JobKind::Additions => run_additions(&store, &config, id),
                    JobKind::Selection => run_selection(&store, &config, engine.as_deref(), id),
                    JobKind::IngestScan => run_ingest_scan(&store, &config, id),
                    JobKind::StatWalk => run_stat_walk(&store, &config, id),
                })
                .await
            };
            let job = match ended {
                Ok(job) => job,
                Err(err) => {
                    let store = store.clone();
                    let message = format!("the {} stopped unexpectedly: {err}", kind.as_str());
                    tokio::task::spawn_blocking(move || store.fail_job(id, &message).ok())
                        .await
                        .ok()
                        .flatten()
                }
            };
            if let Some(job) = job {
                eprintln!("title-select: job {} {}", job.id, job.state.as_str());
                jobs.changed(job);
            }
            // An Audit a DAT imported from an Ingest Folder owes waits for the Job it met to end.
            let owed = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.fulfil_ingest_reaudits()).await
            };
            match owed {
                Ok(Ok(owed)) => {
                    for why in owed.failed {
                        eprintln!("title-select: cannot queue an owed audit: {why}");
                    }
                    owed.queued.into_iter().for_each(|job| jobs.changed(job));
                }
                Ok(Err(err)) => eprintln!("title-select: cannot queue an owed audit: {err}"),
                Err(err) => eprintln!("title-select: cannot queue an owed audit: {err}"),
            }
            // An Audit a finished repair owes waits for the Job it met to end too.
            let owed = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.fulfil_repair_audits()).await
            };
            match owed {
                Ok(Ok(owed)) => {
                    for why in owed.failed {
                        eprintln!("title-select: cannot queue an owed audit: {why}");
                    }
                    owed.queued.into_iter().for_each(|job| jobs.changed(job));
                }
                Ok(Err(err)) => eprintln!("title-select: cannot queue an owed audit: {err}"),
                Err(err) => eprintln!("title-select: cannot queue an owed audit: {err}"),
            }
            // Additions Jobs a Repair or Build Job queued as it ended publish what it kept aside.
            let queued = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.queued_additions_jobs()).await
            };
            if let Ok(Ok(queued)) = queued {
                queued.into_iter().for_each(|job| jobs.changed(job));
            }
            drop(slot);
            jobs.wake.notify_one();
        });
    }
}

/// Runs one claimed Audit, Build, Check or Repair Job, publishing its progress, and returns the Job as it ended.
fn run_one(
    store: &Store,
    config: &Config,
    jobs: &Jobs,
    kind: JobKind,
    id: i64,
    version: u64,
) -> Option<Job> {
    let last = Mutex::new(None::<Instant>);
    let progress = |p: Progress| {
        let mut last = last.lock().unwrap();
        let done = p.files_done == p.files_total;
        if done || last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
            *last = Some(Instant::now());
            jobs.publish(JobEvent::Progress(JobProgress {
                job: id,
                version,
                files_done: p.files_done,
                files_total: p.files_total,
            }));
        }
    };
    let ran = match kind {
        JobKind::Build => store.run_build_job(config, id, Some(&progress)),
        JobKind::Check => store.run_check_job(config, id, Some(&progress)),
        JobKind::Repair => store.run_repair_job(config, id, Some(&progress)),
        _ => store.run_audit_job(config, id, Some(&progress)),
    };
    match ran {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// Runs one claimed Additions Job, and returns the Job as it ended.
fn run_additions(store: &Store, config: &Config, id: i64) -> Option<Job> {
    match store.run_additions_job(config, id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// Runs one claimed scan of an Ingest Folder, and returns the Job as it ended.
fn run_ingest_scan(store: &Store, config: &Config, id: i64) -> Option<Job> {
    match store.run_ingest_scan_job(config, id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// Runs one claimed stat walk of a Source Set, and returns the Job as it ended.
fn run_stat_walk(store: &Store, config: &Config, id: i64) -> Option<Job> {
    match store.run_stat_walk_job(config, id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// How often the schedule looks for Source Sets due a stat walk.
const CHECK_DUE_EVERY: Duration = Duration::from_secs(60);

/// Queues a stat walk of each audited Source Set last checked, or else last audited, more than `check_every_minutes`
/// ago, looking every minute from now; none when it is 0. A walk still queued or running is left to finish.
pub async fn schedule_stat_walks(store: Arc<Store>, config: Arc<Config>, jobs: Arc<Jobs>) {
    let minutes = config.source_sets.check_every_minutes;
    if minutes == 0 {
        return;
    }
    let every = Duration::from_secs(minutes * 60);
    let mut tick = tokio::time::interval(CHECK_DUE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let store = store.clone();
        let queued =
            tokio::task::spawn_blocking(move || store.enqueue_stat_walks("schedule", every)).await;
        match queued {
            Ok(Ok(queued)) => queued.into_iter().for_each(|job| jobs.changed(job)),
            Ok(Err(err)) => eprintln!("title-select: cannot queue scheduled checks: {err}"),
            Err(err) => eprintln!("title-select: cannot queue scheduled checks: {err}"),
        }
    }
}

/// How often the schedule looks for fetches due, from a minute after the Service starts.
const FETCH_DUE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Fetches, from each DAT Provider you turned on, what is due for the systems your Source Sets Follow: each at most
/// once a day (ADR 0012). A fetch runs alongside Jobs, and never queues one.
pub async fn schedule_fetches(store: Arc<Store>, config: Arc<Config>, fetcher: Arc<Fetcher>) {
    let providers: Vec<_> = FetchProvider::ALL
        .into_iter()
        .filter(|p| config.fetch.enabled(*p))
        .collect();
    if providers.is_empty() {
        return;
    }
    let start = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut tick = tokio::time::interval_at(start, FETCH_DUE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        for &provider in &providers {
            let (store, config, fetcher) = (store.clone(), config.clone(), fetcher.clone());
            let fetched = tokio::task::spawn_blocking(move || {
                log_fetch(
                    provider,
                    store.fetch(
                        &config,
                        &fetcher,
                        provider,
                        FetchTrigger::Schedule,
                        "schedule",
                    ),
                )
            })
            .await;
            match fetched {
                Ok(Ok(_)) | Ok(Err(title_select_store::Error::Conflict(_))) => {}
                Ok(Err(err)) => eprintln!(
                    "title-select: cannot fetch from {}: {err}",
                    provider.dat_provider()
                ),
                Err(err) => eprintln!(
                    "title-select: cannot fetch from {}: {err}",
                    provider.dat_provider()
                ),
            }
        }
    }
}

/// Logs what a fetch brought, and where it stopped if it did.
pub fn log_fetch(
    provider: FetchProvider,
    fetched: Result<FetchReport, title_select_store::Error>,
) -> Result<FetchReport, title_select_store::Error> {
    if let Ok(report) = &fetched {
        let name = provider.dat_provider();
        for attempt in &report.attempts {
            if attempt.outcome == Some(FetchOutcome::Added) {
                let what = match attempt.artifact {
                    FetchArtifact::Release => "release",
                    FetchArtifact::Dat => "DAT revision",
                    FetchArtifact::Cues => "cue pack",
                    FetchArtifact::Sbi => "SBI pack",
                };
                let version = attempt.version.as_deref().or(attempt.release.as_deref());
                let version = version.map_or_else(String::new, |v| format!(" ({v})"));
                eprintln!(
                    "title-select: fetched a new {what} of {}{version} from {name}",
                    attempt.system
                );
            }
        }
        if let Some(why) = &report.stopped {
            eprintln!("title-select: a fetch from {name} stopped, and kept what it had: {why}");
        }
    }
    fetched
}

/// Queues a scan of each Ingest Folder with a schedule, every `scan_every_minutes`. A scan still queued or running
/// is left to finish.
pub async fn schedule_ingest_scans(store: Arc<Store>, config: Arc<Config>, jobs: Arc<Jobs>) {
    for (root, settings) in &config.roots {
        let Some(minutes) = settings.scan_every_minutes.filter(|m| *m > 0) else {
            continue;
        };
        if !settings.is_ingest() {
            continue;
        }
        let (store, jobs, root) = (store.clone(), jobs.clone(), root.clone());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(minutes * 60));
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                let (store, root) = (store.clone(), root.clone());
                let queued = tokio::task::spawn_blocking(move || {
                    store.enqueue_ingest_scan(&root, "schedule")
                })
                .await;
                match queued {
                    Ok(Ok(job)) => jobs.changed(job),
                    Ok(Err(title_select_store::Error::JobActive { .. }))
                    | Ok(Err(title_select_store::Error::Conflict(_))) => {}
                    Ok(Err(err)) => eprintln!("title-select: cannot queue a scheduled scan: {err}"),
                    Err(err) => eprintln!("title-select: cannot queue a scheduled scan: {err}"),
                }
            }
        });
    }
}

/// Runs one claimed Selection Job: the engine on its DAT Revision with its Profile version. Returns the Job as it
/// ended.
fn run_selection(store: &Store, config: &Config, engine: Option<&Engine>, id: i64) -> Option<Job> {
    let made = (|| {
        let engine = engine.ok_or_else(|| crate::selections::UNAVAILABLE.to_owned())?;
        let input = store.selection_input(id).map_err(|e| e.to_string())?;
        crate::selections::select(
            store,
            engine,
            &input.dat,
            &input.dat_revision.file,
            &input.profile,
            crate::selections::Engines {
                engine: config.selections.engine,
                shadow: config.selections.shadow,
            },
            None,
        )
        .map_err(|e| e.to_string())
    })();
    let ended = match made {
        Ok((selection, shadow)) => store
            .record_selection(config, id, &selection)
            .and_then(|job| {
                if let (Some(shadow), Some(selection)) = (shadow, job.selection) {
                    store.record_shadow(selection, &shadow)?;
                }
                Ok(job)
            })
            .map_err(|e| e.to_string()),
        Err(message) => Err(message),
    };
    match ended {
        Ok(job) => Some(job),
        Err(message) => {
            eprintln!("title-select: job {id}: {message}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &message).ok(),
            }
        }
    }
}
