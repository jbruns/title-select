//! A source set's page: one hub, in tabs, for its status, games, entries, library folders, additions, history, kept
//! files and repair. The tab is the URL's `tab`, so every tab can be linked to.

use std::cmp::Ordering;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::NavigateOptions;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use title_select_api::additions::AdditionsSet;
use title_select_api::build::{BuildSummary, LibraryFolder};
use title_select_api::ingest::IngestPlan;
use title_select_api::repair::RepairPlan;
use title_select_api::{
    AuditEvidence, AuditSummary, Dat, Finding, FollowedRevision, GameStatus, Job, SetFollow,
    SourceSetRecord, SourceSetSummary, StartAudit, StatWalk,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::build_model::build_state_label;
use crate::events::use_live;
use crate::library_model::live_status;
use crate::model::{
    EntryFilter, audit_choices, audit_evidence_label, needs_review_from_counts, short_sha, when,
};
use crate::parts::icon::Glyph;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Column, List, ListRow, RowAction, Width, last_good_rows};
use crate::parts::pending::PendingStatus;
use crate::parts::status::{
    FindingStatus, LibraryStatus, StaleCause, Status, StatusBadge, StatusIcon,
};
use crate::parts::url_state::encode;
use crate::screens::additions::SetView;
use crate::screens::audit::{EntriesPanel, GamesPanel, load, queued};
use crate::screens::library_folders::live_jobs;
use crate::screens::lifecycle::{LifecycleSection, Paths, PlanAction, UnregisterAction};
use crate::screens::repair::{MAKING_PLAN, RepairDesk, RepairTab, SourceKept};
use crate::screens::source_sets::{
    new_library_href, preview_href, stale_icons, stale_lines, start_audit, start_check, tab_href,
};
use crate::source_set_model::{self as model, Happened, Moment, PageTab};

/// What following a DAT does, for the toggle's tooltip.
const FOLLOW_HINT: &str =
    "A newer revision of its DAT then makes it stale; following never starts an audit";

/// One source set's page.
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
        // Each source set's page is its own, from its repair desk down: one left for another's is gone, not kept
        // while the other loads.
        Some(Ok(set)) if set.name == name.get() => view! { <SetPage set /> }.into_any(),
        Some(Ok(_)) => view! { <Loading what="the source set" /> }.into_any(),
    }
}

/// The source set's repair page from before its tabs: a plan it names opens in the Repair tab, and otherwise its kept
/// files, which the page held too.
#[component]
pub fn RepairRedirect() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let navigate = use_navigate();
    Effect::new(move || {
        let name = params.read().get("name").unwrap_or_default();
        let to = match query.read().get("plan") {
            Some(plan) => format!(
                "{}&plan={}",
                tab_href(&name, PageTab::Repair),
                encode(&plan)
            ),
            None => tab_href(&name, PageTab::Kept),
        };
        navigate(
            &to,
            NavigateOptions {
                replace: true,
                ..Default::default()
            },
        );
    });
}

