//! A source set's Repair tab: the plan proposed from its latest audit, reviewed in bulk, and its approval and job;
//! and its Kept files tab. The plan review is `parts::plan::PlanReview`, the reference for reviewing any plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::Job;
use title_select_api::additions::NewAddition;
use title_select_api::build::{ApprovePlan, PlanKeptDeletion, RETENTION};
use title_select_api::repair::{
    PlanRepair, RepairChoice, RepairItem, RepairPlan, RepairStep, SourceKeptDeletion,
    SourceKeptFile,
};
use title_select_api::storage::{Storage, StorageFormat};
use wasm_bindgen::JsCast;

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::when;
use crate::parts::icon::Glyph;
use crate::parts::list::{Column, ListRow, ToolAction, Width};
use crate::parts::pending::Pending;
use crate::parts::plan::{Change, PlanReview, PlanRow};
use crate::parts::status::{PlanKind, Status, StatusIcon};
use crate::repair_model::{
    apply_to_keys, apply_to_kind, choices_for, chosen, current, fetch_after, kind_choices, place,
    proposed, repair_to_show, requested, steps_by_item, suggested_addition, to_add,
    unfinished_note, unplanned,
};
use crate::screens::additions::AdditionFields;
use crate::screens::jobs::{progress_view, state_view};
use crate::screens::source_sets::tab_href;
use crate::source_set_model::PageTab;

/// What the pending status says while a plan is made.
pub const MAKING_PLAN: &str = "Making the repair plan… a large source set can take a while.";

/// What the storage policy says for the plan's system, as the plan follows it.
pub fn storage_line(storage: &Storage) -> String {
    format!(
        "The storage policy says this system stores games {}{}, and it must still say so when you approve.",
        match storage.format {
            StorageFormat::TorrentZip => "as TorrentZip",
            StorageFormat::Loose => "loose where a game has one ROM",
        },
        if storage.delete_originals {
            "; delete originals after conversion is on, so each original is deleted once its converted file is verified"
        } else {
            "; each original a conversion replaces goes to kept files"
        }
    )
}

fn step_line(step: &RepairStep) -> (String, &'static str) {
    let class = match step {
        RepairStep::Keep { .. } | RepairStep::Delete { .. } => "removal",
        _ => "",
    };
    (step.describe(), class)
}

/// A plan made, with the choices it was asked for with.
type Proposed = (RepairPlan, BTreeMap<String, RepairChoice>);

/// The choices a plan was asked for with, and those you picked on it.
type Staged = (
    BTreeMap<String, RepairChoice>,
    BTreeMap<String, RepairChoice>,
);

/// A source set's repair, kept by its page so that a plan being made, and your picks, outlive a change of tab.
#[derive(Clone, Copy)]
pub struct RepairDesk {
    name: StoredValue<String>,
    /// What you picked on the plan shown, by item; approving approves the plan as it was made, so a pick it doesn't
    /// hold waits until you plan again with it.
    picked: RwSignal<BTreeMap<String, RepairChoice>>,
    /// The choices the plan shown was asked for with.
    asked: RwSignal<BTreeMap<String, RepairChoice>>,
    plan: RwSignal<Option<RepairPlan>>,
    problem: RwSignal<Option<String>>,
    working: RwSignal<bool>,
    /// The job the plan was last fetched for as it ended; kept here, as the progress shown is made again each time.
    fetched: RwSignal<Option<i64>>,
    /// What each item you choose to add as an addition is added as, by key.
    adding: RwSignal<BTreeMap<String, RwSignal<NewAddition>>>,
    /// The page's owner: the fields in `adding` outlive each showing of the plan.
    owner: StoredValue<Owner>,
    system: StoredValue<String>,
    /// What the last bulk choice changed.
    said: RwSignal<Option<String>>,
    /// The plan last asked to be shown, once one has been: `Some(None)` for the open repair.
    requested: RwSignal<Option<Option<i64>>>,
    /// The choices asked for and picked on each plan shown before, by plan, so coming back to one keeps them.
    staged: StoredValue<BTreeMap<i64, Staged>>,
    /// Goes up each time another plan is asked for or shown: a response to an earlier request is dropped.
    epoch: StoredValue<u64>,
    /// The epoch the last proposal was asked for in.
    proposed_in: StoredValue<u64>,
    proposal: Action<PlanRepair, Result<Proposed, String>>,
}

