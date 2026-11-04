//! The lifecycle of registrations in the workbench: unregistering one after a confirmation that names what holds it,
//! the history of what was unregistered, deleting what it left kept aside, and forgetting it.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use serde::Deserialize;
use title_select_api::build::PlanKeptDeletion;
use title_select_api::lifecycle::{
    AdditionsSetAside, FolderMove, Forgotten, Holds, PlanFolderMove, RegistrationKind,
    UnregisteredRecord, Unregistration,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::events::use_live;
use crate::model::when;
use crate::parts::list::{Clip, Column, List, ListRow, Width, last_good_rows};

/// Where the unregistered history is.
pub const UNREGISTERED: &str = "/settings/unregistered";

/// What a registration is unregistering: where to ask what holds it, and where to unregister it.
#[derive(Clone)]
pub struct Paths {
    pub holds: String,
    pub unregister: String,
}

/// An "Unregister" action: it asks what holds the registration, says so, and otherwise asks you to confirm. `what`
/// names it ("the source set Foo"); `left` says what happens to its files.
#[component]
pub fn UnregisterAction(what: String, paths: Paths, left: &'static str) -> impl IntoView {
    let go = navigator();
    let asking = RwSignal::new(false);
    let holds = RwSignal::new(None::<Result<Vec<String>, String>>);
    let problem = RwSignal::new(None::<String>);
    let paths = StoredValue::new(paths);
    let ask = move |_| {
        asking.set(true);
        holds.set(None);
        problem.set(None);
        let path = paths.with_value(|p| p.holds.clone());
        leptos::task::spawn_local(async move {
            holds.set(Some(api::get::<Holds>(&path).await.map(|h| h.holds)));
        });
    };
    let unregister = Action::new_local(move |_: &()| {
        let path = paths.with_value(|p| p.unregister.clone());
        async move { api::post::<_, Unregistration>(&path, &()).await }
    });
    Effect::new(move || match unregister.value().get() {
        Some(Ok(_)) => go.run(UNREGISTERED.to_string()),
        Some(Err(message)) => problem.set(Some(message)),
        None => {}
    });
    let what = StoredValue::new(what);
    view! {
        <section class="lifecycle" aria-label="Unregister">
            <Show
                when=move || asking.get()
                fallback=move || view! {
                    <button type="button" on:click=ask>"Unregister…"</button>
                }
            >
                {move || match holds.get() {
                    None => view! { <Loading what="what holds it" /> }.into_any(),
                    Some(Err(message)) => view! { <Problem message /> }.into_any(),
                    Some(Ok(holds)) if !holds.is_empty() => view! {
                        <p>{format!("You can't unregister {} yet:", what.get_value())}</p>
                        <ul>{holds.into_iter().map(|h| view! { <li>{h}</li> }).collect_view()}</ul>
                        <button type="button" on:click=move |_| asking.set(false)>"Close"</button>
                    }
                    .into_any(),
                    Some(Ok(_)) => view! {
                        <p>
                            {format!(
                                "Unregister {}? {left} Its history stays readable under unregistered in settings.",
                                what.get_value()
                            )}
                        </p>
                        <div class="form">
                            <button
                                type="button"
                                class="primary"
                                disabled=move || unregister.pending().get()
                                on:click=move |_| { unregister.dispatch_local(()); }
                            >
                                "Unregister"
                            </button>
                            <button type="button" on:click=move |_| asking.set(false)>"Cancel"</button>
                        </div>
                    }
                    .into_any(),
                }}
            </Show>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// A plan of deleting kept files, of either shape: the store's kept files of a play library folder, or of a source
/// set or additions set.
#[derive(Debug, Clone, Deserialize)]
struct KeptPlan {
    id: i64,
    digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Kind,
    Name,
    Unregistered,
    Kept,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row(UnregisteredRecord);

/// The kind as a cell or a title begins it.
fn kind_label(kind: RegistrationKind) -> String {
    let label = kind.label();
    let mut chars = label.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

impl ListRow for Row {
    type Col = Col;
    const COLUMNS: &'static [Col] = &[Col::Kind, Col::Name, Col::Unregistered, Col::Kept];

    fn column(col: Col) -> Column {
        match col {
            Col::Kind => Column::new("kind", "Kind", Width::Flex { min: 9.0, share: 1 }).sortable(),
            Col::Name => Column::new(
                "name",
                "Name",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            Col::Unregistered => {
                Column::new("unregistered", "Unregistered", Width::Fixed(13.0)).sortable()
            }
            Col::Kept => Column::new("kept", "Kept", Width::Fixed(5.0))
                .sortable()
                .numeric()
                .hint("Kept files still in its folder"),
        }
    }

    fn key(&self) -> String {
        format!("{}-{}", self.0.kind.as_str(), self.0.id)
    }

    fn title(&self) -> String {
        format!("{} {}", kind_label(self.0.kind), self.0.name)
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}/{}",
            kind_label(self.0.kind),
            self.0.name,
            self.0.root,
            self.0.path
        )
    }

    fn cell(&self, col: Col) -> AnyView {
        match col {
            Col::Kind => view! { <Clip text=kind_label(self.0.kind) /> }.into_any(),
            Col::Name => view! { <Clip text=self.0.name.clone() /> }.into_any(),
            Col::Unregistered => view! { <Clip text=when(&self.0.unregistered_at) /> }.into_any(),
            Col::Kept => self.0.kept_files.to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: Col) -> Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            Col::Kind => a.kind.cmp(&b.kind),
            Col::Name => a.name.cmp(&b.name),
            Col::Unregistered => a.unregistered_at.cmp(&b.unregistered_at),
            Col::Kept => a.kept_files.cmp(&b.kept_files),
        }
    }
}

/// Where kept files of `record` are planned for deletion, and where such a plan is approved.
fn kept_paths(record: &UnregisteredRecord) -> Option<(String, &'static str)> {
    let id = record.id;
    match record.kind {
        RegistrationKind::SourceSet => Some((
            format!("/unregistered/source_set/{id}/kept-deletions"),
            "/source-kept-deletions",
        )),
        RegistrationKind::AdditionsSet => Some((
            format!("/unregistered/additions_set/{id}/kept-deletions"),
            "/additions-kept-deletions",
        )),
        RegistrationKind::LibraryFolder => {
            Some((format!("/libraries/{id}/kept-deletions"), "/kept-deletions"))
        }
        RegistrationKind::IngestFolder => None,
    }
}

/// Everything you unregistered, whose history stays readable until you forget it.
#[component]
pub fn Unregistered() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let records = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move {
            api::get::<Vec<UnregisteredRecord>>("/unregistered")
                .await
                .map(|records| records.into_iter().map(Row).collect::<Vec<_>>())
        }
    });
    let rows = last_good_rows(move || records.get());
    view! {
        <p class="lede">
            "What you unregistered. Its audits, selections, builds and jobs stay readable here until you forget it; \
            forgetting removes that history from the store and touches no file."
        </p>
        {move || match records.get() {
            None => view! { <Loading what="what you unregistered" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="unregistered"
            label="Unregistered"
            rows
            empty="Nothing is unregistered."
            placeholder="Filter by kind, name or path"
            detail=Callback::new(move |row: Row| detail(row, refresh))
        />
    }
}

fn detail(row: Row, refresh: RwSignal<u64>) -> AnyView {
    let r = row.0;
    view! {
        <dl class="facts">
            <dt>"Where"</dt>
            <dd><code class="path">{format!("{}/{}", r.root, r.path)}</code></dd>
            <dt>"Registered"</dt>
            <dd>{when(&r.registered_at)}</dd>
            <dt>"Unregistered"</dt>
            <dd>{when(&r.unregistered_at)}</dd>
            <dt>"History"</dt>
            <dd>
                {format!(
                    "{} audits, {} selections, {} builds, {} jobs",
                    r.audits, r.selections, r.builds, r.jobs
                )}
            </dd>
            <dt>"Kept files"</dt>
            <dd>{r.kept_files.to_string()}</dd>
        </dl>
        <DeleteKept record=r.clone() refresh />
        <Forget record=r refresh />
    }
    .into_any()
}

/// Deleting every kept file an unregistered registration left, after you review and approve the plan.
#[component]
fn DeleteKept(record: UnregisteredRecord, refresh: RwSignal<u64>) -> impl IntoView {
    if record.kept_files == 0 {
        return ().into_any();
    }
    let Some((plan_path, approval)) = kept_paths(&record) else {
        return view! {
            <p class="note">
                {format!(
                    "{} kept files are left in the folder. Title Select no longer manages them; delete them yourself if you want them gone.",
                    record.kept_files
                )}
            </p>
        }
        .into_any();
    };
    let count = record.kept_files;
    let plan = RwSignal::new(None::<KeptPlan>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let plan_path = StoredValue::new(plan_path);
    let make = move |_| {
        busy.set(true);
        problem.set(None);
        let path = plan_path.get_value();
        leptos::task::spawn_local(async move {
            let body = PlanKeptDeletion {
                ids: vec![],
                all: true,
            };
            match api::post::<_, KeptPlan>(&path, &body).await {
                Ok(p) => plan.set(Some(p)),
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    let approve = move |_| {
        let Some(p) = plan.get_untracked() else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let body = title_select_api::build::ApprovePlan { digest: p.digest };
            match api::post::<_, serde_json::Value>(&format!("{approval}/{}/approval", p.id), &body)
                .await
            {
                Ok(_) => {
                    plan.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    view! {
        <section aria-label="Delete kept files">
            {move || match plan.get() {
                None => view! {
                    <button type="button" disabled=busy on:click=make>
                        {format!("Plan deleting its {count} kept files")}
                    </button>
                }
                .into_any(),
                Some(p) => view! {
                    <p>
                        {format!("Plan #{}: every kept file it left is deleted. Nothing is deleted until you approve it.", p.id)}
                    </p>
                    <div class="form">
                        <button type="button" class="primary" disabled=busy on:click=approve>
                            {format!("Approve and delete {count} kept files")}
                        </button>
                        <button type="button" on:click=move |_| plan.set(None)>"Cancel"</button>
                    </div>
                }
                .into_any(),
            }}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
    .into_any()
}

/// Forgetting an unregistered registration, after you confirm.
#[component]
fn Forget(record: UnregisteredRecord, refresh: RwSignal<u64>) -> impl IntoView {
    let asking = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let said = RwSignal::new(None::<String>);
    let (kind, id) = (record.kind, record.id);
    let held = record.kept_files > 0 && kept_paths(&record).is_some();
    let forget = Action::new_local(move |_: &()| async move {
        api::delete::<Forgotten>(&format!("/unregistered/{}/{id}", kind.as_str())).await
    });
    Effect::new(move || match forget.value().get() {
        Some(Ok(f)) => {
            said.set(Some(format!(
                "Forgot {} audits, {} selections, {} builds and {} jobs.",
                f.audits, f.selections, f.builds, f.jobs
            )));
            refresh.update(|n| *n += 1);
        }
        Some(Err(message)) => problem.set(Some(message)),
        None => {}
    });
    let explain = Arc::new(format!(
        "Forget {}? Its history is removed from the store for good. No file is touched.",
        record.name
    ));
    view! {
        <section aria-label="Forget">
            <Show
                when=move || asking.get()
                fallback=move || view! {
                    <button
                        type="button"
                        disabled=held
                        title=held.then_some("Delete its kept files first")
                        on:click=move |_| asking.set(true)
                    >
                        "Forget…"
                    </button>
                }
            >
                <p>{explain.to_string()}</p>
                <div class="form">
                    <button
                        type="button"
                        class="primary"
                        disabled=move || forget.pending().get()
                        on:click=move |_| { forget.dispatch_local(()); }
                    >
                        "Forget"
                    </button>
                    <button type="button" on:click=move |_| asking.set(false)>"Cancel"</button>
                </div>
            </Show>
            <p class="done" role="status">{move || said.get()}</p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// The lifecycle actions of a registration, folded away under a heading so that they are found but not in the way.
#[component]
pub fn LifecycleSection(children: Children) -> impl IntoView {
    view! {
        <details class="lifecycle">
            <summary>"Unregister, set aside or move"</summary>
            {children()}
        </details>
    }
}

/// A plan that only its id names here.
#[derive(Debug, Clone, Deserialize)]
struct Planned {
    id: i64,
}

/// An action that makes a plan, after you confirm, and opens it for your review: nothing changes until you approve
/// it there. `href` is where the plan with the given id is reviewed.
#[component]
pub fn PlanAction(
    label: &'static str,
    explain: String,
    path: String,
    href: Callback<i64, String>,
    /// Bumped once the plan is made, for the page to show it.
    #[prop(optional)]
    refresh: Option<RwSignal<u64>>,
) -> impl IntoView {
    let go = navigator();
    let asking = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let path = StoredValue::new(path);
    let plan = Action::new_local(move |_: &()| {
        let path = path.get_value();
        async move { api::post::<_, Planned>(&path, &()).await }
    });
    Effect::new(move || match plan.value().get() {
        Some(Ok(p)) => {
            if let Some(refresh) = refresh {
                refresh.update(|n| *n += 1);
            }
            go.run(href.run(p.id));
        }
        Some(Err(message)) => problem.set(Some(message)),
        None => {}
    });
    let explain = StoredValue::new(explain);
    view! {
        <section aria-label=label>
            <Show
                when=move || asking.get()
                fallback=move || view! {
                    <button type="button" on:click=move |_| { problem.set(None); asking.set(true); }>
                        {format!("{label}…")}
                    </button>
                }
            >
                <p>{explain.get_value()}</p>
                <div class="form">
                    <button
                        type="button"
                        class="primary"
                        disabled=move || plan.pending().get()
                        on:click=move |_| { plan.dispatch_local(()); }
                    >
                        "Make the plan"
                    </button>
                    <button type="button" on:click=move |_| asking.set(false)>"Cancel"</button>
                </div>
            </Show>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// Setting a whole additions set aside: you review which additions it retires, then approve.
#[component]
pub fn AdditionsSetAsideAction(system: String, refresh: RwSignal<u64>) -> impl IntoView {
    let go = navigator();
    let query = format!("?system={}", api::segment(&system));
    let path = StoredValue::new(format!("/additions-set/set-aside{query}"));
    let plan = RwSignal::new(None::<AdditionsSetAside>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let review = move |_| {
        busy.set(true);
        problem.set(None);
        leptos::task::spawn_local(async move {
            match api::get::<AdditionsSetAside>(&path.get_value()).await {
                Ok(p) => plan.set(Some(p)),
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    let approve = move |_| {
        let Some(p) = plan.get_untracked() else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let body = title_select_api::build::ApprovePlan { digest: p.digest };
            match api::post::<_, AdditionsSetAside>(&path.get_value(), &body).await {
                Ok(_) => {
                    plan.set(None);
                    refresh.update(|n| *n += 1);
                    go.run(UNREGISTERED.to_string());
                }
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    view! {
        <section aria-label="Set aside">
            {move || match plan.get() {
                None => view! {
                    <button type="button" disabled=busy on:click=review>"Set aside…"</button>
                }
                .into_any(),
                Some(p) => {
                    let n = p.additions.len();
                    view! {
                        <p>
                            {format!(
                                "Setting the additions set aside retires its {n} active additions: each file becomes a \
                                kept file, renamed in place, and the set is unregistered. Nothing is copied or deleted."
                            )}
                        </p>
                        <ul class="plain">
                            {p.additions.into_iter().map(|a| view! { <li><code>{a}</code></li> }).collect_view()}
                        </ul>
                        <div class="form">
                            <button type="button" class="primary" disabled=busy on:click=approve>
                                "Approve and set aside"
                            </button>
                            <button type="button" on:click=move |_| plan.set(None)>"Cancel"</button>
                        </div>
                    }
                    .into_any()
                }
            }}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// Moving or renaming a play library folder within its root: a plan that renames it in place, which you approve.
#[component]
pub fn MoveFolderAction(folder: i64, from: String, refresh: RwSignal<u64>) -> impl IntoView {
    let to = RwSignal::new(String::new());
    let plan = RwSignal::new(None::<FolderMove>);
    let problem = RwSignal::new(None::<String>);
    let said = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let asking = RwSignal::new(false);
    let make = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        problem.set(None);
        let body = PlanFolderMove {
            path: to.get_untracked(),
        };
        leptos::task::spawn_local(async move {
            match api::post::<_, FolderMove>(&format!("/libraries/{folder}/moves"), &body).await {
                Ok(p) => plan.set(Some(p)),
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    let approve = move |_| {
        let Some(p) = plan.get_untracked() else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let body = title_select_api::build::ApprovePlan { digest: p.digest };
            let path = format!("/library-moves/{}/approval", p.id);
            match api::post::<_, FolderMove>(&path, &body).await {
                Ok(done) => {
                    plan.set(None);
                    asking.set(false);
                    said.set(Some(format!("Moved to {}.", done.to_path)));
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
            busy.set(false);
        });
    };
    view! {
        <section aria-label="Move">
            <Show
                when=move || asking.get()
                fallback=move || view! {
                    <button type="button" on:click=move |_| { said.set(None); asking.set(true); }>"Move…"</button>
                }
            >
                {move || match plan.get() {
                    None => view! {
                        <form class="form" on:submit=make>
                            <label>
                                "New path in its root"
                                <input
                                    type="text"
                                    required
                                    prop:value=move || to.get()
                                    on:input=move |ev| to.set(event_target_value(&ev))
                                />
                            </label>
                            <button type="submit" disabled=busy>"Plan the move"</button>
                            <button type="button" on:click=move |_| asking.set(false)>"Cancel"</button>
                        </form>
                    }
                    .into_any(),
                    Some(p) => view! {
                        <p>
                            "The folder is renamed in place, files and all, from " <code>{p.from_path.clone()}</code>
                            " to " <code>{p.to_path.clone()}</code> ". Nothing is copied."
                        </p>
                        <div class="form">
                            <button type="button" class="primary" disabled=busy on:click=approve>
                                "Approve and move"
                            </button>
                            <button type="button" on:click=move |_| plan.set(None)>"Cancel"</button>
                        </div>
                    }
                    .into_any(),
                }}
            </Show>
            <p class="done" role="status">{move || said.get()}</p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
            <p class="note">{format!("Now at {from}.")}</p>
        </section>
    }
}