#[component]
fn SetPage(set: SourceSetRecord) -> impl IntoView {
    let live = use_live();
    let query = use_query_map();
    let name = StoredValue::new(set.name.clone());
    let desk = RepairDesk::new(set.name.clone(), set.dat.clone());
    let refresh = RwSignal::new(0u64);
    let summary = LocalResource::new(move || {
        live.settled.track();
        desk.track();
        refresh.track();
        // Owned, as the page may be gone by the time the answers come.
        let name = name.get_value();
        async move {
            let sets: Vec<SourceSetSummary> = api::get("/source-set-summaries").await?;
            let dats: Vec<Dat> = api::get("/dats").await?;
            let summary = sets
                .into_iter()
                .find(|s| s.set.name == name)
                .ok_or_else(|| "That source set is gone.".to_string())?;
            let revisions = dats
                .into_iter()
                .find(|d| d.dat == summary.set.dat)
                .map(|d| d.revisions)
                .unwrap_or_default();
            Ok::<_, String>((summary, revisions))
        }
    });
    let good = Memo::new(move |previous: Option<&Option<_>>| match summary.get() {
        Some(Ok(s)) => Some(s),
        _ => previous.cloned().flatten(),
    });
    let jobs_running = move || {
        live.book
            .with(|b| model::running(b.newest_first().iter().map(|l| &l.job)))
    };
    let status = Memo::new(move |_| {
        good.get().map(|(s, _)| {
            (
                model::status(&s, jobs_running().contains(&name.get_value())),
                s.set.stale,
            )
        })
    });
    let repair_state = Memo::new(move |_| {
        let open = good.get().and_then(|(s, _)| s.repair);
        let job_running = open.as_ref().and_then(|o| o.job).is_some_and(|job| {
            live.book
                .with(|b| b.get(job).is_some_and(|l| !l.job.state.is_final()))
        });
        model::repair_state(open.as_ref(), job_running, desk.proposing().get())
    });
    let tab = Memo::new(move |_| PageTab::from_query(query.read().get("tab").as_deref()));
    // Each its own memo, so a change to the rest of the query (a filter, a sort, a row) remounts no tab.
    let named = Memo::new(move |_| query.read().get("plan").and_then(|p| p.parse::<i64>().ok()));
    let current = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .and_then(|(s, _)| s.repair.as_ref().map(|r| r.id))
        })
    });
    let running = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .is_some_and(|(s, _)| s.repair.as_ref().is_some_and(|r| r.approved))
        })
    });
    let ready = Memo::new(move |_| good.with(Option::is_some));
    let said = RwSignal::new(None::<String>);
    let audit_now = move |_| start_audit(name.get_value(), said);
    let check_now = move |_| start_check(name.get_value(), said);
    let audited =
        Memo::new(move |_| good.with(|g| g.as_ref().is_some_and(|(s, _)| s.latest.is_some())));
    let follows = Memo::new(move |_| good.with(|g| g.as_ref().is_some_and(|(s, _)| s.set.follow)));
    let follow = move |ev: leptos::ev::Event| {
        let request = SetFollow {
            follow: event_target_checked(&ev),
        };
        leptos::task::spawn_local(async move {
            let path = format!("/source-sets/{}/follow", api::segment(&name.get_value()));
            if let Err(problem) = api::put::<_, SourceSetRecord>(&path, &request).await {
                said.set(Some(format!(
                    "Couldn't change whether it follows its DAT: {problem}"
                )));
            }
            refresh.update(|n| *n += 1);
        });
    };

    view! {
        <header class="page-head">
            <h1>{set.name.clone()}</h1>
            {move || {
                status
                    .get()
                    .map(|(s, stale)| {
                        let causes = (s == model::STALE).then(|| stale_icons(&stale));
                        view! {
                            <StatusBadge status=s />
                            {causes}
                        }
                    })
            }}
        </header>
        <div class="actions">
            <button type="button" on:click=audit_now>
                "Audit"
            </button>
            <button
                type="button"
                on:click=check_now
                disabled=move || !audited.get()
                title="Compares sizes and times with its latest audit, reading no file"
            >
                "Check for changes"
            </button>
            <label class="inline" title=FOLLOW_HINT>
                <input type="checkbox" prop:checked=follows on:change=follow />
                " Follow its DAT"
            </label>
            <A href=preview_href(&set.name) attr:class="button">
                "Preview a selection"
            </A>
            <A href=new_library_href(&set.name) attr:class="button">
                "New library folder"
            </A>
            <A href=tab_href(&set.name, PageTab::Repair) attr:class="button">
                "Repair"
            </A>
        </div>
        <p class="done" role="status">
            {move || said.get()}
        </p>
        <Show when=move || desk.proposing().get() && tab.get() != PageTab::Repair>
            <PendingStatus status=MAKING_PLAN />
        </Show>
        <nav class="tabs" aria-label="Source set">
            {move || {
                model::tabs(repair_state.get(), tab.get())
                    .into_iter()
                    .map(|t| {
                        let current = (t == tab.get()).then_some("page");
                        let icon = (t == PageTab::Repair)
                            .then(|| repair_state.get())
                            .flatten()
                            .map(|s| view! { <StatusIcon status=s tab_stop=false /> " " });
                        view! {
                            <a href=tab_href(&name.get_value(), t) aria-current=current>
                                {icon}
                                {t.label()}
                            </a>
                        }
                    })
                    .collect_view()
            }}
        </nav>
        {move || match summary.get() {
            Some(Err(message)) if good.with(Option::is_none) => {
                view! { <Problem message /> }.into_any()
            }
            _ if good.with(Option::is_none) => view! { <Loading what="its status" /> }.into_any(),
            _ => ().into_any(),
        }}
        {move || {
            if !ready.get() {
                return None;
            }
            if tab.get() == PageTab::Repair {
                return Some(view! { <RepairTab desk named current running /> }.into_any());
            }
            Some((move || {
            let (summary, revisions) = good.get()?;
            let latest = summary.latest.clone();
            Some(match tab.get() {
                PageTab::Overview => view! { <Overview set=summary.set.clone() latest revisions /> }.into_any(),
                PageTab::Games => view! { <LatestAudit latest games=true /> }.into_any(),
                PageTab::Entries => view! { <LatestAudit latest games=false /> }.into_any(),
                PageTab::Libraries => {
                    view! { <Libraries name=name.get_value() /> }.into_any()
                }
                PageTab::Additions => view! { <Additions system=summary.set.dat.clone() /> }.into_any(),
                PageTab::History => view! { <History name=name.get_value() /> }.into_any(),
                PageTab::Kept => view! { <SourceKept name=name.get_value() /> }.into_any(),
                PageTab::Repair => ().into_any(),
            })
            }).into_any())
        }}
    }
}

