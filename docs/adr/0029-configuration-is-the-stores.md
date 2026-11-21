# 29. Configuration is the store's; only what bootstraps the Service lives outside it

Date: 2026-11-20

## Status

Accepted. Amends ADR 0008 and ADR 0022.

## Context

`title-select.toml` declares the Roots, auth, the Host's and each Root's Ceilings (where a value set there locks it in
the workbench, ADR 0022), the 7z archive limits and the path to chdman. Everything else is already a setting in the
store, edited in the workbench. The file is optional, but without it there are no Roots, so in practice the Operator
has to write it, deploy it next to the container and restart the Service for every change. Most Operators won't keep
a deployed file the way this project's own deployment does.

## Decision

- **Outside the store is only what's needed before the store opens, or what the workbench can't safely change.** That
  is the store path, the listen address, the workbench's files, the clone lists, chdman, and auth. Each is a flag with
  a matching `TITLE_SELECT_*` environment variable. Auth is `TITLE_SELECT_IDENTITY_HEADER`,
  `TITLE_SELECT_TRUSTED_PROXIES` (comma-separated CIDRs), `TITLE_SELECT_DEV_IDENTITY` and the existing
  `TITLE_SELECT_OPERATOR_TOKEN_FILE`, with the same refusals at startup as now: no way to authenticate, or a dev
  identity off loopback. There is no configuration file: `--config`, `TITLE_SELECT_CONFIG` and `title-select.toml`
  are removed.
- **Roots are registered in the workbench**, and changes take effect without a restart. Each change records who made it.
  - A Root's name is fixed once it's registered, because records refer to it.
  - Its kind is `general`, `ingest` or `additions`, with at most one `additions` Root. It replaces the Root that's
    special because it's named `additions`.
  - Registering a Root checks that its path exists, and that it can be written when it's declared writable.
  - A Root can be unregistered only when nothing is registered in it.
  - Its path or its writability can't be changed while a Job runs on it or while an approved, unapplied plan writes to
    it. Making it less writable voids the approved plans that write where it no longer may.
- **The container's mounts are the trust boundary** (ADR 0006). The workbench can register any folder the container
  can see. Mounting read-only what Title Select must never write is how the Operator bounds it.
- **Every Ceiling and the 7z archive limits are workbench settings.** A Ceiling comes from the Operator's value, then the
  Calibration, then the CPU count. There's no file tier, and nothing is locked. `max_dictionary_mib` is shown with the
  memory it costs.

## Considered Options

- **Keeping Roots in the file as the bound on the workbench**: rejected; the mounts already bound what Title Select
  can reach, and the file makes a first run need hand-written configuration.
- **Allowed base paths in the environment, with Roots registered beneath them**: rejected as a second boundary that
  adds a concept for little gain over the mounts.
- **Auth in the store**: rejected; the workbench mustn't be able to change who can reach the workbench, which risks
  both locking the Operator out and widening access.
- **A file holding only `[auth]`**: rejected; environment variables already carry the rest of bootstrap, and suit a
  container.

## Consequences

- A first run has no Roots, so the workbench leads the Operator through registering one, then Source Sets and Library
  Folders.
- ADR 0008: an Ingest Folder is a Root of kind `ingest` registered as writable in the workbench, not declared in
  `title-select.toml`.
- ADR 0022: a Ceiling's sources lose the configuration file; the workbench value is first.
