//! The `/api/v1` routes.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Extension, Json, Router, middleware};
use futures_util::StreamExt;
use serde::Deserialize;
use title_select_api::additions::{
    Addition, AdditionsCheck, AdditionsSet, EditAddition, SystemQuery,
};
use title_select_api::build::{
    AddLibraryFolder, ApprovePlan, BuildDetail, BuildSummary, BuiltFile, ChangeLibraryFolder,
    FolderCheck, Incomplete, KeptDeletion, KeptFile, LibraryFolder, Plan, PlanBuild, PlanCleanup,
    PlanKeptDeletion,
};
use title_select_api::fetch::{FetchProvider, FetchStatus, FetchTrigger};
use title_select_api::ingest::{
    DismissIngestItems, IngestFolder, IngestItem, IngestKeptFile, IngestPlan, PlanIngest,
    RegisterIngestFolder,
};
use title_select_api::repair::{PlanRepair, RepairPlan, SourceKeptDeletion, SourceKeptFile};
use title_select_api::selection::{Role, SelectionProfile, diff_rows};
use title_select_api::storage::StoragePolicy;
use title_select_api::{
    AddSourceSet, Availability, CreateProfile, Dat, ErrorBody, Finding, GameStatus, Health,
    ImportedDatRevision, JobEvent, JobSnapshot, Page, PathTemplateDefaults, Preview,
    PreviewRequest, ProfileCheck, ProfileRecord, ProfileVersion, Root, SSE_KEEP_ALIVE, SaveProfile,
    SavedProfile, SelectionDiff, SelectionRecord, SelectionReleaseRow, SourceSetSystem, StartAudit,
    StartSelection, Vocabulary,
};
use title_select_selection::Engine;
use title_select_store::config::Config;
use title_select_store::fetch::Fetcher;
use title_select_store::{Error, Store, Workers};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::auth::{Actor, Auth, require_operator};
use crate::jobs::Jobs;
use crate::selections::{Previews, UNAVAILABLE};

mod lifecycle;

/// The largest DAT file an upload may carry.
pub const UPLOAD_LIMIT: usize = 512 * 1024 * 1024;
const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 1000;
/// How many bytes of NDJSON are gathered before they are sent.
const NDJSON_CHUNK: usize = 64 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    pub jobs: Arc<Jobs>,
    pub workers: Workers,
    pub engine: Option<Arc<Engine>>,
    pub previews: Arc<Previews>,
    pub fetcher: Arc<Fetcher>,
}

impl AppState {
    fn engine(&self) -> ApiResult<Arc<Engine>> {
        self.engine
            .clone()
            .ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE.into(), None))
    }
}

