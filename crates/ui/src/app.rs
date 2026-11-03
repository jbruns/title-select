use codee::string::FromToStringCodec;
use leptos::prelude::*;
use leptos_router::components::{A, Outlet, ParentRoute, Redirect, Route, Router, Routes};
use leptos_router::hooks::{use_location, use_navigate, use_params_map};
use leptos_router::{NavigateOptions, path};
use leptos_use::storage::use_local_storage;
use title_select_api::ingest::IngestFolder;
use title_select_api::{Job, JobState};

use crate::api;
use crate::events::{Stream, provide_live, use_live};
use crate::parts::icon::{Glyph, LabelledIcon};
use crate::parts::layout::{Tab, Tabs};
use crate::parts::overlay::Tooltip;
use crate::parts::theme::ThemeToggle;
#[cfg(feature = "bench")]
use crate::screens::Bench;
use crate::screens::{
    About, Activity, Additions, AuditScreen, BuildScreen, DatRevisions, IngestFolderScreen,
    IngestFolders, Jobs, KeptFiles, Libraries, LibraryFolderScreen, NewProfile, ProfileScreen,
    Profiles, RepairScreen, Roots, SelectionScreen, SourceSet, SourceSets, StoragePolicyScreen,
};
use crate::shell::{SeenJobs, moved, needing_attention, waiting_on_decision};

/// Without the `bench` feature, `/bench` is not a screen.
#[cfg(not(feature = "bench"))]
use NotFound as Bench;

/// The workbench: a bar of screens over the routes.
#[component]
pub fn App() -> impl IntoView {
    provide_live();
    provide_seen_jobs();
    provide_context(IngestChanged(RwSignal::new(0)));
    view! {
        <Router>
            <a class="skip" href="#main">"Skip to content"</a>
            <header class="bar">
                <a class="brand" href="/">
                    <img src="/icon-64.png" alt="" width="24" height="24" />
                    <span class="brand-name">"Title Select"</span>
                </a>
                <Nav />
                <StreamStatus />
                <ThemeToggle />
            </header>
            <main id="main" tabindex="-1">
                <Routes fallback=|| view! { <NotFound /> }>
                    <Route path=path!("/") view=|| view! { <Redirect path="/source-sets" options=replace() /> } />
                    <Route path=path!("/source-sets") view=SourceSets />
                    // A Source Set's page and its repair nest under it, ready for its tabs (#74).
                    <ParentRoute path=path!("/source-sets/:name") view=Outlet>
                        <Route path=path!("") view=SourceSet />
                        <Route path=path!("repair") view=RepairScreen />
                    </ParentRoute>
                    <Route path=path!("/dats") view=DatRevisions />
                    <Route path=path!("/audits/:id") view=AuditScreen />
                    <Route path=path!("/profiles") view=Profiles />
                    <Route path=path!("/new-profile") view=NewProfile />
                    <Route path=path!("/profiles/:name") view=ProfileScreen />
                    <Route path=path!("/selections/:id") view=SelectionScreen />
                    <Route path=path!("/libraries") view=Libraries />
                    <Route path=path!("/libraries/:id") view=LibraryFolderScreen />
                    // Library folders' pages before #73, kept so that old bookmarks still open them.
                    <Route path=path!("/managed-folders") view=|| view! { <Redirect path="/libraries" options=replace() /> } />
                    <Route path=path!("/managed-folders/:id") view=OldLibraryFolder />
                    <Route path=path!("/builds/:id") view=BuildScreen />
                    <Route path=path!("/ingest") view=IngestFolders />
                    <Route path=path!("/ingest/:root") view=IngestFolderScreen />
                    // Reachable from the source sets page until it becomes a tab of a source set's page (#74).
                    <Route path=path!("/additions") view=Additions />
                    <ParentRoute path=path!("/activity") view=Activity>
                        <Route path=path!("") view=Jobs />
                        <Route path=path!("kept") view=KeptFiles />
                    </ParentRoute>
                    <ParentRoute path=path!("/settings") view=Settings>
                        <Route path=path!("") view=|| view! { <Redirect path="/settings/storage" options=replace() /> } />
                        <Route path=path!("storage") view=StoragePolicyScreen />
                        <Route path=path!("roots") view=Roots />
                        <Route path=path!("about") view=About />
                    </ParentRoute>
                    // Paths from before #71, kept so that old bookmarks still open their page.
                    <Route path=path!("/dat-revisions") view=Moved />
                    <Route path=path!("/ingest-folders") view=Moved />
                    <Route path=path!("/ingest-folders/:root") view=Moved />
                    <Route path=path!("/jobs") view=Moved />
                    <Route path=path!("/storage-policy") view=Moved />
                    <Route path=path!("/about") view=Moved />
                    <Route path=path!("/bench") view=Bench />
                </Routes>
            </main>
            <footer class="footer">
                <A href="/settings/about">"About Title Select and its licences"</A>
            </footer>
        </Router>
    }
}

