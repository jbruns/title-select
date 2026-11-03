//! The list every list screen uses: a toolbar of actions over a filterable, sortable, virtualized grid of keyed rows
//! with a checkbox column and quick actions, a legend strip under it, and the open row's detail beside it (in a
//! drawer on a narrow screen). Its filter, sort, checked rows and open row live in the URL (`parts::url_state`).
//! The pure logic is in `parts::list_model`.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::html;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_query_map};
use leptos_use::{UseElementSizeReturn, use_element_size, use_media_query};
use wasm_bindgen::JsCast;

use super::icon::{Glyph, IconButton, LabelledIcon, Svg};
use super::layout::DetailPane;
pub use super::list_model::Width;
use super::list_model::{self as model, Checked, Step};
use super::overlay::{Drawer, Menu, MenuItem, Tooltip};
use super::url_state::{ListQuery, query_string, use_list_query};

/// Every row is this tall, here and in the stylesheet (`--table-row-min-height`).
pub const ROW_HEIGHT: f64 = 40.0;
/// Below this width the open row's detail is a drawer over the list rather than a pane beside it.
pub const NARROW: &str = "(max-width: 68.75rem)";
const OVERSCAN: usize = 8;
const CHECK_REM: f32 = 2.5;
const ACTION_REM: f32 = 2.25;
const TOOLBAR_GAP: f64 = 8.0;
const MORE_WIDTH: f64 = 48.0;

/// How a list shows one column.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// Names the column in the URL's `sort`.
    pub id: &'static str,
    pub header: &'static str,
    pub width: Width,
    pub sortable: bool,
    /// Right-aligned, in the data font.
    pub numeric: bool,
    /// The header's tooltip, saying what the column holds.
    pub hint: Option<&'static str>,
}

impl Column {
    pub const fn new(id: &'static str, header: &'static str, width: Width) -> Self {
        Column {
            id,
            header,
            width,
            sortable: false,
            numeric: false,
            hint: None,
        }
    }

    pub const fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }

    pub const fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    pub const fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
}

/// A row type a `List` shows.
pub trait ListRow: Clone + Send + Sync + 'static {
    type Col: Copy + Eq + Send + Sync + 'static;
    const COLUMNS: &'static [Self::Col];
    fn column(col: Self::Col) -> Column;
    /// Identifies the row in the URL (`row`, `sel`) and keys its view: stable across reloads, unique in the list.
    fn key(&self) -> String;
    /// Names the row: the detail's title, and the checkbox's and actions' accessible names.
    fn title(&self) -> String;
    /// The text the filter matches, case aside.
    fn search(&self) -> String;
    fn cell(&self, col: Self::Col) -> AnyView;
    /// The order of two rows by a sortable column.
    fn compare(&self, _other: &Self, _col: Self::Col) -> Ordering {
        Ordering::Equal
    }
    /// The row's own page, where it has one: the detail links to it as "Open".
    fn href(&self) -> Option<String> {
        None
    }
}

/// What a row or toolbar action does: go to a page, or run.
#[derive(Clone)]
pub enum Act {
    Go(String),
    Run(Callback<()>),
}

/// An action on one row: the first `quick` of a row's actions are icons at the row's end, shown on hover or when the
/// row has the keyboard cursor, and the rest are under ⋯. The detail shows them all, labelled.
#[derive(Clone)]
pub struct RowAction {
    pub glyph: Glyph,
    pub label: String,
    pub act: Act,
}

impl RowAction {
    pub fn go(glyph: Glyph, label: impl Into<String>, href: impl Into<String>) -> Self {
        RowAction {
            glyph,
            label: label.into(),
            act: Act::Go(href.into()),
        }
    }

    pub fn run(
        glyph: Glyph,
        label: impl Into<String>,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        RowAction {
            glyph,
            label: label.into(),
            act: Act::Run(Callback::new(move |()| run())),
        }
    }
}

