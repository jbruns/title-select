use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use title_select_api::additions::NewAddition;
use title_select_api::build::{ApprovePlan, PlanKeptDeletion, RETENTION};
use title_select_api::repair::{
    PlanRepair, RepairChoice, RepairPlan, RepairStep, SourceKeptDeletion, SourceKeptFile,
};
use title_select_api::storage::{Storage, StorageFormat};
use title_select_api::{Job, SourceSetRecord};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::when;
use crate::repair_model::{fetch_after, repair_to_show, unfinished_note, unplanned};
use crate::screens::additions::{AdditionFields, new_addition};
use crate::screens::jobs::{progress_view, state_view};

/// The repair screen of a Source Set: a plan proposed from its latest audit, the choice you make for each item, every
/// path it changes, and its approval; then its kept files, and deleting them.
#[component]
pub fn RepairScreen() -> impl IntoView {
    let params = use_params_map();
    let name = move || params.read().get("name").unwrap_or_default();
    view! {
        {move || {
            let name = name();
            let set_href = format!("/source-sets/{}", api::segment(&name));
            view! {
                <h1>{format!("Repair {name}")}</h1>
                <p class="lede">
                    "A repair is proposed from the source set's latest audit, on payloads it read, and from items in \
                    ingest folders. Change any choice, then review every path it changes. Nothing in the source set \
                    changes until you approve, and nothing is deleted: what a repair replaces or sets aside is kept \
                    in the source set's kept files. "
                    <A href=set_href>"Back to the source set"</A>
                </p>
                <Repair name=name.clone() />
                <SourceKept name />
            }
        }}
    }
}

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

