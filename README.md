# Title Select

Audits ROM collections against DAT files, and records exactly what was verified and against which DAT revision.

Status: early pilot. You get a headless audit command; a store of DAT revisions, source sets and audits; the Service,
a JSON API that runs audits, selections and builds as durable jobs; and a headless selection command. A build writes a
selection into a managed folder for your front end, as TorrentZips or loose as your storage policy says, once you
approve its change plan. Removing and
replacing built files follow.
The words used here are defined in [GLOSSARY.md](GLOSSARY.md).

## Audit

```sh
title-select audit --dat <DAT file> <source set directory> [--evidence <quick|full>] [--output <report file>]
```

This audit is stateless: it needs no store or configuration. `audit --set` (below) audits a registered source set
and records the audit in the store; its report has the same shape.

Audits every file under the source set directory against a Logiqx XML DAT (for example a No-Intro DAT) and
writes a JSON report to stdout, or to `--output`. It only reads: your source set is never changed.

- Every payload is read and checksummed (size, CRC32, MD5, SHA-1, SHA-256). Each member of a `.zip` archive is
  an entry; any other file, including other archive formats, is one entry. Nested archives are not opened.
- `--evidence quick` is a quick inventory: each ZIP member's size and CRC32 are taken from the archive's directory
  as claim evidence, without reading its payload. Claim evidence never makes an entry verified. Other files are
  still read. A stateless audit has nothing earlier to reuse, so it is otherwise a full audit, and an incremental
  request says so on stderr and reports `full`.
- A finding is decided by payload before name: a file whose name is wrong but whose payload matches a ROM is
  misnamed, not unmatched. A payload matches a ROM when every checksum the DAT supplies for it agrees.
- An unreadable file, directory, archive or member becomes an unreadable entry and the audit continues.
  Symbolic links are followed.
- Exits 0 when the audit completes, whatever it found. Exits non-zero with a message on stderr when the DAT
  cannot be read or the source set directory does not exist.

### Report

```jsonc
{
  "schema": 3,                       // version of this report's shape
  "dat_revision": {                  // the DAT revision the audit is pinned to
    "sha256": "254e…",               // SHA-256 of the unmodified DAT file: its identity
    "name": "Atari - Atari 2600",    // from the DAT header
    "version": "20261027-195049",
    "file": "Atari - Atari 2600 (20261027-195049).dat"
  },
  "source_set": "/path/to/set",
  "started_at": "2026-10-28T22:45:46.539Z",
  "finished_at": "2026-10-28T22:45:46.942Z",
  "evidence": "incremental",         // quick, incremental or full: how the audit got its checksums
  "summary": {
    "findings": { "matched": 786, "misnamed": 19, "conflicting": 3, "mismatched": 0, "unmatched": 41, "unreadable": 0 },
    "games": { "complete": 802, "incomplete": 0, "missing": 70 }
  },
  "entries": [
    {
      "path": "Breakout (Europe).zip",           // relative to the source set
      "member": "Breakout (Europe).a26",         // present for a ZIP member
      "finding": "conflicting",
      "roms": [{ "game": "Breakout (Europe) (Rev 1)", "name": "Breakout (Europe) (Rev 1).a26" }],
      "named_rom": { "game": "Breakout (Europe)", "name": "Breakout (Europe).a26" },
      "evidence": {
        "level": "payload",                      // or "claim"
        "algorithms": ["size", "crc32", "md5", "sha1", "sha256"],
        "read_at": "2026-10-21T08:12:03.117Z",   // when the payload was read
        "reused": "unchanged_file_heuristic"     // present when this audit reused that read
      }
    }
  ],
  "games": [
    { "name": "Breakout (Europe)", "status": "missing", "missing_roms": ["Breakout (Europe).a26"] }
  ]
}
```

- `finding` is one of `matched`, `misnamed`, `conflicting`, `mismatched`, `unmatched` or `unreadable` (see the
  glossary).
- `roms` lists the ROMs the entry counts towards. For a matched entry that is every ROM with its name and payload,
  so an identical ROM shared by several games counts for each. For a misnamed or conflicting entry it is every
  ROM the payload matches, so the entry completes the game its payload belongs to.
- `named_rom` is the ROM the entry's name gives, present when it differs from the payload's (conflicting, mismatched).
- `evidence.algorithms` lists what the finding rests on. For a payload match, it is size plus each checksum that
  every ROM in `roms` supplies; all of them were compared and agreed. A mismatched or unmatched entry has only size and
  CRC32, the lookup that found no matching ROM. An unreadable entry has no evidence and a `reason` instead.
- `evidence.level` is `payload` when the checksums were computed by reading the payload, and `claim` when a quick
  inventory took them from a ZIP archive's directory; a claim's `algorithms` are always size and CRC32, and it has
  no `read_at`. `read_at` is when the payload was read. `reused` is `unchanged_file_heuristic` when the audit reused
  an earlier read because the file looked unchanged; see `audit --set`.
- A game is `complete`, `incomplete` or `missing`; `missing_roms` lists its ROMs no entry matched. `claimed_roms`,
  present only after a quick inventory, lists its ROMs that only a claim matched: its status rests on claims for
  them, and a build never uses a claim as a source.

## Selection

```sh
title-select select --dat <DAT file> --profile <selection profile file> [--output <file>] [--timeout <seconds>]
```

