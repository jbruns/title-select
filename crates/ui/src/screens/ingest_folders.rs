use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::NavigateOptions;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_navigate, use_params_map, use_query_map};
use title_select_api::Job;
use title_select_api::additions::NewAddition;
use title_select_api::build::RETENTION;
use title_select_api::ingest::{
    Assessment, DismissIngestItems, IngestFolder, IngestItem, IngestKeptFile, IngestPlan,
    PlanIngest, Proposal, RegisterIngestFolder,
};

use crate::api;
use crate::app::{Loading, Problem, use_ingest_changed};
use crate::events::use_live;
use crate::ingest_model::{self, Choices, Count, Decision};
use crate::model::{earlier, short_sha, when};
use crate::parts::icon::{Glyph, LabelledIcon, Svg};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, Keep, List, ListRow, RowAction, Selected, ToolAction, Width, last_good_rows,
};
use crate::parts::pending::ActionPending;
use crate::parts::status::{IngestStatus, Status, StatusBadge, StatusIcon};
use crate::parts::url_state::query_string;
use crate::screens::additions::{AdditionFields, new_addition};
use crate::screens::ingest_plan::IngestPlanReview;
use crate::shell::waiting_on_decision;

fn folder_href(root: &str) -> String {
    format!("/ingest/{}", api::segment(root))
}

/// What is proposed for an assessed item.
fn proposal_label(proposal: &Proposal) -> String {
    match proposal {
        Proposal::SourceSet {
            source_set,
            game,
            need,
        } => format!("{}: {game} in {source_set}", capitalised(need.label())),
        Proposal::Addition {
            system: Some(system),
        } => {
            format!("A candidate addition for {system}")
        }
        Proposal::Addition { system: None } => {
            "A candidate addition; no registered system uses its extension".into()
        }
        Proposal::DatImport => "A DAT you may import".into(),
    }
}

fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The Evidence an assessment found: each match with its ROMs and the members they came from, or the DAT found.
fn evidence(a: &Assessment) -> AnyView {
    let dat = a.dat.as_ref().map(|d| {
        let imported = if d.imported {
            ", already imported".to_string()
        } else if d.providers.is_empty() {
            ", a DAT no source set uses yet".to_string()
        } else {
            format!(", a newer revision of {}", d.providers.join(", "))
        };
        view! { <li>{format!("DAT {} version {}{imported}", d.name, d.version)}</li> }
    });
    let matches = a
        .matches
        .iter()
        .map(|m| {
            let roms = m
                .roms
                .iter()
                .map(|r| match &r.member {
                    Some(member) => format!("{} (member {member})", r.rom),
                    None => r.rom.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut notes = vec![m.need.label().to_string()];
            if !m.complete {
                notes.push("not every ROM of the game".into());
            }
            if m.extra {
                notes.push("with other members".into());
            }
            let text = format!(
                "{} in {} (DAT revision {}): {roms}; {}",
                m.game,
                m.source_set,
                short_sha(&m.dat_revision),
                notes.join(", ")
            );
            view! { <li>{text}</li> }
        })
        .collect_view();
    let sha = a
        .sha256
        .as_ref()
        .map(|sha| view! { <li>"SHA-256 " <code class="hash">{sha.clone()}</code></li> });
    view! { <ul class="plain">{sha} {dat} {matches}</ul> }.into_any()
}

/// How often the folder is scanned on its own, or none when only you start scans. The scheduler keeps its own time,
/// so this names the cadence rather than guess at the next scan.
fn scan_cadence(folder: &IngestFolder) -> Option<String> {
    folder.scan_every_minutes.map(|m| match m {
        1 => "scanned every minute".to_string(),
        m => format!("scanned every {m} minutes"),
    })
}

/// An Ingest Folder in the list of them.
#[derive(Debug, Clone, PartialEq)]
struct FolderRow(IngestFolder);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderCol {
    Folder,
    Path,
    Waiting,
    Scanned,
}

impl FolderRow {
    fn waiting(&self) -> u64 {
        waiting_on_decision(std::slice::from_ref(&self.0))
    }
}

impl ListRow for FolderRow {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Folder,
        FolderCol::Path,
        FolderCol::Waiting,
        FolderCol::Scanned,
    ];

    fn column(col: FolderCol) -> Column {
        match col {
            FolderCol::Folder => Column::new(
                "folder",
                "Folder",
                Width::Flex {
                    min: 10.0,
                    share: 1,
                },
            )
            .sortable(),
            FolderCol::Path => Column::new(
                "path",
                "Path",
                Width::Flex {
                    min: 14.0,
                    share: 2,
                },
            )
            .sortable(),
            FolderCol::Waiting => Column::new("waiting", "Waiting", Width::Fixed(6.5))
                .sortable()
                .numeric()
                .hint("How many items wait on your decision"),
            FolderCol::Scanned => {
                Column::new("scanned", "Last scan", Width::Fixed(13.0)).sortable()
            }
        }
    }

    fn key(&self) -> String {
        self.0.root.clone()
    }

    fn title(&self) -> String {
        format!("Ingest folder {}", self.0.root)
    }

    fn search(&self) -> String {
        format!("{} {}", self.0.root, self.0.path)
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        let f = &self.0;
        match col {
            FolderCol::Folder => view! { <Clip text=f.root.clone() /> }.into_any(),
            FolderCol::Path => view! { <Clip text=f.path.clone() /> }.into_any(),
            FolderCol::Waiting => match f.id {
                Some(_) => self.waiting().to_string().into_any(),
                None => "–".into_any(),
            },
            FolderCol::Scanned => {
                let text = match (&f.id, &f.last_scan_at) {
                    (None, _) => "Not registered".to_string(),
                    (Some(_), Some(at)) => when(at),
                    (Some(_), None) => "Never".to_string(),
                };
                view! { <Clip text /> }.into_any()
            }
        }
    }

    fn compare(&self, other: &Self, col: FolderCol) -> Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            FolderCol::Folder => a.root.cmp(&b.root),
            FolderCol::Path => a.path.cmp(&b.path),
            FolderCol::Waiting => self.waiting().cmp(&other.waiting()),
            FolderCol::Scanned => a.last_scan_at.cmp(&b.last_scan_at),
        }
    }

    fn href(&self) -> Option<String> {
        self.0.id.map(|_| folder_href(&self.0.root))
    }
}

