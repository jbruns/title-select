use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use leptos::html::Div;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use leptos_struct_table::ColumnSort;
use title_select_api::selection::{
    ChoiceStatus, ReleaseChange, Role, Rule, SelectionGroup, SelectionProfile,
};
use title_select_api::{
    Availability, GameStatus, Job, ProfileRecord, ProfileVersion, SaveProfile, SavedProfile,
    SelectionDiff, SelectionRecord, SourceSetRecord, StartSelection, StartSelectionFor,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{Sort, game_status_label, when};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Clip, Column, Keep, List, ListRow, Width};
use crate::parts::overlay::Tooltip;
use crate::parts::status::{GroupStatus, Status, StatusIcon, Tone};
use crate::parts::url_state::use_list_query;
use crate::screens::PlanABuild;
use crate::screens::audit::{ReAudit, focus};
use crate::selection_model::{
    AvailabilityFilter, FlagFilter, GroupAvailability, ReleaseColumn, ReleaseFilter,
    SelectionIndex, availability_label, checked, choice_label, confirm, confirm_unresolved,
    flag_labels, group_status, include_and_check, role_label, staged_changes, toggle,
    unresolved_explanation,
};
use crate::table::{DataTable, Grid, RowSource, Rows, rows_of, sort_of};

/// A rule as the workbench shows it: the stage and the engine's message, or "Your choice".
pub fn rule_text(rule: &Rule) -> String {
    if rule.is_choice() {
        "Your choice".to_string()
    } else {
        format!("{}: {}", rule.stage, rule.message)
    }
}

/// Says that a Job was queued to make a Selection, and links to the Selection once it is made.
#[component]
pub fn SelectionQueued(job: i64) -> impl IntoView {
    let live = use_live();
    let made = move || {
        live.book.with(|b| {
            b.get(job)
                .map(|l| (l.job.state, l.job.selection, l.job.error.clone()))
        })
    };
    view! {
        <p class="done" role="status">
            {format!("Job #{job} is queued to make a selection. ")}
            {move || match made() {
                Some((_, Some(selection), _)) => {
                    view! { <A href=format!("/selections/{selection}")>"Open the new selection"</A> }
                        .into_any()
                }
                Some((state, None, Some(error))) if state.is_final() => {
                    view! { <span class="failed">{format!("It failed: {error}")}</span> }.into_any()
                }
                _ => view! { <A href="/activity">"Follow it in Activity"</A> }.into_any(),
            }}
        </p>
    }
}

/// A release in a Selection's table, by its index.
#[derive(Clone)]
pub struct ReleaseItem {
    index: Arc<SelectionIndex>,
    i: u32,
}

struct ReleaseWindow {
    index: Arc<SelectionIndex>,
    shown: Arc<Vec<u32>>,
}

impl RowSource<ReleaseItem> for ReleaseWindow {
    fn len(&self) -> usize {
        self.shown.len()
    }