pub fn router(state: AppState, auth: Arc<Auth>, ui: Option<&std::path::Path>) -> Router {
    let operator = Router::new()
        .route("/roots", get(roots))
        .route("/dats", get(dats))
        .route(
            "/dat-revisions",
            get(dat_revisions)
                .post(upload_dat)
                .layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route(
            "/cue-packs",
            get(cue_packs)
                .post(upload_cue_pack)
                .layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route("/path-templates", get(path_templates))
        .route("/source-sets", get(source_sets).post(add_source_set))
        .route("/source-set-summaries", get(source_set_summaries))
        .route("/source-sets/{name}", get(source_set))
        .route("/source-sets/{name}/history", get(history))
        .route("/source-sets/{name}/fixdat", get(fixdat))
        .route("/source-sets/{name}/have-miss.csv", get(have_miss))
        .route("/source-sets/{name}/follow", put(set_follow))
        .route(
            "/source-sets/{name}/stat-walks",
            get(stat_walks).post(start_stat_walk),
        )
        .route(
            "/source-sets/{name}/followed-revisions",
            get(followed_revisions),
        )
        .route("/source-sets/{name}/ingests", get(source_set_ingests))
        .route("/fetch", get(fetch_status))
        .route("/fetch/{provider}", post(start_fetch))
        .route("/audits", post(start_audit))
        .route("/audits/{id}", get(audit))
        .route("/audits/{id}/report", get(report))
        .route("/audits/{id}/entries", get(entries))
        .route("/audits/{id}/entries.ndjson", get(entries_ndjson))
        .route("/audits/{id}/games", get(games))
        .route("/vocabulary", get(vocabulary))
        .route("/systems", get(systems))
        .route("/check", post(check_profile))
        .route("/profiles", get(profiles).post(create_profile))
        .route("/profiles/{name}", get(profile))
        .route(
            "/profiles/{name}/versions",
            get(profile_versions).post(save_profile),
        )
        .route("/profiles/{name}/versions/{version}", get(profile_version))
        .route("/preview", post(preview))
        .route(
            "/storage-policy",
            get(storage_policy).put(set_storage_policy),
        )
        .route("/selections", get(selections).post(start_selection))
        .route("/selections/{id}", get(selection_record))
        .route("/selections/{id}/selection", get(selection))
        .route("/selections/{id}/releases", get(selection_releases))
        .route("/selections/{id}/availability", get(availability))
        .route("/selections/{id}/diff", get(selection_diff))
        .route("/libraries", get(library_folders).post(add_library_folder))
        .route(
            "/libraries/{id}",
            get(library_folder).patch(change_library_folder),
        )
        .route("/libraries/{id}/builds", get(folder_builds))
        .route("/libraries/{id}/files", get(built_files))
        .route("/libraries/{id}/plans", post(plan_build))
        .route("/libraries/{id}/cleanups", post(plan_cleanup))
        .route("/libraries/{id}/checks", post(start_check))
        .route("/libraries/{id}/check", get(latest_check))
        .route("/libraries/{id}/incomplete", get(incomplete))
        .route("/libraries/{id}/kept-files", get(kept_files))
        .route("/libraries/{id}/kept-deletions", post(plan_kept_deletion))
        .route("/kept-deletions/{id}", get(kept_deletion))
        .route("/kept-deletions/{id}/approval", post(approve_kept_deletion))
        .route(
            "/ingest-folders",
            get(ingest_folders).post(register_ingest_folder),
        )
        .route("/ingest-folders/{root}", get(ingest_folder))
        .route("/ingest-folders/{root}/scans", post(start_ingest_scan))
        .route("/ingest-folders/{root}/items", get(ingest_items))
        .route(
            "/ingest-folders/{root}/plans",
            get(ingest_plans).post(plan_ingest),
        )
        .route("/ingest-folders/{root}/kept-files", get(ingest_kept_files))
        .route(
            "/ingest-folders/{root}/dismissals",
            post(dismiss_ingest_items),
        )
        .route("/ingest-items/{id}", get(ingest_item))
        .route("/ingest-items/{id}/dismissal", post(dismiss_ingest_item))
        .route("/ingest-plans/{id}", get(ingest_plan))
        .route("/ingest-plans/{id}/approval", post(approve_ingest_plan))
        .route(
            "/source-sets/{name}/repairs",
            get(repair_plans).post(plan_repair),
        )
        .route("/source-sets/{name}/kept-files", get(source_kept_files))
        .route(
            "/source-sets/{name}/kept-deletions",
            post(plan_source_kept_deletion),
        )
        .route("/repairs/{id}", get(repair_plan))
        .route("/repairs/{id}/approval", post(approve_repair))
        .route("/source-kept-deletions/{id}", get(source_kept_deletion))
        .route(
            "/source-kept-deletions/{id}/approval",
            post(approve_source_kept_deletion),
        )
        .route("/additions-sets", get(additions_sets))
        .route("/additions-set", get(additions_set))
        .route("/additions-set/checks", post(check_additions))
        .route("/additions/{id}", get(addition).patch(edit_addition))
        .route("/additions/{id}/retirement", post(retire_addition))
        .route("/additions/{id}/restoration", post(restore_addition))
        .route("/additions-set/kept-files", get(additions_kept_files))
        .route(
            "/additions-set/kept-deletions",
            post(plan_additions_kept_deletion),
        )
        .route(
            "/additions-kept-deletions/{id}",
            get(additions_kept_deletion),
        )
        .route(
            "/additions-kept-deletions/{id}/approval",
            post(approve_additions_kept_deletion),
        )
        .route("/plans/{id}", get(plan))
        .route("/plans/{id}/approval", post(approve_plan))
        .route("/builds/{id}", get(build))
        .route("/jobs", get(jobs))
        .route("/failed-jobs", get(failed_jobs))
        .route("/jobs/{id}", get(job))
        .route("/jobs/{id}/events", get(job_events))
        .route("/jobs/{id}/cancellation", post(cancel_job))
        .route("/events", get(all_events))
        .merge(lifecycle::routes())
        .layer(middleware::from_fn_with_state(auth, require_operator));
    let api = Router::new()
        .route("/health", get(health))
        .merge(operator)
        .fallback(not_found);
    let app = Router::new()
        .nest(title_select_api::PREFIX, api)
        .route("/api", any(not_found))
        .route("/api/{*rest}", any(not_found))
        .route("/metrics", get(metrics))
        .with_state(state);
    match ui {
        Some(dir) => app.merge(workbench(dir)),
        None => app.fallback(not_found),
    }
}

/// The workbench's bundle. Its assets are public; everything it shows comes from the API, which is not. Any path
/// that is not an asset is a workbench screen, so it gets `workbench.html` (cargo-leptos reserves `index.html`).
fn workbench(dir: &std::path::Path) -> Router {
    let files = ServeDir::new(dir)
        .append_index_html_on_directories(false)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(ServeFile::new(dir.join("workbench.html")));
    Router::new()
        .fallback_service(files)
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(WORKBENCH_CSP),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
}

/// Same-origin only. WebAssembly needs `wasm-unsafe-eval`; the table library positions its placeholder rows with
/// inline styles.
const WORKBENCH_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; img-src 'self'; font-src 'self'; connect-src 'self'; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub fn error_response(status: StatusCode, error: String, job: Option<i64>) -> Response {
    (status, Json(ErrorBody { error, job })).into_response()
}

/// An error response with an [`ErrorBody`].
pub struct ApiError(StatusCode, String, Option<i64>);

impl ApiError {
    fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError(StatusCode::BAD_REQUEST, message.into(), None)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.0, self.1, self.2)
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> ApiError {
        let status = match &err {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Refused(_) => StatusCode::BAD_REQUEST,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::JobActive { job, .. } => {
                return ApiError(StatusCode::CONFLICT, err.to_string(), Some(*job));
            }
            Error::Audit(title_select_engine::Error::Dat(_)) => StatusCode::BAD_REQUEST,
            Error::Sqlite(_) | Error::Io(_) | Error::Audit(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, err.to_string(), None)
    }
}

impl From<title_select_selection::Error> for ApiError {
    fn from(err: title_select_selection::Error) -> ApiError {
        use title_select_selection::Error as E;
        let status = match &err {
            E::Profile(_) | E::Patterns(_) | E::Unsupported(_) => StatusCode::BAD_REQUEST,
            E::Cancelled => StatusCode::CONFLICT,
            E::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            E::Install(_) => StatusCode::SERVICE_UNAVAILABLE,
            E::Io { .. } | E::Trace(_) | E::Builtin(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &err {
            E::Cancelled => "a newer preview replaced this one".to_owned(),
            _ => err.to_string(),
        };
        ApiError(status, message, None)
    }
}

macro_rules! rejection {
    ($($t:ty),*) => {$(
        impl From<$t> for ApiError {
            fn from(err: $t) -> ApiError {
                ApiError(err.status(), err.body_text(), None)
            }
        }
    )*};
}
rejection!(
    JsonRejection,
    PathRejection,
    QueryRejection,
    axum::extract::multipart::MultipartRejection,
    axum::extract::multipart::MultipartError
);

type ApiResult<T> = Result<T, ApiError>;

/// Runs a store call on a blocking thread.
async fn blocking<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Store) -> Result<T, Error> + Send + 'static,
) -> ApiResult<T> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?
        .map_err(ApiError::from)
}

async fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "no such API path".into(), None)
}

/// Prometheus metrics, read from the store now. Outside the API, and unauthenticated: they name no path or Game.
async fn metrics(State(state): State<AppState>) -> ApiResult<Response> {
    let metrics = blocking(&state, Store::metrics).await?;
    Ok((
        [(header::CONTENT_TYPE, crate::metrics::CONTENT_TYPE)],
        crate::metrics::render(&metrics),
    )
        .into_response())
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".into(),
    })
}

async fn roots(State(state): State<AppState>) -> Json<Vec<Root>> {
    Json(
        state
            .config
            .roots
            .iter()
            .map(|(name, root)| Root {
                name: name.clone(),
                path: root.path.display().to_string(),
                writable: root.writable,
                writable_paths: root.writable_paths.clone(),
                ingest: root.is_ingest(),
            })
            .collect(),
    )
}

async fn dats(State(state): State<AppState>) -> ApiResult<Json<Vec<Dat>>> {
    let revisions = blocking(&state, Store::dat_revisions).await?;
    let mut dats: BTreeMap<String, Dat> = BTreeMap::new();
    for revision in revisions {
        dats.entry(revision.dat())
            .or_insert_with(|| Dat {
                dat: revision.dat(),
                provider: revision.provider.clone(),
                name: revision.name.clone(),
                revisions: Vec::new(),
            })
            .revisions
            .push(revision);
    }
    Ok(Json(dats.into_values().collect()))
}

