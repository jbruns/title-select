//! The Libraries pages: every Library Folder, with its status and quick actions; a new folder, from its Source Set
//! or one of its Selections; and a folder's page, in tabs. A folder's latest plan shows in its Plan tab through
//! `library_plan`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::{A, Outlet};
use leptos_router::hooks::{use_params_map, use_query_map};
use title_select_api::additions::NewAddition;
use title_select_api::build::{
    AddLibraryFolder, ApprovePlan, BuildDetail, BuildItem, BuildSummary, BuiltFile,
    ChangeLibraryFolder, CleanupAction, FileOutcome, FolderCheck, Incomplete, KeptDeletion,
    KeptFile, LibraryFolder, Plan, PlanBuild, PlanChoices, PlanCleanup, PlanKeptDeletion,
    RETENTION,
};
use title_select_api::{
    Dat, ErrorBody, Job, JobKind, JobState, PathTemplateDefaults, ProfileRecord, Root,
    SelectionRecord, SourceSetRecord, StartSelection,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::build_model::{
    build_state_label, cleanup_action_label, cleanup_actions, drift_kind_label, file_outcome_label,
    folders_for, incomplete_counts, incomplete_reason_label, item_kind_label, item_state_label,
    keep_reason_label, unfinished_label,
};
use crate::events::{Live, use_live};
use crate::library_model::{
    FolderTab, Pick, Replan, Replans, drifted, fresh, live_status, pick, selection_under_way,
    stale_label,
};
use crate::model::{age, earlier, when};
use crate::parts::icon::{Glyph, LabelledIcon, Svg};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, List, ListRow, RowAction, Selected, ToolAction, Width, last_good_rows,
};
use crate::parts::pending::{ActionPending, Pending, action_problem};
use crate::parts::status::{BuildKind, LibraryStatus, Status, StatusBadge, StatusIcon};
use crate::registration;
use crate::screens::additions::{AdditionFields, new_addition};
use crate::screens::jobs::progress_view;
use crate::screens::library_plan::LibraryPlan;

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

fn tab_href(id: i64, tab: FolderTab) -> String {
    format!("/libraries/{id}?tab={}", tab.key())
}

fn set_href(name: &str) -> String {
    format!("/source-sets/{}", api::segment(name))
}

/// Why the folder has its status, beyond its status alone: one line each.
fn reasons(folder: &LibraryFolder) -> AnyView {
    let mut lines: Vec<String> = Vec::new();
    if folder.plan_open {
        lines.push("A plan waits on your approval.".into());
    }
    if drifted(folder) {
        lines.push("Its latest check found files that differ from its build.".into());
    }
    if let Some(counts) = &folder.incomplete {
        lines.push(format!("Incomplete: {}.", incomplete_counts(counts)));
    }
    if let Some(u) = &folder.unfinished {
        lines.push(format!("{}.", unfinished_label(u)));
    }
    lines.extend(folder.stale.iter().map(stale_label));
    if lines.is_empty() {
        return ().into_any();
    }
    view! {
        <ul class="plain reasons">
            {lines.into_iter().map(|l| view! { <li>{l}</li> }).collect_view()}
        </ul>
    }
    .into_any()
}

/// A trail of links to the pages above this one.
#[component]
fn Crumbs(trail: Vec<(String, String)>, #[prop(into)] here: String) -> impl IntoView {
    view! {
        <nav class="crumbs" aria-label="Breadcrumb">
            <ol>
                {trail
                    .into_iter()
                    .map(|(href, label)| {
                        view! {
                            <li>
                                <A href=href>{label}</A>
                                <Svg glyph=Glyph::ChevronRight />
                            </li>
                        }
                    })
                    .collect_view()}
                <li aria-current="page">{here}</li>
            </ol>
        </nav>
    }
}

/// A request to plan a build of a folder: of `selection`, or else of the newest fresh selection, made first when there
/// is none.
#[derive(Debug, Clone)]
struct MakePlan {
    /// Which request it is, to tell its answer from one a later request superseded.
    replan: Replan,
    folder: i64,
    selection: Option<i64>,
    choices: PlanChoices,
}

/// Makes build plans for the Libraries pages, so that a plan started from the list or a new folder shows as under way
/// in the folder's Plan tab.
#[derive(Clone, Copy)]
pub(crate) struct Planner {
    action: Action<MakePlan, Result<Plan, String>>,
    status: RwSignal<String>,
    folder: RwSignal<Option<i64>>,
    /// An older selection the last plan could build instead, offered once making a fresh one failed.
    older: RwSignal<Option<OlderSelection>>,
    /// What the last choice set in bulk did, and in which folder: the plan it replaced is gone.
    pub(crate) said: RwSignal<Option<(i64, String)>>,
    replans: StoredValue<Replans>,
    /// Bumped when the answer to a superseded request lands, so that the pages show the plan as the server holds it.
    dropped: RwSignal<u64>,
}

/// A selection of a folder made before its profile's newest version or its source set's latest DAT revision, which
/// the Operator may choose to plan from when a fresh one can't be made.
#[derive(Debug, Clone, PartialEq)]
struct OlderSelection {
    folder: i64,
    selection: i64,
    label: String,
}

