# Workbench UI

The workbench (`crates/ui`) is icon-first: lists of rows, a detail pane, one legend per list. This page holds the
copy rules and the shared parts in `crates/ui/src/parts/` that every screen builds on. The User-facing text rules in
`AGENTS.md` apply to every string below.

## Copy rules

- **A table cell is terse.** It holds an icon, a number, a name, or a status of three words or fewer. Never a reason.
- **Reasons live in the detail pane.** Why a row has its status, what a plan item will do, what went wrong.
- **Prose is the last resort.** Write it only where no icon, status or number conveys the meaning clearly. Longer
  help goes in a collapsible `<details>`.
- **Every icon is named.** It has a tooltip and an `aria-label`: use `Icon`, `IconButton`, `StatusIcon` or
  `StatusBadge`, which require the label. `LabelledIcon` is the one exception: its visible label names it.
- **Every tooltip is reachable from the keyboard.** A standalone `Icon` is a tab stop. Inside a link or button, draw
  the glyph with `Svg`, name the control, and wrap it in `Tooltip` so there is one tab stop, not two.
- **Destructive actions are labelled and confirmed.** A visible verb ("Set aside", "Delete plan"), never an icon
  alone, and a confirmation step before anything is moved or removed.
- **One legend strip per list.** It shows only the statuses present in that list.
- **Status labels** are sentence case, glossary terms lowercase ("Repair open", "Matches source set"). Hints are one
  sentence addressed to "you".

## Shared parts

All live under `crate::parts`. Pure logic in each module is host-tested (`cargo test -p title-select-ui`); keep new
pure logic there too.

| Need                                   | Use                                                    | Module      |
| -------------------------------------- | ------------------------------------------------------ | ----------- |
| An icon that means something           | `Icon { glyph, label }`                                | `icon`      |
| An icon-only button                    | `IconButton { glyph, label, on_click, disabled? }`     | `icon`      |
| An icon beside its visible text        | `LabelledIcon { glyph, label }`                        | `icon`      |
| A row's status in a cell               | `StatusIcon { status, tab_stop? }`                     | `status`    |
| A status with its words (detail pane)  | `StatusBadge { status }`                               | `status`    |
| The key to a list's statuses           | `Legend { statuses, list }` with `present(rows)`       | `legend`    |
| A slow request                         | `ActionPending { action, status }` around its content  | `pending`   |
| A slow load or any busy flag           | `Pending { pending, status, problem? }`                | `pending`   |
| A hover/focus hint on arbitrary content| `Tooltip { label, focusable?, clipped?, shown? }`      | `overlay`   |
| A row or toolbar menu                  | `Menu { glyph, label }` holding `MenuItem { disabled? }`| `overlay`  |
| A side panel over the page             | `Drawer { open, title }`                               | `overlay`   |
| Any list of rows                       | `List { id, label, rows, empty, detail, .. }`          | `list`      |
| Cell text that may not fit             | `Clip { text }`                                        | `list`      |
| Filter, sort, open/checked rows in URL | `use_list_query()` (`List` uses it already)            | `url_state` |
| List beside a routed detail            | `SplitView` + `DetailPane` under a `ParentRoute`       | `layout`    |
| Routed tabs                            | `Tabs { label, tabs: Vec<Tab> }`                       | `layout`    |
| A plan's items, reviewed in bulk       | `PlanReview { id, label, rows, empty, detail, .. }`    | `plan`      |
| Current → proposed in a cell           | `Change { current, proposed }`                         | `plan`      |
| Light/dark override                    | `ThemeToggle` (already in the header)                  | `theme`     |

### Icons

`Glyph` (`parts/lucide.rs`) is generated from vendored Lucide icons; never edit it by hand. To add one, append its
Lucide name to `ICONS` in `tools/lucide/vendor.py` and run `python3 tools/lucide/vendor.py`. Icons are inline SVG;
nothing is fetched at runtime. The licence ships at `/licences/lucide.txt` and is listed on the About page.

### Status vocabulary

`parts::status` maps every status to a `Look { glyph, tone, label, hint }`. Colour and shape both differ, so no state
is told apart by colour alone. Vocabularies: `FindingStatus`, `GameStatus`, `SourceSetStatus`, `LibraryStatus`,
`PlanKind` (repair plans, `From<RepairKind>`), `BuildKind` (build plans, builds and cleanups, `From<ItemKind>`),
`IngestStatus`, `JobState`, `DatFileStatus`.

- A row with several facts takes the highest-ranked status: build `SourceSetFacts` / `LibraryFacts` and call `of()`,
  or call `highest(iter)` for any vocabulary.