/// The headline: the latest audit's game statuses and findings, each finding linking to its entries, and the form to
/// start another audit.
#[component]
fn Overview(
    set: SourceSetRecord,
    latest: Option<AuditSummary>,
    revisions: Vec<title_select_api::DatRevisionRecord>,
) -> impl IntoView {
    let name = set.name.clone();
    let headline = match &latest {
        None => view! {
            <p class="note">"Not audited yet. Start an audit to see its games and entries."</p>
        }
        .into_any(),
        Some(a) => {
            let review = needs_review_from_counts(&a.findings);
            let findings = Finding::ALL
                .into_iter()
                .filter_map(|f| {
                    let n = a.findings.get(&f).copied().filter(|n| *n > 0)?;
                    let href = format!(
                        "{}&finding={}",
                        tab_href(&name, PageTab::Entries),
                        f.as_str()
                    );
                    Some(view! {
                        <li>
                            <a href=href class="finding-badge">
                                <StatusBadge status=FindingStatus::from(f) />
                                <span class="n">{n}</span>
                            </a>
                        </li>
                    })
                })
                .collect_view();
            view! {
                <section aria-labelledby="games-title">
                    <h2 id="games-title">"Games"</h2>
                    <ul class="counts headline">
                        {GameStatus::ALL
                            .into_iter()
                            .map(|s| {
                                view! {
                                    <li class="count">
                                        <span class="n">{a.games.get(&s).copied().unwrap_or(0)}</span>
                                        <span class="what">
                                            <StatusIcon status=s tab_stop=false />
                                            " "
                                            {s.look().label}
                                        </span>
                                    </li>
                                }
                            })
                            .collect_view()}
                        <li class="count review">
                            <span class="n">{review}</span>
                            <span class="what">"Entries that need review"</span>
                        </li>
                    </ul>
                </section>
                <section aria-labelledby="findings-title">
                    <h2 id="findings-title">"Findings"</h2>
                    <p class="note">"Each opens the entries with that finding."</p>
                    <ul class="finding-badges">{findings}</ul>
                </section>
            }
            .into_any()
        }
    };
    let facts = latest.clone().map(|a| {
        view! {
            <dt>"Latest audit"</dt>
            <dd>
                <A href=format!("/audits/{}", a.id)>{format!("Audit #{}", a.id)}</A>
                {format!(", {}", when(&a.finished_at))}
            </dd>
            <dt>"Against"</dt>
            <dd>
                {format!("DAT revision {} ", a.dat_revision_version)}
                <code class="hash">{short_sha(&a.dat_revision_sha256).to_string()}</code>
            </dd>
            <dt>"Evidence"</dt>
            <dd>{audit_evidence_label(a.evidence)}</dd>
            <dt>"Coverage"</dt>
            <dd>{model::evidence_coverage(&a)}</dd>
        }
    });
    view! {
        {stale_lines(&set)}
        {headline}
        <section aria-labelledby="about-title">
            <h2 id="about-title">"About it"</h2>
            <dl class="facts">
                <dt>"DAT"</dt>
                <dd>{set.dat.clone()}</dd>
                <dt>"Root and path"</dt>
                <dd>
                    <code class="path">{format!("{}: {}", set.root, set.path)}</code>
                </dd>
                <dt>"Registered"</dt>
                <dd>{when(&set.added_at)}</dd>
                {facts}
                <dt>"Last verified against"</dt>
                <dd>
                    {set
                        .last_verified_against
                        .clone()
                        .map_or("Never".to_string(), |last| format!("{} ({})", last.version, when(&last.finished_at)))}
                </dd>
            </dl>
        </section>
        <StartAuditForm name=set.name.clone() latest revisions />
        <SetLifecycle name=set.name.clone() />
    }
}