impl OlderSelection {
    fn of(folder: i64, s: &SelectionRecord) -> Self {
        let made = if s.profile.version < s.latest_profile_version {
            format!(
                "profile version {} (the profile is at version {})",
                s.profile.version, s.latest_profile_version
            )
        } else {
            format!(
                "DAT revision {}, not the latest audited",
                s.dat_revision_version
            )
        };
        OlderSelection {
            folder,
            selection: s.id,
            label: format!(
                "Plan from older selection #{} instead, made with {made}",
                s.id
            ),
        }
    }
}

impl Planner {
    fn new() -> Self {
        let status = RwSignal::new(String::new());
        let older = RwSignal::new(None);
        let replans = StoredValue::new(Replans::default());
        let dropped = RwSignal::new(0);
        // The action keeps only the answer to the latest request; the pages reload when it drops one.
        let action = Action::new_local(move |request: &MakePlan| {
            let request = request.clone();
            async move {
                let replan = request.replan;
                let made = make_plan(request, status, older).await;
                if !replans.with_value(|r| r.is_latest(replan)) {
                    dropped.update(|n| *n += 1);
                }
                made
            }
        });
        Planner {
            action,
            status,
            folder: RwSignal::new(None),
            older,
            said: RwSignal::new(None),
            replans,
            dropped,
        }
    }

    pub(crate) fn start(self, folder: i64, selection: Option<i64>, choices: PlanChoices) {
        self.folder.set(Some(folder));
        self.older.set(None);
        self.said.set(None);
        self.status.set("Making plan…".into());
        let replan = self
            .replans
            .try_update_value(Replans::ask)
            .unwrap_or_default();
        self.action.dispatch(MakePlan {
            replan,
            folder,
            selection,
            choices,
        });
    }

    /// What the last choice set in bulk did in the folder.
    pub(crate) fn said(self, folder: i64) -> Signal<Option<String>> {
        Signal::derive(move || {
            self.said
                .get()
                .and_then(|(f, s)| (f == folder).then_some(s))
        })
    }

    /// Subscribes to the plans made, and to answers dropped for a later request's.
    fn track(self) {
        self.action.version().track();
        self.dropped.track();
    }

    pub(crate) fn pending(self, folder: i64) -> Signal<bool> {
        Signal::derive(move || self.action.pending().get() && self.folder.get() == Some(folder))
    }

    /// The older selection offered for the folder once its plan failed for want of a fresh one.
    fn older(self, folder: i64) -> Signal<Option<OlderSelection>> {
        Signal::derive(move || {
            self.older
                .get()
                .filter(|o| o.folder == folder && !self.action.pending().get())
        })
    }

    fn problem(self, folder: i64) -> Signal<Option<String>> {
        let problem = action_problem(self.action);
        Signal::derive(move || {
            if self.folder.get() == Some(folder) {
                problem.get()
            } else {
                None
            }
        })
    }
}

/// Provides the `Planner` for the whole workbench, so that a plan under way outlives the page that started it.
pub fn provide_planner() {
    provide_context(Planner::new());
}

pub(crate) fn use_planner() -> Planner {
    expect_context::<Planner>()
}

async fn make_plan(
    request: MakePlan,
    status: RwSignal<String>,
    older: RwSignal<Option<OlderSelection>>,
) -> Result<Plan, String> {
    let selection = match request.selection {
        Some(selection) => selection,
        None => {
            status.set("Finding a selection…".into());
            let folder: LibraryFolder = api::get(&format!("/libraries/{}", request.folder)).await?;
            fresh_selection(&folder, status, older).await?
        }
    };
    status.set("Making plan…".into());
    let body = PlanBuild {
        selection,
        choices: request.choices,
    };
    api::post(&format!("/libraries/{}/plans", request.folder), &body).await
}

/// The folder's newest fresh selection, made now when there is none. When making one fails, or makes one that isn't
/// fresh after all, the newest older one is offered in `older`, never taken silently.
async fn fresh_selection(
    folder: &LibraryFolder,
    status: RwSignal<String>,
    older: RwSignal<Option<OlderSelection>>,
) -> Result<i64, String> {
    let set = api::segment(&folder.source_set);
    let selections: Vec<SelectionRecord> =
        api::get(&format!("/selections?source_set={set}")).await?;
    let record: SourceSetRecord = api::get(&format!("/source-sets/{set}")).await?;
    let verified = record
        .last_verified_against
        .as_ref()
        .map(|v| v.sha256.as_str());
    let fallback = match pick(&selections, folder, verified) {
        Pick::Fresh(id) => return Ok(id),
        Pick::Older(id) => selections
            .iter()
            .find(|s| s.id == id)
            .map(|s| OlderSelection::of(folder.id, s)),
        Pick::None => None,
    };
    let profile: ProfileRecord =
        api::get(&format!("/profiles/{}", api::segment(&folder.profile))).await?;
    let newest = profile.latest.map(|v| v.version);
    let made = match make_selection(folder, status, newest, verified).await {
        Ok(id) => api::get::<SelectionRecord>(&format!("/selections/{id}")).await,
        Err(message) => Err(message),
    };
    match made {
        Ok(s) if fresh(&s, folder, verified) => Ok(s.id),
        Ok(s) => {
            older.set(Some(OlderSelection::of(folder.id, &s)));
            Err(format!(
                "Selection #{} isn't of the profile's newest version on the latest audited DAT revision.",
                s.id
            ))
        }
        Err(message) => {
            older.set(fallback);
            Err(message)
        }
    }
}

