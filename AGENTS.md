## Agent skills

Before repository work, run `tools/agent-skills/setup-agent-skills check`. If it fails,
run `tools/agent-skills/setup-agent-skills`, then start a fresh agent session to discover
the pinned skills. Invoke applicable installed skills before proceeding.
For installation, pin updates, or recovery, read `docs/agents/skills.md`.

### Issue tracker

Issues and specs live in this repo's GitHub Issues. Before reading or publishing tickets, read `docs/agents/issue-tracker.md`.

### Triage labels

Use the five default triage labels. Before triaging issues, read `docs/agents/triage-labels.md`.

### Domain docs

Single-context: root `GLOSSARY.md` and `docs/adr/`. Before exploring the codebase, read `docs/agents/domain.md`.

### Build and test

The toolchain is pinned in `rust-toolchain.toml`. CI runs exactly these, so run them before committing:

```sh
tools/retool/setup   # Retool for the Selection tests; needs uv, which fetches Python 3.11
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests build their DATs and Source Sets from synthetic fixtures at test time. Never commit ROMs, and never commit a
real DAT unless its licence permits redistribution.

### Testing against live data

Test against local copies and synthetic fixtures. You may read the Host, but touch live data on it only at a milestone
acceptance agreed with the Operator. The next one is the end of M2.

### Read-only by default

An Audit never writes to the Source Set it reads (ADR 0002). Only an approved Change Plan may change a Source Set
or a Library.

### User-facing text

User-facing text is everything the Operator reads: the workbench, CLI help and output, error messages, and the
README's Operator sections (configuration, Service, headless use, the workbench). In it:

- Address the reader as "you" and "your" ("Your choice", "You chose…"), never "the Operator" or "Operator choice".
- Cite no ADRs.
- Write glossary terms in lowercase, like ordinary words: "source set", "DAT revision", "audit", "selection profile",
  "play library". Proper nouns and acronyms (DAT, ZIP, SHA-256) keep their case.
- Never call a feature, button or setting "Retool": say "selection", "defaults", "reset to defaults". The workbench's
  About page is the one place that names Retool, to acknowledge where selection comes from.
- Follow the Voice section of `docs/brand-kit/title-select-style-guide.md`: name the action, explain the result, make
  uncertainty visible.

Machine-readable output (JSON fields and values, API paths, enum names, report schemas) is not text: it doesn't change
with these rules.

Non-user-facing text keeps the role "the Operator", capitalized glossary terms and ADR citations: code comments,
docstrings that aren't CLI help, ADRs, `GLOSSARY.md`, design and research docs, issues and PRs, and the README's
development and architecture sections.

### Workbench UI

Before changing a workbench screen or `crates/ui/src/parts/`, read `docs/agents/ui.md`: copy rules and the shared
parts (icons, status vocabulary, legend, pending, overlays, URL state, theme).

### Privacy

All prose, user-facing or not, names roles, never people, logins, hostnames, domains or the deployments that run
this application. Non-user-facing prose says "the Operator", "the Host", `<domain>`; user-facing text says "you".
Load-bearing identifiers (paths, config keys, `uses:` refs) stay. Refer to this repo's issues as bare `#N`.