/// Setting the source set aside, or unregistering it.
#[component]
fn SetLifecycle(name: String) -> impl IntoView {
    let base = format!("/source-sets/{}", api::segment(&name));
    let review = name.clone();
    let href = Callback::new(move |plan: i64| {
        format!("{}&plan={plan}", tab_href(&review, PageTab::Repair))
    });
    view! {
        <LifecycleSection>
            <PlanAction
                label="Set aside"
                explain="Set aside plans to keep every file of the source set aside in its own folder, renamed in \
                    place so that it takes no more space, and then to unregister it. You review the plan before \
                    anything moves; once its kept files are deleted, the folder can be registered again."
                    .to_string()
                path=format!("{base}/set-aside")
                href
            />
            <UnregisterAction
                what=format!("the source set {name}")
                paths=Paths {
                    holds: format!("{base}/holds"),
                    unregister: format!("{base}/unregistration"),
                }
                left="Its files stay where they are, no longer managed."
            />
        </LifecycleSection>
    }
}

#[component]
fn StartAuditForm(
    name: String,
    latest: Option<AuditSummary>,
    revisions: Vec<title_select_api::DatRevisionRecord>,
) -> impl IntoView {
    let (default, newer) = audit_choices(latest.as_slice(), &revisions);
    let default = match default {
        Some(r) => format!("{} ({}), the default", r.version, short_sha(&r.sha256)),
        None => "The default".to_string(),
    };
    let newer: Vec<_> = newer.into_iter().cloned().collect();
    let revision = RwSignal::new(String::new());
    let evidence = RwSignal::new(AuditEvidence::Incremental);
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let name = StoredValue::new(name);
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
        <section aria-labelledby="start-title">
            <h2 id="start-title">"Start an audit"</h2>
            <form class="form" on:submit=start>
                <label>
                    "DAT revision"
                    <select prop:value=revision on:change=move |ev| revision.set(event_target_value(&ev))>
                        <option value="">{default}</option>
                        {newer
                            .into_iter()
                            .map(|r| {
                                let label = format!("{} ({}), newer", r.version, short_sha(&r.sha256));
                                view! { <option value=r.sha256>{label}</option> }
                            })
                            .collect_view()}
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
    }
}

/// The latest audit's game statuses, or its entries narrowed by the URL's `q` and `finding`.
#[component]
fn LatestAudit(latest: Option<AuditSummary>, games: bool) -> impl IntoView {
    let query = use_query_map();
    let Some(audit) = latest else {
        return view! { <p class="note">"Not audited yet."</p> }.into_any();
    };
    if audit.pruned {
        return view! {
            <p class="note">"The latest audit is pruned: only its summary is kept, on the overview."</p>
        }
        .into_any();
    }
    let id = audit.id;
    let data = LocalResource::new(move || load(id));
    let initial = query.with_untracked(|q| EntryFilter {
        text: q.get("q").unwrap_or_default(),
        finding: q.get("finding").and_then(|f| Finding::parse(&f)),
        ..EntryFilter::default()
    });
    view! {
        <p class="note">
            {format!("From audit #{id}, {}. ", when(&audit.finished_at))}
            <A href=format!("/audits/{id}")>"Open the audit"</A>
        </p>
        {move || match data.get() {
            None => view! { <Loading what="the audit" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(loaded)) if games => view! { <GamesPanel games=loaded.games /> }.into_any(),
            Some(Ok(loaded)) => {
                view! { <EntriesPanel entries=loaded.entries initial=initial.clone() /> }.into_any()
            }
        }}
    }
    .into_any()
}