/// The checked rows a toolbar action acts on.
#[derive(Clone)]
pub struct Selected<T> {
    /// The checked rows in the list, in list order.
    pub rows: Vec<T>,
    /// With every row matching the filter checked: the filter, so an action on the Service can apply to every match.
    pub matching: Option<String>,
    /// With `matching`, the keys of the rows unchecked since.
    pub left_out: Vec<String>,
}

#[derive(Clone)]
enum ToolRun<T: 'static> {
    Page(Callback<()>),
    Checked(Callback<Selected<T>>),
}

/// A toolbar button, an icon with its label: for the page, or for the checked rows (disabled while none are).
/// Buttons that don't fit go under the toolbar's ⋯.
#[derive(Clone)]
pub struct ToolAction<T: 'static> {
    glyph: Glyph,
    label: String,
    run: ToolRun<T>,
}

impl<T: Clone + Send + Sync + 'static> ToolAction<T> {
    pub fn page(
        glyph: Glyph,
        label: impl Into<String>,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        ToolAction {
            glyph,
            label: label.into(),
            run: ToolRun::Page(Callback::new(move |()| run())),
        }
    }

    pub fn checked(
        glyph: Glyph,
        label: impl Into<String>,
        run: impl Fn(Selected<T>) + Send + Sync + 'static,
    ) -> Self {
        ToolAction {
            glyph,
            label: label.into(),
            run: ToolRun::Checked(Callback::new(run)),
        }
    }
}

/// A screen's own narrowing of a list beside its filter text, such as a status chosen from a `<select>`.
pub type Keep<T> = Option<Arc<dyn Fn(&T) -> bool + Send + Sync>>;

/// Text for a cell: cut off with an ellipsis where it doesn't fit, with the whole text as a tooltip then.
#[component]
pub fn Clip(#[prop(into)] text: String) -> impl IntoView {
    view! {
        <Tooltip label=text.clone() clipped=true>
            {text}
        </Tooltip>
    }
}

