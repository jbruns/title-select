//! An ingest plan, for you to review and approve: what it does with each item, as a `List` of rows reading item →
//! destination with the kind of move beside them, and each item's reason and outcome in its detail. The folder page
//! shows it in place of its items through `IngestPlanReview` alone, so that the shared plan review can take its place.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use title_select_api::build::ApprovePlan;
use title_select_api::ingest::{IngestAction, IngestOutcome, IngestPlan, IngestPlanItem};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::ingest_model::{self, Outcome};
use crate::model::{short_sha, when};
use crate::parts::icon::{Glyph, LabelledIcon};
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Clip, Column, List, ListRow, Width};
use crate::parts::pending::ActionPending;
use crate::parts::status::{IngestPlanKind, Status, StatusBadge, StatusIcon};

/// One item of the plan under review, with how its move ended once you approved the plan.
#[derive(Debug, Clone, PartialEq)]
struct PlanRow {
    item: IngestPlanItem,
    kind: IngestPlanKind,
    outcome: Option<IngestOutcome>,
    result: Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanCol {
    Kind,
    Change,
    Result,
}

impl ListRow for PlanRow {
    type Col = PlanCol;
    const COLUMNS: &'static [PlanCol] = &[PlanCol::Kind, PlanCol::Change, PlanCol::Result];

    fn column(col: PlanCol) -> Column {
        match col {
            PlanCol::Kind => Column::new("kind", "Does", Width::Fixed(4.0))
                .sortable()
                .hint("What the plan does with the item"),
            PlanCol::Change => Column::new(
                "item",
                "Item → destination",
                Width::Flex {
                    min: 18.0,
                    share: 4,
                },
            )
            .sortable(),
            PlanCol::Result => Column::new("result", "Result", Width::Fixed(11.0))
                .sortable()
                .hint("How the item's move ended, once you approve the plan"),
        }
    }

    fn key(&self) -> String {
        self.item.item.to_string()
    }

    fn title(&self) -> String {
        self.item.name.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {}",
            self.item.name,
            self.item.destination,
            self.kind.look().label,
            self.result.label()
        )
    }

    fn cell(&self, col: PlanCol) -> AnyView {
        match col {
            PlanCol::Kind => view! { <StatusIcon status=self.kind tab_stop=false /> }.into_any(),
            PlanCol::Change => view! {
                <span class="ingest-change">
                    <Clip text=self.item.name.clone() />
                    <span aria-hidden="true">"→"</span>
                    <span class="visually-hidden">" goes to "</span>
                    <Clip text=self.item.destination.clone() />
                </span>
            }
            .into_any(),
            PlanCol::Result => self.result.label().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: PlanCol) -> Ordering {
        match col {
            PlanCol::Kind => self.kind.rank().cmp(&other.kind.rank()),
            PlanCol::Change => self.item.name.cmp(&other.item.name),
            PlanCol::Result => self.result.cmp(&other.result),
        }
    }
}

fn rows(plan: &IngestPlan) -> Vec<PlanRow> {
    let kind = IngestPlanKind::from(plan.content.action);
    plan.content
        .items
        .iter()
        .map(|item| {
            let outcome = plan.outcomes.iter().find(|o| o.item == item.item).cloned();
            PlanRow {
                result: ingest_model::outcome(plan, outcome.as_ref()),
                item: item.clone(),
                kind,
                outcome,
            }
        })
        .collect()
}

fn row_detail(row: PlanRow) -> AnyView {
    let PlanRow {
        item,
        kind,
        outcome,
        result,
    } = row;
    let ended = outcome.and_then(|o| o.error.or(o.detail));
    view! {
        <dl class="facts">
            <dt>"Does"</dt>
            <dd>
                <StatusBadge status=kind />
            </dd>
            <dt>"Item"</dt>
            <dd><code>{item.name.clone()}</code></dd>
            <dt>"Goes to"</dt>
            <dd><code>{item.destination.clone()}</code></dd>
            <dt>"Why"</dt>
            <dd>{item.reason.clone()}</dd>
            {item.provider.clone().map(|p| view! {
                <dt>"Provider"</dt>
                <dd>{p}</dd>
            })}
            {item.addition.clone().map(|a| view! {
                <dt>"Added as"</dt>
                <dd>
                    {format!("{} in {}", a.name, a.system)}
                    {a.parent.map(|p| format!(", a clone of {p}"))}
                </dd>
            })}
            {item.addition_target.clone().map(|t| view! {
                <dt>"Published as"</dt>
                <dd>
                    <code>{t.file_name}</code>
                    {t.loose.then_some(", loose")}
                </dd>
            })}
            {item.sha256.clone().map(|sha| view! {
                <dt>"SHA-256"</dt>
                <dd><code class="hash">{sha}</code></dd>
            })}
            <dt>"Result"</dt>
            <dd>
                {result.label()}
                {ended.map(|e| format!(": {e}"))}
            </dd>
        </dl>
    }
    .into_any()
}

fn action_text(action: IngestAction) -> &'static str {
    match action {
        IngestAction::Discard => "Discard: each item is kept aside in the folder's kept files.",
        IngestAction::DatImport => {
            "Import DATs: each is imported, then kept aside in the folder's kept files."
        }
        IngestAction::Addition => {
            "Add as additions: each is repacked into its system's additions set, its original kept aside once the \
             copy reads back exactly."
        }
    }
}