    fn rows(&self, range: Range<usize>) -> Vec<ReleaseItem> {
        self.shown[range]
            .iter()
            .map(|&i| ReleaseItem {
                index: self.index.clone(),
                i,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReleaseCol {
    Group,
    Release,
    Role,
    Availability,
    Rule,
    Flags,
}

impl ReleaseCol {
    fn sort_key(self) -> Option<ReleaseColumn> {
        match self {
            ReleaseCol::Group => Some(ReleaseColumn::Group),
            ReleaseCol::Release => Some(ReleaseColumn::Release),
            ReleaseCol::Role => Some(ReleaseColumn::Role),
            ReleaseCol::Availability => Some(ReleaseColumn::Availability),
            ReleaseCol::Rule | ReleaseCol::Flags => None,
        }
    }
}

fn role_view(role: Role) -> AnyView {
    view! { <span class=format!("role role-{}", role.as_str())>{role_label(role)}</span> }
        .into_any()
}

fn availability_view(status: Option<GameStatus>, claimed: bool) -> AnyView {
    match status {
        Some(s) => {
            view! { <span class=format!("avail avail-{}", s.as_str())>{availability_label(Some(s), claimed)}</span> }
                .into_any()
        }
        None => ().into_any(),
    }
}

impl Grid for ReleaseItem {
    type Col = ReleaseCol;
    const COLUMNS: &'static [ReleaseCol] = &[
        ReleaseCol::Group,
        ReleaseCol::Release,
        ReleaseCol::Role,
        ReleaseCol::Availability,
        ReleaseCol::Rule,
        ReleaseCol::Flags,
    ];

    fn header(col: ReleaseCol) -> &'static str {
        match col {
            ReleaseCol::Group => "Group",
            ReleaseCol::Release => "Release",
            ReleaseCol::Role => "Role",
            ReleaseCol::Availability => "Availability",
            ReleaseCol::Rule => "Why",
            ReleaseCol::Flags => "Flags",
        }
    }

    fn class(col: ReleaseCol) -> &'static str {
        match col {
            ReleaseCol::Group => "c-group",
            ReleaseCol::Release => "c-wide",
            ReleaseCol::Role | ReleaseCol::Availability => "c-status",
            ReleaseCol::Rule => "c-reason",
            ReleaseCol::Flags => "c-flags",
        }
    }

    fn sortable(col: ReleaseCol) -> bool {
        col.sort_key().is_some()
    }

    fn cell(&self, col: ReleaseCol) -> AnyView {
        let i = self.i as usize;
        let row = &self.index.rows[i];
        match col {
            ReleaseCol::Group => row.group.clone().into_any(),
            ReleaseCol::Release => row.title.clone().into_any(),
            ReleaseCol::Role => role_view(row.role),
            ReleaseCol::Availability => {
                availability_view(self.index.availability(i), self.index.claimed(i))
            }
            ReleaseCol::Rule => {
                let text = rule_text(&row.rule);
                let title = row.rule.reference.clone().unwrap_or_default();
                view! { <span title=title>{text}</span> }.into_any()
            }
            ReleaseCol::Flags => flag_labels(self.index.flags(i))
                .into_iter()
                .map(|label| {
                    let review = label != "Coverage drift";
                    view! { <span class=if review { "state review" } else { "tag" }>{label}</span> " " }
                })
                .collect_view()
                .into_any(),
        }
    }
}

/// A Selection's releases with their filters and table. Filtering and sorting run in the browser on every keystroke.
/// Selecting a release selects its group.
#[component]
pub fn ReleasesPanel(
    index: Arc<SelectionIndex>,
    /// Whether an Audit says which releases are available.
    audited: bool,
    group: RwSignal<Option<String>>,
    /// The ID of the element that shows the selected group, focused on Enter.
    detail_id: &'static str,
) -> impl IntoView {
    let total = index.len();
    let sorting = RwSignal::new(VecDeque::<(ReleaseCol, ColumnSort)>::new());
    let filter = RwSignal::new(ReleaseFilter::default());
    let selected = RwSignal::new(None::<usize>);
    let panel = NodeRef::<Div>::new();

    let sorted = Memo::new({
        let index = index.clone();
        move |_| {
            let sort = sort_of(&sorting.read()).and_then(|s| {
                s.column.sort_key().map(|column| Sort {
                    column,
                    descending: s.descending,
                })
            });
            Arc::new(index.sorted(sort))
        }
    });
    let shown = Memo::new({
        let index = index.clone();
        move |_| filter.with(|f| sorted.with(|order| Arc::new(index.filtered(order, f))))
    });
    let rows = Signal::derive({
        let index = index.clone();
        move || {
            Arc::new(ReleaseWindow {
                index: index.clone(),
                shown: shown.get(),
            }) as Rows<ReleaseItem>
        }
    });
    Effect::new({
        let index = index.clone();
        move || {
            let Some(at) = selected.get() else {
                return;
            };
            if let Some(&i) = shown.with_untracked(|s| s.get(at).copied()).as_ref() {
                group.set(Some(index.rows[i as usize].group.clone()));
            }
        }
    });

    let refilter = move |change: Box<dyn FnOnce(&mut ReleaseFilter)>| {
        filter.update(|f| change(f));
        selected.set(None);
        if let Some(scroll) = panel
            .get_untracked()
            .and_then(|p| p.query_selector(".table-scroll").ok().flatten())
        {
            scroll.set_scroll_top(0);
        }
    };

    view! {
        <div class="panel" node_ref=panel>
            <form class="filters" on:submit=|ev| ev.prevent_default()>
                <label>
                    "Filter releases"
                    <input
                        type="search"
                        class="filter-text"
                        placeholder="Group or release name"
                        autocomplete="off"
                        on:input=move |ev| {
                            let text = event_target_value(&ev);
                            refilter(Box::new(move |f| f.text = text));
                        }
                    />
                </label>
                <label>
                    "Role"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(Box::new(move |f| f.role = Role::parse(&v)));
                    }>
                        <option value="">"Any"</option>
                        {Role::ALL
                            .iter()
                            .map(|r| view! { <option value=r.as_str()>{role_label(*r)}</option> })
                            .collect_view()}
                    </select>
                </label>
                <label>
                    "Availability"
                    <select disabled=!audited on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(
                            Box::new(move |f| {
                                f.availability = match v.as_str() {
                                    "unknown" => Some(AvailabilityFilter::Unknown),
                                    other => GameStatus::parse(other).map(AvailabilityFilter::Is),
                                };
                            }),
                        );
                    }>
                        <option value="">"Any"</option>
                        {GameStatus::ALL
                            .iter()
                            .map(|s| view! { <option value=s.as_str()>{game_status_label(*s)}</option> })
                            .collect_view()}
                        <option value="unknown">"Not in the audit"</option>
                    </select>
                </label>
                <label>
                    "Flag"
                    <select on:change=move |ev| {
                        let v = event_target_value(&ev);
                        refilter(Box::new(move |f| f.flag = FlagFilter::parse(&v)));
                    }>
                        <option value="">"Any"</option>
                        {FlagFilter::ALL
                            .iter()
                            .map(|f| view! { <option value=f.key()>{f.label()}</option> })
                            .collect_view()}
                    </select>
                </label>
            </form>
            <p
                class="shown"
                aria-live="polite"
                data-query=move || filter.with(|f| f.text.clone())
                data-shown=move || shown.with(|s| s.len())
            >
                {move || format!("{} of {total} releases", shown.with(|s| s.len()))}
            </p>
            <DataTable
                id="releases"
                label="Releases".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(move |_| focus(detail_id))
                empty="No releases match these filters.".to_string()
            />
        </div>
    }
}

/// A change between two Selections, in a table.
#[derive(Clone)]
struct ChangeItem(ReleaseChange);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChangeCol {
    Group,
    Release,
    Before,
    After,
    Rule,
}

