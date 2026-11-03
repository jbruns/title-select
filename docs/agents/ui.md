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
| A row's status in a cell               | `StatusIcon { status }`                                | `status`    |
| A status with its words (detail pane)  | `StatusBadge { status }`                               | `status`    |
| The key to a list's statuses           | `Legend { statuses, list }` with `present(rows)`       | `legend`    |
| A slow request                         | `ActionPending { action, status }` around its content  | `pending`   |
| A slow load or any busy flag           | `Pending { pending, status, problem? }`                | `pending`   |
| A hover/focus hint on arbitrary content| `Tooltip { label, focusable? }`                        | `overlay`   |
| A row or toolbar menu                  | `Menu { glyph, label }` holding `MenuItem`s            | `overlay`   |
| A side panel over the page             | `Drawer { open, title }`                               | `overlay`   |
| Filter, sort, selected row, tab in URL | `use_list_query()`                                     | `url_state` |
| List beside a routed detail            | `SplitView` + `DetailPane` under a `ParentRoute`       | `layout`    |
| Routed tabs                            | `Tabs { label, tabs: Vec<Tab> }`                       | `layout`    |
| Light/dark override                    | `ThemeToggle` (already in the header)                  | `theme`     |

### Icons

`Glyph` (`parts/lucide.rs`) is generated from vendored Lucide icons; never edit it by hand. To add one, append its
Lucide name to `ICONS` in `tools/lucide/vendor.py` and run `python3 tools/lucide/vendor.py`. Icons are inline SVG;
nothing is fetched at runtime. The licence ships at `/licences/lucide.txt` and is listed on the About page.

### Status vocabulary

`parts::status` maps every status to a `Look { glyph, tone, label, hint }`. Colour and shape both differ, so no state
is told apart by colour alone. Vocabularies: `FindingStatus`, `GameStatus`, `SourceSetStatus`, `LibraryStatus`,
`PlanKind` (repair plans, `From<RepairKind>`), `BuildKind` (build plans, builds and cleanups, `From<ItemKind>`),
`IngestStatus`, `JobState`.

- A row with several facts takes the highest-ranked status: build `SourceSetFacts` / `LibraryFacts` and call `of()`,
  or call `highest(iter)` for any vocabulary.
- To add a vocabulary, implement `Status` (`ALL` in precedence order, `look`). The tests in `status.rs` check label
  length, case, unique glyphs and labels; add the new type to them.
- `Tone` is the colour: `Good`, `Review`, `Failed`, `Active`, `Neutral`, styled by `.tone-*` and `--tone-*`.

### Pending and problems

Write slow requests as `Action::new_local(|input| async { api::post(..).await })` returning `Result<_, String>`,
dispatch with `dispatch_local`, and wrap the content they refresh in `ActionPending`. While pending it shows a
spinner, the short status ("Making plan…") and the elapsed time, and keeps the previous content dimmed and inert. An
`Err` shows as a problem once the action settles. React to `Ok` with an `Effect` on `action.value()`; disable the
triggering button with `action.pending()`. `screens/repair.rs` (propose repair) is the reference.

`Pending` is the general form: give it any busy signal and optional problem. It also wraps its children in
`Transition` (busy while a resource reloads) and `ErrorBoundary`.

### Overlays

`Tooltip`, `Menu` and `Drawer` render through `Portal`, so they escape table and pane overflow. An overlay opens
above its anchor when only above has room, and lines up with the anchor's edge near the sides (`overlay::place`). Each open overlay is a layer on one stack (`parts::layers`): a
single window listener sends Escape to the topmost layer only. `Menu` supports arrow keys and closes on Tab. `Drawer`
is modal: Tab and Shift+Tab wrap over its tabbable elements only (`trap`, `tabbables`), and while any modal is open
the page's top-level elements are `inert`, except those holding the topmost modal and the layers above it. Each
element gets back its original inert state when the last modal closes; then focus returns to where it was. A new
overlay registers with `layers::layer` (or `open_layer`/`close_layer` when its cleanup order matters).

### URL state

`use_list_query()` returns the current `ListQuery { filter, sort, selected, tab }` and a setter that replaces the
URL without scrolling. Keys: `q`, `sort` (`-name` for descending), `row`, `tab`; defaults are omitted and other query
parameters are kept. Use `ListQuery::sorted_by(column)` for a header click.

### Layout and routes

Screens with a list and a detail use a `ParentRoute` whose view is the list wrapped in `SplitView`; the child route
renders a `DetailPane` into its `<Outlet/>`. Below 60rem the pane becomes a drawer. `/source-sets/:name` is already
a `ParentRoute` (bare `Outlet` view) for #74 to fill.

### Theme and colour

The theme follows `prefers-color-scheme`; `ThemeToggle` cycles system → light → dark and stores the override in
localStorage (`title-select.theme`), applied before load by `assets/boot.js`. Semantic colours in `style/main.css`
use `light-dark()`. A new text or tone colour must be added to both themes, and the contrast test in `theme.rs`
(≥ 4.5:1 on canvas, surface and selected rows) must pass.
