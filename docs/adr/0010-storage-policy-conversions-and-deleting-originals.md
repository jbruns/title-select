# 10. A Storage Policy decides each folder's format; Conversions delete an original only with a trusted stamp

Date: 2026-11-02

## Status

Accepted

## Context

Builds write TorrentZip (ADR 0007), and Repairs repack every Game as one. Some front ends and emulators read only
loose ROMs, and some Source Sets, PS2's for one, are too large to keep twice while they are converted. The Operator
needs to choose the format for each system's Source Sets and Play Libraries, and to change it later, without a
second copy of every Game left behind and without risking the only copy of one.

Deleting is new: until now a Build or Repair only ever moved what it replaced into the Kept Files (ADR 0002 keeps
Audits read-only; ADR 0008 deletes an Ingest original only when what was written is exactly a DAT ROM). Linux reuses
inode numbers, so a device, inode and stamp read from a path at recovery time can describe a different file than
the one a Conversion read.

## Decision

- **One decision point.** The Storage Policy has two scopes, Source Sets and Play Libraries, each with a global
  default and per-system overrides, as a Selection Profile has. The default is TorrentZip everywhere. Every writer
  of a Game into a folder, Repairs, fills, Builds and later Operator Additions, asks one function for its scope and
  system.
- **Loose is for single-ROM Games.** A Game with more than one ROM, or whose ROM's name isn't a plain file name,
  stays TorrentZip, and the plan says why. Loose output keeps the DAT's ROM name.
- **Changing a format is a Change Plan.** Saving the policy changes no file. The next plan for a folder proposes a
  Conversion for each Game stored the other way: a Repair item of a Source Set, or an item of a Build of a Play
  Library. The plan binds the policy it follows, so a plan made under another policy is refused at approval. A
  Conversion runs as part of its Repair or Build Job, with its reservations, cancellation and recovery.
- **Originals go to Kept Files.** The converted file is written, verified and published without replacing anything
  before the original is kept aside.
- **Delete Originals After Conversion** is a setting with the same scopes and overrides, off by default. When on, an
  original is deleted only when:
  - its converted file verifies: a ZIP is canonical as `zipdir` opens it and its members' payloads match the ROMs'
    hashes and sizes exactly; a loose file's hashes match the ROM exactly; and
  - the original, read back in `.title-select-retained/.deleting` through a descriptor held across the rename,
    matches the hashes, size and stamp the Conversion recorded when it read it, its change time unchanged while it
    was hashed. The change time after the rename is read through that descriptor and recorded before the unlink.
- **A stamp from a path never authorizes a deletion.** Recovery deletes a file stopped in `.deleting` only when it
  matches the recorded post-rename change time and hashes through a fresh descriptor; otherwise it is put back, or
  recorded as kept where it was found. Without a trusted stamp, the file is kept.

## Consequences

- With the default, nothing is ever deleted by a Conversion; the Operator deletes Kept Files by a separate approval.
- A Conversion the Service stopped during is recovered, not resumed: its original ends deleted, when that was proven
  before the stop, or kept.
- Multi-ROM Games stay TorrentZip even in a loose folder, so a loose folder can hold both.
- Audits already handle loose files and ZIPs in one folder, so a partly converted Source Set audits correctly.