- To add a vocabulary, implement `Status` (`ALL` in precedence order, `look`). The tests in `status.rs` check label
  length, case, unique glyphs and labels; add the new type to them.
- In a cell that isn't a tab stop of its own (the shared list's rows, which the arrow keys move through), pass
  `tab_stop=false` to `StatusIcon` (or `Icon`); its label still names it.
- `Tone` is the colour: `Good`, `Review`, `Failed`, `Active`, `Neutral`, styled by `.tone-*` and `--tone-*`.

### Pending and problems

Write slow requests as `Action::new_local(|input| async { api::post(..).await })` returning `Result<_, String>`,
dispatch with `dispatch_local`, and wrap the content they refresh in `ActionPending`. While pending it shows a
spinner, the short status ("Making plan…") and the elapsed time, and keeps the previous content dimmed and inert. An
`Err` shows as a problem once the action settles. React to `Ok` with an `Effect` on `action.value()`; disable the
triggering button with `action.pending()`. `screens/repair.rs` (`RepairDesk`, propose and plan again) is the
reference: its action lives with the page, so the pending status outlives a change of tab.

`Pending` is the general form: give it any busy signal and optional problem. It also wraps its children in
`Transition` (busy while a resource reloads) and `ErrorBoundary`.

### Overlays

`Tooltip`, `Menu` and `Drawer` render through `Portal`, so they escape table and pane overflow. An overlay opens
above its anchor when only above has room, and lines up with the anchor's edge near the sides (`overlay::place`). Each open overlay is a layer on one stack (`parts::layers`): a
single window listener sends Escape to the topmost layer only. `Menu` supports arrow keys and closes on Tab. `Drawer`
is modal: Tab and Shift+Tab wrap over its tabbable elements only (`trap`, `tabbables`), and while any modal is open
the page's top-level elements are `inert`, except those holding the topmost modal and the layers above it. Each
element gets back its original inert state when the last modal closes; then focus returns to where it was. A new
overlay registers with `layers::layer` (or `open_layer`/`close_layer` when its cleanup order matters). `Tooltip { shown }`
shows it while a signal is true, kept within its anchor's width, for an anchor the keyboard reaches without focus.
A tooltip wraps anywhere, and is placed again for its drawn height, so a tall one that doesn't fit below goes above.

### Plan review

Every plan is reviewed with `parts::plan::PlanReview`, a `List` with a strip of counts by kind above it. Its rows
implement `PlanRow` (a `ListRow` with `type Kind: Status`): `PlanKind` for repairs, `BuildKind` for builds and
cleanups. Each kind's count is a filter (`kind` in the URL); pass `apply` to put an "apply to all" control beside each
count, which must start at "no change" and go back to it. Bulk choices are toolbar `ToolAction::checked` actions over
the checked rows or every row matching the filter. Count what a bulk choice did with `plan_model::apply_choice` and
show `BulkOutcome::message` in `message`, so a partial application says how many items don't offer the choice.
Rows show `Change { current, proposed }`; reasons and evidence go in `detail`. Keep the plan's own choice rules
pure and tested (`repair_model`). `screens/repair.rs` (`RepairTab`) is the reference.

### Lists

Every list screen uses `parts::list::List`; its pure logic (windowing, cursor steps, `Selection`, filter and sort,
toolbar fit, column template) is in `parts::list_model`. `screens/dats.rs` is the reference.

