# Retool

Title Select's Selection engine (`crates/selection/src/native/`) is a port of
[Retool](https://github.com/unexpectedpanda/retool) at commit `59f169f530580448c055c781cb3166910c7bdba5`, so
Retool's licence, `LICENSE`, stays beside it.

`fixtures/` holds the test inputs of that commit that the fixture tests (`crates/selection/tests/fixtures.rs`)
read, unchanged:

- `source/`, `configs/`, `clonelists/`, `metadata/`, `mias/` and `retroachievements/` are Retool's `tests/`
  directories of the same names, published under `LICENSE`.
- `config/internal-config.json` is the internal config from the clone lists and metadata at commit
  `cdeb391e2ea422475dc82091278c9c9ae3f340ae` of
  [retool-clonelists-metadata](https://github.com/unexpectedpanda/retool-clonelists-metadata), frozen here so
  that a clone-list bump doesn't change the fixture goldens. It is published under `config/LICENSE`.

Nothing else of Retool is vendored. The goldens the fixture tests compare against
(`crates/selection/tests/goldens/fixtures/`) were made by the built-in engine while a conformance harness, running
Retool beside it on every case, found no difference other than ones that follow Python's set iteration order. The
goldens mark the one case Retool couldn't finish.
