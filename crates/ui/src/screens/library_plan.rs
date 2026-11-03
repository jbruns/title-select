//! A Library Folder's latest plan, for you to review and approve: a build plan, or a cleanup plan. The folder page
//! shows it in its Plan tab through `PlanReview` alone, so that the shared plan review can take its place.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::Job;
use title_select_api::build::{
    ApprovePlan, BlockerKind, Cleanup, ItemKind, KeepReason, Plan, PlanChoices, PlanItem,
    PlanSummary, RETENTION, RemovalKind,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::build_model::{
    blocker_label, cause_label, cleanup_action_label, group_href, keep_reason_label,
    removal_kind_label, restore_kind_label, with_adopt, with_keep_aside,
};
use crate::events::use_live;
use crate::library_model::kind_counts;
use crate::model::{short_sha, when};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Clip, Column, Keep, List, ListRow, Width};
use crate::parts::status::{BuildKind, Status, StatusBadge, StatusIcon};
use crate::screens::library_folders::BuildProgress;

/// The plan `plan`, loaded. `replan` plans the same selection again with other choices; `refresh` reloads the folder
/// once you approve.
#[component]
pub fn PlanReview(
    plan: i64,
    replan: Callback<(i64, PlanChoices)>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<Plan>(&format!("/plans/{plan}")).await }
    });
    move || match loaded.get() {
        None => view! { <Loading what="the plan" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(plan)) => view! { <PlanView plan replan refresh /> }.into_any(),
    }
}

