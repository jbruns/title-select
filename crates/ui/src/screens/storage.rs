//! The storage policy: how source sets and play libraries store each game, as TorrentZip, loose or a loose folder,
//! set globally and for each system, as a selection profile's settings are.

use std::collections::BTreeSet;

use leptos::prelude::*;
use title_select_api::Dat;
use title_select_api::storage::{
    ScopePolicy, StorageFormat, StoragePolicy, StorageScope, SystemStorage, is_disc_system,
};

use crate::api;
use crate::app::{Loading, Problem};

const SCOPES: [(StorageScope, &str); 2] = [
    (StorageScope::SourceSets, "Source sets"),
    (StorageScope::PlayLibraries, "Play libraries"),
];

const DELETE_WARNING: &str = "With this on, a conversion deletes each original as soon as its converted file is verified, instead of keeping it in kept files. You can't get a deleted original back from Title Select. An original that changed, or whose converted file doesn't verify, is always kept.";

fn scope_mut(policy: &mut StoragePolicy, scope: StorageScope) -> &mut ScopePolicy {
    match scope {
        StorageScope::SourceSets => &mut policy.source_sets,
        StorageScope::PlayLibraries => &mut policy.play_libraries,
    }
}

fn format_label(format: StorageFormat) -> &'static str {
    match format {
        StorageFormat::TorrentZip => "TorrentZip",
        StorageFormat::Loose => "Loose, for games with one ROM",
        StorageFormat::LooseFolder => "Loose folder: a folder per game",
    }
}

