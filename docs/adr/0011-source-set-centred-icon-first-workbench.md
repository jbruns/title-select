# 11. The workbench centres on the Source Set, with master–detail lists, icon-first status and bulk-first plans

Date: 2026-11-02

## Status

Accepted

## Context

The pilot proved the engine, but its workbench was hard to use. It had eight peer pages in the top navigation, and a
Source Set's page linked to its Repair but not to its Library Folders. Plans explained every item in prose, and a
choice could only be changed one item at a time: leaving 128 Unmatched Entries took 128 dropdown changes (#59). Slow
requests only disabled a button (#65, #61). ROM managers such as ClrMamePro lean on dense jargon and on modes that
can disagree without saying so. Media servers centre on metadata that Title Select doesn't keep. Organizer services
such as Sonarr come closest: they centre on one object, keep a toolbar of icons and short labels at the top of each
page, edit in bulk with "no change" as the default, and keep a legend strip under each list.

## Decision

- **The Source Set is the hub.** Audit, Repair, Additions, its Library Folders and their history hang off it. Top
  navigation is Source Sets · Libraries · Ingest · DATs · Profiles · Activity · Settings, with badges for waiting
  work. A Library Folder links back to its Source Set, and the Source Set links forward to its Library Folders.
- **One list pattern.** A checkbox column selects rows, and a toolbar at the top acts on the selection. Clicking a
  row opens it in a detail pane beside the list, which stacks as a drawer on narrow screens. One or two frequent
  actions also appear as icons at the end of the row. Plan items use the same pattern as entity lists.
- **Icon-first status.** Each status is one of a small closed set and is shown with both colour and shape. Every
  icon has a tooltip and an accessible label, and each list has a collapsible legend strip. Prose is used only where
  nothing else conveys the meaning clearly. Per-item reasons appear in the detail pane.
- **Bulk-first plans.** A plan opens as counts by kind, and each count carries an "apply to all" choice whose
  default is "no change". A choice can also be applied to the selected rows or to every row matching a filter.
  Changing a choice by hand still works, and approval still binds the plan as displayed.
- **Leptos is used fully.** Nested routes carry tabs and the detail pane, and URL query state carries filters,
  sorting and the selected row. A shared `Action`-based component shows pending work and problems, `<Portal>` hosts
  overlays and `reactive_stores` holds plan state. Large tables use keyed rows and virtualization. This extends
  ADR 0005; the UI still uses only the versioned JSON API.

## Considered options

- **Task-centred pages** (Audits, Repairs, Builds, each listing every Source Set). Rejected: the Operator's work
  follows one Source Set from Audit to Repair to Build, and task-centred pages split it across the app.
- **A ClrMamePro- or RomVault-style tree with colour-coded nodes.** Rejected: colour alone isn't accessible, and a
  tree hides counts and bulk decisions.
- **A poster grid in the style of a media server.** Rejected: Title Select isn't driven by metadata.

## Consequences

- Managed Folder becomes Library Folder and Operator Addition becomes Addition everywhere: UI, API paths and types,
  store tables and code identifiers. The JSON API changes within `/api/v1`, and the CLI changes with it. During the
  pilot `/api/v1` has no outside clients, so it may break freely.
- Every slow request shows that it's in progress through one shared component, not a disabled button.
- UI copy rules live in `docs/agents/ui.md`.