/// The list. `id` prefixes its elements' IDs and names its legend's remembered state; `label` names it ("DAT
/// revisions"). `rows` are every row, in their default order; `detail` renders the open row's summary.
#[component]
pub fn List<T: ListRow>(
    id: &'static str,
    #[prop(into)] label: String,
    #[prop(into)] rows: Signal<Arc<Vec<T>>>,
    /// Shown when there are no rows at all.
    #[prop(into)]
    empty: String,
    detail: Callback<T, AnyView>,
    #[prop(optional)] actions: Vec<ToolAction<T>>,
    #[prop(optional)] row_actions: Option<Callback<T, Vec<RowAction>>>,
    /// How many of a row's actions are icons at its end; one or two.
    #[prop(default = 1)]
    quick: usize,
    #[prop(optional, into)] placeholder: Option<String>,
    /// More filter controls, beside the filter text.
    #[prop(optional, into)]
    filters: Option<ViewFn>,
    #[prop(optional, into)] keep: Option<Signal<Keep<T>>>,
    /// The legend strip under the list: a `Legend` of the statuses its rows show.
    #[prop(optional, into)]
    legend: Option<ViewFn>,
) -> impl IntoView {
    let (query, set_query) = use_list_query();
    let query_map = use_query_map();
    let location = use_location();
    let go = crate::app::navigator();
    let update = move |change: &dyn Fn(&mut ListQuery)| {
        let mut q = query.get_untracked();
        change(&mut q);
        set_query.run(q);
    };

    // Each derived step reruns only when what it reads changes: a keystroke refilters, but doesn't resort.
    let keys =
        Memo::new(move |_| rows.with(|r| Arc::new(r.iter().map(T::key).collect::<Vec<_>>())));
    let search = Memo::new(move |_| {
        rows.with(|r| {
            Arc::new(
                r.iter()
                    .map(|r| r.search().to_lowercase())
                    .collect::<Vec<_>>(),
            )
        })
    });
    let generation = Memo::new(move |prev: Option<&u64>| {
        rows.track();
        prev.map_or(0, |g| g + 1)
    });
    let filter = Memo::new(move |_| query.with(|q| q.filter.clone()));
    let needle = Memo::new(move |_| filter.with(|f| model::needle(f)));
    let open_key = Memo::new(move |_| query.with(|q| q.selected.clone()));
    let checked = Memo::new(move |_| query.with(|q| q.checked.clone()));
    let sort = Memo::new(move |_| {
        query.with(|q| {
            let s = q.sort.as_ref()?;
            T::COLUMNS
                .iter()
                .copied()
                .find(|c| {
                    let column = T::column(*c);
                    column.sortable && column.id == s.column
                })
                .map(|c| (c, s.descending))
        })
    });
    let order = Memo::new(move |_| {
        let sort = sort.get();
        rows.with(|r| {
            Arc::new(match sort {
                Some((col, descending)) => model::sorted(r, |a, b| a.compare(b, col), descending),
                None => (0..r.len() as u32).collect(),
            })
        })
    });
    let shown = Memo::new(move |_| {
        let keep = keep.and_then(|k| k.get());
        rows.with(|r| {
            search.with(|s| {
                order.with(|o| {
                    needle.with(|n| {
                        Arc::new(match &keep {
                            Some(keep) => model::matching(s, o, n, |i| keep(&r[i])),
                            None => model::matching(s, o, n, |_| true),
                        })
                    })
                })
            })
        })
    });
    let count = Memo::new(move |_| shown.with(|s| s.len()));
    let total = Memo::new(move |_| rows.with(|r| r.len()));

    // How many of the shown rows are listed in the selection's keys.
    let listed = Memo::new(move |_| {
        checked.with(|c| {
            if c.keys.is_empty() {
                return 0;
            }
            shown.with(|s| {
                keys.with(|k| {
                    s.iter()
                        .filter(|&&i| c.keys.contains(&k[i as usize]))
                        .count()
                })
            })
        })
    });
    let checked_count = Memo::new(move |_| checked.with(|c| c.count(count.get(), listed.get())));
    let header = Memo::new(move |_| checked.with(|c| c.checked(count.get(), listed.get())));

    if let Some(keep) = keep {
        Effect::watch(
            move || keep.with(|k| k.as_ref().map(|k| Arc::as_ptr(k) as *const () as usize)),
            move |_, prev, _| {
                if prev.is_some() && checked.with_untracked(|c| c.matching) {
                    update(&|q| q.checked.refiltered());
                }
            },
            false,
        );
    }

    let selected_rows = move || -> Selected<T> {
        let c = checked.get_untracked();
        let rows = rows.with_untracked(|r| {
            keys.with_untracked(|k| {
                if c.matching {
                    shown.with_untracked(|s| {
                        s.iter()
                            .map(|&i| i as usize)
                            .filter(|&i| !c.keys.contains(&k[i]))
                            .map(|i| r[i].clone())
                            .collect()
                    })
                } else {
                    order.with_untracked(|o| {
                        o.iter()
                            .map(|&i| i as usize)
                            .filter(|&i| c.keys.contains(&k[i]))
                            .map(|i| r[i].clone())
                            .collect()
                    })
                }
            })
        });
        Selected {
            rows,
            matching: c.matching.then(|| filter.get_untracked()),
            left_out: if c.matching {
                c.keys.iter().cloned().collect()
            } else {
                Vec::new()
            },
        }
    };

    // Scrolling and the window of rendered rows.
    let scroller = NodeRef::<html::Div>::new();
    let scroll_top = RwSignal::new(0.0);
    let UseElementSizeReturn { height, .. } = use_element_size(scroller);
    let viewport = Memo::new(move |_| (height.get() - ROW_HEIGHT).max(ROW_HEIGHT));
    let window = Memo::new(move |_| {
        model::window(
            scroll_top.get(),
            viewport.get(),
            ROW_HEIGHT,
            count.get(),
            OVERSCAN,
        )
    });
    // The rendered rows: their place among the shown rows, and their place in `rows`.
    let visible = Memo::new(move |_| {
        let w = window.get();
        shown.with(|s| {
            s.get(w.clone())
                .unwrap_or_default()
                .iter()
                .zip(w)
                .map(|(&row, index)| (index, row))
                .collect::<Vec<_>>()
        })
    });
    let reveal = move |index: usize| {
        let Some(el) = scroller.get_untracked() else {
            return;
        };
        if let Some(top) = model::reveal(
            index,
            ROW_HEIGHT,
            el.scroll_top() as f64,
            viewport.get_untracked(),
        ) {
            el.set_scroll_top(top.ceil() as i32);
        }
    };
    let to_top = move || {
        if let Some(el) = scroller.get_untracked() {
            el.set_scroll_top(0);
        }
        scroll_top.set(0.0);
    };

    let cursor = RwSignal::new(None::<usize>);
    let anchor = RwSignal::new(None::<usize>);
    Effect::watch(
        move || shown.track(),
        move |_, _, _| {
            cursor.set(None);
            anchor.set(None);
        },
        false,
    );
    // A link to an open row: put the cursor on it and scroll to it, once.
    let found = StoredValue::new(false);
    Effect::new(move || {
        if found.get_value() || count.get() == 0 {
            return;
        }
        found.set_value(true);
        let Some(key) = open_key.get_untracked() else {
            return;
        };
        let at = shown.with_untracked(|s| {
            keys.with_untracked(|k| s.iter().position(|&i| k[i as usize] == key))
        });
        if let Some(at) = at {
            cursor.set(Some(at));
            reveal(at);
        }
    });

    let key_at = move |index: usize| -> Option<String> {
        let row = shown.with_untracked(|s| s.get(index).copied())?;
        keys.with_untracked(|k| k.get(row as usize).cloned())
    };
    let open = move |index: usize| {
        if let Some(key) = key_at(index) {
            cursor.set(Some(index));
            update(&|q| q.selected = Some(key.clone()));
        }
    };
    let toggle = move |index: usize, range: bool| {
        let Some(key) = key_at(index) else { return };
        let on = !checked.with_untracked(|c| c.contains(&key));
        let from = anchor.get_untracked().filter(|_| range);
        let span: Vec<String> = match from {
            Some(from) => model::span(from, index).filter_map(key_at).collect(),
            None => vec![key],
        };
        anchor.set(Some(index));
        update(&|q| q.checked.set(span.iter().map(String::as_str), on));
    };

    let grid = NodeRef::<html::Div>::new();
    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.target() != ev.current_target() || ev.alt_key() || ev.ctrl_key() || ev.meta_key() {
            return;
        }
        let key = ev.key();
        if let Some(step) = Step::of(&key) {
            let page = (viewport.get_untracked() / ROW_HEIGHT) as usize;
            if let Some(next) = model::step(
                cursor.get_untracked(),
                step,
                count.get_untracked(),
                page.saturating_sub(1).max(1),
            ) {
                ev.prevent_default();
                cursor.set(Some(next));
                reveal(next);
            }
            return;
        }
        match key.as_str() {
            " " => {
                ev.prevent_default();
                if let Some(at) = cursor.get_untracked() {
                    toggle(at, ev.shift_key());
                }
            }
            "Enter" => {
                if let Some(at) = cursor.get_untracked() {
                    ev.prevent_default();
                    open(at);
                }
            }
            "Escape" if open_key.get_untracked().is_some() => {
                update(&|q| q.selected = None);
            }
            _ => {}
        }
    };

    let columns: Vec<(T::Col, Column)> = T::COLUMNS.iter().map(|&c| (c, T::column(c))).collect();
    let widths: Vec<Width> = columns.iter().map(|(_, c)| c.width).collect();
    let action_slots = row_actions.map_or(0, |_| quick.clamp(1, 2) + 1);
    let (template, min) = model::template(CHECK_REM, &widths, ACTION_REM * action_slots as f32);
    let grid_style = format!("--list-columns: {template}; --list-min: {min}rem");
    let quick = quick.clamp(1, 2);

    let sort_header = move |col: Column| {
        let state = move || {
            query.with(|q| match &q.sort {
                Some(s) if s.column == col.id => Some(s.descending),
                _ => None,
            })
        };
        let aria = move || match state() {
            Some(false) => "ascending",
            Some(true) => "descending",
            None => "none",
        };
        let mark = move || match state() {
            Some(false) => "sort-mark sort-asc",
            Some(true) => "sort-mark sort-desc",
            None => "sort-mark",
        };
        let head = if col.sortable {
            view! {
                <button
                    type="button"
                    class="sort"
                    on:click=move |_| {
                        update(&|q| *q = q.clone().sorted_by(col.id));
                    }
                >
                    {col.header}
                    <span class=mark aria-hidden="true"></span>
                </button>
            }
            .into_any()
        } else {
            view! { <span class="label">{col.header}</span> }.into_any()
        };
        let head = match col.hint {
            Some(hint) => view! {
                <Tooltip label=hint focusable=!col.sortable>
                    {head}
                </Tooltip>
            }
            .into_any(),
            None => head,
        };
        view! {
            <div
                role="columnheader"
                class="list-cell"
                class:numeric=col.numeric
                aria-sort=move || col.sortable.then(aria)
            >
                {head}
            </div>
        }
    };

    let label = StoredValue::new(label);
    let head_label = move || {
        let n = count.get();
        if filter.with(|f| f.trim().is_empty()) {
            format!("Select all {n}")
        } else {
            format!("Select all {n} matching the filter")
        }
    };
    let head_checkbox = view! {
        <div role="columnheader" class="list-check">
            <input
                type="checkbox"
                aria-label=head_label
                prop:checked=move || { header.get() == Checked::All }
                prop:indeterminate=move || { header.get() == Checked::Some }
                disabled=move || { count.get() == 0 }
                on:click=move |_| {
                    if header.get_untracked() == Checked::None {
                        update(&|q| q.checked.select_matching());
                    } else {
                        update(&|q| q.checked.clear());
                    }
                }
            />
        </div>
    };

    let header_cells = columns
        .iter()
        .map(|(_, col)| sort_header(*col))
        .collect_view();
    let row_view = move |(_, key, row): (u64, String, u32)| {
        let item = rows.with_untracked(|r| r[row as usize].clone());
        let title = item.title();
        let index = Memo::new({
            let key = key.clone();
            move |_| {
                visible.with(|v| {
                    keys.with_untracked(|k| {
                        v.iter()
                            .find(|(_, r)| k[*r as usize] == key)
                            .map(|(i, _)| *i)
                    })
                })
            }
        });
        let is_checked = Memo::new({
            let key = key.clone();
            move |_| checked.with(|c| c.contains(&key))
        });
        let is_open = Memo::new({
            let key = key.clone();
            move |_| open_key.with(|k| k.as_deref() == Some(key.as_str()))
        });
        let at_cursor = move || index.get().is_some() && cursor.get() == index.get();
        let on_click = move |ev: web_sys::MouseEvent| {
            let inner = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|t| t.closest("a, button, input, select, label").ok().flatten());
            if inner.is_some() {
                return;
            }
            if let Some(i) = index.get_untracked() {
                open(i);
                if let Some(g) = grid.get_untracked() {
                    let _ = g.focus();
                }
            }
        };
        let cells = columns
            .iter()
            .map(|(c, col)| {
                view! {
                    <div role="gridcell" class="list-cell" class:numeric=col.numeric>
                        {item.cell(*c)}
                    </div>
                }
            })
            .collect_view();
        let actions = row_actions.map(|ra| {
            let all = ra.run(item.clone());
            let (icons, rest) = all.split_at(all.len().min(quick));
            let icons = icons
                .iter()
                .cloned()
                .map(|a| quick_icon(a, go))
                .collect_view();
            let rest = rest.to_vec();
            let more = (!rest.is_empty()).then(|| {
                let rest = StoredValue::new(rest);
                view! {
                    <Menu glyph=Glyph::Ellipsis label=format!("More actions for {title}")>
                        {rest
                            .get_value()
                            .into_iter()
                            .map(|a| {
                                let RowAction { glyph, label, act } = a;
                                view! {
                                    <MenuItem
                                        label
                                        glyph
                                        on_select=move |()| run_act(&act, go)
                                    />
                                }
                            })
                            .collect_view()}
                    </Menu>
                }
            });
            view! { <div role="gridcell" class="list-actions">{icons}{more}</div> }
        });
        view! {
            <div
                role="row"
                class="list-row"
                id=move || index.get().map(|i| format!("{id}-r{i}"))
                aria-rowindex=move || index.get().map(|i| i + 2)
                aria-selected=move || is_checked.get().to_string()
                aria-current=move || is_open.get().then_some("true")
                class:checked=is_checked
                class:open=is_open
                class:cursor=at_cursor
                on:click=on_click
            >
                <div role="gridcell" class="list-check">
                    <input
                        type="checkbox"
                        tabindex="-1"
                        aria-label=format!("Select {title}")
                        prop:checked=is_checked
                        on:click=move |ev: web_sys::MouseEvent| {
                            if let Some(i) = index.get_untracked() {
                                toggle(i, ev.shift_key());
                            }
                        }
                    />
                </div>
                {cells}
                {actions}
            </div>
        }
    };

    let actions_head = (action_slots > 0).then(|| {
        view! {
            <div role="columnheader" class="list-actions">
                <span class="visually-hidden">"Actions"</span>
            </div>
        }
    });

    let toolbar = toolbar(
        label,
        actions,
        count,
        total,
        checked_count,
        filter,
        placeholder,
        filters,
        selected_rows,
        update,
        to_top,
    );

    let narrow = use_media_query(NARROW);
    let open_pos = Memo::new(move |_| {
        open_key.with(|key| {
            let key = key.as_ref()?;
            keys.with(|k| k.iter().position(|x| x == key))
        })
    });
    let close_href = Signal::derive(move || {
        let mut q = query.get();
        q.selected = None;
        let pairs = query_map.with(|m| q.merged(m.latest_values().map(|(k, v)| (k.as_ref(), v))));
        format!("{}{}", location.pathname.get(), query_string(&pairs))
    });
    let open_title = Signal::derive(move || {
        open_pos
            .get()
            .map(|p| rows.with(|r| r[p].title()))
            .unwrap_or_default()
    });
    let body = move |p: usize| {
        let item = rows.with_untracked(|r| r[p].clone());
        let acts = row_actions
            .map(|ra| ra.run(item.clone()))
            .unwrap_or_default();
        let href = item.href();
        view! {
            <div class="detail-actions">
                {href
                    .map(|href| {
                        view! {
                            <A href=href attr:class="button primary">
                                <LabelledIcon glyph=Glyph::ExternalLink label="Open" />
                            </A>
                        }
                    })}
                {acts.into_iter().map(|a| labelled_act(a, go)).collect_view()}
            </div>
            {detail.run(item)}
        }
    };
    let drawer_open = RwSignal::new(false);
    Effect::new(move || drawer_open.set(narrow.get() && open_pos.get().is_some()));
    Effect::watch(
        move || drawer_open.get(),
        move |open, prev, _| {
            if !*open
                && prev == Some(&true)
                && narrow.get_untracked()
                && open_key.with_untracked(Option::is_some)
            {
                update(&|q| q.selected = None);
            }
        },
        false,
    );

    let no_match = move || {
        if total.get() == 0 {
            empty.clone()
        } else {
            "Nothing matches this filter.".to_string()
        }
    };

    view! {
        <div class="split list-split">
            <div class="split-list">
                {toolbar}
                <div
                    class="list-scroll"
                    node_ref=scroller
                    style=grid_style
                    on:scroll=move |_| {
                        if let Some(el) = scroller.get_untracked() {
                            scroll_top.set(el.scroll_top() as f64);
                        }
                    }
                >
                    <div
                        role="grid"
                        class="list-grid"
                        tabindex="0"
                        node_ref=grid
                        aria-label=move || label.get_value()
                        aria-rowcount=move || { count.get() + 1 }
                        aria-multiselectable="true"
                        aria-activedescendant=move || cursor.get().map(|i| format!("{id}-r{i}"))
                        on:keydown=on_keydown
                    >
                        <div role="row" class="list-head" aria-rowindex="1">
                            {head_checkbox}
                            {header_cells}
                            {actions_head}
                        </div>
                        <div
                            class="list-body"
                            role="rowgroup"
                            style:height=move || format!("{}px", count.get() as f64 * ROW_HEIGHT)
                        >
                            <div
                                class="list-window"
                                style:transform=move || {
                                    format!("translateY({}px)", window.get().start as f64 * ROW_HEIGHT)
                                }
                            >
                                <For
                                    each=move || {
                                        let g = generation.get();
                                        keys.with(|k| {
                                            visible
                                                .get()
                                                .into_iter()
                                                .map(|(_, row)| (g, k[row as usize].clone(), row))
                                                .collect::<Vec<_>>()
                                        })
                                    }
                                    key=|(g, key, _)| (*g, key.clone())
                                    children=row_view
                                />
                            </div>
                        </div>
                    </div>
                    <Show when=move || { count.get() == 0 }>
                        <p class="empty">{no_match.clone()}</p>
                    </Show>
                </div>
                {legend.map(|l| l.run())}
            </div>
            {move || {
                if narrow.get() {
                    return None;
                }
                open_pos
                    .get()
                    .map(|p| {
                        view! {
                            <DetailPane title=open_title close=close_href>
                                {body(p)}
                            </DetailPane>
                        }
                    })
            }}
        </div>
        <Drawer open=drawer_open title=open_title>
            {move || open_pos.get().map(body)}
        </Drawer>
    }
}

