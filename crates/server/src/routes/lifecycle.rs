//! The lifecycle of registrations: what holds each one, Unregistering it, setting a whole set aside, removing a
//! Library Folder's built files, moving a Library Folder, Retiring a Selection Profile, and the history an
//! Unregistered registration leaves until it is forgotten.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use title_select_api::ProfileRecord;
use title_select_api::additions::SystemQuery;
use title_select_api::build::{ApprovePlan, Plan, PlanKeptDeletion};
use title_select_api::lifecycle::{
    AdditionsSetAside, FolderMove, Forgotten, Holds, PlanFolderMove, RegistrationHistory,
    RegistrationKind, UnregisteredRecord, Unregistration,
};
use title_select_api::repair::{RepairPlan, SourceKeptDeletion, SourceKeptFile};

use super::{ApiResult, AppState, blocking};
use crate::auth::Actor;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/roots/{name}/holds", get(root_holds))
        .route("/source-sets/{name}/holds", get(source_set_holds))
        .route(
            "/source-sets/{name}/unregistration",
            post(unregister_source_set),
        )
        .route("/source-sets/{name}/set-aside", post(plan_set_aside))
        .route("/libraries/{id}/holds", get(library_folder_holds))
        .route(
            "/libraries/{id}/unregistration",
            post(unregister_library_folder),
        )
        .route("/libraries/{id}/removals", post(plan_library_removal))
        .route("/libraries/{id}/moves", post(plan_library_folder_move))
        .route("/library-moves/{id}", get(library_folder_move))
        .route(
            "/library-moves/{id}/approval",
            post(approve_library_folder_move),
        )
        .route("/additions-set/holds", get(additions_set_holds))
        .route(
            "/additions-set/unregistration",
            post(unregister_additions_set),
        )
        .route(
            "/additions-set/set-aside",
            get(plan_additions_set_aside).post(approve_additions_set_aside),
        )
        .route("/ingest-folders/{root}/holds", get(ingest_folder_holds))
        .route(
            "/ingest-folders/{root}/unregistration",
            post(unregister_ingest_folder),
        )
        .route("/profiles/{name}/retirement", post(retire_profile))
        .route("/profiles/{name}/restoration", post(restore_profile))
        .route("/unregistered", get(unregistered))
        .route("/unregistered/{kind}/{id}", delete(forget))
        .route(
            "/unregistered/{kind}/{id}/history",
            get(unregistered_history),
        )
        .route(
            "/unregistered/source_set/{id}/kept-files",
            get(unregistered_source_kept_files),
        )
        .route(
            "/unregistered/source_set/{id}/kept-deletions",
            post(plan_unregistered_source_kept_deletion),
        )
        .route(
            "/unregistered/additions_set/{id}/kept-files",
            get(unregistered_additions_kept_files),
        )
        .route(
            "/unregistered/additions_set/{id}/kept-deletions",
            post(plan_unregistered_additions_kept_deletion),
        )
}

/// What holds a configured Root: each registration in it.
async fn root_holds(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Holds>> {
    let Path(name) = name?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| store.root_holds(&config, &name)).await?,
    ))
}

async fn source_set_holds(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Holds>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.source_set_holds(&name)).await?,
    ))
}

/// Unregisters a Source Set; no game file is touched. Refused, saying what holds it, while anything does.
async fn unregister_source_set(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Unregistration>> {
    let Path(name) = name?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.unregister_source_set(&config, &name)
        })
        .await?,
    ))
}

/// Plans setting a whole Source Set aside: a repair that keeps every file in place, approved as any repair is.
async fn plan_set_aside(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<(StatusCode, Json<RepairPlan>)> {
    let Path(name) = name?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| store.plan_set_aside(&config, &name)).await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn library_folder_holds(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Holds>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.library_folder_holds(id)).await?,
    ))
}

/// Unregisters a Library Folder, leaving what was built in it as unmanaged files.
async fn unregister_library_folder(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Unregistration>> {
    let Path(id) = id?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.unregister_library_folder(&config, id)
        })
        .await?,
    ))
}

/// Plans removing every built file of a Library Folder into its kept files; once the build that carries it out
/// succeeds, the folder is Unregistered. Approved as any plan is.
async fn plan_library_removal(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<(StatusCode, Json<Plan>)> {
    let Path(id) = id?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| store.plan_library_removal(&config, id)).await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

/// Plans moving or renaming a Library Folder within its Root; nothing moves until the plan is approved.
async fn plan_library_folder_move(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanFolderMove>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<FolderMove>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_library_folder_move(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn library_folder_move(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<FolderMove>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.library_folder_move(id)).await?,
    ))
}

/// Approves a move with the digest you reviewed, and renames the folder before it answers.
async fn approve_library_folder_move(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<FolderMove>> {
    let Path(id) = id?;
    let Json(approve) = body?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.approve_library_folder_move(&config, id, &approve.digest, &actor)
        })
        .await?,
    ))
}