async fn dat_revisions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::DatRevisionRecord>>> {
    Ok(Json(blocking(&state, Store::dat_revisions).await?))
}

/// A multipart form: `provider`, an optional `source_url`, and the DAT as `file`, with its original filename.
async fn upload_dat(
    State(state): State<AppState>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> ApiResult<(StatusCode, Json<ImportedDatRevision>)> {
    let mut multipart = multipart?;
    let (mut provider, mut source_url, mut file) = (None, None, None);
    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("provider") => provider = Some(field.text().await?),
            Some("source_url") => source_url = Some(field.text().await?).filter(|s| !s.is_empty()),
            Some("file") => {
                let name = field.file_name().unwrap_or_default().to_owned();
                file = Some((name, field.bytes().await?));
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "unexpected form field {:?}",
                    other.unwrap_or_default()
                )));
            }
        }
    }
    let provider = provider.ok_or_else(|| ApiError::bad_request("the form has no provider"))?;
    let (name, bytes) = file.ok_or_else(|| ApiError::bad_request("the form has no file"))?;
    if name.is_empty() {
        return Err(ApiError::bad_request("the file has no filename"));
    }
    let (dat_revision, outcome) = blocking(&state, move |store| {
        store.import_dat_bytes(&bytes, &name, &provider, source_url.as_deref())
    })
    .await?;
    let status = match outcome {
        title_select_api::ImportOutcome::Added => StatusCode::CREATED,
        _ => StatusCode::OK,
    };
    Ok((
        status,
        Json(ImportedDatRevision {
            outcome,
            dat_revision,
        }),
    ))
}

async fn cue_packs(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::CuePackRecord>>> {
    Ok(Json(blocking(&state, Store::cue_packs).await?))
}

/// Imports a cue pack you upload, for the DAT named in the form's `dat` field as `<provider>/<name>`.
async fn upload_cue_pack(
    State(state): State<AppState>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::CuePackImport>)> {
    let mut multipart = multipart?;
    let (mut dat, mut source_url, mut file) = (None, None, None);
    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("dat") => dat = Some(field.text().await?),
            Some("source_url") => source_url = Some(field.text().await?).filter(|s| !s.is_empty()),
            Some("file") => {
                let name = field.file_name().unwrap_or_default().to_owned();
                file = Some((name, field.bytes().await?));
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "unexpected form field {:?}",
                    other.unwrap_or_default()
                )));
            }
        }
    }
    let dat = dat.ok_or_else(|| ApiError::bad_request("the form has no DAT"))?;
    let (name, bytes) = file.ok_or_else(|| ApiError::bad_request("the form has no file"))?;
    if name.is_empty() {
        return Err(ApiError::bad_request("the file has no filename"));
    }
    let (pack, outcome) = blocking(&state, move |store| {
        store.import_cue_pack_bytes(
            &bytes,
            &name,
            &dat,
            source_url.as_deref(),
            title_select_api::CuePackOrigin::Import,
        )
    })
    .await?;
    let status = match outcome {
        title_select_api::ImportOutcome::Added => StatusCode::CREATED,
        _ => StatusCode::OK,
    };
    Ok((
        status,
        Json(title_select_api::CuePackImport { pack, outcome }),
    ))
}

async fn path_templates(State(state): State<AppState>) -> Json<PathTemplateDefaults> {
    let templates = &state.config.path_templates;
    Json(PathTemplateDefaults {
        source_set: templates.source_set.to_string(),
        library_folder: templates.library_folder.to_string(),
    })
}

async fn source_sets(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::SourceSetRecord>>> {
    Ok(Json(blocking(&state, Store::source_sets).await?))
}

async fn source_set_summaries(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::SourceSetSummary>>> {
    Ok(Json(blocking(&state, Store::source_set_summaries).await?))
}

async fn add_source_set(
    State(state): State<AppState>,
    body: Result<Json<AddSourceSet>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::SourceSetRecord>)> {
    let Json(add) = body?;
    let config = state.config.clone();
    let record = blocking(&state, move |store| {
        let path = match (add.path, add.template) {
            (Some(_), Some(_)) => {
                return Err(Error::Refused("give a path or a template, not both".into()));
            }
            (Some(path), None) => path,
            (None, template) => {
                store.propose_source_set_path(&config, &add.dat, template.as_deref())?
            }
        };
        store.add_source_set(&config, &add.root, &path, &add.dat)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn library_folders(State(state): State<AppState>) -> ApiResult<Json<Vec<LibraryFolder>>> {
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| store.library_folders(&config)).await?,
    ))
}

async fn add_library_folder(
    State(state): State<AppState>,
    body: Result<Json<AddLibraryFolder>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<LibraryFolder>)> {
    let Json(add) = body?;
    let config = state.config.clone();
    let folder = blocking(&state, move |store| store.add_library_folder(&config, &add)).await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

async fn library_folder(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<LibraryFolder>> {
    let Path(id) = id?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| store.library_folder(&config, id)).await?,
    ))
}

async fn change_library_folder(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ChangeLibraryFolder>, JsonRejection>,
) -> ApiResult<Json<LibraryFolder>> {
    let Path(id) = id?;
    let Json(change) = body?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.change_library_folder(&config, id, &change)
        })
        .await?,
    ))
}

async fn folder_builds(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Vec<BuildSummary>>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.builds(id)).await?))
}

async fn built_files(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Vec<BuiltFile>>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.built_files(id)).await?,
    ))
}

/// Queues a read-only Check of the folder. It runs whether or not the client stays connected.
async fn start_check(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Path(id) = id?;
    let job = blocking(&state, move |store| store.enqueue_check(id, &actor)).await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// The folder's latest check; 404 when it was never checked.
async fn latest_check(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<FolderCheck>> {
    let Path(id) = id?;
    let check = blocking(&state, move |store| store.latest_check(id)).await?;
    check
        .map(Json)
        .ok_or_else(|| Error::NotFound(format!("library folder {id} was never checked")).into())
}

async fn incomplete(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Incomplete>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.incomplete(id)).await?,
    ))
}

/// Computes and stores a Change Plan for review, with a check of the folder. Planning reads the store, stats the
/// folder and reads only what changed in it, so it answers at once.
async fn plan_build(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanBuild>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Plan>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_build_with(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

/// Plans a cleanup of the folder's drift, kept files and files that are not TorrentZip; nothing changes until you
/// approve it.
async fn plan_cleanup(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanCleanup>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Plan>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_cleanup(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn plan(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Plan>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.plan(id)).await?))
}

/// Approves a plan by the digest you reviewed and queues its Build Job.
async fn approve_plan(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let (config, workers) = (state.config.clone(), state.workers);
    let job = blocking(&state, move |store| {
        store.approve_plan(&config, id, &approve.digest, &actor, workers)
    })
    .await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn kept_files(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Vec<KeptFile>>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.kept_files(id)).await?,
    ))
}

