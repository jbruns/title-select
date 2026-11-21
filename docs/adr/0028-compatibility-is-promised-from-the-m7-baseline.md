# 28. Compatibility is promised from the M7 baseline, and nothing before it is carried

Date: 2026-11-20

## Status

Accepted

## Context

Before any public release, Title Select has accumulated 40 store migrations, some of which rewrite data or run Rust
code (0008, 0013, 0019, 0023, 0032), along with tests that build every old schema prefix. It also has redirects for
renamed workbench paths and a clamp for Ceilings set before they had an upper bound. Only the Operator's own deployment
has ever run it. Once others run it, every such accommodation becomes a promise. M7 is the point after which feature
work stops for a while in favour of bugs and UX, so it is the last cheap moment to drop them.

## Decision

- **M7 is a baseline, not 1.0.0.** Its release is the first whose formats are promised.
- **Nothing from before the baseline is carried.** The migrations are flattened into one `0001` that creates the
  current schema. The baseline store is stamped with a Title Select `PRAGMA application_id`. A store with
  `user_version > 0` and no stamp was made before the baseline: Title Select refuses to open it and tells the Operator
  to start a new store. There's no migration from it. Library Folders already on disk are taken back by adoption
  (adopting a file that holds exactly the Game's ROMs), not rebuilt.
- **Compatibility shims are removed**, on every surface: environment variables, CLI flags and subcommands, HTTP API
  routes and fields, workbench URL state and local storage, and persisted report and plan formats.
- **Machine formats get a version to keep.** The HTTP API moves under `/api/v1`. The CLI's JSON outputs carry a
  `schema` field.
- **From the baseline release on:**
  - Store migrations are append-only and are never flattened again.
  - An environment variable, CLI flag, API route or field, or JSON field is deprecated, with a warning, for at least
    one minor release before it's removed.
  - A breaking change to a machine format needs a new version of it: `/api/v2`, or a new `schema`.
  - Every release's notes list what it deprecates and what it removes.

## Considered Options

- **Adopting the existing store as the baseline**: flatten, but stamp a store at the last pre-baseline version as the
  baseline and refuse older ones. Rejected: it keeps a schema-equality test and an adoption path for the one store
  that would use it, and the Operator chose to start over.
- **Keeping the old chain in a frozen module**: rejected; it keeps exactly the code the pass exists to remove.

## Consequences

- At M7 acceptance the Operator starts a new store on the Host and re-registers Roots, Source Sets and Library
  Folders. Audits, Selection Profiles, plan history, settings and Calibrations from before are gone, and the Kept
  Files from before stay on disk but are no longer tracked.
- Consolidating duplicated code in M7 changes no behaviour: goldens and benchmarks stay as they are, and behaviour
  that has drifted between two paths gets its own issue.
