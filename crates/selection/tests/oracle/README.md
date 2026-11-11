# Oracle answers

What the bundled Retool (commit `59f169f530580448c055c781cb3166910c7bdba5`, run by its own Python) answered for
generated inputs, frozen when the bundled engine was removed (ADR 0015). The differential tests check the built-in
engine against them. They are Retool's answers, not the engine's, so nothing regenerates them.

- `titletools.jsonl`: `TitleTools` on 3000 generated names under the default Profile
  (`native::tests::titletools_match_the_bundled_engine`).
- `user-patterns.jsonl`, `generated-patterns.jsonl`: whether Python's `re` compiles each pattern with `re.I`, and
  which subjects it finds (`native::userre::tests`).
- `disc-names.jsonl`: `TitleTools.normalize_discs` on disc names built from the frozen internal config's
  `discRename` table (`tests/discs.rs`).
