# 14. Multi-disc Games are selected whole and laid out for ES-DE

Date: 2026-11-03

## Status

Accepted

## Context

Redump lists each disc of a release as its own Game. Retool puts `Title (USA) (Disc 1)` and `(Disc 2)` in one group
but chooses each disc by its own short name, so it can choose discs from different releases, for instance when only
Europe has Disc 2. The Rust port must keep Retool's selections exactly (ADR 0001), so changing that choice inside
the algorithm would break parity. Front ends such as ES-DE treat a folder whose name ends in a file extension as that
file, and an `.m3u` as one entry for several discs.

## Decision

- **Selected whole, by a check after Selection.** If the discs chosen for a group come from different releases, or
  the chosen release lacks one of its discs, the group is unresolved: its Build is blocked, unrelated groups still
  build, and the Operator settles it with an explicit choice. The selection algorithm itself is unchanged.
- **Laid out for ES-DE by default**, as a per-system Storage Policy setting of Play Libraries:
  - A disc Game stored as one file (`.iso`, `.chd`) goes directly in the system folder.
  - A single-disc Game stored as several files (`.cue` and its `.bin` tracks) goes in a folder named exactly for its
    `.cue`, `<system>/<game>.cue/`, holding the `.cue` and its tracks, so the front end points emulators at it.
  - A Multi-disc Game's files, for all of its discs and in whatever format, go flat in `<system>/.multidisc/`. An
    `.m3u`, named for the release without its disc tag, sits in the system folder and lists each disc's entry file
    (`.cue`, `.iso` or `.chd`) by relative path, in disc order.
  - SBI files go beside the `.cue` they belong to, when the system opts in.
- `.multidisc/`, `<game>.cue/` folders and generated `.m3u` files are part of the Library Folder: Drift checks cover
  them, and removals become Kept Files.

## Consequences

- Retool's disc-name normalization decides what counts as one release's discs, so the port carries it too.
- Other layouts, such as all discs flat in the system folder, can be added as further choices of the same setting.
