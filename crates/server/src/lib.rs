//! Title Select's Service: the `/api/v1` JSON API over the store, durable Audit and Selection Jobs, live Selection
//! previews and Operator auth (ADR 0003).
//!
//! The Service owns the store, and its lock, for as long as it runs.

mod auth;
mod jobs;
mod routes;
mod selections;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use title_select_api::JobKind;
use title_select_store::config::Config;
use title_select_store::fetch::{Endpoints, Fetcher};
use title_select_store::{Store, Workers};

pub use auth::TOKEN_ACTOR;
pub use routes::UPLOAD_LIMIT;

/// The environment variable naming the file that holds the Operator token.
pub const TOKEN_FILE_ENV: &str = "TITLE_SELECT_OPERATOR_TOKEN_FILE";

/// Everything the Service needs to start.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The store directory.
    pub store: PathBuf,
    pub config: Config,
    pub listen: SocketAddr,
    /// The file holding the Operator token, if the token is enabled.
    pub token_file: Option<PathBuf>,
    /// The workbench's built bundle, served at `/` (ADR 0005).
    pub ui: Option<PathBuf>,
    /// The selection engine install; by default `$TITLE_SELECT_RETOOL`, else `/opt/retool`. Without one the
    /// Service still starts, and refuses Selections and previews.
    pub retool: Option<PathBuf>,
    /// Leaves queued Jobs queued, so tests can observe them.
    #[doc(hidden)]
    pub hold_jobs: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("refusing to start: {0}")]
    Auth(String),
    #[error(transparent)]
    Store(#[from] title_select_store::Error),
    #[error("cannot listen on {0}: {1}")]
    Listen(SocketAddr, std::io::Error),
    #[error("cannot start the runtime: {0}")]
    Runtime(std::io::Error),
}

/// A Service ready to serve: auth checked, the store open and its interrupted Jobs recovered, the address bound.
struct Prepared {
    listener: std::net::TcpListener,
    app: axum::Router,
    state: routes::AppState,
    hold_jobs: bool,
}

fn prepare(settings: Settings) -> Result<Prepared, StartError> {
    let Settings {
        store,
        config,
        listen,
        token_file,
        ui,
        retool,
        hold_jobs,
    } = settings;
    let engine = match title_select_selection::Engine::locate(retool.as_deref()) {
        Ok(engine) => Some(Arc::new(engine)),
        Err(err) => {
            eprintln!("title-select: selections are unavailable: {err}");
            None
        }
    };
    let auth = auth::Auth::new(&config.auth, token_file.as_deref(), listen)?;
    let store = Store::open(&store)?;
    let workers: Workers = config.jobs.workers();
    for interruption in store.recover_jobs(workers)? {
        let job = &interruption.job;
        match &interruption.requeued {
            Some(again) => eprintln!(
                "title-select: job {} was interrupted by a stop; queued again as job {}",
                job.id, again.id
            ),
            None if job.kind == JobKind::Build => eprintln!(
                "title-select: build job {} was interrupted by a stop; plan again to build the rest",
                job.id
            ),
            None if job.kind == JobKind::Check => eprintln!(
                "title-select: check job {} was interrupted by a stop; check the folder again",
                job.id
            ),
            None => eprintln!(
                "title-select: job {} was interrupted by a stop again; it failed",
                job.id
            ),
        }
    }
    for (build, reason) in store.recover_builds(&config)? {
        eprintln!(
            "title-select: build {build} stopped and its folder was left as it is: {reason}. Plans for the folder are \
             blocked until it is cleaned up when the service next starts"
        );
    }
    for (deletion, reason) in store.recover_kept_deletions(&config)? {
        eprintln!(
            "title-select: deletion {deletion} of kept files stopped and was not finished: {reason}. Its kept files \
             stay recorded, and plans for the folder are blocked, until it is finished when the service next starts"
        );
    }
    for (id, reason) in store.recover_library_folder_moves(&config)? {
        eprintln!(
            "title-select: move {id} of a play library folder stopped and was not finished: {reason}. The folder \
             stays held until it is settled when the service next starts"
        );
    }
    for (id, reason) in store.recover_ingest_moves(&config)? {
        match id {
            0 => eprintln!("title-select: after the last stop: {reason}"),
            _ => eprintln!(
                "title-select: a move out of an ingest folder stopped and was not finished: {reason}. It is \
                 tried again when the service next starts"
            ),
        }
    }
    for job in store.recover_repairs()? {
        eprintln!(
            "title-select: repair {} was not finished; job {} was queued to finish it",
            job.repair.unwrap_or_default(),
            job.id
        );
    }
    for (deletion, reason) in store.recover_source_kept_deletions(&config)? {
        eprintln!(
            "title-select: deletion {deletion} of kept files of a source set stopped and was not finished: {reason}. \
             It is finished when the service next starts"
        );
    }
    for job in store.recover_additions()? {
        eprintln!(
            "title-select: the additions of {} were not all settled; job {} was queued to settle them",
            job.additions_set.as_deref().unwrap_or_default(),
            job.id
        );
    }
    // Audits owed when the service stopped: queued now, or once the Job they wait for ends.
    for why in store.fulfil_repair_audits()?.failed {
        eprintln!("title-select: cannot queue an owed audit: {why}");
    }
    for why in store.fulfil_ingest_reaudits()?.failed {
        eprintln!("title-select: cannot queue an owed audit: {why}");
    }
    let listener =
        std::net::TcpListener::bind(listen).map_err(|e| StartError::Listen(listen, e))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| StartError::Listen(listen, e))?;
    let state = routes::AppState {
        store: Arc::new(store),
        config: Arc::new(config),
        jobs: Arc::new(jobs::Jobs::new()),
        workers,
        engine,
        previews: Arc::default(),
        fetcher: Arc::new(Fetcher::new(Endpoints::official())),
    };
    let app = routes::router(state.clone(), Arc::new(auth), ui.as_deref());
    Ok(Prepared {
        listener,
        app,
        state,
        hold_jobs,
    })
}

