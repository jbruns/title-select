use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use leptos::html::Div;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use leptos_struct_table::ColumnSort;
use title_select_api::build::LibraryFolder;
use title_select_api::{
    ALGORITHMS, AuditEvidence, AuditSummary, Finding, GameStatus, HeaderRule, Job, Placements,
    StartAudit,
};
use wasm_bindgen::JsCast;

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{
    AuditEntries, AuditGames, Derived, EntryColumn, EntryFilter, GameColumn, GameFilter, Sort,
    StatusFilter, absent_roms, audit_evidence_label, entry_name, evidence_text, finding_label,
    game_status_label, game_status_text, games_of_status_label, header_rule_text, header_text,
    needs_review_from_counts, parse_ndjson, payload_read_text, warning_counts, warning_text, when,
};
use crate::parts::pending::Pending;
use crate::parts::status::{GameFlag, StatusBadge, StatusIcon};
use crate::placement_model::{
    FlagsNow, READING_FLAGS, Stamped, flag_detail, flags_unknown, folder_name, game_placements,
    stamped_flags,
};
use crate::table::{DataTable, Grid, RowSource, Rows, sort_of};

/// An audit's entries and game statuses, as its panels show them.
#[derive(Clone)]
pub struct Loaded {
    pub entries: Arc<AuditEntries>,
    pub games: Arc<AuditGames>,
}

/// Fetches the entries and game statuses of the audit `id`.
pub async fn load(id: i64) -> Result<Loaded, String> {
    let text = api::text(&format!("/audits/{id}/entries.ndjson")).await?;
    let rows = parse_ndjson(&text)?;
    let games = api::games(id).await?;
    let placed = game_placements(&rows);
    let entries = AuditEntries::new(rows, &games);
    Ok(Loaded {
        entries: Arc::new(entries),
        games: Arc::new(AuditGames::new(games).with_placements(placed)),
    })
}

/// A source set's placements and the library folders, read for its audits' tables, stamped with the job-settled
/// count the read started at.
pub type PlacementsNow = LocalResource<Stamped<(Placements, Vec<LibraryFolder>)>>;

/// Where source set `source_set`'s games are now, for its audits' tables: its placements, with the review flags builds
/// raised, and the library folders, by name. Read again whenever a job settles, since a build, audit or repair may
/// change them. Without the library folders a placement reads by the folder's number; without the placements the
/// tables say they can't tell which games need review.
pub fn placements_now(source_set: String) -> PlacementsNow {
    let live = use_live();
    LocalResource::new(move || {
        let at = live.settled.get();
        let path = format!("/source-sets/{}/placements", api::segment(&source_set));
        async move {
            let read = async {
                let placements = api::get::<Placements>(&path).await?;
                let folders = api::get::<Vec<LibraryFolder>>("/libraries")
                    .await
                    .unwrap_or_default();
                Ok((placements, folders))
            };
            (at, read.await)
        }
    })
}

/// The review flags among `now`, as a table of game statuses marks them: the last good read, and whether a newer
/// read is under way or failed.
pub fn flags_now(now: Option<PlacementsNow>) -> Memo<FlagsNow> {
    let Some(now) = now else {
        // A table with no source set to ask, such as the bench's, has no game a build flagged.
        return Memo::new(|_| FlagsNow::none());
    };
    let live = use_live();
    Memo::new(move |prev: Option<&FlagsNow>| {
        let read = now.get().map(|(at, read)| {
            stamped_flags(at, read.map(|(placements, _)| placements.review_flags))
        });
        prev.cloned()
            .unwrap_or_default()
            .next(read.as_ref(), live.settled.get())
    })
}

/// Says when a table can't be sure which games need review: while it reads them, the first time or again after a
/// job settles, or that it couldn't.
pub fn flags_notice(flags: Memo<FlagsNow>) -> impl IntoView {
    view! {
        <Pending
            pending=Signal::derive(move || flags.with(|f| f.reading))
            status=READING_FLAGS
            problem=Signal::derive(move || flags.with(|f| f.problem.as_deref().map(flags_unknown)))
        >
            ""
        </Pending>
    }
}