async fn additions_set_holds(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<Holds>> {
    let Query(query) = query?;
    Ok(Json(
        blocking(&state, move |store| {
            store.additions_set_holds(&query.system)
        })
        .await?,
    ))
}

/// Unregisters the Additions Set of `?system=`; no addition's file is touched.
async fn unregister_additions_set(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<Unregistration>> {
    let Query(query) = query?;
    let config = state.config.clone();
    Ok(Json(
        blocking(&state, move |store| {
            store.unregister_additions_set(&config, &query.system)
        })
        .await?,
    ))
}

/// What setting the Additions Set of `?system=` aside would retire.
async fn plan_additions_set_aside(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
) -> ApiResult<Json<AdditionsSetAside>> {
    let Query(query) = query?;
    Ok(Json(
        blocking(&state, move |store| {
            store.plan_additions_set_aside(&query.system)
        })
        .await?,
    ))
}

/// Sets the Additions Set of `?system=` aside with the digest you reviewed: every addition is retired into kept
/// files, then the set is Unregistered.
async fn approve_additions_set_aside(
    State(state): State<AppState>,
    query: Result<Query<SystemQuery>, QueryRejection>,
    body: Result<Json<ApprovePlan>, JsonRejection>,
) -> ApiResult<Json<AdditionsSetAside>> {
    let Query(query) = query?;
    let Json(approve) = body?;
    let config = state.config.clone();
    let (plan, queued) = blocking(&state, move |store| {
        let plan = store.approve_additions_set_aside(&config, &query.system, &approve.digest)?;
        Ok((plan, store.queued_additions_jobs()?))
    })
    .await?;
    queued.into_iter().for_each(|job| state.jobs.changed(job));
    Ok(Json(plan))
}

async fn ingest_folder_holds(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Holds>> {
    let Path(root) = root?;
    Ok(Json(
        blocking(&state, move |store| store.ingest_folder_holds(&root)).await?,
    ))
}

/// Unregisters an Ingest Folder; no file in it is touched.
async fn unregister_ingest_folder(
    State(state): State<AppState>,
    root: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Unregistration>> {
    let Path(root) = root?;
    let config = state.config.clone();
    let unregistered = {
        let root = root.clone();
        blocking(&state, move |store| {
            store.unregister_ingest_folder(&config, &root)
        })
        .await?
    };
    state.jobs.follow_ups.forget(&root);
    Ok(Json(unregistered))
}

/// Retires a Selection Profile: it is hidden from new choices, and its versions stay for the selections that used
/// them.
async fn retire_profile(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<ProfileRecord>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.retire_profile(&name)).await?,
    ))
}

async fn restore_profile(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<ProfileRecord>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.restore_profile(&name)).await?,
    ))
}

/// Every Unregistered registration whose history is still in the store.
async fn unregistered(State(state): State<AppState>) -> ApiResult<Json<Vec<UnregisteredRecord>>> {
    Ok(Json(
        blocking(&state, move |store| store.unregistered()).await?,
    ))
}

/// The history an Unregistered registration left in the store, read-only, by its id.
async fn unregistered_history(
    State(state): State<AppState>,
    path: Result<Path<(RegistrationKind, i64)>, PathRejection>,
) -> ApiResult<Json<RegistrationHistory>> {
    let Path((kind, id)) = path?;
    Ok(Json(
        blocking(&state, move |store| store.unregistered_history(kind, id)).await?,
    ))
}

/// Forgets an Unregistered registration: its history is removed from the store. No file is touched.
async fn forget(
    State(state): State<AppState>,
    path: Result<Path<(RegistrationKind, i64)>, PathRejection>,
) -> ApiResult<Json<Forgotten>> {
    let Path((kind, id)) = path?;
    Ok(Json(
        blocking(&state, move |store| store.forget(kind, id)).await?,
    ))
}

async fn unregistered_source_kept_files(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Vec<SourceKeptFile>>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.source_kept_files_by_id(id)).await?,
    ))
}

/// Plans deleting kept files an Unregistered Source Set left; approved at `/source-kept-deletions/{id}/approval`.
async fn plan_unregistered_source_kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanKeptDeletion>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SourceKeptDeletion>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_source_kept_deletion_by_id(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn unregistered_additions_kept_files(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<Vec<SourceKeptFile>>> {
    let Path(id) = id?;
    Ok(Json(
        blocking(&state, move |store| store.additions_kept_files_by_id(id)).await?,
    ))
}

/// Plans deleting kept files an Unregistered Additions Set left; approved at
/// `/additions-kept-deletions/{id}/approval`.
async fn plan_unregistered_additions_kept_deletion(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanKeptDeletion>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SourceKeptDeletion>)> {
    let Path(id) = id?;
    let Json(request) = body?;
    let config = state.config.clone();
    let plan = blocking(&state, move |store| {
        store.plan_additions_kept_deletion_by_id(&config, id, &request)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(plan)))
}
