use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use leptos_struct_table::ColumnSort;
use title_select_api::selection::{
    Category, Compilations, SelectionOptions, SelectionProfile, SystemSettings,
};
use title_select_api::{
    Preview, PreviewRequest, ProfileCheck, ProfileRecord, ProfileVersion, SaveProfile,
    SavedProfile, SourceSetRecord, StartSelectionFor, Vocabulary,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::model::{short_sha, when};
use crate::screens::selection::{ChangesTable, ReleasesPanel, SelectionQueued};
use crate::selection_model::{
    SelectionIndex, Setting, Source, lines, move_item, reset_global, reset_system,
    use_global_settings,
};
use crate::table::{DataTable, Grid, Rows, rows_of};

/// How long the editor waits after an edit before checking it and previewing it.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// A preview slower than this switches its source set to the Preview button.
const SLOW_PREVIEW_MS: u64 = 2000;

fn profile_href(name: &str) -> String {
    format!("/profiles/{}", api::segment(name))
}

pub fn category_label(category: Category) -> &'static str {
    match category {
        Category::AddOns => "Add-ons",
        Category::Aftermarket => "Aftermarket",
        Category::Applications => "Applications",
        Category::Audio => "Audio",
        Category::BadDumps => "Bad dumps",
        Category::Bios => "BIOS",
        Category::BonusDiscs => "Bonus discs",
        Category::Coverdiscs => "Coverdiscs",
        Category::Demos => "Demos, kiosks and samples",
        Category::Educational => "Educational",
        Category::Games => "Games",
        Category::Manuals => "Manuals",
        Category::Mia => "Missing in action",
        Category::Multimedia => "Multimedia",
        Category::Pirate => "Pirate",
        Category::Preproduction => "Preproduction",
        Category::Promotional => "Promotional",
        Category::Unlicensed => "Unlicensed",
        Category::Video => "Video",
    }
}

/// The selection profiles, newest version of each.
#[derive(Clone)]
struct ProfileItem(ProfileRecord);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProfileCol {
    Name,
    Version,
    Saved,
    SavedBy,
}

impl Grid for ProfileItem {
    type Col = ProfileCol;
    const COLUMNS: &'static [ProfileCol] = &[
        ProfileCol::Name,
        ProfileCol::Version,
        ProfileCol::Saved,
        ProfileCol::SavedBy,
    ];

    fn header(col: ProfileCol) -> &'static str {
        match col {
            ProfileCol::Name => "Selection profile",
            ProfileCol::Version => "Newest version",
            ProfileCol::Saved => "Saved",
            ProfileCol::SavedBy => "Saved by",
        }
    }

    fn class(col: ProfileCol) -> &'static str {
        match col {
            ProfileCol::Name => "c-wide",
            ProfileCol::Version => "c-id data",
            ProfileCol::Saved => "c-time data",
            ProfileCol::SavedBy => "c-actor",
        }
    }

    fn cell(&self, col: ProfileCol) -> AnyView {
        let latest = self.0.latest.as_ref();
        match col {
            ProfileCol::Name => {
                let name = self.0.name.clone();
                view! { <A href=profile_href(&name)>{name.clone()}</A> }.into_any()
            }
            ProfileCol::Version => latest
                .map_or("Not saved yet".to_string(), |v| v.version.to_string())
                .into_any(),
            ProfileCol::Saved => latest.map(|v| when(&v.saved_at)).into_any(),
            ProfileCol::SavedBy => latest.map(|v| v.saved_by.clone()).into_any(),
        }
    }
}