/// Plans deleting kept files of the folder; nothing is deleted until the plan is approved.
async fn plan_kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanKeptDeletion>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<KeptDeletion>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_kept_deletion(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<KeptDeletion>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.kept_deletion(id)).await?,
    ))
}

/// Approves deleting kept files with the digest you reviewed, and deletes them before it answers.
async fn approve_kept_deletion(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<KeptDeletion>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.approve_kept_deletion(&config, id, &approve.digest, &actor)
        })
        .await?,
    ))
}

async fn ingest_folders(State(state): State<AppState>) -> ApiResult<Json<Vec<IngestFolder>>> {
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| store.ingest_folders(&config)).await?,
    ))
}

/// Registers an ingest root as an Ingest Folder of this store, writing its marker.
async fn register_ingest_folder(
    State(state): State<AppState>,
    body: Result<Json<RegisterIngestFolder>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<IngestFolder>)> {
    let Json(register) = body?;
    let config = state.config.clone();
    let folder = blocking(&state, move |store| {
        store.register_ingest_folder(&config, &register.root)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

async fn ingest_folder(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<IngestFolder>> {
    let Path(root) = root?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| store.ingest_folder(&config, &root)).await?,
    ))
}

/// Queues a scan of the Ingest Folder. It runs whether or not the client stays connected.
async fn start_ingest_scan(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Path(root) = root?;
    let job = blocking(&state, move |store| {
        store.enqueue_ingest_scan(&root, &actor)
    })
    .await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn ingest_items(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<IngestItem>>> {
    let Path(root) = root?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_items(&root)).await?,
    ))
}

async fn ingest_item(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<IngestItem>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_item(id)).await?,
    ))
}

/// Dismisses an item until it changes.
async fn dismiss_ingest_item(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<IngestItem>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.dismiss_ingest_item(id)).await?,
    ))
}

/// Dismisses several items of an Ingest Folder at once, each until it changes; none if any can't be.
async fn dismiss_ingest_items(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
    body: Result<Json<DismissIngestItems>, JsonRejection>,
) -> ApiResult<Json<Vec<IngestItem>>> {
    let Path(root) = root?;
    let Json(request) = body?;
    Ok(Json(
        blocking(&state, move |store| {
            store.dismiss_ingest_items(&root, &request.items)
        })
        .await?,
    ))
}

async fn ingest_plans(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<IngestPlan>>> {
    let Path(root) = root?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_plans(&root)).await?,
    ))
}

/// Plans a discard or DAT import of items; nothing changes until you approve it.
async fn plan_ingest(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
    body: Result<Json<PlanIngest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<IngestPlan>)> {
    let Path(root) = root?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_ingest(&config, &root, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn ingest_plan(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<IngestPlan>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_plan(id)).await?,
    ))
}

/// Approves an ingest plan with the digest you reviewed, and carries it out before it answers. Audits a DAT import
/// queues, and Additions Jobs that publish the Additions it adds, start at once.
async fn approve_ingest_plan(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<IngestPlan>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let (config, workers) = (state.config.clone(), state.workers);
    let (plan, queued) = blocking(&state, move |store| {
        let plan = store.approve_ingest_plan(&config, id, &approve.digest, &actor, workers)?;
        let queued: Vec<_> = store
            .jobs(100)?
            .into_iter()
            .filter(|job| job.state == title_select_api::JobState::Queued)
            .collect();
        Ok((plan, queued))
    })
    .await?;
    for job in queued {
        state.jobs.changed(job);
    }
    Ok(Json(plan))
}

async fn ingest_kept_files(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<IngestKeptFile>>> {
    let Path(root) = root?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_kept_files(&root)).await?,
    ))
}

async fn additions_sets(State(state): State<AppState>) -> ApiResult<Json<Vec<AdditionsSet>>> {
    Ok(Json(
        blocking(&state, move |store| store.additions_sets()).await?,
    ))
}

/// The Additions Set of `?system=<provider>/<name>`, with its additions.
async fn additions_set(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<AdditionsSet>> {
    let Query(query) = query?;
    Ok(Json(
        blocking(&state, move |store| store.additions_set(&query.system)).await?,
    ))
}

/// Checks the Additions Set of `?system=` for drift against its additions' records; it only reads the folder.
async fn check_additions(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<AdditionsCheck>> {
    let Query(query) = query?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.check_additions(&config, &query.system)
        })
        .await?,
    ))
}

async fn addition(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Addition>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.addition(id)).await?,
    ))
}

/// Renames an Addition, or sets or clears its parent Game.
async fn edit_addition(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<EditAddition>, JsonRejection>,
) -> ApiResult<Json<Addition>> {
    let Path(id) = id?;
    let Json(edit) = body?;
    Ok(Json(
        blocking(&state, move |store| store.edit_addition(id, &edit)).await?,
    ))
}

/// Retires an Addition: builds stop reading it, and its Additions Job moves its file into the Additions Set's
/// kept files.
async fn retire_addition(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Addition>> {
    let Path(id) = id?;
    let (addition, queued) = blocking(&state, move |store| {
        Ok((store.retire_addition(id)?, store.queued_additions_jobs()?))
    })
    .await?;
    queued.into_iter().for_each(|job| state.jobs.changed(job));
    Ok(Json(addition))
}

/// Restores a retired Addition: its Additions Job moves its file back from the kept files, and builds that
/// opt in read it again.
async fn restore_addition(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Addition>> {
    let Path(id) = id?;
    let (addition, queued) = blocking(&state, move |store| {
        Ok((store.restore_addition(id)?, store.queued_additions_jobs()?))
    })
    .await?;
    queued.into_iter().for_each(|job| state.jobs.changed(job));
    Ok(Json(addition))
}

/// The kept files of the Additions Set of `?system=`: files of additions you retired.
async fn additions_kept_files(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<Vec<SourceKeptFile>>> {
    let Query(query) = query?;
    Ok(Json(
        blocking(&state, move |store| {
            store.additions_kept_files(&query.system)
        })
        .await?,
    ))
}

/// Plans deleting kept files of the Additions Set of `?system=`; nothing is deleted until the plan is approved.
async fn plan_additions_kept_deletion(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
    body: Result<Json<PlanKeptDeletion>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SourceKeptDeletion>)> {
    let Query(query) = query?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_additions_kept_deletion(&config, &query.system, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn additions_kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<SourceKeptDeletion>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.additions_kept_deletion(id)).await?,
    ))
}

/// Approves deleting kept files of an Additions Set with the digest you reviewed, and deletes them before it answers.
async fn approve_additions_kept_deletion(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<SourceKeptDeletion>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.approve_additions_kept_deletion(&config, id, &approve.digest, &actor)
        })
        .await?,
    ))
}

