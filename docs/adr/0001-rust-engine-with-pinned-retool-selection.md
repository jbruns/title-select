# 1. Rust engine, with pinned Retool for Selection

Date: 2026-10-28

## Status

Superseded by ADR 0015 for Selection: the Rust port replaced pinned Retool at the cutover (#95). The Rust engine
for Audits, jobs and the service stands.

## Context

Title Select needs fast, explainable Audits of large Source Sets, durable jobs and a service. It also needs a 1G1R
Selection that explains each choice. Existing tools leave gaps: igir has no DAT Revision store, no Change Plans, no
bad, conflicting or unreadable Findings and no jobs; ClrMamePro and RomVault use region-priority parent/clone
selection with no rule trace. Retool's filter-pipeline Selection is the best available, but it is unmaintained Python
of about 7,900 lines of order-sensitive selection code with no written specification.

## Decision

The Audit engine, job engine and service are Rust. Selection runs a pinned Retool commit inside the image, patched to
emit a structured trace that names the rule responsible for each decision. A Rust port of Retool is a later, separately
approved option, gated on a conformance suite that uses the patched Retool as its oracle.

## Consequences

- The image carries a Python runtime for Selection.
- Title Select maintains a small patch set against unmaintained upstream code, and shows clone-list and metadata drift
  against each DAT Revision instead of hiding it.
- The pristine upstream source, without its GUI, is vendored at its pinned commit beside an ordered set of patches,
  applied at build time. The patches add a JSON Lines trace covering every group, write no DAT that Title Select reads,
  and make no network call. A test shows they leave Retool's selections unchanged. The vendored tree goes away once the
  Rust port lands.
- Clone lists and metadata are not vendored. The image build fetches them at a pinned upstream commit, checked by
  sha256. Renovate proposes a new commit when upstream moves, and the conformance tests gate it. This mirrors
  Retool's own updates while keeping every Selection reproducible. Revisit if upstream maintenance changes.
- The image's base becomes distroless Python, with Retool's few runtime dependencies hash-locked and the GUI's left out.
  Tests and CI build the same patched Retool and environment with one setup script.
