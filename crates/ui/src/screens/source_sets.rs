use std::collections::VecDeque;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use leptos_struct_table::ColumnSort;
use title_select_api::{
    AddSourceSet, AuditEvidence, AuditSummary, Dat, DatRevisionRecord, Finding, GameStatus, Job,
    PathTemplateDefaults, ProfileRecord, Root, SelectionRecord, SourceSetRecord, StartAudit,
    StartSelection,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::events::use_live;
use crate::model::{
    audit_choices, audit_evidence_label, finding_label, game_status_label,
    needs_review_from_counts, short_sha, when,
};
use crate::parts::icon::{Glyph, LabelledIcon};
use crate::registration;
use crate::screens::audit::{ReAudit, queued};
use crate::screens::selection::SelectionQueued;
use crate::table::{DataTable, Grid, Rows, rows_of, sort_of};

#[derive(Clone, PartialEq)]
struct SetItem {
    set: SourceSetRecord,
    latest: Option<AuditSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SetCol {
    Name,
    Dat,
    Location,
    Latest,
    LastVerified,
}

fn set_href(name: &str) -> String {
    format!("/source-sets/{}", api::segment(name))
}

fn latest_view(latest: &Option<AuditSummary>) -> AnyView {
    match latest {
        Some(audit) => view! {
            <A href=format!("/audits/{}", audit.id)>{format!("#{}", audit.id)}</A>
            {format!(" · {}", when(&audit.finished_at))}
        }
        .into_any(),
        None => "Never audited".into_any(),
    }
}

impl Grid for SetItem {
    type Col = SetCol;
    const COLUMNS: &'static [SetCol] = &[
        SetCol::Name,
        SetCol::Dat,
        SetCol::Location,
        SetCol::Latest,
        SetCol::LastVerified,
    ];

    fn header(col: SetCol) -> &'static str {
        match col {
            SetCol::Name => "Source set",
            SetCol::Dat => "DAT",
            SetCol::Location => "Root and path",
            SetCol::Latest => "Latest audit",
            SetCol::LastVerified => "Last verified against",
        }
    }

    fn class(col: SetCol) -> &'static str {
        match col {
            SetCol::Location => "c-wide data",
            _ => "c-wide",
        }
    }

    fn sortable(col: SetCol) -> bool {
        matches!(col, SetCol::Name | SetCol::Dat | SetCol::Latest)
    }

    fn cell(&self, col: SetCol) -> AnyView {
        let set = &self.set;
        match col {
            SetCol::Name => {
                let name = set.name.clone();
                view! { <A href=set_href(&name)>{name}</A> }.into_any()
            }
            SetCol::Dat => set.dat.clone().into_any(),
            SetCol::Location => format!("{}: {}", set.root, set.path).into_any(),
            SetCol::Latest => latest_view(&self.latest),
            SetCol::LastVerified => match &set.last_verified_against {
                Some(last) => format!("{} ({})", last.version, short_sha(&last.sha256)).into_any(),
                None => ().into_any(),
            },
        }
    }
}

async fn sets_with_latest() -> Result<Vec<SetItem>, String> {
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    let mut items = Vec::with_capacity(sets.len());
    for set in sets {
        let history: Vec<AuditSummary> =
            api::get(&format!("/source-sets/{}/history", api::segment(&set.name))).await?;
        items.push(SetItem {
            set,
            latest: history.into_iter().next(),
        });
    }
    Ok(items)
}