async fn repair_plans(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<RepairPlan>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.repair_plans(&name)).await?,
    ))
}

/// Proposes a repair of the Source Set from its latest Audit; nothing changes until the plan is approved.
async fn plan_repair(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<PlanRepair>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<RepairPlan>)> {
    let Path(name) = name?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_repair(&config, &name, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn repair_plan(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<RepairPlan>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.repair_plan(id)).await?,
    ))
}

/// Approves a repair with the digest you reviewed, and queues the Repair Job that carries it out.
async fn approve_repair(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<RepairPlan>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let (config, workers) = (state.config.clone(), state.workers);
    let plan = blocking(&state, move |store| {
        store.approve_repair(&config, id, &approve.digest, &actor, workers)
    })
    .await?;
    if let Some(job) = plan.job {
        let found = blocking(&state, move |store| store.job(job)).await?;
        state.jobs.changed(found);
    }
    Ok(Json(plan))
}

async fn source_kept_files(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<SourceKeptFile>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.source_kept_files(&name)).await?,
    ))
}

/// Plans deleting kept files of the Source Set; nothing is deleted until the plan is approved.
async fn plan_source_kept_deletion(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<PlanKeptDeletion>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SourceKeptDeletion>)> {
    let Path(name) = name?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_source_kept_deletion(&config, &name, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn source_kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<SourceKeptDeletion>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.source_kept_deletion(id)).await?,
    ))
}

/// Approves deleting kept files of a Source Set with the digest you reviewed, and deletes them before it answers.
async fn approve_source_kept_deletion(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<SourceKeptDeletion>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.approve_source_kept_deletion(&config, id, &approve.digest, &actor)
        })
        .await?,
    ))
}

async fn build(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<BuildDetail>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.build(id)).await?))
}

async fn source_set(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<title_select_api::SourceSetRecord>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.source_set(&name)).await?,
    ))
}

/// Makes a Source Set Follow its DAT, or stop. Following never starts an Audit.
async fn set_follow(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<title_select_api::SetFollow>, JsonRejection>,
) -> ApiResult<Json<title_select_api::SourceSetRecord>> {
    let Path(name) = name?;
    let Json(request) = body?;
    Ok(Json(
        blocking(&state, move |store| store.set_follow(&name, request.follow)).await?,
    ))
}

/// Queues a stat walk of a Source Set: a check, from sizes and times alone, for files changed since its latest Audit.
async fn start_stat_walk(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Path(name) = name?;
    let job = blocking(&state, move |store| store.enqueue_stat_walk(&name, &actor)).await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// The stat walks of a Source Set that found files changed, added or removed, newest first.
async fn stat_walks(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<title_select_api::StatWalk>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.stat_walks(&name)).await?,
    ))
}

/// The newer DAT Revisions that made a Source Set that Follows its DAT Stale, newest first.
async fn followed_revisions(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<title_select_api::FollowedRevision>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.followed_revisions(&name)).await?,
    ))
}

async fn fetch_status(State(state): State<AppState>) -> ApiResult<Json<FetchStatus>> {
    let (config, fetcher) = (state.config.clone(), state.fetcher.clone());
    Ok(Json(
        blocking(&state, move |store| store.fetch_status(&config, &fetcher)).await?,
    ))
}

/// How long a fetch you start is waited for before it is left to run on: long enough to report a refusal, which
/// comes before any request.
const FETCH_ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Fetches from a DAT Provider now, by hand: still at most once a day per system. Answers with where fetching stands
/// once the fetch ends, or while it runs on after a moment. A fetch never starts an Audit.
async fn start_fetch(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    provider: Result<Path<String>, PathRejection>,
) -> ApiResult<(StatusCode, Json<FetchStatus>)> {
    let Path(provider) = provider?;
    let provider = FetchProvider::parse(&provider).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("{provider:?} is not a provider Title Select fetches from"),
            None,
        )
    })?;
    let (store, config, fetcher) = (
        state.store.clone(),
        state.config.clone(),
        state.fetcher.clone(),
    );
    let fetch = tokio::task::spawn_blocking(move || {
        crate::jobs::log_fetch(
            provider,
            store.fetch(&config, &fetcher, provider, FetchTrigger::Manual, &actor),
        )
    });
    let status = match tokio::time::timeout(FETCH_ANSWER_WITHIN, fetch).await {
        Ok(ended) => {
            ended
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))??;
            StatusCode::OK
        }
        Err(_) => StatusCode::ACCEPTED,
    };
    let (config, fetcher) = (state.config.clone(), state.fetcher.clone());
    let now = blocking(&state, move |store| store.fetch_status(&config, &fetcher)).await?;
    Ok((status, Json(now)))
}

async fn history(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<title_select_api::AuditSummary>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.history(&name)).await?,
    ))
}

#[derive(Deserialize)]
struct ExportQuery {
    selection: Option<i64>,
}

/// The Fixdat of a Source Set's latest Audit, as a download.
async fn fixdat(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    query: Result<Query<ExportQuery>, QueryRejection>,
) -> ApiResult<Response> {
    let (Path(name), Query(query)) = (name?, query?);
    let export = blocking(&state, move |store| store.fixdat(&name, query.selection)).await?;
    Ok(download(export, "application/xml"))
}

/// The have/miss CSV of a Source Set's latest Audit, as a download.
async fn have_miss(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    query: Result<Query<ExportQuery>, QueryRejection>,
) -> ApiResult<Response> {
    let (Path(name), Query(query)) = (name?, query?);
    let export = blocking(&state, move |store| store.have_miss(&name, query.selection)).await?;
    Ok(download(export, "text/csv; charset=utf-8"))
}

/// An exported file, to be saved under its name: an ASCII fallback, and the name itself as RFC 5987 gives it.
fn download(export: title_select_store::Export, content_type: &'static str) -> Response {
    let fallback: String = export
        .file_name
        .chars()
        .map(|c| match c {
            ' '..='~' if c != '"' && c != '\\' => c,
            _ => '_',
        })
        .collect();
    let encoded: String = export
        .file_name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect();
    let disposition = format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}");
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).expect("visible ASCII"),
            ),
        ],
        export.content,
    )
        .into_response()
}

/// The approved ingest plans that changed a Source Set: DAT imports that queued an Audit of it, and additions to its
/// DAT's Additions Set.
async fn source_set_ingests(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<title_select_api::ingest::IngestPlan>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.source_set_ingests(&name)).await?,
    ))
}

/// Queues an Audit Job. It runs whether or not the client stays connected.
async fn start_audit(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    body: Result<Json<StartAudit>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Json(start) = body?;
    let workers = state.workers;
    let job = blocking(&state, move |store| {
        store.enqueue_audit(
            &start.source_set,
            start.revision.as_deref(),
            &actor,
            workers,
            start.evidence,
        )
    })
    .await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn audit(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::AuditSummary>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.audit(id)).await?))
}

