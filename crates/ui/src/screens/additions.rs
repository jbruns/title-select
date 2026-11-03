//! Additions: each system's additions set, the additions in it, and the fields every plan that adds one
//! asks for.

use leptos::prelude::*;
use title_select_api::Dat;
use title_select_api::additions::{
    Addition, AdditionState, AdditionsCheck, AdditionsFinding, AdditionsSet, EditAddition,
    NewAddition,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::screens::repair::KeptFiles;

/// The name suggested for an addition made from the file at `path`: its file name, without folders or extension.
pub fn suggested_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => file.to_owned(),
    }
}

/// What you add a file as: its system, suggested when one is known, and its name, suggested from the file's.
pub fn new_addition(path: &str, system: Option<&str>) -> NewAddition {
    NewAddition {
        system: system.unwrap_or_default().to_owned(),
        name: suggested_name(path),
        parent: None,
    }
}

pub fn state_label(state: AdditionState) -> &'static str {
    match state {
        AdditionState::Pending => "Being published",
        AdditionState::Active => "Active",
        AdditionState::Retiring => "Being retired",
        AdditionState::Retired => "Retired",
        AdditionState::Restoring => "Being restored",
        AdditionState::Failed => "Failed",
    }
}

pub fn finding_label(finding: AdditionsFinding) -> &'static str {
    match finding {
        AdditionsFinding::Intact => "Intact",
        AdditionsFinding::Missing => "Missing",
        AdditionsFinding::Changed => "Changed",
        AdditionsFinding::Retired => "Retired, still there",
        AdditionsFinding::Unknown => "Unknown file",
        AdditionsFinding::Other => "Not a file",
    }
}

/// The system, name and parent game to add a file as an addition under, edited in place.
#[component]
pub fn AdditionFields(value: RwSignal<NewAddition>) -> impl IntoView {
    let dats = LocalResource::new(|| async { api::get::<Vec<Dat>>("/dats").await });
    let list = format!("systems-{}", js_sys::Math::random().to_bits());
    let options = move || {
        dats.get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(|d| view! { <option value=d.dat.clone()></option> })
            .collect_view()
    };
    view! {
        <span class="addition-fields">
            <label>
                "System "
                <input
                    type="text"
                    list=list.clone()
                    required
                    placeholder="provider/DAT name"
                    prop:value=move || value.with(|v| v.system.clone())
                    on:input=move |ev| value.update(|v| v.system = event_target_value(&ev))
                />
                <datalist id=list>{options}</datalist>
            </label>
            " "
            <label>
                "Name "
                <input
                    type="text"
                    required
                    prop:value=move || value.with(|v| v.name.clone())
                    on:input=move |ev| value.update(|v| v.name = event_target_value(&ev))
                />
            </label>
            " "
            <label>
                "Parent game, if any "
                <input
                    type="text"
                    prop:value=move || value.with(|v| v.parent.clone().unwrap_or_default())
                    on:input=move |ev| {
                        let parent = event_target_value(&ev);
                        value.update(|v| v.parent = (!parent.trim().is_empty()).then_some(parent))
                    }
                />
            </label>
        </span>
    }
}

/// Every additions set, with its additions.
#[component]
pub fn Additions() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let sets = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async { api::get::<Vec<AdditionsSet>>("/additions-sets").await }
    });
    view! {
        <h1>"Additions"</h1>
        <p class="lede">
            "Files no DAT knows that you chose to keep, each known by its own hashes and a name you give it, in an \
            additions set per system. A selection profile that opts in for a system builds them alongside the \
            source set. Add one from an ingest folder, a repair of a source set or a cleanup of a library folder. \
            An additions job publishes each one after you approve it, and moves the file of one you retire into \
            the additions set's kept files."
        </p>
        {move || match sets.get() {
            None => view! { <Loading what="the additions sets" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(sets)) if sets.is_empty() => {
                view! { <p class="note">"You have no additions yet."</p> }.into_any()
            }
            Some(Ok(sets)) => sets
                .into_iter()
                .map(|set| view! { <SetView set refresh /> })
                .collect_view()
                .into_any(),
        }}
    }
}

