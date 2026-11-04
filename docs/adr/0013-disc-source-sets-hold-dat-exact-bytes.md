# 13. Disc Source Sets hold DAT-exact bytes; CHD exists only in Library Folders

Date: 2026-11-03

## Status

Accepted

## Context

Redump DATs hash each track's `.bin` or `.iso` and the `.cue`. Compressed disc formats such as CHD and RVZ don't
hash to those values without being decompressed, and a regenerated `.cue` rarely matches byte for byte, so a CHD in
a Source Set could be Verified only by decompressing it at every Audit, or trusted on claims. Disc Games have many
ROMs, so the Storage Policy keeps them TorrentZip (ADR 0010), which is slow for multi-gigabyte DVD images. Front
ends and emulators widely prefer CHD and `.m3u` playlists.

## Decision

- A Source Set holds a disc Game's ROMs exactly as the DAT lists them, so Audits verify it by payload hash like any
  other Game. CHD, RVZ and similar formats are never a Source Set format.
- The Storage Policy gains a loose folder format: `<game>/` holding the ROMs under their DAT names, open to any
  multi-ROM Game, and the default for disc systems.
- In Library Folders, a Build may convert a disc Game to CHD with a pinned `chdman` shipped in the image. A new CHD is
  verified once by extracting it and matching the DAT's hashes, which gives the trusted stamp ADR 0010 requires;
  Drift checks then read the SHA-1 the CHD records in its header.
- Every Game of one release is stored in one format: a Multi-disc Game's discs, and every track of a disc, are
  never split across formats.
- Redump's `.cue` files are DAT ROMs, so its cue pack can fill or replace them in a Repair. SBI files are not DAT
  ROMs: Title Select keeps them in its store and a Build places them beside a Game only when the system opts in.

## Considered Options

- CHD in Source Sets, Verified by decompressing at each Audit: keeps one copy, but turns every Audit into a full
  decompression and makes `.cue` provenance uncertain.
- CHD in Source Sets on claims only: cheap, but a Source Set would no longer be a verified preservation copy.

## Consequences

- A disc Source Set needs its full uncompressed size. If a system is too large to keep so, revisit for that system
  rather than weakening Evidence everywhere.
- RVZ, CSO and ZSO are later Build conversions, under the same verify-once rule.