impl RepairDesk {
    /// The repair of the source set `name`, whose DAT is `system`.
    pub fn new(name: String, system: String) -> Self {
        let path = format!("/source-sets/{}/repairs", api::segment(&name));
        let proposal = Action::new_local(move |body: &PlanRepair| {
            let (path, body) = (path.clone(), body.clone());
            async move {
                let planned = api::post::<_, RepairPlan>(&path, &body).await;
                planned.map(|p| (p, body.choices))
            }
        });
        let desk = RepairDesk {
            name: StoredValue::new(name),
            picked: RwSignal::new(BTreeMap::new()),
            asked: RwSignal::new(BTreeMap::new()),
            plan: RwSignal::new(None),
            problem: RwSignal::new(None),
            working: RwSignal::new(false),
            fetched: RwSignal::new(None),
            adding: RwSignal::new(BTreeMap::new()),
            owner: StoredValue::new(Owner::current().expect("the source set page has an owner")),
            system: StoredValue::new(system),
            said: RwSignal::new(None),
            requested: RwSignal::new(None),
            staged: StoredValue::new(BTreeMap::new()),
            epoch: StoredValue::new(0),
            proposed_in: StoredValue::new(0),
            proposal,
        };
        Effect::new(move || {
            if let Some(Ok((p, chosen))) = proposal.value().get()
                && desk.is_current(desk.proposed_in.get_value())
            {
                desk.show(p, chosen);
            }
        });
        desk
    }

    /// Starts another epoch, ending every request made before it.
    fn next_epoch(self) -> u64 {
        self.epoch.update_value(|e| *e += 1);
        self.epoch.get_value()
    }

    /// Whether nothing else was asked for or shown since `epoch` began.
    fn is_current(self, epoch: u64) -> bool {
        self.epoch.get_value() == epoch
    }

    /// Keeps what was asked for and picked on the plan shown, for coming back to it.
    fn stash(self) {
        if let Some(id) = self.plan.with_untracked(|p| p.as_ref().map(|p| p.id)) {
            let staged = (self.asked.get_untracked(), self.picked.get_untracked());
            self.staged.update_value(|s| {
                s.insert(id, staged);
            });
        }
    }

    /// Shows `plan`, asked for with `asked`: with what you picked on it before, if you've shown it already.
    fn show(self, plan: RepairPlan, asked: BTreeMap<String, RepairChoice>) {
        self.stash();
        let (asked, picked) = self
            .staged
            .with_value(|s| s.get(&plan.id).cloned())
            .unwrap_or((asked, BTreeMap::new()));
        self.next_epoch();
        self.problem.set(None);
        self.said.set(None);
        self.picked.set(picked);
        self.asked.set(asked);
        self.plan.set(Some(plan));
    }

    /// Subscribes to the plan shown, so that what reads the source set's open repair reads it again.
    pub fn track(self) {
        self.plan.track();
    }

    /// Whether a plan is being made.
    pub fn proposing(self) -> Signal<bool> {
        self.proposal.pending().into()
    }

    /// Shows the plan `wanted` names, or else the approved repair that hasn't finished, each time what's wanted
    /// changes. The plan already shown stays as it is, with your picks.
    pub fn load(self, wanted: Option<i64>) {
        if self.requested.get_untracked() == Some(wanted) {
            return;
        }
        self.requested.set(Some(wanted));
        let path = format!(
            "/source-sets/{}/repairs",
            api::segment(&self.name.get_value())
        );
        let before = self.shown_id();
        let epoch = self.next_epoch();
        leptos::task::spawn_local(async move {
            let plans = api::get::<Vec<RepairPlan>>(&path).await;
            // Another plan was asked for, or one was proposed or shown, meanwhile: that one wins.
            if !self.is_current(epoch) {
                return;
            }
            match plans {
                Ok(plans) => match repair_to_show(&plans, wanted) {
                    Some(p) if Some(p.id) == before => {}
                    Some(p) => self.show(p.clone(), chosen(p)),
                    None => {
                        self.stash();
                        self.next_epoch();
                        self.plan.set(None);
                    }
                },
                Err(message) => self.problem.set(Some(message)),
            }
        });
    }

    fn shown_id(self) -> Option<i64> {
        self.plan.with_untracked(|p| p.as_ref().map(|p| p.id))
    }