#[component]
fn Repair(name: String) -> impl IntoView {
    let base = format!("/source-sets/{}", api::segment(&name));
    let choices = RwSignal::new(BTreeMap::<String, RepairChoice>::new());
    // The choices the plan shown was made with; approving approves it as it was made.
    let planned_with = RwSignal::new(BTreeMap::<String, RepairChoice>::new());
    let plan = RwSignal::new(None::<RepairPlan>);
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    // The job the plan was last fetched for as it ended; kept here, as the progress shown is made again each time.
    let fetched = RwSignal::new(None::<i64>);
    // What each item you choose to add as an operator addition is added as, by key; its system defaults to the
    // source set's DAT.
    let adding = RwSignal::new(BTreeMap::<String, RwSignal<NewAddition>>::new());
    let system = RwSignal::new(None::<String>);
    {
        let path = base.clone();
        leptos::task::spawn_local(async move {
            if let Ok(set) = api::get::<SourceSetRecord>(&path).await {
                system.set(Some(set.dat));
            }
        });
    }
    let query = use_query_map();
    let wanted = query
        .read_untracked()
        .get("plan")
        .and_then(|id| id.parse::<i64>().ok());
    {
        // An approved repair that hasn't finished, or the one the link named, is shown rather than a new proposal.
        let path = format!("{base}/repairs");
        leptos::task::spawn_local(async move {
            match api::get::<Vec<RepairPlan>>(&path).await {
                Ok(plans) => {
                    if let Some(p) = repair_to_show(&plans, wanted) {
                        plan.set(Some(p.clone()));
                    }
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    }
    let propose = {
        let path = format!("{base}/repairs");
        move || {
            working.set(true);
            let chosen = choices.get_untracked();
            let shown = plan.get_untracked();
            let additions = adding.with_untracked(|adding| {
                adding
                    .iter()
                    .filter(|(key, _)| {
                        let choice = chosen.get(*key).copied().or_else(|| {
                            shown.as_ref().and_then(|p| {
                                p.content
                                    .items
                                    .iter()
                                    .find(|i| &i.key == *key)
                                    .map(|i| i.chosen)
                            })
                        });
                        choice == Some(RepairChoice::AddAddition)
                    })
                    .map(|(key, a)| (key.clone(), a.get_untracked()))
                    .collect()
            });
            let body = PlanRepair {
                choices: chosen.clone(),
                additions,
            };
            let path = path.clone();
            leptos::task::spawn_local(async move {
                let planned = api::post::<_, RepairPlan>(&path, &body).await;
                working.set(false);
                match planned {
                    Ok(p) => {
                        problem.set(None);
                        planned_with.set(chosen);
                        plan.set(Some(p));
                    }
                    Err(message) => problem.set(Some(message)),
                }
            });
        }
    };
    let approve = move |_| {
        let Some(p) = plan.get_untracked() else {
            return;
        };
        working.set(true);
        let body = ApprovePlan {
            digest: p.digest.clone(),
        };
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, RepairPlan>(&format!("/repairs/{}/approval", p.id), &body).await;
            working.set(false);
            match done {
                Ok(p) => {
                    problem.set(None);
                    choices.set(BTreeMap::new());
                    planned_with.set(BTreeMap::new());
                    plan.set(Some(p));
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let first = propose.clone();
    let again = propose.clone();
    let review = move || {
        plan.get().map(|p| {
            let open = p.approval.is_none();
            let again = again.clone();
            let items = p
                .content
                .items
                .iter()
                .map(|item| {
                    let key = item.key.clone();
                    let outcome = p.outcomes.iter().find(|o| o.item == item.key).map(|o| {
                        let what = match (&o.error, o.done) {
                            (Some(e), _) => format!("Not done: {e}"),
                            (None, true) => "Done".to_string(),
                            (None, false) => "Not done".to_string(),
                        };
                        let detail = o.detail.join("; ");
                        view! { <p>{what}{(!detail.is_empty()).then(|| format!(" ({detail})"))}</p> }
                    });
                    let options = item
                        .choices
                        .iter()
                        .map(|c| {
                            let selected = *c == item.chosen;
                            view! { <option value=c.label() selected=selected>{c.label()}</option> }
                        })
                        .collect_view();
                    let offered = item.choices.clone();
                    let default = item.default;
                    // An item that can be added as an operator addition shows what it would be added as.
                    let addable = item.choices.contains(&RepairChoice::AddAddition).then(|| {
                        adding.with_untracked(|a| a.get(&key).copied()).unwrap_or_else(|| {
                            let path = item.path.clone().unwrap_or_else(|| key.clone());
                            let fields = RwSignal::new(item.addition.clone().unwrap_or_else(|| {
                                new_addition(&path, system.get_untracked().as_deref())
                            }));
                            adding.update_untracked(|a| {
                                a.insert(key.clone(), fields);
                            });
                            fields
                        })
                    });
                    let shows = {
                        let key = key.clone();
                        let planned = item.chosen;
                        move || {
                            choices.with(|m| m.get(&key).copied().unwrap_or(planned))
                                == RepairChoice::AddAddition
                        }
                    };
                    view! {
                        <li>
                            <strong>{item.kind.label()}</strong>
                            ": "
                            {item.summary.clone()}
                            {format!(" (proposed: {})", default.label())}
                            {open.then(|| view! {
                                " "
                                <select
                                    aria-label=format!("What to do with {key}")
                                    on:change=move |ev| {
                                        let value = event_target_value(&ev);
                                        if let Some(c) = offered.iter().find(|c| c.label() == value) {
                                            let c = *c;
                                            choices.update(|m| {
                                                if c == default {
                                                    m.remove(&key);
                                                } else {
                                                    m.insert(key.clone(), c);
                                                }
                                            });
                                        }
                                    }
                                >
                                    {options}
                                </select>
                            })}
                            {addable.map(|value| {
                                let shows = shows.clone();
                                view! {
                                    <Show when=move || open && shows()>
                                        <p>"Add it as: " <AdditionFields value /></p>
                                    </Show>
                                }
                            })}
                            {item.addition.clone().filter(|_| !open).map(|a| view! {
                                <p>{format!("Added as the operator addition {} of {}", a.name, a.system)}
                                {a.parent.map(|p| format!(", a version of {p}"))}</p>
                            })}
                            {item.note.clone().map(|n| view! { <p class="note">{n}</p> })}
                            {outcome}
                        </li>
                    }
                })
                .collect_view();
            let steps = p
                .content
                .steps
                .iter()
                .map(|s| {
                    let (line, class) = step_line(&s.step);
                    view! { <li class=class>{line}</li> }
                })
                .collect_view();
            let removals = p
                .content
                .steps
                .iter()
                .filter(|s| matches!(s.step, RepairStep::Keep { .. }))
                .count();
            let notes = p
                .content
                .notes
                .iter()
                .map(|n| view! { <li>{n.clone()}</li> })
                .collect_view();
            let count = p.content.steps.len();
            let changed = {
                let p = p.clone();
                move || {
                    let adding = adding.with(|a| {
                        a.iter()
                            .map(|(k, v)| (k.clone(), v.get()))
                            .collect::<BTreeMap<_, _>>()
                    });
                    planned_with.with(|planned| choices.with(|c| unplanned(&p, planned, c, &adding).len()))
                }
            };
            let held = {
                let changed = changed.clone();
                move || working.get() || changed() != 0
            };
            let audit_href = format!("/audits/{}", p.content.audit);
            view! {
                <h3>{format!("Repair plan #{}", p.id)}</h3>
                <p>
                    "From " <A href=audit_href>{format!("audit #{}", p.content.audit)}</A>
                    " · digest " <code class="hash">{p.digest.clone()}</code>
                    {format!(
                        " · it binds the {} entries of the source set as they are now; anything changing there, or \
                         another audit, before you approve means planning again",
                        p.content.fingerprint.len()
                    )}
                    {p.content.creates_marker.then_some(
                        ". It is this source set's first repair, so it writes the source set's .title-select marker",
                    )}
                </p>
                <p class="note">{storage_line(&p.content.storage)}</p>
                {if p.content.items.is_empty() {
                    view! { <p class="note">"Nothing to repair."</p> }.into_any()
                } else {
                    view! { <ul class="plain">{items}</ul> }.into_any()
                }}
                {open.then(|| view! {
                    <div class="form">
                        <button type="button" disabled=move || working.get() on:click=move |_| again()>
                            "Plan again with your choices"
                        </button>
                    </div>
                })}
                <h4>{format!("Every change, in order ({count}; {removals} kept aside)")}</h4>
                <ul class="plain">{steps}</ul>
                {(!p.content.notes.is_empty()).then(|| view! {
                    <h4>"Left as it is"</h4>
                    <ul class="plain">{notes}</ul>
                })}
                {p.job.map(|job| view! { <RepairProgress job plan=p.id shown=plan fetched /> })}
                {match unfinished_note(&p) {
                    Some(note) => Some(view! { <p class="notice" role="status">{note}</p> }.into_any()),
                    None => p.recovery_error.clone().map(|e| view! { <Problem message=e /> }.into_any()),
                }}
                {p.audit_job.map(|job| view! {
                    <p>
                        "An incremental audit was queued: "
                        <A href="/jobs">{format!("job {job}")}</A>
                    </p>
                })}
                {p.audit_owed.then(|| view! {
                    <p class="note">"An incremental audit is queued once the job running for the source set ends."</p>
                })}
                {open.then(|| view! {
                    {move || {
                        let n = changed();
                        (n > 0).then(|| view! {
                            <p class="notice" role="status">
                                {format!(
                                    "You changed {n} {} since this plan was made. Plan again with your choices to \
                                     review and approve them; approving now would approve the plan as shown.",
                                    if n == 1 { "item" } else { "items" }
                                )}
                            </p>
                        })
                    }}
                })}
                {(open && count > 0).then(|| view! {
                    <div class="form">
                        <button
                            type="button"
                            class="primary"
                            disabled=held
                            on:click=approve
                        >
                            {format!("Approve and make {count} changes")}
                        </button>
                    </div>
                })}
            }
        })
    };
    view! {
        <section aria-labelledby="repair-title">
            <h2 id="repair-title">"Repair"</h2>
            {move || {
                let first = first.clone();
                let running = plan.with(|p| {
                    p.as_ref().is_some_and(|p| p.approval.is_some() && p.finished_at.is_none())
                });
                (!running).then(|| view! {
                    <div class="form">
                        <button type="button" class="primary" disabled=move || working.get() on:click=move |_| first()>
                            "Propose a repair"
                        </button>
                    </div>
                })
            }}
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// The repair's job, live: its state and progress, a way to cancel it while it runs, and the plan fetched again
/// with its outcomes once it ends.
#[component]
fn RepairProgress(
    job: i64,
    plan: i64,
    shown: RwSignal<Option<RepairPlan>>,
    fetched: RwSignal<Option<i64>>,
) -> impl IntoView {
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
            leptos::task::spawn_local(async move {
                match api::get::<RepairPlan>(&format!("/repairs/{plan}")).await {
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
                None => view! { <A href="/jobs">"Follow it on the jobs screen"</A> }.into_any(),
            }}
        </p>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

#[component]
fn SourceKept(name: String) -> impl IntoView {
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
