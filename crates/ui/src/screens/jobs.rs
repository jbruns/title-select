use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::NavigateOptions;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_navigate, use_query_map};
use title_select_api::additions::AdditionsSet;
use title_select_api::build::{KeptFile, LibraryFolder};
use title_select_api::ingest::{IngestFolder, IngestKeptFile};
use title_select_api::repair::SourceKeptFile;
use title_select_api::{Job, JobKind, JobState, SourceSetRecord};

use crate::activity_model::{
    Counts, KeptPlace, KeptRow, Outcome, Subject, counts, listed, newest_first, opened_failure,
};
use crate::api;
use crate::app::{Loading, Problem, use_seen_jobs};
use crate::events::{Stream, use_live};
use crate::jobs::{LiveJob, state_label};
use crate::model::{audit_evidence_label, when};
use crate::parts::icon::{Glyph, Svg};
use crate::parts::layout::{Tab, Tabs};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, Keep, List, ListRow, RowAction, ToolAction, Width, last_good_rows,
};
use crate::parts::overlay::Tooltip;
use crate::parts::status::{Status, StatusBadge, StatusIcon};
use crate::parts::url_state::{query_string, use_list_query};
use crate::shell::unseen_failures;

pub(crate) fn progress_view(live: &LiveJob) -> AnyView {
    match (live.job.state, live.progress) {
        (JobState::Running, Some(p)) => view! {
            <span class="progress">
                <progress max=p.files_total.max(1) value=p.files_done></progress>
                <span class="data">
                    {format!(
                        "{} / {} {}",
                        p.files_done,
                        p.files_total,
                        if live.job.kind == JobKind::Repair { "steps" } else { "files" },
                    )}
                </span>
            </span>
        }
        .into_any(),
        (JobState::Running, None) => "Starting".into_any(),
        _ => ().into_any(),
    }
}

pub(crate) fn state_view(state: JobState) -> AnyView {
    view! { <span class=format!("job-state job-{}", state.as_str())>{state_label(state)}</span> }
        .into_any()
}

pub(crate) fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Audit => "Audit",
        JobKind::Selection => "Selection",
        JobKind::Build => "Build",
        JobKind::Check => "Check",
        JobKind::IngestScan => "Ingest scan",
        JobKind::Repair => "Repair",
        JobKind::Additions => "Additions",
        JobKind::StatWalk => "Check for changes",
    }
}

/// Activity: the Jobs, and the kept files of every folder, as tabs.
#[component]
pub fn Activity() -> impl IntoView {
    view! {
        <h1>"Activity"</h1>
        <Tabs
            label="Activity"
            tabs=vec![
                Tab { path: "/activity", label: "Jobs" },
                Tab { path: "/activity/kept", label: "Kept files" },
            ]
        />
    }
}

/// A Job's row: the Job as the events left it, and the name of what it works on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobRow {
    live: LiveJob,
    subject: Subject,
    subject_name: String,
}

impl JobRow {
    fn new(live: LiveJob, libraries: &BTreeMap<i64, String>) -> JobRow {
        let subject = Subject::of(&live.job);
        let subject_name = match &subject {
            Subject::SourceSet(name) | Subject::IngestFolder(name) => name.clone(),
            Subject::LibraryFolder(id) => libraries
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("Library folder #{id}")),
            Subject::AdditionsSet(system) => format!("Additions of {system}"),
        };
        JobRow {
            live,
            subject,
            subject_name,
        }
    }

    fn job(&self) -> &Job {
        &self.live.job
    }

    /// The label and glyph of the link to what it works on.
    fn subject_link(&self) -> (Glyph, String) {
        let name = &self.subject_name;
        match self.subject {
            Subject::SourceSet(_) => (Glyph::Database, format!("Open source set {name}")),
            Subject::LibraryFolder(_) => (Glyph::Library, format!("Open library folder {name}")),
            Subject::IngestFolder(_) => (Glyph::FolderInput, format!("Open ingest folder {name}")),
            Subject::AdditionsSet(_) => (Glyph::CirclePlus, format!("Open the {name}")),
        }
    }
}