#[component]
pub fn Profiles() -> impl IntoView {
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let go = navigator();
    let rows = Signal::derive(move || {
        let items = profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(ProfileItem)
            .collect::<Vec<_>>();
        rows_of(items) as Rows<ProfileItem>
    });
    view! {
        <h1>"Selection profiles"</h1>
        <p class="lede">
            "A selection profile holds your settings for choosing one release per game, and your choices where you want something else. Every save is kept as a new version."
        </p>
        <div class="actions">
            <A href="/new-profile" attr:class="button">
                "New selection profile"
            </A>
        </div>
        {move || match profiles.get() {
            None => view! { <Loading what="the selection profiles" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(list)) if list.is_empty() => {
                view! {
                    <p class="note">
                        "You have no selection profile yet. Start from the defaults: preview what they select, change anything you like, then confirm."
                    </p>
                }
                    .into_any()
            }
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="profiles"
            label="Selection profiles".to_string()
            rows
            sorting=RwSignal::new(VecDeque::<(ProfileCol, ColumnSort)>::new())
            selected=RwSignal::new(None)
            on_open=Callback::new(move |item: ProfileItem| go.run(profile_href(&item.0.name)))
            empty="No selection profiles yet.".to_string()
        />
    }
}

/// The first run: an unsaved draft with no settings, which means the defaults.
#[component]
pub fn NewProfile() -> impl IntoView {
    view! { <Editor name=None initial=None /> }
}

/// One selection profile: its editor on the newest version, and its versions.
#[component]
pub fn ProfileScreen() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let name = Memo::new(move |_| params.read().get("name").unwrap_or_default());
    let record = LocalResource::new(move || {
        let name = name.get();
        async move { api::get::<ProfileRecord>(&profile_href(&name)).await }
    });
    let just_saved = move || {
        let q = query.read();
        let version = q.get("saved")?.parse::<u32>().ok()?;
        Some((version, q.get("job").and_then(|j| j.parse::<i64>().ok())))
    };
    move || match record.get() {
        None => view! { <Loading what="the selection profile" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Selection profile"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(record)) => {
            let name = record.name.clone();
            view! {
                {just_saved()
                    .map(|(version, job)| {
                        view! {
                            <p class="done" role="status">{format!("Saved version {version}.")}</p>
                            {job.map(|job| view! { <SelectionQueued job /> })}
                        }
                    })}
                <Editor name=Some(record.name.clone()) initial=record.latest.clone() />
                <Versions name />
            }
            .into_any()
        }
    }
}

#[component]
fn Versions(name: String) -> impl IntoView {
    let path = format!("{}/versions", profile_href(&name));
    let versions = LocalResource::new(move || {
        let path = path.clone();
        async move { api::get::<Vec<ProfileVersion>>(&path).await }
    });
    view! {
        <section aria-labelledby="versions-title">
            <h2 id="versions-title">"Versions"</h2>
            <p class="lede">"Newest first. Versions are never changed or deleted."</p>
            {move || match versions.get() {
                None => view! { <Loading what="the versions" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(versions)) => {
                    view! {
                        <ul class="plain">
                            {versions
                                .into_iter()
                                .map(|v| {
                                    view! {
                                        <li>
                                            {format!(
                                                "Version {}, saved {} by {} ",
                                                v.version,
                                                when(&v.saved_at),
                                                v.saved_by,
                                            )}
                                            <code class="hash small">{short_sha(&v.sha256).to_string()}</code>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Runs `f` once edits have paused for [`DEBOUNCE`], unless the editor is gone by then.
fn debouncer(f: impl Fn() + Clone + 'static) -> impl Fn() + Clone + 'static {
    let pending = StoredValue::new(None::<TimeoutHandle>);
    on_cleanup(move || {
        if let Some(Some(handle)) = pending.try_get_value() {
            handle.clear();
        }
    });
    move || {
        if let Some(handle) = pending.get_value() {
            handle.clear();
        }
        pending.set_value(set_timeout_with_handle(f.clone(), DEBOUNCE).ok());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    Live,
    /// Previews of this source set were slow, so they run on the Preview button.
    Button,
}

/// The editor: global settings and a tab per system, checked and previewed live, saved as a new version.
#[component]
fn Editor(name: Option<String>, initial: Option<ProfileVersion>) -> impl IntoView {
    let go = navigator();
    let existing = name.is_some();
    let name = RwSignal::new(name.unwrap_or_else(|| "Default".to_string()));
    let base = RwSignal::new(initial.as_ref().map(|v| v.version));
    let saved = RwSignal::new(initial.map(|v| v.profile).unwrap_or_default());
    let profile = RwSignal::new(saved.get_untracked());
    let dirty = Memo::new(move |_| profile.with(|p| saved.with(|s| p != s)));
    let vocabulary = LocalResource::new(|| api::get::<Vocabulary>("/vocabulary"));
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let set = RwSignal::new(String::new());
    Effect::new(move || {
        if let Some(Ok(sets)) = sets.get()
            && set.get_untracked().is_empty()
            && let Some(first) = sets.first()
        {
            set.set(first.name.clone());
        }
    });

    let problems = RwSignal::new(Vec::<String>::new());
    let check = debouncer(move || {
        let request = profile.get_untracked();
        leptos::task::spawn_local(async move {
            match api::post::<_, ProfileCheck>("/check", &request).await {
                Ok(check) => problems.set(check.problems),
                Err(message) => problems.set(vec![message]),
            }
        });
    });
    Effect::new(move || {
        profile.track();
        check();
    });

    let preview = RwSignal::new(None::<Result<Arc<Preview>, String>>);
    let running = RwSignal::new(false);
    let slow = RwSignal::new(BTreeSet::<String>::new());
    let mode = Memo::new(move |_| {
        if set.with(|s| slow.with(|slow| slow.contains(s))) {
            PreviewMode::Button
        } else {
            PreviewMode::Live
        }
    });
    let sequence = StoredValue::new(0u64);
    // The request in flight, aborted when a newer preview starts or the editor goes, so that the Service stops
    // the engine run instead of finishing it for nobody.
    let in_flight = StoredValue::new_local(None::<web_sys::AbortController>);
    let abort = move || {
        if let Some(Some(controller)) = in_flight.try_update_value(Option::take) {
            controller.abort();
        }
    };
    on_cleanup(abort);
    let run_preview = move || {
        let source_set = set.get_untracked();
        if source_set.is_empty() {
            return;
        }
        let request = PreviewRequest {
            source_set: source_set.clone(),
            revision: None,
            profile: profile.get_untracked(),
        };
        let n = sequence.get_value() + 1;
        sequence.set_value(n);
        running.set(true);
        abort();
        let controller = web_sys::AbortController::new().ok();
        let signal = controller.as_ref().map(web_sys::AbortController::signal);
        in_flight.set_value(controller);
        leptos::task::spawn_local(async move {
            let result = api::post_until::<_, Preview>("/preview", &request, signal.as_ref()).await;
            // A newer preview superseded this one, or the editor is gone.
            if sequence.try_get_value() != Some(n) {
                return;
            }
            running.set(false);
            if let Ok(p) = &result
                && p.elapsed_ms > SLOW_PREVIEW_MS
            {
                slow.update(|s| {
                    s.insert(source_set);
                });
            }
            preview.set(Some(result.map(Arc::new)));
        });
    };
    let live_preview = debouncer(run_preview);
    Effect::new(move || {
        profile.track();
        set.track();
        if mode.get_untracked() == PreviewMode::Live {
            live_preview();
        }
    });

    let previewed_system = Memo::new(move |_| {
        preview.with(|p| {
            p.as_ref()?
                .as_ref()
                .ok()
                .map(|p| p.selection.system.clone())
        })
    });
    let systems = Memo::new(move |_| {
        let mut systems: BTreeSet<String> =
            profile.with(|p| p.systems.keys().chain(p.choices.keys()).cloned().collect());
        systems.extend(previewed_system.get());
        systems.into_iter().collect::<Vec<_>>()
    });
    let tab = RwSignal::new(None::<String>);

    let queue = RwSignal::new(true);
    let outcome = RwSignal::new(None::<Result<SavedProfile, String>>);
    let saving = RwSignal::new(false);
    let save = move |_| {
        let target = name.get_untracked().trim().to_string();
        let request = SaveProfile {
            profile: profile.get_untracked(),
            base_version: base.get_untracked(),
            queue: if queue.get_untracked() && !set.get_untracked().is_empty() {
                vec![StartSelectionFor {
                    source_set: set.get_untracked(),
                    revision: None,
                }]
            } else {
                Vec::new()
            },
        };
        let path = format!("{}/versions", profile_href(&target));
        saving.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post::<_, SavedProfile>(&path, &request).await;
            saving.set(false);
            match result {
                Ok(done) if !existing => {
                    let job = done
                        .jobs
                        .first()
                        .map(|j| format!("&job={}", j.id))
                        .unwrap_or_default();
                    go.run(format!(
                        "{}?saved={}{job}",
                        profile_href(&target),
                        done.version.version
                    ));
                }
                Ok(done) => {
                    base.set(Some(done.version.version));
                    saved.set(done.version.profile.clone());
                    outcome.set(Some(Ok(done)));
                }
                Err(message) => outcome.set(Some(Err(message))),
            }
        });
    };
    let save_label = move || match base.get() {
        None => "Confirm and save version 1".to_string(),
        Some(v) => format!("Save version {}", v + 1),
    };

    view! {
        <h1>
            {move || {
                if existing { format!("Selection profile {}", name.get()) } else { "New selection profile".to_string() }
            }}
        </h1>
        <p class="lede">
            {move || match base.get() {
                None => {
                    "An unsaved draft with no settings, so the defaults apply. Preview what it selects, change anything you like, then confirm to save version 1.".to_string()
                }
                Some(v) if dirty.get() => format!("Editing version {v}. You have unsaved changes."),
                Some(v) => format!("Version {v}, the newest."),
            }}
        </p>
        {(!existing)
            .then(|| {
                view! {
                    <form class="form" on:submit=|ev| ev.prevent_default()>
                        <label>
                            "Name"
                            <input
                                type="text"
                                prop:value=name
                                on:input=move |ev| name.set(event_target_value(&ev))
                                maxlength="100"
                            />
                        </label>
                    </form>
                }
            })}
        {move || {
            let problems = problems.get();
            (!problems.is_empty())
                .then(|| {
                    view! {
                        <div class="problem" role="alert">
                            <p>"These settings can't be used yet:"</p>
                            <ul>{problems.into_iter().map(|p| view! { <li>{p}</li> }).collect_view()}</ul>
                        </div>
                    }
                })
        }}
        <div class="tabs" role="tablist" aria-label="Settings">
            <SettingsTab tab which=None label="Global settings".to_string() />
            {move || {
                systems
                    .get()
                    .into_iter()
                    .map(|system| {
                        view! { <SettingsTab tab which=Some(system.clone()) label=system /> }
                    })
                    .collect_view()
            }}
        </div>
        {move || {
            let vocabulary = vocabulary.get().and_then(Result::ok);
            match (tab.get(), vocabulary) {
                (_, None) => view! { <Loading what="the names you can use" /> }.into_any(),
                (None, Some(vocabulary)) => {
                    view! { <GlobalSettings profile vocabulary=Arc::new(vocabulary) /> }.into_any()
                }
                (Some(system), Some(vocabulary)) => {
                    view! { <SystemSettingsForm profile system vocabulary=Arc::new(vocabulary) /> }
                        .into_any()
                }
            }
        }}

        <section class="staged" aria-labelledby="save-title">
            <h2 id="save-title">{move || if base.get().is_none() { "Confirm" } else { "Save" }}</h2>
            <div class="actions">
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=queue
                        on:change=move |ev| queue.set(event_target_checked(&ev))
                    />
                    {move || {
                        let s = set.get();
                        if s.is_empty() {
                            "Then make a selection (register a source set first)".to_string()
                        } else {
                            format!("Then make a selection for {s}")
                        }
                    }}
                </label>
                <button
                    type="button"
                    class="primary"
                    disabled=move || {
                        saving.get() || !problems.with(Vec::is_empty) || name.with(|n| n.trim().is_empty())
                            || (base.get().is_some() && !dirty.get())
                    }
                    on:click=save
                >
                    {save_label}
                </button>
                <button
                    type="button"
                    disabled=move || !dirty.get()
                    on:click=move |_| profile.set(saved.get_untracked())
                >
                    "Discard changes"
                </button>
            </div>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(done) => {
                            let message = if done.created {
                                format!("Saved version {}.", done.version.version)
                            } else {
                                format!(
                                    "Nothing changed, so no new version was saved; version {} stays the newest.",
                                    done.version.version,
                                )
                            };
                            view! {
                                <p class="done" role="status">{message}</p>
                                {done
                                    .jobs
                                    .into_iter()
                                    .map(|job| view! { <SelectionQueued job=job.id /> })
                                    .collect_view()}
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>

        <section aria-labelledby="preview-title">
            <h2 id="preview-title">"Preview"</h2>
            <form class="form" on:submit=move |ev| {
                ev.prevent_default();
                run_preview();
            }>
                <label>
                    "Source set"
                    <select prop:value=set on:change=move |ev| set.set(event_target_value(&ev))>
                        {move || {
                            sets.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|s| {
                                    let name = s.name;
                                    view! { <option value=name.clone()>{name.clone()}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <button type="submit" disabled=move || set.with(String::is_empty)>
                    "Preview"
                </button>
            </form>
            <p class="note" aria-live="polite">
                {move || {
                    if running.get() {
                        "Previewing…".to_string()
                    } else if mode.get() == PreviewMode::Button {
                        "Previews of this source set take more than 2 seconds, so they run when you press Preview."
                            .to_string()
                    } else {
                        "The preview follows your edits. Nothing is saved until you save.".to_string()
                    }
                }}
            </p>
            {move || match preview.get() {
                None => {
                    sets.get()
                        .and_then(Result::ok)
                        .filter(Vec::is_empty)
                        .map(|_| {
                            view! { <p class="note">"Register a source set to preview what this profile selects."</p> }
                        })
                        .into_any()
                }
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(p)) => view! { <PreviewView preview=p /> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn SettingsTab(
    tab: RwSignal<Option<String>>,
    which: Option<String>,
    label: String,
) -> impl IntoView {
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

/// What a preview selects, and what changed from the latest saved selection.
#[component]
fn PreviewView(preview: Arc<Preview>) -> impl IntoView {
    let selection = &preview.selection;
    let summary = selection.summary;
    let index = Arc::new(SelectionIndex::new(selection.clone(), &Default::default()));
    let changes = preview.diff.clone();
    let count = changes.len();
    view! {
        <dl class="facts">
            <dt>"DAT revision"</dt>
            <dd>
                {preview.dat_revision.version.clone()} " "
                <code class="hash">{short_sha(&preview.dat_revision.sha256).to_string()}</code>
            </dd>
            <dt>"System"</dt>
            <dd>
                {selection.system.clone()} " · "
                {if selection.system_settings {
                    "your system settings apply"
                } else {
                    "only the global settings apply"
                }}
            </dd>
            <dt>"Took"</dt>
            <dd>{format!("{:.1} s", preview.elapsed_ms as f64 / 1000.0)}</dd>
        </dl>
        <ul class="counts">
            <li class="count">
                <span class="n">{summary.groups}</span>
                <span class="what">"Groups"</span>
            </li>
            <li class="count verified">
                <span class="n">{summary.winners}</span>
                <span class="what">"Selected"</span>
            </li>
            <li class="count">
                <span class="n">{summary.alternatives}</span>
                <span class="what">"Alternatives"</span>
            </li>
            <li class="count">
                <span class="n">{summary.exclusions}</span>
                <span class="what">"Excluded"</span>
            </li>
            <li class="count review">
                <span class="n">{summary.needs_review()}</span>
                <span class="what">"Needs review"</span>
            </li>
            <li class="count">
                <span class="n">{summary.choices}</span>
                <span class="what">"Your choices"</span>
            </li>
        </ul>
        <h3>"Changes"</h3>
        {match preview.against {
            Some(against) => {
                view! {
                    <p class="lede">
                        "Compared with "
                        <A href=format!("/selections/{against}")>{format!("selection #{against}")}</A>
                        {format!(
                            ", the latest saved selection for this DAT revision: {count} {} change role.",
                            if count == 1 { "release" } else { "releases" },
                        )}
                    </p>
                    <ChangesTable id="preview-changes" changes />
                }
                    .into_any()
            }
            None => {
                view! { <p class="note">"There is no saved selection for this DAT revision to compare with yet."</p> }
                    .into_any()
            }
        }}
        <h3>"What it selects"</h3>
        <ReleasesPanel index audited=false group=RwSignal::new(None) detail_id="preview-title" />
    }
}

/// A setting's editor in a system tab: where it comes from, and the way to change that.
#[component]
fn SystemSetting(
    profile: RwSignal<SelectionProfile>,
    system: StoredValue<String>,
    setting: Setting,
    label: &'static str,
    /// The global value, in words.
    global: Signal<String>,
    /// The order the global value stands for, when it is an empty order that means the default one.
    #[prop(optional)]
    global_default: Option<Signal<Option<Vec<String>>>>,
    children: ChildrenFn,
) -> impl IntoView {
    let source = Memo::new(move |_| {
        profile.with(|p| {
            setting.source(
                p.systems
                    .get(&system.get_value())
                    .unwrap_or(&SystemSettings::default()),
            )
        })
    });
    view! {
        <fieldset class="setting">
            <legend>{label}</legend>
            <p class="source">
                <span class="tag">{move || source.get().label()}</span>
                " "
                {move || match source.get() {
                    Source::Global => {
                        view! {
                            {move || {
                                global_default
                                    .is_none_or(|d| d.with(Option::is_none))
                                    .then(|| view! { <span>{global}</span> " " })
                            }}
                            <button
                                type="button"
                                on:click=move |_| profile.update(|p| setting.set_for_system(p, &system.get_value()))
                            >
                                "Set for this system"
                            </button>
                        }
                            .into_any()
                    }
                    Source::System => {
                        view! {
                            <button
                                type="button"
                                on:click=move |_| profile.update(|p| setting.use_global(p, &system.get_value()))
                            >
                                "Use global setting"
                            </button>
                        }
                            .into_any()
                    }
                }}
            </p>
            {move || {
                (source.get() == Source::Global)
                    .then(|| global_default.and_then(|d| d.get()))
                    .flatten()
                    .map(|order| view! { <DefaultOrder order /> })
            }}
            {move || (source.get() == Source::System).then(|| children())}
        </fieldset>
    }
}

/// The order an empty region or video order stands for, listed in full, with a link to how the defaults work.
#[component]
fn DefaultOrder(order: Vec<String>) -> impl IntoView {
    view! {
        <details class="default-order note small">
            <summary>"The default order, which suits English speakers."</summary>
            <ol class="default-list">
                {order.into_iter().map(|name| view! { <li>{name}</li> }).collect_view()}
            </ol>
            <p>
                <A href=DEFAULT_ORDERS_HREF>"How the default orders work"</A>
            </p>
        </details>
    }
}

/// Where the About page explains the default orders.
const DEFAULT_ORDERS_HREF: &str = "/about#default-orders";

fn order_text(list: &[String], empty: &str) -> String {
    if list.is_empty() {
        empty.to_string()
    } else {
        list.join(", ")
    }
}

const REGIONS_EMPTY: &str = "The default order.";
const LANGUAGES_EMPTY: &str = "The languages of your region order.";
const LOCALIZATION_EMPTY: &str = "No local names.";
const VIDEO_EMPTY: &str = "The default order.";

/// The global settings tab.
#[component]
fn GlobalSettings(
    profile: RwSignal<SelectionProfile>,
    vocabulary: Arc<Vocabulary>,
) -> impl IntoView {
    let choices = move || profile.with(|p| p.choices.values().map(|c| c.len()).sum::<usize>());
    view! {
        <div class="settings" role="tabpanel" aria-label="Global settings">
            <div class="actions">
                <button type="button" on:click=move |_| profile.update(reset_global)>
                    "Reset to defaults"
                </button>
                <button
                    type="button"
                    disabled=move || choices() == 0
                    on:click=move |_| profile.update(|p| p.reset_all_choices())
                >
                    {move || format!("Reset to default choices for the whole profile ({})", choices())}
                </button>
            </div>
            <OrderedList
                id="g-regions"
                label="Regions, in order of preference"
                note="A region left out is not selected."
                empty=REGIONS_EMPTY
                options=vocabulary.regions.clone()
                default=vocabulary.default_region_order.clone()
                items=Signal::derive(move || profile.with(|p| p.region_order.clone()))
                on_change=Callback::new(move |v| profile.update(|p| p.region_order = v))
            />
            <OrderedList
                id="g-languages"
                label="Languages, in order of preference"
                note=""
                empty=LANGUAGES_EMPTY
                options=vocabulary.languages.clone()
                items=Signal::derive(move || profile.with(|p| p.language_order.clone()))
                on_change=Callback::new(move |v| profile.update(|p| p.language_order = v))
            />
            <OrderedList
                id="g-video"
                label="Video standards, in order of preference"
                note=""
                empty=VIDEO_EMPTY
                options=vocabulary.videos.clone()
                default=vocabulary.default_video_order.clone()
                items=Signal::derive(move || profile.with(|p| p.video_order.clone()))
                on_change=Callback::new(move |v| profile.update(|p| p.video_order = v))
            />
            <OrderedList
                id="g-local"
                label="Languages for local names"
                note="Used only to rename releases; it doesn't change what is selected."
                empty=LOCALIZATION_EMPTY
                options=vocabulary.languages.clone()
                items=Signal::derive(move || profile.with(|p| p.localization_order.clone()))
                on_change=Callback::new(move |v| profile.update(|p| p.localization_order = v))
            />
            <fieldset class="setting">
                <legend>"Leave out"</legend>
                <Exclusions
                    id="g-exclude"
                    value=Signal::derive(move || profile.with(|p| p.exclude.clone()))
                    on_change=Callback::new(move |v| profile.update(|p| p.exclude = v))
                />
            </fieldset>
            <fieldset class="setting">
                <legend>"Options"</legend>
                <Options
                    id="g-options"
                    value=Signal::derive(move || profile.with(|p| p.options.clone()))
                    on_change=Callback::new(move |v| profile.update(|p| p.options = v))
                />
            </fieldset>
            <fieldset class="setting">
                <legend>"Overrides"</legend>
                <OverrideHelp />
                <LineEditor
                    id="g-include"
                    label="Always include"
                    items=Signal::derive(move || profile.with(|p| p.overrides.include.clone()))
                    on_change=Callback::new(move |v| profile.update(|p| p.overrides.include = v))
                />
                <LineEditor
                    id="g-exclude-lines"
                    label="Always exclude"
                    items=Signal::derive(move || profile.with(|p| p.overrides.exclude.clone()))
                    on_change=Callback::new(move |v| profile.update(|p| p.overrides.exclude = v))
                />
            </fieldset>
            <fieldset class="setting">
                <legend>"Post filters"</legend>
                <p class="note small">"A selected release must match one of these to stay selected."</p>
                <LineEditor
                    id="g-post"
                    label="Keep only releases matching"
                    items=Signal::derive(move || profile.with(|p| p.post_filters.clone()))
                    on_change=Callback::new(move |v| profile.update(|p| p.post_filters = v))
                />
            </fieldset>
        </div>
    }
}

#[component]
fn OverrideHelp() -> impl IntoView {
    view! {
        <p class="note small">
            "One per line. Plain text matches part of a name, "
            <code>"|"</code>
            " then a full name matches exactly, and "
            <code>"/"</code>
            " then a regular expression matches by pattern. Wrap one in "
            <code>"<…>"</code>
            " to match related releases too."
        </p>
    }
}

/// Reads one system's settings from the Profile, defaulting each to the global one.
fn own(p: &SelectionProfile, system: &str) -> SystemSettings {
    p.systems.get(system).cloned().unwrap_or_default()
}

fn update_own(
    profile: RwSignal<SelectionProfile>,
    system: &str,
    f: impl FnOnce(&mut SystemSettings),
) {
    let system = system.to_owned();
    profile.update(|p| {
        f(p.systems.entry(system.clone()).or_default());
        if p.systems.get(&system).is_some_and(SystemSettings::is_empty) {
            p.systems.remove(&system);
        }
    });
}

/// A system tab: each setting from the global settings or set for this system.
#[component]
fn SystemSettingsForm(
    profile: RwSignal<SelectionProfile>,
    system: String,
    vocabulary: Arc<Vocabulary>,
) -> impl IntoView {
    let s = StoredValue::new(system.clone());
    let has_own = move || profile.with(|p| p.systems.contains_key(&s.get_value()));
    let choices = move || profile.with(|p| p.choices.get(&s.get_value()).map_or(0, |c| c.len()));
    let global = move |f: fn(&SelectionProfile) -> String| Signal::derive(move || profile.with(f));
    let global_default = move |f: fn(&SelectionProfile) -> &Vec<String>, order: Vec<String>| {
        Signal::derive(move || profile.with(|p| f(p).is_empty().then(|| order.clone())))
    };
    let v = StoredValue::new(vocabulary);
    view! {
        <div class="settings" role="tabpanel" aria-label=format!("System settings for {system}")>
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
                    on:click=move |_| profile.update(|p| use_global_settings(p, &s.get_value()))
                >
                    "Use global settings for this system"
                </button>
                <button type="button" on:click=move |_| profile.update(|p| reset_system(p, &s.get_value()))>
                    "Reset to defaults for this system"
                </button>
                <button
                    type="button"
                    disabled=move || choices() == 0
                    on:click=move |_| profile.update(|p| p.reset_choices(&s.get_value(), None))
                >
                    {move || format!("Reset to default choices for this system ({})", choices())}
                </button>
            </div>
            <p class="note small">
                "Reset to defaults sets every setting for this system to the defaults, whatever your global settings say. Your global overrides still apply, and your choices stay."
            </p>
            <SystemSetting
                profile
                system=s
                setting=Setting::RegionOrder
                label="Regions, in order of preference"
                global=global(|p| order_text(&p.region_order, REGIONS_EMPTY))
                global_default=global_default(|p| &p.region_order, v.with_value(|v| v.default_region_order.clone()))
            >
                <OrderedList
                    id="s-regions"
                    label="Regions for this system"
                    note=""
                    empty=REGIONS_EMPTY
                    options=v.with_value(|v| v.regions.clone())
                    default=v.with_value(|v| v.default_region_order.clone())
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).region_order.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.region_order = Some(v)))
                />
            </SystemSetting>
            <SystemSetting
                profile
                system=s
                setting=Setting::LanguageOrder
                label="Languages, in order of preference"
                global=global(|p| order_text(&p.language_order, LANGUAGES_EMPTY))
            >
                <OrderedList
                    id="s-languages"
                    label="Languages for this system"
                    note=""
                    empty=LANGUAGES_EMPTY
                    options=v.with_value(|v| v.languages.clone())
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).language_order.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.language_order = Some(v)))
                />
            </SystemSetting>
            <SystemSetting
                profile
                system=s
                setting=Setting::VideoOrder
                label="Video standards, in order of preference"
                global=global(|p| order_text(&p.video_order, VIDEO_EMPTY))
                global_default=global_default(|p| &p.video_order, v.with_value(|v| v.default_video_order.clone()))
            >
                <OrderedList
                    id="s-video"
                    label="Video standards for this system"
                    note=""
                    empty=VIDEO_EMPTY
                    options=v.with_value(|v| v.videos.clone())
                    default=v.with_value(|v| v.default_video_order.clone())
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).video_order.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.video_order = Some(v)))
                />
            </SystemSetting>
            <SystemSetting
                profile
                system=s
                setting=Setting::LocalizationOrder
                label="Languages for local names"
                global=global(|p| order_text(&p.localization_order, LOCALIZATION_EMPTY))
            >
                <OrderedList
                    id="s-local"
                    label="Languages for local names on this system"
                    note=""
                    empty=LOCALIZATION_EMPTY
                    options=v.with_value(|v| v.languages.clone())
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).localization_order.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.localization_order = Some(v)))
                />
            </SystemSetting>
            <SystemSetting
                profile
                system=s
                setting=Setting::Exclude
                label="Leave out"
                global=global(|p| {
                    if p.exclude.is_empty() {
                        "Nothing.".to_string()
                    } else {
                        p.exclude.iter().map(|c| category_label(*c)).collect::<Vec<_>>().join(", ")
                    }
                })
            >
                <Exclusions
                    id="s-exclude"
                    value=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).exclude.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.exclude = Some(v)))
                />
            </SystemSetting>
            <SystemSetting
                profile
                system=s
                setting=Setting::Options
                label="Options"
                global=global(|p| options_text(&p.options))
            >
                <Options
                    id="s-options"
                    value=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).options.unwrap_or_default()))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.options = Some(v)))
                />
            </SystemSetting>
            <fieldset class="setting">
                <legend>"Overrides for this system"</legend>
                <p class="source">
                    <span class="tag">"Added to global settings"</span>
                </p>
                <OverrideHelp />
                <LineEditor
                    id="s-include"
                    label="Always include"
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).overrides.include))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.overrides.include = v))
                />
                <LineEditor
                    id="s-exclude-lines"
                    label="Always exclude"
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).overrides.exclude))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.overrides.exclude = v))
                />
            </fieldset>
            <fieldset class="setting">
                <legend>"Post filters for this system"</legend>
                <p class="source">
                    <span class="tag">
                        {move || {
                            if profile.with(|p| own(p, &s.get_value()).replace_post_filters) {
                                "Replace global settings"
                            } else {
                                "Added to global settings"
                            }
                        }}
                    </span>
                </p>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=move || profile.with(|p| own(p, &s.get_value()).replace_post_filters)
                        on:change=move |ev| {
                            let on = event_target_checked(&ev);
                            update_own(profile, &s.get_value(), |o| o.replace_post_filters = on)
                        }
                    />
                    "Use only these, not the global post filters"
                </label>
                <LineEditor
                    id="s-post"
                    label="Keep only releases matching"
                    items=Signal::derive(move || profile.with(|p| own(p, &s.get_value()).post_filters))
                    on_change=Callback::new(move |v| update_own(profile, &s.get_value(), |o| o.post_filters = v))
                />
            </fieldset>
        </div>
    }
}

