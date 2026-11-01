use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use title_select_api::build::{ApprovePlan, PlanKeptDeletion, RETENTION};
use title_select_api::repair::{
    PlanRepair, RepairChoice, RepairPlan, RepairStep, SourceKeptDeletion, SourceKeptFile,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::when;

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

fn step_line(step: &RepairStep) -> (String, &'static str) {
    let class = match step {
        RepairStep::Keep { .. } => "removal",
        _ => "",
    };
    (step.describe(), class)
}

#[component]
fn Repair(name: String) -> impl IntoView {
    let base = format!("/source-sets/{}", api::segment(&name));
    let choices = RwSignal::new(BTreeMap::<String, RepairChoice>::new());
    let plan = RwSignal::new(None::<RepairPlan>);
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    let propose = {
        let path = format!("{base}/repairs");
        move || {
            working.set(true);
            let body = PlanRepair {
                choices: choices.get_untracked(),
            };
            let path = path.clone();
            leptos::task::spawn_local(async move {
                let planned = api::post::<_, RepairPlan>(&path, &body).await;
                working.set(false);
                match planned {
                    Ok(p) => {
                        problem.set(None);
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
                {p.recovery_error.clone().map(|e| view! { <Problem message=e /> })}
                {p.audit_job.map(|job| view! {
                    <p>
                        "An incremental audit was queued: "
                        <A href="/jobs">{format!("job {job}")}</A>
                    </p>
                })}
                {p.audit_owed.then(|| view! {
                    <p class="note">"An incremental audit is queued once the job running for the source set ends."</p>
                })}
                {(open && count > 0).then(|| view! {
                    <div class="form">
                        <button type="button" class="primary" disabled=move || working.get() on:click=approve>
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
            <div class="form">
                <button type="button" class="primary" disabled=move || working.get() on:click=move |_| first()>
                    "Propose a repair"
                </button>
            </div>
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

#[component]
fn SourceKept(name: String) -> impl IntoView {
    let live = use_live();
    let base = format!("/source-sets/{}", api::segment(&name));
    let refresh = RwSignal::new(0u64);
    let files = {
        let path = format!("{base}/kept-files");
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
        let path = format!("{base}/kept-deletions");
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
                &format!("/source-kept-deletions/{}/approval", d.id),
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
                                    " as {RETENTION}/{}/{}: {}, {} bytes, repair #{}, {}",
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
        <section aria-labelledby="source-kept-title">
            <h2 id="source-kept-title">"Kept files"</h2>
            <p class="lede">
                "What repairs replaced or set aside, kept in the source set. Audits do not read them. Nothing here is \
                deleted until you approve deleting it."
            </p>
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
