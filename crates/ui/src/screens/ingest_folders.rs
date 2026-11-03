use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use title_select_api::Job;
use title_select_api::additions::NewAddition;
use title_select_api::build::{ApprovePlan, RETENTION};
use title_select_api::ingest::{
    Assessment, IngestAction, IngestFolder, IngestItem, IngestKeptFile, IngestPlan, ItemState,
    PlanIngest, Proposal, RegisterIngestFolder,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::screens::additions::{AdditionFields, new_addition};

fn folder_href(root: &str) -> String {
    format!("/ingest/{}", api::segment(root))
}

fn counts(folder: &IngestFolder) -> String {
    if folder.counts.is_empty() {
        return "No items".into();
    }
    folder
        .counts
        .iter()
        .map(|(state, n)| format!("{n} {}", state.label()))
        .collect::<Vec<_>>()
        .join(", ")
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

/// The Ingest Folders: every ingest root, registered or not.
#[component]
pub fn IngestFolders() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let folders = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        api::get::<Vec<IngestFolder>>("/ingest-folders")
    });
    let problem = RwSignal::new(None::<String>);
    let register = move |root: String| {
        problem.set(None);
        leptos::task::spawn_local(async move {
            let body = RegisterIngestFolder { root };
            match api::post::<_, IngestFolder>("/ingest-folders", &body).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <h1>"Ingest folders"</h1>
        <p class="lede">
            "An ingest folder is a writable root of kind ingest where new files arrive. A scan assesses each item \
            against every source set; items leave the folder only through a plan you approve."
        </p>
        {move || problem.get().map(|message| view! { <Problem message /> })}
        {move || match folders.get() {
            None => view! { <Loading what="the ingest folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(folders)) if folders.is_empty() => view! {
                <p class="note">
                    "No ingest folders. Declare a root with kind = \"ingest\" and writable = true in your \
                    configuration, then restart the service."
                </p>
            }
            .into_any(),
            Some(Ok(folders)) => view! {
                <ul class="plain">
                    {folders
                        .into_iter()
                        .map(|f| {
                            let text = format!("{}: {}", f.root, f.path);
                            let root = f.root.clone();
                            match f.id {
                                Some(_) => view! {
                                    <li>
                                        <A href=folder_href(&f.root)>{text}</A>
                                        {format!(" · {}", counts(&f))}
                                        {f.last_scan_at.as_deref().map(|at| format!(" · scanned {}", when(at)))}
                                    </li>
                                }
                                .into_any(),
                                None => view! {
                                    <li>
                                        {text}
                                        " · not registered "
                                        <button type="button" on:click=move |_| register(root.clone())>
                                            "Register it"
                                        </button>
                                    </li>
                                }
                                .into_any(),
                            }
                        })
                        .collect_view()}
                </ul>
            }
            .into_any(),
        }}
    }
}

#[component]
pub fn IngestFolderScreen() -> impl IntoView {
    let params = use_params_map();
    let root = move || params.read().get("root").unwrap_or_default();
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let folder = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let root = root();
        async move {
            api::get::<IngestFolder>(&format!("/ingest-folders/{}", api::segment(&root))).await
        }
    });
    move || match folder.get() {
        None => view! { <Loading what="the ingest folder" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Ingest folder"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(folder)) => view! { <FolderView folder refresh /> }.into_any(),
    }
}

#[component]
fn FolderView(folder: IngestFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let root = folder.root.clone();
    let base = format!("/ingest-folders/{}", api::segment(&root));
    let problem = RwSignal::new(None::<String>);
    let scan = {
        let base = base.clone();
        move |_| {
            problem.set(None);
            let path = format!("{base}/scans");
            leptos::task::spawn_local(async move {
                match api::post::<_, Job>(&path, &()).await {
                    Ok(_) => refresh.update(|n| *n += 1),
                    Err(message) => problem.set(Some(message)),
                }
            });
        }
    };
    let schedule = match folder.scan_every_minutes {
        Some(m) => format!("Every {m} minutes, and when you ask"),
        None => "Only when you ask".into(),
    };
    view! {
        <h1>{format!("Ingest folder {}", folder.root)}</h1>
        <dl class="facts">
            <dt>"Path"</dt>
            <dd>{folder.path.clone()}</dd>
            <dt>"Settles after"</dt>
            <dd>{format!("{} seconds unchanged", folder.settle_secs)}</dd>
            <dt>"Scans"</dt>
            <dd>{schedule}</dd>
            <dt>"Last scan"</dt>
            <dd>{folder.last_scan_at.as_deref().map(when).unwrap_or_else(|| "Never".into())}</dd>
            <dt>"Items"</dt>
            <dd>{counts(&folder)}</dd>
        </dl>
        <div class="form">
            <button type="button" disabled=folder.id.is_none() on:click=scan>
                "Scan the folder"
            </button>
        </div>
        {move || problem.get().map(|message| view! { <Problem message /> })}
        <Items base=base.clone() refresh />
        <KeptFiles base refresh />
    }
}

