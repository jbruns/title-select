use std::collections::VecDeque;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_struct_table::ColumnSort;
use title_select_api::{Job, JobKind, JobState};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::{Stream, use_live};
use crate::jobs::{LiveJob, state_label};
use crate::model::{audit_evidence_label, short_sha, when};
use crate::table::{DataTable, Grid, Rows, rows_of, sort_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobCol {
    Id,
    Kind,
    State,
    SourceSet,
    DatRevision,
    Progress,
    Actor,
    Created,
    Finished,
    Result,
}

#[derive(Clone, PartialEq)]
struct JobItem(LiveJob);

pub(crate) fn progress_view(live: &LiveJob) -> AnyView {
    match (live.job.state, live.progress) {
        (JobState::Running, Some(p)) => view! {
            <span class="progress">
                <progress max=p.files_total.max(1) value=p.files_done></progress>
                <span class="data">{format!("{} / {} files", p.files_done, p.files_total)}</span>
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

impl Grid for JobItem {
    type Col = JobCol;
    const COLUMNS: &'static [JobCol] = &[
        JobCol::Id,
        JobCol::Kind,
        JobCol::State,
        JobCol::SourceSet,
        JobCol::DatRevision,
        JobCol::Progress,
        JobCol::Actor,
        JobCol::Created,
        JobCol::Finished,
        JobCol::Result,
    ];

    fn header(col: JobCol) -> &'static str {
        match col {
            JobCol::Id => "Job",
            JobCol::Kind => "Kind",
            JobCol::State => "State",
            JobCol::SourceSet => "Source set",
            JobCol::DatRevision => "DAT revision",
            JobCol::Progress => "Progress",
            JobCol::Actor => "Started by",
            JobCol::Created => "Queued",
            JobCol::Finished => "Finished",
            JobCol::Result => "Result",
        }
    }

    fn class(col: JobCol) -> &'static str {
        match col {
            JobCol::Id => "c-id data",
            JobCol::Kind | JobCol::Result => "c-kind",
            JobCol::State => "c-state",
            JobCol::SourceSet | JobCol::Progress => "c-wide",
            JobCol::DatRevision => "c-hash data",
            JobCol::Actor => "c-actor",
            JobCol::Created | JobCol::Finished => "c-time data",
        }
    }

    fn sortable(col: JobCol) -> bool {
        matches!(
            col,
            JobCol::Id
                | JobCol::Kind
                | JobCol::State
                | JobCol::SourceSet
                | JobCol::Created
                | JobCol::Finished
        )
    }

    fn cell(&self, col: JobCol) -> AnyView {
        let job = &self.0.job;
        match col {
            JobCol::Id => format!("#{}", job.id).into_any(),
            JobCol::Kind => kind_label(job.kind).into_any(),
            JobCol::State => state_view(job.state),
            JobCol::SourceSet => job.source_set.clone().into_any(),
            JobCol::DatRevision => short_sha(&job.dat_revision).to_string().into_any(),
            JobCol::Progress => progress_view(&self.0),
            JobCol::Actor => job.actor.clone().into_any(),
            JobCol::Created => when(&job.created_at).into_any(),
            JobCol::Finished => job
                .finished_at
                .as_deref()
                .map(when)
                .unwrap_or_default()
                .into_any(),
            JobCol::Result => result_link(job),
        }
    }
}

fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Audit => "Audit",
        JobKind::Selection => "Selection",
        JobKind::Build => "Build",
        JobKind::Check => "Check",
    }
}

/// A link to the Audit or Selection a Job recorded, or the Managed Folder a Build or Check Job is for.
fn result_link(job: &Job) -> AnyView {
    if let Some(folder) = job.managed_folder {
        return view! {
            <A href=format!("/managed-folders/{folder}")>{format!("Managed folder #{folder}")}</A>
        }
        .into_any();
    }
    match (job.audit, job.selection) {
        (Some(audit), _) => {
            view! { <A href=format!("/audits/{audit}")>{format!("Audit #{audit}")}</A> }.into_any()
        }
        (None, Some(selection)) => view! {
            <A href=format!("/selections/{selection}")>{format!("Selection #{selection}")}</A>
        }
        .into_any(),
        (None, None) => ().into_any(),
    }
}

fn sorted(mut jobs: Vec<LiveJob>, sorting: &VecDeque<(JobCol, ColumnSort)>) -> Vec<JobItem> {
    if let Some(sort) = sort_of(sorting) {
        jobs.sort_by(|a, b| {
            let (a, b) = (&a.job, &b.job);
            let ord = match sort.column {
                JobCol::Kind => a.kind.as_str().cmp(b.kind.as_str()),
                JobCol::State => a.state.as_str().cmp(b.state.as_str()),
                JobCol::SourceSet => a.source_set.cmp(&b.source_set),
                JobCol::Created => a.created_at.cmp(&b.created_at),
                JobCol::Finished => a.finished_at.cmp(&b.finished_at),
                _ => a.id.cmp(&b.id),
            };
            if sort.descending { ord.reverse() } else { ord }
        });
    }
    jobs.into_iter().map(JobItem).collect()
}

/// Every Queued and Running Job, live, with the history of finished ones.
#[component]
pub fn Jobs() -> impl IntoView {
    let live = use_live();
    let history = LocalResource::new(move || {
        live.settled.track();
        async { api::get::<Vec<Job>>("/jobs").await }
    });
    let sorting = RwSignal::new(VecDeque::<(JobCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let fetched = history.get().and_then(Result::ok).unwrap_or_default();
        let jobs = live.book.with(|book| book.overlay(&fetched));
        Arc::new(sorting.with(|s| sorted(jobs, s)))
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<JobItem>);
    let detail = move || {
        let item = shown.with(|s| s.get(selected.get()?).cloned())?;
        Some(job_detail(&item.0))
    };

    view! {
        <h1>"Jobs"</h1>
        <p class="lede">
            {move || match live.stream.get() {
                Stream::Live => "Live: state and progress arrive as they happen.".to_string(),
                Stream::Connecting => "Connecting to the service's events…".to_string(),
                Stream::Reconnecting => {
                    "The event stream dropped; reconnecting. What is shown may be out of date.".to_string()
                }
            }}
        </p>
        {move || match history.get() {
            None => view! { <Loading what="the jobs" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="jobs"
            label="Jobs".to_string()
            rows
            sorting
            selected
            on_open=Callback::new(|_| crate::screens::audit::focus("job-detail"))
            empty="No jobs yet. Start an audit or a selection from a source set.".to_string()
        />
        <section class="detail" id="job-detail" tabindex="-1" aria-label="Selected job">
            {move || match detail() {
                Some(view) => view,
                None => view! { <p class="note">"Select a job to see its details."</p> }.into_any(),
            }}
        </section>
    }
}

fn job_detail(live: &LiveJob) -> AnyView {
    let job = live.job.clone();
    let set_href = format!("/source-sets/{}", api::segment(&job.source_set));
    let result = (job.audit.is_some() || job.selection.is_some()).then(|| result_link(&job));
    view! {
        <h3>{format!("{} job #{}", kind_label(job.kind), job.id)}</h3>
        <dl class="facts">
            <dt>"State"</dt>
            <dd>{state_view(job.state)} " " {progress_view(live)}</dd>
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{job.source_set.clone()}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <code class="hash">{job.dat_revision.clone()}</code>
            </dd>
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
            {result
                .map(|link| {
                    view! {
                        <dt>"Result"</dt>
                        <dd>{link}</dd>
                    }
                })}
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