    /// Proposes a plan; planning again from a plan still open carries its choices and additions.
    pub fn propose(self) {
        let open = self.plan.get_untracked().filter(|p| p.approval.is_none());
        let chosen = open
            .as_ref()
            .map(|p| requested(p, &self.asked.get_untracked(), &self.picked.get_untracked()))
            .unwrap_or_default();
        let additions = self.adding.with_untracked(|adding| {
            let adding = adding
                .iter()
                .map(|(k, v)| (k.clone(), v.get_untracked()))
                .collect();
            open.as_ref()
                .map(|p| to_add(p, &chosen, &adding, &self.system.get_value()))
                .unwrap_or_default()
        });
        self.proposed_in.set_value(self.next_epoch());
        self.proposal.dispatch_local(PlanRepair {
            choices: chosen,
            additions,
        });
    }

    fn approve(self) {
        let Some(p) = self.plan.get_untracked() else {
            return;
        };
        self.working.set(true);
        let body = ApprovePlan {
            digest: p.digest.clone(),
        };
        let epoch = self.next_epoch();
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, RepairPlan>(&format!("/repairs/{}/approval", p.id), &body).await;
            self.working.set(false);
            // An approved plan holds no picks, shown or not.
            if done.is_ok() {
                self.staged.update_value(|s| {
                    s.remove(&p.id);
                });
            }
            if !self.is_current(epoch) {
                return;
            }
            match done {
                Ok(p) => {
                    self.picked.set(BTreeMap::new());
                    self.asked.set(BTreeMap::new());
                    self.show(p, BTreeMap::new());
                }
                Err(message) => self.problem.set(Some(message)),
            }
        });
    }

    /// The fields of what `item` is added as, made the first time they're asked for.
    fn fields(self, item: &RepairItem) -> RwSignal<NewAddition> {
        if let Some(fields) = self.adding.with_untracked(|a| a.get(&item.key).copied()) {
            return fields;
        }
        let value = item
            .addition
            .clone()
            .unwrap_or_else(|| suggested_addition(item, &self.system.get_value()));
        let fields = self.owner.get_value().with(|| RwSignal::new(value));
        self.adding.update_untracked(|a| {
            a.insert(item.key.clone(), fields);
        });
        fields
    }

    /// How many items you changed since the plan shown was made.
    fn changed(self) -> usize {
        let adding = self.adding.with(|a| {
            a.iter()
                .map(|(k, v)| (k.clone(), v.get()))
                .collect::<BTreeMap<_, _>>()
        });
        self.plan.with(|p| {
            p.as_ref()
                .map_or(0, |p| self.picked.with(|c| unplanned(p, c, &adding).len()))
        })
    }
}

/// One item of the plan shown.
#[derive(Clone)]
struct RepairRow {
    item: RepairItem,
    place: String,
    proposed: String,
    open: bool,
    outcome: Option<String>,
    desk: RepairDesk,
}

impl PartialEq for RepairRow {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.proposed == other.proposed
            && self.open == other.open
            && self.outcome == other.outcome
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairCol {
    Kind,
    Change,
    Choice,
}

impl PlanRow for RepairRow {
    type Kind = PlanKind;
    fn kind(&self) -> PlanKind {
        PlanKind::from(self.item.kind)
    }
}

impl ListRow for RepairRow {
    type Col = RepairCol;
    const COLUMNS: &'static [RepairCol] = &[RepairCol::Kind, RepairCol::Change, RepairCol::Choice];

    fn column(col: RepairCol) -> Column {
        match col {
            RepairCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the repair does to it"),
            RepairCol::Change => Column::new(
                "change",
                "Current → proposed",
                Width::Flex {
                    min: 16.0,
                    share: 1,
                },
            )
            .sortable(),
            RepairCol::Choice => Column::new("choice", "Choice", Width::Fixed(11.0))
                .sortable()
                .hint("What you choose to do with it"),
        }
    }

    fn key(&self) -> String {
        self.item.key.clone()
    }

    fn title(&self) -> String {
        self.place.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.place,
            self.proposed,
            self.item.summary,
            self.kind().look().label,
            self.item.chosen.label()
        )
    }

