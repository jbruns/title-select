//! A Library Folder's latest plan, for you to review and approve: a build plan, or a cleanup plan. Both are reviewed
//! with the shared plan review (`parts::plan`); a build plan's conflicts carry your choice, and setting one, for a
//! row, the rows you check or every item of a kind, plans again.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::Job;
use title_select_api::build::{
    ApprovePlan, BlockerKind, CleanupAction, Plan, PlanChoices, PlanContent, RETENTION, RemovalKind,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::build_model::{
    ConflictChoice, blocker_label, cause_label, cleanup_action_label, group_href, item_change,
    keep_reason_label, plan_kinds, restore_kind_label, with_conflict_choices,
};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::parts::icon::Glyph;
use crate::parts::list::{Clip, Column, ListRow, Width};
use crate::parts::plan::{Change, PlanReview, PlanRow, apply_to_all, set_actions};
use crate::parts::plan_model::{apply_choice, offered_by_any};
use crate::parts::status::{BuildKind, Status, StatusIcon};
use crate::screens::library_folders::{BuildProgress, Planner, use_planner};

/// The plan `plan` of the folder `folder`, loaded. `refresh` reloads the folder once you approve.
#[component]
pub fn LibraryPlan(plan: i64, folder: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<Plan>(&format!("/plans/{plan}")).await }
    });
    move || match loaded.get() {
        None => view! { <Loading what="the plan" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(plan)) => view! { <PlanView plan folder refresh /> }.into_any(),
    }
}

/// Your choices for a build plan's conflicts, while it waits on your approval: each change plans again.
#[derive(Clone, Copy)]
struct Chooser {
    choices: StoredValue<PlanChoices>,
    planner: Planner,
    folder: i64,
    selection: i64,
    /// While you approve the plan or it is planned again: your choices wait.
    busy: Signal<bool>,
}

// A plan's rows share its one chooser.
impl PartialEq for Chooser {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Chooser {
    /// Sets `choice` for each of `rows` that offers it, plans again if that changed any, and, `telling`, says what
    /// it did.
    fn set<'a>(
        self,
        rows: impl IntoIterator<Item = &'a ItemRow>,
        choice: ConflictChoice,
        telling: bool,
    ) {
        if self.busy.get_untracked() {
            return;
        }
        let mut picked = BTreeMap::new();
        let items = rows
            .into_iter()
            .map(|r| (r.key.as_str(), r.chosen, r.offers.as_slice()));
        let outcome = apply_choice(items, choice, &mut picked);
        if outcome.changed > 0 {
            let next = self
                .choices
                .with_value(|c| with_conflict_choices(c, &picked));
            self.planner.start(self.folder, Some(self.selection), next);
        }
        if telling {
            let said = outcome.message(choice.label());
            self.planner.said.set(Some((self.folder, said)));
        }
    }

    /// The choice of `row`, as a `<select>`.
    fn select(self, row: &ItemRow) -> AnyView {
        let row = row.clone();
        let chosen = row.chosen;
        let options = row
            .offers
            .iter()
            .map(|c| view! { <option value=c.label() selected=*c == chosen>{c.label()}</option> })
            .collect_view();
        view! {
            <select
                class="choice"
                aria-label=format!("What to do with {}", row.key)
                disabled=self.busy
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    if let Some(c) = row.offers.iter().copied().find(|c| c.label() == value) {
                        self.set([&row], c, false);
                    }
                }
            >
                {options}
            </select>
        }
        .into_any()
    }
}

/// One item of a plan: a file it writes or skips, a built file it removes, or what a cleanup does with an entry.
#[derive(Clone, PartialEq)]
struct ItemRow {
    kind: BuildKind,
    /// What else it does, such as setting a file aside.
    also: Option<BuildKind>,
    key: String,
    game: String,
    current: String,
    proposed: String,
    /// What the detail says of it besides, by label.
    facts: Vec<(&'static str, String)>,
    offers: Vec<ConflictChoice>,
    chosen: ConflictChoice,
    chooser: Option<Chooser>,
}

impl ItemRow {
    /// A row named `key`, which is what it is now, becoming `proposed`. `plan_rows` sets its kinds.
    fn new(key: &str, game: &str, proposed: impl Into<String>) -> Self {
        ItemRow {
            kind: BuildKind::Unchanged,
            also: None,
            key: key.to_string(),
            game: game.to_string(),
            current: key.to_string(),
            proposed: proposed.into(),
            facts: Vec::new(),
            offers: Vec::new(),
            chosen: ConflictChoice::Leave,
            chooser: None,
        }
    }