/// How many items of each kind the plan holds.
fn kind_strip(summary: &PlanSummary) -> AnyView {
    view! {
        <ul class="counts kinds" aria-label="Items by kind">
            {kind_counts(summary)
                .into_iter()
                .map(|(kind, n)| {
                    view! {
                        <li class="count">
                            <span class="n">{n}</span>
                            <span class="what"><StatusBadge status=kind /></span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

/// Where an item's file comes from, or why it isn't written.
fn item_detail(item: &PlanItem) -> String {
    let kept = item.keep.as_ref().map(|k| {
        format!(
            "{} is kept aside first ({}); ",
            k.name,
            keep_reason_label(k.reason)
        )
    });
    let mut kept = kept.unwrap_or_default();
    if let Some(c) = &item.converts {
        kept.push_str(&format!(
            "converted from {}, stored the other way, which is set aside once this is written; ",
            c.name
        ));
    }
    if let Some(reason) = &item.reason {
        return format!("{kept}{reason}");
    }
    let mut paths: Vec<String> = item
        .sources
        .iter()
        .map(|s| match &s.member {
            Some(member) => format!("{} ({member})", s.path),
            None => s.path.clone(),
        })
        .collect();
    paths.dedup();
    format!("{kept}{}", paths.join(", "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemCol {
    Kind,
    Game,
    Target,
    Detail,
}

#[derive(Clone, PartialEq)]
struct ItemRow(PlanItem);

impl ItemRow {
    fn kind(&self) -> BuildKind {
        self.0.kind.into()
    }
}

impl ListRow for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Kind,
        ItemCol::Game,
        ItemCol::Target,
        ItemCol::Detail,
    ];

    fn column(col: ItemCol) -> Column {
        match col {
            ItemCol::Kind => Column::new("kind", "Does", Width::Fixed(4.0))
                .sortable()
                .hint("What the build does with the file"),
            ItemCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 12.0,
                    share: 2,
                },
            )
            .sortable(),
            ItemCol::Target => Column::new(
                "file",
                "File",
                Width::Flex {
                    min: 12.0,
                    share: 2,
                },
            )
            .sortable(),
            ItemCol::Detail => Column::new(
                "from",
                "From, or why not",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            ),
        }
    }

    fn key(&self) -> String {
        self.0.target.clone()
    }

    fn title(&self) -> String {
        self.0.game.clone()
    }

    fn search(&self) -> String {
        format!("{} {} {}", self.0.game, self.0.target, item_detail(&self.0))
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        match col {
            ItemCol::Kind => view! { <StatusIcon status=self.kind() tab_stop=false /> }.into_any(),
            ItemCol::Game => view! { <Clip text=self.0.game.clone() /> }.into_any(),
            ItemCol::Target => view! { <Clip text=self.0.target.clone() /> }.into_any(),
            ItemCol::Detail => view! { <Clip text=item_detail(&self.0) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: ItemCol) -> Ordering {
        match col {
            ItemCol::Kind => self.kind().rank().cmp(&other.kind().rank()),
            ItemCol::Game => self.0.game.cmp(&other.0.game),
            ItemCol::Target | ItemCol::Detail => self.0.target.cmp(&other.0.target),
        }
    }
}

fn item_view(row: ItemRow) -> AnyView {
    let i = row.0;
    let kind = BuildKind::from(i.kind);
    let detail = item_detail(&i);
    view! {
        <p>
            <StatusBadge status=kind />
        </p>
        <dl class="facts">
            <dt>"Game"</dt>
            <dd>{i.game.clone()}</dd>
            <dt>"File"</dt>
            <dd>
                <code class="path">{i.target.clone()}</code>
            </dd>
            <dt>"From, or why not"</dt>
            <dd>{detail}</dd>
            <dt>"ROMs"</dt>
            <dd>{i.roms.len()}</dd>
        </dl>
    }
    .into_any()
}

/// What an approved cleanup does: each entry you chose, each kept file put back and each file repacked.
fn cleanup_view(cleanup: &Cleanup) -> AnyView {
    let chosen: Vec<_> = cleanup
        .entries
        .iter()
        .filter_map(|e| e.action.map(|a| (a, e)))
        .map(|(a, e)| {
            let to = e
                .target
                .as_ref()
                .filter(|t| **t != e.name)
                .map(|t| view! { " as " <code>{t.clone()}</code> });
            view! {
                <li>
                    <strong>{cleanup_action_label(a)}</strong>
                    " "
                    <code>{e.name.clone()}</code>
                    {to}
                </li>
            }
        })
        .collect();
    let left = cleanup
        .entries
        .iter()
        .filter(|e| e.action.is_none())
        .count();
    view! {
        <h3>"What the cleanup does"</h3>
        <ul class="plain">
            {chosen}
            {cleanup
                .restores
                .iter()
                .map(|r| view! {
                    <li>
                        <strong>{restore_kind_label(r.kind)}</strong>
                        " "
                        <code>{format!("{RETENTION}/{}/{}", r.kept.generation, r.kept.kept_name)}</code>
                        " as "
                        <code>{r.target.clone()}</code>
                    </li>
                })
                .collect_view()}
            {cleanup
                .repacks
                .iter()
                .map(|r| view! {
                    <li>
                        <strong>"Repack as TorrentZip"</strong>
                        " "
                        <code>{r.target.clone()}</code>
                        ", keeping the file as it is aside"
                    </li>
                })
                .collect_view()}
        </ul>
        {(left > 0).then(|| view! {
            <p class="note small">
                {format!(
                    "{left} other entries are left as they are. If anything in the folder changes before you \
                     approve, you're asked to plan again."
                )}
            </p>
        })}
    }
    .into_any()
}

#[component]
fn PlanView(
    plan: Plan,
    replan: Callback<(i64, PlanChoices)>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let id = plan.id;
    let selection = plan.content.selection;
    let digest = StoredValue::new(plan.digest.clone());
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let busy = RwSignal::new(false);
    let approve = move |_| {
        busy.set(true);
        let request = ApprovePlan {
            digest: digest.get_value(),
        };
        leptos::task::spawn_local(async move {
            let approved = api::post::<_, Job>(&format!("/plans/{id}/approval"), &request).await;
            busy.set(false);
            outcome.set(Some(approved));
            refresh.update(|n| *n += 1);
        });
    };
    let approvable = plan.approvable() && plan.approval.is_none();
    let writes = plan.summary.copy + plan.summary.repack;
    // Your choices are part of the plan: changing one plans again, and you review the new plan.
    let choices = plan.content.choices.clone();
    let replan = move |choices: PlanChoices| replan.run((selection, choices));
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
    let conflicts: Vec<PlanItem> = plan
        .content
        .items
        .iter()
        .filter(|i| i.adoptable || i.kind == ItemKind::Adopt)
        .cloned()
        .collect();
    let adopt_choices = (open && !conflicts.is_empty()).then(|| {
        view! {
            <details class="choices" open>
                <summary>"Conflicts you can adopt"</summary>
                <p class="note small">
                    "Each file holds the ROMs the DAT expects at its name. Leave it, and it's skipped and the folder \
                    is incomplete; adopt it, and it's recorded as built without writing anything."
                </p>
                <ul class="plain">
                    {conflicts
                        .into_iter()
                        .map(|i| {
                            let adopted = i.kind == ItemKind::Adopt;
                            let next = with_adopt(&choices, &i.target, !adopted);
                            let (now, label) = if adopted {
                                ("adopted", "Leave it")
                            } else {
                                ("left as it is", "Adopt it")
                            };
                            view! {
                                <li>
                                    <code>{i.target.clone()}</code>
                                    {format!(": {now} ")}
                                    <button type="button" disabled=busy on:click=move |_| replan(next.clone())>{label}</button>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </details>
        }
    });

    let keepable: Vec<PlanItem> = plan
        .content
        .items
        .iter()
        .filter(|i| {
            i.keepable && i.kind != ItemKind::Adopt
                || i.keep
                    .as_ref()
                    .is_some_and(|k| k.reason == KeepReason::Conflict)
        })
        .cloned()
        .collect();
    let keep_choices = (open && !keepable.is_empty()).then(|| {
        let choices = plan.content.choices.clone();
        view! {
            <details class="choices" open>
                <summary>"Conflicts you can keep aside"</summary>
                <p class="note small">
                    "Keep a file aside, and it's moved into the folder's retention area, never deleted, and the game \
                    is built in its place. You can delete kept files later, from Kept files."
                </p>
                <ul class="plain">
                    {keepable
                        .into_iter()
                        .map(|i| {
                            let chosen = choices.keep_aside.contains(&i.target);
                            let next = with_keep_aside(&choices, &i.target, !chosen);
                            let (now, label) = if chosen {
                                ("kept aside and rebuilt", "Leave it")
                            } else {
                                ("left as it is", "Keep it aside and rebuild")
                            };
                            view! {
                                <li>
                                    <code>{i.target.clone()}</code>
                                    {format!(": {now} ")}
                                    <button type="button" disabled=busy on:click=move |_| replan(next.clone())>{label}</button>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </details>
        }
    });
    let removals = (!plan.content.removals.is_empty()).then(|| {
        view! {
            <details class="choices">
                <summary>{format!("Built files no longer selected ({})", plan.content.removals.len())}</summary>
                <p class="note small">
                    {format!(
                        "Each file kept aside is moved into {RETENTION}, never deleted; you delete kept files only by \
                         approving that separately."
                    )}
                </p>
                <ul class="plain">
                    {plan
                        .content
                        .removals
                        .iter()
                        .map(|r| {
                            view! {
                                <li>
                                    <strong>{removal_kind_label(r.kind)}</strong>
                                    " "
                                    <code>{r.target.clone()}</code>
                                    {format!(": {}", r.reason)}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </details>
        }
    });
    let removes = plan
        .content
        .removals
        .iter()
        .filter(|r| r.kind == RemovalKind::Keep)
        .count();
    let cleanup = plan.content.cleanup.as_ref().map(cleanup_view);
    let is_cleanup = cleanup.is_some();
    let approve_label = match (is_cleanup, removes) {
        (true, _) => "Approve the cleanup".to_string(),
        (false, 0) => format!("Approve and build {writes} files"),
        (false, n) => format!("Approve and build {writes} files, keeping {n} aside"),
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
    let items: Vec<ItemRow> = plan.content.items.iter().cloned().map(ItemRow).collect();
    let statuses = present(items.iter().map(ItemRow::kind));
    let kinds = statuses.clone();
    let rows = Signal::stored(Arc::new(items));
    let shown_kind = RwSignal::new(None::<BuildKind>);
    let keep = Signal::derive(move || -> Keep<ItemRow> {
        shown_kind.get().map(|k| {
            Arc::new(move |r: &ItemRow| r.kind() == k)
                as Arc<dyn Fn(&ItemRow) -> bool + Send + Sync>
        })
    });
    let kinds = StoredValue::new(kinds);
    let filters = move || {
        view! {
            <label class="filter-select">
                <span class="visually-hidden">"Kind"</span>
                <select on:change=move |ev| {
                    let value = event_target_value(&ev);
                    shown_kind.set(kinds.with_value(|k| k.iter().copied().find(|k| k.look().label == value)));
                }>
                    <option value="">"Every kind"</option>
                    {kinds
                        .get_value()
                        .into_iter()
                        .map(|k| {
                            let label = k.look().label;
                            view! { <option value=label selected=move || shown_kind.get() == Some(k)>{label}</option> }
                        })
                        .collect_view()}
                </select>
            </label>
        }
    };

    view! {
        <section aria-labelledby="latest-plan-title">
            <h2 id="latest-plan-title">{heading}</h2>
            <p class="note">{state}</p>
            {kind_strip(&plan.summary)}
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
            {cleanup}
            {partial}
            {adopt_choices}
            {keep_choices}
            {removals}
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
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => view! { <BuildProgress job=job.id /> }.into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
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
            {(!is_cleanup).then(|| view! {
                <List
                    id="plan-items"
                    label="Plan items"
                    rows
                    empty="The plan has no files."
                    placeholder="Filter by game, file or source"
                    detail=Callback::new(item_view)
                    keep
                    filters=filters
                    legend=move || view! { <Legend statuses=statuses.clone() list="plan-items" /> }
                />
            })}
        </section>
    }
}