    fn cell(&self, col: RepairCol) -> AnyView {
        match col {
            RepairCol::Kind => {
                view! { <StatusIcon status=self.kind() tab_stop=false /> }.into_any()
            }
            RepairCol::Change => {
                view! { <Change current=self.place.clone() proposed=self.proposed.clone() /> }
                    .into_any()
            }
            RepairCol::Choice if self.open => choice_select(&self.item, self.desk),
            RepairCol::Choice => self.item.chosen.label().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: RepairCol) -> Ordering {
        match col {
            RepairCol::Kind => self.kind().rank().cmp(&other.kind().rank()),
            RepairCol::Change => self.place.cmp(&other.place),
            RepairCol::Choice => self.item.chosen.label().cmp(other.item.chosen.label()),
        }
    }
}

/// The choice of one item, as a `<select>` at the end of its row.
fn choice_select(item: &RepairItem, desk: RepairDesk) -> AnyView {
    let key = StoredValue::new(item.key.clone());
    let planned = StoredValue::new(item.clone());
    let now = move || desk.picked.with(|p| planned.with_value(|i| current(i, p)));
    let offered = item.choices.clone();
    let planned_choice = item.chosen;
    view! {
        <select
            class="choice"
            class:changed=move || now() != planned_choice
            aria-label=format!("What to do with {}", place(item))
            prop:value=move || now().label()
            on:change=move |ev| {
                let value = event_target_value(&ev);
                if let Some(c) = offered.iter().find(|c| c.label() == value) {
                    let c = *c;
                    desk.picked.update(|m| {
                        m.insert(key.get_value(), c);
                    });
                }
            }
        >
            {item
                .choices
                .iter()
                .map(|c| view! { <option value=c.label()>{c.label()}</option> })
                .collect_view()}
        </select>
    }
    .into_any()
}

/// Why and how: the item's summary, the plan's note on it, its steps, what it is added as, and its outcome.
fn repair_detail(row: RepairRow, steps: Arc<Vec<(String, String, &'static str)>>) -> AnyView {
    let item = row.item.clone();
    let desk = row.desk;
    let kind = row.kind();
    let addable = (row.open && item.choices.contains(&RepairChoice::AddAddition)).then(|| {
        let fields = desk.fields(&item);
        let shows = {
            let item = item.clone();
            move || desk.picked.with(|p| current(&item, p)) == RepairChoice::AddAddition
        };
        view! {
            <Show when=shows>
                <h3>"Add it as"</h3>
                <AdditionFields value=fields />
            </Show>
        }
    });
    let own: Vec<_> = steps
        .iter()
        .filter(|(k, _, _)| *k == item.key)
        .map(|(_, line, class)| view! { <li class=*class>{line.clone()}</li> })
        .collect();
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd>{kind.look().label} " — " {kind.look().hint}</dd>
            <dt>"Why"</dt>
            <dd>{item.summary.clone()}</dd>
            <dt>"Now"</dt>
            <dd>
                <code class="path">{row.place.clone()}</code>
            </dd>
            <dt>"Proposed"</dt>
            <dd>
                <code class="path">{row.proposed.clone()}</code>
            </dd>
            <dt>"Choice"</dt>
            <dd>
                {format!("{} (the plan proposed {})", item.chosen.label(), item.default.label())}
                {row.open.then(|| view! { " " {choice_select(&item, desk)} })}
            </dd>
            {item.game.clone().map(|g| view! {
                <dt>"Game"</dt>
                <dd>{g}</dd>
            })}
            {item.note.clone().map(|n| view! {
                <dt>"Note"</dt>
                <dd>{n}</dd>
            })}
            {row.outcome.clone().map(|o| view! {
                <dt>"Outcome"</dt>
                <dd>{o}</dd>
            })}
            {item.addition.clone().filter(|_| !row.open).map(|a| view! {
                <dt>"Added as"</dt>
                <dd>
                    {format!("The addition {} of {}", a.name, a.system)}
                    {a.parent.map(|p| format!(", a version of {p}"))}
                </dd>
            })}
        </dl>
        {addable}
        {(!own.is_empty()).then(|| view! {
            <h3>"Its changes"</h3>
            <ul class="plain">{own}</ul>
        })}
    }
    .into_any()
}

/// The "apply to all" control of a kind: a `<select>` defaulting to "no change".
fn apply_to_all(desk: RepairDesk, kind: PlanKind) -> AnyView {
    let choices = desk
        .plan
        .with_untracked(|p| p.as_ref().map(|p| kind_choices(p, kind)))
        .unwrap_or_default();
    if choices.is_empty() {
        return ().into_any();
    }
    let label = kind.look().label.to_lowercase();
    view! {
        <select
            class="apply-all"
            aria-label=format!("Set every {label} item to")
            title=format!("Set every {label} item to one choice")
            on:change=move |ev| {
                let value = event_target_value(&ev);
                let choice = choices.iter().copied().find(|c| c.label() == value);
                let said = desk.plan.with_untracked(|p| {
                    let p = p.as_ref()?;
                    let mut picked = desk.picked.get_untracked();
                    let outcome = apply_to_kind(p, kind, choice, &mut picked)?;
                    desk.picked.set(picked);
                    Some(outcome.message(choice?.label()))
                });
                if let Some(said) = said {
                    desk.said.set(Some(said));
                }
                // The control goes back to "no change": it sets, it doesn't hold, a choice.
                if let Some(el) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok()) {
                    el.set_value("");
                }
            }
        >
            <option value="">"Apply to all: no change"</option>
            {choices
                .iter()
                .map(|c| view! { <option value=c.label()>{format!("Apply to all: {}", c.label())}</option> })
                .collect_view()}
        </select>
    }
    .into_any()
}