pub fn focus(id: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        let _ = el.unchecked_into::<web_sys::HtmlElement>().focus();
    }
}

/// One Audit: its summary counts, Entries and Game Statuses.
#[component]
pub fn AuditScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let summary = LocalResource::new(move || {
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<AuditSummary>(&format!("/audits/{id}")).await,
                None => Err("That is not an audit number.".into()),
            }
        }
    });
    move || match summary.get() {
        None => view! { <Loading what="the audit" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Audit"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(audit)) => view! { <AuditView audit /> }.into_any(),
    }
}

#[component]
fn AuditView(audit: AuditSummary) -> impl IntoView {
    let id = audit.id;
    let pruned = audit.pruned;
    let set = StoredValue::new(audit.source_set.clone());
    let data =
        LocalResource::new(move || async move { if pruned { None } else { Some(load(id).await) } });
    let counts = Signal::derive(move || {
        data.get()
            .flatten()
            .and_then(|d| d.ok())
            .map(|d| d.entries.counts())
    });
    let reaudit = ReAudit::new(
        audit.source_set.clone(),
        audit.dat_revision_sha256.clone(),
        RwSignal::new(None),
    );
    let tab = RwSignal::new(Tab::Entries);
    let set_href = format!("/source-sets/{}", api::segment(&audit.source_set));
    let repair_href = format!("{set_href}?tab=repair");
    let findings = audit.findings.clone();
    let games = audit.games.clone();
    let needs_review = needs_review_from_counts(&findings);
    let rule = audit.header_rule.as_ref().and_then(|r| r.rule);
    let header_rule = audit.header_rule.as_ref().map(|named| {
        view! {
            <dt>"Header rule"</dt>
            <dd>{header_rule_text(named)}</dd>
        }
    });
    let warnings = warning_counts(&audit).map(|said| {
        view! {
            <dt>"Warnings"</dt>
            <dd>{said}</dd>
        }
    });

    view! {
        <h1>{format!("Audit #{id}")}</h1>
        <dl class="facts">
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{audit.source_set.clone()}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <super::dats::RevisionName
                    sha256=audit.dat_revision_sha256.clone()
                    version=audit.dat_revision_version.clone()
                />
            </dd>
            <dt>"Started"</dt>
            <dd>{when(&audit.started_at)}</dd>
            <dt>"Finished"</dt>
            <dd>{when(&audit.finished_at)}</dd>
            <dt>"Evidence"</dt>
            <dd>{audit_evidence_label(audit.evidence)}</dd>
            {header_rule}
            {warnings}
        </dl>
        <div class="actions">
            {(!pruned)
                .then(|| {
                    view! {
                        <a
                            class="button"
                            href=api::url(&format!("/audits/{id}/report"))
                            download=format!("audit-{id}.json")
                        >
                            "Export JSON report"
                        </a>
                    }
                })}
            {reaudit.button()}
            <A href=repair_href attr:class="button">"Repair the source set"</A>
        </div>
        {reaudit.outcome()}

        <section aria-labelledby="summary-title">
            <h2 id="summary-title">"Summary"</h2>
            <ul class="counts">
                <li class="count verified">
                    <span class="n">
                        {move || counts.get().map_or("—".to_string(), |c| c.verified.to_string())}
                    </span>
                    <span class="what">"Verified"</span>
                </li>
                <li class="count review">
                    <span class="n">{needs_review}</span>
                    <span class="what">"Needs review"</span>
                </li>
                {Finding::ALL
                    .iter()
                    .map(|f| {
                        view! {
                            <li class="count">
                                <span class="n">{findings.get(f).copied().unwrap_or(0)}</span>
                                <span class="what">{finding_label(*f)}</span>
                            </li>
                        }
                    })
                    .collect_view()}
                {GameStatus::ALL
                    .iter()
                    .map(|s| {
                        view! {
                            <li class="count game">
                                <span class="n">{games.get(s).copied().unwrap_or(0)}</span>
                                <span class="what">{games_of_status_label(*s)}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
            {pruned
                .then(|| {
                    view! {
                        <p class="note">
                            "This audit is pruned: only its summary is kept, so its entries, game statuses and verified count are gone."
                        </p>
                    }
                })}
        </section>

        {move || match data.get() {
            None => view! { <Loading what="the entries" /> }.into_any(),
            Some(None) => ().into_any(),
            Some(Some(Err(message))) => view! { <Problem message /> }.into_any(),
            Some(Some(Ok(loaded))) => {
                view! {
                    <div class="tabs" role="tablist" aria-label="Audit details">
                        <TabButton tab which=Tab::Entries label="Entries" />
                        <TabButton tab which=Tab::Games label="Game statuses" />
                    </div>
                    <div
                        role="tabpanel"
                        id="panel-entries"
                        aria-labelledby="tab-entries"
                        hidden=move || tab.get() != Tab::Entries
                    >
                        <EntriesPanel entries=loaded.entries.clone() source_set=set.get_value() />
                    </div>
                    <div
                        role="tabpanel"
                        id="panel-games"
                        aria-labelledby="tab-games"
                        hidden=move || tab.get() != Tab::Games
                    >
                        <GamesPanel games=loaded.games.clone() rule source_set=set.get_value() />
                    </div>
                }
                    .into_any()
            }
        }}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Entries,
    Games,
}

impl Tab {
    fn key(self) -> &'static str {
        match self {
            Tab::Entries => "entries",
            Tab::Games => "games",
        }
    }
}

#[component]
fn TabButton(tab: RwSignal<Tab>, which: Tab, label: &'static str) -> impl IntoView {
    let selected = move || tab.get() == which;
    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if matches!(ev.key().as_str(), "ArrowLeft" | "ArrowRight") {
            ev.prevent_default();
            let other = if which == Tab::Entries {
                Tab::Games
            } else {
                Tab::Entries
            };
            tab.set(other);
            focus(&format!("tab-{}", other.key()));
        }
    };
    view! {
        <button
            type="button"
            role="tab"
            id=format!("tab-{}", which.key())
            aria-controls=format!("panel-{}", which.key())
            aria-selected=move || selected().to_string()
            tabindex=move || if selected() { "0" } else { "-1" }
            on:click=move |_| tab.set(which)
            on:keydown=on_keydown
        >
            {label}
        </button>
    }
}

/// Starts an Audit of a Source Set against one DAT Revision, and says how that went.
#[derive(Clone, Copy)]
pub struct ReAudit {
    source_set: StoredValue<String>,
    revision: StoredValue<String>,
    outcome: RwSignal<Option<Result<Job, String>>>,
}

impl ReAudit {
    /// `outcome` receives the queued Job or the Service's refusal.
    pub fn new(
        source_set: String,
        revision: String,
        outcome: RwSignal<Option<Result<Job, String>>>,
    ) -> ReAudit {
        ReAudit {
            source_set: StoredValue::new(source_set),
            revision: StoredValue::new(revision),
            outcome,
        }
    }

    pub fn start(self) {
        let request = StartAudit {
            source_set: self.source_set.get_value(),
            revision: Some(self.revision.get_value()),
            evidence: AuditEvidence::Incremental,
        };
        leptos::task::spawn_local(async move {
            let job = api::post::<_, Job>("/audits", &request).await;
            self.outcome.set(Some(job));
        });
    }

    pub fn button(self) -> impl IntoView {
        view! {
            <button type="button" on:click=move |_| self.start()>
                "Re-audit against this revision"
            </button>
        }
    }

    pub fn outcome(self) -> impl IntoView {
        move || {
            self.outcome.get().map(|outcome| match outcome {
                Ok(job) => queued(&job).into_any(),
                Err(message) => view! { <Problem message /> }.into_any(),
            })
        }
    }
}

/// Says that a Job was queued, with the way to follow it.
pub fn queued(job: &Job) -> impl IntoView + use<> {
    view! {
        <p class="done" role="status">
            {format!("Job #{} is queued to audit {} against ", job.id, job.source_set)}
            <super::dats::RevisionName sha256=job.dat_revision.clone() />
            ". "
            <A href="/activity">"Follow it in Activity"</A>
        </p>
    }
}

/// An Entry in an Audit's Entries table, by its index.
#[derive(Clone)]
pub struct EntryItem {
    entries: Arc<AuditEntries>,
    flags: FlagsNow,
    i: u32,
}

struct EntryWindow {
    entries: Arc<AuditEntries>,
    flags: FlagsNow,
    shown: Arc<Vec<u32>>,
}

impl RowSource<EntryItem> for EntryWindow {
    fn len(&self) -> usize {
        self.shown.len()
    }

    fn rows(&self, range: Range<usize>) -> Vec<EntryItem> {
        self.shown[range]
            .iter()
            .map(|&i| EntryItem {
                entries: self.entries.clone(),
                flags: self.flags.clone(),
                i,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryCol {
    Name,
    Finding,
    State,
    Game,
    GameStatus,
    Evidence,
    Reason,
}

impl EntryCol {
    fn sort_key(self) -> Option<EntryColumn> {
        match self {
            EntryCol::Name => Some(EntryColumn::Name),
            EntryCol::Finding => Some(EntryColumn::Finding),
            EntryCol::GameStatus => Some(EntryColumn::GameStatus),
            EntryCol::Evidence => Some(EntryColumn::Evidence),
            EntryCol::State | EntryCol::Game | EntryCol::Reason => None,
        }
    }
}

fn state_badge(state: Option<Derived>) -> AnyView {
    match state {
        Some(Derived::Verified) => {
            view! { <span class="state verified">"Verified"</span> }.into_any()
        }
        Some(Derived::NeedsReview) => {
            view! { <span class="state review">"Needs review"</span> }.into_any()
        }
        None => ().into_any(),
    }
}

impl Grid for EntryItem {
    type Col = EntryCol;
    const COLUMNS: &'static [EntryCol] = &[
        EntryCol::Name,
        EntryCol::Finding,
        EntryCol::State,
        EntryCol::Game,
        EntryCol::GameStatus,
        EntryCol::Evidence,
        EntryCol::Reason,
    ];

    fn header(col: EntryCol) -> &'static str {
        match col {
            EntryCol::Name => "Entry",
            EntryCol::Finding => "Finding",
            EntryCol::State => "State",
            EntryCol::Game => "Game",
            EntryCol::GameStatus => "Game status",
            EntryCol::Evidence => "Evidence",
            EntryCol::Reason => "Reason",
        }
    }

    fn class(col: EntryCol) -> &'static str {
        match col {
            EntryCol::Name => "c-wide data",
            EntryCol::Finding => "c-finding",
            EntryCol::State => "c-state",
            EntryCol::Game => "c-wide",
            EntryCol::GameStatus => "c-status",
            EntryCol::Evidence => "c-evidence data",
            EntryCol::Reason => "c-reason",
        }
    }

    fn sortable(col: EntryCol) -> bool {
        col.sort_key().is_some()
    }

    fn cell(&self, col: EntryCol) -> AnyView {
        let i = self.i as usize;
        let entry = &self.entries.rows[i].entry;
        match col {
            EntryCol::Name => entry_name(entry).into_any(),
            EntryCol::Finding => finding_label(entry.finding).into_any(),
            EntryCol::State => state_badge(self.entries.derived(i)),
            EntryCol::Game => {
                let mut games: Vec<&str> = entry.roms.iter().map(|r| r.game.as_str()).collect();
                games.dedup();
                games.join(", ").into_any()
            }
            EntryCol::GameStatus => {
                let status = self
                    .entries
                    .primary_status(i)
                    .map(game_status_label)
                    .unwrap_or("");
                let flagged = self.flags.any(entry.roms.iter().map(|r| r.game.as_str()));
                let flag = flagged.then(
                    || view! { " " <StatusIcon status=GameFlag::NeedsReview tab_stop=false /> },
                );
                view! { {status} {flag} }.into_any()
            }
            EntryCol::Evidence => evidence_text(entry).into_any(),
            EntryCol::Reason => entry.reason.clone().unwrap_or_default().into_any(),
        }
    }
}

/// An Audit's Entries with their filters, table and the selected Entry's details. Filtering and sorting run in the
/// browser on every keystroke.
#[component]
pub fn EntriesPanel(
    entries: Arc<AuditEntries>,
    /// What the entries are narrowed to at first, such as a finding a badge links to.
    #[prop(optional)]
    initial: EntryFilter,
    /// Its source set, to mark the games a build flagged as needing review; none where there's none to ask.
    #[prop(optional)]
    source_set: Option<String>,
) -> impl IntoView {
    let total = entries.len();
    let flags = flags_now(source_set.map(placements_now));
    let sorting = RwSignal::new(VecDeque::<(EntryCol, ColumnSort)>::new());
    let filter = RwSignal::new(initial.clone());
    let first = initial;
    let selected = RwSignal::new(None::<usize>);
    let panel = NodeRef::<Div>::new();

    let sorted = Memo::new({
        let entries = entries.clone();
        move |_| {
            let sort = sort_of(&sorting.read()).and_then(|s| {
                s.column.sort_key().map(|column| Sort {
                    column,
                    descending: s.descending,
                })
            });
            Arc::new(entries.sorted(sort))
        }
    });
    let shown = Memo::new({
        let entries = entries.clone();
        move |_| filter.with(|f| sorted.with(|order| Arc::new(entries.filtered(order, f))))
    });
    let rows = Signal::derive({
        let entries = entries.clone();
        move || {
            Arc::new(EntryWindow {
                entries: entries.clone(),
                flags: flags.get(),
                shown: shown.get(),
            }) as Rows<EntryItem>
        }
    });

    let refilter = move |change: Box<dyn FnOnce(&mut EntryFilter)>| {
        filter.update(|f| change(f));
        selected.set(None);
        if let Some(scroll) = panel
            .get_untracked()
            .and_then(|p| p.query_selector(".table-scroll").ok().flatten())
        {
            scroll.set_scroll_top(0);
        }
    };

    let detail = move || {
        let shown = shown.get();
        let i = *shown.get(selected.get()?)? as usize;
        Some(entry_detail(&entries, i))
    };

    view! {
        <div class="panel" node_ref=panel>
            <form class="filters" on:submit=|ev| ev.prevent_default()>
                <label>
                    "Filter entries"
                    <input
                        type="search"
                        class="filter-text"
                        placeholder="Entry or game name"
                        autocomplete="off"
                        prop:value=first.text.clone()
                        on:input=move |ev| {
                            let text = event_target_value(&ev);
                            refilter(Box::new(move |f| f.text = text));
                        }
                    />
                </label>
                <label>
                    "Finding"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(Box::new(move |f| f.finding = Finding::parse(&v)));
                    }>
                        <option value="">"Any"</option>
                        {Finding::ALL
                            .iter()
                            .map(|f| {
                                let chosen = first.finding == Some(*f);
                                view! { <option value=f.as_str() selected=chosen>{finding_label(*f)}</option> }
                            })
                            .collect_view()}
                    </select>
                </label>
                <label>
                    "State"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(
                            Box::new(move |f| {
                                f.state = match v.as_str() {
                                    "verified" => Some(Derived::Verified),
                                    "needs-review" => Some(Derived::NeedsReview),
                                    _ => None,
                                };
                            }),
                        );
                    }>
                        <option value="">"Any"</option>
                        <option value="verified">"Verified"</option>
                        <option value="needs-review">"Needs review"</option>
                    </select>
                </label>
                <label>
                    "Game status"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(
                            Box::new(move |f| {
                                f.game_status = match v.as_str() {
                                    "none" => Some(StatusFilter::None),
                                    other => GameStatus::parse(other).map(StatusFilter::Is),
                                };
                            }),
                        );
                    }>
                        <option value="">"Any"</option>
                        {GameStatus::ALL
                            .iter()
                            .map(|s| view! { <option value=s.as_str()>{game_status_label(*s)}</option> })
                            .collect_view()}
                        <option value="none">"No game"</option>
                    </select>
                </label>
                <label>
                    "Algorithm"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(Box::new(move |f| f.algorithm = (!v.is_empty()).then_some(v)));
                    }>
                        <option value="">"Any"</option>
                        {ALGORITHMS.iter().map(|a| view! { <option value=*a>{*a}</option> }).collect_view()}
                    </select>
                </label>
            </form>
            <p
                class="shown"
                aria-live="polite"
                data-query=move || filter.with(|f| f.text.clone())
                data-shown=move || shown.with(|s| s.len())
            >
                {move || format!("{} of {total} entries", shown.with(|s| s.len()))}
            </p>
            {flags_notice(flags)}
            <DataTable
                id="entries"
                label="Entries".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_| focus("entry-detail"))
                empty="No entries match these filters.".to_string()
            />
            <section class="detail" id="entry-detail" tabindex="-1" aria-label="Selected entry">
                {move || {
                    detail()
                        .unwrap_or_else(|| {
                            view! {
                                <p class="note">
                                    "Select an entry with the arrow keys or a click to see its details."
                                </p>
                            }
                                .into_any()
                        })
                }}
            </section>
        </div>
    }
}