/// The full `schema: 3` report.
async fn report(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::Report>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.report(id)).await?))
}

#[derive(Deserialize)]
struct EntriesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    finding: Option<Finding>,
}

#[derive(Deserialize)]
struct GamesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    status: Option<GameStatus>,
}

/// The cursor is the `seq` of the last item on the previous page.
fn paging(cursor: Option<&str>, limit: Option<usize>) -> ApiResult<(Option<u64>, usize)> {
    let after = cursor
        .map(|c| {
            c.parse()
                .map_err(|_| ApiError::bad_request(format!("invalid cursor {c:?}")))
        })
        .transpose()?;
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    Ok((after, limit))
}

fn to_page<T>((items, next): (Vec<T>, Option<u64>)) -> Json<Page<T>> {
    Json(Page {
        items,
        next_cursor: next.map(|n| n.to_string()),
    })
}

async fn entries(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<EntriesQuery>, QueryRejection>,
) -> ApiResult<Json<Page<title_select_api::EntryRow>>> {
    let (Path(id), Query(query)) = (id?, query?);
    let (after, limit) = paging(query.cursor.as_deref(), query.limit)?;
    let page = blocking(&state, move |store| {
        store.entries(id, after, limit, query.finding)
    })
    .await?;
    Ok(to_page(page))
}

async fn games(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<GamesQuery>, QueryRejection>,
) -> ApiResult<Json<Page<title_select_api::GameRow>>> {
    let (Path(id), Query(query)) = (id?, query?);
    let (after, limit) = paging(query.cursor.as_deref(), query.limit)?;
    let page = blocking(&state, move |store| {
        store.games(id, after, limit, query.status)
    })
    .await?;
    Ok(to_page(page))
}

/// Every Entry of a full Audit, one JSON [`title_select_api::EntryRow`] per line, read in one transaction.
async fn entries_ndjson(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Response> {
    let Path(id) = id?;
    let summary = blocking(&state, move |store| store.audit(id)).await?;
    if summary.pruned {
        return Err(
            Error::Refused(format!("audit {id} was pruned; only its summary is kept")).into(),
        );
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let mut chunk = Vec::with_capacity(NDJSON_CHUNK);
        let sent = store.for_each_entry(id, |entry| {
            serde_json::to_writer(&mut chunk, &entry).expect("an Entry serializes");
            chunk.push(b'\n');
            if chunk.len() < NDJSON_CHUNK {
                return true;
            }
            let full = std::mem::replace(&mut chunk, Vec::with_capacity(NDJSON_CHUNK));
            tx.blocking_send(Ok(full.into())).is_ok()
        });
        let last = match sent {
            Ok(()) => Ok(chunk.into()),
            Err(err) => Err(std::io::Error::other(err.to_string())),
        };
        let _ = tx.blocking_send(last);
    });
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(ReceiverStream::new(rx)),
    )
        .into_response())
}

async fn vocabulary(State(state): State<AppState>) -> ApiResult<Json<Vocabulary>> {
    let engine = state.engine()?;
    let names = tokio::task::spawn_blocking(move || engine.names())
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))??;
    Ok(Json(Vocabulary {
        regions: names.regions,
        languages: names.languages,
        videos: names.videos,
        categories: title_select_api::selection::Category::ALL.to_vec(),
        default_region_order: names.default_region_order,
        default_video_order: names.default_video_order,
    }))
}

/// The system each registered Source Set's latest DAT Revision is for, so that the Profile editor can offer
/// settings for every system before anything was selected.
async fn systems(State(state): State<AppState>) -> ApiResult<Json<Vec<SourceSetSystem>>> {
    let engine = state.engine()?;
    let store = state.store.clone();
    let systems = tokio::task::spawn_blocking(move || crate::selections::systems(&store, &engine))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))??;
    Ok(Json(systems))
}

/// Checks that the engine can read a Selection Profile; the problems are empty when it can.
async fn check_profile(
    State(state): State<AppState>,
    body: Result<Json<SelectionProfile>, JsonRejection>,
) -> ApiResult<Json<ProfileCheck>> {
    let Json(profile) = body?;
    let engine = state.engine()?;
    let checked = tokio::task::spawn_blocking(move || engine.check(&profile))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?;
    Ok(Json(ProfileCheck {
        problems: match checked {
            Ok(()) => Vec::new(),
            Err(title_select_selection::Error::Profile(problem)) => vec![problem],
            Err(title_select_selection::Error::Patterns(problems)) => problems,
            Err(err) => return Err(err.into()),
        },
    }))
}

async fn profiles(State(state): State<AppState>) -> ApiResult<Json<Vec<ProfileRecord>>> {
    Ok(Json(blocking(&state, |store| store.profiles()).await?))
}

async fn storage_policy(State(state): State<AppState>) -> ApiResult<Json<StoragePolicy>> {
    Ok(Json(blocking(&state, Store::storage_policy).await?))
}

/// Replaces the whole storage policy. Nothing in any folder changes until you approve a plan that converts it.
async fn set_storage_policy(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    body: Result<Json<StoragePolicy>, JsonRejection>,
) -> ApiResult<Json<StoragePolicy>> {
    let Json(policy) = body?;
    Ok(Json(
        blocking(&state, move |store| {
            store.set_storage_policy(&policy, &actor)
        })
        .await?,
    ))
}

async fn create_profile(
    State(state): State<AppState>,
    body: Result<Json<CreateProfile>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<ProfileRecord>)> {
    let Json(create) = body?;
    let record = blocking(&state, move |store| store.create_profile(&create.name)).await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn profile(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<ProfileRecord>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.profile(&name)).await?,
    ))
}

async fn profile_versions(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<ProfileVersion>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.profile_versions(&name)).await?,
    ))
}

async fn profile_version(
    State(state): State<AppState>,
    path: Result<Path<(String, u32)>, PathRejection>,
) -> ApiResult<Json<ProfileVersion>> {
    let Path((name, version)) = path?;
    Ok(Json(
        blocking(&state, move |store| {
            store.profile_version(&name, Some(version))
        })
        .await?,
    ))
}

/// Saves a Profile as its next version, creating the Profile on its first save, and queues the Selections asked
/// for. A save with nothing changed creates no version.
async fn save_profile(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<SaveProfile>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SavedProfile>)> {
    let (Path(name), Json(save)) = (name?, body?);
    let engine = state.engine()?;
    let profile = save.profile.clone();
    tokio::task::spawn_blocking(move || engine.check(&profile))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))??;
    let workers = state.workers;
    let saved = blocking(&state, move |store| {
        store.save_profile(
            &name,
            &save.profile,
            save.base_version,
            &actor,
            &save.queue,
            workers,
        )
    })
    .await?;
    for job in &saved.jobs {
        state.jobs.changed(job.clone());
    }
    let status = if saved.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(saved)))
}