fn outcome_text(o: &title_select_api::repair::RepairOutcome) -> String {
    {
        let what = match (&o.error, o.done) {
            (Some(e), _) => format!("Not done: {e}"),
            (None, true) => "Done".to_string(),
            (None, false) => "Not done".to_string(),
        };
        let detail = o.detail.join("; ");
        if detail.is_empty() {
            what
        } else {
            format!("{what} ({detail})")
        }
    }
}

/// The Repair tab: a plan proposed from the source set's latest audit, the choice for each item, set in bulk or one
/// at a time, every change it makes, and its approval and job.
///
/// `wanted` is the plan to show, and `current` the source set's open repair: a plan not yet approved that isn't the
/// open repair, shown because the URL names it, was replaced, so it is shown read-only.
#[component]
pub fn RepairTab(
    desk: RepairDesk,
    #[prop(into)] wanted: Signal<Option<i64>>,
    #[prop(into)] current: Signal<Option<i64>>,
) -> impl IntoView {
    Effect::new(move || desk.load(wanted.get()));
    let replaced = Memo::new(move |_| {
        desk.plan.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.approval.is_none() && wanted.get() == Some(p.id) && current.get() != Some(p.id)
            })
        })
    });
    let proposal = desk.proposal;
    let running = move || {
        desk.plan.with(|p| {
            p.as_ref()
                .is_some_and(|p| p.approval.is_some() && p.finished_at.is_none())
        })
    };
    let problem = Signal::derive(move || {
        proposal
            .value()
            .with(|v| v.as_ref().and_then(|r| r.as_ref().err().cloned()))
    });
    view! {
        <section aria-label="Repair">
            <p class="lede">
                "A repair is proposed from the source set's latest audit and from items in ingest folders. Set a \
                choice for every item of a kind, for the rows you check, or one row at a time, then plan again to \
                review what it changes. Nothing changes until you approve, and nothing is deleted: what a repair \
                replaces or sets aside goes to kept files."
            </p>
            {move || {
                (!running() && desk.plan.with(Option::is_none)).then(|| view! {
                    <div class="actions">
                        <button
                            type="button"
                            class="primary"
                            disabled=move || desk.working.get() || proposal.pending().get()
                            on:click=move |_| desk.propose()
                        >
                            "Propose a repair"
                        </button>
                    </div>
                })
            }}
            <Pending pending=proposal.pending() status=MAKING_PLAN problem>
                {move || {
                    let replaced = replaced.get();
                    desk.plan.get().map(|p| view! { <PlanShown desk plan=p replaced /> })
                }}
            </Pending>
            {move || desk.problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

#[component]
fn PlanShown(desk: RepairDesk, plan: RepairPlan, replaced: bool) -> impl IntoView {
    let p = plan;
    let open = p.approval.is_none() && !replaced;
    let steps: Arc<Vec<(String, String, &'static str)>> = Arc::new(
        p.content
            .steps
            .iter()
            .map(|s| {
                let (line, class) = step_line(&s.step);
                (s.item.clone(), line, class)
            })
            .collect(),
    );
    let by_item = steps_by_item(&p);
    let outcomes: HashMap<&str, _> = p.outcomes.iter().map(|o| (o.item.as_str(), o)).collect();
    let rows: Arc<Vec<RepairRow>> = Arc::new(
        p.content
            .items
            .iter()
            .map(|item| RepairRow {
                place: place(item),
                proposed: proposed(
                    item,
                    by_item
                        .get(item.key.as_str())
                        .map_or(&[][..], Vec::as_slice),
                ),
                open,
                outcome: outcomes.get(item.key.as_str()).map(|o| outcome_text(o)),
                item: item.clone(),
                desk,
            })
            .collect(),
    );
    drop(by_item);
    drop(outcomes);
    let removals = p
        .content
        .steps
        .iter()
        .filter(|s| matches!(s.step, RepairStep::Keep { .. }))
        .count();
    let count = p.content.steps.len();
    let changed = move || desk.changed();
    let held = move || desk.working.get() || desk.proposal.pending().get() || changed() != 0;
    let bulk = |glyph: Glyph, choice: RepairChoice| {
        ToolAction::checked(
            glyph,
            format!("Set to {}", choice.label()),
            move |chosen: crate::parts::list::Selected<RepairRow>| {
                let keys: Vec<String> = chosen.rows.iter().map(|r| r.item.key.clone()).collect();
                let said = desk.plan.with_untracked(|p| {
                    let p = p.as_ref()?;
                    let mut picked = desk.picked.get_untracked();
                    let outcome = apply_to_keys(p, &keys, choice, &mut picked);
                    desk.picked.set(picked);
                    Some(outcome.message(choice.label()))
                });
                desk.said.set(said);
            },
        )
    };
    // Only the choices some item offers are offered in bulk.
    let all_keys: Vec<String> = p.content.items.iter().map(|i| i.key.clone()).collect();
    let offered = choices_for(&p, &all_keys);
    let actions: Vec<ToolAction<RepairRow>> = if open {
        [
            (Glyph::Check, RepairChoice::Apply),
            (Glyph::CircleOff, RepairChoice::Leave),
            (Glyph::Archive, RepairChoice::KeepAside),
        ]
        .into_iter()
        .filter(|(_, c)| offered.contains(c))
        .map(|(g, c)| bulk(g, c))
        .collect()
    } else {
        Vec::new()
    };
    let detail_steps = steps.clone();
    let audit_href = format!("/audits/{}", p.content.audit);
    let notes = p.content.notes.clone();
    let replaced_note = replaced.then(|| {
        view! {
            <p class="notice" role="status">
                "A newer plan replaced this one, so it is shown read-only. "
                <A href=tab_href(&desk.name.get_value(), PageTab::Repair)>"Show the source set's repair"</A>
            </p>
        }
    });
    view! {
        <h2>{format!("Repair plan #{}", p.id)}</h2>
        {replaced_note}
        <p class="note">
            "From " <A href=audit_href>{format!("audit #{}", p.content.audit)}</A>
            {format!(
                ". It binds the {} entries of the source set as they are now: anything changing there, or another \
                 audit, before you approve means planning again.",
                p.content.fingerprint.len()
            )}
            {p.content.creates_marker.then_some(
                " It is this source set's first repair, so it writes the source set's .title-select marker.",
            )}
            " " {storage_line(&p.content.storage)}
        </p>
        {p.job.map(|job| view! { <RepairProgress job plan=p.id desk /> })}
        {match unfinished_note(&p) {
            Some(note) => Some(view! { <p class="notice" role="status">{note}</p> }.into_any()),
            None => p.recovery_error.clone().map(|e| view! { <Problem message=e /> }.into_any()),
        }}
        {p.audit_job.map(|job| view! {
            <p>
                "An incremental audit was queued: "
                <A href=format!("/activity?row={job}")>{format!("job {job}")}</A>
            </p>
        })}
        {p.audit_owed.then(|| view! {
            <p class="note">"An incremental audit is queued once the job running for the source set ends."</p>
        })}
        {if p.content.items.is_empty() {
            view! { <p class="note">"Nothing to repair."</p> }.into_any()
        } else {
            view! {
                <PlanReview
                    id="repair-items"
                    label="Repair items"
                    rows=Signal::derive(move || rows.clone())
                    empty="Nothing to repair."
                    placeholder="Filter by file, game, kind or choice"
                    detail=Callback::new(move |row: RepairRow| repair_detail(row, detail_steps.clone()))
                    actions
                    apply=Callback::new(move |kind: PlanKind| if open { apply_to_all(desk, kind) } else { ().into_any() })
                    message=desk.said
                />
            }
                .into_any()
        }}
        {open.then(|| view! {
            {move || {
                let n = changed();
                (n > 0).then(|| view! {
                    <p class="notice" role="status">
                        {format!(
                            "You changed {n} {} since this plan was made. Plan again with your choices to review \
                             and approve them; approving now would approve the plan as shown.",
                            if n == 1 { "item" } else { "items" }
                        )}
                    </p>
                })
            }}
            <div class="actions">
                <button
                    type="button"
                    class:primary=move || changed() != 0
                    disabled=move || desk.working.get() || desk.proposal.pending().get()
                    on:click=move |_| desk.propose()
                >
                    "Plan again with your choices"
                </button>
                {(count > 0).then(|| view! {
                    <button
                        type="button"
                        class:primary=move || changed() == 0
                        disabled=held
                        on:click=move |_| desk.approve()
                    >
                        {format!("Approve and make {count} changes")}
                    </button>
                })}
            </div>
        })}
        <details>
            <summary>{format!("Every change, in order ({count}; {removals} kept aside)")}</summary>
            <ul class="plain">
                {steps
                    .iter()
                    .map(|(_, line, class)| view! { <li class=*class>{line.clone()}</li> })
                    .collect_view()}
            </ul>
        </details>
        {(!notes.is_empty()).then(|| view! {
            <details>
                <summary>{format!("Left as it is ({})", notes.len())}</summary>
                <ul class="plain">{notes.into_iter().map(|n| view! { <li>{n}</li> }).collect_view()}</ul>
            </details>
        })}
    }
}

/// The repair's job, live: its state and progress, a way to cancel it while it runs, and the plan fetched again
/// with its outcomes once it ends.
#[component]
fn RepairProgress(job: i64, plan: i64, desk: RepairDesk) -> impl IntoView {
    let (shown, fetched) = (desk.plan, desk.fetched);
    let live = use_live();
    let problem = RwSignal::new(None::<String>);
    let cancelling = RwSignal::new(false);
    Effect::new(move |_| {
        let ended = live
            .book
            .with(|b| b.get(job).is_some_and(|l| l.job.state.is_final()));
        let unfinished =
            shown.with_untracked(|p| p.as_ref().is_some_and(|p| p.finished_at.is_none()));
        if fetch_after(job, ended, unfinished, fetched.get_untracked()) {
            fetched.set(Some(job));
            let epoch = desk.epoch.get_value();
            leptos::task::spawn_local(async move {
                let done = api::get::<RepairPlan>(&format!("/repairs/{plan}")).await;
                // Another plan was asked for or shown meanwhile: this one is no longer on screen.
                if !desk.is_current(epoch) || desk.shown_id() != Some(plan) {
                    return;
                }
                match done {
                    Ok(p) if p.id == plan => shown.set(Some(p)),
                    Ok(_) => {}
                    Err(message) => problem.set(Some(message)),
                }
            });
        }
    });
    let cancel = move |_| {
        cancelling.set(true);
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, Job>(&format!("/jobs/{job}/cancellation"), &serde_json::json!({}))
                    .await;
            cancelling.set(false);
            match done {
                Ok(_) => problem.set(None),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <p class="notice" role="status">
            {format!("Repair job #{job} ")}
            {move || match live.book.with(|b| b.get(job).cloned()) {
                Some(l) if l.job.state.is_final() => {
                    let outcome = match &l.job.error {
                        Some(error) => format!("ended: {error}"),
                        None => "ended.".to_string(),
                    };
                    view! { {state_view(l.job.state)} " " <span>{outcome}</span> }.into_any()
                }
                Some(l) => {
                    let asked = l.job.cancel_requested_at.is_some();
                    view! {
                        {state_view(l.job.state)}
                        " "
                        {progress_view(&l)}
                        {if asked {
                            view! {
                                <span class="note">
                                    " You cancelled it: the change it started finishes, and the rest are skipped."
                                </span>
                            }
                            .into_any()
                        } else {
                            view! {
                                " "
                                <button type="button" disabled=move || cancelling.get() on:click=cancel>
                                    "Cancel the repair"
                                </button>
                            }
                            .into_any()
                        }}
                    }
                    .into_any()
                }
                None => view! { <A href="/activity">"Follow it in Activity"</A> }.into_any(),
            }}
        </p>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// The Kept files tab.
#[component]
pub fn SourceKept(name: String) -> impl IntoView {
    let base = format!("/source-sets/{}", api::segment(&name));
    view! {
        <KeptFiles
            files_path=format!("{base}/kept-files")
            plan_path=format!("{base}/kept-deletions")
            approval_prefix="/source-kept-deletions"
            kept_by="repair"
            title="Kept files"
            lede="What repairs replaced or set aside, kept in the source set. Audits do not read them. Nothing here \
                is deleted until you approve deleting it."
        />
    }
}

/// Kept files of a source set or an additions set, and the approved deletion of the ones you pick.
#[component]
pub fn KeptFiles(
    files_path: String,
    plan_path: String,
    approval_prefix: &'static str,
    /// What `plan` of each kept file names: the repair that kept it, or the addition retired.
    kept_by: &'static str,
    title: &'static str,
    lede: &'static str,
) -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let files = {
        let path = files_path;
        LocalResource::new(move || {
            live.settled.track();
            refresh.track();
            let path = path.clone();
            async move { api::get::<Vec<SourceKeptFile>>(&path).await }
        })
    };
    let picked = RwSignal::new(Vec::<i64>::new());
    let deletion = RwSignal::new(None::<SourceKeptDeletion>);
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    let plan = {
        let path = plan_path;
        move |_| {
            working.set(true);
            let body = PlanKeptDeletion {
                ids: picked.get_untracked(),
                all: false,
            };
            let path = path.clone();
            leptos::task::spawn_local(async move {
                let planned = api::post::<_, SourceKeptDeletion>(&path, &body).await;
                working.set(false);
                match planned {
                    Ok(d) => {
                        problem.set(None);
                        deletion.set(Some(d));
                    }
                    Err(message) => problem.set(Some(message)),
                }
            });
        }
    };
    let approve = move |_| {
        let Some(d) = deletion.get_untracked() else {
            return;
        };
        working.set(true);
        let body = ApprovePlan {
            digest: d.digest.clone(),
        };
        leptos::task::spawn_local(async move {
            let done = api::post::<_, SourceKeptDeletion>(
                &format!("{approval_prefix}/{}/approval", d.id),
                &body,
            )
            .await;
            working.set(false);
            match done {
                Ok(d) => {
                    picked.set(Vec::new());
                    deletion.set(Some(d));
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let list = move || match files.get() {
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
                        let id = f.id;
                        let checked = move || picked.with(|p| p.contains(&id));
                        view! {
                            <li>
                                <label>
                                    <input
                                        type="checkbox"
                                        prop:checked=checked
                                        on:change=move |_| {
                                            picked.update(|p| {
                                                if let Some(at) = p.iter().position(|x| *x == id) {
                                                    p.remove(at);
                                                } else {
                                                    p.push(id);
                                                }
                                            })
                                        }
                                    />
                                    " "
                                    <code>{f.name.clone()}</code>
                                </label>
                                {format!(
                                    " as {RETENTION}/{}/{}: {}, {} bytes, {kept_by} #{}, {}",
                                    f.generation,
                                    f.kept_name,
                                    f.reason,
                                    f.file.size,
                                    f.plan,
                                    when(&f.kept_at),
                                )}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
        .into_any(),
    };
    let review = move || {
        deletion.get().map(|d| {
            let open = d.approval.is_none();
            let count = d.content.files.len();
            view! {
                <h3>{format!("Deleting kept files, plan #{}", d.id)}</h3>
                <p>"Digest " <code class="hash">{d.digest.clone()}</code></p>
                <ul class="plain">
                    {d.content.files.iter().map(|f| {
                        let outcome = d.outcomes.iter().find(|o| o.id == f.id).map(|o| match (o.deleted, &o.detail) {
                            (true, _) => ": deleted".to_string(),
                            (false, Some(why)) => format!(": left, {why}"),
                            (false, None) => ": left".to_string(),
                        });
                        view! {
                            <li class="removal">
                                <code>{format!("{RETENTION}/{}/{}", f.generation, f.kept_name)}</code>
                                {format!(" (was {})", f.name)}
                                {outcome}
                            </li>
                        }
                    }).collect_view()}
                </ul>
                {open.then(|| view! {
                    <div class="form">
                        <button type="button" class="primary" disabled=move || working.get() on:click=approve>
                            {format!("Approve and delete {count} files")}
                        </button>
                    </div>
                })}
            }
        })
    };
    view! {
        <section aria-label=title>
            <h2>{title}</h2>
            <p class="lede">{lede}</p>
            {list}
            <div class="form">
                <button type="button" disabled=move || working.get() || picked.with(Vec::is_empty) on:click=plan>
                    "Plan deleting the files you picked"
                </button>
            </div>
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// A repair plan of `plan`, open for review, in the Repair tab's review: for the filtering benchmark.
#[cfg(feature = "bench")]
pub(crate) fn bench_plan(plan: RepairPlan) -> impl IntoView {
    let desk = RepairDesk::new(plan.content.source_set.clone(), "Bench".to_string());
    view! { <PlanShown desk plan replaced=false /> }
}
