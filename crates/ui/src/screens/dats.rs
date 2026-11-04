use std::cmp::Ordering;
use std::collections::BTreeMap;

use leptos::html::Form;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::{
    AuditSummary, CuePackImport, CuePackRecord, Dat, DatRevisionRecord, ImportOutcome,
    ImportedDatRevision, SourceSetRecord,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::parts::icon::Glyph;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, List, ListRow, RowAction, ToolAction, Width, last_good_rows,
};
use crate::parts::overlay::Drawer;
use crate::parts::status::{DatFileStatus, StatusIcon};

/// An Audit that refers to a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsedBy {
    audit: i64,
    source_set: String,
    pruned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevisionItem {
    revision: DatRevisionRecord,
    used_by: Vec<UsedBy>,
    /// The cue packs imported for its DAT.
    cue_packs: Vec<CuePackRecord>,
}

impl RevisionItem {
    fn file_status(&self) -> DatFileStatus {
        if self.revision.blob_present {
            DatFileStatus::Kept
        } else {
            DatFileStatus::Deleted
        }
    }

    /// The Audits that refer to it, newest first.
    fn audits_newest_first(&self) -> Vec<&UsedBy> {
        let mut audits: Vec<&UsedBy> = self.used_by.iter().collect();
        audits.sort_by_key(|a| std::cmp::Reverse(a.audit));
        audits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionCol {
    File,
    Dat,
    Version,
    Imported,
    Audits,
}

impl ListRow for RevisionItem {
    type Col = RevisionCol;
    const COLUMNS: &'static [RevisionCol] = &[
        RevisionCol::File,
        RevisionCol::Dat,
        RevisionCol::Version,
        RevisionCol::Imported,
        RevisionCol::Audits,
    ];

    fn column(col: RevisionCol) -> Column {
        match col {
            RevisionCol::File => Column::new("file", "File", Width::Fixed(3.5))
                .sortable()
                .hint("Whether the store still keeps its DAT file"),
            RevisionCol::Dat => Column::new(
                "dat",
                "DAT",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            RevisionCol::Version => {
                Column::new("version", "Version", Width::Flex { min: 7.0, share: 1 }).sortable()
            }
            RevisionCol::Imported => {
                Column::new("imported", "Imported", Width::Fixed(13.0)).sortable()
            }
            RevisionCol::Audits => Column::new("audits", "Audits", Width::Fixed(5.5))
                .sortable()
                .numeric()
                .hint("How many audits refer to it"),
        }
    }

    fn key(&self) -> String {
        self.revision.sha256.clone()
    }

    fn title(&self) -> String {
        format!("{} {}", self.revision.dat(), self.revision.version)
    }

    fn search(&self) -> String {
        let r = &self.revision;
        format!("{} {} {} {}", r.dat(), r.version, r.file, r.sha256)
    }

    fn cell(&self, col: RevisionCol) -> AnyView {
        let r = &self.revision;
        match col {
            RevisionCol::File => {
                view! { <StatusIcon status=self.file_status() tab_stop=false /> }.into_any()
            }
            RevisionCol::Dat => view! { <Clip text=r.dat() /> }.into_any(),
            RevisionCol::Version => view! { <Clip text=r.version.clone() /> }.into_any(),
            RevisionCol::Imported => view! { <Clip text=when(&r.imported_at) /> }.into_any(),
            RevisionCol::Audits => self.used_by.len().to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: RevisionCol) -> Ordering {
        let (a, b) = (&self.revision, &other.revision);
        match col {
            RevisionCol::File => a.blob_present.cmp(&b.blob_present),
            RevisionCol::Dat => a.dat().cmp(&b.dat()),
            RevisionCol::Version => a.version.cmp(&b.version),
            RevisionCol::Imported => a.imported_at.cmp(&b.imported_at),
            RevisionCol::Audits => self.used_by.len().cmp(&other.used_by.len()),
        }
    }
}

/// Every DAT Revision, with the Audits of every Source Set's history that refer to it.
async fn revisions() -> Result<Vec<RevisionItem>, String> {
    let dats: Vec<Dat> = api::get("/dats").await?;
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    let packs: Vec<CuePackRecord> = api::get("/cue-packs").await?;
    let mut used_by: BTreeMap<String, Vec<UsedBy>> = BTreeMap::new();
    for set in sets {
        let history: Vec<AuditSummary> =
            api::get(&format!("/source-sets/{}/history", api::segment(&set.name))).await?;
        for audit in history {
            used_by
                .entry(audit.dat_revision_sha256)
                .or_default()
                .push(UsedBy {
                    audit: audit.id,
                    source_set: audit.source_set,
                    pruned: audit.pruned,
                });
        }
    }
    Ok(dats
        .into_iter()
        .flat_map(|dat| dat.revisions.into_iter().rev())
        .map(|revision| RevisionItem {
            used_by: used_by.remove(&revision.sha256).unwrap_or_default(),
            cue_packs: packs
                .iter()
                .filter(|p| p.dat == revision.dat())
                .cloned()
                .collect(),
            revision,
        })
        .collect())
}

/// Puts `text` on the clipboard, then says so in `said`.
fn copy(text: String, what: String, said: RwSignal<Option<String>>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = window.navigator().clipboard().write_text(&text);
    leptos::task::spawn_local(async move {
        let message = match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(_) => format!("Copied {what}."),
            Err(_) => format!("Couldn't copy {what}: the browser didn't allow it."),
        };
        said.set(Some(message));
    });
}

/// The actions on one DAT Revision: open its newest Audit, copy its SHA-256, and open its other Audits.
fn revision_actions(item: &RevisionItem, said: RwSignal<Option<String>>) -> Vec<RowAction> {
    let audits = item.audits_newest_first();
    let mut actions = Vec::new();
    if let Some(latest) = audits.first() {
        actions.push(RowAction::go(
            Glyph::ClipboardList,
            format!("Open its latest audit, #{}", latest.audit),
            format!("/audits/{}", latest.audit),
        ));
    }
    let sha = item.revision.sha256.clone();
    actions.push(RowAction::run(Glyph::Copy, "Copy its SHA-256", move || {
        copy(sha.clone(), "the SHA-256".to_string(), said)
    }));
    actions.extend(audits.iter().skip(1).map(|u| {
        RowAction::go(
            Glyph::ClipboardList,
            format!("Open audit #{} of {}", u.audit, u.source_set),
            format!("/audits/{}", u.audit),
        )
    }));
    actions
}

/// The DATs and their DAT Revisions, with their provenance, and the form to import another.
#[component]
pub fn DatRevisions() -> impl IntoView {
    let live = use_live();
    let uploaded = RwSignal::new(0u32);
    let items = LocalResource::new(move || {
        live.settled.track();
        uploaded.track();
        revisions()
    });
    let rows = last_good_rows(move || items.get());
    let statuses =
        Signal::derive(move || present(rows.get().iter().map(RevisionItem::file_status)));
    let importing = RwSignal::new(false);
    let importing_cues = RwSignal::new(false);
    let said = RwSignal::new(None::<String>);
    let actions = vec![
        ToolAction::page(Glyph::Upload, "Import", move || importing.set(true)),
        ToolAction::page(Glyph::Upload, "Import cue pack", move || {
            importing_cues.set(true)
        }),
        ToolAction::checked(Glyph::Copy, "Copy SHA-256s", move |chosen| {
            let n = chosen.rows.len();
            let text = chosen
                .rows
                .iter()
                .map(|r: &RevisionItem| r.revision.sha256.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let what = if n == 1 {
                "1 SHA-256".to_string()
            } else {
                format!("{n} SHA-256s")
            };
            copy(text, what, said);
        }),
    ];

    view! {
        <h1>"DAT revisions"</h1>
        <p class="lede">
            "Each import of a DAT file is a DAT revision, named by its SHA-256. Newest import first within each DAT. \
             Import a cue pack, a ZIP of a disc system's cue sheets, and a repair can fill a missing cue sheet, or \
             replace one that doesn't match, from it."
        </p>
        {move || match items.get() {
            None => view! { <Loading what="the DAT revisions" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <p class="done" role="status">
            {move || said.get()}
        </p>
        <List
            id="dat-revisions"
            label="DAT revisions"
            rows
            empty="No DAT revisions yet. Choose Import to add a DAT."
            placeholder="Filter by DAT, version, file or SHA-256"
            actions
            row_actions=Callback::new(move |item: RevisionItem| revision_actions(&item, said))
            quick=2
            detail=Callback::new(revision_detail)
            legend=move || view! { <Legend statuses list="dat-revisions" /> }
        />
        <Drawer open=importing title="Import a DAT">
            <Upload on_done=Callback::new(move |_| uploaded.update(|n| *n += 1)) />
        </Drawer>
        <Drawer open=importing_cues title="Import a cue pack">
            <CuePackUpload on_done=Callback::new(move |_| uploaded.update(|n| *n += 1)) />
        </Drawer>
    }
}

#[component]
fn CuePackUpload(on_done: Callback<()>) -> impl IntoView {
    let form = NodeRef::<Form>::new();
    let outcome = RwSignal::new(None::<Result<CuePackImport, String>>);
    let busy = RwSignal::new(false);
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(el) = form.get_untracked() else {
            return;
        };
        let Ok(data) = web_sys::FormData::new_with_form(&el) else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post_form::<CuePackImport>("/cue-packs", data).await;
            busy.set(false);
            if result.is_ok() {
                el.reset();
                on_done.run(());
            }
            outcome.set(Some(result));
        });
    };
    view! {
        <section aria-label="Import a cue pack">
            <form class="form" node_ref=form on:submit=submit>
                <label>
                    "DAT"
                    <select name="dat" required>
                        {move || {
                            dats.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|d| view! { <option value=d.dat.clone()>{d.dat.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Source URL"
                    <input type="url" name="source_url" placeholder="Where the cue pack came from" />
                </label>
                <label>
                    "Cue pack"
                    <input type="file" name="file" required accept=".zip" />
                </label>
                <button type="submit" class="primary" disabled=busy>
                    {move || if busy.get() { "Importing…" } else { "Import" }}
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(imported) => {
                            let p = imported.pack;
                            let what = match imported.outcome {
                                ImportOutcome::Added => "Added cue pack",
                                ImportOutcome::AlreadyImported => "Already imported: cue pack",
                                ImportOutcome::Restored => "Restored the kept file of cue pack",
                            };
                            let sheets = match p.cues {
                                1 => "1 cue sheet".to_string(),
                                n => format!("{n} cue sheets"),
                            };
                            view! {
                                <p class="done" role="status">
                                    {format!("{what} {} for {} ({sheets}).", p.file, p.dat)}
                                </p>
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}

fn revision_detail(item: RevisionItem) -> AnyView {
    let r = item.revision;
    view! {
        <dl class="facts">
            <dt>"SHA-256"</dt>
            <dd>
                <code class="hash">{r.sha256.clone()}</code>
            </dd>
            <dt>"File"</dt>
            <dd>
                <code class="path">{r.file.clone()}</code>
            </dd>
            <dt>"Provider"</dt>
            <dd>{r.provider.clone()}</dd>
            <dt>"Source URL"</dt>
            <dd>
                {match r.source_url.clone() {
                    Some(url) => view! { <code class="path">{url}</code> }.into_any(),
                    None => "Not recorded".into_any(),
                }}
            </dd>
            <dt>"Imported"</dt>
            <dd>{when(&r.imported_at)}</dd>
            <dt>"File kept"</dt>
            <dd>
                {if r.blob_present {
                    "Kept in the store"
                } else {
                    "Deleted from the store; the DAT revision is still recorded, and importing the same file again restores it"
                }}
            </dd>
            <dt>"Audits"</dt>
            <dd>
                {if item.used_by.is_empty() {
                    view! { "No audit refers to it" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .used_by
                                .into_iter()
                                .map(|u| {
                                    view! {
                                        <li>
                                            <A href=format!("/audits/{}", u.audit)>{format!("Audit #{}", u.audit)}</A>
                                            {format!(
                                                " of {}{}",
                                                u.source_set,
                                                if u.pruned { ", pruned" } else { "" },
                                            )}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
            <dt>"Cue packs"</dt>
            <dd>
                {if item.cue_packs.is_empty() {
                    view! { "None imported for this DAT" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .cue_packs
                                .into_iter()
                                .map(|p| {
                                    let sheets = match p.cues {
                                        1 => "1 cue sheet".to_string(),
                                        n => format!("{n} cue sheets"),
                                    };
                                    let kept = if p.blob_present {
                                        ""
                                    } else {
                                        "; its file was deleted since a later pack replaced it, so import it again to use it"
                                    };
                                    view! {
                                        <li>
                                            <code class="path">{p.file}</code>
                                            {format!(", {sheets}, imported {}{kept}", when(&p.imported_at))}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
        </dl>
    }
    .into_any()
}

#[component]
fn Upload(on_done: Callback<()>) -> impl IntoView {
    let form = NodeRef::<Form>::new();
    let outcome = RwSignal::new(None::<Result<ImportedDatRevision, String>>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(el) = form.get_untracked() else {
            return;
        };
        let Ok(data) = web_sys::FormData::new_with_form(&el) else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post_form::<ImportedDatRevision>("/dat-revisions", data).await;
            busy.set(false);
            if result.is_ok() {
                el.reset();
                on_done.run(());
            }
            outcome.set(Some(result));
        });
    };
    view! {
        <section aria-label="Import a DAT">
            <form class="form" node_ref=form on:submit=submit>
                <label>
                    "Provider"
                    <input type="text" name="provider" required placeholder="No-Intro" />
                </label>
                <label>
                    "Source URL"
                    <input type="url" name="source_url" placeholder="Where the DAT came from" />
                </label>
                <label>
                    "DAT file"
                    <input type="file" name="file" required accept=".dat,.xml" />
                </label>
                <button type="submit" class="primary" disabled=busy>
                    {move || if busy.get() { "Importing…" } else { "Import" }}
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(imported) => {
                            let r = imported.dat_revision;
                            let what = match imported.outcome {
                                ImportOutcome::Added => "Added DAT revision",
                                ImportOutcome::AlreadyImported => "Already imported: DAT revision",
                                ImportOutcome::Restored => "Restored the kept file of DAT revision",
                            };
                            view! {
                                <p class="done" role="status">
                                    {format!("{what} {} {} ({}).", r.dat(), r.version, short_sha(&r.sha256))}
                                </p>
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}