/// The Ingest Folders: every ingest root, registered or not.
#[component]
pub fn IngestFolders() -> impl IntoView {
    let live = use_live();
    // Shared with the Ingest badge, which counts again after every change made here.
    let refresh = use_ingest_changed();
    let folders = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        api::get::<Vec<IngestFolder>>("/ingest-folders")
    });
    let rows = last_good_rows(move || {
        folders
            .get()
            .map(|r| r.map(|f| f.into_iter().map(FolderRow).collect::<Vec<_>>()))
    });
    let registering = Action::new_local(move |root: &String| {
        let body = RegisterIngestFolder { root: root.clone() };
        async move {
            let done = api::post::<_, IngestFolder>("/ingest-folders", &body).await;
            if done.is_ok() {
                refresh.update(|n| *n += 1);
            }
            done
        }
    });
    let row_actions = Callback::new(move |row: FolderRow| {
        let root = row.0.root.clone();
        match row.0.id {
            Some(_) => vec![RowAction::go(
                Glyph::FolderInput,
                format!("Open {root}"),
                folder_href(&root),
            )],
            None => vec![RowAction::run(
                Glyph::CirclePlus,
                format!("Register {root}"),
                move || {
                    registering.dispatch_local(root.clone());
                },
            )],
        }
    });
    view! {
        <h1>"Ingest folders"</h1>
        <p class="lede">
            "An ingest folder is a writable root of kind ingest where new files arrive. A scan assesses each item \
            against every source set; items leave the folder only through a plan you approve."
        </p>
        {move || match folders.get() {
            None => view! { <Loading what="the ingest folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <ActionPending action=registering status="Registering…">
            <List
                id="ingest-folders"
                label="Ingest folders"
                rows
                empty="No ingest folders. Declare a root with kind = \"ingest\" and writable = true in your \
                    configuration, then restart the service."
                placeholder="Filter by folder or path"
                row_actions
                detail=Callback::new(folder_detail)
            />
        </ActionPending>
    }
}

fn folder_detail(row: FolderRow) -> AnyView {
    let f = row.0;
    let counts = if f.counts.is_empty() {
        "No items".to_string()
    } else {
        f.counts
            .iter()
            .map(|(state, n)| format!("{n} {}", state.label()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    view! {
        <dl class="facts">
            <dt>"Path"</dt>
            <dd>
                <code class="path">{f.path.clone()}</code>
            </dd>
            <dt>"Registered"</dt>
            <dd>
                {if f.id.is_some() {
                    "Yes"
                } else {
                    "Not yet: register it to scan it and move items out of it"
                }}
            </dd>
            <dt>"Items"</dt>
            <dd>{counts}</dd>
        </dl>
    }
    .into_any()
}

#[component]
pub fn IngestFolderScreen() -> impl IntoView {
    let params = use_params_map();
    let root = move || params.read().get("root").unwrap_or_default();
    let live = use_live();
    // Shared with the Ingest badge, which counts again after every change made here.
    let refresh = use_ingest_changed();
    let folder = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let root = root();
        async move {
            api::get::<IngestFolder>(&format!("/ingest-folders/{}", api::segment(&root))).await
        }
    });
    // Rendered once per folder: a reload after a decision updates the facts and lists in place, keeping the plan under
    // review and what was said.
    move || {
        let root = root();
        view! { <FolderView root folder refresh /> }
    }
}

#[component]
fn FolderView(
    root: String,
    folder: LocalResource<Result<IngestFolder, String>>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let base = format!("/ingest-folders/{}", api::segment(&root));
    let latest = Memo::new(
        move |prev: Option<&Option<IngestFolder>>| match folder.get() {
            Some(Ok(f)) => Some(f),
            _ => prev.cloned().flatten(),
        },
    );
    let facts = move || match (latest.get(), folder.get()) {
        (Some(folder), _) => {
            let schedule = match folder.scan_every_minutes {
                Some(m) => format!("Every {m} minutes, and when you ask"),
                None => "Only when you ask".into(),
            };
            view! {
                <dl class="facts">
                    <dt>"Path"</dt>
                    <dd>
                        <code class="path">{folder.path.clone()}</code>
                    </dd>
                    <dt>"Settles after"</dt>
                    <dd>{format!("{} seconds unchanged", folder.settle_secs)}</dd>
                    <dt>"Scans"</dt>
                    <dd>{schedule}</dd>
                    <dt>"Last scan"</dt>
                    <dd>{folder.last_scan_at.as_deref().map(when).unwrap_or_else(|| "Never".into())}</dd>
                </dl>
            }
            .into_any()
        }
        (None, None) => view! { <Loading what="the ingest folder" /> }.into_any(),
        (None, Some(Err(message))) => view! { <Problem message /> }.into_any(),
        (None, Some(Ok(_))) => ().into_any(),
    };
    view! {
        <h1>{format!("Ingest folder {root}")}</h1>
        {facts}
        <Items folder=latest base=base.clone() refresh />
        <KeptFiles base refresh />
    }
}

/// An item in the folder's list.
#[derive(Debug, Clone, PartialEq)]
struct ItemRow {
    item: IngestItem,
    status: IngestStatus,
    /// How often an arriving item is looked at: the folder's scan cadence, or none when only you start scans.
    cadence: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemCol {
    Status,
    Name,
    Proposed,
    Size,
    Seen,
}

impl ItemRow {
    /// What is proposed, or under way, in a few words.
    fn proposed(&self) -> String {
        let item = &self.item;
        let job = item.last_move.as_ref().and_then(|m| m.job);
        match self.status {
            IngestStatus::Moving => match job {
                Some(job) => format!("Moving · job #{job}"),
                None => "Moving".into(),
            },
            IngestStatus::Arriving => match &self.cadence {
                Some(every) => format!("Assessed once settled · {every}"),
                None => "Assessed once settled · when you scan".into(),
            },
            IngestStatus::MatchesSourceSet => match &item.assessment.proposal {
                Some(Proposal::SourceSet {
                    source_set, game, ..
                }) => format!("{game} in {source_set}"),
                _ => "A source set needs it".into(),
            },
            IngestStatus::NoDatMatch => match &item.assessment.proposal {
                Some(Proposal::Addition {
                    system: Some(system),
                }) => format!("Addition for {system}"),
                _ => "Addition; choose its system".into(),
            },
            IngestStatus::DatFile => match &item.assessment.dat {
                Some(d) => format!("Import DAT {} {}", d.name, d.version),
                None => "Import the DAT".into(),
            },
            IngestStatus::Duplicate => match &item.assessment.dat {
                Some(_) => "DAT already imported".into(),
                None => "Already held".into(),
            },
            IngestStatus::Unreadable => item
                .reason
                .clone()
                .unwrap_or_else(|| "Couldn't be read".into()),
            IngestStatus::Dismissed => "Dismissed until it changes".into(),
            IngestStatus::Done => item
                .last_move
                .as_ref()
                .and_then(|m| m.note.clone())
                .unwrap_or_else(|| "Moved out of the folder".into()),
        }
    }
}

impl ListRow for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Status,
        ItemCol::Name,
        ItemCol::Proposed,
        ItemCol::Size,
        ItemCol::Seen,
    ];

    fn column(col: ItemCol) -> Column {
        match col {
            ItemCol::Status => Column::new("status", "Status", Width::Fixed(3.5))
                .sortable()
                .hint("What the item is, and whether it's being moved"),
            ItemCol::Name => Column::new(
                "name",
                "Item",
                Width::Flex {
                    min: 14.0,
                    share: 3,
                },
            )
            .sortable(),
            ItemCol::Proposed => Column::new(
                "proposed",
                "Proposed",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            ItemCol::Size => Column::new("size", "Size", Width::Fixed(7.0))
                .sortable()
                .numeric(),
            ItemCol::Seen => Column::new("seen", "First seen", Width::Fixed(13.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        self.item.id.to_string()
    }

    fn title(&self) -> String {
        self.item.name.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}",
            self.item.name,
            self.status.look().label,
            self.proposed()
        )
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        match col {
            ItemCol::Status => {
                view! { <StatusIcon status=self.status tab_stop=false /> }.into_any()
            }
            ItemCol::Name => view! { <Clip text=self.item.name.clone() /> }.into_any(),
            ItemCol::Proposed => view! { <Clip text=self.proposed() /> }.into_any(),
            ItemCol::Size => ingest_model::size(self.item.file.size).into_any(),
            ItemCol::Seen => view! { <Clip text=when(&self.item.first_seen_at) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: ItemCol) -> Ordering {
        match col {
            ItemCol::Status => self.status.rank().cmp(&other.status.rank()),
            ItemCol::Name => self.item.name.cmp(&other.item.name),
            ItemCol::Proposed => self.proposed().cmp(&other.proposed()),
            ItemCol::Size => self.item.file.size.cmp(&other.item.file.size),
            ItemCol::Seen => {
                if earlier(&self.item.first_seen_at, &other.item.first_seen_at) {
                    Ordering::Less
                } else if earlier(&other.item.first_seen_at, &self.item.first_seen_at) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }
        }
    }
}

/// What a decision asks of the Service: a plan to review, or dismissing items at once.
#[derive(Debug, Clone)]
enum Ask {
    Plan(PlanIngest),
    Dismiss(Vec<i64>),
}

#[derive(Debug, Clone)]
enum Decided {
    Planned(i64),
    Dismissed(usize),
}

/// The `status` URL value: the one status the list shows, if you chose one.
const STATUS: &str = "status";
/// The `plan` URL value: the plan under review, shown in place of the items.
const PLAN: &str = "plan";
/// The URL values a list keeps its state in, which the other list on the page must not inherit.
const LIST_STATE: [&str; 5] = ["q", "sort", "row", "sel", "all"];

/// The folder's items: a strip of how many show each status, with the decisions you can apply to all of them; the
/// list, with decisions on the items you check; and the plan you review before anything moves.
#[component]
fn Items(
    folder: Memo<Option<IngestFolder>>,
    base: String,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let live = use_live();
    let items = {
        let base = base.clone();
        LocalResource::new(move || {
            live.settled.track();
            refresh.track();
            let path = format!("{base}/items");
            async move { api::get::<Vec<IngestItem>>(&path).await }
        })
    };
    let plans = {
        let base = base.clone();
        LocalResource::new(move || {
            live.settled.track();
            refresh.track();
            let path = format!("{base}/plans");
            async move { api::get::<Vec<IngestPlan>>(&path).await }
        })
    };
    let all = last_good_rows(move || items.get());
    let cadence = Memo::new(move |_| folder.with(|f| f.as_ref().and_then(scan_cadence)));
    let rows: Signal<Arc<Vec<ItemRow>>> = Signal::derive(move || {
        Arc::new(
            all.get()
                .iter()
                .filter_map(|item| {
                    ingest_model::status(item).map(|status| ItemRow {
                        item: item.clone(),
                        status,
                        cadence: cadence.get(),
                    })
                })
                .collect(),
        )
    });

    // What you chose to add items as, by id, and the provider to import DATs under.
    let edits = RwSignal::new(BTreeMap::<i64, NewAddition>::new());
    let provider = RwSignal::new(String::new());
    let choices = move || Choices {
        additions: edits.get(),
        provider: provider.get(),
    };
    let strip = Memo::new(move |_| ingest_model::strip(&all.get(), &choices()));

    // The one status the list shows, from the URL.
    let query = use_query_map();
    let only = Memo::new(move |_| {
        query
            .read()
            .get(STATUS)
            .and_then(|v| ingest_model::parse_slug(&v))
    });
    let location = use_location();
    let navigate = use_navigate();
    let target = RwSignal::new(None::<String>);
    Effect::new(move || {
        if let Some(path) = target.get() {
            navigate(
                &path,
                NavigateOptions {
                    replace: true,
                    scroll: false,
                    ..Default::default()
                },
            );
        }
    });
    // Sets the URL value `key`, or removes it, dropping the `cleared` ones too.
    let set_query = move |key: &str, value: Option<String>, cleared: &[&str]| {
        let mut pairs: Vec<(String, String)> = query.with_untracked(|q| {
            q.latest_values()
                .filter(|(k, _)| k.as_ref() != key && !cleared.contains(&k.as_ref()))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        });
        if let Some(value) = value {
            pairs.push((key.into(), value));
        }
        let path = location.pathname.get_untracked();
        target.set(Some(format!("{path}{}", query_string(&pairs))));
    };
    let show_only = move |status: Option<IngestStatus>| {
        set_query(STATUS, status.map(|s| ingest_model::slug(s).into()), &[])
    };
    // The plan under review, by id, from the URL. The plan's list and the items' list each keep their own state in
    // the URL, so going from one to the other leaves the last one's behind.
    let reviewing = Memo::new(move |_| query.read().get(PLAN).and_then(|v| v.parse::<i64>().ok()));
    let review = Callback::new(move |plan: Option<i64>| {
        set_query(PLAN, plan.map(|p| p.to_string()), &LIST_STATE)
    });
    let keep: Signal<Keep<ItemRow>> = Signal::derive(move || {
        let only = only.get();
        Some(
            Arc::new(move |row: &ItemRow| ingest_model::shown(only, row.status))
                as Arc<dyn Fn(&ItemRow) -> bool + Send + Sync>,
        )
    });
    let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.status)));

    let said = RwSignal::new(None::<String>);
    let deciding = RwSignal::new(String::new());
    let decide = {
        let base = base.clone();
        Action::new_local(move |ask: &Ask| {
            let (base, ask) = (base.clone(), ask.clone());
            async move {
                let decided = match ask {
                    Ask::Plan(body) => api::post::<_, IngestPlan>(&format!("{base}/plans"), &body)
                        .await
                        .map(|plan| Decided::Planned(plan.id)),
                    Ask::Dismiss(items) => api::post::<_, Vec<IngestItem>>(
                        &format!("{base}/dismissals"),
                        &DismissIngestItems { items },
                    )
                    .await
                    .map(|done| Decided::Dismissed(done.len())),
                };
                if decided.is_ok() {
                    refresh.update(|n| *n += 1);
                }
                decided
            }
        })
    };
    Effect::new(move || match decide.value().get() {
        Some(Ok(Decided::Planned(id))) => review.run(Some(id)),
        Some(Ok(Decided::Dismissed(n))) => said.set(Some(match n {
            1 => "Dismissed 1 item. It stays dismissed until it changes.".into(),
            n => format!("Dismissed {n} items. They stay dismissed until they change."),
        })),
        _ => {}
    });
    // Applies `decision` to those of `asked` it applies to, saying how many it leaves out.
    let apply = Callback::new(move |(decision, asked): (Decision, Vec<IngestItem>)| {
        let choices = Choices {
            additions: edits.get_untracked(),
            provider: provider.get_untracked(),
        };
        let bulk = ingest_model::choose(decision, &asked, &choices);
        said.set((bulk.left_out > 0).then(|| decision.left_out(bulk.left_out)));
        if bulk.items.is_empty() {
            return;
        }
        deciding.set(decision.pending().into());
        let ask = match ingest_model::request(decision, &bulk, &asked, &choices) {
            Some(plan) => Ask::Plan(plan),
            None => Ask::Dismiss(bulk.items),
        };
        decide.dispatch_local(ask);
    });

    let scanning = {
        let base = base.clone();
        Action::new_local(move |_: &()| {
            let path = format!("{base}/scans");
            async move {
                let started = api::post::<_, Job>(&path, &()).await;
                if started.is_ok() {
                    refresh.update(|n| *n += 1);
                }
                started
            }
        })
    };
    let registered =
        Memo::new(move |_| folder.with(|f| f.as_ref().is_some_and(|f| f.id.is_some())));
    let mut actions = vec![ToolAction::page(Glyph::RefreshCw, "Scan now", move || {
        if registered.get_untracked() {
            scanning.dispatch_local(());
        }
    })];
    actions.extend(Decision::ALL.into_iter().map(|decision| {
        ToolAction::checked(
            decision.glyph(),
            decision.verb(),
            move |chosen: Selected<ItemRow>| {
                apply.run((decision, chosen.rows.into_iter().map(|r| r.item).collect()))
            },
        )
    }));
    let actions = StoredValue::new(actions);
    let row_actions = Callback::new(move |row: ItemRow| {
        let item = &row.item;
        let mut actions = Vec::new();
        if let Some(m) = &item.last_move {
            if let Some(job) = m.job {
                actions.push(RowAction::go(
                    Glyph::Activity,
                    format!("Open job #{job}"),
                    format!("/activity?row={job}"),
                ));
            }
            if let (Some(plan), Some(Proposal::SourceSet { source_set, .. })) =
                (m.repair, &item.assessment.proposal)
            {
                actions.push(RowAction::go(
                    Glyph::Wrench,
                    format!("Open repair #{plan}"),
                    format!(
                        "/source-sets/{}/repair?plan={plan}",
                        api::segment(source_set)
                    ),
                ));
            }
            if let Some(plan) = m.plan {
                actions.push(RowAction::run(
                    Glyph::ClipboardList,
                    format!("Review plan #{plan}"),
                    move || review.run(Some(plan)),
                ));
            }
        }
        if row.status == IngestStatus::MatchesSourceSet
            && let Some(Proposal::SourceSet { source_set, .. }) = &item.assessment.proposal
        {
            actions.push(RowAction::go(
                Glyph::Wrench,
                format!("Repair {source_set}"),
                format!("/source-sets/{}/repair", api::segment(source_set)),
            ));
        }
        // Tracked, so the open item's detail offers a decision as soon as you fill in what it needs.
        let choices = Choices {
            additions: edits.get(),
            provider: provider.get(),
        };
        for decision in Decision::ALL {
            if ingest_model::applies(decision, item, &choices) {
                let item = item.clone();
                actions.push(RowAction::run(
                    decision.glyph(),
                    decision.single(),
                    move || apply.run((decision, vec![item.clone()])),
                ));
            }
        }
        actions
    });
    let detail = Callback::new(move |row: ItemRow| item_detail(row, edits, provider));

    // The newest plan you approved that is still moving items, if any.
    let moving_plan = Memo::new(move |_| {
        plans.get().and_then(Result::ok).and_then(|plans| {
            plans
                .into_iter()
                .find(|p| p.approval.is_some() && p.finished_at.is_none())
        })
    });
    let banner = move || {
        moving_plan.get().map(|plan| {
            let id = plan.id;
            let job = all.with(|items| {
                items
                    .iter()
                    .filter_map(|i| i.last_move.as_ref())
                    .find(|m| m.plan == Some(id) && m.under_way)
                    .and_then(|m| m.job)
            });
            let n = plan.content.items.len();
            let what = if n == 1 {
                "1 item".to_string()
            } else {
                format!("{n} items")
            };
            view! {
                <p class="banner" role="status">
                    <span class="spinner" aria-hidden="true"></span>
                    <span>{format!("Plan #{id} you approved is moving {what} out of the folder.")}</span>
                    {job.map(|job| view! {
                        <A href=format!("/activity?row={job}")>{format!("Job #{job}")}</A>
                    })}
                    <button type="button" on:click=move |_| review.run(Some(id))>
                        {format!("Review plan #{id}")}
                    </button>
                </p>
            }
        })
    };

    let filters = move || {
        view! {
            <label>
                "Status"
                <select on:change=move |ev| {
                    show_only(ingest_model::parse_slug(&event_target_value(&ev)))
                }>
                    <option value="" selected=move || only.get().is_none()>
                        "Still in the folder"
                    </option>
                    {IngestStatus::ALL
                        .iter()
                        .map(|&s| {
                            view! {
                                <option
                                    value=ingest_model::slug(s)
                                    selected=move || only.get() == Some(s)
                                >
                                    {s.look().label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            </label>
        }
    };

    view! {
        <section aria-labelledby="items-title">
            <h2 id="items-title">"Items"</h2>
            <p class="lede">
                "Each item is assessed once its size and times stop changing. Decide for every item of a status at \
                once, for the items you check, or for one. Nothing moves until you approve the plan a decision makes; \
                dismissing moves nothing."
            </p>
            {move || match items.get() {
                None => view! { <Loading what="the items" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            {banner}
            <ActionPending action=scanning status="Starting a scan…">
                {move || (folder.with(Option::is_some) && !registered.get()).then(|| view! {
                    <p class="note">"Register the folder before you scan it."</p>
                })}
            </ActionPending>
            {move || match reviewing.get() {
                Some(plan) => view! {
                    <IngestPlanReview plan refresh back=Callback::new(move |()| review.run(None)) />
                }
                .into_any(),
                None => view! {
                    <ActionPending action=decide status=deciding>
                        <Strip strip only apply all provider show_only=Callback::new(show_only) />
                    </ActionPending>
                    <p class="note" role="status">
                        {move || said.get()}
                    </p>
                    <List
                        id="ingest-items"
                        label="Items"
                        rows
                        empty="Nothing has arrived in the folder yet. Items appear here after a scan."
                        placeholder="Filter by name, status or proposal"
                        actions=actions.get_value()
                        row_actions
                        quick=2
                        detail
                        filters=filters
                        keep
                        legend=move || view! { <Legend statuses list="ingest-items" /> }
                    />
                }
                .into_any(),
            }}
        </section>
    }
}

/// How many items show each status, with the decisions you can apply to all of them.
#[component]
fn Strip(
    strip: Memo<Vec<Count>>,
    only: Memo<Option<IngestStatus>>,
    apply: Callback<(Decision, Vec<IngestItem>)>,
    all: Signal<Arc<Vec<IngestItem>>>,
    provider: RwSignal<String>,
    show_only: Callback<Option<IngestStatus>>,
) -> impl IntoView {
    // Whether a DAT waiting to be imported has no registered DAT naming its provider.
    let needs_provider = Memo::new(move |_| {
        all.with(|items| {
            items.iter().any(|i| {
                ingest_model::status(i) == Some(IngestStatus::DatFile)
                    && i.assessment
                        .dat
                        .as_ref()
                        .is_some_and(|d| !d.imported && d.providers.len() != 1)
            })
        })
    });
    move || {
        let counts = strip.get();
        if counts.is_empty() {
            return ().into_any();
        }
        view! {
            <ul class="ingest-strip" aria-label="Items by status">
                {counts
                    .into_iter()
                    .map(|count| {
                        let status = count.status;
                        let look = status.look();
                        let pressed = move || only.get() == Some(status);
                        let toggle = move |_| {
                            show_only.run(if only.get_untracked() == Some(status) { None } else { Some(status) })
                        };
                        let decisions = count
                            .decisions
                            .into_iter()
                            .map(|(decision, bulk)| {
                                let label = decision.label(bulk.items.len());
                                let ids = bulk.items;
                                let run = move |_| {
                                    let asked = all.with_untracked(|items| {
                                        items.iter().filter(|i| ids.contains(&i.id)).cloned().collect()
                                    });
                                    apply.run((decision, asked));
                                };
                                view! {
                                    <button type="button" on:click=run>
                                        <LabelledIcon glyph=decision.glyph() label=label />
                                    </button>
                                }
                            })
                            .collect_view();
                        let provider_field = (status == IngestStatus::DatFile && needs_provider.get()).then(|| {
                            view! {
                                <label class="inline">
                                    "Provider for DATs no source set uses yet "
                                    <input
                                        type="text"
                                        prop:value=move || provider.get()
                                        on:input=move |ev| provider.set(event_target_value(&ev))
                                    />
                                </label>
                            }
                        });
                        view! {
                            <li>
                                <button
                                    type="button"
                                    class="ingest-count"
                                    aria-pressed=move || pressed().to_string()
                                    title=look.hint
                                    on:click=toggle
                                >
                                    <span class=format!("status {}", look.tone.class())>
                                        <Svg glyph=look.glyph />
                                    </span>
                                    <span>{look.label}</span>
                                    <span class="tally">{count.items}</span>
                                </button>
                                {decisions}
                                {provider_field}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
        .into_any()
    }
}

fn item_detail(
    row: ItemRow,
    edits: RwSignal<BTreeMap<i64, NewAddition>>,
    provider: RwSignal<String>,
) -> AnyView {
    let item = row.item.clone();
    let id = item.id;
    let moving = ingest_model::moving(&item);
    let proposal = item.assessment.proposal.as_ref().map(proposal_label);
    let last_move = item.last_move.clone().map(|m| {
        let what = match (m.under_way, &m.error, &m.note) {
            (true, _, _) => "Under way".to_string(),
            (false, Some(e), _) => format!("Failed: {e}"),
            (false, None, Some(note)) => capitalised(note),
            (false, None, None) => "Ended".to_string(),
        };
        let by = match (m.plan, m.repair) {
            (Some(plan), _) => format!("Plan #{plan}"),
            (None, Some(repair)) => format!("Repair #{repair}"),
            (None, None) => "A plan".to_string(),
        };
        let at = m
            .finished_at
            .as_deref()
            .map(|at| format!(", {}", when(at)))
            .unwrap_or_default();
        view! {
            <dt>"Last move"</dt>
            <dd>
                {format!("{by}: {what}{at}")}
                {m.job.map(|job| view! {
                    " · " <A href=format!("/activity?row={job}")>{format!("Job #{job}")}</A>
                })}
            </dd>
        }
    });
    let addable = match &item.assessment.proposal {
        Some(Proposal::Addition { system }) if item.assessment.payloads.len() == 1 && !moving => {
            let initial = edits
                .with_untracked(|e| e.get(&id).cloned())
                .unwrap_or_else(|| new_addition(&item.name, system.as_deref()));
            let value = RwSignal::new(initial);
            Effect::new(move || {
                let v = value.get();
                edits.update(|e| {
                    if e.get(&id) != Some(&v) {
                        e.insert(id, v);
                    }
                });
            });
            Some(view! {
                <div class="form">
                    <AdditionFields value />
                </div>
            })
        }
        _ => None,
    };
    let provider_field = item
        .assessment
        .dat
        .as_ref()
        .filter(|d| !d.imported && d.providers.len() != 1 && !moving)
        .map(|_| {
            view! {
                <div class="form">
                    <label>
                        "Provider, for a DAT no source set uses yet"
                        <input
                            type="text"
                            prop:value=move || provider.get()
                            on:input=move |ev| provider.set(event_target_value(&ev))
                        />
                    </label>
                </div>
            }
        });
    view! {
        <dl class="facts">
            <dt>"Status"</dt>
            <dd>
                <StatusBadge status=row.status />
            </dd>
            {proposal.map(|p| view! {
                <dt>"Proposed"</dt>
                <dd>{p}</dd>
            })}
            {item.reason.clone().map(|r| view! {
                <dt>"Why"</dt>
                <dd>{r}</dd>
            })}
            {(row.status == IngestStatus::Arriving).then(|| view! {
                <dt>"Assessed"</dt>
                <dd>{row.proposed()}</dd>
            })}
            {last_move}
            <dt>"Size"</dt>
            <dd>{format!("{} ({} bytes)", ingest_model::size(item.file.size), item.file.size)}</dd>
            <dt>"First seen"</dt>
            <dd>{when(&item.first_seen_at)}</dd>
            {item.assessed_at.clone().map(|at| view! {
                <dt>"Evidence"</dt>
                <dd>
                    {format!("Assessed {}", when(&at))}
                    {evidence(&item.assessment)}
                </dd>
            })}
        </dl>
        {addable}
        {provider_field}
    }
    .into_any()
}

#[component]
fn KeptFiles(base: String, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let path = format!("{base}/kept-files");
        async move { api::get::<Vec<IngestKeptFile>>(&path).await }
    });
    let summary = move || match files.get() {
        Some(Ok(files)) => format!("Kept files ({})", files.len()),
        _ => "Kept files".to_string(),
    };
    view! {
        <details class="kept">
            <summary>{summary}</summary>
            <p class="lede">
                "Items you discarded, DATs you imported and the originals of repacks, kept aside in the folder. \
                Nothing here is deleted until you approve a cleanup."
            </p>
            {move || match files.get() {
                None => view! { <Loading what="the kept files" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(files)) if files.is_empty() => {
                    view! { <p class="note">"No files are kept aside."</p> }.into_any()
                }
                Some(Ok(files)) => view! {
                    <ul class="plain">
                        {files
                            .into_iter()
                            .map(|f| {
                                view! {
                                    <li>
                                        <code>{f.name.clone()}</code>
                                        {format!(
                                            " as {RETENTION}/{}/{}: {}, {}, {}",
                                            f.generation,
                                            f.kept_name,
                                            f.reason,
                                            ingest_model::size(f.file.size),
                                            when(&f.kept_at),
                                        )}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                }
                .into_any(),
            }}
        </details>
    }
}