/// A live preview: the Selection a Profile, saved or not, would make for a Source Set now, outside the Job queue.
/// A newer preview from the same actor, or the client going away, kills the engine run.
async fn preview(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    body: Result<Json<PreviewRequest>, JsonRejection>,
) -> ApiResult<Json<Preview>> {
    let Json(request) = body?;
    let engine = state.engine()?;
    let guard = state.previews.start(&actor);
    let cancel = guard.flag();
    let started = std::time::Instant::now();
    let store = state.store.clone();
    let made = tokio::task::spawn_blocking(move || -> ApiResult<Preview> {
        let (revision, blob) =
            store.selection_revision(&request.source_set, request.revision.as_deref())?;
        let (selection, _) = crate::selections::select(
            &store,
            &engine,
            &blob,
            &revision.file,
            &request.profile,
            Some(cancel),
        )?;
        let against = store.latest_selection(&request.source_set, &revision.sha256)?;
        let diff = match against {
            Some(id) => diff_rows(&store.selection_rows(id)?, &selection.rows()),
            None => Vec::new(),
        };
        Ok(Preview {
            dat_revision: revision,
            selection,
            against,
            diff,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?;
    drop(guard);
    Ok(Json(made?))
}

/// Queues a Selection Job. It runs whether or not the client stays connected.
async fn start_selection(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    body: Result<Json<StartSelection>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Json(start) = body?;
    state.engine()?;
    let workers = state.workers;
    let job = blocking(&state, move |store| {
        store.enqueue_selection(
            &start.source_set,
            start.revision.as_deref(),
            &start.profile,
            start.version,
            &actor,
            workers,
        )
    })
    .await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[derive(Deserialize)]
struct SelectionsQuery {
    source_set: Option<String>,
    profile: Option<String>,
}

async fn selections(
    State(state): State<AppState>,
    query: Result<Query<SelectionsQuery>, QueryRejection>,
) -> ApiResult<Json<Vec<SelectionRecord>>> {
    let Query(query) = query?;
    Ok(Json(
        blocking(&state, move |store| {
            store.selections_of(query.source_set.as_deref(), query.profile.as_deref())
        })
        .await?,
    ))
}

async fn selection_record(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<SelectionRecord>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.selection_record(id)).await?,
    ))
}

/// The whole explained Selection, group by group.
async fn selection(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::selection::Selection>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.selection(id)).await?,
    ))
}

#[derive(Deserialize)]
struct ReleasesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    role: Option<Role>,
    group: Option<String>,
}

async fn selection_releases(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<ReleasesQuery>, QueryRejection>,
) -> ApiResult<Json<Page<SelectionReleaseRow>>> {
    let (Path(id), Query(query)) = (id?, query?);
    let (after, limit) = paging(query.cursor.as_deref(), query.limit)?;
    let page = blocking(&state, move |store| {
        store.selection_releases(id, after, limit, query.role, query.group.as_deref())
    })
    .await?;
    Ok(to_page(page))
}

async fn availability(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Availability>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.selection_availability(id)).await?,
    ))
}

#[derive(Deserialize)]
struct DiffQuery {
    /// The Selection to compare against; by default the Source Set's Selection before this one.
    against: Option<i64>,
}

/// Release by release, what changed from the `against` Selection to this one.
async fn selection_diff(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<DiffQuery>, QueryRejection>,
) -> ApiResult<Json<Option<SelectionDiff>>> {
    let (Path(id), Query(query)) = (id?, query?);
    Ok(Json(
        blocking(&state, move |store| {
            let against = match query.against {
                Some(against) => against,
                None => {
                    let record = store.selection_record(id)?;
                    let earlier = store
                        .selections(&record.source_set)?
                        .into_iter()
                        .find(|s| s.id < id && !s.pruned)
                        .map(|s| s.id);
                    match earlier {
                        Some(earlier) => earlier,
                        // The first Selection of its Source Set has nothing to compare with.
                        None => return Ok(None),
                    }
                }
            };
            Ok(Some(SelectionDiff {
                from: against,
                to: id,
                changes: diff_rows(&store.selection_rows(against)?, &store.selection_rows(id)?),
            }))
        })
        .await?,
    ))
}

async fn jobs(State(state): State<AppState>) -> ApiResult<Json<Vec<title_select_api::Job>>> {
    Ok(Json(blocking(&state, |store| store.jobs(100)).await?))
}

/// Every Failed Job, however old, so a client can show each failure the Operator hasn't seen.
async fn failed_jobs(State(state): State<AppState>) -> ApiResult<Json<Vec<title_select_api::Job>>> {
    Ok(Json(blocking(&state, |store| store.failed_jobs()).await?))
}

async fn job(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::Job>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.job(id)).await?))
}

/// Asks a Repair Job to stop at its next safe point.
async fn cancel_job(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::Job>> {
    let Path(id) = id?;
    let job = blocking(&state, move |store| store.cancel_job(id)).await?;
    state.jobs.changed(job.clone());
    Ok(Json(job))
}

type EventStream = Sse<
    KeepAliveStream<
        futures_util::stream::Map<
            ReceiverStream<JobEvent>,
            fn(JobEvent) -> Result<Event, Infallible>,
        >,
    >,
>;

fn sse_event(event: JobEvent) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event(event.name())
        .json_data(&event)
        .expect("a Job event serializes"))
}

fn event_stream(rx: mpsc::Receiver<JobEvent>) -> EventStream {
    let to_event: fn(JobEvent) -> Result<Event, Infallible> = sse_event;
    Sse::new(ReceiverStream::new(rx).map(to_event))
        .keep_alive(KeepAlive::new().interval(SSE_KEEP_ALIVE))
}

/// The latest version of each Job a client has been sent, to drop events that a snapshot or a later state superseded.
/// Subscriptions start before their snapshot is read, so events buffered meanwhile may be older than it.
#[derive(Default)]
struct Sent(HashMap<i64, u64>);

impl Sent {
    /// Whether the event is news to the client: a `state` newer than the Job's latest sent, `progress` of that state
    /// or a newer one, or a `resync`. Records what it lets through.
    fn admit(&mut self, event: &JobEvent) -> bool {
        match event {
            JobEvent::State(job) => self.newer(job.id, job.version),
            JobEvent::Progress(p) => self.0.get(&p.job).is_none_or(|&v| p.version >= v),
            JobEvent::Resync(snapshot) => {
                for job in &snapshot.jobs {
                    self.newer(job.id, job.version);
                }
                true
            }
        }
    }

