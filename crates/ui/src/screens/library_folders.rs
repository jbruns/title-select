use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use leptos_struct_table::ColumnSort;
use title_select_api::additions::NewAddition;
use title_select_api::build::{
    AddLibraryFolder, ApprovePlan, BlockerKind, BuildDetail, BuildItem, BuildSummary, BuiltFile,
    ChangeLibraryFolder, Cleanup, CleanupAction, FileOutcome, FolderCheck, Incomplete, ItemKind,
    KeepReason, KeptDeletion, KeptFile, LibraryFolder, Plan, PlanBuild, PlanChoices, PlanCleanup,
    PlanItem, PlanKeptDeletion, PlanSummary, RETENTION,
};
use title_select_api::{
    Dat, Job, JobKind, PathTemplateDefaults, ProfileRecord, Root, SelectionRecord, SourceSetRecord,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::build_model::{
    blocker_label, build_state_label, by_kind, cause_label, cleanup_action_label, cleanup_actions,
    drift_kind_label, file_outcome_label, folders_for, group_href, incomplete_counts,
    incomplete_reason_label, item_kind_label, item_kind_note, item_state_label, keep_reason_label,
    removal_kind_label, restore_kind_label, unfinished_label, with_adopt, with_keep_aside,
};
use crate::events::use_live;
use crate::model::{age, earlier, short_sha, when};
use crate::registration;
use crate::screens::additions::{AdditionFields, new_addition};
use crate::screens::jobs::progress_view;
use crate::table::{DataTable, Grid, Rows, rows_of, sort_of};

fn folder_href(id: i64) -> String {
    format!("/libraries/{id}")
}

fn location(folder: &LibraryFolder) -> String {
    format!("{}: {}", folder.root, folder.path)
}

fn current_build(build: &Option<BuildSummary>) -> AnyView {
    match build {
        Some(b) => view! {
            <A href=format!("/builds/{}", b.id)>{format!("#{}", b.id)}</A>
            {format!(", generation {} · {}", b.generation, when(b.finished_at.as_deref().unwrap_or(&b.started_at)))}
        }
        .into_any(),
        None => "Never built".into_any(),
    }
}

/// The time now in milliseconds, ticking while the screen is open so that ages stay current without refetching.
#[derive(Clone, Copy)]
struct Now(ReadSignal<f64>);

const NOW_TICK: std::time::Duration = std::time::Duration::from_secs(30);

fn provide_now() {
    let (now, set_now) = signal(js_sys::Date::now());
    if let Ok(handle) = set_interval_with_handle(move || set_now.set(js_sys::Date::now()), NOW_TICK)
    {
        on_cleanup(move || handle.clear());
    }
    provide_context(Now(now));
}

/// When the folder was last checked, how long ago, and whether a build has changed it since. `full` shows the time
/// as well as the age.
fn last_checked(folder: &LibraryFolder, full: bool) -> AnyView {
    let Some(check) = &folder.check else {
        return "Never checked".into_any();
    };
    let at = when(&check.at);
    let checked = js_sys::Date::parse(&check.at);
    let now = use_context::<Now>();
    let title = at.clone();
    let text = move || {
        let elapsed = now.map_or_else(js_sys::Date::now, |n| n.0.get()) - checked;
        match (elapsed.is_nan(), full) {
            (true, _) => at.clone(),
            (false, true) => format!("{at} ({})", age(elapsed)),
            (false, false) => age(elapsed),
        }
    };
    let stale = folder
        .build
        .as_ref()
        .and_then(|b| b.finished_at.as_deref())
        .is_some_and(|built| earlier(&check.at, built));
    view! {
        <span title=title>{text}</span>
        {stale.then(|| view! { " " <span class="state review">"Before the current build"</span> })}
    }
    .into_any()
}

/// The folder's badges: incomplete, with its counts by reason, and a build that stopped before it finished.
fn badges(folder: &LibraryFolder) -> AnyView {
    let incomplete = folder.incomplete.as_ref().map(|counts| {
        let text = format!("Incomplete: {}", incomplete_counts(counts));
        view! { <span class="state review">{text}</span> " " }
    });
    let unfinished = folder.unfinished.as_ref().map(|u| {
        let text = unfinished_label(u);
        view! { <span class="state review">{text}</span> }
    });
    view! { {incomplete} {unfinished} }.into_any()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FolderCol {
    Location,
    System,
    SourceSet,
    Profile,
    Build,
    Checked,
    Files,
    Status,
}

#[derive(Clone, PartialEq)]
struct FolderItem(LibraryFolder);

impl Grid for FolderItem {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Location,
        FolderCol::System,
        FolderCol::SourceSet,
        FolderCol::Profile,
        FolderCol::Build,
        FolderCol::Checked,
        FolderCol::Files,
        FolderCol::Status,
    ];

    fn header(col: FolderCol) -> &'static str {
        match col {
            FolderCol::Location => "Root and path",
            FolderCol::System => "System",
            FolderCol::SourceSet => "Source set",
            FolderCol::Profile => "Selection profile",
            FolderCol::Build => "Current build",
            FolderCol::Checked => "Last checked",
            FolderCol::Files => "Files",
            FolderCol::Status => "Status",
        }
    }

    fn class(col: FolderCol) -> &'static str {
        match col {
            FolderCol::Location => "c-wide data",
            FolderCol::Files => "c-num",
            _ => "c-wide",
        }
    }

    fn sortable(col: FolderCol) -> bool {
        matches!(
            col,
            FolderCol::Location | FolderCol::System | FolderCol::SourceSet
        )
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        let f = &self.0;
        match col {
            FolderCol::Location => {
                let (href, text) = (folder_href(f.id), location(f));
                view! { <A href=href>{text}</A> }.into_any()
            }
            FolderCol::System => f.system.clone().into_any(),
            FolderCol::SourceSet => f.source_set.clone().into_any(),
            FolderCol::Profile => f.profile.clone().into_any(),
            FolderCol::Build => current_build(&f.build),
            FolderCol::Checked => last_checked(f, false),
            FolderCol::Files => f.files.to_string().into_any(),
            FolderCol::Status => badges(f),
        }
    }
}