/// The formats you can choose for folders of `scope`: play libraries don't store loose folders yet.
fn formats(scope: StorageScope) -> &'static [StorageFormat] {
    match scope {
        StorageScope::SourceSets => &[
            StorageFormat::TorrentZip,
            StorageFormat::Loose,
            StorageFormat::LooseFolder,
        ],
        StorageScope::PlayLibraries => &[StorageFormat::TorrentZip, StorageFormat::Loose],
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Changes `system`'s overrides in `scope`, removing them once they override nothing.
fn update_own(
    policy: RwSignal<StoragePolicy>,
    scope: StorageScope,
    system: &str,
    f: impl FnOnce(&mut SystemStorage),
) {
    policy.update(|p| {
        let systems = &mut scope_mut(p, scope).systems;
        f(systems.entry(system.to_owned()).or_default());
        if systems
            .get(system)
            .is_some_and(|s| s.format.is_none() && s.delete_originals.is_none())
        {
            systems.remove(system);
        }
    });
}

#[component]
pub fn StoragePolicyScreen() -> impl IntoView {
    let loaded = LocalResource::new(|| async {
        let policy = api::get::<StoragePolicy>("/storage-policy").await?;
        let dats = api::get::<Vec<Dat>>("/dats").await?;
        Ok::<_, String>((policy, dats))
    });
    view! {
        <h1>"Storage policy"</h1>
        <p class="lede">
            "The storage policy says how each folder stores a game: as one TorrentZip per game; loose, as the ROM itself under the name the DAT gives it; or as a loose folder, a folder named for the game holding each of its ROMs loose. Source sets and play libraries each have a global setting, and you can set a system differently."
        </p>
        {move || match loaded.get() {
            None => view! { <Loading what="the storage policy" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok((policy, dats))) => {
                let systems = dats.into_iter().map(|d| d.dat).collect::<Vec<_>>();
                view! { <Editor initial=policy systems /> }.into_any()
            }
        }}
    }
}

#[component]
fn Editor(initial: StoragePolicy, systems: Vec<String>) -> impl IntoView {
    let saved = RwSignal::new(initial.clone());
    let policy = RwSignal::new(initial);
    let dirty = Memo::new(move |_| policy.with(|p| saved.with(|s| p != s)));
    let systems = Memo::new(move |_| {
        let mut all: BTreeSet<String> = systems.iter().cloned().collect();
        policy.with(|p| {
            all.extend(p.source_sets.systems.keys().cloned());
            all.extend(p.play_libraries.systems.keys().cloned());
        });
        all.into_iter().collect::<Vec<_>>()
    });
    let tab = RwSignal::new(None::<String>);
    let outcome = RwSignal::new(None::<Result<(), String>>);
    let saving = RwSignal::new(false);
    let save = move |_| {
        let body = policy.get_untracked();
        saving.set(true);
        leptos::task::spawn_local(async move {
            let result = api::put::<_, StoragePolicy>("/storage-policy", &body).await;
            saving.set(false);
            match result {
                Ok(done) => {
                    saved.set(done.clone());
                    policy.set(done);
                    outcome.set(Some(Ok(())));
                }
                Err(message) => outcome.set(Some(Err(message))),
            }
        });
    };
    view! {
        <div class="tabs" role="tablist" aria-label="Storage settings">
            <Tab tab which=None label="Global settings".to_string() />
            {move || {
                systems
                    .get()
                    .into_iter()
                    .map(|system| view! { <Tab tab which=Some(system.clone()) label=system /> })
                    .collect_view()
            }}
        </div>
        {move || match tab.get() {
            None => view! { <GlobalSettings policy /> }.into_any(),
            Some(system) => view! { <SystemSettings policy system /> }.into_any(),
        }}
        <section class="staged" aria-labelledby="save-title">
            <h2 id="save-title">"Save"</h2>
            <p class="note">
                "Saving changes no file. The next plan for each folder whose format changed proposes converting its games, and nothing is converted until you approve that plan. Each original goes to kept files unless delete originals after conversion is on."
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="primary"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=save
                >
                    {move || if saving.get() { "Saving…" } else { "Save the storage policy" }}
                </button>
                <button
                    type="button"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=move |_| policy.set(saved.get_untracked())
                >
                    "Discard changes"
                </button>
            </div>
            {move || match outcome.get() {
                Some(Ok(())) if !dirty.get() => {
                    view! { <p class="done" role="status">"Saved the storage policy."</p> }.into_any()
                }
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
        </section>
    }
}

#[component]
fn Tab(tab: RwSignal<Option<String>>, which: Option<String>, label: String) -> impl IntoView {
    let which = StoredValue::new(which);
    let selected = move || tab.with(|t| *t == which.get_value());
    view! {
        <button
            type="button"
            role="tab"
            aria-selected=move || selected().to_string()
            on:click=move |_| tab.set(which.get_value())
        >
            {label}
        </button>
    }
}

#[component]
fn GlobalSettings(policy: RwSignal<StoragePolicy>) -> impl IntoView {
    view! {
        <div class="settings" role="tabpanel" aria-label="Global storage settings">
            <p class="lede">
                "These apply to every system you haven't set differently. Stored loose, a game with more than one ROM stays TorrentZip, and a plan says so. Source sets of disc systems, those whose DAT Redump publishes, are stored as loose folders unless you set the system differently."
            </p>
            {SCOPES
                .iter()
                .map(|&(scope, legend)| {
                    let id = scope.as_str();
                    view! {
                        <fieldset class="setting">
                            <legend>{legend}</legend>
                            <label class="stacked" for=format!("g-{id}-format")>
                                "Format"
                            </label>
                            <select
                                id=format!("g-{id}-format")
                                prop:value=move || policy.with(|p| p.scope(scope).format.as_str())
                                on:change=move |ev| {
                                    if let Some(f) = StorageFormat::parse(&event_target_value(&ev)) {
                                        policy.update(|p| scope_mut(p, scope).format = f);
                                    }
                                }
                            >
                                {formats(scope)
                                    .iter()
                                    .map(|&f| view! { <option value=f.as_str()>{format_label(f)}</option> })
                                    .collect_view()}
                            </select>
                            <p>
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked=move || policy.with(|p| p.scope(scope).delete_originals)
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            policy.update(|p| scope_mut(p, scope).delete_originals = on);
                                        }
                                    />
                                    "Delete originals after conversion"
                                </label>
                            </p>
                            <p class="note small">{DELETE_WARNING}</p>
                        </fieldset>
                    }
                })
                .collect_view()}
        </div>
    }
}

#[component]
fn SystemSettings(policy: RwSignal<StoragePolicy>, system: String) -> impl IntoView {
    let s = StoredValue::new(system.clone());
    let has_own = move || {
        policy.with(|p| {
            SCOPES
                .iter()
                .any(|(scope, _)| p.scope(*scope).systems.contains_key(&s.get_value()))
        })
    };
    view! {
        <div class="settings" role="tabpanel" aria-label=format!("Storage settings for {system}")>
            <p class="lede">
                {move || {
                    if has_own() {
                        "Some settings are set for this system; the rest come from your global settings."
                    } else {
                        "Every setting comes from your global settings. Set one for this system to change it here only."
                    }
                }}
            </p>
            <div class="actions">
                <button
                    type="button"
                    disabled=move || !has_own()
                    on:click=move |_| {
                        policy.update(|p| {
                            for (scope, _) in SCOPES {
                                scope_mut(p, scope).systems.remove(&s.get_value());
                            }
                        })
                    }
                >
                    "Use global settings for this system"
                </button>
            </div>
            {SCOPES
                .iter()
                .map(|&(scope, legend)| {
                    let id = scope.as_str();
                    let own = move || {
                        policy.with(|p| p.scope(scope).systems.get(&s.get_value()).cloned().unwrap_or_default())
                    };
                    // A disc system's source sets follow the loose folder default, not the global format.
                    let global = move || {
                        policy.with(|p| {
                            let format = match scope {
                                StorageScope::SourceSets if is_disc_system(&s.get_value()) => {
                                    StorageFormat::LooseFolder
                                }
                                _ => p.scope(scope).format,
                            };
                            (format, p.scope(scope).delete_originals)
                        })
                    };
                    view! {
                        <fieldset class="setting">
                            <legend>{legend}</legend>
                            <label class="stacked" for=format!("s-{id}-format")>
                                "Format"
                            </label>
                            <select
                                id=format!("s-{id}-format")
                                prop:value=move || own().format.map_or("global", StorageFormat::as_str)
                                on:change=move |ev| {
                                    let f = StorageFormat::parse(&event_target_value(&ev));
                                    update_own(policy, scope, &s.get_value(), |o| o.format = f);
                                }
                            >
                                <option value="global">
                                    {move || {
                                        if scope == StorageScope::SourceSets && is_disc_system(&s.get_value()) {
                                            format!("Disc system default: {}", format_label(global().0))
                                        } else {
                                            format!("Global setting: {}", format_label(global().0))
                                        }
                                    }}
                                </option>
                                {formats(scope)
                                    .iter()
                                    .map(|&f| view! { <option value=f.as_str()>{format_label(f)}</option> })
                                    .collect_view()}
                            </select>
                            <label class="stacked" for=format!("s-{id}-delete")>
                                "Delete originals after conversion"
                            </label>
                            <select
                                id=format!("s-{id}-delete")
                                prop:value=move || match own().delete_originals {
                                    None => "global",
                                    Some(true) => "on",
                                    Some(false) => "off",
                                }
                                on:change=move |ev| {
                                    let v = match event_target_value(&ev).as_str() {
                                        "on" => Some(true),
                                        "off" => Some(false),
                                        _ => None,
                                    };
                                    update_own(policy, scope, &s.get_value(), |o| o.delete_originals = v);
                                }
                            >
                                <option value="global">
                                    {move || format!("Global setting: {}", on_off(global().1))}
                                </option>
                                <option value="off">"Off: keep each original in kept files"</option>
                                <option value="on">"On: delete each original once its converted file is verified"</option>
                            </select>
                            <p class="note small">{DELETE_WARNING}</p>
                        </fieldset>
                    }
                })
                .collect_view()}
        </div>
    }
}