    fn newer(&mut self, job: i64, version: u64) -> bool {
        let sent = self.0.entry(job).or_default();
        let newer = version > *sent;
        *sent = (*sent).max(version);
        newer
    }
}

/// The Job's `state` now and as it changes, and its `progress` while Running, ending after a final state.
async fn job_events(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<EventStream> {
    let Path(id) = id?;
    // Subscribed before reading the Job, so no change between the two is missed.
    let mut events = state.jobs.subscribe();
    let job = blocking(&state, move |store| store.job(id)).await?;
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut finished = job.state.is_final();
        let mut sent = Sent::default();
        let first = JobEvent::State(Box::new(job));
        sent.admit(&first);
        if tx.send(first).await.is_err() {
            return;
        }
        while !finished {
            let received = tokio::select! {
                received = events.recv() => received,
                () = tx.closed() => return,
            };
            let event = match received {
                Ok(event) if event.job_id() == Some(id) => event,
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let store = state.store.clone();
                    match tokio::task::spawn_blocking(move || store.job(id)).await {
                        Ok(Ok(job)) => JobEvent::State(Box::new(job)),
                        _ => return,
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if !sent.admit(&event) {
                continue;
            }
            finished = matches!(&event, JobEvent::State(job) if job.state.is_final());
            if tx.send(event).await.is_err() {
                return;
            }
        }
    });
    Ok(event_stream(rx))
}

/// How many finished Jobs a `resync` carries besides every Queued or Running one.
const RESYNC_RECENT: usize = 50;

/// A `resync` of the Jobs now.
async fn resync(state: &AppState) -> Option<JobEvent> {
    let store = state.store.clone();
    match tokio::task::spawn_blocking(move || store.job_snapshot(RESYNC_RECENT)).await {
        Ok(Ok(jobs)) => Some(JobEvent::Resync(JobSnapshot { jobs })),
        _ => None,
    }
}

/// A `resync` when the client connects and whenever it falls behind, then every Job's `state` changes and
/// `progress`, until the client leaves.
async fn all_events(State(state): State<AppState>) -> EventStream {
    event_stream(all_job_events(state))
}

fn all_job_events(state: AppState) -> mpsc::Receiver<JobEvent> {
    // Subscribed before the snapshot is read, so no change after it is missed.
    let mut events = state.jobs.subscribe();
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut sent = Sent::default();
        let mut next = resync(&state).await;
        while let Some(event) = next {
            if sent.admit(&event) && tx.send(event).await.is_err() {
                return;
            }
            let received = tokio::select! {
                received = events.recv() => received,
                () = tx.closed() => return,
            };
            next = match received {
                Ok(event) => Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Skips the rest of the backlog, which the snapshot supersedes.
                    events = events.resubscribe();
                    resync(&state).await
                }
                Err(broadcast::error::RecvError::Closed) => None,
            };
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::JobProgress;

    fn state(dir: &std::path::Path) -> AppState {
        AppState {
            store: Arc::new(Store::open(&dir.join("store")).unwrap()),
            config: Arc::new(Config::parse("").unwrap()),
            jobs: Arc::new(Jobs::new(Arc::new(crate::notify::Notifier::off()))),
            workers: Workers {
                readers: 1,
                hashers: 1,
            },
            engine: None,
            previews: Arc::default(),
            fetcher: Arc::new(Fetcher::new(title_select_store::fetch::Endpoints::at(
                "http://127.0.0.1:9",
            ))),
        }
    }

    fn progress(files_done: usize) -> JobEvent {
        JobEvent::Progress(JobProgress {
            job: 1,
            version: 2,
            files_done,
            files_total: 100_000,
        })
    }

    #[tokio::test]
    async fn events_buffered_while_the_snapshot_is_read_are_dropped_when_it_supersedes_them() {
        let dir = tempfile::tempdir().unwrap();
        let set = dir.path().join("sets/Test - System");
        std::fs::create_dir_all(&set).unwrap();
        let mut state = state(dir.path());
        state.config = Arc::new(
            Config::parse(&format!(
                "[roots.sets]\npath = {:?}\n",
                dir.path().join("sets").display().to_string()
            ))
            .unwrap(),
        );
        let dat = r#"<?xml version="1.0"?>
<datafile><header><name>Test - System</name><version>1</version></header>
<game name="Pong"><rom name="Pong.a26" size="4" crc="00000000"/></game></datafile>"#;
        let store = &state.store;
        store
            .import_dat_bytes(dat.as_bytes(), "test.dat", "No-Intro", None)
            .unwrap();
        store
            .add_source_set(
                &state.config,
                "sets",
                "Test - System",
                "No-Intro/Test - System",
            )
            .unwrap();
        store
            .enqueue_audit(
                "Test - System",
                None,
                "Operator",
                state.workers,
                title_select_api::AuditEvidence::Incremental,
            )
            .unwrap();
        let running = store.claim_job().unwrap().unwrap();

        // Subscribed, and the Running state buffered, before the snapshot is read.
        let mut rx = all_job_events(state.clone());
        state
            .jobs
            .publish(JobEvent::State(Box::new(running.clone())));
        let stale = JobProgress {
            job: running.id,
            version: running.version,
            files_done: 0,
            files_total: 1,
        };
        state.jobs.publish(JobEvent::Progress(stale));
        let done = store
            .run_audit_job(&state.config, running.id, None)
            .unwrap();
        assert_eq!(done.state, title_select_api::JobState::Succeeded);
        state.jobs.publish(JobEvent::State(Box::new(done.clone())));

        let first = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv());
        let Ok(Some(JobEvent::Resync(snapshot))) = first.await else {
            panic!("a snapshot first");
        };
        assert_eq!(snapshot.jobs, vec![done.clone()]);
        let next = JobEvent::Progress(JobProgress {
            job: done.id + 1,
            ..stale
        });
        state.jobs.publish(next.clone());
        let after = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv());
        assert_eq!(
            after.await.unwrap(),
            Some(next),
            "the buffered Running, its progress and the repeated Succeeded are dropped"
        );
    }

    #[tokio::test]
    async fn a_client_that_falls_behind_on_every_jobs_events_is_sent_a_resync() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let mut rx = all_job_events(state.clone());
        assert!(
            matches!(rx.recv().await, Some(JobEvent::Resync(_))),
            "a snapshot on connecting"
        );

        for n in 0..10_000 {
            state.jobs.publish(progress(n));
        }
        let mut seen = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            seen.push(event);
        }

        let resync = seen.iter().position(|e| matches!(e, JobEvent::Resync(_)));
        assert!(resync.is_some(), "{} events, none a resync", seen.len());
        assert!(seen.len() < 10_000, "the backlog was skipped");
        state.jobs.publish(progress(10_000));
        assert_eq!(
            rx.recv().await,
            Some(progress(10_000)),
            "events go on after it"
        );
    }
}