/// The folder's items, with what each is, the Evidence for it and what is proposed; and plans to discard or import
/// the items you pick.
#[component]
fn Items(base: String, refresh: RwSignal<u64>) -> impl IntoView {
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
    let picked = RwSignal::new(Vec::<i64>::new());
    let plan = RwSignal::new(None::<IngestPlan>);
    let problem = RwSignal::new(None::<String>);
    let working = RwSignal::new(false);
    let provider = RwSignal::new(String::new());
    // What each item that can be added as an addition would be added as, by id.
    let adding = RwSignal::new(BTreeMap::<i64, RwSignal<NewAddition>>::new());
    let request = {
        let base = base.clone();
        move |action: IngestAction| {
            working.set(true);
            let provider = provider.get_untracked().trim().to_owned();
            let body = PlanIngest {
                action,
                items: picked.get_untracked(),
                provider: (action == IngestAction::DatImport && !provider.is_empty())
                    .then_some(provider),
                additions: match action {
                    IngestAction::Addition => adding.with_untracked(|adding| {
                        picked
                            .get_untracked()
                            .iter()
                            .filter_map(|id| adding.get(id).map(|a| (*id, a.get_untracked())))
                            .collect()
                    }),
                    _ => BTreeMap::new(),
                },
            };
            let path = format!("{base}/plans");
            leptos::task::spawn_local(async move {
                let planned = api::post::<_, IngestPlan>(&path, &body).await;
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
    let dismiss = move |id: i64| {
        leptos::task::spawn_local(async move {
            match api::post::<_, IngestItem>(&format!("/ingest-items/{id}/dismissal"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
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
                api::post::<_, IngestPlan>(&format!("/ingest-plans/{}/approval", p.id), &body)
                    .await;
            working.set(false);
            match done {
                Ok(p) => {
                    picked.set(Vec::new());
                    plan.set(Some(p));
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let list = move || match items.get() {
        None => view! { <Loading what="the items" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(items)) => {
            let present: Vec<IngestItem> = items
                .into_iter()
                .filter(|i| !matches!(i.state, ItemState::Gone | ItemState::Moved))
                .collect();
            if present.is_empty() {
                return view! { <p class="note">"Nothing is waiting in the folder."</p> }
                    .into_any();
            }
            present
                .into_iter()
                .map(|item| {
                    let id = item.id;
                    let checked = move || picked.with(|p| p.contains(&id));
                    let movable = item.state != ItemState::Arriving;
                    let proposal = item.assessment.proposal.as_ref().map(proposal_label);
                    let assessed = item.assessed_at.is_some();
                    let dismissible = matches!(
                        item.state,
                        ItemState::Assessed | ItemState::AlreadyHeld | ItemState::Unreadable | ItemState::Unsupported
                    );
                    let state = item.state.label();
                    // Only an item holding one ROM no DAT knows can be added as an addition.
                    let addable = match &item.assessment.proposal {
                        Some(Proposal::Addition { system }) if item.assessment.payloads.len() == 1 => {
                            Some(adding.with_untracked(|a| a.get(&id).copied()).unwrap_or_else(|| {
                                let fields = RwSignal::new(new_addition(&item.name, system.as_deref()));
                                adding.update_untracked(|a| {
                                    a.insert(id, fields);
                                });
                                fields
                            }))
                        }
                        _ => None,
                    };
                    view! {
                        <li>
                            <label>
                                <input
                                    type="checkbox"
                                    disabled=!movable
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
                                <code>{item.name.clone()}</code>
                            </label>
                            " "
                            <span class="state">{state}</span>
                            {format!(" · {} bytes · seen {}", item.file.size, when(&item.first_seen_at))}
                            {item.reason.clone().map(|r| format!(": {r}"))}
                            {proposal.map(|p| view! { <p>{p}</p> })}
                            {assessed.then(|| evidence(&item.assessment))}
                            {addable.map(|value| view! {
                                <p>"To add it as an addition: " <AdditionFields value /></p>
                            })}
                            {dismissible.then(|| view! {
                                <button type="button" on:click=move |_| dismiss(id)>"Dismiss until it changes"</button>
                            })}
                        </li>
                    }
                })
                .collect_view()
                .into_any()
        }
    };
    let discard = request.clone();
    let add = request.clone();
    let review = move || {
        plan.get().map(|p| {
            let open = p.approval.is_none();
            let action = match p.content.action {
                IngestAction::Discard => "Discarding",
                IngestAction::DatImport => "Importing DATs",
                IngestAction::Addition => "Adding additions",
            };
            view! {
                <h3>{format!("{action}, plan #{}", p.id)}</h3>
                <p>
                    "Digest " <code class="hash">{p.digest.clone()}</code>
                    {format!(
                        " · it binds the {} entries of the folder as they are now; anything changing there before you \
                         approve means planning again",
                        p.content.fingerprint.len()
                    )}
                </p>
                <ul class="plain">
                    {p
                        .content
                        .items
                        .iter()
                        .map(|i| {
                            let outcome = p.outcomes.iter().find(|o| o.item == i.item).map(|o| {
                                match (&o.error, &o.detail) {
                                    (Some(e), _) => format!(": {e}"),
                                    (None, Some(d)) => format!(": {d}"),
                                    (None, None) if o.moved => ": moved".to_string(),
                                    (None, None) => ": left as it is".to_string(),
                                }
                            });
                            view! {
                                <li>
                                    <code>{i.name.clone()}</code>
                                    " to "
                                    <code>{i.destination.clone()}</code>
                                    {format!(": {}", i.reason)}
                                    {i.provider.clone().map(|p| format!(", as a DAT from {p}"))}
                                    {outcome}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
                {open
                    .then(|| {
                        view! {
                            <div class="form">
                                <button
                                    type="button"
                                    class="primary"
                                    disabled=move || working.get()
                                    on:click=approve
                                >
                                    {format!("Approve and move {} items", p.content.items.len())}
                                </button>
                            </div>
                        }
                    })}
            }
        })
    };
    view! {
        <section aria-labelledby="items-title">
            <h2 id="items-title">"Items"</h2>
            <p class="lede">
                "Each item is assessed once its size and times stop changing. Pick items to plan a discard, which \
                keeps them aside in the folder's kept files, to import a DAT, or to add an item no DAT knows as an \
                addition: repacked into its system's additions set, its original deleted only once the copy \
                reads back exactly. Nothing moves until you approve."
            </p>
            <ul class="plain">{list}</ul>
            <div class="form">
                <button
                    type="button"
                    disabled=move || working.get() || picked.with(Vec::is_empty)
                    on:click=move |_| discard(IngestAction::Discard)
                >
                    "Plan discarding the items you picked"
                </button>
                <label>
                    "Provider, for a DAT no source set uses yet "
                    <input
                        type="text"
                        prop:value=move || provider.get()
                        on:input=move |ev| provider.set(event_target_value(&ev))
                    />
                </label>
                <button
                    type="button"
                    disabled=move || working.get() || picked.with(Vec::is_empty)
                    on:click=move |_| request(IngestAction::DatImport)
                >
                    "Plan importing the DATs you picked"
                </button>
                <button
                    type="button"
                    disabled=move || working.get() || picked.with(Vec::is_empty)
                    on:click=move |_| add(IngestAction::Addition)
                >
                    "Plan adding the items you picked as additions"
                </button>
            </div>
            {review}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
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
    view! {
        <section aria-labelledby="kept-title">
            <h2 id="kept-title">"Kept files"</h2>
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
                                            " as {RETENTION}/{}/{}: {}, {} bytes, {}",
                                            f.generation,
                                            f.kept_name,
                                            f.reason,
                                            f.file.size,
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
        </section>
    }
}
