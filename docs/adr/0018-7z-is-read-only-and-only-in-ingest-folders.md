# 18. 7z is read only in Ingest Folders, within a bounded dictionary

Date: 2026-11-11

## Status

Accepted

## Context

Arrivals in Ingest Folders are often 7z, which Title Select treats as one opaque file. Torrent7z was considered as a
Storage Policy format, but archives other managers accept as torrent7z are byte-identical only when written by 7-Zip's
own C encoder at old pinned versions. RomVault's own variant disagrees with its companion tool on dictionary size,
and no Rust crate binds that encoder. Decoding a solid 7z needs memory about the size of its dictionary, up to 4 GB for
some RomVault archives, and the service runs in a fixed memory limit.

## Decision

- Title Select reads 7z, LZMA, LZMA2 and zstd, only in Ingest Folders, using the pure-Rust `sevenz-rust2`. Its members
  are assessed and moved by approved Change Plans like those of a ZIP. Source Sets and Library Folders never hold 7z.
- Title Select never writes 7z or torrent7z, and doesn't read RAR.
- At most `archive_workers` archives (default 2) are decoded at once. An archive with a dictionary larger than
  `max_dictionary_mib` (default 128) is refused with a message, before any memory is allocated for it. Members are
  decoded one at a time and never cached; applying a plan decodes again.
- The memory limit to set is about twice the measured peak without 7z, plus `archive_workers` × `max_dictionary_mib`.

## Considered Options

- A deterministic RomVault-style 7z written in Rust: recognised by RomVault, but not byte-identical to anyone else's,
  so it lacks the point of torrent7z.
- Binding 7-Zip's C encoder for byte-identical torrent7z: a large, fragile dependency for a format with no settled
  definition.
