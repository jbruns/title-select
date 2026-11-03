use std::cmp::Ordering;
use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::repair::{PlanRepair, RepairPlan};
use title_select_api::{
    AddSourceSet, AuditEvidence, Dat, GameStatus, Job, PathTemplateDefaults, Root, SourceSetRecord,
    SourceSetSummary, StartAudit,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::events::use_live;
use crate::model::{needs_review_from_counts, short_sha, when};
use crate::parts::icon::Glyph;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, List, ListRow, RowAction, ToolAction, Width, last_good_rows,
};
use crate::parts::overlay::Drawer;
use crate::parts::pending::PendingStatus;
use crate::parts::status::{SourceSetStatus, Status, StatusBadge, StatusIcon};
use crate::parts::url_state::encode;
use crate::registration;
use crate::source_set_model::{self as model, PageTab};

/// A source set's page.
pub fn set_href(name: &str) -> String {
    format!("/source-sets/{}", api::segment(name))
}

/// A tab of a source set's page.
pub fn tab_href(name: &str, tab: PageTab) -> String {
    format!("{}?tab={}", set_href(name), tab.slug())
}

/// Where you preview a selection of the source set.
pub fn preview_href(name: &str) -> String {
    format!("/selections/new?source_set={}", encode(name))
}

/// Where you make a library folder for the source set.
pub fn new_library_href(name: &str) -> String {
    format!("/libraries/new?source_set={}", encode(name))
}

/// Queues an incremental audit of `name` against its default DAT revision, and says how that went in `said`.
pub fn start_audit(name: String, said: RwSignal<Option<String>>) {
    let request = StartAudit {
        source_set: name.clone(),
        revision: None,
        evidence: AuditEvidence::Incremental,
    };
    leptos::task::spawn_local(async move {
        let message = match api::post::<_, Job>("/audits", &request).await {
            Ok(job) => format!("Queued audit job #{} for {name}.", job.id),
            Err(problem) => format!("Couldn't audit {name}: {problem}"),
        };
        said.set(Some(message));
    });
}

/// Queues a check of `name` for files changed since its latest audit, and says how that went in `said`.
pub fn start_check(name: String, said: RwSignal<Option<String>>) {
    leptos::task::spawn_local(async move {
        let path = format!("{}/stat-walks", set_href(&name));
        let message = match api::post::<_, Job>(&path, &()).await {
            Ok(job) => format!("Queued check #{} of {name} for changes.", job.id),
            Err(problem) => format!("Couldn't check {name} for changes: {problem}"),
        };
        said.set(Some(message));
    });
}

/// Each cause of a stale source set as its icon, whose tooltip says what it found.
pub fn stale_icons(stale: &[title_select_api::SourceSetStale]) -> AnyView {
    model::stale_causes(stale)
        .into_iter()
        .map(|(cause, line)| {
            view! {
                <span class="stale-cause" title=line>
                    <StatusIcon status=cause tab_stop=false />
                </span>
            }
        })
        .collect_view()
        .into_any()
}

/// The status line while repairs are proposed for `names`.
pub fn proposing_status(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("Making the repair plan for {one}… a large source set can take a while."),
        _ => format!(
            "Making repair plans for {}… a large source set can take a while.",
            names.join(", ")
        ),
    }
}

#[derive(Clone, PartialEq)]
struct SetRow {
    summary: SourceSetSummary,
    status: SourceSetStatus,
}

