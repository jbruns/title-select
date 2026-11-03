//! The Roots in the Service's configuration, read-only: they change only in its config file.

use leptos::prelude::*;
use title_select_api::Root;

use crate::api;
use crate::parts::pending::Pending;

fn access(root: &Root) -> &'static str {
    match (root.ingest, root.writable) {
        (true, _) => "Ingest folder",
        (false, true) => "Writable",
        (false, false) => "Read-only",
    }
}

#[component]
pub fn Roots() -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let problem = Signal::derive(move || roots.get().and_then(Result::err));
    view! {
        <h1>"Roots"</h1>
        <p class="lede">
            "The folders the service works in, from its configuration file. To change them, edit that file and restart the service."
        </p>
        <Pending pending=Signal::derive(move || roots.get().is_none()) status="Loading roots…" problem>
            {move || {
                roots
                    .get()
                    .and_then(Result::ok)
                    .map(|roots| {
                        view! {
                            <div class="table-scroll">
                                <table aria-label="Roots">
                                    <thead>
                                        <tr>
                                            <th scope="col">
                                                <span class="label">"Name"</span>
                                            </th>
                                            <th scope="col">
                                                <span class="label">"Path"</span>
                                            </th>
                                            <th scope="col">
                                                <span class="label">"Access"</span>
                                            </th>
                                            <th scope="col">
                                                <span class="label">"Writable paths"</span>
                                            </th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {roots
                                            .into_iter()
                                            .map(|root| {
                                                let kind = access(&root);
                                                let path = root.path.clone();
                                                view! {
                                                    <tr>
                                                        <td>{root.name}</td>
                                                        <td>
                                                            <code title=path>{root.path}</code>
                                                        </td>
                                                        <td>{kind}</td>
                                                        <td>
                                                            {root
                                                                .writable_paths
                                                                .into_iter()
                                                                .map(|p| view! { <code>{p}</code> " " })
                                                                .collect_view()}
                                                        </td>
                                                    </tr>
                                                }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                    })
            }}
        </Pending>
    }
}
