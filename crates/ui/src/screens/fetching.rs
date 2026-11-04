//! Where fetching newer DAT revisions from Redump and MAME stands, per provider and system, with a way to fetch now.

use leptos::prelude::*;
use title_select_api::fetch::{FetchProviderStatus, FetchStatus, FetchSystem};

use crate::api;
use crate::events::use_live;
use crate::fetch_model::{
    artifact_name, failures, last_fetched, next_fetch, provider_state, system_state,
};
use crate::model::when;
use crate::parts::pending::{ActionPending, Pending};
use crate::parts::status::{StatusBadge, StatusIcon};

fn provider_name(p: &FetchProviderStatus) -> &'static str {
    p.provider.dat_provider()
}

/// The fetching panel of the DATs screen.
#[component]
pub fn Fetching() -> impl IntoView {
    let live = use_live();
    let fetched = RwSignal::new(0u32);
    let status = LocalResource::new(move || {
        live.settled.track();
        fetched.track();
        api::get::<FetchStatus>("/fetch")
    });
    let fetch = Action::new_local(move |provider: &String| {
        let path = format!("/fetch/{provider}");
        async move {
            let now: Result<FetchStatus, String> = api::post(&path, &()).await;
            fetched.update(|n| *n += 1);
            now
        }
    });
    let problem = Signal::derive(move || status.get().and_then(Result::err));
    view! {
        <section aria-labelledby="fetching">
            <h2 id="fetching">"Fetching"</h2>
            <details>
                <summary>"How fetching works"</summary>
                <p>
                    "For the systems your source sets follow, the service can fetch newer DAT revisions from Redump and from MAME's releases, and Redump's cue and SBI packs. Each provider is off until you turn it on in your configuration. Each system is fetched at most once a day, even when you fetch by hand. A fetch that fails keeps the last good DAT revision. A newer DAT revision makes the following source sets stale; fetching never starts an audit."
                </p>
                <p>
                    "No-Intro DATs are never fetched: download them yourself and drop them into an ingest folder, or import them here."
                </p>
            </details>
            <ActionPending action=fetch status="Fetching…".to_string()>
                <Pending
                    pending=Signal::derive(move || status.get().is_none())
                    status="Loading fetching…"
                    problem
                >
                    {move || {
                        status
                            .get()
                            .and_then(Result::ok)
                            .map(|now| {
                                now.providers
                                    .into_iter()
                                    .map(|p| view! { <ProviderPanel provider=p fetch /> })
                                    .collect_view()
                            })
                    }}
                </Pending>
            </ActionPending>
        </section>
    }
}

#[component]
fn ProviderPanel(
    provider: FetchProviderStatus,
    fetch: Action<String, Result<FetchStatus, String>>,
) -> impl IntoView {
    let name = provider_name(&provider);
    let key = provider.provider.as_str().to_string();
    let state = provider_state(&provider);
    let can_fetch = provider.enabled && !provider.running;
    let release = provider.release.clone().and_then(|r| {
        r.last_success
            .and_then(|a| a.release)
            .map(|tag| format!("Latest release found: {tag}."))
    });
    let note = if !provider.enabled {
        Some(format!(
            "Off. To turn it on, set {} = true under [fetch] in your configuration file and restart the service.",
            provider.provider.as_str()
        ))
    } else if provider.systems.is_empty() {
        Some(format!(
            "No source set follows a {name} DAT, so nothing is fetched."
        ))
    } else {
        None
    };
    let systems = provider.systems.clone();
    let rows = systems
        .iter()
        .map(|s| system_row(&provider, s))
        .collect_view();
    view! {
        <section aria-label=name class="fetch-provider">
            <h3>
                {name} " " <StatusBadge status=state />
            </h3>
            <button
                type="button"
                disabled=move || !can_fetch || fetch.pending().get()
                on:click=move |_| {
                    fetch.dispatch_local(key.clone());
                }
            >
                {format!("Fetch from {name} now")}
            </button>
            {release.map(|r| view! { <p>{r}</p> })}
            {note.map(|n| view! { <p>{n}</p> })}
            {(!provider.systems.is_empty())
                .then(move || {
                    view! {
                        <div class="table-scroll">
                            <table aria-label=format!("{name} systems")>
                                <thead>
                                    <tr>
                                        <th scope="col">
                                            <span class="label">"State"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"DAT"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Fetches"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Source sets"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Last fetched"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Next"</span>
                                        </th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {rows}
                                </tbody>
                            </table>
                        </div>
                        <SystemProblems systems />
                    }
                })}
        </section>
    }
}

fn system_row(provider: &FetchProviderStatus, system: &FetchSystem) -> impl IntoView + use<> {
    let state = system_state(provider, system);
    let fetches = if system.system.is_none() {
        "Can't fetch".to_string()
    } else {
        system
            .artifacts
            .iter()
            .map(|a| artifact_name(a.artifact))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let last = last_fetched(&system.artifacts).map_or_else(|| "Never".to_string(), |t| when(&t));
    let next = next_fetch(&system.artifacts).map_or_else(|| "Now".to_string(), |t| when(&t));
    view! {
        <tr>
            <td>
                <StatusIcon status=state />
            </td>
            <td>{system.dat.clone()}</td>
            <td>{fetches}</td>
            <td>{system.source_sets.join(", ")}</td>
            <td>{last}</td>
            <td>{next}</td>
        </tr>
    }
}

/// Why each system's last fetch failed, under its provider's table.
#[component]
fn SystemProblems(systems: Vec<FetchSystem>) -> impl IntoView {
    let problems: Vec<String> = systems
        .iter()
        .flat_map(|s| {
            failures(s).into_iter().map(move |(artifact, error)| {
                format!("{} {}: {error}", s.dat, artifact_name(artifact))
            })
        })
        .collect();
    (!problems.is_empty()).then(|| {
        view! {
            <ul class="problems" aria-label="Failed fetches">
                {problems.into_iter().map(|p| view! { <li>{p}</li> }).collect_view()}
            </ul>
        }
    })
}