fn outcome_link(outcome: &Outcome) -> (Glyph, String) {
    match outcome {
        Outcome::Audit(id) => (Glyph::ClipboardList, format!("Open audit #{id}")),
        Outcome::Selection(id) => (Glyph::SlidersHorizontal, format!("Open selection #{id}")),
        Outcome::Repair { .. } => (Glyph::Wrench, "Open its repair".to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobCol {
    State,
    Id,
    Kind,
    Subject,
    Progress,
    Queued,
}

impl ListRow for JobRow {
    type Col = JobCol;
    const COLUMNS: &'static [JobCol] = &[
        JobCol::State,
        JobCol::Id,
        JobCol::Kind,
        JobCol::Subject,
        JobCol::Progress,
        JobCol::Queued,
    ];

    fn column(col: JobCol) -> Column {
        match col {
            JobCol::State => Column::new("state", "State", Width::Fixed(3.5)).sortable(),
            JobCol::Id => Column::new("job", "Job", Width::Fixed(5.0))
                .sortable()
                .numeric(),
            JobCol::Kind => Column::new("kind", "Kind", Width::Fixed(7.5)).sortable(),
            JobCol::Subject => Column::new(
                "subject",
                "Works on",
                Width::Flex {
                    min: 10.0,
                    share: 3,
                },
            )
            .sortable()
            .hint("The source set or folder it works on"),
            JobCol::Progress => Column::new("progress", "Progress", Width::Fixed(10.0)),
            JobCol::Queued => Column::new("queued", "Queued", Width::Fixed(12.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        self.job().id.to_string()
    }

    fn title(&self) -> String {
        format!("{} job #{}", kind_label(self.job().kind), self.job().id)
    }

    fn search(&self) -> String {
        let job = self.job();
        format!(
            "#{} {} {} {} {} {}",
            job.id,
            kind_label(job.kind),
            state_label(job.state),
            self.subject_name,
            job.source_set,
            job.actor
        )
    }

    fn cell(&self, col: JobCol) -> AnyView {
        let job = self.job();
        match col {
            JobCol::State => view! { <StatusIcon status=job.state tab_stop=false /> }.into_any(),
            JobCol::Id => format!("#{}", job.id).into_any(),
            JobCol::Kind => view! { <Clip text=kind_label(job.kind) /> }.into_any(),
            JobCol::Subject => view! { <Clip text=self.subject_name.clone() /> }.into_any(),
            JobCol::Progress => progress_view(&self.live),
            JobCol::Queued => view! { <Clip text=when(&job.created_at) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: JobCol) -> Ordering {
        let (a, b) = (self.job(), other.job());
        match col {
            JobCol::State => a.state.rank().cmp(&b.state.rank()),
            JobCol::Kind => kind_label(a.kind).cmp(kind_label(b.kind)),
            JobCol::Subject => self.subject_name.cmp(&other.subject_name),
            JobCol::Queued => a.created_at.cmp(&b.created_at),
            JobCol::Id | JobCol::Progress => a.id.cmp(&b.id),
        }
    }
}

/// A row's links: what it works on, and what it recorded or carries out.
fn job_actions(row: &JobRow) -> Vec<RowAction> {
    let (glyph, label) = row.subject_link();
    let mut actions = vec![RowAction::go(glyph, label, row.subject.href())];
    if let Some(outcome) = Outcome::of(row.job()) {
        let (glyph, label) = outcome_link(&outcome);
        actions.push(RowAction::go(glyph, label, outcome.href()));
    }
    actions
}

/// The last list a fetch returned, or `None` until one succeeds; a failed or pending refetch keeps it.
fn last_good<T, E>(
    load: impl Fn() -> Option<Result<Vec<T>, E>> + Send + Sync + 'static,
) -> Memo<Option<Arc<Vec<T>>>>
where
    T: Send + Sync + 'static,
{
    Memo::new_with_compare(
        move |prev: Option<&Option<Arc<Vec<T>>>>| match load() {
            Some(Ok(list)) => Some(Arc::new(list)),
            _ => prev.cloned().flatten(),
        },
        |prev, next| match (prev, next) {
            (Some(Some(a)), Some(Some(b))) => !Arc::ptr_eq(a, b),
            (Some(None), Some(None)) => false,
            _ => true,
        },
    )
}

/// The `state` query parameter: the Job State the counters narrow the list to.
const STATE: &str = "state";

fn state_named(name: &str) -> Option<JobState> {
    JobState::ALL.into_iter().find(|s| s.as_str() == name)
}

/// The Jobs tab: counters of Running, Queued and Failed Jobs over every Job, live, with the history of finished ones.
#[component]
pub fn Jobs() -> impl IntoView {
    let live = use_live();
    let history = LocalResource::new(move || {
        live.settled.track();
        async { api::get::<Vec<Job>>("/jobs").await }
    });
    // Every failure, however old, so that each one the Activity badge counts has a row to open.
    let failed = LocalResource::new(move || {
        live.settled.track();
        async { api::get::<Vec<Job>>("/failed-jobs").await }
    });
    let libraries = LocalResource::new(move || {
        live.settled.track();
        async { api::get::<Vec<LibraryFolder>>("/libraries").await }
    });
    // Each fetch keeps its last success, and the live book always goes over them: a failed fetch shows its
    // problem but never stops the Jobs that stream in.
    let recent = last_good(move || history.get());
    let failures = last_good(move || failed.get());
    let rows: Signal<Arc<Vec<JobRow>>> = Memo::new_with_compare(
        move |_| {
            let names: BTreeMap<i64, String> = libraries
                .get()
                .and_then(Result::ok)
                .unwrap_or_default()
                .into_iter()
                .map(|f| (f.id, f.path))
                .collect();
            let jobs = recent.with(|recent| {
                failures.with(|failed| {
                    live.book.with(|book| {
                        listed(
                            recent.as_deref().map(Vec::as_slice),
                            failed.as_deref().map(Vec::as_slice),
                            book,
                        )
                    })
                })
            });
            Arc::new(
                jobs.into_iter()
                    .map(|l| JobRow::new(l, &names))
                    .collect::<Vec<_>>(),
            )
        },
        |_, _| true,
    )
    .into();
    let counted = Memo::new(move |_| rows.with(|r| counts(r.iter().map(JobRow::job))));
    let statuses = Signal::derive(move || {
        present(rows.with(|r| r.iter().map(|r| r.job().state).collect::<Vec<_>>()))
    });

    // Opening a failed Job marks it seen, so the Activity badge stops counting it.
    let seen = use_seen_jobs();
    let (query, _) = use_list_query();
    Effect::new(move || {
        let row = query.with(|q| q.selected.clone());
        let opened = rows.with(|r| opened_failure(r.iter().map(JobRow::job), row.as_deref()));
        if let Some(id) = opened {
            seen.mark(id);
        }
    });
    let unseen = move || {
        let failed: Vec<i64> = rows.with_untracked(|r| {
            r.iter()
                .filter(|r| r.job().state == JobState::Failed)
                .map(|r| r.job().id)
                .collect()
        });
        seen.get()
            .with_untracked(|seen| unseen_failures(std::iter::empty(), failed, seen))
    };
    let said = RwSignal::new(None::<String>);
    let actions = vec![
        ToolAction::page(Glyph::Eye, "Mark all failures seen", move || {
            let ids = unseen();
            said.set(Some(match ids.len() {
                0 => "Every failed job was already seen.".to_string(),
                1 => "Marked 1 failed job seen.".to_string(),
                n => format!("Marked {n} failed jobs seen."),
            }));
            seen.mark_all(ids);
        }),
        ToolAction::checked(Glyph::Eye, "Mark seen", move |chosen| {
            let ids: Vec<i64> = chosen
                .rows
                .iter()
                .filter(|r: &&JobRow| r.job().state == JobState::Failed)
                .map(|r| r.job().id)
                .collect();
            said.set(Some(match ids.len() {
                0 => "None of them failed, so there was nothing to mark.".to_string(),
                1 => "Marked 1 failed job seen.".to_string(),
                n => format!("Marked {n} failed jobs seen."),
            }));
            seen.mark_all(ids);
        }),
    ];

    // The counters narrow the list to one Job State, kept in the URL.
    let query_map = use_query_map();
    let shown_state =
        Memo::new(move |_| query_map.with(|q| q.get(STATE).as_deref().and_then(state_named)));
    let keep = Signal::derive(move || -> Keep<JobRow> {
        let state = shown_state.get()?;
        Some(Arc::new(move |r: &JobRow| r.job().state == state))
    });
    let location = use_location();
    let navigate = use_navigate();
    let choose = Callback::new(move |state: Option<JobState>| {
        let mut pairs: Vec<(String, String)> = query_map.with_untracked(|q| {
            q.latest_values()
                .filter(|(k, _)| k.as_ref() != STATE)
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        });
        if let Some(state) = state {
            pairs.push((STATE.to_string(), state.as_str().to_string()));
        }
        let path = location.pathname.get_untracked();
        navigate(
            &format!("{path}{}", query_string(&pairs)),
            NavigateOptions {
                replace: true,
                scroll: false,
                ..Default::default()
            },
        );
    });

    view! {
        <p class="lede">
            {move || match live.stream.get() {
                Stream::Live => "Live: state and progress arrive as they happen.".to_string(),
                Stream::Connecting => "Connecting to the service's events…".to_string(),
                Stream::Reconnecting => {
                    "The event stream dropped; reconnecting. What is shown may be out of date.".to_string()
                }
            }}
        </p>
        <Counters counted shown=shown_state choose />
        {move || match (history.get(), failed.get()) {
            (Some(Err(message)), _) | (_, Some(Err(message))) => {
                view! { <Problem message /> }.into_any()
            }
            (None, _) | (_, None) => view! { <Loading what="the jobs" /> }.into_any(),
            _ => ().into_any(),
        }}
        <p class="done" role="status">
            {move || said.get()}
        </p>
        <List
            id="jobs"
            label="Jobs"
            rows
            empty="No jobs yet. Start an audit or a selection from a source set."
            placeholder="Filter by job, kind, state or name"
            actions
            row_actions=Callback::new(move |row: JobRow| job_actions(&row))
            quick=2
            keep
            detail=Callback::new(job_detail)
            legend=move || view! { <Legend statuses list="jobs" /> }
        />
    }
}

/// The Running, Queued and Failed counters. Each narrows the list to its Jobs; choosing it again shows every Job.
#[component]
fn Counters(
    counted: Memo<Counts>,
    shown: Memo<Option<JobState>>,
    choose: Callback<Option<JobState>>,
) -> impl IntoView {
    let counter = move |state: JobState, count: fn(&Counts) -> usize| {
        let look = state.look();
        let pressed = move || shown.get() == Some(state);
        let n = move || counted.with(count);
        let label = Signal::derive(move || {
            if pressed() {
                format!("{}: {}. Show every job", look.label, n())
            } else {
                format!("{}: {}. Show only these jobs", look.label, n())
            }
        });
        view! {
            <Tooltip label>
                <button
                    type="button"
                    class="counter"
                    aria-pressed=move || pressed().to_string()
                    aria-label=move || label.get()
                    on:click=move |_| choose.run((!pressed()).then_some(state))
                >
                    <span class=look.tone.class()>
                        <Svg glyph=look.glyph />
                    </span>
                    <span class="counter-count data">{n}</span>
                    <span class="counter-label">{look.label}</span>
                </button>
            </Tooltip>
        }
    };
    view! {
        <div class="counters" role="group" aria-label="Jobs by state">
            {counter(JobState::Running, |c| c.running)}
            {counter(JobState::Queued, |c| c.queued)}
            {counter(JobState::Failed, |c| c.failed)}
        </div>
    }
}

fn job_detail(row: JobRow) -> AnyView {
    let live = row.live.clone();
    let job = live.job.clone();
    let set_href = Subject::SourceSet(job.source_set.clone()).href();
    view! {
        <dl class="facts">
            <dt>"State"</dt>
            <dd>
                <StatusBadge status=job.state />
                " "
                {progress_view(&live)}
            </dd>
            <dt>"Works on"</dt>
            <dd>
                <A href=row.subject.href()>{row.subject_name.clone()}</A>
            </dd>
            {(!matches!(row.subject, Subject::SourceSet(_)) && !job.source_set.is_empty())
                .then(|| {
                    view! {
                        <dt>"Source set"</dt>
                        <dd>
                            <A href=set_href>{job.source_set.clone()}</A>
                        </dd>
                    }
                })}
            {(!job.dat_revision.is_empty())
                .then(|| {
                    view! {
                        <dt>"DAT revision"</dt>
                        <dd>
                            <code class="hash">{job.dat_revision.clone()}</code>
                        </dd>
                    }
                })}
            {job
                .profile
                .clone()
                .map(|p| {
                    view! {
                        <dt>"Selection profile"</dt>
                        <dd>
                            <A href=format!("/profiles/{}", api::segment(&p.name))>{p.name.clone()}</A>
                            {format!(", version {}", p.version)}
                        </dd>
                    }
                })}
            <dt>"Started by"</dt>
            <dd>{job.actor.clone()}</dd>
            <dt>"Attempt"</dt>
            <dd>
                {job.attempt}
                {job.requeued_from.map(|from| format!(", queued again for interrupted job #{from}"))}
            </dd>
            {job
                .evidence
                .map(|evidence| {
                    view! {
                        <dt>"Evidence"</dt>
                        <dd>{audit_evidence_label(evidence)}</dd>
                    }
                })}
            {(job.kind == JobKind::Audit)
                .then(|| {
                    view! {
                        <dt>"Workers"</dt>
                        <dd>
                            {format!(
                                "{} payload readers, {} hash workers",
                                job.workers.readers,
                                job.workers.hashers,
                            )}
                        </dd>
                    }
                })}
            <dt>"Queued"</dt>
            <dd>{when(&job.created_at)}</dd>
            <dt>"Started"</dt>
            <dd>{job.started_at.as_deref().map(when)}</dd>
            <dt>"Finished"</dt>
            <dd>{job.finished_at.as_deref().map(when)}</dd>
            {job
                .error
                .clone()
                .map(|error| {
                    view! {
                        <dt>"Error"</dt>
                        <dd class="failed">{error}</dd>
                    }
                })}
        </dl>
    }
    .into_any()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptCol {
    Name,
    Folder,
    Reason,
    Size,
    Kept,
}

impl ListRow for KeptRow {
    type Col = KeptCol;
    const COLUMNS: &'static [KeptCol] = &[
        KeptCol::Name,
        KeptCol::Folder,
        KeptCol::Reason,
        KeptCol::Size,
        KeptCol::Kept,
    ];

    fn column(col: KeptCol) -> Column {
        match col {
            KeptCol::Name => Column::new(
                "name",
                "File",
                Width::Flex {
                    min: 10.0,
                    share: 3,
                },
            )
            .sortable(),
            KeptCol::Folder => Column::new("folder", "Folder", Width::Flex { min: 8.0, share: 2 })
                .sortable()
                .hint("The folder it was kept aside from"),
            KeptCol::Reason => {
                Column::new("reason", "Why", Width::Flex { min: 9.0, share: 2 }).sortable()
            }
            KeptCol::Size => Column::new("size", "Bytes", Width::Fixed(7.0))
                .sortable()
                .numeric(),
            KeptCol::Kept => Column::new("kept", "Kept", Width::Fixed(12.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        KeptRow::key(self)
    }

    fn title(&self) -> String {
        self.name.clone()
    }

    fn search(&self) -> String {
        format!("{} {} {}", self.name, self.place.name(), self.reason)
    }

    fn cell(&self, col: KeptCol) -> AnyView {
        match col {
            KeptCol::Name => view! { <Clip text=self.name.clone() /> }.into_any(),
            KeptCol::Folder => view! { <Clip text=self.place.name() /> }.into_any(),
            KeptCol::Reason => view! { <Clip text=self.reason.clone() /> }.into_any(),
            KeptCol::Size => self.size.to_string().into_any(),
            KeptCol::Kept => view! { <Clip text=when(&self.kept_at) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: KeptCol) -> Ordering {
        match col {
            KeptCol::Name => self.name.cmp(&other.name),
            KeptCol::Folder => self.place.name().cmp(&other.place.name()),
            KeptCol::Reason => self.reason.cmp(&other.reason),
            KeptCol::Size => self.size.cmp(&other.size),
            KeptCol::Kept => self.kept_at.cmp(&other.kept_at),
        }
    }
}

fn place_link(place: &KeptPlace) -> (Glyph, String) {
    let name = place.name();
    match place {
        KeptPlace::LibraryFolder { .. } => (
            Glyph::Library,
            format!("Open the kept files of library folder {name}"),
        ),
        KeptPlace::IngestFolder(_) => (Glyph::FolderInput, format!("Open ingest folder {name}")),
        KeptPlace::SourceSet(_) => (
            Glyph::Database,
            format!("Open the kept files of source set {name}"),
        ),
        KeptPlace::AdditionsSet(_) => (Glyph::CirclePlus, format!("Open the additions of {name}")),
    }
}

/// The kept files of every Library Folder, Ingest Folder, Source Set and Additions Set.
async fn every_kept_file() -> Result<Vec<KeptRow>, String> {
    let mut rows = Vec::new();
    let folders: Vec<LibraryFolder> = api::get("/libraries").await?;
    for folder in folders {
        let kept: Vec<KeptFile> = api::get(&format!("/libraries/{}/kept-files", folder.id)).await?;
        rows.extend(kept.into_iter().map(|f| KeptRow::library(&folder, f)));
    }
    let ingest: Vec<IngestFolder> = api::get("/ingest-folders").await?;
    for folder in ingest.into_iter().filter(|f| f.id.is_some()) {
        let kept: Vec<IngestKeptFile> = api::get(&format!(
            "/ingest-folders/{}/kept-files",
            api::segment(&folder.root)
        ))
        .await?;
        rows.extend(kept.into_iter().map(|f| KeptRow::ingest(&folder.root, f)));
    }
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    for set in sets {
        let kept: Vec<SourceKeptFile> = api::get(&format!(
            "/source-sets/{}/kept-files",
            api::segment(&set.name)
        ))
        .await?;
        rows.extend(
            kept.into_iter()
                .map(|f| KeptRow::source(KeptPlace::SourceSet(set.name.clone()), f)),
        );
    }
    let additions: Vec<AdditionsSet> = api::get("/additions-sets").await?;
    for set in additions {
        rows.extend(
            set.kept
                .into_iter()
                .map(|f| KeptRow::source(KeptPlace::AdditionsSet(set.system.clone()), f)),
        );
    }
    Ok(newest_first(rows))
}

/// The Kept files tab: every folder's kept files in one list. You delete them from their folder's page.
#[component]
pub fn KeptFiles() -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        every_kept_file()
    });
    let rows = last_good_rows(move || files.get());
    view! {
        <p class="lede">"Files set aside in every folder. Open a file's folder to delete them."</p>
        {move || match files.get() {
            None => view! { <Loading what="the kept files" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="kept-files"
            label="Kept files"
            rows
            empty="No files are kept aside in any folder."
            placeholder="Filter by file, folder or reason"
            row_actions=Callback::new(move |row: KeptRow| {
                let (glyph, label) = place_link(&row.place);
                vec![RowAction::go(glyph, label, row.place.href())]
            })
            detail=Callback::new(kept_detail)
        />
    }
}

fn kept_detail(row: KeptRow) -> AnyView {
    view! {
        <dl class="facts">
            <dt>"Folder"</dt>
            <dd>
                <A href=row.place.href()>{row.place.name()}</A>
            </dd>
            <dt>"Why"</dt>
            <dd>{row.reason.clone()}</dd>
            <dt>"Kept as"</dt>
            <dd>
                <code class="path">{format!("{}/{}", row.generation, row.kept_name)}</code>
            </dd>
            <dt>"Size"</dt>
            <dd class="data">{format!("{} bytes", row.size)}</dd>
            <dt>"Kept"</dt>
            <dd>{when(&row.kept_at)}</dd>
        </dl>
    }
    .into_any()
}