Chooses releases from a DAT with your selection profile and prints the explained selection as JSON on stdout, or to
`--output`. The selection engine is pinned and runs as a subprocess: offline, on one CPU, in a scratch folder under
`<store>/tmp` (or the system's temp folder without a store) that is deleted afterwards, and stopped after `--timeout`
seconds (600 by default). The selection is derived from the engine's trace of every decision it made, never from the
DAT the engine writes. The engine install is `--retool`, else `TITLE_SELECT_RETOOL`, else `/opt/retool`; an install
built from other pins than this binary's is refused. `title-select --version` prints the pins, and the workbench's
About page says where the engine comes from.

Your selection profile is JSON; every field is optional, and an unknown field, region, language or video standard is
refused:

```jsonc
{
  "region_order": ["USA", "World", "Europe"],  // a region left out is filtered out; empty: the default order
  "language_order": ["En"],                    // empty: the languages implied by the region order
  "localization_order": [],                    // local names only: no effect on what is chosen
  "video_order": [],                           // empty: the default order
  "exclude": ["demos", "bad_dumps"],           // add_ons, aftermarket, applications, audio, bad_dumps, bios,
                                               // bonus_discs, coverdiscs, demos, educational, games, manuals, mia,
                                               // multimedia, pirate, preproduction, promotional, unlicensed, video
  "options": {
    "prefer_retroachievements": false, "prefer_oldest": false, "prefer_regions": false,
    "prefer_licensed": false, "prefer_modern_rips": false, "filter_languages": false,
    "disable_1g1r": false,
    "compilations": "default"                  // default, individual, keep or optimize
  },
  "overrides": { "exclude": ["/^Pong"], "include": ["|Pong (USA)"] },  // text, |full name, or /regex
  "post_filters": [],
  "systems": {                                 // keyed by the system name derived from the DAT header
    "Atari - Atari 2600 (No-Intro)": { "region_order": ["Europe", "USA"], "replace_post_filters": false }
  },
  "choices": {                                 // your choices, by system and then group: the releases you checked
    "Atari - Atari 2600 (No-Intro)": { "pong": ["Pong (Europe)"], "parachute": [] }
  }
}
```

A per-system order, `exclude` or `options` replaces the global one; its overrides are added to the global ones, and
its post filters too unless `replace_post_filters` is set. An empty region or video order, global or per-system, is the
default order, which suits English speakers: [docs/default-orders.md](docs/default-orders.md) lists it and says where
it comes from.

In overrides and post filters, plain text matches part of a release's name, `|` the full name and `/` a regular
expression (Python syntax, ignoring case). A profile with a regular expression that doesn't compile is refused, so it
can't silently match nothing. Anything else the engine warns about on a run that succeeds goes to the server's log, or
to standard error for `title-select select`.

A choice replaces the selection's winners in one group with the releases you checked, zero or more, after the engine
has run. It is kept only where it differs from what the engine picks, or where you confirmed an unresolved group's
pick. A choice is not applied, and is flagged, when a checked release is now excluded by your settings (conflicting)
or gone from the group (stale); a choice for a group that is gone is kept and listed until you drop it. A choice that
the engine now makes anyway is flagged as matching the default, so you can drop it.

```jsonc
{
  "pins": {                          // what the selection was made with
    "retool_commit": "59f169f5…", "patch_set_sha256": "…", "clone_list_commit": "cdeb391e…", "trace_schema": 1
  },
  "dat_sha256": "254e…",             // the DAT revision
  "system": "Atari - Atari 2600 (No-Intro)",
  "system_settings": false,          // whether the profile has settings for this system
  "summary": { "groups": 601, "winners": 602, "alternatives": 270, "exclusions": 0, "unresolved": 2 },
  "coverage_drift": {                // where the clone list and metadata no longer match the DAT
    "clone_list": true, "metadata": true,
    "unmatched_clone_list_terms": ["…"], "titles_without_metadata": ["…"]
  },
  "groups": [
    {
      "group": "parachute",
      "unresolved": true,            // decided by a tie-breaker: the winner is provisional
      "winners": [
        { "title": "…", "unresolved": true,
          "rule": { "stage": "Parent selection", "ref": "REF0059", "message": "…" } }
      ],
      "alternatives": [
        { "title": "…", "clone_of": "…", "rule": { "stage": "…", "ref": "…", "message": "…" } }
      ],
      "exclusions": [ { "title": "…", "reason": "Demos, kiosks, and samples", "rule": { "…": "…" } } ]
    }
  ]
}
```

- Each winner, alternative and exclusion carries the rule that decided it: the stage, the engine's trace reference and
  its message. `ref` is absent when no trace record names the release, for example a release filtered out by region.
- A group is `unresolved` when its deciding rule is the engine's fail-safe (choosing the highest string) or a
  compilation tie-breaker. The engine's pick stays as the provisional winner: review it. Confirming it, or choosing
  other releases, resolves it.
- A group with your choice carries `choice`: the releases you checked, its `status` (`applied`, `matches_default`,
  `conflicting` or `stale`), the engine's `picks` and whether the engine left it unresolved. Releases it moved carry
  a rule whose stage is `choice`. The selection's `stale_choices` lists your choices for groups that are gone.
- A group has `drift` when a release in it has no metadata, or the clone list names it but no longer matches the DAT.

## Store

The store is one directory: a SQLite database (`title-select.sqlite`), the unmodified DAT files under `dats/<sha256>`,
`backups/`, and `tmp/`, where selections and previews run. Pass it with `--store <dir>` or `TITLE_SELECT_STORE`; it is created on
first use. Only one Title Select process may have a store open at a time: it holds an exclusive lock on
`<store>/lock`, and another process fails at once with "the store is in use by another Title Select process".

- The database runs in WAL mode. Every connection uses `synchronous=FULL`, `foreign_keys=ON` and a busy timeout.
- A DAT file is written to a temp file, fsynced and renamed into place before its row is inserted. Files under
  `dats/` that no DAT revision holds are deleted each time the store opens.
- Migrations are embedded and forward-only. Before one runs on a store that already has a schema, the database is
  copied with `VACUUM INTO` to `backups/schema-<version>-<time>.sqlite`. A store from a newer Title Select is refused.
- A DAT revision's file is kept while a retained or running audit refers to it or while it is the newest imported
  revision of its DAT. Otherwise the file is deleted after the next import or audit; the DAT revision's row stays, so
  history still names it. Importing the same file again restores it and keeps it until an audit using it finishes or a
  newer revision of its DAT is imported.

## Configuration

`title-select.toml`, passed with `--config <file>` or `TITLE_SELECT_CONFIG`, declares the roots Title Select may
read. Without it there are no roots, so Title Select reads none of your folders.

```toml
[roots.sets]                # a root called `sets`
path = "/data/source-sets"
writable = false            # the default
# writable_paths = ["No-Intro/Atari - 2600"]   # source sets in it that repairs may write

[roots.play]                # a root for your front end's `roms/` folder
path = "/data/roms"
writable = true             # only managed folders may live in a writable root

[roots.downloads]           # an ingest folder, where new files arrive
path = "/data/downloads/games"
writable = true             # an ingest root is always declared writable
kind = "ingest"
settle_secs = 60            # unchanged this long before an item is assessed (the default)
# scan_every_minutes = 15   # scan on a schedule; by default only when you ask

[audits]
keep = 20                   # full audits kept per source set (the default)

[selections]
keep = 20                   # full selections kept per source set (the default)

[auth]                      # how the Service knows it is you; see "Service"
identity_header = "Remote-User"
trusted_proxies = ["10.0.0.0/8"]
# dev_identity = "Developer"

[jobs]
concurrent = 1              # jobs run at once (the default)
pace = "default"            # gentle | default | unthrottled
# readers = 2               # overrides the pace's payload readers
# hashers = 2               # overrides the pace's hash workers

[path_templates]            # the paths registration proposes; see "Path templates"
source_set = "{provider}/{dat}"     # the default
managed_folder = "{platform}"       # the default
```

### Path templates

When you register a folder, a path template proposes its path in the root from the DAT, so you don't type it in full.
Its tokens are:

- `{provider}`: the DAT's provider, such as `No-Intro` or `Redump`;
- `{dat}`: the DAT's header name, such as `Atari - Atari 2600`;
- `{platform}`: the front end's folder name for the system, such as `atari2600`.

A source set's path is proposed from `path_templates.source_set`, by default `{provider}/{dat}`: the Atari 2600 DAT
gives `No-Intro/Atari - Atari 2600`. `path_templates.managed_folder`, by default `{platform}`, proposes the managed folder
a play library build writes to, in a writable root pointing at the front end's `roms/` folder: `atari2600`. You can change
either default here, use another template when you register, or type the path yourself.

The proposed path keeps every character of the provider and DAT name as it is. It must be relative to the root, not
empty and without `..`, and registration refuses a path that resolves outside the root. Folders are separated with
`/`: a path with a `\` or a drive such as `C:` is refused. A template that could never give a valid path, such as an
empty one or one starting with `/` or `../`, is refused when the configuration loads. A template only proposes: when
you change it, source sets you already registered keep their paths.

`{platform}` comes from a table in this repository, `crates/api/data/platforms.tsv`, of No-Intro and Redump DATs and
the system folder names ES-DE uses that RomM also reads as the same system. A system they name differently, such as
the Commodore Plus/4, isn't listed. A dump-format qualifier at the end of a DAT's name, such as `(Headered)` or
`(Decrypted)`, doesn't change its system. A DAT the table doesn't list has no `{platform}`: the workbench says so, and
you type the path.

## Commands

```sh
title-select dat import <DAT file> --provider <name> [--source-url <url>]
```

Imports an unmodified DAT file as a DAT revision, recording its SHA-256, header `name` and `version`, original
filename, import time, provider and source URL. A DAT is identified by its provider and header name, written
`<provider>/<name>`; each import of new bytes adds a DAT revision of it. Importing the same bytes again changes
nothing and prints the existing DAT revision, restoring its file if the store had dropped it.

```sh
title-select set add <root> [<relative path>] --dat <provider>/<name> [--template <path template>]
title-select set list
title-select set history <source set>
```

`set add` registers a folder in a root, such as `sets "No-Intro/Atari - 2600"`, as a source set bound to an imported
DAT. The source set is named by its folder (`Atari - 2600`). Without a path, it registers the path the path template
proposes for the DAT: `--template`, else `path_templates.source_set`, by default `{provider}/{dat}` (see "Path
templates"). An absolute path, `..`, or a symlink that resolves outside the root is refused. `set list` prints each source set with the DAT revision it was last verified against:
its latest audit's, the latest being the last to finish. `set history` lists a source set's audits, latest first, with
the time, DAT revision, counts, evidence, and whether the full audit is kept or only its summary.

```sh
title-select audit --set <source set> [--revision <sha256 prefix>] [--evidence <quick|incremental|full>]
  [--output <report file>]
```

Audits a registered source set, read-only, and records it. Unlike the stateless audit, it does not follow a symlink
that resolves outside the root: such a file or folder is an unreadable entry with that reason. Without `--revision`,
the DAT revision is the latest audit's, else the newest imported revision of the bound DAT, so a newer DAT revision is
adopted only when you name it. The entries and game statuses are written in one transaction when the audit finishes,
and the report below is generated from them. Afterwards, audits beyond the newest `audits.keep` are pruned to a
one-row summary; the latest audit, and any audit cited as provenance, are never pruned.

An audit is incremental unless you choose otherwise. The store records, for each file whose payloads were all read,
the checksums and the file's device, inode, size, modification time and status change time. An incremental audit
reuses that read when all five are unchanged, and reads the file again otherwise. This change test is a heuristic: a
file rewritten in place with its times put back would pass it, so its entries keep the time their payload was read
and say the read was reused. `--evidence full` reads every payload again. `--evidence quick` reuses reads as an
incremental audit does, and takes the remaining ZIP members as claims, as for the stateless audit. Each entry is
matched against the DAT revision afresh, so an audit against a newer DAT revision reuses the checksums and compares
them with the new ROMs. The read, reused and claimed counts go to stderr, and `set history` shows each audit's
evidence.

```sh
title-select ingest register <ingest root>
title-select ingest scan <ingest root>
title-select ingest items <ingest root>
```

`ingest register` registers a root of kind `ingest` as an ingest folder, writing its marker; see "Ingest folders".
`ingest scan` scans it now, and `ingest items` lists each item with its state and what is proposed for it.

## Service

```sh
title-select serve [--listen <address:port>] [--ui <dir>] [--retool <dir>]
```

Runs the Service: the `/api/v1` JSON API, and the jobs that run audits and selections. It listens on `--listen` or
`TITLE_SELECT_LISTEN`, by default `127.0.0.1:8080`, and holds the store, and its lock, until it stops on SIGTERM or
Ctrl-C. While it runs, the store commands above fail with "the store is in use"; use the API, or the headless audit
below, instead. This is the supported way to run Title Select on your server. With `--ui` or `TITLE_SELECT_UI` naming
the workbench's built bundle, it also serves the workbench at `/`; see "Workbench". Selections and previews use the
selection engine install named by `--retool` or `TITLE_SELECT_RETOOL`, else `/opt/retool`, as for `select`. Without
one the Service still starts and runs audits, and refuses selections and previews, saying why.

### Auth

Every API path but `/api/v1/health` needs you to authenticate. The Service refuses to start without a way to
authenticate you:

- `[auth] identity_header` with `[auth] trusted_proxies`: your authenticating reverse proxy sets the header to your
  identity. The header is trusted only from a peer inside one of the CIDRs, and a request carrying it from any other
  peer is rejected. The identity is recorded as the actor of each job the request starts.
- The operator token: a file named by `TITLE_SELECT_OPERATOR_TOKEN_FILE` (or `--token-file`) holds a secret, sent as
  `Authorization: Bearer <token>`. The Service keeps only its SHA-256 and compares in constant time. Jobs it starts
  record the actor `Operator token`.
- `[auth] dev_identity`, for local development: a request with no credential is you, under that identity. The
  Service refuses to start with it unless it listens on a loopback address.

Any other request, or a wrong token, gets `401`.

### Jobs

An audit started through the API is a job: `queued`, `running`, then `succeeded` or `failed`. Each change of state is
one transaction, and a job runs to the end whether or not its client stays connected. The DAT revision is chosen when
the job is queued, as for `audit --set`, and its file is kept until the job ends. A source set has at most one queued
or running job; starting another is refused with `409`, naming the job.

A job found running when the Service starts was cut off by a stop: its unfinished audit is discarded, the job becomes
`interrupted`, and a fresh job (attempt 2) is queued for it. If that job is cut off too, it becomes `failed`. An audit
records each file's read as soon as the file is finished, so the fresh job resumes after the last finished file: it
reuses the reads the interrupted job made, and for an incremental audit or quick inventory any earlier ones too, but
only for files that pass the change test above. Claims are not
recorded; a resumed quick inventory takes them again.

`[jobs] concurrent` jobs run at once, 1 by default. Each audit reads payloads on `readers` threads, which also
decompress ZIP members, and checksums them on `hashers` threads. The pace sets both:

| `pace`        | readers | hashers | for                                                          |
|---------------|---------|---------|--------------------------------------------------------------|
| `gentle`      | 1       | 1       | one file at a time, leaving disks and CPUs to other services |
| `default`     | 2       | 2       | the default: overlaps one file's reading with another's hash |
| `unthrottled` | CPUs    | CPUs    | one of each per CPU, for an otherwise idle server            |

`[jobs] readers` or `hashers` overrides the pace's count. A job records the counts it was queued with.

A selection is a job too, of kind `selection`. It runs one version of a selection profile against the source set's
DAT revision, chosen as for an audit, and records the selection in the transaction that ends the job. A source set
may have one audit job and one selection job queued or running at once. Afterwards, selections beyond the newest
`selections.keep` are pruned to their summary; the latest is never pruned.

### Selection profiles

A selection profile is named, and every save is a new version, numbered from 1, that is never changed or deleted.
A version is stored as canonical JSON with its SHA-256, so saving a profile with nothing changed creates no version.
A save names the version it started from, and is refused with `409` if a newer version was saved since. A save may
queue a selection for one or more source sets with the new version.

### Storage policy

The storage policy says how each folder stores a game: as one TorrentZip per game, `<game>.zip`, or loose, as its
ROM itself under the name the DAT gives it. It has two scopes, source sets and play libraries (managed folders), each
with a global setting and settings for each system, a DAT as `<provider>/<name>`, as a selection profile has. The
default is TorrentZip everywhere. Only a game with one ROM can be stored loose: a game with more stays TorrentZip, and
so does a game whose ROM's name isn't a plain file name; a plan says which games and why.

Every write of a game into a folder follows it: a build writes each system's play library in that system's format,
and a repair, including a fill from an ingest folder, writes a source set in its system's format. Saving the policy
(`PUT /api/v1/storage-policy`) changes no file. The next plan for a folder whose format changed proposes converting
each game stored the other way, and a plan made under the old policy is refused at approval. A conversion is a
repair of a source set, or part of a build of a play library: it writes the game in the new format, from what an
audit verified, and moves the original into the folder's kept files.

"Delete originals after conversion", set globally and for each system in each scope, is off by default. When it's on,
a conversion deletes each original only once its converted file is verified: a TorrentZip is opened as canonical and
its members' hashes and sizes match the ROMs exactly; a loose file's hashes match its ROM exactly. The original is
kept aside first, then moved into `.title-select-retained/.deleting` and read back through the same open file, and
deleted only when it still matches what the conversion read and recorded, before it put the converted file in place:
its hash, size, modification time and change time. A check of the folder never changes that record. An original
that changed in any way, even repacked with the same ROMs, or whose converted file doesn't verify, stays in the kept
files, and the plan's outcome says why. Only a game whose ROMs are the same, by name, size and every hash, is
converted; a game whose ROMs changed is replaced, and its old file is kept, never deleted. A conversion the service stopped during ends, when it next starts, with the original either deleted, when
that was proven before the stop, or kept; a file found by its path alone is never deleted.

### Managed folders and builds

A managed folder is a folder in a writable root that Title Select builds selections into, for your front end to play
from. It is bound to a source set and a selection profile, and so to a system; a system may have several, each with
its own profile. A source set cannot be registered in a writable root, and nothing else is ever written to one.

Registering a managed folder needs a folder that is empty or missing; registration creates it and writes a
`.title-select` marker holding the store's id, the folder's id and its build generation. A plan refuses a folder whose
marker is missing, names another store or folder, or has another generation, as after restoring a backup or mounting
the wrong share.

A plan lists, for each selected release, what a build would do, from the selection and the source set's latest audit
against the selection's DAT revision:

- **copy** a source that is already the game's TorrentZip, holding exactly its ROMs, where the storage policy says
  TorrentZip;
- **repack** the game's ROMs into a new TorrentZip, or, where the storage policy says loose, write its one ROM as a
  loose file. A game built the other way is converted: the new file is written and put in place, then the old one
  is kept aside, or deleted once the new one verifies when delete originals after conversion is on, before the next
  game is written, so a conversion never needs room for a second copy of the folder. An old file left in place by a
  stop is kept aside, or deleted under the same rule, by the next build, before it writes anything else: the plan
  lists it with the file that replaced it;
- **unchanged**: the folder already holds the file, as built;
- **no source**: no audited file holds every ROM, by payload. Claims are never sources: a ROM only a quick
  inventory claimed has no source until you audit the source set incrementally or in full;
- **conflict**: a file the store did not build, or one changed since it was built, is at the target. It is never
  overwritten;
- **blocked**: the game's name is not a valid file name on every filesystem, two names differ only in case, or the
  game is not in the DAT;
- **adopt**: you chose to adopt a conflict whose file, at exactly the game's name, holds exactly the ROMs the DAT
  expects. Nothing is written; the build checks the file again and records it as built;
- **unresolved**: you chose to build resolved groups only, and the game's group is unresolved. Whatever is at its
  target is left untouched.

Each ROM's source is ranked: matched before misnamed or conflicting, payload evidence before metadata, a copyable
archive before another, then by path. No source, conflict and blocked items are skipped and shown. A plan cannot be
approved while the source set has no audit against the selection's DAT revision, a group is unresolved and you have
neither confirmed a choice nor chosen to build resolved groups only, the root is not writable, or the folder, its
marker or its retention area is wrong. It shows what changed since the current build, and whether your settings,
your choices or the DAT revision caused it.

A plan never deletes a file. A built file whose game is no longer selected is a removal: the plan keeps it aside when
a check finds it as built, forgets its record when it is gone already, and leaves it when it changed since it was
built. A built file the plan writes again from other ROMs, as when a newer DAT revision changes the game, is a
replacement: the old file is kept aside and the new one written in the same build. Keeping a file aside moves it, by
one rename that never replaces anything, into `.title-select-retained/<generation>/<name>.kept` in the same folder,
only while it is still the file the plan found. The retention area and each generation's folder are created under a
temporary name the store records, made durable, renamed into place without replacing anything, and used only while
they are still the empty folder made, owned by the service and private to it. One never renamed into place is left,
recorded; none of these folders is ever removed, even once empty. The store records each kept file with its original name, the build
that kept it and its provenance, and holds that provenance's selection and audit from pruning. A plan never removes a
file Title Select did not build.

Your choices are part of the plan, and of its digest: building resolved groups only, and, for each conflict, leaving
it (the default), adopting it when it holds the ROMs the DAT expects, or keeping it aside and building the game in its
place. To change one, plan again with it. The removals are part of the digest too.

Kept files stay until you delete them: list them (`GET /managed-folders/{id}/kept-files`), plan deleting some or all
(`POST /managed-folders/{id}/kept-deletions`), review the plan's digest, and approve it. Each kept file is deleted
only while it is still the file that was kept: it is moved, without replacing anything, into the private folder
`.title-select-retained/.deleting`, and deleted there only once what was moved is found to be that file. Anything
else, a symlink or a directory included, is left or moved back, and reported. Until it is moved back, it stays
recorded where it is, and the deletion stays unfinished and blocks plans for the folder; when its name is taken, you
move what is there out of the way, and the service moves it back when it next starts. A kept file found already gone, with the
folder's marker checked, is reported gone and no longer listed. A deletion the service stopped during is finished
when it next starts, once the folder's root is writable and its marker shows it is this store's folder; until then
its kept files stay recorded and plans for the folder are blocked. Checks and drift never report the retention area.

Planning also checks the folder, as a check job does. A check job (`POST /managed-folders/{id}/checks`) reads the
folder and writes only to the store. It reads a built file only when its size or modification time differs from the
store's record: a file with the same content refreshes the record; one holding the same ROMs in another ZIP is
recorded as built and flagged not TorrentZip; anything else is a conflict. Every other entry is drift, reported as a
known title that is not selected, a selected title the store did not build, a known title under another name, an
unknown file, or a directory, which is never opened. Symlinks and special files are unknown and never opened. Title
Select's own entries are never drift. Drift is a warning: only a conflict at a selected game's file makes the folder
incomplete.

A cleanup acts on what a check found, only as you choose, entry by entry (`POST /managed-folders/{id}/cleanups`):
keep an entry aside, a file or a whole folder, into the retention area as a kept file; adopt a file that holds exactly
a selected game's ROMs under the current DAT revision, renamed to that game's file name when it is not already, and
recorded as built; or acknowledge an entry, which writes nothing to the folder and stops reporting it until its
identity, size or modification time changes. A cleanup also puts kept files back, under their own name when it is
free and, for a file holding a selected game, that game's file name; a kept file holding a selected game under
another name is adopted under that game's file name instead, and one whose name is taken is refused. It repacks a
built file flagged not TorrentZip from its own contents, in one read, keeping the file as it was aside; if the
service stops after the old file is kept aside and before the new one is in place, a selected game is reported failed, so
the folder shows as incomplete until you put the kept copy back or build again. A kept folder is put back
as it was, never adopted or deleted by Title Select. Every entry the cleanup found is part of its digest, chosen or
not, along with the built files, acknowledged entries and kept files as they were: if anything in the folder changes
before you approve, or before the cleanup starts, it is refused and you review the new plan.
Every move is one rename that never replaces anything, made only while the entry is still the one the plan found,
and recorded before it is made, so that a cleanup the service stopped during is settled when it next starts. A
cleanup needs a current build, whose selection and DAT revision it uses; only one that moves or writes something
advances the folder's generation. A cleanup never counts as a build: a build that stopped stays reported until a
build finishes.

A folder is incomplete when its current build lacks some of its selection: unresolved groups left out, games with no
source, files that failed, conflicts and blocked games, or built files a check found missing. The store records this
for each build, with counts and items by reason; it is never written to the marker. A build cut off by a stop or a
failure shows as an interrupted or failed build, with how many files were applied and how many are uncertain, from
the store's record, straight after a restart. Planning again finishes it: the files already built are unchanged.

A plan is stored with the SHA-256 of its content, its digest, and you approve it by sending that digest. The approval
queues a job of kind `build`, which plans again and refuses, writing nothing, when the plan changed since you
approved it, storing the new plan for you to review. Otherwise it writes each file into a staging folder inside the
managed folder, re-hashes its ROMs against the DAT, checks the TorrentZip's structure, and renames it into place
without replacing anything; a source that fails its re-hash fails only its own file. Files get your umask's
permissions and the build's time as their modification time. The marker's generation goes up last, when the build
succeeds. Every write goes through the managed folder as it was opened, never a symlink, and the build checks the
marker again before each file and before the marker; if the marker changed, or the folder itself fails a write, the
build fails and the marker stays where it was. You can cancel a build while it runs: the file it is writing, and
the original it converts, finish; files already in place stay, the rest are left out, and the build shows as
interrupted until a new plan finishes the folder. Renaming without replacing needs a filesystem that supports it; when
yours does not, the build fails rather than risk overwriting a file. A ROM name with a path separator, a `..`
part, a drive or a device name such as `CON` is a blocked item.

The staging folder is private to the server's user. Each file is renamed into place only while the staged file is
still the one verified, and recorded only if the file then in place is that one; otherwise it is left and the build
fails. The store records the next generation before the marker follows it, and a marker is never moved back.
While a build is queued or running, its selection and audit are kept from being pruned.

A build cut off by a stop or a failure is not run again. When the server starts, it cleans up after such a build only
if the managed folder's root is still writable and the folder's marker is still this store's and matches what the
store recorded. It removes only what the build recorded creating, by device and inode: the staging folder, the files
in it, and a marker temporary file. It records each file already put in place whose content it re-hashes and finds
as planned, and marks any other file at that target uncertain, leaving it as it is. A marker that did not follow the
generation the store recorded is moved forward to it. Otherwise it changes nothing, and planning for that folder is
blocked until you resolve it and restart the server; this includes a marker newer than the store, as after you
restore the store from a backup. Anything else in the staging folder, or at the marker's temporary name, also blocks
planning until you remove it.

The current build of each managed folder keeps its selection and its audit from being pruned.

### Ingest folders

An ingest folder is a writable root of kind `ingest` where new files arrive, such as a downloader's completed folder
or a folder you drop files into. There may be several. Registering one writes a marker naming this store; without it,
scans and every write fail.

A scan reads the folder and changes nothing in it. It is a job you start, or that `scan_every_minutes` queues; the
folder is not watched. Dotfiles and partial downloads (`.part`, `.!qB`, `.crdownload`, `.tmp`) are ignored. A new or
changed item is arriving until its size, modification time and change time stay the same for `settle_secs`; then it
is assessed: hashed once, and matched against the current DAT revision of every source set. Its hashes are reused
while its device, inode, size, modification time and change time are unchanged, as an incremental audit reuses an
entry's. A ZIP is opened, here and in audits, only in the canonical layout TorrentZip and mainstream ZIP writers
produce. Its last end record is the real one, and its comment runs exactly to the end of the file with no other end
record in it. Its directory ends exactly where that record (or its ZIP64 record, which ends at the locator just
before it) begins, and starts with a directory header. Its directory declares at most 65535 entries and 64 MiB, and a
ZIP64 end record carries at most 64 KiB of extensible data. Any other ZIP is refused unread: as unsupported here, as
unreadable in an audit. Whether a file is a ZIP is decided by how it begins: one that does not begin as a ZIP is a
loose file, hashed whole, whatever bytes its tail holds, and is never opened as an archive. A ZIP is judged from its
directory before any member is read. Its members are read only up to the limits of one game's ZIP: 4096 members, 16 GiB each and 32 GiB in all. Each item is one of:

- assessed, with what it is: a game a source set is missing, or a better copy of a mismatched or unreadable entry;
  a candidate operator addition, with the system its extension suggests when exactly one registered system uses it;
  or a DAT you may import;
- already held, when every source set it matches holds it with a verified payload, or it is a DAT revision already
  imported. A game held only by a quick audit's claim is not already held: the item would replace an unverified copy;
- unsupported: 7z, RAR, an archive holding archives (by name or by content), a ZIP of several games that no one
  game accounts for, a ZIP past the limits, or a folder, never read further;
- unreadable, failed, stale (changed since it was assessed), gone, dismissed (until it changes), or moved.

Items leave the folder only through a plan you approve. Only an item that has settled and been read can be in a
plan, whatever state it shows; an unsupported or dismissed item can still be discarded unread once settled, and
dismissing an item never makes it movable. An item with other hard links to it cannot be in a plan. A plan lists each item, where it goes and why, and binds every
entry of the folder by device, inode, size, modification time, change time and link count. Approving it is refused if
anything changed.
Discarding an item moves it into the folder's kept files, `.title-select-retained/<generation>/<name>.kept`. Importing
a DAT imports it as an upload would, then keeps the file the same way. When it is a newer revision, it queues an
incremental audit of each source set bound to its DAT. If another job of that source set is queued or running, the
audit is queued once that job ends; until then it keeps the DAT's file from being cleaned up. Within one filesystem a move is a rename that never replaces anything. What
arrives is then read back and hashed; anything other than the file planned is put back, or kept if its name was taken
meanwhile, and the move fails. Across filesystems the file is copied, the copy checked by hash and renamed into place.
The original is deleted only when the copy exactly matches a ROM of the DAT the plan names; any other original is
kept, never deleted except by a cleanup you approve. Before deleting, the original is set aside and read back again,
and deleted only if it is still exactly what was copied. An original that changed meanwhile is left in the folder, or
kept, and never deleted; the plan's outcome says which. Moves into kept files are always renames.
A move cut off by a stop is finished when the service next starts, once the folder's root and marker check out.

### Repairs

A repair changes a source set, and only through a plan you approve, separately from any build. The source set's root
must list its path in `writable_paths`, which keeps the source set registered where it is, with its history; and its
folder must be writable when the repair runs. The first repair of a source set writes a `.title-select` marker
holding the store's id and the source set's id, and says so in its plan; every later repair, and every move into the
source set, is refused while the marker is missing or names another store or source set.

A repair is proposed from the source set's latest audit and from items assessed in ingest folders. It acts only on
what the audit read from payloads, as a full audit, or an incremental audit reading a file, does; a file the audit
judged by name and size alone, or that changed since that audit read it, is left as it is and listed with why,
even when a later audit that didn't finish read it again. A rename checks again that the file holds exactly the
game's ROMs before it renames it. Each item has a choice you can change before approving:

- a game whose files are named otherwise than the DAT says is renamed, or written under its name: on by default;
- a game not stored as the storage policy says, `<game>.zip` or its one ROM loose, is repacked or converted to it:
  always proposed for now. A conversion writes the converted file and puts it in place first, then keeps the
  original, or deletes it once the converted file verifies when delete originals after conversion is on. If the
  converted file's name is taken, the original stays where it is, unless you chose to keep what is there aside: it
  is kept aside before the converted file is put in place;
- a missing game is filled from an ingest folder item whose payload matches it: on by default when there is one;
- a game with a mismatched or unreadable entry is replaced from an ingest folder item that matches it, keeping the
  original: on by default when there is one;
- an entry whose payload is one ROM while its name gives another is a conflict: nothing is proposed, and you choose
  to take it as the ROM its payload matches, keep it aside or leave it;
- unmatched entries, duplicates and extra members of a game's ZIP are kept aside by default.

Setting a file aside that holds the last copy of a ROM no repair writes is never the default; when you choose to,
the item says so, and you can restore it from the kept files. A conflicting entry you leave is never displaced by
another item, and an unreadable entry is replaced only while it is as the audit found it. The plan lists every
change in order: each file written, renamed, published or kept aside. It binds the source set's folder, and every
entry in it, by device, inode, size, modification time, change time and link count, and its audit: approving is
refused if anything changed or a newer audit finished, or while an audit of the source set, or a build reading it,
is queued or running. If something else puts another file in place of one a repair is moving, that file is never
deleted or left untracked: it's put back under its old name, or recorded among the kept files as what it is.

Approving queues the repair as a job, run by the same scheduler as audits, within `jobs.concurrent`; the repair
screen and the jobs screen show its progress, one step at a time. While it is queued or running, and until its
follow-up audit is queued, no audit of the source set starts and no build reading it is approved. You can cancel it:
the step it is on finishes, every change of the item it started finishes, and the rest are skipped.

A repair never deletes. A file it replaces or sets aside is moved, by a rename that never replaces anything, into the
source set's kept files, `.title-select-retained/<generation>/<name>.kept`; a new file is written under a temporary
name, read back, and put in place by a rename that never replaces anything. A fill moves the ingest folder item as an
approved ingest move does: the item's original is deleted only when what was written is exactly the ROM it names, as
for a loose file holding one ROM, and kept in the ingest folder's kept files otherwise. Once a repair finishes it
queues an incremental audit of the source set, even if the service stops right after. A repair cut off by a stop
resumes as a job when the service next starts: what it started is finished, or rolled back, once the root, the path
and the marker check out. So does a repair whose job failed, for example because a file it staged and won't use
couldn't be deleted; until it finishes, the source set stays reserved for it. A repair that stops before its first
change, for example because the folder can't be written or a marker appeared in it, ends there instead, saying why,
and you plan it again. Something that would stop a resumed repair the same way every time ends that change instead,
saying what was left where: for example, if a file it put in place was replaced before its ingest original was dealt
with, the fill fails and both are left as they are. Audits never read `.title-select` or
`.title-select-retained`. You delete a source set's kept files by picking them and approving the deletion.

### API

Every body is JSON, and every error is `{"error": "...", "job": <id, when one is named>}`.

| Method and path                                    | What                                                         |
|----------------------------------------------------|--------------------------------------------------------------|
| `GET /api/v1/health`                               | `{"status": "ok"}`, unauthenticated                          |
| `GET /api/v1/roots`                                | the configured roots                                         |
| `GET /api/v1/dats`                                 | each DAT with its DAT revisions                              |
| `GET /api/v1/dat-revisions`                        | every DAT revision                                           |
| `POST /api/v1/dat-revisions`                       | import a DAT: multipart `provider`, `source_url`?, `file`    |
| `GET /api/v1/source-sets`                          | the source sets, with the DAT revision last verified against |
| `GET /api/v1/path-templates`                       | the default path templates: `{"source_set", "managed_folder"}` |
| `POST /api/v1/source-sets`                         | register one: `{"root", "path"?, "template"?, "dat"}`; with no `path`, the path `template`, else the default, proposes |
| `GET /api/v1/source-sets/{name}`                   | one source set                                               |
| `GET /api/v1/source-sets/{name}/history`           | its audits, latest first                                     |
| `POST /api/v1/audits`                              | queue an audit job: `{"source_set", "revision"?, "evidence"?}`, `evidence` being `quick`, `incremental` (the default) or `full`; `202` |
| `GET /api/v1/audits/{id}`                          | an audit's summary                                           |
| `GET /api/v1/audits/{id}/report`                   | the full `schema: 3` report                                  |
| `GET /api/v1/audits/{id}/entries`                  | entries, paged; `?finding=`                                  |
| `GET /api/v1/audits/{id}/entries.ndjson`           | every entry, one JSON object per line                        |
| `GET /api/v1/audits/{id}/games`                    | game statuses, paged; `?status=`                             |
| `GET /api/v1/vocabulary`                           | the names, categories and default orders                     |
| `POST /api/v1/check`                               | check a selection profile: `{"problems": [...]}`             |
| `GET /api/v1/profiles`                             | the selection profiles, each with its newest version         |
| `POST /api/v1/profiles`                            | create one with no versions: `{"name"}`                      |
| `GET /api/v1/profiles/{name}`                      | one selection profile                                        |
| `GET /api/v1/profiles/{name}/versions`             | its versions, newest first                                   |
| `POST /api/v1/profiles/{name}/versions`            | save: `{"profile", "base_version"?, "queue"?}`; creates it   |
| `GET /api/v1/profiles/{name}/versions/{version}`   | one version                                                  |
| `GET /api/v1/storage-policy`                       | the storage policy: `{"source_sets", "play_libraries"}`, each `{"format", "delete_originals", "systems"}`, `format` being `torrent_zip` or `loose`, and `systems` each system's own `{"format"?, "delete_originals"?}` |
| `PUT /api/v1/storage-policy`                       | replace it; changes no file, and the next plans propose the conversions |
| `POST /api/v1/preview`                             | the selection a profile would make: `{"source_set", "profile", "revision"?}`, with the changes from the latest saved selection |
| `GET /api/v1/selections?source_set=`               | a source set's selections, newest first                      |
| `POST /api/v1/selections`                          | queue a selection job: `{"source_set", "profile", "version"?, "revision"?}`; `202` |
| `GET /api/v1/selections/{id}`                      | a selection's record and summary                             |
| `GET /api/v1/selections/{id}/selection`            | the explained selection, as `select` prints it               |
| `GET /api/v1/selections/{id}/releases`             | releases, paged, with availability; `?role=`, `?group=`      |
| `GET /api/v1/selections/{id}/availability`         | game statuses of the latest audit against its DAT revision   |
| `GET /api/v1/selections/{id}/diff`                 | what changed from the previous selection; `?against=`        |
| `GET /api/v1/managed-folders`                      | the managed folders, each with its current build             |
| `POST /api/v1/managed-folders`                     | register one: `{"root", "path"?, "template"?, "source_set", "profile"}`; `201` |
| `GET /api/v1/managed-folders/{id}`                 | one managed folder                                           |
| `PATCH /api/v1/managed-folders/{id}`               | rebind it: `{"source_set"?, "profile"?}`                     |
| `GET /api/v1/managed-folders/{id}/builds`          | its builds, newest first                                     |
| `GET /api/v1/managed-folders/{id}/files`           | the files built into it, with their provenance               |
| `POST /api/v1/managed-folders/{id}/plans`          | plan building a selection into it: `{"selection", "resolved_only"?, "adopt"?: [target], "keep_aside"?: [target]}`; `201` |
| `POST /api/v1/managed-folders/{id}/cleanups`       | plan a cleanup: `{"entries"?: {name: "keep_aside" \| "adopt" \| "acknowledge"}, "restore"?: [kept id], "repack"?: [target]}`; `201`, approved as a plan |
| `POST /api/v1/managed-folders/{id}/checks`         | check it; `202` with the check job                           |
| `GET /api/v1/managed-folders/{id}/check`           | its latest check: each built file, the drift and the acknowledged entries; `404` when never checked |
| `GET /api/v1/managed-folders/{id}/incomplete`      | what its current build lacks, by reason                      |
| `GET /api/v1/managed-folders/{id}/kept-files`      | the files builds kept aside in it, newest generation first   |
| `POST /api/v1/managed-folders/{id}/kept-deletions` | plan deleting kept files: `{"ids"?: [id], "all"?}`; `201`     |
| `GET /api/v1/kept-deletions/{id}`                  | a plan to delete kept files, with its digest and outcomes    |
| `POST /api/v1/kept-deletions/{id}/approval`        | approve it: `{"digest"}`; deletes them and answers with each outcome, `409` when they changed |
| `GET /api/v1/ingest-folders`                       | every ingest root, with its counts of items by state once registered |
| `POST /api/v1/ingest-folders`                      | register one: `{"root"}`; `201`                              |
| `GET /api/v1/ingest-folders/{root}`                | one ingest folder                                            |
| `POST /api/v1/ingest-folders/{root}/scans`         | scan it; `202` with the scan job                             |
| `GET /api/v1/ingest-folders/{root}/items`          | its items, with each one's state, evidence and proposal      |
| `GET /api/v1/ingest-folders/{root}/plans`          | its plans, newest first                                      |
| `POST /api/v1/ingest-folders/{root}/plans`         | plan moving items: `{"action": "discard" \| "dat_import", "items": [id], "provider"?}`; `201` |
| `GET /api/v1/ingest-folders/{root}/kept-files`     | the files its plans kept aside                               |
| `GET /api/v1/ingest-items/{id}`                    | one item                                                     |
| `POST /api/v1/ingest-items/{id}/dismissal`         | dismiss it until it changes                                  |
| `GET /api/v1/ingest-plans/{id}`                    | an ingest plan, with its digest and outcomes                 |
| `POST /api/v1/ingest-plans/{id}/approval`          | approve it: `{"digest"}`; moves the items and answers with each outcome, `409` when the folder changed |
| `GET /api/v1/source-sets/{name}/repairs`           | its repair plans, newest first                               |
| `POST /api/v1/source-sets/{name}/repairs`          | plan a repair from its latest audit: `{"choices"?: {key: "apply" \| "leave" \| "keep_aside"}}`; `201` |
| `GET /api/v1/repairs/{id}`                         | a repair plan, with its items, steps, digest and outcomes    |
| `POST /api/v1/repairs/{id}/approval`               | approve it: `{"digest"}`; queues its repair job and answers with the plan naming it, `409` when anything changed |
| `GET /api/v1/source-sets/{name}/kept-files`        | the files its repairs kept aside                             |
| `POST /api/v1/source-sets/{name}/kept-deletions`   | plan deleting kept files: `{"ids"?: [id], "all"?}`; `201`     |
| `GET /api/v1/source-kept-deletions/{id}`           | a plan to delete a source set's kept files                   |
| `POST /api/v1/source-kept-deletions/{id}/approval` | approve it: `{"digest"}`; deletes them and answers with each outcome |
| `GET /api/v1/plans/{id}`                           | a plan, with its digest, summary and changes                 |
| `POST /api/v1/plans/{id}/approval`                 | approve it: `{"digest"}`; `202` with the build job, `409` when the plan changed |
| `GET /api/v1/builds/{id}`                          | a build and each file it wrote                               |
| `GET /api/v1/jobs`                                 | the latest 100 jobs, newest first                            |
| `GET /api/v1/jobs/{id}`                            | one job                                                      |
| `GET /api/v1/jobs/{id}/events`                     | server-sent events for one job, ending at its final state    |
| `POST /api/v1/jobs/{id}/cancellation`              | cancel a repair job at the next step, or a build job at the next file; `409` once it has ended |
| `GET /api/v1/events`                               | server-sent events for every job, opening with a `resync`    |

A paged list is `{"items": [...], "next_cursor": "..."}`; pass `next_cursor` back as `?cursor=` for the next page, and
`?limit=` (1 to 1000, default 100) for its size. Each item carries its `seq`, its position in the report. A DAT upload
may be up to 512 MiB, and is `201` when it adds a DAT revision, `200` when the bytes were already imported. After a
quick inventory, availability's `claimed` names the games whose status rests on claims, and such a release carries
`"claimed": true`.

Server-sent events carry JSON data, under one of these event names:

| Event      | Data                                   | Sent                                                         |
| ---------- | -------------------------------------- | ------------------------------------------------------------ |
| `state`    | the job                                | when a job's state changes; first on a job's own stream      |
| `progress` | `{"job", "files_done", "files_total"}` | while a job is running, at most five times a second          |
| `resync`   | `{"jobs": [...]}`, newest first        | by `/events` on connecting, and after the client fell behind |

A `resync` lists every queued or running job and the 50 most recently finished; it replaces what the client knew,
since events in between may have been dropped. An idle stream sends a comment every 15 seconds.

Every job carries a `version` that goes up with each change of its state, and `progress` carries the `version` of the
running job it belongs to. The Service drops events that a `resync` or a later `state` already superseded, and a
client should do the same: ignore a `state` whose `version` is at or below the one it holds for that job, and
`progress` whose `version` is below it.

### Headless audit

```sh
title-select audit --server <url> --set <source set> [--revision <sha256 prefix>]
  [--evidence <quick|incremental|full>] [--output <report file>]
```

Starts an audit job on the Service at `<url>`, sending your operator token from `TITLE_SELECT_OPERATOR_TOKEN_FILE` (or
`--token-file`), and follows it to the end. `--evidence` chooses the audit's evidence as for `audit --set`. Progress
and a summary of the findings, game statuses and the read, reused and claimed counts go to stderr; the
report goes to stdout, or to `--output`. If the job's event stream sends nothing, not even its keep-alive, for 45
seconds, the client polls the job's state instead. Exits 0 when the audit completes, and non-zero when the job fails,
the token is refused or the Service cannot be reached.

### Image

`ghcr.io/jbruns/title-select` is a static binary, the workbench's bundle (at `/ui`) and the patched selection engine
with its pinned clone lists and venv (at `/opt/retool`) on `gcr.io/distroless/python3-debian12:nonroot`, for
linux/amd64. The licence texts the About page shows are at `/ui/licences`: Title Select's, the selection engine's and
the clone lists and metadata's. It runs `serve` on `0.0.0.0:8080` as the nonroot user, with the store at the volume
`/store`, and serves the workbench at `/`. It needs nothing else writable but your writable roots, so you can run it
with a read-only root filesystem:

```sh
docker run --read-only -p 8080:8080 -v title-select-store:/store \
  -v /path/to/token:/run/operator-token:ro -e TITLE_SELECT_OPERATOR_TOKEN_FILE=/run/operator-token \
  -v /path/to/title-select.toml:/etc/title-select.toml:ro -e TITLE_SELECT_CONFIG=/etc/title-select.toml \
  -v /data/source-sets:/data/source-sets:ro ghcr.io/jbruns/title-select:sha-<commit>
```

CI publishes `sha-<commit>` (the full commit SHA) for each push to `main`, and `<version>` and `<major>.<minor>` for
each `v<version>` tag.

### Health

```sh
title-select health [--url <url>] [--timeout <seconds>]
```

Checks a running Service, for a container healthcheck in an image with no shell or curl. `--url` defaults to
`http://127.0.0.1:8080/api/v1/health` and `--timeout` to 5 seconds (1 to 300). It follows no redirect and ignores
proxy settings. Exits 0, printing nothing, when the Service answers 200 with `{"status": "ok"}`; otherwise exits 1
with one line on stderr saying why: the Service cannot be reached, did not answer in time, answered another status, or
answered another body. In Compose:

```yaml
healthcheck: { test: ["CMD", "/title-select", "health"] }
```

## Workbench

The workbench is your browser interface to the Service: a Leptos app compiled to WebAssembly that uses only the
`/api/v1` JSON API, from the same origin. Its screens:

- **Source sets** lists each source set with its latest audit and the DAT revision it was last verified against, and
  registers another from a root, a DAT and a path in the root. The path template proposes the path from the DAT, with a
  live preview of the folder it names; change the template, or type over the path and use "Use the proposed path" to
  go back. When the DAT has no `{platform}`, the preview says so and you type the path. Opening a source set shows its facts, starts an audit against
  the default DAT revision or a newer one, as a quick inventory or an incremental (the default) or full audit, makes a selection with a selection profile, lists its selections, and
  lists its audit history, latest first, with a Re-audit for each row. A re-audit is incremental.
- **DAT revisions** lists every DAT revision, newest import first within each DAT, with its provenance (provider,
  source URL, file, SHA-256, import time), whether its file is kept, and the audits that refer to it. It uploads a DAT
  file as a new DAT revision.
- **Audit** shows the audit's source set, DAT revision, times and evidence, exports its JSON report and re-audits. Its summary
  counts each finding and game status, and two derived states: an entry is verified when it is matched with payload
  evidence, and needs review when it is misnamed, conflicting, mismatched, unmatched or unreadable. Its entries load in
  one request from the bulk NDJSON endpoint, then filter by name, finding, state, game status and evidence algorithm,
  and sort by any column, in the browser as each key is typed. Selecting an entry shows its evidence level and algorithms,
  when its payload was last read and whether this audit reused that read because the file looked unchanged (a
  heuristic), and the ROMs it matches. A pruned audit shows only its summary.
- **Selection profiles** lists your selection profiles. A new one starts as an unsaved draft with the defaults:
  preview what it selects, change anything, and confirm to save version 1. The editor has a global settings tab and
  a tab per system; each system setting says whether it comes from your global settings or is set for that system,
  with "Use global setting" to go back. "Reset to defaults" and "Reset to default choices" work per group, per system
  and for the whole profile. An empty region or video order shows the default order it stands for, which you can
  expand, start your own order from, or reset to, with a link to how the default orders work. The preview follows your edits 500 ms after you stop, cancelling a preview still
  running; when a preview of a source set takes more than 2 seconds, it waits for the Preview button instead. It
  shows what changed from the latest saved selection. Saving creates the next version and can make a selection.
- **Selection** explains every group: each release's role, the rule that decided it, its availability in the latest
  audit against the same DAT revision, and flags for unresolved, stale, conflicting and drift. Its releases filter by
  name, role, availability and flag as you type. Opening a group lets you check the releases you want, confirm an
  unresolved pick, include an excluded release, or reset the group to the default choices; your choices are staged
  until you save them as the next version of the profile, which makes a new selection. "Plan a build" plans building
  it into a managed folder bound to its source set and selection profile.
- **Storage policy** sets how source sets and play libraries store games, TorrentZip or loose, and whether a
  conversion deletes each original once its converted file verifies, with a global settings tab and a tab per
  system, as the selection profile editor has. Saving changes no file; each folder's next plan proposes its
  conversions, and a build or repair plan says which format it follows and which games stay TorrentZip, and why.
- **Managed folders** lists each managed folder with its current build and when it was last checked, marking a check
  made before the current build, and registers another in a writable root,
  with the path proposed from the source set's DAT as for a source set. Opening one plans a build of a selection,
  shows the plan's files by what they do, what blocks it and what changed, approves it and follows the build live, with a way to cancel it.
  It rebinds the folder, and lists its builds and the files built into it. A plan with unresolved groups offers to build
  resolved groups only, each conflict you can adopt has a choice to leave or adopt it, and each conflict you can keep
  aside has a choice to keep it aside and rebuild. The plan lists the built files no longer selected and what
  happens to each. The folder lists its kept files by generation, where you pick some or all and approve deleting
  them. It plans a cleanup of what the latest check found: an action for each entry not built here, kept files to
  put back and built files to repack as TorrentZip, reviewed and approved as a plan. The folder shows badges
  for an incomplete folder and an interrupted build, what the folder lacks, and its latest check, which you can run
  again. Each build shows every file it wrote or failed to write.
- **Ingest folders** lists each ingest root and registers it. Opening one scans it, and lists each item with its
  state, the evidence assessing it found and what is proposed. You pick items to plan discarding them or importing
  DATs, review the plan, approve it and see what happened to each item; you can dismiss an item until it changes.
  It lists the folder's kept files.
- **Repair**, opened from a source set or an audit, proposes a repair of the source set, lists each item with what is
  proposed and a choice you can change before planning again, and every change in order, including what is kept
  aside. You approve it with its digest and see what happened to each item. While an approved repair hasn't
  finished, the screen opens on it, with its job's progress and a way to cancel it, instead of proposing another, and
  says what stopped it if it was left unfinished; a repair job on the jobs screen links to its plan. It lists the
  source set's kept files, where you pick some and approve deleting them.
- **Jobs** lists the latest jobs with their kind, state, progress and result, live: it follows `/api/v1/events`, applies `state`
  and `progress` events by the job `version` rule above, and replaces what it holds with each `resync` snapshot,
  which also arrives after a reconnect.
- **About**, linked from every screen's footer, says where selection comes from, explains the default orders, and shows the licence texts of Title
  Select, the selection engine and its clone lists and metadata, served from the bundle's `licences/` folder.

Every table is virtualized, sorts by its column headers and works from the keyboard: Tab to the table, then the arrow
keys, Page Up, Page Down, Home and End move the selected row, Enter opens it and Escape clears it. Each screen's
controls are ordinary buttons, links and form fields. The workbench needs an evergreen browser; it makes no request
to any other origin and keeps no credential in browser storage. Its fonts, Liberation Sans and Liberation Mono, are
served with it under the SIL Open Font License (`crates/ui/assets/fonts/LICENSE-OFL.txt`).

The workbench's own files are served without auth, with a same-origin Content Security Policy; everything it shows
comes from the API, which needs you to authenticate as above. Behind an authenticating reverse proxy, the proxy's
identity header covers the browser's API requests. To run it locally, with `[auth] dev_identity` on a loopback address:

```sh
rustup target add wasm32-unknown-unknown   # rust-toolchain.toml lists it
cargo install cargo-leptos --locked
cargo leptos build --release               # the workbench into target/site, and the binary
tools/retool/setup                         # the selection engine, with the licences the About page shows
mkdir -p target/site/licences && cp LICENSE target/site/licences/title-select.txt \
  && cp .retool/retool/LICENSE target/site/licences/retool.txt \
  && cp .retool/retool/clonelists-LICENSE target/site/licences/retool-clonelists-metadata.txt
printf '[roots.sets]\npath = "/data/source-sets"\n\n[auth]\ndev_identity = "Developer"\n' > dev.toml
target/release/title-select --store dev-store --config dev.toml serve --ui target/site --retool .retool
```

Then open `http://127.0.0.1:8080/`.

`crates/ui/bench/filter.py` measures the audit screen's filtering over 100,000 synthetic entries in headless Chromium.
It builds the workbench with the `bench` feature, which adds a `/bench` screen of synthetic entries that no release or
image has, serves it on a loopback port with a scratch store under `target/bench`, and reports p50 and p95. It needs
Playwright for Python; from the repository's root:

```sh
python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
.venv/bin/python crates/ui/bench/filter.py
```

## Development

Decisions are recorded in [docs/adr/](docs/adr/). A Cargo workspace: `crates/api` (the API's and the report's types), `crates/engine` (the Audit), `crates/store`
(the store and configuration), `crates/server` (the Service), `crates/cli` (the `title-select` binary) and
`crates/ui` (the workbench). The workbench's filtering, sorting, derived states and Job event handling are plain Rust,
tested on the host with `cargo test`; `cargo clippy --package title-select-ui --target wasm32-unknown-unknown` checks
it as the browser runs it. `crates/selection` runs Retool and derives the Selection from its trace.

### Retool

`third_party/retool/` holds Retool for Selection (ADR 0001):

- `upstream/`: the pristine Retool tree at a pinned commit, without its GUI, never edited in place. Its `tests/`
  fixtures seed the conformance tests.
- `patches/`: applied in order at build time. `0001-no-network` turns every download into an error, so a missing
  clone-list or metadata folder or config file fails instead of being fetched; `0002-run-paths` adds
  `--clonelist-dir`, `--metadata-dir`, `--mia-dir`, `--ra-dir` and `--system-config-dir`; `0003-trace-json` adds
  `--trace-json <path>`; `0004-system-settings` makes a system's `override exclusions` clear the aftermarket exclusion
  and its `override options` clear and accept `l` and `n`, which the parser missed.
- `pins.toml`: the Retool commit and the sha256 of its tree, each patch's sha256, and the clone-list and metadata
  commit with the sha256 of its archive. `tools/retool/build.py` checks every one before it builds.
- `requirements.in` and the hash-locked `requirements.txt`: Retool's runtime dependencies, without its GUI's.

`tools/retool/setup [dir]` builds `.retool/` (gitignored), which the Selection tests use through
`TITLE_SELECT_RETOOL` (cargo sets it): the patched Retool with the pinned clone lists, the pristine Retool for the
patch-neutrality test, and a Python 3.11 venv made with [uv](https://docs.astral.sh/uv/). Run it after changing a pin
or a patch; the tests fail, rather than skip, without it.

- **Clone-list pin:** Renovate proposes upstream's newest `main` commit in `pins.toml`, and the `clone-list pin`
  workflow writes the new archive's sha256 on its branch and runs CI. It runs the default branch's `build.py`, never
  the branch's code, because it holds a write token. By hand: change `commit`, run
  `python3 tools/retool/build.py pin-clone-lists`, then `tools/retool/setup` and the tests.
- **Retool pin:** frozen; it changes only by hand. Vendor the new tree into `upstream/`, rework the patches, and copy
  the hashes `python3 tools/retool/build.py hash-tree` prints into `pins.toml`.
- **Goldens:** `crates/selection/tests/goldens/` holds each conformance case's Selection and trace (its records that
  don't depend on set order). After a deliberate change to a patch, a pin or the derivation, regenerate them with
  `TITLE_SELECT_REGENERATE_GOLDENS=1 cargo test -p title-select-selection --test conformance` and review the diff.
- **A real DAT:** `TITLE_SELECT_ATARI_DAT=<DAT file> cargo test -p title-select-selection --test selection --
  --ignored --nocapture` selects from it, checks every group has a rule and prints the counts. Never commit the DAT.

## Licence

MIT. See [LICENSE](LICENSE). The vendored Retool is BSD-3-Clause
([third_party/retool/upstream/LICENSE](third_party/retool/upstream/LICENSE)); the image ships the clone lists and
metadata with their BSD-3-Clause licence beside them. The image also ships all three licence texts under
`/ui/licences`, where the workbench's About page shows them.