fn run_act(act: &Act, go: Callback<String>) {
    match act {
        Act::Go(href) => go.run(href.clone()),
        Act::Run(run) => run.run(()),
    }
}

/// A row action as an icon at the row's end.
fn quick_icon(action: RowAction, go: Callback<String>) -> AnyView {
    let RowAction { glyph, label, act } = action;
    match act {
        Act::Go(href) => view! {
            <Tooltip label=label.clone()>
                <A href=href attr:class="icon-button" attr:aria-label=label>
                    <Svg glyph />
                </A>
            </Tooltip>
        }
        .into_any(),
        Act::Run(run) => {
            let _ = go;
            view! { <IconButton glyph label on_click=run /> }.into_any()
        }
    }
}

/// A row action as a labelled button, in the detail.
fn labelled_act(action: RowAction, go: Callback<String>) -> AnyView {
    let RowAction { glyph, label, act } = action;
    match act {
        Act::Go(href) => view! {
            <A href=href attr:class="button">
                <LabelledIcon glyph label />
            </A>
        }
        .into_any(),
        Act::Run(run) => {
            let _ = go;
            view! {
                <button type="button" on:click=move |_| run.run(())>
                    <LabelledIcon glyph label />
                </button>
            }
            .into_any()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn toolbar<T: ListRow>(
    label: StoredValue<String>,
    actions: Vec<ToolAction<T>>,
    count: Memo<usize>,
    total: Memo<usize>,
    checked_count: Memo<usize>,
    filter: Memo<String>,
    placeholder: Option<String>,
    filters: Option<ViewFn>,
    selected_rows: impl Fn() -> Selected<T> + Copy + Send + Sync + 'static,
    update: impl Fn(&dyn Fn(&mut ListQuery)) + Copy + Send + Sync + 'static,
    to_top: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let n = actions.len();
    let actions = StoredValue::new(actions);
    let refs: Vec<NodeRef<html::Button>> = (0..n).map(|_| NodeRef::new()).collect();
    let refs = StoredValue::new(refs);
    let room = NodeRef::<html::Div>::new();
    let UseElementSizeReturn { width, .. } = use_element_size(room);
    let measured = StoredValue::new(Vec::<f64>::new());
    let fits = RwSignal::new(n);
    Effect::new(move || {
        let room = width.get();
        if room <= 0.0 {
            return;
        }
        if measured.with_value(|m| m.len() != n || m.iter().any(|w| *w <= 0.0)) {
            let widths: Vec<f64> = refs.with_value(|r| {
                r.iter()
                    .map(|b| b.get_untracked().map_or(0.0, |b| b.offset_width() as f64))
                    .collect()
            });
            measured.set_value(widths);
        }
        fits.set(measured.with_value(|m| model::fit(m, TOOLBAR_GAP, room, MORE_WIDTH)));
    });
    let none_checked = move || checked_count.get() == 0;
    let run = move |i: usize| {
        let Some(action) = actions.with_value(|a| a.get(i).cloned()) else {
            return;
        };
        match action.run {
            ToolRun::Page(run) => run.run(()),
            ToolRun::Checked(run) => run.run(selected_rows()),
        }
    };
    let buttons = (0..n)
        .map(|i| {
            let (glyph, text, needs) = actions.with_value(|a| {
                (
                    a[i].glyph,
                    a[i].label.clone(),
                    matches!(a[i].run, ToolRun::Checked(_)),
                )
            });
            let node = refs.with_value(|r| r[i]);
            view! {
                <button
                    type="button"
                    class="tool"
                    node_ref=node
                    hidden=move || { i >= fits.get() }
                    disabled=move || { needs && none_checked() }
                    on:click=move |_| run(i)
                >
                    <LabelledIcon glyph label=text />
                </button>
            }
        })
        .collect_view();
    let more = move || {
        (fits.get() < n).then(|| {
            view! {
                <Menu glyph=Glyph::Ellipsis label="More actions">
                    {move || {
                        (fits.get_untracked()..n)
                            .map(|i| {
                                let (glyph, text, needs) = actions
                                    .with_value(|a| {
                                        (
                                            a[i].glyph,
                                            a[i].label.clone(),
                                            matches!(a[i].run, ToolRun::Checked(_)),
                                        )
                                    });
                                view! {
                                    <MenuItem
                                        label=text
                                        glyph
                                        disabled=Signal::derive(move || needs && none_checked())
                                        on_select=move |()| run(i)
                                    />
                                }
                            })
                            .collect_view()
                    }}
                </Menu>
            }
        })
    };
    let matching_offer = move || {
        let n = count.get();
        let c = checked_count.get();
        (c > 0 && c < n).then(|| {
            let text = if filter.with(|f| f.trim().is_empty()) {
                format!("Select all {n}")
            } else {
                format!("Select all {n} matching")
            };
            view! {
                <button
                    type="button"
                    class="link-button"
                    on:click=move |_| update(&|q| q.checked.select_matching())
                >
                    {text}
                </button>
            }
        })
    };
    let placeholder = placeholder.unwrap_or_else(|| "Filter".to_string());
    view! {
        <div class="list-toolbar">
            <div
                class="toolbar-actions"
                role="toolbar"
                aria-label=move || format!("{} actions", label.get_value())
                node_ref=room
            >
                {buttons}
                {more}
            </div>
            <Show when=move || { checked_count.get() > 0 }>
                <p class="list-checked" role="status">
                    <span>{move || format!("{} selected", checked_count.get())}</span>
                    {matching_offer}
                    <IconButton
                        glyph=Glyph::X
                        label="Clear the selection"
                        on_click=move |()| update(&|q| q.checked.clear())
                    />
                </p>
            </Show>
            {filters.map(|f| f.run())}
            <label class="list-filter">
                <Svg glyph=Glyph::Search />
                <span class="visually-hidden">{move || format!("Filter {}", label.get_value())}</span>
                <input
                    type="search"
                    class="filter-text"
                    placeholder=placeholder
                    autocomplete="off"
                    prop:value=move || filter.get()
                    on:input=move |ev| {
                        let text = event_target_value(&ev);
                        to_top();
                        update(&|q| {
                            q.filter = text.clone();
                            q.checked.refiltered();
                        });
                    }
                />
            </label>
            <p
                class="shown"
                aria-live="polite"
                data-query=move || filter.get()
                data-shown=move || count.get()
            >
                {move || format!("{} of {}", count.get(), total.get())}
            </p>
        </div>
    }
}