/// The registered Source Sets, and the form to register another.
#[component]
pub fn SourceSets() -> impl IntoView {
    let live = use_live();
    let sets = LocalResource::new(move || {
        live.settled.track();
        sets_with_latest()
    });
    let sorting = RwSignal::new(VecDeque::<(SetCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items = sets.get().and_then(Result::ok).unwrap_or_default();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    SetCol::Dat => a.set.dat.cmp(&b.set.dat),
                    SetCol::Latest => a
                        .latest
                        .as_ref()
                        .map(|l| &l.finished_at)
                        .cmp(&b.latest.as_ref().map(|l| &l.finished_at)),
                    _ => a.set.name.cmp(&b.set.name),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<SetItem>);
    let go = navigator();

    view! {
        <h1>"Source sets"</h1>
        <p class="lede">
            "A source set is a folder in a root, audited against its DAT. Open one to start an audit or see its history."
        </p>
        <p class="page-links">
            <A href="/additions">
                <LabelledIcon glyph=Glyph::CirclePlus label="Additions" />
            </A>
        </p>
        {move || match sets.get() {
            None => view! { <Loading what="the source sets" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="source-sets"
            label="Source sets".to_string()
            rows
            sorting
            selected
            on_open=Callback::new(move |item: SetItem| go.run(set_href(&item.set.name)))
            empty="No source sets yet. Register a folder in a root below.".to_string()
        />
        <Register on_added=Callback::new(move |name: String| go.run(set_href(&name))) />
    }
}

#[component]
fn Register(on_added: Callback<String>) -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let defaults = LocalResource::new(|| api::get::<PathTemplateDefaults>("/path-templates"));
    let root = RwSignal::new(String::new());
    let dat = RwSignal::new(String::new());
    let template = RwSignal::new(None::<String>);
    // The path you typed over the proposal; `None` follows the template.
    let typed = RwSignal::new(None::<String>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let proposal = Memo::new(move |_| {
        let Some(template) = template.get() else {
            return Err("Loading the path template…".to_string());
        };
        let dat = dat.get();
        let dats = dats.get().and_then(Result::ok).unwrap_or_default();
        match dats.iter().find(|d| d.dat == dat) {
            Some(d) => registration::propose(&template, d),
            None => Err("Choose a DAT to propose a path.".to_string()),
        }
    });
    let path = Memo::new(move |_| {
        registration::chosen_path(typed.get().as_deref(), &proposal.get()).unwrap_or_default()
    });
    let preview = move || {
        let root_path = roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.name == root.get())
            .map(|r| r.path)
            .unwrap_or_default();
        let proposal = proposal.get();
        match (typed.get(), &proposal) {
            (None, Err(reason)) => Err(reason.clone()),
            _ => registration::preview(&root_path, &path.get()),
        }
    };

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = path.get_untracked();
        if chosen.trim().is_empty() {
            problem.set(Some(
                "Type a path, or change the template so it proposes one.".into(),
            ));
            return;
        }
        let request = AddSourceSet {
            root: root.get_untracked(),
            path: Some(chosen),
            template: None,
            dat: dat.get_untracked(),
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let added = api::post::<_, SourceSetRecord>("/source-sets", &request).await;
            busy.set(false);
            match added {
                Ok(set) => on_added.run(set.name),
                Err(message) => problem.set(Some(message)),
            }
        });
    };

    Effect::new(move || {
        if let Some(Ok(roots)) = roots.get()
            && root.get_untracked().is_empty()
            && let Some(first) = roots.first()
        {
            root.set(first.name.clone());
        }
    });
    Effect::new(move || {
        if let Some(Ok(dats)) = dats.get()
            && dat.get_untracked().is_empty()
            && let Some(first) = dats.first()
        {
            dat.set(first.dat.clone());
        }
    });
    Effect::new(move || {
        if let Some(Ok(defaults)) = defaults.get()
            && template.get_untracked().is_none()
        {
            template.set(Some(defaults.source_set));
        }
    });

    view! {
        <section aria-labelledby="register-title">
            <h2 id="register-title">"Register a source set"</h2>
            <form class="form" on:submit=submit>
                <label>
                    "Root"
                    <select required prop:value=root on:change=move |ev| root.set(event_target_value(&ev))>
                        {move || {
                            roots
                                .get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
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
                    "DAT"
                    <select required prop:value=dat on:change=move |ev| dat.set(event_target_value(&ev))>
                        {move || {
                            dats.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|d| view! { <option value=d.dat.clone()>{d.dat.clone()}</option> })
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
                <button type="submit" class="primary" disabled=busy>
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
                "The path template proposes a path from the DAT: {provider} is its provider, {dat} its name and \
                {platform} its front-end folder name. Change the template or type over the path. A registered \
                source set keeps its path when the template changes."
            </p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

#[derive(Clone)]
struct HistoryItem {
    audit: AuditSummary,
    reaudit: ReAudit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HistoryCol {
    Audit,
    Finished,
    Revision,
    Findings,
    Games,
    Action,
}

fn findings_text(audit: &AuditSummary) -> String {
    Finding::ALL
        .iter()
        .filter_map(|f| {
            audit
                .findings
                .get(f)
                .filter(|n| **n > 0)
                .map(|n| format!("{n} {}", finding_label(*f)))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn games_text(audit: &AuditSummary) -> String {
    GameStatus::ALL
        .iter()
        .filter_map(|s| {
            audit
                .games
                .get(s)
                .map(|n| format!("{n} {}", game_status_label(*s)))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

impl Grid for HistoryItem {
    type Col = HistoryCol;
    const COLUMNS: &'static [HistoryCol] = &[
        HistoryCol::Audit,
        HistoryCol::Finished,
        HistoryCol::Revision,
        HistoryCol::Findings,
        HistoryCol::Games,
        HistoryCol::Action,
    ];

    fn header(col: HistoryCol) -> &'static str {
        match col {
            HistoryCol::Audit => "Audit",
            HistoryCol::Finished => "Finished",
            HistoryCol::Revision => "DAT revision",
            HistoryCol::Findings => "Findings",
            HistoryCol::Games => "Game statuses",
            HistoryCol::Action => "Action",
        }
    }

    fn class(col: HistoryCol) -> &'static str {
        match col {
            HistoryCol::Audit => "c-id",
            HistoryCol::Finished => "c-time data",
            HistoryCol::Revision => "c-revision",
            HistoryCol::Findings => "c-wide",
            HistoryCol::Games => "c-wide",
            HistoryCol::Action => "c-action",
        }
    }

    fn cell(&self, col: HistoryCol) -> AnyView {
        let audit = &self.audit;
        match col {
            HistoryCol::Audit => view! {
                <A href=format!("/audits/{}", audit.id)>{format!("#{}", audit.id)}</A>
                {audit.pruned.then_some(view! { " " <span class="tag">"Pruned"</span> })}
            }
            .into_any(),
            HistoryCol::Finished => when(&audit.finished_at).into_any(),
            HistoryCol::Revision => format!(
                "{} ({})",
                audit.dat_revision_version,
                short_sha(&audit.dat_revision_sha256)
            )
            .into_any(),
            HistoryCol::Findings => {
                let review = needs_review_from_counts(&audit.findings);
                let verb = if review == 1 { "needs" } else { "need" };
                format!("{review} {verb} review · {}", findings_text(audit)).into_any()
            }
            HistoryCol::Games => games_text(audit).into_any(),
            HistoryCol::Action => {
                let reaudit = self.reaudit;
                let label = format!(
                    "Re-audit against DAT revision {}",
                    audit.dat_revision_version
                );
                view! {
                    <button type="button" class="small" aria-label=label.clone() title=label on:click=move |_| reaudit.start()>
                        "Re-audit"
                    </button>
                }
                .into_any()
            }
        }
    }
}

/// One Source Set: its latest Audit, starting an Audit, and its history.
#[component]
pub fn SourceSet() -> impl IntoView {
    let params = use_params_map();
    let name = Memo::new(move |_| params.read().get("name").unwrap_or_default());
    let set = LocalResource::new(move || {
        let name = name.get();
        async move {
            api::get::<SourceSetRecord>(&format!("/source-sets/{}", api::segment(&name))).await
        }
    });
    move || match set.get() {
        None => view! { <Loading what="the source set" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Source set"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(set)) => view! { <SourceSetView set /> }.into_any(),
    }
}

#[component]
fn SourceSetView(set: SourceSetRecord) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(set.name.clone());
    let history = LocalResource::new(move || {
        live.settled.track();
        let path = format!("/source-sets/{}/history", api::segment(&name.get_value()));
        async move { api::get::<Vec<AuditSummary>>(&path).await }
    });
    // The record's "last verified against" changes when an Audit of this Source Set settles.
    let record = LocalResource::new(move || {
        live.settled.track();
        let path = format!("/source-sets/{}", api::segment(&name.get_value()));
        async move { api::get::<SourceSetRecord>(&path).await }
    });
    let first_verified = set.last_verified_against.clone();
    let last_verified = Memo::new(move |_| match record.get() {
        Some(Ok(fresh)) => fresh.last_verified_against,
        _ => first_verified.clone(),
    });
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let sorting = RwSignal::new(VecDeque::<(HistoryCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let items = Memo::new(move |_| {
        let audits = history.get().and_then(Result::ok).unwrap_or_default();
        let items: Vec<HistoryItem> = audits
            .into_iter()
            .map(|audit| HistoryItem {
                reaudit: ReAudit::new(
                    audit.source_set.clone(),
                    audit.dat_revision_sha256.clone(),
                    outcome,
                ),
                audit,
            })
            .collect();
        Arc::new(ItemsById(items))
    });
    let rows = Signal::derive(move || rows_of(items.get().0.clone()) as Rows<HistoryItem>);
    let go = navigator();

    let bound = set.dat.clone();
    let choices = Memo::new(move |_| {
        let audits = history.get().and_then(Result::ok).unwrap_or_default();
        let revisions = dats
            .get()
            .and_then(Result::ok)
            .and_then(|dats| dats.into_iter().find(|d| d.dat == bound))
            .map(|d| d.revisions)
            .unwrap_or_default();
        let (default, newer) = audit_choices(&audits, &revisions);
        (
            default.cloned(),
            newer.into_iter().cloned().collect::<Vec<_>>(),
        )
    });
    let revision = RwSignal::new(String::new());
    let evidence = RwSignal::new(AuditEvidence::Incremental);
    let start = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = revision.get_untracked();
        let request = StartAudit {
            source_set: name.get_value(),
            revision: (!chosen.is_empty()).then_some(chosen),
            evidence: evidence.get_untracked(),
        };
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, Job>("/audits", &request).await));
        });
    };

    view! {
        <h1>{set.name.clone()}</h1>
        <dl class="facts">
            <dt>"DAT"</dt>
            <dd>{set.dat.clone()}</dd>
            <dt>"Root and path"</dt>
            <dd>
                <code class="path">{format!("{}: {}", set.root, set.path)}</code>
            </dd>
            <dt>"Registered"</dt>
            <dd>{when(&set.added_at)}</dd>
            <dt>"Repair and kept files"</dt>
            <dd>
                <A href=format!("/source-sets/{}/repair", api::segment(&set.name))>"Repair this source set"</A>
            </dd>
            <dt>"Latest audit"</dt>
            <dd>
                {move || {
                    let latest = history.get().and_then(Result::ok).and_then(|h| h.into_iter().next());
                    latest_view(&latest)
                }}
            </dd>
            <dt>"Last verified against"</dt>
            <dd>
                {move || {
                    last_verified
                        .get()
                        .map_or("Never".to_string(), |last| format!("{} ({})", last.version, when(&last.finished_at)))
                }}
                " "
                {move || last_verified.get().map(|last| view! { <code class="hash">{last.sha256}</code> })}
            </dd>
        </dl>

        <section aria-labelledby="start-title">
            <h2 id="start-title">"Start an audit"</h2>
            <form class="form" on:submit=start>
                <label>
                    "DAT revision"
                    <select prop:value=revision on:change=move |ev| revision.set(event_target_value(&ev))>
                        {move || {
                            let (default, newer) = choices.get();
                            let default = match default {
                                Some(r) => {
                                    format!("{} ({}), the default", r.version, short_sha(&r.sha256))
                                }
                                None => "The default".to_string(),
                            };
                            view! {
                                <option value="">{default}</option>
                                {newer
                                    .into_iter()
                                    .map(|r| {
                                        let label = format!("{} ({}), newer", r.version, short_sha(&r.sha256));
                                        view! { <option value=r.sha256>{label}</option> }
                                    })
                                    .collect_view()}
                            }
                        }}
                    </select>
                </label>
                <label>
                    "Evidence"
                    <select
                        prop:value=move || evidence.get().as_str()
                        on:change=move |ev| {
                            if let Some(chosen) = AuditEvidence::parse(&event_target_value(&ev)) {
                                evidence.set(chosen);
                            }
                        }
                    >
                        {AuditEvidence::ALL
                            .into_iter()
                            .map(|e| view! { <option value=e.as_str()>{audit_evidence_label(e)}</option> })
                            .collect_view()}
                    </select>
                </label>
                <button type="submit" class="primary">
                    "Start audit"
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => queued(&job).into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>

        <Selections name choices />

        <section aria-labelledby="history-title">
            <h2 id="history-title">"History"</h2>
            <p class="lede">"Latest first. A pruned audit keeps only its summary."</p>
            {move || match history.get() {
                None => view! { <Loading what="the history" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            <DataTable
                id="history"
                label="History".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(move |item: HistoryItem| go.run(format!("/audits/{}", item.audit.id)))
                empty="Not audited yet.".to_string()
            />
        </section>
    }
}

/// The Selections made for a Source Set, and the form to make another.
#[component]
fn Selections(
    name: StoredValue<String>,
    /// The DAT Revisions to offer: the default, and newer ones.
    choices: Memo<(Option<DatRevisionRecord>, Vec<DatRevisionRecord>)>,
) -> impl IntoView {
    let live = use_live();
    let selections = LocalResource::new(move || {
        live.settled.track();
        let path = format!("/selections?source_set={}", api::segment(&name.get_value()));
        async move { api::get::<Vec<SelectionRecord>>(&path).await }
    });
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let saved = move || {
        profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.latest.is_some())
            .collect::<Vec<_>>()
    };
    let profile = RwSignal::new(String::new());
    Effect::new(move || {
        if profile.get_untracked().is_empty()
            && let Some(first) = saved().into_iter().next()
        {
            profile.set(first.name);
        }
    });
    let revision = RwSignal::new(String::new());
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let start = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = revision.get_untracked();
        let request = StartSelection {
            source_set: name.get_value(),
            revision: (!chosen.is_empty()).then_some(chosen),
            profile: profile.get_untracked(),
            version: None,
        };
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, Job>("/selections", &request).await));
        });
    };
    view! {
        <section aria-labelledby="selections-title">
            <h2 id="selections-title">"Selections"</h2>
            <p class="lede">"Latest first. Each records the selection profile version it used."</p>
            {move || match profiles.get() {
                Some(Ok(_)) if saved().is_empty() => {
                    view! {
                        <p class="note">
                            "To make a selection, first "
                            <A href="/new-profile">"create a selection profile"</A>
                            "."
                        </p>
                    }
                        .into_any()
                }
                _ => {
                    view! {
                        <form class="form" on:submit=start>
                            <label>
                                "Selection profile"
                                <select prop:value=profile on:change=move |ev| profile.set(event_target_value(&ev))>
                                    {move || {
                                        saved()
                                            .into_iter()
                                            .map(|p| {
                                                let label = format!(
                                                    "{}, version {}",
                                                    p.name,
                                                    p.latest.as_ref().map_or(0, |v| v.version),
                                                );
                                                view! { <option value=p.name>{label}</option> }
                                            })
                                            .collect_view()
                                    }}
                                </select>
                            </label>
                            <label>
                                "DAT revision"
                                <select prop:value=revision on:change=move |ev| revision.set(event_target_value(&ev))>
                                    {move || {
                                        let (default, newer) = choices.get();
                                        let default = match default {
                                            Some(r) => format!("{} ({}), the default", r.version, short_sha(&r.sha256)),
                                            None => "The default".to_string(),
                                        };
                                        view! {
                                            <option value="">{default}</option>
                                            {newer
                                                .into_iter()
                                                .map(|r| {
                                                    let label = format!("{} ({}), newer", r.version, short_sha(&r.sha256));
                                                    view! { <option value=r.sha256>{label}</option> }
                                                })
                                                .collect_view()}
                                        }
                                    }}
                                </select>
                            </label>
                            <button type="submit" class="primary" disabled=move || profile.with(String::is_empty)>
                                "Make selection"
                            </button>
                        </form>
                    }
                        .into_any()
                }
            }}
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => view! { <SelectionQueued job=job.id /> }.into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
            {move || match selections.get() {
                None => view! { <Loading what="the selections" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(list)) if list.is_empty() => {
                    view! { <p class="note">"No selections yet."</p> }.into_any()
                }
                Some(Ok(list)) => {
                    view! {
                        <ul class="plain selections">
                            {list.into_iter().map(selection_line).collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

fn selection_line(s: SelectionRecord) -> impl IntoView {
    let summary = s.summary;
    let review = summary.needs_review();
    view! {
        <li>
            <A href=format!("/selections/{}", s.id)>{format!("Selection #{}", s.id)}</A>
            {format!(
                ", {} version {}, DAT revision {} ({}), {}: {} selected, {} excluded",
                s.profile.name,
                s.profile.version,
                s.dat_revision_version,
                short_sha(&s.dat_revision_sha256),
                when(&s.created_at),
                summary.winners,
                summary.exclusions,
            )}
            {(review > 0).then(|| view! { <span class="review">{format!(", {review} to review")}</span> })}
            {s.pruned.then_some(" (pruned: only the summary is kept)")}
        </li>
    }
}

/// History items compare by Audit, so a refetch that changes nothing does not reload the table.
struct ItemsById(Vec<HistoryItem>);

impl PartialEq for ItemsById {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(&other.0).all(|(a, b)| a.audit == b.audit)
    }
}