    fn fact(mut self, label: &'static str, text: impl Into<Option<String>>) -> Self {
        self.facts.extend(text.into().map(|t| (label, t)));
        self
    }

    /// The choice, where you may change it.
    fn choosing(&self) -> Option<Chooser> {
        self.chooser.filter(|_| !self.offers.is_empty())
    }
}

/// The plan's rows: a build plan's items and removals, or a cleanup's entries, kept files put back and repacks.
fn plan_rows(content: &PlanContent, chooser: Option<Chooser>) -> Vec<ItemRow> {
    let Some(cleanup) = &content.cleanup else {
        let items = content.items.iter().map(|i| {
            let (current, proposed) = item_change(i);
            ItemRow {
                offers: ConflictChoice::offered(i),
                chosen: ConflictChoice::of(i, &content.choices),
                chooser,
                current,
                ..ItemRow::new(&i.target, &i.game, proposed)
                    .fact("Why", i.reason.clone())
                    .fact(
                        "Kept aside first",
                        i.keep
                            .as_ref()
                            .map(|k| format!("{} ({})", k.name, keep_reason_label(k.reason))),
                    )
                    .fact(
                        "Converts",
                        i.converts.as_ref().map(|c| {
                            format!(
                                "{}, stored the other way, kept aside once this is written",
                                c.name
                            )
                        }),
                    )
                    .fact("ROMs", i.roms.len().to_string())
            }
        });
        let removals = content.removals.iter().map(|r| {
            let to = match r.kind {
                RemovalKind::Keep => "kept files",
                RemovalKind::Forget => "forgotten",
                RemovalKind::Leave => "left as it is",
            };
            ItemRow::new(&r.target, &r.game, to).fact("Why", r.reason.clone())
        });
        return with_kinds(content, items.chain(removals));
    };
    let entries = cleanup.entries.iter().filter_map(|e| {
        let action = e.action?;
        let to = match action {
            CleanupAction::KeepAside => "kept files",
            CleanupAction::AddAddition => "additions set",
            CleanupAction::Acknowledge => "left as it is",
            CleanupAction::Adopt => e.target.as_deref().unwrap_or(&e.name),
        };
        let game = e.game.as_deref().unwrap_or_default();
        Some(ItemRow::new(&e.name, game, to).fact("Does", cleanup_action_label(action).to_string()))
    });
    let restores = cleanup.restores.iter().map(|r| {
        let kept = format!("{RETENTION}/{}/{}", r.kept.generation, r.kept.kept_name);
        let game = r.game.as_deref().unwrap_or_default();
        ItemRow::new(&kept, game, &r.target).fact("Does", restore_kind_label(r.kind).to_string())
    });
    let repacks = cleanup.repacks.iter().map(|r| {
        let does = "Repack as TorrentZip, keeping the file as it is aside";
        ItemRow::new(&r.target, &r.game, &r.target).fact("Does", does.to_string())
    });
    with_kinds(content, entries.chain(restores).chain(repacks))
}

/// `rows`, listed as `plan_kinds` lists them, with their kinds.
fn with_kinds(content: &PlanContent, rows: impl Iterator<Item = ItemRow>) -> Vec<ItemRow> {
    rows.zip(plan_kinds(content))
        .map(|(row, (kind, also))| ItemRow { kind, also, ..row })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemCol {
    Kind,
    Game,
    Change,
    Choice,
}

impl PlanRow for ItemRow {
    type Kind = BuildKind;
    fn kind(&self) -> BuildKind {
        self.kind
    }

    fn also(&self) -> Option<BuildKind> {
        self.also
    }
}

impl ListRow for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Kind,
        ItemCol::Game,
        ItemCol::Change,
        ItemCol::Choice,
    ];