/// The plan `plan`, loaded and listed, with approving it. `refresh` is the shared ingest counter: approving bumps it,
/// and the plan reloads with it. `back` returns to the folder's items.
#[component]
pub fn IngestPlanReview(plan: i64, refresh: RwSignal<u64>, back: Callback<()>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<IngestPlan>(&format!("/ingest-plans/{plan}")).await }
    });
    // The last plan loaded, so a reload doesn't redraw the list.
    let latest = Memo::new(
        move |prev: Option<&Option<IngestPlan>>| match loaded.get() {
            Some(Ok(p)) => Some(p),
            _ => prev.cloned().flatten(),
        },
    );
    let approving = Action::new_local(move |p: &IngestPlan| {
        let (id, body) = (
            p.id,
            ApprovePlan {
                digest: p.digest.clone(),
            },
        );
        async move {
            let done =
                api::post::<_, IngestPlan>(&format!("/ingest-plans/{id}/approval"), &body).await;
            if done.is_ok() {
                refresh.update(|n| *n += 1);
            }
            done
        }
    });
    let list_rows: Signal<Arc<Vec<PlanRow>>> =
        Signal::derive(move || Arc::new(latest.with(|p| p.as_ref().map(rows).unwrap_or_default())));
    let kinds = Signal::derive(move || present(list_rows.get().iter().map(|r| r.kind)));

    let header = move || {
        latest.get().map(|p| {
            let n = p.content.items.len();
            let state = match (&p.approval, &p.finished_at) {
                (None, _) => None,
                (Some(a), None) => Some(format!(
                    "You approved it {}. It's moving the items now.",
                    when(&a.at)
                )),
                (Some(_), Some(at)) => Some(format!("Finished {}.", when(at))),
            };
            let label = if n == 1 {
                "Approve and move 1 item".to_string()
            } else {
                format!("Approve and move {n} items")
            };
            let approve = p.approval.is_none().then(|| {
                let p = p.clone();
                view! {
                    <button
                        type="button"
                        class="primary"
                        disabled=move || approving.pending().get()
                        on:click=move |_| { approving.dispatch_local(p.clone()); }
                    >
                        {label.clone()}
                    </button>
                }
            });
            view! {
                <p>{action_text(p.content.action)}</p>
                <p class="small">
                    "Digest " <code class="hash">{short_sha(&p.digest).to_string()}</code>
                    {format!(
                        " · it binds the {} entries of the folder as they are now; if anything there changes before \
                         you approve, plan again",
                        p.content.fingerprint.len()
                    )}
                </p>
                {state.map(|s| view! { <p class="done" role="status">{s}</p> })}
                <div class="form">{approve}</div>
            }
        })
    };

    view! {
        <section aria-labelledby="plan-title">
            <h2 id="plan-title">{format!("Plan #{plan}")}</h2>
            <p>
                <button type="button" on:click=move |_| back.run(())>
                    <LabelledIcon glyph=Glyph::ClipboardList label="Back to the items" />
                </button>
            </p>
            {move || match loaded.get() {
                None if latest.with(Option::is_none) => view! { <Loading what="the plan" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
            <ActionPending action=approving status="Approving…">
                {header}
            </ActionPending>
            <List
                id="ingest-plan"
                label=format!("Plan #{plan}")
                rows=list_rows
                empty="The plan moves nothing."
                placeholder="Filter by item, destination or result"
                detail=Callback::new(row_detail)
                legend=move || view! { <Legend statuses=kinds list="ingest-plan" /> }
            />
        </section>
    }
}
