use gloo_net::http::Request;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_location;

use crate::app::Loading;
use crate::model::is_plain_text;

const RETOOL: &str = "https://github.com/unexpectedpanda/retool";
const CLONE_LISTS: &str = "https://github.com/unexpectedpanda/retool-clonelists-metadata";

/// A licence text shipped beside the workbench's bundle, under `/licences/`.
struct Licence {
    title: &'static str,
    file: &'static str,
    /// Where to read it when this build does not ship it; Title Select's own is in its source.
    upstream: Option<&'static str>,
}

const LICENCES: &[Licence] = &[
    Licence {
        title: "Title Select: MIT License",
        file: "/licences/title-select.txt",
        upstream: None,
    },
    Licence {
        title: "Retool: BSD 3-Clause License",
        file: "/licences/retool.txt",
        upstream: Some("https://github.com/unexpectedpanda/retool/blob/main/LICENSE"),
    },
    Licence {
        title: "Retool clone lists and metadata: BSD 3-Clause License",
        file: "/licences/retool-clonelists-metadata.txt",
        upstream: Some(
            "https://github.com/unexpectedpanda/retool-clonelists-metadata/blob/main/LICENSE",
        ),
    },
    Licence {
        title: "chdman, from MAME: GNU General Public License, version 2",
        file: "/licences/mame.txt",
        upstream: Some("https://github.com/mamedev/mame/blob/master/COPYING"),
    },
    Licence {
        title: "Lucide icons: ISC License",
        file: "/licences/lucide.txt",
        upstream: Some("https://github.com/lucide-icons/lucide/blob/main/LICENSE"),
    },
];

/// The text at `file`, or `None` when the bundle does not have it: the Service then answers with the workbench's
/// page instead.
async fn fetch(file: &'static str) -> Option<String> {
    let response = Request::get(file).send().await.ok()?;
    let plain = is_plain_text(response.headers().get("content-type").as_deref());
    if !response.ok() || !plain {
        return None;
    }
    response.text().await.ok()
}

/// Where selection comes from, how its default orders work, and the licences of Title Select and of the projects
/// it is derived from.
#[component]
pub fn About() -> impl IntoView {
    let hash = Hash(StoredValue::new(use_location().hash.get_untracked()));
    request_animation_frame(move || hash.scroll());
    provide_context(hash);
    view! {
        <h1>"About Title Select"</h1>
        <p class="lede">
            "Title Select audits your source sets against DAT files and records exactly what was verified, and against which DAT revision."
        </p>
        <section aria-labelledby="selection-title">
            <h2 id="selection-title">"Where selection comes from"</h2>
            <p>
                "Selection is derived from " <a href=RETOOL>"Retool"</a>
                ", with the community-maintained " <a href=CLONE_LISTS>"Retool clone lists and metadata"</a>
                ". Title Select's selection engine is a port of Retool, built into Title Select: it selects as Retool does, offline, with a pinned copy of the clone lists and metadata, and explains each choice from the trace Retool would print. Both projects are published under the BSD 3-Clause License, shown below."
            </p>
            <p class="note">
                "To see the clone-list commit this build is pinned to, run " <code>"title-select --version"</code>
                "."
            </p>
        </section>
        <section id="default-orders" aria-labelledby="default-orders-title">
            <h2 id="default-orders-title">"The default orders"</h2>
            <p>
                "When you leave the region order or the video order of a selection profile empty, for the whole profile or for one system, selection uses the default order. Your selection profile lists it in full: open \"The default order\" under the setting."
            </p>
            <p>
                "The default region order puts USA first, then the regions whose releases are most often in English, then the rest of the world. The default video order prefers NTSC, then PAL. So the defaults suit English speakers."
            </p>
            <p>
                "They are Retool's own default orders, the ones its \"Use default order for English speakers\" button fills in. Title Select uses them as the pinned copy of the clone lists and metadata ships them, and does not change them."
            </p>
            <p>
                "To prefer other regions, open a " <A href="/profiles">"selection profile"</A>
                " and choose \"Start from the default order\", then move, add or remove regions. A region you leave out of your own order is not selected. \"Reset to the default order\" empties the order again, so the default applies."
            </p>
        </section>
        <section aria-labelledby="licences-title">
            <h2 id="licences-title">"Licences"</h2>
            {LICENCES.iter().map(|licence| view! { <LicenceText licence /> }).collect_view()}
            <p class="note">
                "The workbench's fonts, Liberation Sans and Liberation Mono, are served under the "
                <a href="/fonts/LICENSE-OFL.txt">"SIL Open Font License"</a> "."
            </p>
        </section>
    }
}

/// The `#` part of the link that opened the page, naming a section.
#[derive(Clone, Copy)]
struct Hash(StoredValue<String>);

impl Hash {
    /// Scrolls to the section. The browser cannot do it itself: the link arrives before the section exists.
    fn scroll(self) {
        let hash = self.0.get_value();
        let id = hash.trim_start_matches('#');
        if let Some(section) = (!id.is_empty())
            .then(|| document().get_element_by_id(id))
            .flatten()
        {
            section.scroll_into_view();
        }
    }
}

#[component]
fn LicenceText(licence: &'static Licence) -> impl IntoView {
    let text = LocalResource::new(move || fetch(licence.file));
    // A licence text that arrives lengthens the page, so the section the link names may only now be reachable.
    let hash = use_context::<Hash>();
    Effect::new(move || {
        if let (true, Some(hash)) = (text.with(Option::is_some), hash) {
            request_animation_frame(move || hash.scroll());
        }
    });
    view! {
        <section class="licence" aria-label=licence.title>
            <h3>{licence.title}</h3>
            {move || match text.get() {
                None => view! { <Loading what="the licence" /> }.into_any(),
                Some(Some(text)) => view! { <pre class="licence-text">{text}</pre> }.into_any(),
                Some(None) => {
                    view! {
                        <p class="problem" role="alert">
                            "This build does not include this licence text. "
                            {match licence.upstream {
                                Some(url) => view! { "Read it at " <a href=url>{url}</a> "." }.into_any(),
                                None => "It is the LICENSE file at the root of Title Select's source.".into_any(),
                            }}
                        </p>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}