/// The library folders, and the form to register another.
#[component]
pub fn Libraries() -> impl IntoView {
    provide_now();
    let live = use_live();
    let folders = LocalResource::new(move || {
        live.settled.track();
        api::get::<Vec<LibraryFolder>>("/libraries")
    });
    let sorting = RwSignal::new(VecDeque::<(FolderCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items: Vec<FolderItem> = folders
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(FolderItem)
            .collect();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    FolderCol::System => a.0.system.cmp(&b.0.system),
                    FolderCol::SourceSet => a.0.source_set.cmp(&b.0.source_set),
                    _ => location(&a.0).cmp(&location(&b.0)),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<FolderItem>);
    let go = navigator();

    view! {
        <h1>"Libraries"</h1>
        <p class="lede">
            "A library folder is a folder in a writable root that you build a source set's selections into, \
            for your front end to play from. Open one to plan a build, review it and approve it."
        </p>
        {move || match folders.get() {
            None => view! { <Loading what="the library folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="libraries"
            label="Libraries".to_string()
            rows
            sorting
            selected
            on_open=Callback::new(move |item: FolderItem| go.run(folder_href(item.0.id)))
            empty="No library folders yet. Register one in a writable root below.".to_string()
        />
        <RegisterFolder on_added=Callback::new(move |id: i64| go.run(folder_href(id))) />
    }
}

#[component]
fn RegisterFolder(on_added: Callback<i64>) -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let defaults = LocalResource::new(|| api::get::<PathTemplateDefaults>("/path-templates"));
    let writable = move || {
        roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.writable)
            .collect::<Vec<_>>()
    };
    let saved = move || {
        profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.latest.is_some())
            .collect::<Vec<_>>()
    };
    let root = RwSignal::new(String::new());
    let set = RwSignal::new(String::new());
    let profile = RwSignal::new(String::new());
    let template = RwSignal::new(None::<String>);
    let typed = RwSignal::new(None::<String>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let proposal = Memo::new(move |_| {
        let Some(template) = template.get() else {
            return Err("Loading the path template…".to_string());
        };
        let set = set.get();
        let sets = sets.get().and_then(Result::ok).unwrap_or_default();
        let Some(record) = sets.into_iter().find(|s| s.name == set) else {
            return Err("Choose a source set to propose a path.".to_string());
        };
        let dats = dats.get().and_then(Result::ok).unwrap_or_default();
        match dats.iter().find(|d| d.dat == record.dat) {
            Some(d) => registration::propose(&template, d),
            None => Err("Loading the source set's DAT…".to_string()),
        }
    });
    let path = Memo::new(move |_| {
        registration::chosen_path(typed.get().as_deref(), &proposal.get()).unwrap_or_default()
    });
    let preview = move || {
        let root_path = writable()
            .into_iter()
            .find(|r| r.name == root.get())
            .map(|r| r.path)
            .unwrap_or_default();
        match (typed.get(), &proposal.get()) {
            (None, Err(reason)) => Err(reason.clone()),
            _ => registration::preview(&root_path, &path.get()),
        }
    };

    Effect::new(move || {
        if root.get_untracked().is_empty()
            && let Some(first) = writable().into_iter().next()
        {
            root.set(first.name);
        }
    });
    Effect::new(move || {
        if let Some(Ok(sets)) = sets.get()
            && set.get_untracked().is_empty()
            && let Some(first) = sets.first()
        {
            set.set(first.name.clone());
        }
    });
    Effect::new(move || {
        if profile.get_untracked().is_empty()
            && let Some(first) = saved().into_iter().next()
        {
            profile.set(first.name);
        }
    });
    Effect::new(move || {
        if let Some(Ok(defaults)) = defaults.get()
            && template.get_untracked().is_none()
        {
            template.set(Some(defaults.library_folder));
        }
    });

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = path.get_untracked();
        if chosen.trim().is_empty() {
            problem.set(Some(
                "Type a path, or change the template so it proposes one.".into(),
            ));
            return;
        }
        let request = AddLibraryFolder {
            root: root.get_untracked(),
            path: Some(chosen),
            template: None,
            source_set: set.get_untracked(),
            profile: profile.get_untracked(),
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let added = api::post::<_, LibraryFolder>("/libraries", &request).await;
            busy.set(false);
            match added {
                Ok(folder) => on_added.run(folder.id),
                Err(message) => problem.set(Some(message)),
            }
        });
    };

    view! {
        <section aria-labelledby="register-title">
            <h2 id="register-title">"Register a library folder"</h2>
            {move || match roots.get() {
                Some(Ok(_)) if writable().is_empty() => view! {
                    <p class="note">
                        "None of your roots is writable. To build into your front end's folders, add a root \
                        for them with "<code>"writable = true"</code>" to your configuration."
                    </p>
                }
                .into_any(),
                _ => ().into_any(),
            }}
            <form class="form" on:submit=submit>
                <label>
                    "Writable root"
                    <select required prop:value=root on:change=move |ev| root.set(event_target_value(&ev))>
                        {move || {
                            writable()
                                .into_iter()
                                .map(|r| {
                                    let label = format!("{} ({})", r.name, r.path);
                                    view! { <option value=r.name>{label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Source set"
                    <select required prop:value=set on:change=move |ev| set.set(event_target_value(&ev))>
                        {move || {
                            sets.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|s| {
                                    let label = format!("{} ({})", s.name, s.dat);
                                    view! { <option value=s.name>{label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Selection profile"
                    <select required prop:value=profile on:change=move |ev| profile.set(event_target_value(&ev))>
                        {move || {
                            saved()
                                .into_iter()
                                .map(|p| view! { <option value=p.name.clone()>{p.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Path template"
                    <input
                        type="text"
                        class="data"
                        title="Tokens: {provider}, {dat} and {platform}"
                        prop:value=move || template.get().unwrap_or_default()
                        on:input=move |ev| template.set(Some(event_target_value(&ev)))
                    />
                </label>
                <label>
                    "Path in the root"
                    <input
                        type="text"
                        class="data"
                        placeholder="Type a path"
                        prop:value=path
                        on:input=move |ev| typed.set(Some(event_target_value(&ev)))
                    />
                </label>
                {move || {
                    (typed.get().is_some() && proposal.get().is_ok())
                        .then(|| {
                            view! {
                                <button type="button" on:click=move |_| typed.set(None)>
                                    "Use the proposed path"
                                </button>
                            }
                        })
                }}
                <button
                    type="submit"
                    class="primary"
                    disabled=move || busy.get() || root.with(String::is_empty) || profile.with(String::is_empty)
                >
                    "Register"
                </button>
            </form>
            <p class="note" aria-live="polite">
                {move || match preview() {
                    Ok(folder) => view! { "Registers " <code>{folder}</code> }.into_any(),
                    Err(reason) => reason.into_any(),
                }}
            </p>
            <p class="note small">
                "The folder must be empty or missing; registering it creates it with a marker file that ties it \
                to this store. {platform} is the front end's folder name for the system. Several library folders \
                may hold the same system, each with its own selection profile."
            </p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// One library folder: what it is bound to, its latest plan, its builds and its files.
#[component]
pub fn LibraryFolderScreen() -> impl IntoView {
    provide_now();
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let folder = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<LibraryFolder>(&format!("/libraries/{id}")).await,
                None => Err("That is not a library folder number.".into()),
            }
        }
    });
    move || match folder.get() {
        None => view! { <Loading what="the library folder" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Library folder"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(folder)) => view! { <FolderView folder refresh /> }.into_any(),
    }
}

#[component]
fn FolderView(folder: LibraryFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let set_href = format!("/source-sets/{}", api::segment(&folder.source_set));
    let profile_href = format!("/profiles/{}", api::segment(&folder.profile));
    let active_job = folder.active_job;
    let (title, system, source_set, profile) = (
        format!("Library folder {}", location(&folder)),
        folder.system.clone(),
        folder.source_set.clone(),
        folder.profile.clone(),
    );
    view! {
        <h1>{title}</h1>
        <p>{badges(&folder)}</p>
        <dl class="facts">
            <dt>"System"</dt>
            <dd>{system}</dd>
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{source_set}</A>
            </dd>
            <dt>"Selection profile"</dt>
            <dd>
                <A href=profile_href>{profile}</A>
            </dd>
            <dt>"Root"</dt>
            <dd>
                {folder.root.clone()}
                {if folder.writable { ", writable" } else { ", not writable now" }}
            </dd>
            <dt>"Generation"</dt>
            <dd>{folder.generation}</dd>
            <dt>"Current build"</dt>
            <dd>{current_build(&folder.build)}</dd>
            <dt>"Last checked"</dt>
            <dd>{last_checked(&folder, true)}</dd>
            <dt>"Files"</dt>
            <dd>{folder.files}</dd>
            <dt>"Registered"</dt>
            <dd>{when(&folder.created_at)}</dd>
        </dl>
        {active_job.map(|job| view! { <BuildProgress job /> })}
        {folder.incomplete.is_some().then(|| view! { <IncompleteItems folder=id /> })}
        <CheckPanel folder=id busy=active_job.is_some() refresh />
        <CleanupForm folder=id source_set=folder.source_set.clone() busy=active_job.is_some() refresh />
        <PlanForm folder=folder.clone() refresh />
        {folder.latest_plan.map(|plan| view! { <PlanPanel plan refresh /> })}
        <ChangeBinding folder=folder.clone() refresh />
        <Builds folder=id />
        <BuiltFiles folder=id />
        <KeptFiles folder=id busy=active_job.is_some() refresh />
    }
}

/// What the folder lacks of its current build's selection, and why.
#[component]
fn IncompleteItems(folder: i64) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Incomplete>(&format!("/libraries/{folder}/incomplete")).await }
    });
    view! {
        <section aria-labelledby="incomplete-title">
            <h2 id="incomplete-title">"Incomplete"</h2>
            <p class="lede">
                "The folder lacks these selected games. Fix what stops them, then plan again; failed files are \
                listed with their errors."
            </p>
            {move || match loaded.get() {
                None => view! { <Loading what="what the folder lacks" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(incomplete)) => view! {
                    <ul class="plain">
                        {incomplete
                            .items
                            .into_iter()
                            .map(|i| {
                                let class = if i.reason == title_select_api::build::IncompleteReason::Failed {
                                    "failed"
                                } else {
                                    ""
                                };
                                view! {
                                    <li>
                                        <code>{i.target}</code>
                                        {format!(": {}", incomplete_reason_label(i.reason))}
                                        {i.detail.map(|d| view! { <span class=class>{format!(": {d}")}</span> })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                }
                .into_any(),
            }}
        </section>
    }
}

/// Starts a read-only check of the folder, and shows the latest one.
#[component]
fn CheckPanel(folder: i64, busy: bool, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move {
            api::get::<FolderCheck>(&format!("/libraries/{folder}/check"))
                .await
                .ok()
        }
    });
    let problem = RwSignal::new(None::<String>);
    let start = move |_| {
        problem.set(None);
        leptos::task::spawn_local(async move {
            match api::post::<_, Job>(&format!("/libraries/{folder}/checks"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <section aria-labelledby="check-title">
            <h2 id="check-title">"Check the folder"</h2>
            <p class="lede">
                "A check compares the folder with what was built into it, and lists everything else in it. It reads \
                only the files whose size or modification time changed, and never changes the folder."
            </p>
            <div class="form">
                <button type="button" disabled=busy on:click=start>"Check the folder"</button>
            </div>
            {move || problem.get().map(|message| view! { <Problem message /> })}
            {move || loaded.get().flatten().map(check_view)}
        </section>
    }
}

fn check_view(check: FolderCheck) -> impl IntoView {
    let s = &check.summary;
    let by = match s.job {
        Some(job) => format!("Checked by job #{job} · {}", when(&s.at)),
        None => format!("Checked with a plan · {}", when(&s.at)),
    };
    let counts = [
        ("Unchanged", s.unchanged + s.refreshed, "count verified"),
        ("Same ROMs, another ZIP", s.repacked, "count"),
        ("Conflict", s.conflict, "count review"),
        ("Missing", s.missing, "count review"),
        ("Unreadable", s.unreadable, "count review"),
        ("Not built here", s.drift, "count"),
        ("Read", s.hashed, "count"),
    ];
    let marker = s.marker.clone();
    let files: Vec<_> = check
        .files
        .iter()
        .filter(|f| f.outcome != FileOutcome::Unchanged || !f.torrentzip)
        .cloned()
        .collect();
    view! {
        <p class="note">{by}</p>
        {marker.map(|m| view! { <div class="notice" role="status"><p>{format!("The marker does not match: {m}")}</p></div> })}
        <ul class="counts">
            {counts
                .into_iter()
                .map(|(what, n, class)| view! {
                    <li class=class><span class="n">{n}</span><span class="what">{what}</span></li>
                })
                .collect_view()}
        </ul>
        {(!files.is_empty()).then(|| view! {
            <h3>"Built files that changed"</h3>
            <ul class="plain">
                {files
                    .into_iter()
                    .map(|f| {
                        let flag = (!f.torrentzip).then_some(", not TorrentZip");
                        view! {
                            <li>
                                <code>{f.target}</code>
                                {format!(": {}", file_outcome_label(f.outcome))}
                                {flag}
                                {f.detail.filter(|d| d != "not TorrentZip").map(|d| format!(": {d}"))}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        })}
        {(!check.drift.is_empty()).then(|| view! {
            <h3>"Not built here"</h3>
            <p class="note small">
                "Entries Title Select did not build. They are left as they are; only one at a selected game's file \
                makes the folder incomplete."
            </p>
            <ul class="plain">
                {check
                    .drift
                    .into_iter()
                    .map(|d| {
                        let game = d.game.map(|g| format!(" ({g})"));
                        let conflict = d.conflict.then_some(", at a selected game's file");
                        view! {
                            <li>
                                <code>{d.name}</code>
                                {format!(": {}", drift_kind_label(d.kind))}
                                {game}
                                {conflict}
                                {d.detail.map(|x| format!(": {x}"))}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        })}
        {(!check.acknowledged.is_empty()).then(|| view! {
            <h3>"Acknowledged"</h3>
            <p class="note small">
                "Entries you acknowledged. They are not reported as drift again until they change."
            </p>
            <ul class="plain">
                {check
                    .acknowledged
                    .into_iter()
                    .map(|d| view! {
                        <li>
                            <code>{d.name}</code>
                            {format!(": {}", drift_kind_label(d.kind))}
                        </li>
                    })
                    .collect_view()}
            </ul>
        })}
    }
}

/// Plans a cleanup of what the latest check found: what to do with each entry Title Select did not build that you
/// choose, the kept files to put back, and the built files to repack as TorrentZip. Nothing changes until you
/// approve the plan.
#[component]
fn CleanupForm(
    folder: i64,
    source_set: String,
    busy: bool,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let live = use_live();
    // An entry added as an addition is suggested its source set's DAT as its system.
    let system = LocalResource::new(move || {
        let path = format!("/source-sets/{}", api::segment(&source_set));
        async move { api::get::<SourceSetRecord>(&path).await.ok().map(|s| s.dat) }
    });
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move {
            let check = api::get::<FolderCheck>(&format!("/libraries/{folder}/check"))
                .await
                .ok();
            let kept = api::get::<Vec<KeptFile>>(&format!("/libraries/{folder}/kept-files"))
                .await
                .unwrap_or_default();
            (check, kept)
        }
    });
    let request = RwSignal::new(PlanCleanup::default());
    // What each entry you choose to add as an addition is added as, by name.
    let adding = RwSignal::new(BTreeMap::<String, RwSignal<NewAddition>>::new());
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    let plan = move |_| {
        working.set(true);
        let mut body = request.get_untracked();
        body.additions = adding.with_untracked(|adding| {
            body.entries
                .iter()
                .filter(|(_, a)| **a == CleanupAction::AddAddition)
                .filter_map(|(name, _)| adding.get(name).map(|a| (name.clone(), a.get_untracked())))
                .collect()
        });
        leptos::task::spawn_local(async move {
            let planned =
                api::post::<_, Plan>(&format!("/libraries/{folder}/cleanups"), &body).await;
            working.set(false);
            match planned {
                Ok(_) => {
                    problem.set(None);
                    request.set(PlanCleanup::default());
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let toggle = |list: &mut Vec<String>, name: &str| match list.iter().position(|x| x == name) {
        Some(at) => {
            list.remove(at);
        }
        None => list.push(name.to_string()),
    };
    let form = move || {
        loaded.get().map(|(check, kept)| {
            let drift = check.as_ref().map(|c| c.drift.clone()).unwrap_or_default();
            let repackable: Vec<String> = check
                .map(|c| {
                    c.files
                        .into_iter()
                        .filter(|f| {
                            !f.torrentzip
                                && matches!(
                                    f.outcome,
                                    FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
                                )
                        })
                        .map(|f| f.target)
                        .collect()
                })
                .unwrap_or_default();
            if drift.is_empty() && repackable.is_empty() && kept.is_empty() {
                return view! {
                    <p class="note">
                        "Nothing to clean up: the latest check found nothing Title Select did not build, every built \
                        file is TorrentZip, and no files are kept aside."
                    </p>
                }
                .into_any();
            }
            view! {
                {(!drift.is_empty()).then(|| view! {
                    <h3>"Not built here"</h3>
                    <ul class="plain">
                        {drift
                            .into_iter()
                            .map(|d| {
                                let name = StoredValue::new(d.name.clone());
                                let current = move || {
                                    request.with(|r| {
                                        name.with_value(|n| r.entries.get(n).map(|a| a.as_str()).unwrap_or(""))
                                            .to_string()
                                    })
                                };
                                let choose = move |ev| {
                                    let value = event_target_value(&ev);
                                    let action = cleanup_actions_all().into_iter().find(|a| a.as_str() == value);
                                    request.update(|r| {
                                        let n = name.get_value();
                                        match action {
                                            Some(a) => {
                                                r.entries.insert(n, a);
                                            }
                                            None => {
                                                r.entries.remove(&n);
                                            }
                                        }
                                    });
                                };
                                let addable = cleanup_actions(&d).contains(&CleanupAction::AddAddition).then(|| {
                                    let n = d.name.clone();
                                    adding.with_untracked(|a| a.get(&n).copied()).unwrap_or_else(|| {
                                        let suggested = system.get_untracked().flatten();
                                        let fields = RwSignal::new(new_addition(&n, suggested.as_deref()));
                                        adding.update_untracked(|a| {
                                            a.insert(n, fields);
                                        });
                                        fields
                                    })
                                });
                                let adds = move || {
                                    request.with(|r| {
                                        name.with_value(|n| r.entries.get(n) == Some(&CleanupAction::AddAddition))
                                    })
                                };
                                view! {
                                    <li>
                                        <label>
                                            <code>{d.name.clone()}</code>
                                            {format!(": {} ", drift_kind_label(d.kind))}
                                            <select prop:value=current on:change=choose>
                                                <option value="">"Leave it"</option>
                                                {cleanup_actions(&d)
                                                    .into_iter()
                                                    .map(|a| view! { <option value=a.as_str()>{cleanup_action_label(a)}</option> })
                                                    .collect_view()}
                                            </select>
                                        </label>
                                        {addable.map(|value| view! {
                                            <Show when=adds>
                                                <p>"Add it as: " <AdditionFields value /></p>
                                            </Show>
                                        })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                })}
                {(!repackable.is_empty()).then(|| view! {
                    <h3>"Built files that are not TorrentZip"</h3>
                    <ul class="plain">
                        {repackable
                            .into_iter()
                            .map(|target| {
                                let t = StoredValue::new(target.clone());
                                view! {
                                    <li>
                                        <label>
                                            <input
                                                type="checkbox"
                                                prop:checked=move || request.with(|r| t.with_value(|t| r.repack.contains(t)))
                                                on:change=move |_| request.update(|r| t.with_value(|t| toggle(&mut r.repack, t)))
                                            />
                                            " Repack "
                                            <code>{target}</code>
                                            " as TorrentZip, keeping the file as it is aside"
                                        </label>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                })}
                {(!kept.is_empty()).then(|| view! {
                    <h3>"Kept files"</h3>
                    <ul class="plain">
                        {kept
                            .into_iter()
                            .map(|k| {
                                let id = k.id;
                                let what = if k.directory { "folder" } else { "file" };
                                view! {
                                    <li>
                                        <label>
                                            <input
                                                type="checkbox"
                                                prop:checked=move || request.with(|r| r.restore.contains(&id))
                                                on:change=move |_| request.update(|r| match r.restore.iter().position(|x| *x == id) {
                                                    Some(at) => {
                                                        r.restore.remove(at);
                                                    }
                                                    None => r.restore.push(id),
                                                })
                                            />
                                            {format!(" Put back the kept {what} ")}
                                            <code>{k.name.clone()}</code>
                                        </label>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                })}
                <div class="form">
                    <button
                        type="button"
                        disabled=move || busy || working.get() || request.with(PlanCleanup::is_empty)
                        on:click=plan
                    >
                        "Plan the cleanup"
                    </button>
                </div>
            }
            .into_any()
        })
    };
    view! {
        <section aria-labelledby="cleanup-title">
            <h2 id="cleanup-title">"Clean up the folder"</h2>
            <p class="lede">
                {format!(
                    "Choose what to do with what the latest check found. Keep an entry aside, and it is moved, whole, \
                     into {RETENTION}; adopt a file that holds a selected game, and it is recorded as built under that \
                     game's file name; acknowledge an entry, and it is no longer reported until it changes; add a file \
                     no DAT knows as an addition, and it is kept aside and copied into its system's \
                     additions set. Anything \
                     you do not choose is left as it is, and nothing changes until you approve the plan."
                )}
            </p>
            {form}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

fn cleanup_actions_all() -> [CleanupAction; 4] {
    [
        CleanupAction::KeepAside,
        CleanupAction::Adopt,
        CleanupAction::Acknowledge,
        CleanupAction::AddAddition,
    ]
}

/// What an approved cleanup does: each entry you chose, each kept file put back and each file repacked.
fn cleanup_view(cleanup: &Cleanup) -> AnyView {
    let chosen: Vec<_> = cleanup
        .entries
        .iter()
        .filter_map(|e| e.action.map(|a| (a, e)))
        .map(|(a, e)| {
            let to = e
                .target
                .as_ref()
                .filter(|t| **t != e.name)
                .map(|t| view! { " as " <code>{t.clone()}</code> });
            view! {
                <li>
                    <strong>{cleanup_action_label(a)}</strong>
                    " "
                    <code>{e.name.clone()}</code>
                    {to}
                </li>
            }
        })
        .collect();
    let left = cleanup
        .entries
        .iter()
        .filter(|e| e.action.is_none())
        .count();
    view! {
        <h3>"What the cleanup does"</h3>
        <ul class="plain">
            {chosen}
            {cleanup
                .restores
                .iter()
                .map(|r| view! {
                    <li>
                        <strong>{restore_kind_label(r.kind)}</strong>
                        " "
                        <code>{format!("{RETENTION}/{}/{}", r.kept.generation, r.kept.kept_name)}</code>
                        " as "
                        <code>{r.target.clone()}</code>
                    </li>
                })
                .collect_view()}
            {cleanup
                .repacks
                .iter()
                .map(|r| view! {
                    <li>
                        <strong>"Repack as TorrentZip"</strong>
                        " "
                        <code>{r.target.clone()}</code>
                        ", keeping the file as it is aside"
                    </li>
                })
                .collect_view()}
        </ul>
        {(left > 0).then(|| view! {
            <p class="note small">
                {format!(
                    "{left} other entries are left as they are. The plan holds the folder as it found it: if anything \
                     in it changes before you approve, you are asked to plan again."
                )}
            </p>
        })}
    }
    .into_any()
}

/// A job's progress on the folder, live, with a way to cancel a build while it runs.
#[component]
fn BuildProgress(job: i64) -> impl IntoView {
    let live = use_live();
    let problem = RwSignal::new(None::<String>);
    let cancelling = RwSignal::new(false);
    let cancel = move |_| {
        cancelling.set(true);
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, Job>(&format!("/jobs/{job}/cancellation"), &serde_json::json!({}))
                    .await;
            cancelling.set(false);
            match done {
                Ok(_) => problem.set(None),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <p class="notice" role="status">
            {format!("Job #{job} ")}
            {move || match live.book.with(|b| b.get(job).cloned()) {
                Some(l) if l.job.state.is_final() => {
                    let outcome = match &l.job.error {
                        Some(error) => format!("ended {}: {error}", l.job.state.as_str()),
                        None => format!("ended {}.", l.job.state.as_str()),
                    };
                    view! { <span>{outcome}</span> }.into_any()
                }
                Some(l) => {
                    let control = if l.job.kind != JobKind::Build {
                        None
                    } else if l.job.cancel_requested_at.is_some() {
                        Some(
                            view! {
                                <span class="note">
                                    " You cancelled it: the file it is writing finishes, files already in place stay, and the rest are left for a new plan."
                                </span>
                            }
                            .into_any(),
                        )
                    } else {
                        Some(
                            view! {
                                " "
                                <button type="button" disabled=move || cancelling.get() on:click=cancel>
                                    "Cancel the build"
                                </button>
                            }
                            .into_any(),
                        )
                    };
                    view! { <span>"is running. "</span> {progress_view(&l)} {control} }.into_any()
                }
                None => view! { <A href="/activity">"Follow it in Activity"</A> }.into_any(),
            }}
        </p>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// Plans building one of the bound selection profile's selections of the source set.
#[component]
fn PlanForm(folder: LibraryFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let set = folder.source_set.clone();
    let selections = LocalResource::new(move || {
        let path = format!("/selections?source_set={}", api::segment(&set));
        async move { api::get::<Vec<SelectionRecord>>(&path).await }
    });
    let folders = StoredValue::new(vec![folder]);
    let usable = move || {
        selections
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.pruned && !folders_for(&folders.get_value(), s).is_empty())
            .collect::<Vec<_>>()
    };
    let chosen = RwSignal::new(String::new());
    Effect::new(move || {
        if chosen.get_untracked().is_empty()
            && let Some(first) = usable().into_iter().next()
        {
            chosen.set(first.id.to_string());
        }
    });
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Ok(selection) = chosen.get_untracked().parse::<i64>() else {
            return;
        };
        busy.set(true);
        problem.set(None);
        leptos::task::spawn_local(async move {
            let planned = api::post::<_, Plan>(
                &format!("/libraries/{id}/plans"),
                &PlanBuild::new(selection),
            )
            .await;
            busy.set(false);
            match planned {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <section aria-labelledby="plan-title">
            <h2 id="plan-title">"Plan a build"</h2>
            <p class="lede">
                "A plan lists what a build would write into the folder from a selection and the source set's \
                latest audit. Nothing is written until you approve it."
            </p>
            {move || match selections.get() {
                Some(Ok(_)) if usable().is_empty() => view! {
                    <p class="note">
                        "This source set has no selection with this selection profile yet. Make one from the source set."
                    </p>
                }
                .into_any(),
                _ => view! {
                    <form class="form" on:submit=submit>
                        <label>
                            "Selection"
                            <select prop:value=chosen on:change=move |ev| chosen.set(event_target_value(&ev))>
                                {move || {
                                    usable()
                                        .into_iter()
                                        .map(|s| {
                                            let label = format!(
                                                "#{}, version {}, DAT revision {}, {}",
                                                s.id,
                                                s.profile.version,
                                                s.dat_revision_version,
                                                when(&s.created_at),
                                            );
                                            view! { <option value=s.id.to_string()>{label}</option> }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </label>
                        <button type="submit" class="primary" disabled=move || busy.get() || chosen.with(String::is_empty)>
                            "Plan a build"
                        </button>
                    </form>
                }
                .into_any(),
            }}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

fn summary_view(s: &PlanSummary) -> AnyView {
    let counts = [
        ("Copy", s.copy, "count verified"),
        ("Repack", s.repack, "count verified"),
        ("Adopt", s.adopt, "count verified"),
        ("Unchanged", s.unchanged, "count"),
        ("Kept aside", s.keep, "count review"),
        ("No longer selected", s.remove, "count review"),
        ("No source", s.no_source, "count review"),
        ("Conflict", s.conflict, "count review"),
        ("Unresolved", s.unresolved, "count review"),
        ("Blocked", s.blocked, "count review"),
        ("Put back", s.restore, "count"),
        ("Acknowledged", s.acknowledge, "count"),
    ];
    view! {
        <ul class="counts">
            {counts
                .into_iter()
                .map(|(what, n, class)| {
                    view! {
                        <li class=class>
                            <span class="n">{n}</span>
                            <span class="what">{what}</span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ItemCol {
    Kind,
    Game,
    Target,
    Detail,
}

#[derive(Clone, PartialEq)]
struct ItemRow(PlanItem);

fn item_detail(item: &PlanItem) -> String {
    let kept = item.keep.as_ref().map(|k| {
        format!(
            "{} is kept aside first ({}); ",
            k.name,
            keep_reason_label(k.reason)
        )
    });
    let mut kept = kept.unwrap_or_default();
    if let Some(c) = &item.converts {
        kept.push_str(&format!(
            "converted from {}, stored the other way, which is set aside once this is written; ",
            c.name
        ));
    }
    if let Some(reason) = &item.reason {
        return format!("{kept}{reason}");
    }
    let mut paths: Vec<String> = item
        .sources
        .iter()
        .map(|s| match &s.member {
            Some(member) => format!("{} ({member})", s.path),
            None => s.path.clone(),
        })
        .collect();
    paths.dedup();
    format!("{kept}{}", paths.join(", "))
}

impl Grid for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Kind,
        ItemCol::Game,
        ItemCol::Target,
        ItemCol::Detail,
    ];

    fn header(col: ItemCol) -> &'static str {
        match col {
            ItemCol::Kind => "Does",
            ItemCol::Game => "Game",
            ItemCol::Target => "File",
            ItemCol::Detail => "From, or why not",
        }
    }

    fn class(col: ItemCol) -> &'static str {
        match col {
            ItemCol::Kind => "c-state",
            ItemCol::Target | ItemCol::Detail => "c-wide data",
            ItemCol::Game => "c-wide",
        }
    }

    fn sortable(col: ItemCol) -> bool {
        matches!(col, ItemCol::Kind | ItemCol::Game)
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        let i = &self.0;
        match col {
            ItemCol::Kind => item_kind_label(i.kind).into_any(),
            ItemCol::Game => i.game.clone().into_any(),
            ItemCol::Target => i.target.clone().into_any(),
            ItemCol::Detail => item_detail(i).into_any(),
        }
    }
}

/// A plan: what it would write, what it skips, what blocks it, and the approval.
#[component]
fn PlanPanel(plan: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Plan>(&format!("/plans/{plan}")).await }
    });
    move || match loaded.get() {
        None => view! { <Loading what="the plan" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(plan)) => view! { <PlanView plan refresh /> }.into_any(),
    }
}

#[component]
fn PlanView(plan: Plan, refresh: RwSignal<u64>) -> impl IntoView {
    let id = plan.id;
    let selection = plan.content.selection;
    let digest = StoredValue::new(plan.digest.clone());
    let kind = RwSignal::new(String::new());
    let all_items = StoredValue::new(plan.content.items.clone());
    let sorting = RwSignal::new(VecDeque::<(ItemCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let wanted = ItemKind::parse(&kind.get());
        let mut items: Vec<ItemRow> = all_items.with_value(|items| {
            by_kind(items)
                .into_iter()
                .filter(|(k, _)| wanted.is_none_or(|w| w == *k))
                .flat_map(|(_, items)| items.into_iter().cloned().map(ItemRow))
                .collect()
        });
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    ItemCol::Kind => a.0.kind.cmp(&b.0.kind),
                    _ => a.0.game.cmp(&b.0.game),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<ItemRow>);
    let kinds: Vec<ItemKind> = by_kind(&plan.content.items)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let busy = RwSignal::new(false);
    let approve = move |_| {
        busy.set(true);
        let request = ApprovePlan {
            digest: digest.get_value(),
        };
        leptos::task::spawn_local(async move {
            let approved = api::post::<_, Job>(&format!("/plans/{id}/approval"), &request).await;
            busy.set(false);
            outcome.set(Some(approved));
            refresh.update(|n| *n += 1);
        });
    };
    let approvable = plan.approvable() && plan.approval.is_none();
    let writes = plan.summary.copy + plan.summary.repack;
    // Your choices are part of the plan: changing one plans again, and you review the new plan.
    let (folder, choices) = (plan.content.folder, plan.content.choices.clone());
    let replan = move |choices: PlanChoices| {
        busy.set(true);
        let request = PlanBuild { selection, choices };
        leptos::task::spawn_local(async move {
            let planned =
                api::post::<_, Plan>(&format!("/libraries/{folder}/plans"), &request).await;
            busy.set(false);
            match planned {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => outcome.set(Some(Err(message))),
            }
        });
    };
    let open = plan.approval.is_none();
    let unresolved = plan
        .content
        .blockers
        .iter()
        .any(|b| b.kind == BlockerKind::Unresolved);
    let partial = {
        let choices = choices.clone();
        (open && (unresolved || choices.resolved_only)).then(|| {
            let on = !choices.resolved_only;
            let next = PlanChoices {
                resolved_only: on,
                ..choices.clone()
            };
            let label = if on {
                "Build resolved groups only"
            } else {
                "Plan every group again"
            };
            view! {
                <div class="form">
                    <button type="button" disabled=busy on:click=move |_| replan(next.clone())>{label}</button>
                </div>
            }
        })
    };
    let conflicts: Vec<PlanItem> = plan
        .content
        .items
        .iter()
        .filter(|i| i.adoptable || i.kind == ItemKind::Adopt)
        .cloned()
        .collect();
    let adopt_choices = (open && !conflicts.is_empty()).then(|| {
        view! {
            <h3>"Conflicts you can adopt"</h3>
            <p class="note small">
                "Each of these files holds the ROMs the DAT expects at its name. Leave it, and it is skipped and the \
                folder is incomplete; adopt it, and it is recorded as built without writing anything."
            </p>
            <ul class="plain">
                {conflicts
                    .into_iter()
                    .map(|i| {
                        let adopted = i.kind == ItemKind::Adopt;
                        let next = with_adopt(&choices, &i.target, !adopted);
                        let (now, label) = if adopted {
                            ("adopted", "Leave it")
                        } else {
                            ("left as it is", "Adopt it")
                        };
                        view! {
                            <li>
                                <code>{i.target.clone()}</code>
                                {format!(": {now} ")}
                                <button type="button" disabled=busy on:click=move |_| replan(next.clone())>{label}</button>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    });

    let keepable: Vec<PlanItem> = plan
        .content
        .items
        .iter()
        .filter(|i| {
            i.keepable && i.kind != ItemKind::Adopt
                || i.keep
                    .as_ref()
                    .is_some_and(|k| k.reason == KeepReason::Conflict)
        })
        .cloned()
        .collect();
    let keep_choices = (open && !keepable.is_empty()).then(|| {
        let choices = plan.content.choices.clone();
        view! {
            <h3>"Conflicts you can keep aside"</h3>
            <p class="note small">
                "Keep a file aside, and it is moved into the folder's retention area, never deleted, and the game is \
                built in its place. You can delete kept files later, from this folder's kept files."
            </p>
            <ul class="plain">
                {keepable
                    .into_iter()
                    .map(|i| {
                        let chosen = choices.keep_aside.contains(&i.target);
                        let next = with_keep_aside(&choices, &i.target, !chosen);
                        let (now, label) = if chosen {
                            ("kept aside and rebuilt", "Leave it")
                        } else {
                            ("left as it is", "Keep it aside and rebuild")
                        };
                        view! {
                            <li>
                                <code>{i.target.clone()}</code>
                                {format!(": {now} ")}
                                <button type="button" disabled=busy on:click=move |_| replan(next.clone())>{label}</button>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    });
    let removals = (!plan.content.removals.is_empty()).then(|| {
        view! {
            <h3>"Built files no longer selected"</h3>
            <p class="note small">
                {format!(
                    "Each file kept aside is moved into {RETENTION}, never deleted; you delete kept files only by \
                     approving that separately."
                )}
            </p>
            <ul class="plain">
                {plan
                    .content
                    .removals
                    .iter()
                    .map(|r| {
                        view! {
                            <li>
                                <strong>{removal_kind_label(r.kind)}</strong>
                                " "
                                <code>{r.target.clone()}</code>
                                {format!(": {}", r.reason)}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    });
    let removes = plan
        .content
        .removals
        .iter()
        .filter(|r| r.kind == title_select_api::build::RemovalKind::Keep)
        .count();
    let cleanup = plan.content.cleanup.as_ref().map(cleanup_view);
    let is_cleanup = cleanup.is_some();
    let approve_label = match (is_cleanup, removes) {
        (true, _) => "Approve the cleanup".to_string(),
        (false, 0) => format!("Approve and build {writes} files"),
        (false, n) => format!("Approve and build {writes} files, keeping {n} aside"),
    };
    let heading = if is_cleanup {
        format!("Cleanup plan #{id}")
    } else {
        format!("Plan #{id}")
    };

    view! {
        <section aria-labelledby="latest-plan-title">
            <h2 id="latest-plan-title">{heading}</h2>
            <dl class="facts">
                <dt>"Selection"</dt>
                <dd>
                    <A href=format!("/selections/{selection}")>{format!("#{selection}")}</A>
                    {plan.previous_selection.map(|p| format!(", replacing #{p} of the current build"))}
                </dd>
                <dt>"Audit"</dt>
                <dd>
                    {match plan.content.audit {
                        Some(a) => view! { <A href=format!("/audits/{a}")>{format!("#{a}")}</A> }.into_any(),
                        None => "None".into_any(),
                    }}
                </dd>
                <dt>"DAT revision"</dt>
                <dd>
                    <code class="hash">{short_sha(&plan.content.dat_revision).to_string()}</code>
                </dd>
                <dt>"Storage"</dt>
                <dd>{crate::screens::repair::storage_line(&plan.content.storage)}</dd>
                <dt>"Planned"</dt>
                <dd>{when(&plan.created_at)}</dd>
                <dt>"Digest"</dt>
                <dd>
                    <code class="hash">{plan.digest.clone()}</code>
                </dd>
                <dt>"Approval"</dt>
                <dd>
                    {match &plan.approval {
                        Some(a) => format!("Approved by {} · {}", a.actor, when(&a.at)),
                        None => "Not approved".to_string(),
                    }}
                    {plan.job.map(|j| view! { ", build job " <A href="/activity">{format!("#{j}")}</A> })}
                </dd>
            </dl>
            {summary_view(&plan.summary)}
            {(!plan.content.notes.is_empty())
                .then(|| {
                    view! {
                        <h3>"Stored differently from the storage policy"</h3>
                        <ul class="plain">
                            {plan.content.notes.iter().map(|n| view! { <li>{n.clone()}</li> }).collect_view()}
                        </ul>
                    }
                })}
            {(!plan.content.blockers.is_empty())
                .then(|| {
                    view! {
                        <div class="notice" role="status">
                            <p>"You cannot approve this plan until these are fixed:"</p>
                            <ul class="plain">
                                {plan
                                    .content
                                    .blockers
                                    .iter()
                                    .map(|b| {
                                        let link = b
                                            .group
                                            .as_ref()
                                            .map(|g| {
                                                view! {
                                                    " " <A href=group_href(selection, g)>"Confirm a choice"</A>
                                                }
                                            });
                                        view! {
                                            <li>
                                                <strong>{blocker_label(b.kind)}</strong>
                                                {format!(": {}", b.message)}
                                                {link}
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                        </div>
                    }
                })}
            {cleanup}
            {partial}
            {adopt_choices}
            {keep_choices}
            {removals}
            {approvable
                .then(|| {
                    view! {
                        <div class="form">
                            <button type="button" class="primary" disabled=busy on:click=approve>
                                {approve_label.clone()}
                            </button>
                        </div>
                    }
                })}
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => view! { <BuildProgress job=job.id /> }.into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
            {(!plan.changes.is_empty())
                .then(|| {
                    view! {
                        <h3>"What changed since the current build"</h3>
                        <ul class="plain">
                            {plan
                                .changes
                                .iter()
                                .map(|c| {
                                    let what = match (c.before, c.after) {
                                        (None, _) => "added",
                                        (_, None) => "gone",
                                        _ => "changed",
                                    };
                                    view! {
                                        <li>
                                            {format!("{}: {what}, by {}", c.title, cause_label(c.cause))}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                })}
            {(!is_cleanup).then(|| view! {
            <h3>"Files"</h3>
            <ul class="plain small">
                {kinds
                    .iter()
                    .map(|k| view! { <li><strong>{item_kind_label(*k)}</strong>{format!(": {}", item_kind_note(*k))}</li> })
                    .collect_view()}
            </ul>
            <form class="form" on:submit=|ev| ev.prevent_default()>
                <label>
                    "Show"
                    <select prop:value=kind on:change=move |ev| kind.set(event_target_value(&ev))>
                        <option value="">"Every file"</option>
                        {kinds
                            .iter()
                            .map(|k| view! { <option value=k.as_str()>{item_kind_label(*k)}</option> })
                            .collect_view()}
                    </select>
                </label>
            </form>
            <DataTable
                id="plan-items"
                label="Plan items".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_: ItemRow| {})
                empty="The plan has no files.".to_string()
            />
            })}
        </section>
    }
}

/// Rebinds the folder to another source set of the same DAT, or to another selection profile.
#[component]
fn ChangeBinding(folder: LibraryFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let system = folder.system.clone();
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let set = RwSignal::new(folder.source_set.clone());
    let profile = RwSignal::new(folder.profile.clone());
    let (was_set, was_profile) = (folder.source_set.clone(), folder.profile.clone());
    let problem = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (s, p) = (set.get_untracked(), profile.get_untracked());
        let request = ChangeLibraryFolder {
            source_set: (s != was_set).then_some(s),
            profile: (p != was_profile).then_some(p),
        };
        leptos::task::spawn_local(async move {
            match api::patch::<_, LibraryFolder>(&format!("/libraries/{id}"), &request).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <section aria-labelledby="binding-title">
            <h2 id="binding-title">"Change what it builds"</h2>
            <p class="lede">
                "The next plan builds the new source set's or selection profile's selections. Files already built \
                stay until a build replaces them."
            </p>
            <form class="form" on:submit=submit>
                <label>
                    "Source set"
                    <select prop:value=set on:change=move |ev| set.set(event_target_value(&ev))>
                        {move || {
                            let system = system.clone();
                            sets.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(move |s| s.dat == system)
                                .map(|s| view! { <option value=s.name.clone()>{s.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Selection profile"
                    <select prop:value=profile on:change=move |ev| profile.set(event_target_value(&ev))>
                        {move || {
                            profiles
                                .get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|p| p.latest.is_some())
                                .map(|p| view! { <option value=p.name.clone()>{p.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <button type="submit">"Change"</button>
            </form>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

fn build_line(b: BuildSummary) -> impl IntoView {
    view! {
        <li>
            <A href=format!("/builds/{}", b.id)>{format!("Build #{}", b.id)}</A>
            {format!(
                ", {}, selection #{}, {}: {} written, {} failed, {} not written",
                build_state_label(b.state).to_lowercase(),
                b.selection,
                when(b.finished_at.as_deref().unwrap_or(&b.started_at)),
                b.applied,
                b.failed,
                b.pending,
            )}
        </li>
    }
}

#[component]
fn Builds(folder: i64) -> impl IntoView {
    let live = use_live();
    let builds = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuildSummary>>(&format!("/libraries/{folder}/builds")).await }
    });
    view! {
        <section aria-labelledby="builds-title">
            <h2 id="builds-title">"Builds"</h2>
            {move || match builds.get() {
                None => view! { <Loading what="the builds" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(list)) if list.is_empty() => {
                    view! { <p class="note">"Not built yet."</p> }.into_any()
                }
                Some(Ok(list)) => view! {
                    <ul class="plain">{list.into_iter().map(build_line).collect_view()}</ul>
                }
                .into_any(),
            }}
        </section>
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileCol {
    Target,
    Game,
    Kind,
    Build,
    Size,
}

#[derive(Clone, PartialEq)]
struct FileRow(BuiltFile);

impl Grid for FileRow {
    type Col = FileCol;
    const COLUMNS: &'static [FileCol] = &[
        FileCol::Target,
        FileCol::Game,
        FileCol::Kind,
        FileCol::Build,
        FileCol::Size,
    ];

    fn header(col: FileCol) -> &'static str {
        match col {
            FileCol::Target => "File",
            FileCol::Game => "Game",
            FileCol::Kind => "Made by",
            FileCol::Build => "Build",
            FileCol::Size => "Bytes",
        }
    }

    fn class(col: FileCol) -> &'static str {
        match col {
            FileCol::Target => "c-wide data",
            FileCol::Game => "c-wide",
            FileCol::Kind => "c-state",
            FileCol::Build => "c-id",
            FileCol::Size => "c-num",
        }
    }

    fn sortable(col: FileCol) -> bool {
        matches!(col, FileCol::Target | FileCol::Build)
    }

    fn cell(&self, col: FileCol) -> AnyView {
        let f = &self.0;
        match col {
            FileCol::Target => f.target.clone().into_any(),
            FileCol::Game => f.game.clone().into_any(),
            FileCol::Kind => item_kind_label(f.kind).into_any(),
            FileCol::Build => {
                view! { <A href=format!("/builds/{}", f.build)>{format!("#{}", f.build)}</A> }
                    .into_any()
            }
            FileCol::Size => f.file.size.to_string().into_any(),
        }
    }
}

#[component]
fn BuiltFiles(folder: i64) -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuiltFile>>(&format!("/libraries/{folder}/files")).await }
    });
    let sorting = RwSignal::new(VecDeque::<(FileCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items: Vec<FileRow> = files
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(FileRow)
            .collect();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    FileCol::Build => a.0.build.cmp(&b.0.build),
                    _ => a.0.target.cmp(&b.0.target),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<FileRow>);
    view! {
        <section aria-labelledby="files-title">
            <h2 id="files-title">"Built files"</h2>
            <p class="lede">"Each file the store built into the folder, with the build that wrote it."</p>
            {move || match files.get() {
                None => view! { <Loading what="the built files" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            <DataTable
                id="built-files"
                label="Built files".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_: FileRow| {})
                empty="No files built yet.".to_string()
            />
        </section>
    }
}

/// One build: what it wrote, and what failed.
#[component]
pub fn BuildScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let live = use_live();
    let detail = LocalResource::new(move || {
        live.settled.track();
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<BuildDetail>(&format!("/builds/{id}")).await,
                None => Err("That is not a build number.".into()),
            }
        }
    });
    move || match detail.get() {
        None => view! { <Loading what="the build" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Build"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(detail)) => build_view(detail).into_any(),
    }
}

fn build_item_line(item: BuildItem) -> impl IntoView {
    view! {
        <li>
            <code>{item.target}</code>
            {format!(
                ": {}, {}",
                item_kind_label(item.kind).to_lowercase(),
                item_state_label(item.state).to_lowercase(),
            )}
            {item.error.map(|e| view! { <span class="failed">{format!(": {e}")}</span> })}
        </li>
    }
}

fn build_view(detail: BuildDetail) -> impl IntoView {
    let b = detail.build;
    view! {
        <h1>{format!("Build #{}", b.id)}</h1>
        <dl class="facts">
            <dt>"Library folder"</dt>
            <dd>
                <A href=folder_href(b.folder)>{format!("#{}", b.folder)}</A>
            </dd>
            <dt>"State"</dt>
            <dd>{build_state_label(b.state)}</dd>
            <dt>"Plan"</dt>
            <dd>{format!("#{}", b.plan)}</dd>
            <dt>"Selection"</dt>
            <dd>
                <A href=format!("/selections/{}", b.selection)>{format!("#{}", b.selection)}</A>
            </dd>
            <dt>"Audit"</dt>
            <dd>
                <A href=format!("/audits/{}", b.audit)>{format!("#{}", b.audit)}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <code class="hash">{b.dat_revision.clone()}</code>
            </dd>
            <dt>"Generation"</dt>
            <dd>{b.generation}</dd>
            <dt>"Started"</dt>
            <dd>{when(&b.started_at)}</dd>
            <dt>"Finished"</dt>
            <dd>{b.finished_at.as_deref().map(when).unwrap_or_default()}</dd>
        </dl>
        <ul class="counts">
            <li class="count verified">
                <span class="n">{b.applied}</span>
                <span class="what">"Written"</span>
            </li>
            <li class="count review">
                <span class="n">{b.failed}</span>
                <span class="what">"Failed"</span>
            </li>
            <li class="count">
                <span class="n">{b.pending}</span>
                <span class="what">"Not written"</span>
            </li>
        </ul>
        <section aria-labelledby="items-title">
            <h2 id="items-title">"Files"</h2>
            {if detail.items.is_empty() {
                view! { <p class="note">"The build had no files to write."</p> }.into_any()
            } else {
                view! { <ul class="plain">{detail.items.into_iter().map(build_item_line).collect_view()}</ul> }
                    .into_any()
            }}
        </section>
    }
}

/// On a selection's screen: plans building it into a library folder bound to its source set and profile.
#[component]
pub fn PlanABuild(selection: SelectionRecord) -> impl IntoView {
    let folders = LocalResource::new(|| api::get::<Vec<LibraryFolder>>("/libraries"));
    let selection = StoredValue::new(selection);
    let problem = RwSignal::new(None::<String>);
    let go = navigator();
    let plan = move |folder: i64| {
        let id = selection.with_value(|s| s.id);
        leptos::task::spawn_local(async move {
            let planned =
                api::post::<_, Plan>(&format!("/libraries/{folder}/plans"), &PlanBuild::new(id))
                    .await;
            match planned {
                Ok(_) => go.run(folder_href(folder)),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        {move || {
            let all = folders.get().and_then(Result::ok).unwrap_or_default();
            let matching: Vec<LibraryFolder> = selection
                .with_value(|s| folders_for(&all, s).into_iter().cloned().collect());
            if matching.is_empty() {
                return view! {
                    <p class="note">
                        "To build this selection for your front end, "
                        <A href="/libraries">"register a library folder"</A>
                        " with its source set and selection profile."
                    </p>
                }
                .into_any();
            }
            view! {
                <div class="form">
                    {matching
                        .into_iter()
                        .map(|f| {
                            let label = format!("Plan a build into {}", location(&f));
                            let id = f.id;
                            view! {
                                <button type="button" on:click=move |_| plan(id)>
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            }
            .into_any()
        }}
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// The files builds kept aside in the folder's retention area, by generation, and deleting the ones you pick by
/// approving a plan that lists them.
#[component]
fn KeptFiles(folder: i64, busy: bool, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<Vec<KeptFile>>(&format!("/libraries/{folder}/kept-files")).await }
    });
    let picked = RwSignal::new(Vec::<i64>::new());
    let plan = RwSignal::new(None::<KeptDeletion>);
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    let request = move |all: bool| {
        working.set(true);
        let body = PlanKeptDeletion {
            ids: if all {
                Vec::new()
            } else {
                picked.get_untracked()
            },
            all,
        };
        leptos::task::spawn_local(async move {
            let planned =
                api::post::<_, KeptDeletion>(&format!("/libraries/{folder}/kept-deletions"), &body)
                    .await;
            working.set(false);
            match planned {
                Ok(p) => {
                    problem.set(None);
                    plan.set(Some(p));
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let approve = move |_| {
        let Some(p) = plan.get_untracked() else {
            return;
        };
        working.set(true);
        let body = ApprovePlan {
            digest: p.digest.clone(),
        };
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, KeptDeletion>(&format!("/kept-deletions/{}/approval", p.id), &body)
                    .await;
            working.set(false);
            match done {
                Ok(p) => {
                    picked.set(Vec::new());
                    plan.set(Some(p));
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let list = move || match files.get() {
        None => view! { <Loading what="the kept files" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(files)) if files.is_empty() => {
            view! { <p class="note">"No files are kept aside."</p> }.into_any()
        }
        Some(Ok(files)) => {
            let mut generations: Vec<(u64, Vec<KeptFile>)> = Vec::new();
            for f in files {
                match generations.last_mut() {
                    Some((g, list)) if *g == f.generation => list.push(f),
                    _ => generations.push((f.generation, vec![f])),
                }
            }
            view! {
                {generations
                    .into_iter()
                    .map(|(generation, files)| {
                        view! {
                            <h3>{format!("Kept by generation {generation}")}</h3>
                            <ul class="plain">
                                {files
                                    .into_iter()
                                    .map(|f| {
                                        let id = f.id;
                                        let checked = move || picked.with(|p| p.contains(&id));
                                        view! {
                                            <li>
                                                <label>
                                                    <input
                                                        type="checkbox"
                                                        prop:checked=checked
                                                        on:change=move |_| {
                                                            picked.update(|p| {
                                                                if let Some(at) = p.iter().position(|x| *x == id) {
                                                                    p.remove(at);
                                                                } else {
                                                                    p.push(id);
                                                                }
                                                            })
                                                        }
                                                    />
                                                    " "
                                                    <code>{f.name.clone()}</code>
                                                </label>
                                                {format!(
                                                    " as {RETENTION}/{}/{}: {}, {} bytes",
                                                    f.generation,
                                                    f.kept_name,
                                                    keep_reason_label(f.reason),
                                                    f.file.size,
                                                )}
                                                {f.game.clone().map(|g| format!(", {g}"))}
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                        }
                    })
                    .collect_view()}
                <div class="form">
                    <button
                        type="button"
                        disabled=move || busy || working.get() || picked.with(Vec::is_empty)
                        on:click=move |_| request(false)
                    >
                        "Plan deleting the files you picked"
                    </button>
                    <button type="button" disabled=move || busy || working.get() on:click=move |_| request(true)>
                        "Plan deleting every kept file"
                    </button>
                </div>
            }
            .into_any()
        }
    };
    let review = move || {
        plan.get().map(|p| {
            let open = p.approval.is_none();
            view! {
                <h3>{format!("Deleting kept files, plan #{}", p.id)}</h3>
                <p>
                    "Digest " <code class="hash">{p.digest.clone()}</code>
                </p>
                <ul class="plain">
                    {p
                        .content
                        .files
                        .iter()
                        .map(|f| {
                            let outcome = p.outcomes.iter().find(|o| o.id == f.id).map(|o| match (
                                o.deleted,
                                &o.detail,
                            ) {
                                (true, _) => ": deleted".to_string(),
                                (false, Some(d)) => format!(": {d}"),
                                (false, None) => ": left as it is".to_string(),
                            });
                            // Deleted only while it still reads as this; otherwise its outcome says it changed.
                            let read = p.content.reviewed.iter().find(|r| r.id == f.id).map(|r| {
                                view! { ", read as SHA-256 " <code class="hash">{r.sha256.clone()}</code> }
                            });
                            view! {
                                <li>
                                    <code>{format!("{RETENTION}/{}/{}", f.generation, f.kept_name)}</code>
                                    {read}
                                    {outcome}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
                {open
                    .then(|| {
                        view! {
                            <div class="form">
                                <button
                                    type="button"
                                    class="primary"
                                    disabled=move || busy || working.get()
                                    on:click=approve
                                >
                                    {format!("Approve and delete {} kept files", p.content.files.len())}
                                </button>
                            </div>
                        }
                    })}
            }
        })
    };
    view! {
        <section aria-labelledby="kept-title">
            <h2 id="kept-title">"Kept files"</h2>
            <p class="lede">
                "Files builds moved aside instead of deleting: removed, replaced, or conflicts you chose to keep aside. \
                Nothing here is deleted until you approve it."
            </p>
            {list}
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}
