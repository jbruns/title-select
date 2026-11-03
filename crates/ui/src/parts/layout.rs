//! Layouts over nested routes. A list page is a `ParentRoute` whose view puts its list in a `SplitView`; a child
//! route, such as `:id`, renders into the split view's `<Outlet/>` as a `DetailPane`, and a `""` child leaves it
//! empty. A page with tabs is a `ParentRoute` whose view shows `Tabs`, one child route per tab.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet};

use super::icon::{Glyph, Svg};
use super::overlay::Tooltip;

/// A list beside the detail pane its nested route renders. On a narrow screen the pane stacks over the list as a
/// drawer.
#[component]
pub fn SplitView(children: Children) -> impl IntoView {
    view! {
        <div class="split">
            <div class="split-list">{children()}</div>
            <Outlet />
        </div>
    }
}

/// The detail of the row a nested route selected, titled `title`. `close` is where its close link goes: the list's
/// own path, with its query string.
#[component]
pub fn DetailPane(
    #[prop(into)] title: Signal<String>,
    #[prop(into)] close: Signal<String>,
    children: Children,
) -> impl IntoView {
    view! {
        <aside class="detail-pane" aria-label=move || title.get()>
            <header class="detail-head">
                <h2>{move || title.get()}</h2>
                <Tooltip label="Close the detail">
                    <A href=move || close.get() attr:class="icon-button" attr:aria-label="Close the detail">
                        <Svg glyph=Glyph::X />
                    </A>
                </Tooltip>
            </header>
            <div class="detail-body">{children()}</div>
        </aside>
    }
}

/// One tab: its child route's path, relative to the page, and its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tab {
    pub path: &'static str,
    pub label: &'static str,
}

/// A page's tabs, named `label` for assistive technology, over the child route the selected tab renders.
#[component]
pub fn Tabs(label: &'static str, tabs: Vec<Tab>) -> impl IntoView {
    view! {
        <nav class="tabs" aria-label=label>
            {tabs
                .into_iter()
                .map(|t| view! { <A href=t.path exact=true>{t.label}</A> })
                .collect_view()}
        </nav>
        <Outlet />
    }
}
