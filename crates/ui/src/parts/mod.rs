//! The parts every screen builds on. `docs/agents/ui.md` says when to use each.
//!
//! - `icon`: `Icon`, `IconButton` and `LabelledIcon` over the vendored Lucide glyphs in `lucide`.
//! - `status`: the status vocabulary, and `StatusIcon` and `StatusBadge` to show a status.
//! - `legend`: the `Legend` strip under a list.
//! - `list`: `List`, the toolbar, checkable rows, quick actions and detail every list screen uses, over `list_model`.
//! - `plan`: `PlanReview`, the plan review's counts by kind, rows reading current → proposed and choices set in bulk,
//!   over `plan_model`.
//! - `pending`: `ActionPending` and `Pending` for slow requests and their problems.
//! - `overlay`: `Tooltip`, `Menu` and `Drawer`, hosted in a `<Portal>`, stacked by `layers`.
//! - `theme`: the light and dark themes and `ThemeToggle`.
//! - `url_state`: a list's filter, sort, selected row and tab in the query string.
//! - `layout`: `SplitView`, `DetailPane` and `Tabs` over nested routes.

pub mod icon;
pub mod layers;
pub mod layout;
pub mod legend;
pub mod list;
pub mod list_model;
pub mod lucide;
pub mod overlay;
pub mod pending;
pub mod plan;
pub mod plan_model;
pub mod status;
pub mod theme;
pub mod url_state;