impl SetRow {
    fn name(&self) -> &str {
        &self.summary.set.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetCol {
    Status,
    Name,
    Games,
    Dat,
    Audited,
}

/// The source set's game counts by status, each an icon and its number.
fn game_counts(games: &BTreeMap<GameStatus, usize>) -> AnyView {
    view! {
        <span class="game-counts">
            {GameStatus::ALL
                .into_iter()
                .map(|s| {
                    view! {
                        <span class="game-count">
                            <StatusIcon status=s tab_stop=false />
                            <span class="n">{games.get(&s).copied().unwrap_or(0)}</span>
                        </span>
                    }
                })
                .collect_view()}
        </span>
    }
    .into_any()
}

impl ListRow for SetRow {
    type Col = SetCol;
    const COLUMNS: &'static [SetCol] = &[
        SetCol::Status,
        SetCol::Name,
        SetCol::Games,
        SetCol::Dat,
        SetCol::Audited,
    ];

    fn column(col: SetCol) -> Column {
        match col {
            SetCol::Status => Column::new("status", "Status", Width::Fixed(6.0))
                .sortable()
                .hint("Running, repair open, needs review, stale, verified or never audited"),
            SetCol::Name => Column::new(
                "name",
                "Source set",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            SetCol::Games => Column::new("games", "Games", Width::Fixed(15.0))
                .hint("Complete, incomplete and missing games in its latest audit"),
            SetCol::Dat => Column::new(
                "dat",
                "DAT",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            SetCol::Audited => {
                Column::new("audited", "Latest audit", Width::Fixed(13.0)).sortable()
            }
        }
    }

    fn key(&self) -> String {
        self.summary.set.name.clone()
    }

    fn title(&self) -> String {
        self.summary.set.name.clone()
    }

    fn search(&self) -> String {
        let set = &self.summary.set;
        let causes: Vec<&str> = model::stale_causes(&set.stale)
            .into_iter()
            .map(|(cause, _)| cause.look().label)
            .collect();
        format!(
            "{} {} {} {} {} {}",
            set.name,
            set.dat,
            set.root,
            set.path,
            self.status.look().label,
            causes.join(" ")
        )
    }

    fn cell(&self, col: SetCol) -> AnyView {
        let s = &self.summary;
        match col {
            SetCol::Status => {
                let causes =
                    (self.status == SourceSetStatus::Stale).then(|| stale_icons(&s.set.stale));
                view! {
                    <StatusIcon status=self.status tab_stop=false />
                    {causes}
                }
                .into_any()
            }
            SetCol::Name => view! { <Clip text=s.set.name.clone() /> }.into_any(),
            SetCol::Games => match &s.latest {
                Some(audit) => game_counts(&audit.games),
                None => "—".into_any(),
            },
            SetCol::Dat => view! { <Clip text=s.set.dat.clone() /> }.into_any(),
            SetCol::Audited => match &s.latest {
                Some(audit) => view! { <Clip text=when(&audit.finished_at) /> }.into_any(),
                None => "Never".into_any(),
            },
        }
    }

    fn compare(&self, other: &Self, col: SetCol) -> Ordering {
        let (a, b) = (&self.summary, &other.summary);
        match col {
            SetCol::Status => self.status.rank().cmp(&other.status.rank()),
            SetCol::Name | SetCol::Games => a.set.name.cmp(&b.set.name),
            SetCol::Dat => a.set.dat.cmp(&b.set.dat),
            SetCol::Audited => a
                .latest
                .as_ref()
                .map(|l| &l.finished_at)
                .cmp(&b.latest.as_ref().map(|l| &l.finished_at)),
        }
    }

    fn href(&self) -> Option<String> {
        Some(set_href(self.name()))
    }
}

/// What a list action reports to, and what it changes.
#[derive(Clone, Copy)]
struct Ctx {
    said: RwSignal<Option<String>>,
    proposing: RwSignal<Vec<String>>,
    refresh: RwSignal<u64>,
}

impl Ctx {
    /// Proposes a repair of each of `names` in turn, saying how each went.
    fn propose(self, names: Vec<String>) {
        let names: Vec<String> = names
            .into_iter()
            .filter(|n| !self.proposing.with_untracked(|p| p.contains(n)))
            .collect();
        if names.is_empty() {
            return;
        }
        self.proposing.update(|p| p.extend(names.iter().cloned()));
        leptos::task::spawn_local(async move {
            let mut said = Vec::new();
            for name in names {
                let path = format!("{}/repairs", set_href(&name));
                let made = api::post::<_, RepairPlan>(&path, &PlanRepair::default()).await;
                said.push(match made {
                    Ok(plan) => format!(
                        "Proposed repair #{} of {name}: {} items to review on its Repair tab.",
                        plan.id,
                        plan.content.items.len()
                    ),
                    Err(problem) => format!("Couldn't propose a repair of {name}: {problem}"),
                });
                self.proposing.update(|p| p.retain(|n| *n != name));
            }
            self.said.set(Some(said.join(" ")));
            self.refresh.update(|n| *n += 1);
        });
    }
}

fn set_actions(row: &SetRow, ctx: Ctx) -> Vec<RowAction> {
    let name = row.name().to_string();
    let mut actions = vec![{
        let name = name.clone();
        RowAction::run(Glyph::ShieldCheck, format!("Audit {name}"), move || {
            start_audit(name.clone(), ctx.said)
        })
    }];
    if row.summary.latest.is_some() {
        let name = name.clone();
        actions.push(RowAction::run(
            Glyph::ScanSearch,
            "Check for changes",
            move || start_check(name.clone(), ctx.said),
        ));
    }
    actions.push(match &row.summary.repair {
        Some(open) => RowAction::go(
            Glyph::Wrench,
            format!("Review its repair #{}", open.id),
            tab_href(&name, PageTab::Repair),
        ),
        None => {
            let name = name.clone();
            RowAction::run(Glyph::Wrench, "Propose a repair", move || {
                ctx.propose(vec![name.clone()])
            })
        }
    });
    actions.extend([
        RowAction::go(Glyph::Eye, "Preview a selection", preview_href(&name)),
        RowAction::go(
            Glyph::Library,
            "New library folder",
            new_library_href(&name),
        ),
        RowAction::go(
            Glyph::FolderTree,
            "Its library folders",
            tab_href(&name, PageTab::Libraries),
        ),
        RowAction::go(
            Glyph::CirclePlus,
            "Its additions",
            tab_href(&name, PageTab::Additions),
        ),
        RowAction::go(
            Glyph::History,
            "Its history",
            tab_href(&name, PageTab::History),
        ),
        RowAction::go(
            Glyph::Archive,
            "Its kept files",
            tab_href(&name, PageTab::Kept),
        ),
    ]);
    actions
}

fn set_detail(row: SetRow) -> AnyView {
    let s = row.summary;
    let name = s.set.name.clone();
    view! {
        <p>
            <StatusBadge status=row.status />
        </p>
        {stale_lines(&s.set)}
        <dl class="facts">
            <dt>"DAT"</dt>
            <dd>{s.set.dat.clone()}</dd>
            <dt>"Root and path"</dt>
            <dd>
                <code class="path">{format!("{}: {}", s.set.root, s.set.path)}</code>
            </dd>
            <dt>"Registered"</dt>
            <dd>{when(&s.set.added_at)}</dd>
            <dt>"Latest audit"</dt>
            <dd>
                {match &s.latest {
                    Some(audit) => {
                        let review = needs_review_from_counts(&audit.findings);
                        view! {
                            <A href=format!("/audits/{}", audit.id)>{format!("#{}", audit.id)}</A>
                            {format!(
                                ", {}, against {}; {review} need review",
                                when(&audit.finished_at),
                                audit.dat_revision_version,
                            )}
                        }
                            .into_any()
                    }
                    None => "Never audited".into_any(),
                }}
            </dd>
            {s.latest.as_ref().map(|a| view! {
                <dt>"Games"</dt>
                <dd>{game_counts(&a.games)}</dd>
            })}
            <dt>"Last verified against"</dt>
            <dd>
                {s.set
                    .last_verified_against
                    .as_ref()
                    .map_or("Never".to_string(), |l| format!("{} ({})", l.version, short_sha(&l.sha256)))}
            </dd>
            <dt>"Repair"</dt>
            <dd>
                {match &s.repair {
                    Some(open) => view! {
                        <A href=tab_href(&name, PageTab::Repair)>
                            {format!(
                                "Repair #{} {}",
                                open.id,
                                if open.approved { "approved, not finished" } else { "waits for your review" },
                            )}
                        </A>
                    }
                        .into_any(),
                    None => "None open".into_any(),
                }}
            </dd>
        </dl>
    }
    .into_any()
}

/// Why the source set is stale, one line each with its cause's icon, and whether it follows its DAT.
pub fn stale_lines(set: &SourceSetRecord) -> AnyView {
    let causes = model::stale_causes(&set.stale);
    (!causes.is_empty())
        .then(|| {
            view! {
                <ul class="plain reasons">
                    {causes
                        .into_iter()
                        .map(|(cause, line)| {
                            view! {
                                <li>
                                    <StatusIcon status=cause tab_stop=false />
                                    " "
                                    {line}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
        })
        .into_any()
}

async fn summaries() -> Result<Vec<SourceSetSummary>, String> {
    api::get("/source-set-summaries").await
}

/// The registered source sets with their status, every action on them, and the form to register another.
#[component]
pub fn SourceSets() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        summaries()
    });
    let latest = last_good_rows(move || loaded.get());
    let rows = Memo::new(move |_| {
        let running = live
            .book
            .with(|b| model::running(b.newest_first().iter().map(|l| &l.job)));
        let rows: Vec<SetRow> = latest.with(|sets| {
            sets.iter()
                .map(|s| SetRow {
                    status: model::status(s, running.contains(&s.set.name)),
                    summary: s.clone(),
                })
                .collect()
        });
        std::sync::Arc::new(rows)
    });
    let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.status)));
    let ctx = Ctx {
        said: RwSignal::new(None),
        proposing: RwSignal::new(Vec::new()),
        refresh,
    };
    let registering = RwSignal::new(false);
    let go = navigator();
    // Previewing a selection or making a library folder is about one source set: the first checked.
    let one = move |chosen: Vec<SetRow>, href: fn(&str) -> String| match chosen.as_slice() {
        [row] => go.run(href(row.name())),
        [row, ..] => {
            ctx.said.set(Some(format!(
                "That works on one source set at a time: going on with {}.",
                row.name()
            )));
            go.run(href(row.name()));
        }
        [] => {}
    };
    let actions: Vec<ToolAction<SetRow>> = vec![
        ToolAction::page(Glyph::Plus, "Register", move || registering.set(true)),
        ToolAction::<SetRow>::checked(Glyph::ShieldCheck, "Audit", move |chosen| {
            for row in chosen.rows {
                start_audit(row.name().to_string(), ctx.said);
            }
        }),
        ToolAction::<SetRow>::checked(Glyph::ScanSearch, "Check for changes", move |chosen| {
            for row in chosen.rows {
                start_check(row.name().to_string(), ctx.said);
            }
        }),
        ToolAction::<SetRow>::checked(Glyph::Wrench, "Propose repair", move |chosen| {
            ctx.propose(chosen.rows.iter().map(|r| r.name().to_string()).collect())
        }),
        ToolAction::<SetRow>::checked(Glyph::Eye, "Preview selection", move |chosen| {
            one(chosen.rows, preview_href)
        }),
        ToolAction::<SetRow>::checked(Glyph::Library, "New library folder", move |chosen| {
            one(chosen.rows, new_library_href)
        }),
    ];

    view! {
        <h1>"Source sets"</h1>
        <p class="lede">
            "A source set is a folder in a root, audited against its DAT. Open one for its games, entries, library \
            folders, history and repair."
        </p>
        {move || match loaded.get() {
            None => view! { <Loading what="the source sets" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <Show when=move || ctx.proposing.with(|p| !p.is_empty())>
            <PendingStatus status=Signal::derive(move || ctx.proposing.with(|p| proposing_status(p))) />
        </Show>
        <p class="done" role="status">
            {move || ctx.said.get()}
        </p>
        <List
            id="source-sets"
            label="Source sets"
            rows
            empty="No source sets yet. Choose Register to add a folder in a root."
            placeholder="Filter by name, DAT, root, path or status"
            actions
            row_actions=Callback::new(move |row: SetRow| set_actions(&row, ctx))
            detail=Callback::new(set_detail)
            legend=move || view! { <Legend statuses list="source-sets" /> }
        />
        <Drawer open=registering title="Register a source set">
            <Register on_added=Callback::new(move |name: String| {
                registering.set(false);
                go.run(set_href(&name))
            }) />
        </Drawer>
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
                // The form may be gone by now.
                Ok(set) => {
                    on_added.try_run(set.name);
                }
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
        <section aria-label="Register a source set">
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
