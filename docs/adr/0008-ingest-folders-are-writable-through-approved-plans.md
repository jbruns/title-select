# 8. Ingest Folders are writable, and moved out of only through approved plans

Date: 2026-10-31

## Status

Accepted

## Context

An Ingest Folder is where new files arrive, from a Downloader or a manual drop. The glossary first said Title Select
never writes to it. Then filling a Missing Game, discarding junk or importing a DAT found there would leave a copy
behind for the Operator to delete by hand, forever. Downloaders already move files out of their completed folders.
Title Select already writes Managed Folders safely: a marker, a no-follow folder handle, renames that never replace,
identity checks and durable intents (ADR 0007, #37, #38). Unlike a Managed Folder, an Ingest Folder is also written
by the Operator and by Downloaders, which may run as another uid with group write. Any of them can append to, overwrite,
rename, replace or link to an entry at any moment, including while Title Select is moving it.

## Decision

- An Ingest Folder is a Root of kind `ingest`, declared writable in `title-select.toml`. There may be several. The
  Operator registers each one, which writes a marker naming this store; every scan and every write fails closed
  without it.
- Scans only read. They are Jobs the Operator starts, or that an optional per-folder schedule queues; there is no
  filesystem watching. An item is assessed only once its size, modification time and change time stay unchanged for
  the settle interval. Its hashes are reused under the same change test an incremental Audit uses: device, inode,
  size, modification time and change time all unchanged. A ZIP is judged from its central directory first, by
  member count, names, declared sizes and each member's first bytes. Its members are then read only up to the limits
  of one game's ZIP. Archives within it and anything past the limits are Unsupported, and never read further.
- Nothing leaves an Ingest Folder except through a plan the Operator approves. The plan lists each item, its
  destination and the reason. Only an item that has settled and was assessed can be planned, and it is checked again
  before it is moved. An item with another hard link to it is refused, since moving one name would not move the file.
  The plan's digest binds a fingerprint of every entry in the folder: device, inode, size, modification time, change
  time and link count, without re-hashing. Each move's intent binds the same values for its item. Approval is
  refused if anything changed. The change time catches an overwrite of the same length with its modification time
  put back, and a new link under a name scans ignore.
- Every move goes through one shared primitive, recorded as a durable intent before anything changes and rolled
  forward by recovery once the folder's Root and marker check out:
  - on the same filesystem and mount, a rename that never replaces, made only while the entry is still the file
    planned. Afterwards, what arrived is read back: type, identity, size, modification time, one link and its
    SHA-256. A rename changes the change time, so the re-hash stands in for it. Anything else is put back under its
    name, or set aside in the kept files if that name was taken meanwhile, and the move fails;
  - otherwise, a copy to a recorded staged name, checked by hash, published by a rename that never replaces. Only
    then is the original moved into the private `.deleting` area. It is read back there against everything bound at
    approval and the hash of the copy, and deleted only if it still matches and the item has not been seen changing.
    An original that changed before it was set aside stays in the folder; one that changed after is kept in the
    kept files instead of deleted. Either way, the outcome says so;
  - a ZIP holding a Game's ROM among other members has the ROM repacked as TorrentZip, and the original is kept.
- Discarding an item and importing a DAT move the file into the folder's own kept files. Deleting kept files is a
  later, separately approved cleanup.
- Importing a newer revision of a registered DAT records, before the import, the incremental Audit it owes each
  Source Set bound to that DAT. Each owed Audit is queued when no other Job of that Source Set is in its way: at
  once, once that Job ends, or when the service next starts.

## Consequences

- Every step tolerates other writers and fails toward keeping data: a check that fails leaves the entry where it is,
  puts it back, or keeps it, and never deletes it.
- A residual risk remains. A writer that already holds the original open, and writes to it after the last read-back
  in `.deleting`, loses those bytes when the original is deleted. Nothing short of not deleting closes this, and a
  Downloader writes only to files it has not finished. Those are not yet settled, so they cannot be planned.
- A process running as the service's uid is out of scope, as for Managed Folders.
- Repairs and Operator Additions move items out through the same primitive and plans.
- A DAT found in an Ingest Folder is imported through the same validation as an upload.