    fn column(col: ItemCol) -> Column {
        match col {
            ItemCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the plan does with the file"),
            ItemCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 10.0,
                    share: 1,
                },
            )
            .sortable(),
            ItemCol::Change => Column::new(
                "change",
                "Current → proposed",
                Width::Flex {
                    min: 16.0,
                    share: 2,
                },
            )
            .sortable(),
            ItemCol::Choice => Column::new("choice", "Choice", Width::Fixed(11.0))
                .sortable()
                .hint("What you choose to do with a conflict"),
        }
    }

    fn key(&self) -> String {
        self.key.clone()
    }

    fn title(&self) -> String {
        [&self.game, &self.key]
            .into_iter()
            .find(|t| !t.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {}",
            self.game,
            self.current,
            self.proposed,
            self.kind.look().label
        )
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        match col {
            ItemCol::Kind => view! { <StatusIcon status=self.kind tab_stop=false /> }.into_any(),
            ItemCol::Game => view! { <Clip text=self.game.clone() /> }.into_any(),
            ItemCol::Change => {
                view! { <Change current=self.current.clone() proposed=self.proposed.clone() /> }
                    .into_any()
            }
            ItemCol::Choice => (!self.offers.is_empty())
                .then_some(self.chosen.label())
                .into_any(),
        }
    }

    fn compare(&self, other: &Self, col: ItemCol) -> Ordering {
        match col {
            ItemCol::Kind => self.kind.rank().cmp(&other.kind.rank()),
            ItemCol::Game => self.game.cmp(&other.game),
            ItemCol::Change => self.current.cmp(&other.current),
            ItemCol::Choice => self
                .offers
                .is_empty()
                .cmp(&other.offers.is_empty())
                .then(self.chosen.label().cmp(other.chosen.label())),
        }
    }
}

/// Why and how: the item's kind, what it is now and becomes, what else is known of it, and your choice.
fn item_view(row: ItemRow) -> AnyView {
    let look = row.kind.look();
    let choice = (!row.offers.is_empty()).then(|| {
        let now = row
            .choosing()
            .map_or(row.chosen.label().into_any(), |c| c.select(&row));
        view! { <dt>"Choice"</dt> <dd>{now}</dd> }
    });
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd>{look.label} " — " {look.hint}</dd>
            <dt>"Now"</dt>
            <dd>
                <code class="path">{row.current.clone()}</code>
            </dd>
            <dt>"Proposed"</dt>
            <dd>
                <code class="path">{row.proposed.clone()}</code>
            </dd>
            {row
                .facts
                .iter()
                .map(|(label, text)| view! {
                    <dt>{*label}</dt>
                    <dd>{text.clone()}</dd>
                })
                .collect_view()}
            {choice}
        </dl>
    }
    .into_any()
}

