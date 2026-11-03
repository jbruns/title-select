# 8. Ingest Folders are writable, and moved out of only through approved plans

Date: 2026-10-31

## Status

Accepted

## Context

An Ingest Folder is where new files arrive, from a Downloader or a manual drop. The glossary first said Title Select
never writes to it. Then filling a Missing Game, discarding junk or importing a DAT found there would leave a copy
behind for the Operator to delete by hand, forever. Downloaders already move files out of their completed folders.
Title Select already writes Library Folders safely: a marker, a no-follow folder handle, renames that never replace,
identity checks and durable intents (ADR 0007, #37, #38). Unlike a Library Folder, an Ingest Folder is also written
by the Operator and by Downloaders, which may run as another uid with group write. Any of them can append to, overwrite,
rename, replace or link to an entry at any moment, including while Title Select is moving it.

## Decision

- An Ingest Folder is a Root of kind `ingest`, declared writable in `title-select.toml`. There may be several. The
  Operator registers each one, which writes a marker naming this store; every scan and every write fails closed
  without it.
- Scans only read. They are Jobs the Operator starts, or that an optional per-folder schedule queues; there is no
  filesystem watching. An item is assessed only once its size, modification time and change time stay unchanged for
  the settle interval. Its hashes are reused under the same change test an incremental Audit uses: device, inode,
  size, modification time and change time all unchanged. A ZIP is opened, in ingest and in Audits, only in
  canonical layout, so that the archive reader takes its first end record and never searches the file. That layout
  is the one TorrentZip and mainstream writers produce:
  - the last end-record signature in the tail is the end record;
  - its comment runs exactly to the end of the file, and holds no end-record or ZIP64 signature;
  - with ZIP64, the locator sits just before the end record, and the ZIP64 end record ends exactly at the locator,
    with at most 64 KiB of extensible data;
  - the central directory ends exactly where its end record begins, and starts with a directory header, or is empty.

  The directory's entry count and size are then bounded. Any other ZIP is refused unread: Unsupported in ingest,
  Unreadable in an Audit. Mirroring the reader's own search instead would leave its fallback scans, which can
  remember every directory-header signature in the file, free to exhaust memory. Whether a file is a ZIP is decided
  first by how it begins: one that does not begin with a local header (or, empty, with its end record) is a loose
  file, whatever its tail holds. Only a file judged a ZIP in canonical layout is ever given to the archive reader,
  through one gate every caller uses (probes, builds, repacks and Audits); everything else is hashed whole or refused.
  The reader looks a little further back for an end record than the longest comment allows, so a file the layout
  check cannot see as a ZIP must never reach it.

  An opened ZIP is judged from its central directory, by member count, names, declared sizes and each member's first
  bytes. Its members are then read only up to the limits of one game's ZIP. Archives within it and anything past the
  limits are Unsupported, and never read further.
- Nothing leaves an Ingest Folder except through a plan the Operator approves. The plan lists each item, its
  destination and the reason. Only an item that has settled can be planned: its stamp stayed unchanged for the
  settle interval, whatever state it is shown in. It must also have been read and assessed, except that an
  Unsupported or Dismissed item may be kept aside unread. Dismissing an item never makes it movable. Both are
  checked again before it is moved. An item with another hard link to it is refused, since moving one name would not move the file.
  The plan's digest binds a fingerprint of every entry in the folder: device, inode, size, modification time, change
  time and link count, without re-hashing. Each move's intent binds the same values for its item. Approval is
  refused if anything changed. The change time catches an overwrite of the same length with its modification time
  put back, and a new link under a name scans ignore.
- Every move goes through one shared primitive, recorded as a durable intent before anything changes and rolled
  forward by recovery once the folder's Root and marker check out:
  - on the same filesystem and mount, a rename that never replaces, made only while the entry is still the file
    planned. Afterwards, what arrived is read back: type, identity, size, modification time, one link and its
    SHA-256. A rename changes the change time, so the re-hash stands in for it. Anything else is put back under its
    name, or set aside in the kept files if that name was taken meanwhile, and the move fails. A move into the kept
    files is always such a rename, never a copy;
  - otherwise, a copy to a recorded staged name, checked by hash, published by a rename that never replaces. The
    original is deleted only when the published copy's hashes exactly match a ROM of the DAT the plan names (or, once
    Additions are moved, the hashes recorded when one was approved). Any other original is moved into the
    folder's kept files instead, and never deleted outside an approved cleanup. For an exact match, the original is
    moved into the private `.deleting` area. It is read back there against everything bound at
    approval and the hash of the copy, and deleted only if it still matches and the item has not been seen changing.
    An original that changed before it was set aside stays in the folder; one that changed after is kept in the
    kept files instead of deleted. Either way, the outcome says so;
  - a loose file repacked as a Game's TorrentZip has its original deleted, as above, only when the published ZIP,
    opened through the bounded ZIP gate, is canonical TorrentZip with exactly one member, and that member's payload,
    hashed again, has the size and every checksum (CRC32, MD5, SHA-1, SHA-256) of the ROM the move names and the
    SHA-256 and size of the original as the repack read it. The original set aside in `.deleting` must still hash as
    what was repacked. The payload is then the ROM, byte for byte, so the ZIP is only a container and the original
    adds nothing. Any other repack keeps its original;
  - a ZIP holding a Game's ROM among other members has the ROM repacked as TorrentZip, and the original is kept.
- Discarding an item and importing a DAT move the file into the folder's own kept files. Deleting kept files is a
  later, separately approved cleanup.
- Importing a newer revision of a registered DAT records, before the import, the incremental Audit it owes each
  Source Set bound to that DAT. Each owed Audit is queued when no other Job of that Source Set is in its way: at
  once, once that Job ends, or when the service next starts. An owed Audit holds its revision's DAT file against blob
  retention until its Job takes the hold over. It waits while its revision is not yet imported, and ends, with the
  reason recorded, only once the import failed or the plan finished without it. One that cannot be queued is
  recorded and does not hold up the others or the service's start.
- An item whose Game a Source Set holds only by a quick-inventory claim, never verified by reading its payload, is not
  Already held: it is shown as replacing an unverified copy.
- Every rename a move makes, including one a stop cut off and recovery found done, has both folders synced before
  the move is recorded as ended.

## Consequences

- Every step tolerates other writers and fails toward keeping data: a check that fails leaves the entry where it is,
  puts it back, or keeps it, and never deletes it.
- A residual risk remains. A writer that already holds the original open, and writes to it after the last read-back
  in `.deleting`, loses those bytes when the original is deleted. This is accepted because of the invariant above:
  only an original whose verified copy exactly matches a wanted ROM is ever deleted. That file is already complete,
  so later writes through a stale descriptor could only corrupt it, not add wanted content. A Downloader also writes
  only to files it has not finished, and those are not settled, so they cannot be planned.
- After a rename within one filesystem, a writer holding a stale descriptor can still alter the published file. The
  follow-up Audit's change test catches this, since the file's size, modification time or change time changes.
- A process running as the service's uid is out of scope, as for Library Folders.
- Repairs and Additions move items out through the same primitive and plans.
- A DAT found in an Ingest Folder is imported through the same validation as an upload.