#[derive(Clone, PartialEq)]
struct FolderRow {
    folder: LibraryFolder,
    status: LibraryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderCol {
    Status,
    Path,
    Profile,
    Built,
}

impl ListRow for FolderRow {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Status,
        FolderCol::Path,
        FolderCol::Profile,
        FolderCol::Built,
    ];

    fn column(col: FolderCol) -> Column {
        match col {
            FolderCol::Status => Column::new("status", "Status", Width::Fixed(3.5)).sortable(),
            FolderCol::Path => Column::new(
                "path",
                "Library folder",
                Width::Flex {
                    min: 14.0,
                    share: 2,
                },
            )
            .sortable(),
            FolderCol::Profile => Column::new("profile", "Profile", Width::Fixed(10.0)).sortable(),
            FolderCol::Built => Column::new("built", "Built", Width::Fixed(11.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        self.folder.id.to_string()
    }

    fn title(&self) -> String {
        format!("{}: {}", self.folder.root, self.folder.path)
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}",
            self.title(),
            self.folder.profile,
            self.status.look().label
        )
    }

    fn href(&self) -> Option<String> {
        Some(format!("/libraries/{}", self.folder.id))
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        match col {
            FolderCol::Status => {
                view! { <StatusIcon status=self.status tab_stop=false /> }.into_any()
            }
            FolderCol::Path => view! { <code class="path">{self.title()}</code> }.into_any(),
            FolderCol::Profile => self.folder.profile.clone().into_any(),
            FolderCol::Built => built(&self.folder).into_any(),
        }
    }

    fn compare(&self, other: &Self, col: FolderCol) -> Ordering {
        match col {
            FolderCol::Status => self.status.rank().cmp(&other.status.rank()),
            FolderCol::Path => self.title().cmp(&other.title()),
            FolderCol::Profile => self.folder.profile.cmp(&other.folder.profile),
            FolderCol::Built => built_at(&self.folder).cmp(&built_at(&other.folder)),
        }
    }
}

fn built_at(folder: &LibraryFolder) -> Option<String> {
    folder.build.as_ref().map(|b| {
        b.finished_at
            .clone()
            .unwrap_or_else(|| b.started_at.clone())
    })
}

fn built(folder: &LibraryFolder) -> String {
    built_at(folder).map_or("Never".to_string(), |at| when(&at))
}

/// The source set's library folders, with their status.
#[component]
fn Libraries(name: String) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(name);
    let folders = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<LibraryFolder>>("/libraries").await }
    });
    let jobs = Memo::new(move |_| live_jobs(live));
    // A failed refresh keeps the rows last shown; the problem shows above them.
    let rows = last_good_rows(move || {
        let jobs = jobs.get();
        folders.get().map(|loaded| {
            loaded.map(|folders| {
                folders
                    .into_iter()
                    .filter(|f| f.source_set == name.get_value())
                    .map(|folder| FolderRow {
                        status: live_status(&folder, &jobs),
                        folder,
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        {move || match folders.get() {
            None => view! { <Loading what="its library folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="set-libraries"
            label="Library folders"
            rows
            empty="No library folder is built from this source set yet."
            placeholder="Filter by folder, profile or status"
            legend=move || {
                let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.status)));
                view! { <Legend statuses list="set-libraries" /> }
            }
            row_actions=Callback::new(|row: FolderRow| {
                vec![RowAction::go(Glyph::Library, "Open the library folder", format!("/libraries/{}", row.folder.id))]
            })
            detail=Callback::new(|row: FolderRow| {
                view! {
                    <dl class="facts">
                        <dt>"Status"</dt>
                        <dd><StatusBadge status=row.status /></dd>
                        <dt>"Profile"</dt>
                        <dd>{row.folder.profile.clone()}</dd>
                        <dt>"Built"</dt>
                        <dd>{built(&row.folder)}</dd>
                        <dt>"Files"</dt>
                        <dd>{row.folder.files}</dd>
                    </dl>
                }
                    .into_any()
            })
        />
        <p>
            <A href=new_library_href(&name.get_value())>"Make a new library folder from it"</A>
        </p>
    }
}

/// The additions set of the source set's DAT.
#[component]
fn Additions(system: String) -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let system = StoredValue::new(system);
    // Every additions set, rather than the one of the DAT: a DAT without additions has none, which isn't a problem.
    let set = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let system = system.get_value();
        async move {
            let sets = api::get::<Vec<AdditionsSet>>("/additions-sets").await?;
            Ok::<_, String>(sets.into_iter().find(|s| s.system == system))
        }
    });
    view! {
        <p class="lede">
            "Files no DAT knows that you chose to keep for this DAT, each known by its own hashes and a name you give \
            it. A repair adds one when you choose to add an unmatched file as an addition. "
            <A href="/additions">"Every additions set"</A>
        </p>
        {move || match set.get() {
            None => view! { <Loading what="its additions" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(None)) => view! { <p class="note">"This DAT has no additions yet."</p> }.into_any(),
            Some(Ok(Some(set))) => view! { <SetView set refresh /> }.into_any(),
        }}
    }
}

#[derive(Clone, PartialEq)]
struct MomentRow {
    moment: Moment,
    set: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MomentCol {
    When,
    What,
    Detail,
}

impl MomentRow {
    fn what(&self) -> String {
        match &self.moment.what {
            Happened::Audit { id, .. } => format!("Audit #{id}"),
            Happened::Repair { id, .. } => format!("Repair #{id}"),
            Happened::Ingest { repair, .. } => format!("Ingest by repair #{repair}"),
            Happened::Ingested { id, .. } => format!("Ingest plan #{id}"),
            Happened::Build { id, .. } => format!("Build #{id}"),
            Happened::FilesChanged { .. } => StaleCause::FilesChanged.look().label.to_string(),
            Happened::NewerRevision { .. } => StaleCause::NewerRevision.look().label.to_string(),
        }
    }

    /// The icon of a moment that made the source set stale.
    fn cause(&self) -> Option<StaleCause> {
        match &self.moment.what {
            Happened::FilesChanged { .. } => Some(StaleCause::FilesChanged),
            Happened::NewerRevision { .. } => Some(StaleCause::NewerRevision),
            _ => None,
        }
    }

    fn detail(&self) -> String {
        match &self.moment.what {
            Happened::Audit {
                revision,
                needs_review,
                pruned,
                ..
            } => format!(
                "Against {revision}; {needs_review} {} review{}",
                if *needs_review == 1 {
                    "entry needs"
                } else {
                    "entries need"
                },
                if *pruned { "; pruned" } else { "" }
            ),
            Happened::Repair {
                changes,
                approved,
                finished,
                ..
            } => format!(
                "{changes} {}; {}",
                if *changes == 1 { "change" } else { "changes" },
                match (approved, finished) {
                    (_, true) => "finished",
                    (true, false) => "approved",
                    (false, false) => "proposed",
                }
            ),
            Happened::Ingest { games, .. } => format!(
                "Filled {games} {} from ingest folders",
                if *games == 1 { "file" } else { "files" }
            ),
            Happened::Ingested {
                root,
                addition,
                items,
                finished,
                ..
            } => format!(
                "{} {items} {} from {root}{}",
                if *addition { "Added" } else { "Imported" },
                match (addition, *items == 1) {
                    (true, true) => "addition",
                    (true, false) => "additions",
                    (false, true) => "DAT",
                    (false, false) => "DATs",
                },
                if *finished { "" } else { "; still running" }
            ),
            Happened::Build { path, state, .. } => {
                format!("{} into {path}", build_state_label(*state))
            }
            Happened::FilesChanged {
                audit,
                changed,
                added,
                removed,
                ..
            } => format!(
                "{} since audit #{audit}",
                model::change_counts(*changed, *added, *removed)
            ),
            Happened::NewerRevision { version, .. } => {
                format!("Revision {version} of its DAT made it stale")
            }
        }
    }

    fn link(&self) -> String {
        match &self.moment.what {
            Happened::Audit { id, .. } => format!("/audits/{id}"),
            Happened::Repair { id, .. } | Happened::Ingest { repair: id, .. } => {
                format!("{}&plan={id}", tab_href(&self.set, PageTab::Repair))
            }
            Happened::Ingested { root, .. } => format!("/ingest/{}", api::segment(root)),
            Happened::Build { id, .. } => format!("/builds/{id}"),
            Happened::FilesChanged { audit, .. } => format!("/audits/{audit}"),
            Happened::NewerRevision { .. } => "/dats".to_string(),
        }
    }
}

impl ListRow for MomentRow {
    type Col = MomentCol;
    const COLUMNS: &'static [MomentCol] = &[MomentCol::When, MomentCol::What, MomentCol::Detail];

    fn column(col: MomentCol) -> Column {
        match col {
            MomentCol::When => Column::new("when", "When", Width::Fixed(12.0)).sortable(),
            MomentCol::What => Column::new("what", "What", Width::Fixed(13.0)).sortable(),
            MomentCol::Detail => Column::new(
                "detail",
                "Detail",
                Width::Flex {
                    min: 16.0,
                    share: 1,
                },
            ),
        }
    }

    fn key(&self) -> String {
        self.moment.key()
    }

    fn title(&self) -> String {
        self.what()
    }

    fn search(&self) -> String {
        format!("{} {}", self.what(), self.detail())
    }

    fn href(&self) -> Option<String> {
        Some(self.link())
    }

    fn cell(&self, col: MomentCol) -> AnyView {
        match col {
            MomentCol::When => when(&self.moment.at).into_any(),
            MomentCol::What => match self.cause() {
                Some(cause) => view! {
                    <StatusIcon status=cause tab_stop=false />
                    " "
                    {self.what()}
                }
                .into_any(),
                None => self.what().into_any(),
            },
            MomentCol::Detail => self.detail().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: MomentCol) -> Ordering {
        match col {
            MomentCol::When => self.moment.at.cmp(&other.moment.at),
            MomentCol::What => self.what().cmp(&other.what()),
            MomentCol::Detail => self.detail().cmp(&other.detail()),
        }
    }
}

async fn history(name: String) -> Result<Vec<Moment>, String> {
    let base = format!("/source-sets/{}", api::segment(&name));
    let audits: Vec<AuditSummary> = api::get(&format!("{base}/history")).await?;
    let repairs: Vec<RepairPlan> = api::get(&format!("{base}/repairs")).await?;
    let ingests: Vec<IngestPlan> = api::get(&format!("{base}/ingests")).await?;
    let walks: Vec<StatWalk> = api::get(&format!("{base}/stat-walks")).await?;
    let followed: Vec<FollowedRevision> = api::get(&format!("{base}/followed-revisions")).await?;
    let folders: Vec<LibraryFolder> = api::get("/libraries").await?;
    let mut builds = Vec::new();
    for f in folders.into_iter().filter(|f| f.source_set == name) {
        let path = format!("{}: {}", f.root, f.path);
        let made: Vec<BuildSummary> = api::get(&format!("/libraries/{}/builds", f.id)).await?;
        builds.extend(made.into_iter().map(|b| (path.clone(), b)));
    }
    Ok(model::timeline(
        &audits, &repairs, &ingests, &builds, &walks, &followed,
    ))
}

/// One timeline of the source set's audits, repairs, ingests, builds and what made it stale, newest first.
#[component]
fn History(name: String) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(name);
    let moments = LocalResource::new(move || {
        live.settled.track();
        history(name.get_value())
    });
    // A failed refresh keeps the rows last shown; the problem shows above them.
    let rows = last_good_rows(move || {
        moments.get().map(|loaded| {
            loaded.map(|moments| {
                moments
                    .into_iter()
                    .map(|moment| MomentRow {
                        moment,
                        set: name.get_value(),
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        <p class="lede">
            "Newest first. A past audit opens read-only; a pruned audit keeps only its summary."
        </p>
        {move || match moments.get() {
            None => view! { <Loading what="its history" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="set-history"
            label="History"
            rows
            empty="Nothing has happened to this source set yet."
            placeholder="Filter by audit, repair, ingest, build, change or detail"
            row_actions=Callback::new(|row: MomentRow| vec![RowAction::go(Glyph::Eye, "Open", row.link())])
            detail=Callback::new(|row: MomentRow| {
                view! {
                    <dl class="facts">
                        <dt>"When"</dt>
                        <dd>{when(&row.moment.at)}</dd>
                        <dt>"What"</dt>
                        <dd>{row.what()}</dd>
                        <dt>"Detail"</dt>
                        <dd>{row.detail()}</dd>
                    </dl>
                }
                    .into_any()
            })
        />
    }
}