fn options_text(options: &SelectionOptions) -> String {
    let on: Vec<&str> = OPTIONS
        .iter()
        .filter(|(_, get, _)| get(options))
        .map(|(label, _, _)| *label)
        .collect();
    let mut text = if on.is_empty() {
        "None set.".to_string()
    } else {
        on.join("; ")
    };
    if options.compilations != Compilations::Default {
        text.push_str(&format!(
            "; compilations: {}",
            compilations_label(options.compilations)
        ));
    }
    text
}

type Flag = (
    &'static str,
    fn(&SelectionOptions) -> bool,
    fn(&mut SelectionOptions, bool),
);

const OPTIONS: &[Flag] = &[
    (
        "Prefer releases that support RetroAchievements",
        |o| o.prefer_retroachievements,
        |o, v| o.prefer_retroachievements = v,
    ),
    (
        "Prefer the oldest production versions",
        |o| o.prefer_oldest,
        |o, v| o.prefer_oldest = v,
    ),
    (
        "Follow the region order strictly, even over a preferred language",
        |o| o.prefer_regions,
        |o, v| o.prefer_regions = v,
    ),
    (
        "Prefer licensed releases",
        |o| o.prefer_licensed,
        |o, v| o.prefer_licensed = v,
    ),
    (
        "Prefer releases ripped from modern rereleases",
        |o| o.prefer_modern_rips,
        |o, v| o.prefer_modern_rips = v,
    ),
    (
        "Leave out releases in none of your languages",
        |o| o.filter_languages,
        |o, v| o.filter_languages = v,
    ),
    (
        "Keep every release, not one per game",
        |o| o.disable_1g1r,
        |o, v| o.disable_1g1r = v,
    ),
];