fn entry_detail(entries: &AuditEntries, i: usize) -> AnyView {
    let entry = entries.rows[i].entry.clone();
    let evidence = evidence_text(&entry);
    let read = payload_read_text(&entry);
    let header = header_text(&entry);
    let roms = entry
        .roms
        .iter()
        .map(|r| view! { <li>{r.game.clone()} " › " <code>{r.name.clone()}</code></li> })
        .collect_view();
    view! {
        <h3>"Entry"</h3>
        <dl class="facts">
            <dt>"Path"</dt>
            <dd>
                <code class="path">{entry.path.clone()}</code>
            </dd>
            {entry
                .member
                .clone()
                .map(|m| {
                    view! {
                        <dt>"Member"</dt>
                        <dd>
                            <code class="path">{m}</code>
                        </dd>
                    }
                })}
            <dt>"Finding"</dt>
            <dd>{finding_label(entry.finding)} " " {state_badge(entries.derived(i))}</dd>
            <dt>"Evidence"</dt>
            <dd>{if evidence.is_empty() { "None".to_string() } else { evidence }}</dd>
            {header
                .map(|header| {
                    view! {
                        <dt>"Header"</dt>
                        <dd>{header}</dd>
                    }
                })}
            {read
                .map(|read| {
                    view! {
                        <dt>"Payload last read"</dt>
                        <dd>{read}</dd>
                    }
                })}
            {entry
                .named_rom
                .clone()
                .map(|r| {
                    view! {
                        <dt>"Its name gives"</dt>
                        <dd>{r.game} " › " <code>{r.name}</code></dd>
                    }
                })}
            {(!entry.roms.is_empty())
                .then(|| {
                    view! {
                        <dt>"Its payload matches"</dt>
                        <dd>
                            <ul>{roms}</ul>
                        </dd>
                    }
                })}
            {entry
                .reason
                .clone()
                .map(|r| {
                    view! {
                        <dt>"Reason"</dt>
                        <dd>{r}</dd>
                    }
                })}
        </dl>
    }
    .into_any()
}