/// A selection of the folder's source set with its profile's `newest` version on the `verified` DAT revision: the one a
/// selection job already queued or running for them makes, or else a new job's. Its caller checks it is fresh, since
/// the job the Service names when it refuses a new one may run something older.
async fn make_selection(
    folder: &LibraryFolder,
    status: RwSignal<String>,
    newest: Option<u32>,
    verified: Option<&str>,
) -> Result<i64, String> {
    let wait = |job: i64| {
        status.set(format!("Waiting for selection job #{job}…"));
        follow(job)
    };
    let jobs: Vec<Job> = api::get("/jobs").await?;
    if let Some(job) = selection_under_way(&jobs, folder, newest, verified) {
        return wait(job).await;
    }
    status.set("Making selection…".into());
    let start = StartSelection {
        source_set: folder.source_set.clone(),
        revision: None,
        profile: folder.profile.clone(),
        version: None,
    };
    match api::post_refusable::<_, Job>("/selections", &start).await {
        Ok(job) => wait(job.id).await,
        Err(ErrorBody {
            error,
            job: Some(job),
        }) => match api::get::<Job>(&format!("/jobs/{job}")).await {
            Ok(j) if j.kind == JobKind::Selection => wait(job).await,
            _ => Err(format!("Couldn't make a fresh selection: {error}")),
        },
        Err(ErrorBody { error, job: None }) => {
            Err(format!("Couldn't make a fresh selection: {error}"))
        }
    }
}