fn maybe_role(role: Option<Role>) -> AnyView {
    match role {
        Some(role) => role_view(role),
        None => view! { <span class="tag">"Not in the DAT"</span> }.into_any(),
    }
}

impl Grid for ChangeItem {
    type Col = ChangeCol;
    const COLUMNS: &'static [ChangeCol] = &[
        ChangeCol::Group,
        ChangeCol::Release,
        ChangeCol::Before,
        ChangeCol::After,
        ChangeCol::Rule,
    ];

    fn header(col: ChangeCol) -> &'static str {
        match col {
            ChangeCol::Group => "Group",
            ChangeCol::Release => "Release",
            ChangeCol::Before => "Before",
            ChangeCol::After => "Now",
            ChangeCol::Rule => "Why",
        }
    }

    fn class(col: ChangeCol) -> &'static str {
        match col {
            ChangeCol::Group => "c-group",
            ChangeCol::Release => "c-wide",
            ChangeCol::Before | ChangeCol::After => "c-status",
            ChangeCol::Rule => "c-reason",
        }
    }

    fn cell(&self, col: ChangeCol) -> AnyView {
        let change = &self.0;
        match col {
            ChangeCol::Group => change.group.clone().into_any(),
            ChangeCol::Release => change.title.clone().into_any(),
            ChangeCol::Before => maybe_role(change.before),
            ChangeCol::After => maybe_role(change.after),
            ChangeCol::Rule => rule_text(&change.rule).into_any(),
        }
    }
}

/// What changed, release by release, in a table.
#[component]
pub fn ChangesTable(id: &'static str, changes: Vec<ReleaseChange>) -> impl IntoView {
    let rows = rows_of(changes.into_iter().map(ChangeItem).collect::<Vec<_>>());
    view! {
        <DataTable
            id
            label="Changes".to_string()
            rows=Signal::stored(rows)
            sorting=RwSignal::new(VecDeque::new())
            selected=RwSignal::new(None)
            on_open=Callback::new(|_| {})
            empty="No release changes its role.".to_string()
        />
    }
}

