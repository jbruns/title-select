# 8. Ingest Folders are writable, and moved out of only through approved plans

Date: 2026-10-31

## Status

Accepted

## Context

An Ingest Folder is where new files arrive, from a Downloader or a manual drop. The glossary first said Title Select
never writes to it. Then filling a Missing Game, discarding junk or importing a DAT found there would leave a copy
behind for the Operator to delete by hand, forever. Downloaders already move files out of their completed folders.
Title Select already writes Managed Folders safely: a marker, a no-follow folder handle, renames that never replace,
identity checks and durable intents (ADR 0007, #37, #38).

## Decision

- An Ingest Folder is a Root of kind `ingest`, declared writable in `title-select.toml`. There may be several. The
  Operator registers each one, which writes a marker naming this store; every scan and every write fails closed
  without it.
- Scans only read. They are Jobs the Operator starts, or that an optional per-folder schedule queues; there is no
  filesystem watching. An item is assessed only once its size, modification time and change time stay unchanged for
  the settle interval. Its hashes are reused while its device, inode, size and times are unchanged.
- Nothing leaves an Ingest Folder except through a plan the Operator approves. The plan lists each item, its
  destination and the reason. Its digest binds a fingerprint of every entry in the folder: identity, size and
  modification time, without re-hashing. Approval is refused if anything changed.
- Every move goes through one shared primitive, recorded as a durable intent before anything changes and rolled
  forward by recovery once the folder's Root and marker check out:
  - on the same filesystem and mount, a rename that never replaces, made only while the entry is still the file
    planned;
  - otherwise, a copy to a recorded staged name, checked by hash, published by a rename that never replaces, and
    only then the original is moved into the private `.deleting` area, checked again there, and deleted;
  - a ZIP holding a Game's ROM among other members has the ROM repacked as TorrentZip, and the original is kept.
- Discarding an item and importing a DAT move the file into the folder's own kept files. Deleting kept files is a
  later, separately approved cleanup.

## Consequences

- An Ingest Folder needs the same mount and ownership as a Managed Folder: the service's uid can rename entries in it,
  and nothing else can. A process running as that same uid is out of scope.
- Repairs and Operator Additions move items out through the same primitive and plans.
- A DAT found in an Ingest Folder is imported through the same validation as an upload.
