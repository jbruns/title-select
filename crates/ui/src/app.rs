use leptos::prelude::*;
use leptos_router::components::{A, Outlet, ParentRoute, Redirect, Route, Router, Routes};
use leptos_router::hooks::{use_navigate, use_params_map};
use leptos_router::path;

use crate::events::{Stream, provide_live, use_live};
use crate::parts::theme::ThemeToggle;
#[cfg(feature = "bench")]
use crate::screens::Bench;
use crate::screens::{
    About, Additions, AuditScreen, BuildScreen, DatRevisions, IngestFolderScreen, IngestFolders,
    Jobs, Libraries, LibraryFolderScreen, NewProfile, ProfileScreen, Profiles, RepairScreen,
    SelectionScreen, SourceSet, SourceSets, StoragePolicyScreen,
};

/// Without the `bench` feature, `/bench` is not a screen.
#[cfg(not(feature = "bench"))]
use NotFound as Bench;

/// The workbench: a bar of screens over the routes.
#[component]
pub fn App() -> impl IntoView {
    provide_live();
    view! {
        <Router>
            <a class="skip" href="#main">"Skip to content"</a>
            <header class="bar">
                <a class="brand" href="/">
                    <img src="/icon-64.png" alt="" width="24" height="24" />
                    "Title Select"
                </a>
                <nav aria-label="Workbench">
                    <A href="/source-sets">"Source sets"</A>
                    <A href="/dat-revisions">"DAT revisions"</A>
                    <A href="/profiles">"Selection profiles"</A>
                    <A href="/libraries">"Libraries"</A>
                    <A href="/storage-policy">"Storage policy"</A>
                    <A href="/ingest-folders">"Ingest folders"</A>
                    <A href="/additions">"Additions"</A>
                    <A href="/jobs">"Jobs"</A>
                </nav>
                <StreamStatus />
                <ThemeToggle />
            </header>
            <main id="main" tabindex="-1">
                <Routes fallback=|| view! { <NotFound /> }>
                    <Route path=path!("/") view=|| view! { <Redirect path="/source-sets" /> } />
                    <Route path=path!("/source-sets") view=SourceSets />
                    // A Source Set's page and its repair nest under it, ready for its tabs (#74).
                    <ParentRoute path=path!("/source-sets/:name") view=Outlet>
                        <Route path=path!("") view=SourceSet />
                        <Route path=path!("repair") view=RepairScreen />
                    </ParentRoute>
                    <Route path=path!("/dat-revisions") view=DatRevisions />
                    <Route path=path!("/audits/:id") view=AuditScreen />
                    <Route path=path!("/profiles") view=Profiles />
                    <Route path=path!("/new-profile") view=NewProfile />
                    <Route path=path!("/profiles/:name") view=ProfileScreen />
                    <Route path=path!("/selections/:id") view=SelectionScreen />
                    <Route path=path!("/libraries") view=Libraries />
                    <Route path=path!("/libraries/:id") view=LibraryFolderScreen />
                    // Library folders' pages before #73, kept so that old bookmarks still open them.
                    <Route path=path!("/managed-folders") view=|| view! { <Redirect path="/libraries" /> } />
                    <Route path=path!("/managed-folders/:id") view=OldLibraryFolder />
                    <Route path=path!("/builds/:id") view=BuildScreen />
                    <Route path=path!("/ingest-folders") view=IngestFolders />
                    <Route path=path!("/ingest-folders/:root") view=IngestFolderScreen />
                    <Route path=path!("/storage-policy") view=StoragePolicyScreen />
                    <Route path=path!("/additions") view=Additions />
                    <Route path=path!("/jobs") view=Jobs />
                    <Route path=path!("/about") view=About />
                    <Route path=path!("/bench") view=Bench />
                </Routes>
            </main>
            <footer class="footer">
                <A href="/about">"About Title Select and its licences"</A>
            </footer>
        </Router>
    }
}

/// Sends a library folder's page from before #73 to the page it moved to.
#[component]
fn OldLibraryFolder() -> impl IntoView {
    let params = use_params_map();
    let id = params.with_untracked(|p| p.get("id").unwrap_or_default());
    view! { <Redirect path=format!("/libraries/{id}") /> }
}

#[component]
fn StreamStatus() -> impl IntoView {
    let live = use_live();
    let active = move || live.book.with(|b| b.active());
    view! {
        <p class="stream" aria-live="polite">
            <span class=move || format!("dot {}", live.stream.get().label().to_lowercase())></span>
            {move || match live.stream.get() {
                Stream::Live => format!("Live · {} jobs active", active()),
                other => other.label().to_string(),
            }}
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