impl Prepared {
    fn addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("a bound listener")
    }

    /// Serves until the process ends; must run inside the runtime.
    async fn serve(self) -> std::io::Result<()> {
        let state = self.state;
        if !self.hold_jobs {
            tokio::spawn(jobs::schedule_ingest_scans(
                state.store.clone(),
                state.config.clone(),
                state.jobs.clone(),
            ));
            tokio::spawn(jobs::schedule_stat_walks(
                state.store.clone(),
                state.config.clone(),
                state.jobs.clone(),
            ));
            tokio::spawn(jobs::schedule_fetches(
                state.store.clone(),
                state.config.clone(),
                state.fetcher.clone(),
            ));
            tokio::spawn(jobs::run(
                state.store,
                state.config,
                state.jobs,
                state.engine,
            ));
        }
        let listener = tokio::net::TcpListener::from_std(self.listener)?;
        axum::serve(
            listener,
            self.app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, StartError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Runtime)
}

/// Runs the Service until SIGTERM or Ctrl-C. A Job still Running then is left for the next start to recover.
pub fn run(settings: Settings) -> Result<(), StartError> {
    let prepared = prepare(settings)?;
    let addr = prepared.addr();
    let runtime = runtime()?;
    let served = runtime.block_on(async {
        eprintln!(
            "title-select: serving {} on http://{addr}",
            title_select_api::PREFIX
        );
        tokio::select! {
            served = prepared.serve() => served,
            () = stop_signal() => Ok(()),
        }
    });
    runtime.shutdown_background();
    served.map_err(|e| StartError::Listen(addr, e))
}

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
    eprintln!("title-select: stopping");
}

/// A Service running on its own runtime thread, stopped when dropped.
pub struct Handle {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Starts the Service on a background thread, for tests and embedding.
pub fn spawn(settings: Settings) -> Result<Handle, StartError> {
    let prepared = prepare(settings)?;
    let addr = prepared.addr();
    let runtime = runtime()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        runtime.block_on(async {
            tokio::select! {
                served = prepared.serve() => {
                    if let Err(err) = served {
                        eprintln!("title-select: {err}");
                    }
                }
                _ = stopped => {}
            }
        });
        // Lets a running Audit finish and release the store, so it can be opened again.
        runtime.shutdown_timeout(std::time::Duration::from_secs(60));
    });
    Ok(Handle {
        addr,
        stop: Some(stop),
        thread: Some(thread),
    })
}

impl Handle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The Service's base URL, such as `http://127.0.0.1:8080`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