fn compilations_label(c: Compilations) -> &'static str {
    match c {
        Compilations::Default => {
            "the default: individual releases unless a compilation ranks higher"
        }
        Compilations::Individual => "always prefer individual releases",
        Compilations::Keep => "keep individual releases and compilations",
        Compilations::Optimize => "least duplication (experimental)",
    }
}

const COMPILATIONS: [(Compilations, &str); 4] = [
    (Compilations::Default, "default"),
    (Compilations::Individual, "individual"),
    (Compilations::Keep, "keep"),
    (Compilations::Optimize, "optimize"),
];

#[component]
fn Options(
    id: &'static str,
    value: Signal<SelectionOptions>,
    on_change: Callback<SelectionOptions>,
) -> impl IntoView {
    view! {
        <ul class="checks">
            {OPTIONS
                .iter()
                .enumerate()
                .map(|(i, (label, get, set))| {
                    let (get, set) = (*get, *set);
                    view! {
                        <li>
                            <label class="inline">
                                <input
                                    type="checkbox"
                                    id=format!("{id}-{i}")
                                    prop:checked=move || value.with(get)
                                    on:change=move |ev| {
                                        let mut o = value.get_untracked();
                                        set(&mut o, event_target_checked(&ev));
                                        on_change.run(o);
                                    }
                                />
                                {*label}
                            </label>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
        <label class="stacked">
            "Compilations"
            <select
                prop:value=move || {
                    let c = value.with(|o| o.compilations);
                    COMPILATIONS.iter().find(|(k, _)| *k == c).map_or("default", |(_, key)| *key)
                }
                on:change=move |ev| {
                    let key = event_target_value(&ev);
                    if let Some((c, _)) = COMPILATIONS.iter().find(|(_, k)| *k == key) {
                        let mut o = value.get_untracked();
                        o.compilations = *c;
                        on_change.run(o);
                    }
                }
            >
                {COMPILATIONS
                    .iter()
                    .map(|(c, key)| view! { <option value=*key>{compilations_label(*c)}</option> })
                    .collect_view()}
            </select>
        </label>
    }
}

#[component]
fn Exclusions(
    id: &'static str,
    value: Signal<BTreeSet<Category>>,
    on_change: Callback<BTreeSet<Category>>,
) -> impl IntoView {
    view! {
        <ul class="checks columns">
            {Category::ALL
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let c = *c;
                    view! {
                        <li>
                            <label class="inline">
                                <input
                                    type="checkbox"
                                    id=format!("{id}-{i}")
                                    prop:checked=move || value.with(|v| v.contains(&c))
                                    on:change=move |ev| {
                                        let mut v = value.get_untracked();
                                        if event_target_checked(&ev) {
                                            v.insert(c);
                                        } else {
                                            v.remove(&c);
                                        }
                                        on_change.run(v);
                                    }
                                />
                                {category_label(c)}
                            </label>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
}

/// One override or post filter per line, committed when you leave the box.
#[component]
fn LineEditor(
    id: &'static str,
    label: &'static str,
    items: Signal<Vec<String>>,
    on_change: Callback<Vec<String>>,
) -> impl IntoView {
    view! {
        <label class="stacked" for=id>
            {label}
        </label>
        <textarea
            id=id
            class="lines data"
            rows="4"
            spellcheck="false"
            prop:value=move || items.with(|i| i.join("\n"))
            on:change=move |ev| on_change.run(lines(&event_target_value(&ev)))
        ></textarea>
    }
}

/// An ordered list: drag an item, or use its up and down buttons, to reorder it.
#[component]
fn OrderedList(
    id: &'static str,
    label: &'static str,
    note: &'static str,
    /// Shown when the list is empty.
    empty: &'static str,
    options: Vec<String>,
    items: Signal<Vec<String>>,
    on_change: Callback<Vec<String>>,
    /// The order an empty list stands for, shown in full and offered as a starting point.
    #[prop(optional)]
    default: Option<Vec<String>>,
) -> impl IntoView {
    let default = StoredValue::new(default);
    let dragging = RwSignal::new(None::<usize>);
    let adding = RwSignal::new(String::new());
    let options = StoredValue::new(options);
    let moved = move |from: usize, to: usize, focus_on: String| {
        let mut list = items.get_untracked();
        move_item(&mut list, from, to);
        on_change.run(list);
        request_animation_frame(move || crate::screens::audit::focus(&focus_on));
    };
    let remaining = move || {
        let chosen = items.get();
        options
            .get_value()
            .into_iter()
            .filter(|o| !chosen.contains(o))
            .collect::<Vec<_>>()
    };
    view! {
        <fieldset class="setting ordered">
            <legend>{label}</legend>
            {(!note.is_empty()).then(|| view! { <p class="note small">{note}</p> })}
            <ol class="order">
                {move || {
                    let list = items.get();
                    let n = list.len();
                    list.into_iter()
                        .enumerate()
                        .map(|(i, item)| {
                            let up = format!("{id}-up-{i}");
                            let down = format!("{id}-down-{i}");
                            let to_up = format!("{id}-up-{}", i.saturating_sub(1));
                            let to_down = format!("{id}-down-{}", (i + 1).min(n.saturating_sub(1)));
                            let it = item.clone();
                            view! {
                                <li
                                    draggable="true"
                                    class:dragging=move || dragging.get() == Some(i)
                                    on:dragstart=move |ev: web_sys::DragEvent| {
                                        if let Some(data) = ev.data_transfer() {
                                            let _ = data.set_data("text/plain", &it);
                                        }
                                        dragging.set(Some(i));
                                    }
                                    on:dragover=|ev: web_sys::DragEvent| ev.prevent_default()
                                    on:drop=move |ev: web_sys::DragEvent| {
                                        ev.prevent_default();
                                        if let Some(from) = dragging.get_untracked() {
                                            moved(from, i, format!("{id}-up-{i}"));
                                        }
                                        dragging.set(None);
                                    }
                                    on:dragend=move |_| dragging.set(None)
                                >
                                    <span class="grip" aria-hidden="true">"⠿"</span>
                                    <span class="item">{item.clone()}</span>
                                    <button
                                        type="button"
                                        id=up
                                        class="small"
                                        aria-label=format!("Move {item} up")
                                        disabled=i == 0
                                        on:click={
                                            let to_up = to_up.clone();
                                            move |_| moved(i, i.saturating_sub(1), to_up.clone())
                                        }
                                    >
                                        "↑"
                                    </button>
                                    <button
                                        type="button"
                                        id=down
                                        class="small"
                                        aria-label=format!("Move {item} down")
                                        disabled=i + 1 == n
                                        on:click=move |_| moved(i, i + 1, to_down.clone())
                                    >
                                        "↓"
                                    </button>
                                    <button
                                        type="button"
                                        class="small"
                                        aria-label=format!("Remove {item}")
                                        on:click=move |_| {
                                            let mut list = items.get_untracked();
                                            list.remove(i);
                                            on_change.run(list);
                                        }
                                    >
                                        "Remove"
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ol>
            {move || match (items.with(Vec::is_empty), default.get_value()) {
                (false, Some(_)) => view! {
                    <div class="actions">
                        <button type="button" class="small" on:click=move |_| on_change.run(Vec::new())>
                            "Reset to the default order"
                        </button>
                    </div>
                }
                .into_any(),
                (false, None) => ().into_any(),
                (true, Some(order)) => view! {
                    <DefaultOrder order=order.clone() />
                    <div class="actions">
                        <button type="button" class="small" on:click=move |_| on_change.run(order.clone())>
                            "Start from the default order"
                        </button>
                    </div>
                }
                .into_any(),
                (true, None) => view! { <p class="note small">{empty}</p> }.into_any(),
            }}
            <div class="add">
                <label class="inline">
                    <span class="visually-hidden">{format!("Add to {}", label.to_lowercase())}</span>
                    <select prop:value=adding on:change=move |ev| adding.set(event_target_value(&ev))>
                        <option value="">"Add…"</option>
                        {move || {
                            remaining()
                                .into_iter()
                                .map(|o| view! { <option value=o.clone()>{o.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <button
                    type="button"
                    class="small"
                    disabled=move || adding.with(String::is_empty)
                    on:click=move |_| {
                        let chosen = adding.get_untracked();
                        let mut list = items.get_untracked();
                        if !chosen.is_empty() && !list.contains(&chosen) {
                            list.push(chosen);
                            on_change.run(list);
                        }
                        adding.set(String::new());
                    }
                >
                    "Add"
                </button>
            </div>
        </fieldset>
    }
}
