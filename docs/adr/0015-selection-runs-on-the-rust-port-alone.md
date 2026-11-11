# 15. Selection runs on the Rust port alone

Date: 2026-11-10

## Status

Accepted. Supersedes ADR 0001's pinned Retool for Selection; its Rust engine for everything else stands.

## Context

ADR 0001 ran Selection on a pinned, patched Retool in Python, and allowed a Rust port gated on a conformance suite
with that Retool as its oracle. The port (#94) shipped beside it. Over Retool's fixture DATs and test configs, both
engines agreed on every case apart from differences the oracle proved come only from the order Python meets titles
in; on the Host, shadow mode found no difference on three real systems.

## Decision

- The Rust port is the only Selection engine. The vendored Retool, its patches, its Python environment, the
  conformance command and shadow mode are removed, and the image carries no Python.
- The port's output on Retool's fixture DATs and test configs, made while the oracle passed, is frozen as committed
  text goldens, and those goldens are the regression oracle from now on. Retool's test inputs stay vendored, with its
  BSD-3-Clause licence, because the goldens need them; everything else of Retool goes.
- The port still reads Retool's clone lists and metadata at a pinned commit, checked by sha256. A POSIX shell script
  fetches them for the image, the tests and CI, and writes the sha256 for a new pin; Renovate still proposes pins.
- A Selection records the Title Select version and the clone-list commit it was made with. Selections made before
  the cutover keep the Retool commit and patch-set digest they recorded.
- `[selections] engine`, `shadow` and `shadow_result_mib` are accepted and ignored, with a warning, for one release.

## Consequences

- A goldens change is a Selection change: it is reviewed as one, and regenerated deliberately.
- Retool's later fixes no longer arrive by bumping a pin; porting one is ordinary engine work, checked against the
  goldens. The clone lists and metadata still follow upstream.
- The About page still acknowledges Retool and its licence, since the engine is a derivative of it.