/// The top navigation, with a badge on Ingest for the items waiting on a decision, and on Activity for the Jobs
/// running or failed and not yet seen.
#[component]
fn Nav() -> impl IntoView {
    let live = use_live();
    let seen = use_seen_jobs();
    let ingest_changed = use_ingest_changed();
    let folders = LocalResource::new(move || {
        live.settled.track();
        ingest_changed.track();
        api::get::<Vec<IngestFolder>>("/ingest-folders")
    });
    let failed = LocalResource::new(move || {
        live.settled.track();
        api::get::<Vec<Job>>("/failed-jobs")
    });
    // Forget the seen Jobs that no longer failed, which keeps local storage bounded. A failure the live Jobs show
    // may be newer than the fetched list, so it stays.
    Effect::new(move || {
        if let Some(Ok(failed)) = failed.get() {
            let mut ids: Vec<i64> = failed.iter().map(|j| j.id).collect();
            live.book.with_untracked(|book| {
                ids.extend(
                    book.newest_first()
                        .iter()
                        .filter(|l| l.job.state == JobState::Failed)
                        .map(|l| l.job.id),
                )
            });
            seen.keep_only(ids);
        }
    });
    let waiting = Signal::derive(move || {
        folders
            .get()
            .and_then(Result::ok)
            .map_or(0, |f| waiting_on_decision(&f))
    });
    let attention = Signal::derive(move || {
        let failed = failed.get().and_then(Result::ok).unwrap_or_default();
        let failed = failed.iter().map(|j| j.id);
        let live = live.book.with(|book| book.newest_first());
        seen.get()
            .with(|seen| needing_attention(live.iter().map(|l| &l.job), failed, seen))
    });
    view! {
        <nav aria-label="Workbench">
            <NavLink href="/source-sets" glyph=Glyph::Database label="Source sets" />
            <NavLink href="/libraries" glyph=Glyph::Library label="Libraries" />
            <NavLink
                href="/ingest"
                glyph=Glyph::FolderInput
                label="Ingest"
                badge=waiting
                badge_label=|n| format!("{n} waiting on your decision")
            />
            <NavLink href="/dats" glyph=Glyph::FileText label="DATs" />
            <NavLink href="/profiles" glyph=Glyph::SlidersHorizontal label="Profiles" />
            <NavLink
                href="/activity"
                glyph=Glyph::Activity
                label="Activity"
                badge=attention
                badge_label=|n| format!("{n} running or failed and not yet seen")
            />
            <NavLink href="/settings" glyph=Glyph::Settings label="Settings" />
        </nav>
    }
}

/// A screen in the top navigation, with an optional count of waiting work shown when it isn't zero.
#[component]
fn NavLink(
    href: &'static str,
    glyph: Glyph,
    label: &'static str,
    #[prop(optional, into)] badge: Option<Signal<u64>>,
    #[prop(optional)] badge_label: Option<fn(u64) -> String>,
) -> impl IntoView {
    let count = move || badge.map_or(0, |b| b.get());
    let words = move |n: u64| badge_label.map(|f| f(n)).unwrap_or_default();
    // Below 48rem the label is hidden, so the tooltip names the screen; the link stays the only tab stop.
    let tip = Signal::derive(move || match count() {
        0 => label.to_string(),
        n => format!("{label}: {}", words(n)),
    });
    view! {
        <Tooltip label=tip>
        <A href=href>
            <LabelledIcon glyph label />
            {move || {
                let n = count();
                (n > 0)
                    .then(|| {
                        view! {
                            <span class="nav-badge" aria-hidden="true">{n}</span>
                            <span class="visually-hidden">{format!(", {}", words(n))}</span>
                        }
                    })
            }}
        </A>
        </Tooltip>
    }
}

/// The Settings pages, as tabs.
#[component]
fn Settings() -> impl IntoView {
    view! {
        <Tabs
            label="Settings"
            tabs=vec![
                Tab { path: "/settings/storage", label: "Storage policy" },
                Tab { path: "/settings/roots", label: "Roots" },
                Tab { path: "/settings/about", label: "About" },
            ]
        />
    }
}