/// A Game Status in an Audit's Game Statuses table.
#[derive(Clone)]
pub struct GameItem {
    games: Arc<AuditGames>,
    flags: FlagsNow,
    i: u32,
}

struct GameWindow {
    games: Arc<AuditGames>,
    flags: FlagsNow,
    shown: Arc<Vec<u32>>,
}

impl RowSource<GameItem> for GameWindow {
    fn len(&self) -> usize {
        self.shown.len()
    }

    fn rows(&self, range: Range<usize>) -> Vec<GameItem> {
        self.shown[range]
            .iter()
            .map(|&i| GameItem {
                games: self.games.clone(),
                flags: self.flags.clone(),
                i,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GameCol {
    Name,
    Status,
    Placement,
    Absent,
}

impl Grid for GameItem {
    type Col = GameCol;
    const COLUMNS: &'static [GameCol] = &[
        GameCol::Name,
        GameCol::Status,
        GameCol::Placement,
        GameCol::Absent,
    ];

    fn header(col: GameCol) -> &'static str {
        match col {
            GameCol::Name => "Game",
            GameCol::Status => "Game status",
            GameCol::Placement => "Placement",
            GameCol::Absent => "Absent ROMs",
        }
    }

    fn class(col: GameCol) -> &'static str {
        match col {
            GameCol::Name => "c-wide",
            GameCol::Status => "c-status",
            GameCol::Placement => "c-status",
            GameCol::Absent => "c-wide data",
        }
    }

    fn sortable(_col: GameCol) -> bool {
        true
    }

    fn cell(&self, col: GameCol) -> AnyView {
        let game = &self.games.rows[self.i as usize].game;
        match col {
            GameCol::Name => game.name.clone().into_any(),
            GameCol::Status => {
                let flag = self.flags.get(&game.name).is_some().then(
                    || view! { " " <StatusIcon status=GameFlag::NeedsReview tab_stop=false /> },
                );
                view! { {game_status_text(game)} {flag} }.into_any()
            }
            GameCol::Placement => self
                .games
                .placement(self.i as usize)
                .map(|p| view! { <StatusIcon status=p tab_stop=false /> })
                .into_any(),
            GameCol::Absent => match game.missing_roms.len() {
                0 => ().into_any(),
                n => format!("{n}: {}", game.missing_roms.join(", ")).into_any(),
            },
        }
    }
}

/// Why a past audit shows a review flag it didn't raise.
const FLAGGED_NOW: &str = "This flag is the game's now, not part of this audit: every audit of the source set \
shows it until a full audit verifies the file or the game returns.";

#[component]
pub fn GamesPanel(
    games: Arc<AuditGames>,
    rule: Option<HeaderRule>,
    source_set: String,
) -> impl IntoView {
    let total = games.len();
    let now = placements_now(source_set);
    let flags = flags_now(Some(now));
    let folders = Memo::new(move |_| {
        now.get()
            .and_then(|(_, read)| read.ok())
            .map(|(_, f)| f)
            .unwrap_or_default()
    });
    let sorting = RwSignal::new(VecDeque::<(GameCol, ColumnSort)>::new());
    let filter = RwSignal::new(GameFilter::default());
    let selected = RwSignal::new(None::<usize>);
    let sorted = Memo::new({
        let games = games.clone();
        move |_| {
            let sort = sort_of(&sorting.read()).map(|s| Sort {
                column: match s.column {
                    GameCol::Name => GameColumn::Name,
                    GameCol::Status => GameColumn::Status,
                    GameCol::Placement => GameColumn::Placement,
                    GameCol::Absent => GameColumn::Absent,
                },
                descending: s.descending,
            });
            Arc::new(games.sorted(sort))
        }
    });
    let shown = Memo::new({
        let games = games.clone();
        move |_| filter.with(|f| sorted.with(|order| Arc::new(games.filtered(order, f))))
    });
    let rows = Signal::derive({
        let games = games.clone();
        move || {
            Arc::new(GameWindow {
                games: games.clone(),
                flags: flags.get(),
                shown: shown.get(),
            }) as Rows<GameItem>
        }
    });
    let detail = move || {
        let i = *shown.get().get(selected.get()?)? as usize;
        let game = games.rows[i].game.clone();
        let placement = games.placement(i).map(|p| {
            let place = match games.placed_in(i) {
                Some(id) => view! {
                    "Placed in "
                    <A href=format!("/libraries/{id}")>{move || folders.with(|f| folder_name(f, id))}</A>
                    " under single copy: its file there was the game's only copy."
                }
                .into_any(),
                None => "In your source set's own folder.".into_any(),
            };
            view! {
                <h4>"Placement"</h4>
                <p><StatusBadge status=p /></p>
                <p>{place}</p>
            }
        });
        let flag = flags.with(|f| f.get(&game.name).cloned()).map(|flag| {
            view! {
                <h4>"Flagged now"</h4>
                <p><StatusBadge status=GameFlag::NeedsReview /></p>
                <p>{flag_detail(&flag)}</p>
                <p class="note">{FLAGGED_NOW}</p>
            }
        });
        let absent = (!game.missing_roms.is_empty()).then(|| {
            let elsewhere = (!game.available_elsewhere.is_empty()).then(|| {
                view! {
                    <p class="note">"A ROM available in another game is inside that game's ZIP or folder, so it doesn't count towards this game. A repair can fill this game from it unless the file is named for the other game."</p>
                }
            });
            view! {
                <h4>"Absent ROMs"</h4>
                {elsewhere}
                <ul class="data">{absent_roms(&game).into_iter().map(|r| view! { <li>{r}</li> }).collect_view()}</ul>
            }
        });
        let claimed = (!game.claimed_roms.is_empty()).then(|| {
            view! {
                <h4>"Present only on claim evidence"</h4>
                <p class="note">"A quick inventory read these ROMs' names, sizes and CRC32s from a ZIP directory without reading their payloads. Audit the source set incrementally or in full to verify them; builds never use them."</p>
                <ul class="data">{game.claimed_roms.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
            }
        });
        let warnings = (!game.warnings.is_empty()).then(|| {
            view! {
                <h4>"Warnings"</h4>
                <ul class="data">
                    {game
                        .warnings
                        .iter()
                        .map(|w| view! { <li>{format!("It {}", warning_text(*w, rule))}</li> })
                        .collect_view()}
                </ul>
            }
        });
        Some(view! {
            <h3>{game.name.clone()}</h3>
            <p>{format!("Game status: {}", game_status_text(&game))}</p>
            {flag}
            {placement}
            {warnings}
            {absent}
            {claimed}
        })
    };

    view! {
        <div class="panel">
            <form class="filters" on:submit=|ev| ev.prevent_default()>
                <label>
                    "Filter game statuses"
                    <input
                        type="search"
                        placeholder="Game or absent ROM name"
                        autocomplete="off"
                        on:input=move |ev| {
                            let text = event_target_value(&ev);
                            filter.update(|f| f.text = text);
                            selected.set(None);
                        }
                    />
                </label>
                <label>
                    "Game status"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        filter.update(|f| f.status = GameStatus::parse(&v));
                        selected.set(None);
                    }>
                        <option value="">"Any"</option>
                        {GameStatus::ALL
                            .iter()
                            .map(|s| view! { <option value=s.as_str()>{game_status_label(*s)}</option> })
                            .collect_view()}
                    </select>
                </label>
            </form>
            <p class="shown" aria-live="polite">
                {move || format!("{} of {total} game statuses", shown.with(|s| s.len()))}
            </p>
            {flags_notice(flags)}
            <DataTable
                id="games"
                label="Game statuses".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_| focus("game-detail"))
                empty="No game statuses match these filters.".to_string()
            />
            <section class="detail" id="game-detail" tabindex="-1" aria-label="Selected game status">
                {move || match detail() {
                    Some(view) => view.into_any(),
                    None => view! { <p class="note">"Select a game to see its absent ROMs."</p> }.into_any(),
                }}
            </section>
        </div>
    }
}