/// Makes a what-if Selection: a Profile's Selection from a Source Set that no Library Folder uses yet. Opened from a
/// Profile (`?profile=`, pick a Source Set) or from a Source Set (`?source_set=`, pick a Profile).
#[component]
pub fn NewSelection() -> impl IntoView {
    let query = use_query_map();
    let given = |key: &'static str| query.read_untracked().get(key).filter(|v| !v.is_empty());
    let source_set = RwSignal::new(given("source_set").unwrap_or_default());
    let profile = RwSignal::new(given("profile").unwrap_or_default());
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let request = StartSelection {
            source_set: source_set.get_untracked(),
            revision: None,
            profile: profile.get_untracked(),
            version: None,
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, Job>("/selections", &request).await));
            busy.set(false);
        });
    };
    let ready = move || !source_set.get().is_empty() && !profile.get().is_empty() && !busy.get();
    let back = match (given("profile"), given("source_set")) {
        (Some(p), _) => Some((format!("/profiles/{}", api::segment(&p)), p)),
        (None, Some(s)) => Some((format!("/source-sets/{}", api::segment(&s)), s)),
        (None, None) => None,
    };

    view! {
        <h1>"New what-if selection"</h1>
        <p class="lede">
            "A what-if selection shows what a selection profile would keep from a source set, without changing any library folder. It's listed on its profile until a library folder uses it."
        </p>
        {back.map(|(href, name)| view! { <p><A href=href>{format!("Back to {name}")}</A></p> })}
        <form class="form" on:submit=submit>
            <label>
                "Source set"
                <select
                    required
                    prop:value=move || source_set.get()
                    on:change=move |ev| source_set.set(event_target_value(&ev))
                >
                    <option value="">"Choose a source set"</option>
                    {move || {
                        sets.get()
                            .and_then(Result::ok)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|s| {
                                let selected = s.name == source_set.get_untracked();
                                view! { <option value=s.name.clone() selected=selected>{s.name.clone()}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <label>
                "Selection profile"
                <select
                    required
                    prop:value=move || profile.get()
                    on:change=move |ev| profile.set(event_target_value(&ev))
                >
                    <option value="">"Choose a selection profile"</option>
                    {move || {
                        profiles
                            .get()
                            .and_then(Result::ok)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|p| p.latest.is_some() && p.retired_at.is_none())
                            .map(|p| {
                                let selected = p.name == profile.get_untracked();
                                let label = format!(
                                    "{} (version {})",
                                    p.name,
                                    p.latest.as_ref().map_or(0, |v| v.version),
                                );
                                view! { <option value=p.name.clone() selected=selected>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <button type="submit" class="primary" disabled=move || !ready()>
                {move || if busy.get() { "Queueing…" } else { "Make the selection" }}
            </button>
        </form>
        {move || match (sets.get(), profiles.get()) {
            (Some(Err(message)), _) | (_, Some(Err(message))) => {
                view! { <Problem message /> }.into_any()
            }
            _ => ().into_any(),
        }}
        {move || {
            outcome
                .get()
                .map(|outcome| match outcome {
                    Ok(job) => view! { <SelectionQueued job=job.id /> }.into_any(),
                    Err(message) => view! { <Problem message /> }.into_any(),
                })
        }}
    }
}

#[derive(Clone)]
struct Loaded {
    index: Arc<SelectionIndex>,
    availability: Availability,
}

async fn load(id: i64) -> Result<Loaded, String> {
    let selection = api::get(&format!("/selections/{id}/selection")).await?;
    let availability: Availability = api::get(&format!("/selections/{id}/availability")).await?;
    let index = SelectionIndex::new(selection, &availability.games, &availability.claimed);
    Ok(Loaded {
        index: Arc::new(index),
        availability,
    })
}

/// One Selection: what the engine and your choices selected, why, and what of it you have.
#[component]
pub fn SelectionScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let record = LocalResource::new(move || {
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<SelectionRecord>(&format!("/selections/{id}")).await,
                None => Err("That is not a selection number.".into()),
            }
        }
    });
    move || match record.get() {
        None => view! { <Loading what="the selection" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Selection"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(record)) => view! { <SelectionView record /> }.into_any(),
    }
}

/// One game group of a Selection as its list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupItem {
    name: String,
    status: GroupStatus,
    /// The releases kept: the winners.
    kept: Vec<String>,
    releases: usize,
    /// The rule that decided the first winner, or what excluded the group.
    rule: String,
    availability: GroupAvailability,
}

impl GroupItem {
    fn new(group: &SelectionGroup, availability: &Availability) -> Self {
        let first = group.winners.first();
        let rule = match (first, group.exclusions.first()) {
            (Some(w), _) => rule_text(&w.rule),
            (None, Some(e)) => e.reason.clone(),
            (None, None) => String::new(),
        };
        GroupItem {
            name: group.group.clone(),
            status: group_status(group),
            kept: group.winners.iter().map(|w| w.title.clone()).collect(),
            releases: group.winners.len() + group.alternatives.len() + group.exclusions.len(),
            rule,
            availability: GroupAvailability::of(group, &availability.games, &availability.claimed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupCol {
    Status,
    Group,
    Kept,
    Rule,
    Releases,
    Have,
}

impl ListRow for GroupItem {
    type Col = GroupCol;
    const COLUMNS: &'static [GroupCol] = &[
        GroupCol::Status,
        GroupCol::Group,
        GroupCol::Kept,
        GroupCol::Rule,
        GroupCol::Releases,
        GroupCol::Have,
    ];

    fn column(col: GroupCol) -> Column {
        match col {
            GroupCol::Status => Column::new("status", "Status", Width::Fixed(4.0))
                .sortable()
                .hint("Chosen, unresolved, your choice or excluded"),
            GroupCol::Group => Column::new(
                "group",
                "Group",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            GroupCol::Kept => Column::new(
                "kept",
                "Kept",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable()
            .hint("The releases the selection keeps"),
            GroupCol::Rule => Column::new(
                "rule",
                "Rule",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .hint("The rule that chose the kept release, or what excluded the group"),
            GroupCol::Releases => Column::new("releases", "Releases", Width::Fixed(6.0))
                .sortable()
                .numeric()
                .hint("How many releases the group has"),
            GroupCol::Have => Column::new("have", "You have", Width::Fixed(11.0))
                .sortable()
                .hint("The worst of the kept releases, from the latest audit"),
        }
    }

    fn key(&self) -> String {
        self.name.clone()
    }

    fn title(&self) -> String {
        self.name.clone()
    }

    fn search(&self) -> String {
        format!("{} {} {}", self.name, self.kept.join(" "), self.rule)
    }

    fn cell(&self, col: GroupCol) -> AnyView {
        match col {
            GroupCol::Status => {
                view! { <StatusIcon status=self.status tab_stop=false /> }.into_any()
            }
            GroupCol::Group => view! { <Clip text=self.name.clone() /> }.into_any(),
            GroupCol::Kept => {
                let kept = if self.kept.is_empty() {
                    "Nothing".to_string()
                } else {
                    self.kept.join(", ")
                };
                view! { <Clip text=kept /> }.into_any()
            }
            GroupCol::Rule => view! { <Clip text=self.rule.clone() /> }.into_any(),
            GroupCol::Releases => self.releases.to_string().into_any(),
            GroupCol::Have => view! { <Clip text=self.availability.label() /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: GroupCol) -> std::cmp::Ordering {
        match col {
            GroupCol::Status => self.status.rank().cmp(&other.status.rank()),
            GroupCol::Group => self.name.cmp(&other.name),
            GroupCol::Kept => self.kept.cmp(&other.kept),
            GroupCol::Rule => self.rule.cmp(&other.rule),
            GroupCol::Releases => self.releases.cmp(&other.releases),
            GroupCol::Have => self.availability.label().cmp(&other.availability.label()),
        }
    }
}

#[component]
fn SelectionView(record: SelectionRecord) -> impl IntoView {
    let id = record.id;
    let pruned = record.pruned;
    let record = StoredValue::new(record);
    let r = record.get_value();
    let data =
        LocalResource::new(move || async move { if pruned { None } else { Some(load(id).await) } });
    let diff = LocalResource::new(move || async move {
        if pruned {
            None
        } else {
            api::get::<Option<SelectionDiff>>(&format!("/selections/{id}/diff"))
                .await
                .ok()
                .flatten()
        }
    });
    let version_path = format!(
        "/profiles/{}/versions/{}",
        api::segment(&r.profile.name),
        r.profile.version
    );
    let saved = LocalResource::new(move || {
        let path = version_path.clone();
        async move { api::get::<ProfileVersion>(&path).await }
    });
    let staged = RwSignal::new(None::<SelectionProfile>);
    Effect::new(move || {
        if let Some(Ok(version)) = saved.get()
            && staged.get_untracked().is_none()
        {
            staged.set(Some(version.profile));
        }
    });
    // A link to a group, such as a plan's `?group=`, opens the selection at it.
    let (list_query, set_list_query) = use_list_query();
    if let Some(group) = use_query_map().read_untracked().get("group")
        && list_query.get_untracked().selected.is_none()
    {
        let mut q = list_query.get_untracked();
        q.selected = Some(group);
        set_list_query.run(q);
    }
    let audit_outcome = RwSignal::new(None::<Result<Job, String>>);
    let reaudit = ReAudit::new(
        r.source_set.clone(),
        r.dat_revision_sha256.clone(),
        audit_outcome,
    );
    let newer = RwSignal::new(None::<Result<Job, String>>);
    let start_newer = move |_| {
        let r = record.get_value();
        let request = StartSelection {
            source_set: r.source_set,
            revision: Some(r.dat_revision_sha256),
            profile: r.profile.name,
            version: Some(r.latest_profile_version),
        };
        leptos::task::spawn_local(async move {
            newer.set(Some(api::post::<_, Job>("/selections", &request).await));
        });
    };
    let summary = r.summary;
    let set_href = format!("/source-sets/{}", api::segment(&r.source_set));
    let profile_href = format!("/profiles/{}", api::segment(&r.profile.name));
    let what_if = r.library_folders.is_empty();
    let promote_href = format!("/libraries/new?selection={id}");

    view! {
        <h1>{format!("Selection #{id}")}</h1>
        <dl class="facts">
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{r.source_set.clone()}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                {r.dat_revision_version.clone()} " "
                <code class="hash">{r.dat_revision_sha256.clone()}</code>
            </dd>
            <dt>"Selection profile"</dt>
            <dd>
                <A href=profile_href>{r.profile.name.clone()}</A>
                {format!(", version {}", r.profile.version)}
            </dd>
            <dt>"System"</dt>
            <dd>
                {r.system.clone()} " · "
                {if r.system_settings {
                    "your system settings applied"
                } else {
                    "only the global settings applied"
                }}
            </dd>
            <dt>"Made"</dt>
            <dd>{when(&r.created_at)}</dd>
            <dt>"Library folders"</dt>
            <dd>
                {if what_if {
                    view! { "None: this is a what-if selection" }.into_any()
                } else {
                    r.library_folders
                        .iter()
                        .enumerate()
                        .map(|(i, folder)| {
                            view! {
                                {(i > 0).then_some(", ")}
                                <A href=format!("/libraries/{folder}")>{format!("Library folder #{folder}")}</A>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </dd>
        </dl>
        {(r.latest_profile_version > r.profile.version)
            .then(|| {
                view! {
                    <div class="notice" role="status">
                        <p>
                            {format!(
                                "Version {} of this selection profile is available. This selection was made with version {}.",
                                r.latest_profile_version,
                                r.profile.version,
                            )}
                        </p>
                        <button type="button" on:click=start_newer>
                            {format!("Make a selection with version {}", r.latest_profile_version)}
                        </button>
                    </div>
                }
            })}
        {(!pruned)
            .then(|| {
                view! {
                    <div class="actions">
                        <A href=promote_href attr:class="button">
                            "Create library folder from this"
                        </A>
                    </div>
                    <PlanABuild selection=record.get_value() />
                }
            })}
        {move || {
            newer
                .get()
                .map(|outcome| match outcome {
                    Ok(job) => view! { <SelectionQueued job=job.id /> }.into_any(),
                    Err(message) => view! { <Problem message /> }.into_any(),
                })
        }}

        {pruned
            .then(|| {
                view! {
                    <ul class="counts">
                        <li class="count">
                            <span class="n">{summary.groups}</span>
                            <span class="what">"Groups"</span>
                        </li>
                        <li class="count verified">
                            <span class="n">{summary.winners}</span>
                            <span class="what">"Selected"</span>
                        </li>
                        <li class="count review">
                            <span class="n">{summary.needs_review()}</span>
                            <span class="what">"Needs review"</span>
                        </li>
                    </ul>
                    <p class="note">
                        "This selection is pruned: only its summary is kept, so its releases are gone."
                    </p>
                }
            })}

        {move || match data.get() {
            None => view! { <Loading what="the groups" /> }.into_any(),
            Some(None) => ().into_any(),
            Some(Some(Err(message))) => view! { <Problem message /> }.into_any(),
            Some(Some(Ok(loaded))) => {
                view! {
                    <AvailabilityNote audit=loaded.availability.audit reaudit />
                    <StaleGroups index=loaded.index.clone() staged />
                    <Groups loaded staged />
                    <StagedChoices record staged saved=Signal::derive(move || saved.get().and_then(Result::ok)) />
                }
                    .into_any()
            }
        }}

        {move || match diff.get() {
            None | Some(None) => ().into_any(),
            Some(Some(diff)) => {
                let count = diff.changes.len();
                view! {
                    <section aria-labelledby="changes-title">
                        <h2 id="changes-title">"Changes"</h2>
                        <p class="lede">
                            "Since "
                            <A href=format!("/selections/{}", diff.from)>{format!("selection #{}", diff.from)}</A>
                            {format!(": {count} {} changed role.", if count == 1 { "release" } else { "releases" })}
                        </p>
                        <ChangesTable id="changes" changes=diff.changes />
                    </section>
                }
                    .into_any()
            }
        }}
    }
}

/// Where the availability comes from, or the offer to audit when nothing says.
#[component]
fn AvailabilityNote(audit: Option<i64>, reaudit: ReAudit) -> impl IntoView {
    match audit {
        Some(audit) => view! {
            <p class="lede">
                "What you have is from "
                <A href=format!("/audits/{audit}")>{format!("audit #{audit}")}</A>
                ", the latest audit of this source set against this DAT revision. A kept release that is missing stays missing: to fall back to an alternative you have, check it."
            </p>
        }
        .into_any(),
        None => view! {
            <div class="notice" role="status">
                <p>"Not audited against this revision, so it is unknown which releases you have."</p>
                <button type="button" on:click=move |_| reaudit.start()>
                    "Audit against this revision"
                </button>
            </div>
            {reaudit.outcome()}
        }
        .into_any(),
    }
}

/// The game groups: a strip of counts that narrows the list and confirms every unresolved pick at once, and the
/// list, with the open group's releases beside it.
#[component]
fn Groups(loaded: Loaded, staged: RwSignal<Option<SelectionProfile>>) -> impl IntoView {
    let index = loaded.index.clone();
    let items: Vec<GroupItem> = index
        .selection
        .groups
        .iter()
        .map(|g| GroupItem::new(g, &loaded.availability))
        .collect();
    let mut counts = vec![0usize; GroupStatus::ALL.len()];
    for item in &items {
        counts[item.status.rank()] += 1;
    }
    let rows = Signal::stored(Arc::new(items));
    let statuses = present(rows.get_untracked().iter().map(|i| i.status));
    let legend_statuses = statuses.clone();
    let only = RwSignal::new(None::<GroupStatus>);
    let keep = Signal::derive(move || -> Keep<GroupItem> {
        only.get()
            .map(|status| Arc::new(move |i: &GroupItem| i.status == status) as _)
    });
    let system = StoredValue::new(index.selection.system.clone());
    let groups = StoredValue::new(index.selection.groups.clone());
    let unresolved = counts[GroupStatus::Unresolved.rank()];
    // How many unresolved groups have no staged choice yet.
    let open = move || {
        staged.with(|p| {
            p.as_ref().map_or(0, |p| {
                groups.with_value(|gs| {
                    gs.iter()
                        .filter(|g| {
                            g.unresolved
                                && p.choices
                                    .get(&system.get_value())
                                    .is_none_or(|c| !c.contains_key(&g.group))
                        })
                        .count()
                })
            })
        })
    };
    let confirm_all = move |_| {
        staged.update(|p| {
            if let Some(p) = p {
                groups.with_value(|gs| confirm_unresolved(p, &system.get_value(), gs));
            }
        })
    };
    let availability = StoredValue::new(loaded.availability.clone());
    let detail = Callback::new(move |item: GroupItem| -> AnyView {
        match index.group(&item.name) {
            Some(g) => {
                let a = availability.get_value();
                view! {
                    <GroupPanel
                        group=g.clone()
                        system=system.get_value()
                        availability=a.games
                        claimed=a.claimed
                        staged
                    />
                }
                .into_any()
            }
            None => ().into_any(),
        }
    });

    view! {
        <section aria-labelledby="groups-title">
            <h2 id="groups-title">"Game groups"</h2>
            <p class="lede">
                "Open a group to see its releases, the rule that decided each, and to choose the releases you want. Your choices are staged until you save them."
            </p>
            <div class="count-strip" role="group" aria-label="Show groups by status">
                <ul class="counts">
                    {statuses
                        .into_iter()
                        .map(|status| {
                            let look = status.look();
                            let n = counts[status.rank()];
                            let tone = if look.tone == Tone::Review { "count review" } else { "count" };
                            view! {
                                <li>
                                    <Tooltip label=look.hint>
                                        <button
                                            type="button"
                                            class=tone
                                            aria-pressed=move || (only.get() == Some(status)).to_string()
                                            on:click=move |_| {
                                                only.update(|o| {
                                                    *o = if *o == Some(status) { None } else { Some(status) }
                                                })
                                            }
                                        >
                                            <span class="n">{n}</span>
                                            <span class="what">{look.label}</span>
                                        </button>
                                    </Tooltip>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
                {(unresolved > 0)
                    .then(|| {
                        view! {
                            <div class="actions">
                                <button
                                    type="button"
                                    disabled=move || open() == 0
                                    on:click=confirm_all
                                >
                                    {move || match open() {
                                        0 => "Every unresolved pick is confirmed".to_string(),
                                        1 => "Apply to all: confirm the 1 unresolved pick".to_string(),
                                        n => format!("Apply to all: confirm the {n} unresolved picks"),
                                    }}
                                </button>
                                <span class="note small">
                                    "Confirming keeps the selection's pick, or the releases you checked, as your choice."
                                </span>
                            </div>
                        }
                    })}
            </div>
            <List
                id="groups"
                label="Game groups"
                rows
                empty="This selection has no groups."
                placeholder="Filter by group, release or rule"
                keep
                detail
                legend=move || view! { <Legend statuses=legend_statuses.clone() list="groups" /> }
            />
        </section>
    }
}

/// Choices of yours for groups that are gone from this DAT Revision: stale, kept until you drop them.
#[component]
fn StaleGroups(
    index: Arc<SelectionIndex>,
    staged: RwSignal<Option<SelectionProfile>>,
) -> impl IntoView {
    let stale = index.selection.stale_choices.clone();
    let system = index.selection.system.clone();
    (!stale.is_empty()).then(|| {
        view! {
            <section aria-labelledby="stale-title">
                <h2 id="stale-title">"Stale choices"</h2>
                <p class="lede">
                    "These groups are gone from this DAT revision. Your choices for them are kept until you drop them."
                </p>
                <ul class="plain">
                    {stale
                        .into_iter()
                        .map(|(group, titles)| {
                            let (s, g) = (system.clone(), group.clone());
                            let dropped = move || {
                                staged
                                    .with(|p| {
                                        p.as_ref()
                                            .is_some_and(|p| {
                                                p.choices.get(&s).is_none_or(|c| !c.contains_key(&g))
                                            })
                                    })
                            };
                            let system = system.clone();
                            view! {
                                <li>
                                    <strong>{group.clone()}</strong>
                                    {format!(": {}", titles.into_iter().collect::<Vec<_>>().join(", "))}
                                    " "
                                    <button
                                        type="button"
                                        disabled=dropped
                                        on:click=move |_| {
                                            staged
                                                .update(|p| {
                                                    if let Some(p) = p {
                                                        p.reset_choices(&system, Some(&group));
                                                    }
                                                })
                                        }
                                    >
                                        "Drop this choice"
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </section>
        }
    })
}

/// An edit of the staged Profile for one group.
type GroupEdit = dyn FnOnce(&mut SelectionProfile, &str, &SelectionGroup);

/// One group: its releases with checkboxes for your choice, and what the choice does.
#[component]
fn GroupPanel(
    group: SelectionGroup,
    system: String,
    availability: std::collections::BTreeMap<String, GameStatus>,
    claimed: std::collections::BTreeSet<String>,
    staged: RwSignal<Option<SelectionProfile>>,
) -> impl IntoView {
    let group = StoredValue::new(group);
    let system = StoredValue::new(system);
    let g = group.get_value();
    let checked_now = move || {
        staged.with(|p| {
            p.as_ref()
                .map(|p| checked(p, &system.get_value(), &group.get_value()))
        })
    };
    let has_choice = move || {
        staged.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.choices
                    .get(&system.get_value())
                    .is_some_and(|c| c.contains_key(&group.get_value().group))
            })
        })
    };
    let ready = move || staged.with(Option::is_some);
    let update = move |f: Box<GroupEdit>| {
        staged.update(|p| {
            if let Some(p) = p {
                f(p, &system.get_value(), &group.get_value());
            }
        });
    };
    let picks: Vec<String> = g.picks().into_iter().collect();
    let explanation = match &g.choice {
        Some(choice) if choice.status == ChoiceStatus::Applied && g.unresolved => {
            unresolved_explanation(&g)
        }
        Some(choice) => match choice.status {
            ChoiceStatus::Applied => format!(
                "Your choice decides this group. Without it the selection would be: {}.",
                list_or_none(&picks)
            ),
            ChoiceStatus::MatchesDefault => {
                "Your choice matches the default now, so you can drop it and nothing changes."
                    .to_string()
            }
            ChoiceStatus::Conflicting => format!(
                "Your choice includes a release your settings exclude: {}. It is not applied: the default applies until you change your choice or your settings.",
                choice.problems.join(", ")
            ),
            ChoiceStatus::Stale => format!(
                "Your choice names a release that is gone from this DAT revision: {}. It is not applied, and it is kept until you change it.",
                choice.problems.join(", ")
            ),
        },
        None if g.unresolved => unresolved_explanation(&g),
        None => "The selection's default applies to this group.".to_string(),
    };
    let status = g.choice.as_ref().map(|c| c.status);
    let unresolved = g.engine_unresolved();
    let releases: Vec<(String, Role, String)> = g
        .winners
        .iter()
        .map(|w| (w.title.clone(), Role::Winner, rule_text(&w.rule)))
        .chain(
            g.alternatives
                .iter()
                .map(|a| (a.title.clone(), Role::Alternative, rule_text(&a.rule))),
        )
        .chain(g.exclusions.iter().map(|e| {
            (
                e.title.clone(),
                Role::Exclusion,
                format!("{}: {}", e.reason, rule_text(&e.rule)),
            )
        }))
        .collect();

    view! {
        <p>
            {status.map(|s| {
                let review = matches!(s, ChoiceStatus::Stale | ChoiceStatus::Conflicting);
                view! { <span class=if review { "state review" } else { "tag" }>{choice_label(s)}</span> " " }
            })}
            {g.unresolved.then(|| view! { <span class="state review">"Unresolved"</span> " " })}
            {g.drift.then(|| view! { <span class="tag">"Coverage drift"</span> " " })}
            {explanation}
        </p>
        {g.drift
            .then(|| {
                view! {
                    <p class="note small">
                        "Coverage drift: a release in this group has no metadata entry, so the selection may know less about it."
                    </p>
                }
            })}
        <fieldset class="releases" disabled=move || !ready()>
            <legend>"Candidate releases: check the ones to keep"</legend>
            <ul class="plain">
                {releases
                    .into_iter()
                    .map(|(title, role, rule)| {
                        let t = StoredValue::new(title.clone());
                        let is_checked = move || {
                            checked_now().is_some_and(|c| c.contains(&t.get_value()))
                        };
                        let avail = availability.get(&title).copied();
                        let claimed_only = claimed.contains(&title);
                        let excluded = role == Role::Exclusion;
                        view! {
                            <li class="release">
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        disabled=excluded
                                        prop:checked=is_checked
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            update(
                                                Box::new(move |p, system, g| toggle(p, system, g, &t.get_value(), on)),
                                            );
                                        }
                                    />
                                    {title.clone()}
                                </label>
                                " "
                                {role_view(role)}
                                " "
                                {availability_view(avail, claimed_only)}
                                <span class="note small rule">{rule}</span>
                                {excluded
                                    .then(|| {
                                        view! {
                                            " "
                                            <button
                                                type="button"
                                                class="small"
                                                on:click=move |_| {
                                                    update(
                                                        Box::new(move |p, system, g| {
                                                            include_and_check(p, system, g, &t.get_value())
                                                        }),
                                                    )
                                                }
                                            >
                                                "Include and check"
                                            </button>
                                        }
                                    })}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </fieldset>
        <div class="actions">
            {unresolved
                .then(|| {
                    view! {
                        <button
                            type="button"
                            disabled=move || !ready() || has_choice()
                            on:click=move |_| update(Box::new(confirm))
                        >
                            "Confirm these releases"
                        </button>
                    }
                })}
            <button
                type="button"
                disabled=move || !has_choice()
                on:click=move |_| {
                    update(Box::new(|p, system, g| p.reset_choices(system, Some(&g.group))))
                }
            >
                {if status == Some(ChoiceStatus::MatchesDefault) {
                    "Drop your choice"
                } else {
                    "Reset to default choices for this group"
                }}
            </button>
        </div>
        <p class="note small">
            "Including an excluded release adds an include override for it to this system's settings. Leaving every box unchecked leaves the group out."
        </p>
    }
}

fn list_or_none(titles: &[String]) -> String {
    if titles.is_empty() {
        "nothing".to_string()
    } else {
        titles.join(", ")
    }
}

/// Your staged choices: how many, and the buttons to save or reset them.
#[component]
fn StagedChoices(
    record: StoredValue<SelectionRecord>,
    staged: RwSignal<Option<SelectionProfile>>,
    saved: Signal<Option<ProfileVersion>>,
) -> impl IntoView {
    let r = record.get_value();
    let system = StoredValue::new(r.system.clone());
    let count = Memo::new(move |_| {
        let saved = saved.get()?;
        staged.with(|p| {
            p.as_ref()
                .map(|p| staged_changes(&saved.profile, p, &system.get_value()))
        })
    });
    let choices_in = move |whole: bool| {
        staged.with(|p| {
            p.as_ref().map_or(0, |p| {
                if whole {
                    p.choices.values().map(|c| c.len()).sum()
                } else {
                    p.choices.get(&system.get_value()).map_or(0, |c| c.len())
                }
            })
        })
    };
    let outcome = RwSignal::new(None::<Result<SavedProfile, String>>);
    let saving = RwSignal::new(false);
    let save = move |_| {
        let Some(profile) = staged.get_untracked() else {
            return;
        };
        let r = record.get_value();
        let request = SaveProfile {
            profile,
            base_version: Some(r.profile.version),
            queue: vec![StartSelectionFor {
                source_set: r.source_set.clone(),
                revision: Some(r.dat_revision_sha256.clone()),
            }],
        };
        let path = format!("/profiles/{}/versions", api::segment(&r.profile.name));
        saving.set(true);
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, SavedProfile>(&path, &request).await));
            saving.set(false);
        });
    };
    let discard = move |_| {
        if let Some(saved) = saved.get_untracked() {
            staged.set(Some(saved.profile));
        }
        outcome.set(None);
    };
    let next = r.profile.version + 1;

    view! {
        <section class="staged" aria-labelledby="staged-title">
            <h2 id="staged-title">"Your choices"</h2>
            <p aria-live="polite">
                {move || match count.get() {
                    None => "Loading your selection profile…".to_string(),
                    Some(0) => "Nothing staged. Check releases in a group to stage a choice.".to_string(),
                    Some(1) => "1 change staged.".to_string(),
                    Some(n) => format!("{n} changes staged."),
                }}
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="primary"
                    disabled=move || saving.get() || count.get().unwrap_or(0) == 0
                    on:click=save
                >
                    {format!("Save as version {next} and make a selection")}
                </button>
                <button type="button" disabled=move || count.get().unwrap_or(0) == 0 on:click=discard>
                    "Discard staged changes"
                </button>
                <button
                    type="button"
                    disabled=move || choices_in(false) == 0
                    on:click=move |_| {
                        staged
                            .update(|p| {
                                if let Some(p) = p {
                                    p.reset_choices(&system.get_value(), None);
                                }
                            })
                    }
                >
                    "Reset to default choices for this system"
                </button>
                <button
                    type="button"
                    disabled=move || choices_in(true) == 0
                    on:click=move |_| {
                        staged
                            .update(|p| {
                                if let Some(p) = p {
                                    p.reset_all_choices();
                                }
                            })
                    }
                >
                    "Reset to default choices for the whole profile"
                </button>
            </div>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(saved) => {
                            let message = if saved.created {
                                format!("Saved version {}.", saved.version.version)
                            } else {
                                format!(
                                    "Nothing changed, so no new version was saved; version {} stays the newest.",
                                    saved.version.version,
                                )
                            };
                            view! {
                                <p class="done" role="status">{message}</p>
                                {saved
                                    .jobs
                                    .into_iter()
                                    .map(|job| view! { <SelectionQueued job=job.id /> })
                                    .collect_view()}
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}
