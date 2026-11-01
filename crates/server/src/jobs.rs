//! Runs queued Jobs on blocking threads, at most `[jobs] concurrent` at once, and broadcasts their progress and states.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use title_select_api::{Job, JobEvent, JobKind, JobProgress};
use title_select_engine::Progress;
use title_select_selection::Engine;
use title_select_store::Store;
use title_select_store::config::Config;
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

    /// Announces a Job's new state, and wakes the runner in case it was queued.
    pub fn changed(&self, job: Job) {
        let queued = job.state == title_select_api::JobState::Queued;
        self.publish(JobEvent::State(Box::new(job)));
        if queued {
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
                    JobKind::Audit | JobKind::Build | JobKind::Check => {
                        run_one(&store, &config, &jobs, kind, id, version)
                    }
                    JobKind::Selection => run_selection(&store, &config, engine.as_deref(), id),
                    JobKind::IngestScan => run_ingest_scan(&store, &config, id),
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
                Ok(Ok(queued)) => queued.into_iter().for_each(|job| jobs.changed(job)),
                Ok(Err(err)) => eprintln!("title-select: cannot queue an owed audit: {err}"),
                Err(err) => eprintln!("title-select: cannot queue an owed audit: {err}"),
            }
            drop(slot);
            jobs.wake.notify_one();
        });
    }
}

/// Runs one claimed Audit, Build or Check Job, publishing its progress, and returns the Job as it ended.
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
            None,
        )
        .map_err(|e| e.to_string())
    })();
    let ended = match made {
        Ok(selection) => store
            .record_selection(config, id, &selection)
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