1. **A row type.** Implement `ListRow` for the row (`Clone + PartialEq + Send + Sync + 'static`; equality decides whether a
   reload redraws the open row's detail):
   - `type Col` (a `Copy + Eq` enum) and `const COLUMNS: &[Col]`, in display order.
   - `column(col) -> Column`: `Column::new(id, header, width)`, then `.sortable()`, `.numeric()` (right-aligned, data
     font) and `.hint(text)` (the header's tooltip). `id` names the column in the URL's `sort`. `width` is
     `Width::Fixed(rem)` or `Width::Flex { min: rem, share }` (`minmax(min, share fr)`). Give text columns a `Flex`
     with a `min`, and wrap their text in `Clip { text }`: it is cut off with an ellipsis and its tooltip shows the
     whole text only when cut off: on hover, and while the keyboard cursor is on its row (the grid keeps focus, so
     a cell's own focus never fires). The columns' minimums are the grid's minimum width; past it the list scrolls
     sideways, so keep the sum under about 50rem (the list's width at 1400px with a detail pane open).
   - `key()`: stable and unique, it names the row in the URL (`row`, `sel`) and keys its element.
   - `title()`: the detail's title and the accessible name of the row's checkbox and actions.
   - `search()`: the text the filter matches; it is lowercased once per change of `rows`.
   - `cell(col) -> AnyView`: terse (copy rules). A status cell is `StatusIcon { status, tab_stop: false }`.
   - `compare(other, col)` for sortable columns, and `href()` when the row has its own page (the detail links to it
     as "Open").
2. **The list.** `<List id label rows empty detail actions? row_actions? quick? placeholder? filters? keep? legend? />`:
   - `id: &'static str` prefixes element IDs; `label` names the grid and toolbar; `empty` shows when there are no
     rows at all.
   - `rows: Signal<Arc<Vec<T>>>`: every row, in their default order. A reload replaces the `Arc`. It must reflect
     successful loads only: rows missing from it are gone, so an empty list from a failed or pending load would
     close the open row. Build it with `last_good_rows(move || resource.get())`, which keeps the last rows loaded,
     and show the error separately (`Problem`).
   - `detail: Callback<T, AnyView>`: the open row's summary, under its actions in the `DetailPane` (or the drawer
     below 1100px). Its title is `title()`. It follows the row's data across reloads, and closes if the row leaves
     `rows`.
   - `actions: Vec<ToolAction<T>>`: toolbar buttons, icon and label. `ToolAction::page(glyph, label, || ..)` acts on
     the page; `ToolAction::checked(glyph, label, |chosen: Selected<T>| ..)` acts on the checked rows and is
     disabled while none are. `Selected { rows, matching, left_out }`: `rows` are the checked rows in list order;
     with every match checked, `matching` holds the filter and `left_out` the keys unchecked since, so a Service
     action can apply to the filter rather than to a list of keys. Buttons that don't fit go under a ⋯ menu.
   - `row_actions: Callback<T, Vec<RowAction>>` and `quick` (1 or 2, default 1): the first `quick` actions are
     icons at the row's end, shown on hover, on the keyboard cursor and on the open row; the rest are under the
     row's ⋯ menu. The detail shows them all, labelled. `RowAction::go(glyph, label, href)` or
     `RowAction::run(glyph, label, || ..)`. Labels are full ("Open its latest audit, #12").
   - `filters: ViewFn` adds controls beside the filter text; `keep: Signal<Keep<T>>` is the matching predicate
     (`Option<Arc<dyn Fn(&T) -> bool>>`), applied with the filter text. Changing it clears an "every match"
     selection.
   - `legend: ViewFn`: the strip under the list, usually `Legend { statuses: present(rows), list: id }`.
3. **What it does for you.** Filter text (`q`), sort (`sort`), the open row (`row`) and the checked rows (`sel`,
   `all`) live in the URL. Rows are keyed and virtualized: only the rows in view plus a few more are in the DOM, all
   40px tall, so a list of 100,000 filters within the ADR 0005 budget (`crates/ui/bench/filter.py --list`). The
   header checkbox checks every row matching the filter (stored as `all=1`, not as keys), even when some already
   are, and clears only when all are; changing the
   filter text clears such a selection but keeps rows checked one by one. The toolbar shows "N selected", "Select
   all N" and a clear button. Keyboard: the grid is one tab stop; arrows, Page Up/Down, Home and End move the
   cursor; Space checks (Shift+Space checks the range from the last one); Enter opens; Escape closes the open row.
   Clicking a row opens it, except on its links, buttons and checkbox. `p.shown` reads "N of M" and carries
   `data-query` and `data-shown` for the bench.

### URL state

`use_list_query()` returns the current `ListQuery { filter, sort, selected, tab, checked }` and a setter that
replaces the URL without scrolling. Keys: `q`, `sort` (`-name` for descending), `row` (the open row's key), `tab`,
`sel` (checked rows' keys, comma-separated, `\` escapes) and `all=1` (every row matching the filter is checked; `sel`
then lists the rows unchecked since). Defaults are omitted and other query parameters are kept. Use
`ListQuery::sorted_by(column)` for a header click. `checked` is a `list_model::Selection`.

### Layout and routes

Screens with a list and a detail use a `ParentRoute` whose view is the list wrapped in `SplitView`; the child route
renders a `DetailPane` into its `<Outlet/>`. Below 60rem the pane becomes a drawer. `List` needs no route of its
own: it renders the open row's `DetailPane` itself, as a `Drawer` below 1100px (`list::NARROW`). `/source-sets/:name` is already
a `ParentRoute` (bare `Outlet` view) for #74 to fill.

### Theme and colour

The theme follows `prefers-color-scheme`; `ThemeToggle` cycles system → light → dark and stores the override in
localStorage (`title-select.theme`), applied before load by `assets/boot.js`. Semantic colours in `style/main.css`
use `light-dark()`. A new text or tone colour must be added to both themes, and the contrast test in `theme.rs`
(≥ 4.5:1 on canvas, surface and selected rows) must pass.