/// One additions set, with its additions, its check and its kept files.
#[component]
pub fn SetView(set: AdditionsSet, refresh: RwSignal<u64>) -> impl IntoView {
    let check = RwSignal::new(None::<AdditionsCheck>);
    let problem = RwSignal::new(None::<String>);
    let system = set.system.clone();
    let run_check = move |_| {
        let path = format!("/additions-set/checks?system={}", api::segment(&system));
        leptos::task::spawn_local(async move {
            match api::post::<_, AdditionsCheck>(&path, &()).await {
                Ok(c) => {
                    problem.set(None);
                    check.set(Some(c));
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let found = move || {
        check.get().map(|c| {
            let summary = match c.drifted() {
                true => "The check found drift: anything below that is not intact.",
                false => "The check found every active addition intact, and nothing else.",
            };
            view! {
                <p>{format!("Checked {}. {summary}", when(&c.checked_at))}</p>
                <ul class="plain">
                    {c
                        .entries
                        .into_iter()
                        .filter(|e| e.finding != AdditionsFinding::Intact)
                        .map(|e| {
                            view! {
                                <li>
                                    <code>{e.name}</code>
                                    {format!(": {}", finding_label(e.finding))}
                                    {e.detail.map(|d| format!(", {d}"))}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
        })
    };
    let count = set.additions.len();
    let query = format!("?system={}", api::segment(&set.system));
    let (kept_path, plan_path) = (
        format!("/additions-set/kept-files{query}"),
        format!("/additions-set/kept-deletions{query}"),
    );
    let leftovers = (!set.leftovers.is_empty()).then(|| {
        view! {
            <h3>"Found entries"</h3>
            <p class="note">
                "Folders an addition was being published through when the service stopped. Nothing reads or \
                deletes them; remove them yourself once you have looked at them."
            </p>
            <ul class="plain">
                {set
                    .leftovers
                    .iter()
                    .map(|l| {
                        view! {
                            <li>
                                <code>{l.name.clone()}</code>
                                {format!(", found {}", when(&l.found_at))}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    });
    view! {
        <section aria-label=format!("Additions of {}", set.system)>
            <h2>{set.system.clone()}</h2>
            <p>
                "In root " <code>{set.root.clone()}</code> " at " <code>{set.path.clone()}</code>
                {format!(" · {count} additions")}
            </p>
            <div class="form">
                <button type="button" on:click=run_check>
                    "Check the additions set for drift"
                </button>
            </div>
            {found}
            {move || problem.get().map(|message| view! { <Problem message /> })}
            <ul class="plain">
                {set
                    .additions
                    .into_iter()
                    .map(|a| view! { <AdditionView addition=a refresh /> })
                    .collect_view()}
            </ul>
            {leftovers}
            <KeptFiles
                files_path=kept_path
                plan_path=plan_path
                approval_prefix="/additions-kept-deletions"
                kept_by="addition"
                title="Kept files"
                lede="Files of additions you retired, kept in the additions set. Builds and checks do not read them. \
                    Restore an addition to move its file back. Nothing here is deleted until you approve deleting it."
            />
        </section>
    }
}

#[component]
fn AdditionView(addition: Addition, refresh: RwSignal<u64>) -> impl IntoView {
    let id = addition.id;
    let name = RwSignal::new(addition.name.clone());
    let parent = RwSignal::new(addition.parent.clone().unwrap_or_default());
    let problem = RwSignal::new(None::<String>);
    let editable = matches!(
        addition.state,
        AdditionState::Active | AdditionState::Pending
    );
    let retirable = addition.state == AdditionState::Active;
    let restorable = addition.state == AdditionState::Retired;
    let save = move |_| {
        let parent = parent.get_untracked();
        let body = EditAddition {
            name: Some(name.get_untracked()),
            parent: Some((!parent.trim().is_empty()).then_some(parent)),
        };
        leptos::task::spawn_local(async move {
            match api::patch::<_, Addition>(&format!("/additions/{id}"), &body).await {
                Ok(_) => {
                    problem.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let retire = move |_| {
        leptos::task::spawn_local(async move {
            match api::post::<_, Addition>(&format!("/additions/{id}/retirement"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let restore = move |_| {
        leptos::task::spawn_local(async move {
            match api::post::<_, Addition>(&format!("/additions/{id}/restoration"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let hashes = addition.rom.as_ref().map(|rom| {
        view! {
            <p class="data">
                {format!("{} · {} bytes", rom.name, rom.size)}
                {rom.crc.clone().map(|h| format!(" · CRC32 {h}"))}
                {rom.md5.clone().map(|h| format!(" · MD5 {h}"))}
                {rom.sha1.clone().map(|h| format!(" · SHA-1 {h}"))}
                {rom.sha256.clone().map(|h| format!(" · SHA-256 {}", short_sha(&h)))}
            </p>
        }
    });
    view! {
        <li>
            <strong>{addition.name.clone()}</strong>
            " "
            <span class="state">{state_label(addition.state)}</span>
            {format!(
                " · from the {} {} · stored as {}",
                addition.source.label(),
                addition.origin,
                addition.file_name
            )}
            {addition.parent.clone().map(|p| format!(" · a version of {p}"))}
            {format!(" · approved by {} {}", addition.approved_by, when(&addition.created_at))}
            {addition.error.clone().map(|e| view! { <p class="problem">{e}</p> })}
            {hashes}
            {editable.then(|| view! {
                <div class="form">
                    <label>
                        "Name "
                        <input
                            type="text"
                            prop:value=move || name.get()
                            on:input=move |ev| name.set(event_target_value(&ev))
                        />
                    </label>
                    <label>
                        "Parent game, if any "
                        <input
                            type="text"
                            prop:value=move || parent.get()
                            on:input=move |ev| parent.set(event_target_value(&ev))
                        />
                    </label>
                    <button type="button" on:click=save>"Save the name and parent"</button>
                    {retirable.then(|| view! {
                        <button type="button" on:click=retire>"Retire it from builds"</button>
                    })}
                </div>
            })}
            {restorable.then(|| view! {
                <div class="form">
                    <button type="button" on:click=restore>"Restore it to builds"</button>
                </div>
            })}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </li>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggested_name_drops_folders_and_the_extension() {
        assert_eq!(suggested_name("hacks/My Hack (v1.1).a26"), "My Hack (v1.1)");
        assert_eq!(suggested_name("Demo.v2.zip"), "Demo.v2");
        assert_eq!(suggested_name(".hidden"), ".hidden");
        assert_eq!(suggested_name("noext"), "noext");
    }
}