/// The selection a selection job made, once it ends.
async fn follow(job: i64) -> Result<i64, String> {
    loop {
        sleep(1000).await;
        let j: Job = api::get(&format!("/jobs/{job}")).await?;
        if j.state.is_final() {
            return match (j.state, j.selection) {
                (JobState::Succeeded, Some(id)) => Ok(id),
                _ => Err(j.error.unwrap_or_else(|| {
                    format!("Selection job #{job} ended {}.", j.state.as_str())
                })),
            };
        }
    }
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Starts a check of the folder, and says how that went in `said`.
fn check(folder: i64, what: String, said: RwSignal<Option<Result<String, String>>>) {
    leptos::task::spawn_local(async move {
        let started = api::post::<_, Job>(&format!("/libraries/{folder}/checks"), &()).await;
        said.set(Some(match started {
            Ok(job) => Ok(format!("Checking {what}: job #{}.", job.id)),
            Err(message) => Err(format!("Couldn't check {what}: {message}")),
        }));
    });
}

fn said_view(said: RwSignal<Option<Result<String, String>>>) -> impl IntoView {
    move || {
        said.get().map(|said| match said {
            Ok(text) => view! { <p class="done" role="status">{text}</p> }.into_any(),
            Err(message) => view! { <Problem message /> }.into_any(),
        })
    }
}

/// The jobs the live job book holds, so a folder shows Running as soon as a job for it starts, wherever it was started.
pub(crate) fn live_jobs(live: Live) -> Vec<Job> {
    live.book
        .with(|b| b.newest_first().into_iter().map(|l| l.job).collect())
}

/// The Libraries pages share one clock.
#[component]
pub fn LibrariesPages() -> impl IntoView {
    provide_now();
    view! { <Outlet /> }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderCol {
    Status,
    Library,
    SourceSet,
    Profile,
    Build,
    Files,
}

#[derive(Clone, PartialEq)]
struct FolderRow(LibraryFolder, LibraryStatus);

impl ListRow for FolderRow {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Status,
        FolderCol::Library,
        FolderCol::SourceSet,
        FolderCol::Profile,
        FolderCol::Build,
        FolderCol::Files,
    ];

    fn column(col: FolderCol) -> Column {
        match col {
            FolderCol::Status => Column::new("status", "Status", Width::Fixed(4.0))
                .sortable()
                .hint("Where the folder stands"),
            FolderCol::Library => Column::new(
                "library",
                "Library folder",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            FolderCol::SourceSet => Column::new(
                "source-set",
                "Source set",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            FolderCol::Profile => {
                Column::new("profile", "Profile", Width::Flex { min: 8.0, share: 1 }).sortable()
            }
            FolderCol::Build => Column::new("build", "Build", Width::Fixed(6.0))
                .sortable()
                .hint("Its current build"),
            FolderCol::Files => Column::new("files", "Files", Width::Fixed(5.0))
                .sortable()
                .numeric(),
        }
    }

    fn key(&self) -> String {
        self.0.id.to_string()
    }

    fn title(&self) -> String {
        location(&self.0)
    }

    fn search(&self) -> String {
        let f = &self.0;
        format!(
            "{} {} {} {}",
            location(f),
            f.system,
            f.source_set,
            f.profile
        )
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        let f = &self.0;
        match col {
            FolderCol::Status => view! { <StatusIcon status=self.1 tab_stop=false /> }.into_any(),
            FolderCol::Library => view! { <Clip text=location(f) /> }.into_any(),
            FolderCol::SourceSet => view! { <Clip text=f.source_set.clone() /> }.into_any(),
            FolderCol::Profile => view! { <Clip text=f.profile.clone() /> }.into_any(),
            FolderCol::Build => match &f.build {
                Some(b) => format!("#{}", b.id).into_any(),
                None => "".into_any(),
            },
            FolderCol::Files => f.files.to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: FolderCol) -> Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            FolderCol::Status => self.1.rank().cmp(&other.1.rank()),
            FolderCol::Library => location(a).cmp(&location(b)),
            FolderCol::SourceSet => a.source_set.cmp(&b.source_set),
            FolderCol::Profile => a.profile.cmp(&b.profile),
            FolderCol::Build => a
                .build
                .as_ref()
                .map(|b| b.id)
                .cmp(&b.build.as_ref().map(|b| b.id)),
            FolderCol::Files => a.files.cmp(&b.files),
        }
    }

    fn href(&self) -> Option<String> {
        Some(folder_href(self.0.id))
    }
}

/// What the folder is bound to and where it stands.
fn folder_facts(f: &LibraryFolder) -> AnyView {
    let writable = if f.writable {
        ", writable"
    } else {
        ", not writable now"
    };
    let checked = last_checked(f, true);
    let (set, profile) = (f.source_set.clone(), f.profile.clone());
    let profile_href = format!("/profiles/{}", api::segment(&profile));
    view! {
        <dl class="facts">
            <dt>"System"</dt>
            <dd>{f.system.clone()}</dd>
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href(&set)>{set.clone()}</A>
            </dd>
            <dt>"Selection profile"</dt>
            <dd>
                <A href=profile_href>{profile}</A>
            </dd>
            <dt>"Root"</dt>
            <dd>
                {f.root.clone()}
                {writable}
            </dd>
            <dt>"Current build"</dt>
            <dd>{current_build(&f.build)}</dd>
            <dt>"Last checked"</dt>
            <dd>{checked}</dd>
            <dt>"Files"</dt>
            <dd>{f.files}</dd>
        </dl>
    }
    .into_any()
}

fn folder_detail(row: FolderRow) -> AnyView {
    let (f, now) = (row.0, row.1);
    view! {
        <p>
            <StatusBadge status=now />
        </p>
        {reasons(&f)}
        {folder_facts(&f)}
    }
    .into_any()
}

/// Every library folder, with its status and its quick actions.
#[component]
pub fn Libraries() -> impl IntoView {
    let live = use_live();
    let planner = use_planner();
    let folders = LocalResource::new(move || {
        live.settled.track();
        planner.track();
        api::get::<Vec<LibraryFolder>>("/libraries")
    });
    let jobs = Memo::new(move |_| live_jobs(live));
    let rows = last_good_rows(move || {
        let jobs = jobs.get();
        folders.get().map(|r| {
            r.map(|f| {
                f.into_iter()
                    .map(|f| {
                        let now = live_status(&f, &jobs);
                        FolderRow(f, now)
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.1)));
    let said = RwSignal::new(None::<Result<String, String>>);
    let go = navigator();
    let actions = vec![
        ToolAction::page(Glyph::Plus, "New library folder", move || {
            go.run("/libraries/new".to_string())
        }),
        ToolAction::checked(
            Glyph::FileCheck,
            "Check",
            move |chosen: Selected<FolderRow>| {
                for row in chosen.rows {
                    check(row.0.id, location(&row.0), said);
                }
            },
        ),
    ];
    let row_actions = Callback::new(move |row: FolderRow| {
        let (id, what) = (row.0.id, location(&row.0));
        vec![
            RowAction::run(Glyph::FileCheck, "Check", move || {
                check(id, what.clone(), said)
            }),
            RowAction::run(Glyph::ClipboardList, "Plan build", move || {
                planner.start(id, None, PlanChoices::default());
                go.run(tab_href(id, FolderTab::Plan));
            }),
        ]
    });

    view! {
        <h1>"Libraries"</h1>
        <p class="lede">"Folders you build a source set's selections into, for your front end to play from."</p>
        {move || match folders.get() {
            None => view! { <Loading what="the library folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        {said_view(said)}
        <List
            id="libraries"
            label="Library folders"
            rows
            empty="No library folders yet. Choose New library folder to make one."
            placeholder="Filter by folder, system, source set or profile"
            actions
            row_actions
            quick=2
            detail=Callback::new(folder_detail)
            legend=move || view! { <Legend statuses list="libraries" /> }
        />
    }
}

/// A new library folder: of a source set, or of one of its selections, with a profile and a writable root you pick and
/// the path the template proposes. Once made, its first build plan is made.
#[component]
pub fn NewLibraryFolder() -> impl IntoView {
    let query = use_query_map();
    let from_set = query.with_untracked(|q| q.get("source_set"));
    let from_selection =
        query.with_untracked(|q| q.get("selection").and_then(|s| s.parse::<i64>().ok()));
    let planner = use_planner();
    let go = navigator();
    let chosen = LocalResource::new(move || async move {
        match from_selection {
            Some(id) => api::get::<SelectionRecord>(&format!("/selections/{id}"))
                .await
                .map(Some),
            None => Ok(None),
        }
    });
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
            .filter(|r| r.writable && !r.ingest)
            .collect::<Vec<_>>()
    };
    let existing = LocalResource::new(|| api::get::<Vec<LibraryFolder>>("/libraries"));
    let saved = move || {
        profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.latest.is_some())
            .collect::<Vec<_>>()
    };
    let fixed = from_set.is_some() || from_selection.is_some();
    let root = RwSignal::new(String::new());
    let set = RwSignal::new(from_set.clone().unwrap_or_default());
    let profile = RwSignal::new(String::new());
    let template = RwSignal::new(None::<String>);
    let typed = RwSignal::new(None::<String>);
    let problem = RwSignal::new(None::<String>);

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

    // The root your library folders are already in, else the first writable one.
    Effect::new(move || {
        let Some(Ok(folders)) = existing.get() else {
            return;
        };
        let roots = writable();
        if root.get_untracked().is_empty()
            && let Some(first) = roots.iter().max_by_key(|r| {
                (
                    folders.iter().filter(|f| f.root == r.name).count(),
                    std::cmp::Reverse(r.name.clone()),
                )
            })
        {
            root.set(first.name.clone());
        }
    });
    Effect::new(move || {
        if let Some(Ok(Some(selection))) = chosen.get() {
            set.set(selection.source_set);
            profile.set(selection.profile.name);
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
            && from_selection.is_none()
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

    let create = Action::new_local(move |request: &AddLibraryFolder| {
        let request = request.clone();
        async move { api::post::<_, LibraryFolder>("/libraries", &request).await }
    });
    Effect::new(move || {
        if let Some(Ok(folder)) = create.value().get() {
            planner.start(folder.id, from_selection, PlanChoices::default());
            go.run(tab_href(folder.id, FolderTab::Plan));
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
        problem.set(None);
        create.dispatch(AddLibraryFolder {
            root: root.get_untracked(),
            path: Some(chosen),
            template: None,
            source_set: set.get_untracked(),
            profile: profile.get_untracked(),
        });
    };

    let trail = match &from_set {
        Some(name) => vec![
            ("/source-sets".to_string(), "Source sets".to_string()),
            (set_href(name), name.clone()),
        ],
        None => vec![("/libraries".to_string(), "Libraries".to_string())],
    };
    view! {
        <Crumbs trail here="New library folder" />
        <h1>"New library folder"</h1>
        {move || match roots.get() {
            Some(Ok(_)) if writable().is_empty() => view! {
                <p class="note">
                    "None of your roots is writable. To build into your front end's folders, add a root for them \
                    with "<code>"writable = true"</code>" to your configuration."
                </p>
            }
            .into_any(),
            _ => ().into_any(),
        }}
        {move || match chosen.get() {
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(Some(s))) => view! {
                <p class="note">
                    "It builds selection "
                    <A href=format!("/selections/{}", s.id)>{format!("#{}", s.id)}</A>
                    {format!(" of {}, with {} version {}.", s.source_set, s.profile.name, s.profile.version)}
                </p>
            }
            .into_any(),
            _ => ().into_any(),
        }}
        <ActionPending action=create status="Making the folder…">
            <form class="form" on:submit=submit>
                {(!fixed).then(|| view! {
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
                })}
                {fixed.then(|| view! {
                    <p class="field">"Source set " <strong>{move || set.get()}</strong></p>
                })}
                <label>
                    "Selection profile"
                    <select
                        required
                        disabled=from_selection.is_some()
                        prop:value=profile
                        on:change=move |ev| profile.set(event_target_value(&ev))
                    >
                        {move || {
                            let mut names: Vec<String> = saved().into_iter().map(|p| p.name).collect();
                            let current = profile.get();
                            if !current.is_empty() && !names.contains(&current) {
                                names.push(current);
                            }
                            names
                                .into_iter()
                                .map(|n| view! { <option value=n.clone()>{n.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Writable root"
                    <select required prop:value=root on:change=move |ev| root.set(event_target_value(&ev))>
                        {move || {
                            writable()
                                .into_iter()
                                .map(|r| view! { <option value=r.name.clone()>{r.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
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
                    disabled=move || {
                        create.pending().get() || root.with(String::is_empty) || profile.with(String::is_empty)
                            || set.with(String::is_empty)
                    }
                >
                    "Create and plan a build"
                </button>
            </form>
        </ActionPending>
        <p class="note" aria-live="polite">
            {move || match preview() {
                Ok(folder) => view! { "Creates " <code>{folder}</code> }.into_any(),
                Err(reason) => reason.into_any(),
            }}
        </p>
        {move || {
            (profiles.get().is_some_and(|p| p.is_ok()) && saved().is_empty() && from_selection.is_none())
                .then(|| view! {
                    <p class="note">
                        "None of your selection profiles is saved yet. "
                        <A href="/profiles">"Save one"</A>
                        " to build with it."
                    </p>
                })
        }}
        <details class="help">
            <summary>"About the path"</summary>
            <p class="note small">
                "The path template proposes the path; you can type over it. The folder must be empty or missing: \
                making it creates it with a marker file that ties it to this store. Several library folders may \
                hold the same system, each with its own selection profile."
            </p>
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
        </details>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// One library folder's page, in tabs.
#[component]
pub fn LibraryFolderScreen() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let tab = Memo::new(move |_| query.with(|q| FolderTab::parse(q.get("tab").as_deref())));
    let live = use_live();
    let planner = use_planner();
    let refresh = RwSignal::new(0u64);
    let folder = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        planner.track();
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<LibraryFolder>(&format!("/libraries/{id}")).await,
                None => Err("That isn't a library folder number.".into()),
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
        Some(Ok(folder)) => view! { <FolderPage folder tab refresh /> }.into_any(),
    }
}

#[component]
fn FolderPage(
    folder: LibraryFolder,
    tab: Memo<FolderTab>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let id = folder.id;
    let planner = use_planner();
    let go = navigator();
    let live = use_live();
    let now = {
        let folder = folder.clone();
        Memo::new(move |_| live_status(&folder, &live_jobs(live)))
    };
    let busy = Signal::derive(move || now.get() == LibraryStatus::Running);
    let title = location(&folder);
    let said = RwSignal::new(None::<Result<String, String>>);
    let what = title.clone();
    let start_check = move |_| check(id, what.clone(), said);
    let plan = move |_| {
        planner.start(id, None, PlanChoices::default());
        go.run(tab_href(id, FolderTab::Plan));
    };
    let planning = planner.pending(id);
    let trail = vec![
        ("/source-sets".to_string(), "Source sets".to_string()),
        (set_href(&folder.source_set), folder.source_set.clone()),
    ];
    let latest = folder.latest_plan;
    let folder = StoredValue::new(folder);
    view! {
        <Crumbs trail here=title.clone() />
        <header class="page-head">
            <div class="page-title">
                <h1>{title}</h1>
                {move || view! { <StatusBadge status=now.get() /> }}
            </div>
            <div class="page-actions">
                <button type="button" disabled=move || busy.get() on:click=start_check>
                    <LabelledIcon glyph=Glyph::FileCheck label="Check" />
                </button>
                <button type="button" class="primary" disabled=move || busy.get() || planning.get() on:click=plan>
                    <LabelledIcon glyph=Glyph::ClipboardList label="Plan build" />
                </button>
            </div>
        </header>
        {said_view(said)}
        <nav class="tabs" aria-label="Library folder">
            {FolderTab::ALL
                .into_iter()
                .map(|t| {
                    view! {
                        <a href=tab_href(id, t) aria-current=move || (tab.get() == t).then_some("page")>
                            {t.label()}
                        </a>
                    }
                })
                .collect_view()}
        </nav>
        {move || match tab.get() {
            FolderTab::Overview => view! { <Overview folder=folder.get_value() /> }.into_any(),
            FolderTab::Selection => view! {
                <SelectionTab folder=folder.get_value() refresh />
            }
            .into_any(),
            FolderTab::Plan => view! { <PlanTab folder=id latest refresh /> }.into_any(),
            FolderTab::Builds => view! { <Builds folder=id /> }.into_any(),
            FolderTab::Drift => {
                let source_set = folder.with_value(|f| f.source_set.clone());
                view! {
                    <CheckPanel folder=id busy refresh />
                    <CleanupForm folder=id source_set busy refresh />
                }
                .into_any()
            }
            FolderTab::Kept => view! { <KeptFiles folder=id busy refresh /> }.into_any(),
        }}
    }
}

/// Where the folder stands, what it's bound to, and the files built into it.
#[component]
fn Overview(folder: LibraryFolder) -> impl IntoView {
    let id = folder.id;
    view! {
        {folder.active_job.map(|job| view! { <BuildProgress job /> })}
        {reasons(&folder)}
        {folder_facts(&folder)}
        {folder.incomplete.is_some().then(|| view! { <IncompleteItems folder=id /> })}
        <BuiltFiles folder=id />
    }
}

/// The selection the folder's current build built, a plan of another, and what the folder is bound to.
#[component]
fn SelectionTab(folder: LibraryFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let built = folder.build.as_ref().map(|b| b.selection);
    view! {
        <dl class="facts">
            <dt>"Built from"</dt>
            <dd>
                {match built {
                    Some(s) => view! { <A href=format!("/selections/{s}")>{format!("Selection #{s}")}</A> }.into_any(),
                    None => "Never built".into_any(),
                }}
            </dd>
        </dl>
        <PlanForm folder=folder.clone() />
        <ChangeBinding folder refresh />
    }
}

/// The folder's latest plan, or the one being made for it.
#[component]
fn PlanTab(folder: i64, latest: Option<i64>, refresh: RwSignal<u64>) -> impl IntoView {
    let planner = use_planner();
    view! {
        <Pending pending=planner.pending(folder) status=planner.status problem=planner.problem(folder)>
            {move || {
                planner
                    .older(folder)
                    .get()
                    .map(|older| {
                        view! {
                            <p class="choices">
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        planner.start(folder, Some(older.selection), PlanChoices::default())
                                    }
                                >
                                    {older.label.clone()}
                                </button>
                            </p>
                        }
                    })
            }}
            {match latest {
                Some(plan) => view! { <LibraryPlan plan folder refresh /> }.into_any(),
                None => view! { <p class="note">"No plan yet. Choose Plan build to make one."</p> }.into_any(),
            }}
        </Pending>
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
fn CheckPanel(folder: i64, busy: Signal<bool>, refresh: RwSignal<u64>) -> impl IntoView {
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
                <button type="button" disabled=move || busy.get() on:click=start>"Check the folder"</button>
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
    busy: Signal<bool>,
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
    let go = navigator();
    let planning = Action::new_local(move |body: &PlanCleanup| {
        let body = body.clone();
        async move { api::post::<_, Plan>(&format!("/libraries/{folder}/cleanups"), &body).await }
    });
    Effect::new(move || {
        if let Some(Ok(_)) = planning.value().get() {
            request.set(PlanCleanup::default());
            refresh.update(|n| *n += 1);
            go.run(tab_href(folder, FolderTab::Plan));
        }
    });
    let plan = move |_| {
        let mut body = request.get_untracked();
        body.additions = adding.with_untracked(|adding| {
            body.entries
                .iter()
                .filter(|(_, a)| **a == CleanupAction::AddAddition)
                .filter_map(|(name, _)| adding.get(name).map(|a| (name.clone(), a.get_untracked())))
                .collect()
        });
        planning.dispatch(body);
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
                        disabled=move || busy.get() || planning.pending().get() || request.with(PlanCleanup::is_empty)
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
            <ActionPending action=planning status="Making the cleanup plan…">
                {form}
            </ActionPending>
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
/// A job's progress on the folder, live, with a way to cancel a build while it runs.
#[component]
pub(crate) fn BuildProgress(job: i64) -> impl IntoView {
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
fn PlanForm(folder: LibraryFolder) -> impl IntoView {
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
    let planner = use_planner();
    let busy = planner.pending(id);
    let go = navigator();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Ok(selection) = chosen.get_untracked().parse::<i64>() else {
            return;
        };
        planner.start(id, Some(selection), PlanChoices::default());
        go.run(tab_href(id, FolderTab::Plan));
    };
    view! {
        <section aria-labelledby="plan-title">
            <h2 id="plan-title">"Plan a build of another selection"</h2>
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
                        <button type="submit" disabled=move || busy.get() || chosen.with(String::is_empty)>
                            "Plan a build"
                        </button>
                    </form>
                }
                .into_any(),
            }}
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildCol {
    Build,
    State,
    Selection,
    Finished,
    Written,
    Failed,
    NotWritten,
}

#[derive(Clone, PartialEq)]
struct BuildRow(BuildSummary);

impl ListRow for BuildRow {
    type Col = BuildCol;
    const COLUMNS: &'static [BuildCol] = &[
        BuildCol::Build,
        BuildCol::State,
        BuildCol::Selection,
        BuildCol::Finished,
        BuildCol::Written,
        BuildCol::Failed,
        BuildCol::NotWritten,
    ];

    fn column(col: BuildCol) -> Column {
        match col {
            BuildCol::Build => Column::new("build", "Build", Width::Fixed(5.0)).sortable(),
            BuildCol::State => Column::new("state", "State", Width::Flex { min: 7.0, share: 1 }),
            BuildCol::Selection => Column::new("selection", "Selection", Width::Fixed(6.0)),
            BuildCol::Finished => Column::new(
                "finished",
                "Finished",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            BuildCol::Written => Column::new("written", "Written", Width::Fixed(5.0)).numeric(),
            BuildCol::Failed => Column::new("failed", "Failed", Width::Fixed(5.0)).numeric(),
            BuildCol::NotWritten => Column::new("not-written", "Not written", Width::Fixed(6.0))
                .numeric()
                .hint("Left for a new plan"),
        }
    }

    fn key(&self) -> String {
        self.0.id.to_string()
    }

    fn title(&self) -> String {
        format!("Build #{}", self.0.id)
    }

    fn search(&self) -> String {
        format!("#{} {}", self.0.id, build_state_label(self.0.state))
    }

    fn cell(&self, col: BuildCol) -> AnyView {
        let b = &self.0;
        match col {
            BuildCol::Build => format!("#{}", b.id).into_any(),
            BuildCol::State => build_state_label(b.state).into_any(),
            BuildCol::Selection => format!("#{}", b.selection).into_any(),
            BuildCol::Finished => b
                .finished_at
                .as_deref()
                .map(when)
                .unwrap_or_default()
                .into_any(),
            BuildCol::Written => b.applied.to_string().into_any(),
            BuildCol::Failed => b.failed.to_string().into_any(),
            BuildCol::NotWritten => b.pending.to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: BuildCol) -> Ordering {
        match col {
            BuildCol::Finished => self.0.finished_at.cmp(&other.0.finished_at),
            _ => self.0.id.cmp(&other.0.id),
        }
    }

    fn href(&self) -> Option<String> {
        Some(format!("/builds/{}", self.0.id))
    }
}

fn build_detail(row: BuildRow) -> AnyView {
    let b = row.0;
    view! {
        <dl class="facts">
            <dt>"State"</dt>
            <dd>{build_state_label(b.state)}</dd>
            <dt>"Generation"</dt>
            <dd>{b.generation}</dd>
            <dt>"Selection"</dt>
            <dd>
                <A href=format!("/selections/{}", b.selection)>{format!("#{}", b.selection)}</A>
            </dd>
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
    }
    .into_any()
}

/// The folder's builds, newest first.
#[component]
fn Builds(folder: i64) -> impl IntoView {
    let live = use_live();
    let builds = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuildSummary>>(&format!("/libraries/{folder}/builds")).await }
    });
    let rows = last_good_rows(move || {
        builds
            .get()
            .map(|r| r.map(|b| b.into_iter().map(BuildRow).collect::<Vec<_>>()))
    });
    view! {
        {move || match builds.get() {
            None => view! { <Loading what="the builds" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="builds"
            label="Builds"
            rows
            empty="Not built yet."
            detail=Callback::new(build_detail)
        />
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileCol {
    Kind,
    Target,
    Game,
    Build,
    Size,
}

#[derive(Clone, PartialEq)]
struct FileRow(BuiltFile);

impl ListRow for FileRow {
    type Col = FileCol;
    const COLUMNS: &'static [FileCol] = &[
        FileCol::Kind,
        FileCol::Target,
        FileCol::Game,
        FileCol::Build,
        FileCol::Size,
    ];

    fn column(col: FileCol) -> Column {
        match col {
            FileCol::Kind => Column::new("kind", "Kind", Width::Fixed(4.5))
                .sortable()
                .hint("How its build wrote it"),
            FileCol::Target => Column::new(
                "file",
                "File",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            FileCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            FileCol::Build => Column::new("build", "Build", Width::Fixed(5.0)).sortable(),
            FileCol::Size => Column::new("size", "Bytes", Width::Fixed(7.0))
                .sortable()
                .numeric(),
        }
    }

    fn key(&self) -> String {
        self.0.target.clone()
    }

    fn title(&self) -> String {
        self.0.target.clone()
    }

    fn search(&self) -> String {
        format!("{} {}", self.0.target, self.0.game)
    }

    fn cell(&self, col: FileCol) -> AnyView {
        let f = &self.0;
        match col {
            FileCol::Kind => {
                view! { <StatusIcon status=BuildKind::from(f.kind) tab_stop=false /> }.into_any()
            }
            FileCol::Target => view! { <Clip text=f.target.clone() /> }.into_any(),
            FileCol::Game => view! { <Clip text=f.game.clone() /> }.into_any(),
            FileCol::Build => format!("#{}", f.build).into_any(),
            FileCol::Size => f.file.size.to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: FileCol) -> Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            FileCol::Kind => BuildKind::from(a.kind)
                .rank()
                .cmp(&BuildKind::from(b.kind).rank()),
            FileCol::Target => a.target.cmp(&b.target),
            FileCol::Game => a.game.cmp(&b.game),
            FileCol::Build => a.build.cmp(&b.build),
            FileCol::Size => a.file.size.cmp(&b.file.size),
        }
    }
}

fn file_detail(row: FileRow) -> AnyView {
    let f = row.0;
    view! {
        <p>
            <StatusBadge status=BuildKind::from(f.kind) />
        </p>
        <dl class="facts">
            <dt>"Game"</dt>
            <dd>{f.game.clone()}</dd>
            <dt>"Build"</dt>
            <dd>
                <A href=format!("/builds/{}", f.build)>{format!("#{}", f.build)}</A>
            </dd>
            <dt>"Bytes"</dt>
            <dd>{f.file.size}</dd>
        </dl>
    }
    .into_any()
}

/// Each file the store built into the folder, with the build that wrote it.
#[component]
fn BuiltFiles(folder: i64) -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuiltFile>>(&format!("/libraries/{folder}/files")).await }
    });
    let rows = last_good_rows(move || {
        files
            .get()
            .map(|r| r.map(|f| f.into_iter().map(FileRow).collect::<Vec<_>>()))
    });
    let kinds =
        Signal::derive(move || present(rows.get().iter().map(|r| BuildKind::from(r.0.kind))));
    view! {
        <section aria-labelledby="files-title">
            <h2 id="files-title">"Built files"</h2>
            {move || match files.get() {
                None => view! { <Loading what="the built files" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            <List
                id="built-files"
                label="Built files"
                rows
                empty="No files built yet."
                placeholder="Filter by file or game"
                detail=Callback::new(file_detail)
                legend=move || view! { <Legend statuses=kinds list="built-files" /> }
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
    let new_href = format!("/libraries/new?selection={}", selection.id);
    let selection = StoredValue::new(selection);
    let planner = use_planner();
    let go = navigator();
    let plan = move |folder: i64| {
        let id = selection.with_value(|s| s.id);
        planner.start(folder, Some(id), PlanChoices::default());
        go.run(tab_href(folder, FolderTab::Plan));
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
                        <A href=new_href.clone()>"make a library folder"</A>
                        " of it."
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
    }
}

/// The files builds kept aside in the folder's retention area, by generation, and deleting the ones you pick by
/// approving a plan that lists them.
#[component]
fn KeptFiles(folder: i64, busy: Signal<bool>, refresh: RwSignal<u64>) -> impl IntoView {
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
    let planning = Action::new_local(move |body: &PlanKeptDeletion| {
        let body = body.clone();
        async move {
            api::post::<_, KeptDeletion>(&format!("/libraries/{folder}/kept-deletions"), &body)
                .await
        }
    });
    Effect::new(move || {
        if let Some(Ok(p)) = planning.value().get() {
            plan.set(Some(p));
        }
    });
    let request = move |all: bool| {
        problem.set(None);
        planning.dispatch(PlanKeptDeletion {
            ids: if all {
                Vec::new()
            } else {
                picked.get_untracked()
            },
            all,
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
                        disabled=move || busy.get() || planning.pending().get() || picked.with(Vec::is_empty)
                        on:click=move |_| request(false)
                    >
                        "Plan deleting the files you picked"
                    </button>
                    <button type="button" disabled=move || busy.get() || planning.pending().get() on:click=move |_| request(true)>
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
                                    disabled=move || busy.get() || working.get()
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
            <ActionPending action=planning status="Making the deletion plan…">
                {list}
            </ActionPending>
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}