#[component]
fn PlanView(plan: Plan, folder: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let id = plan.id;
    let selection = plan.content.selection;
    let digest = StoredValue::new(plan.digest.clone());
    let live = use_live();
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let approve = move |_| {
        busy.set(true);
        let request = ApprovePlan {
            digest: digest.get_value(),
        };
        leptos::task::spawn_local(async move {
            let approved = api::post::<_, Job>(&format!("/plans/{id}/approval"), &request).await;
            busy.set(false);
            match approved {
                // The plan, loaded again, names its build job, so its progress outlives this view.
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let approvable = plan.approvable() && plan.approval.is_none();
    let writes = plan.summary.copy + plan.summary.repack;
    // Your choices are part of the plan: changing one plans again, and you review the new plan.
    let choices = plan.content.choices.clone();
    let planner = use_planner();
    let replan = move |choices: PlanChoices| planner.start(folder, Some(selection), choices);
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
    // As the strip counts it: replacements, conflicts kept aside and removals that keep their file.
    let removes = plan.summary.keep;
    let cleanup = plan.content.cleanup.as_ref();
    let is_cleanup = cleanup.is_some();
    // A cleanup's entries it leaves as they are aren't its items.
    let left = cleanup.map_or(0, |c| {
        c.entries.iter().filter(|e| e.action.is_none()).count()
    });
    let approve_label = match (is_cleanup, removes) {
        (true, _) => "Approve the cleanup".to_string(),
        (false, 0) => format!("Approve and build {writes} files"),
        (false, n) => format!("Approve and build {writes} files, setting {n} aside"),
    };
    let heading = if is_cleanup {
        format!("Cleanup plan #{id}")
    } else {
        format!("Build plan #{id}")
    };
    let state = match &plan.approval {
        Some(a) => format!("Approved by {} · {}", a.actor, when(&a.at)),
        None => "Waiting on your approval".to_string(),
    };
    let replanning = planner.pending(folder);
    let chooser = open.then(|| Chooser {
        choices: StoredValue::new(choices.clone()),
        planner,
        folder,
        selection,
        busy: Signal::derive(move || busy.get() || replanning.get()),
    });
    let rows: Arc<Vec<ItemRow>> = Arc::new(plan_rows(&plan.content, chooser));
    let offered = offered_by_any(rows.iter().map(|r| r.offers.as_slice()));
    let actions = match chooser {
        Some(chooser) => set_actions(
            [
                (Glyph::CircleOff, ConflictChoice::Leave),
                (Glyph::FolderCheck, ConflictChoice::Adopt),
                (Glyph::Archive, ConflictChoice::KeepAside),
            ],
            &offered,
            ConflictChoice::label,
            move |rows: Vec<ItemRow>, c| chooser.set(&rows, c, true),
        ),
        None => Vec::new(),
    };
    let apply = {
        let rows = rows.clone();
        Callback::new(move |kind: BuildKind| match chooser {
            Some(chooser) => {
                let of_kind: Vec<ItemRow> = rows.iter().filter(|r| r.is(kind)).cloned().collect();
                let choices = offered_by_any(of_kind.iter().map(|r| r.offers.as_slice()));
                apply_to_all(kind, choices, ConflictChoice::label, move |c| {
                    chooser.set(&of_kind, c, true)
                })
            }
            None => ().into_any(),
        })
    };
    let empty = if is_cleanup {
        "The cleanup changes nothing."
    } else {
        "The plan has no files."
    };

    view! {
        <section aria-labelledby="latest-plan-title">
            <h2 id="latest-plan-title">{heading}</h2>
            <p class="note">{state}</p>
            <details class="facts-more">
                <summary>"What it was planned from"</summary>
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
                    {plan.job.map(|j| view! {
                        <dt>"Build job"</dt>
                        <dd><A href=format!("/activity?row={j}")>{format!("#{j}")}</A></dd>
                    })}
                </dl>
            </details>
            {(!plan.content.notes.is_empty())
                .then(|| {
                    view! {
                        <details class="choices">
                            <summary>"Stored differently from the storage policy"</summary>
                            <ul class="plain">
                                {plan.content.notes.iter().map(|n| view! { <li>{n.clone()}</li> }).collect_view()}
                            </ul>
                        </details>
                    }
                })}
            {(!plan.content.blockers.is_empty())
                .then(|| {
                    view! {
                        <div class="notice" role="status">
                            <p>"You can't approve this plan until these are fixed:"</p>
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
            {partial}
            {(left > 0).then(|| view! {
                <p class="note small">
                    {format!(
                        "{left} other entries are left as they are. If anything in the folder changes before you \
                         approve, you're asked to plan again."
                    )}
                </p>
            })}
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
            {plan
                .job
                .map(|job| {
                    move || {
                        live.book
                            .with(|b| b.get(job).is_some())
                            .then(|| view! { <BuildProgress job /> })
                    }
                })}
            {move || problem.get().map(|message| view! { <Problem message /> })}
            {(!plan.changes.is_empty())
                .then(|| {
                    view! {
                        <details class="choices">
                            <summary>{format!("What changed since the current build ({})", plan.changes.len())}</summary>
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
                        </details>
                    }
                })}
            <PlanReview
                id="plan-items"
                label="Plan items"
                rows=Signal::stored(rows)
                empty
                placeholder="Filter by game, file, source or kind"
                detail=Callback::new(item_view)
                actions
                apply
                message=planner.said(folder)
            />
        </section>
    }
}