/// Sends a path from before #71 to where it moved.
#[component]
fn Moved() -> impl IntoView {
    let location = use_location();
    let search = location.search.get_untracked();
    let path = format!(
        "{}{}{}{}",
        location.pathname.get_untracked(),
        if search.is_empty() { "" } else { "?" },
        search,
        location.hash.get_untracked(),
    );
    view! { <Redirect path=moved(&path).unwrap_or_else(|| "/".into()) options=replace() /> }
}

/// Legacy paths redirect in place, so that Back doesn't return to the old path only to be sent forward again.
fn replace() -> NavigateOptions {
    NavigateOptions {
        replace: true,
        ..Default::default()
    }
}

/// The local storage key holding the failed Jobs the Operator opened.
const SEEN_JOBS_KEY: &str = "title-select.seen-failed-jobs";

/// The failed Jobs the Operator opened, shared by the Activity badge and the screen that marks them seen.
#[derive(Clone, Copy)]
pub struct Seen {
    seen: Signal<SeenJobs>,
    set: WriteSignal<SeenJobs>,
}

impl Seen {
    pub fn get(self) -> Signal<SeenJobs> {
        self.seen
    }

    /// Marks a failed Job seen, so the Activity badge stops counting it.
    pub fn mark(self, id: i64) {
        if !self.seen.with_untracked(|s| s.is_seen(id)) {
            let mut next = self.seen.get_untracked();
            next.mark(id);
            self.set.set(next);
        }
    }

    /// Marks every failed Job in `ids` seen at once.
    pub fn mark_all(self, ids: impl IntoIterator<Item = i64>) {
        let mut next = self.seen.get_untracked();
        let before = next.clone();
        ids.into_iter().for_each(|id| next.mark(id));
        if next != before {
            self.set.set(next);
        }
    }

    /// Forgets the seen Jobs not in `failed`, the server's list of every Failed Job's ID.
    fn keep_only(self, failed: Vec<i64>) {
        let mut next = self.seen.get_untracked();
        if next.keep_only(failed) {
            self.set.set(next);
        }
    }
}

/// Bumped whenever an ingest screen changes an Ingest Folder, so the Ingest badge counts again: dismissing an item
/// settles no Job.
#[derive(Clone, Copy)]
struct IngestChanged(RwSignal<u64>);

/// The shared ingest-change counter; ingest screens fetch again on it and bump it after each change.
pub fn use_ingest_changed() -> RwSignal<u64> {
    expect_context::<IngestChanged>().0
}

fn provide_seen_jobs() {
    let (seen, set, _) = use_local_storage::<SeenJobs, FromToStringCodec>(SEEN_JOBS_KEY);
    provide_context(Seen { seen, set });
}

pub fn use_seen_jobs() -> Seen {
    expect_context::<Seen>()
}

/// Sends a library folder's page from before #73 to the page it moved to.
#[component]
fn OldLibraryFolder() -> impl IntoView {
    let params = use_params_map();
    let id = params.with_untracked(|p| p.get("id").unwrap_or_default());
    view! { <Redirect path=format!("/libraries/{id}") options=replace() /> }
}

#[component]
fn StreamStatus() -> impl IntoView {
    let live = use_live();
    let active = move || live.book.with(|b| b.active());
    view! {
        <p class="stream" aria-live="polite">
            <span class=move || format!("dot {}", live.stream.get().label().to_lowercase())></span>
            <span class="stream-text">
                {move || match live.stream.get() {
                    Stream::Live => format!("Live · {} jobs active", active()),
                    other => other.label().to_string(),
                }}
            </span>
        </p>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <h1>"Not found"</h1>
        <p>"There is no workbench screen here. Choose one above."</p>
    }
}

/// Navigates within the workbench from an event handler.
pub fn navigator() -> Callback<String> {
    let target = RwSignal::new(None::<String>);
    let navigate = use_navigate();
    Effect::new(move || {
        if let Some(path) = target.get() {
            navigate(&path, Default::default());
        }
    });
    Callback::new(move |path: String| target.set(Some(path)))
}

/// An error from the Service, announced as it appears.
#[component]
pub fn Problem(message: String) -> impl IntoView {
    view! { <p class="problem" role="alert">{message}</p> }
}

#[component]
pub fn Loading(what: &'static str) -> impl IntoView {
    view! { <p class="loading-note" aria-live="polite">{format!("Loading {what}…")}</p> }
}
