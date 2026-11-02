# Title Select

Title Select audits Source Sets against DAT Revisions and, later, builds explained Selections from them into a Play
Library. "Title Select" is the product's name, not a domain term.

## Inventory

**DAT**:
A published inventory of expected game files and their identifying checksums (e.g. No-Intro, Redump, MAME).
_Avoid_: datfile, database

**DAT Revision**:
One particular edition of a DAT, identified by the SHA-256 of its unmodified file. A newer revision does not replace
the reference used by an earlier Audit or Selection.

**Game**:
An entry in a DAT Revision: one release, made of one or more ROMs. The product says Game; Title is an accepted
synonym in conversation.
_Avoid_: set

**ROM**:
One expected file of a Game, with its name, size and checksums as the DAT Revision gives them. Also used for any
file, such as a `.wav` or `.cue`, that a DAT lists.

## Collections

**Source Set**:
A preserved collection, kept with its files, variants and provenance, that can be audited against its DAT and is
recorded with the DAT Revision of its latest Audit; the source from which a Play Library is built. It changes only
through an approved Repair, and only while its Root may write it and its marker says it is this store's Source Set.
_Avoid_: Verified Set, Archive, romset, dump folder

**Play Library**:
The selected collection that players and front ends read, derived for a device, purpose or preference. Title Select
builds Selections into it.
_Avoid_: Library (alone), output

**Ingest Folder**:
A folder where new files arrive, such as a Downloader's completed folder or a manual drop: a writable Root of kind
`ingest` that the Operator registers for Title Select to assess. Title Select writes to it only through an approved
plan that names each item, where it goes and why, and only while its marker says it is this store's Ingest Folder.
_Avoid_: Source (alone), Downloads

**Root**:
A named folder the Operator configures for Title Select to read, or to write, such as the one holding Source Sets.
A Source Set or a Play Library folder is identified by its Root and a path relative to it. A Root that is not
writable may still name, in its `writable_paths`, the Source Sets that Repairs may write.
_Avoid_: Mount, share

**Operator**:
The person who runs Title Select and approves its Change Plans.

**Job**:
A durable unit of work that Title Select runs for the Operator, such as an Audit. It is Queued, Running, Succeeded,
Failed or Interrupted, and it survives a browser disconnecting or the Service restarting.
_Avoid_: Task, run

**Operator Addition**:
An item the Operator approves for management in a Play Library independently of an official DAT, such as a ROM hack. It
is identified by its own hashes (CRC32, MD5, SHA-1, SHA-256 and size) and a name the Operator can edit, and may name a
parent Game as metadata only. It comes from an Ingest Folder item, Drift in a Managed Folder or a Source Set Entry
that matched nothing. Its recorded provenance and local integrity checks do not make it an official DAT match.
_Avoid_: Custom ROM, homebrew (as a kind)

**Additions Set**:
The folder holding the Operator Additions of one system, `<DAT name>` in the Root named `additions`, each stored under
its name in the format the Storage Policy says for its system's Source Sets. A Build reads it alongside the Source Set
only when its Selection Profile opts in for that system, and it is checked for Drift like a Managed Folder. An Additions
Job publishes approved additions into it, and moves the file of one retired into its own Kept Files.
_Avoid_: Extras folder, custom set

## Audit

**Audit**:
An assessment of a Source Set against a particular DAT Revision, recording discrepancies and the verification
evidence actually obtained.

**Entry**:
One file an Audit finds in a Source Set: a loose file, or a member of a ZIP archive.

**Evidence**:
What an Audit actually checked for an Entry: the Evidence Level and the checksum algorithms compared.

**Evidence Level**:
How an Audit knows an Entry's checksums. _Claim_: taken from an archive's directory without reading the payload.
_Payload_: computed by reading every byte.

**Incremental Audit**:
An Audit that reuses an earlier read of a file's payloads when the file looks unchanged since that read; the
default. Looking unchanged is a heuristic, not proof.

**Full Audit**:
An Audit that reads every payload, reusing no earlier read.

**Quick Inventory**:
An Audit that takes Claim Evidence for a ZIP archive's members instead of reading their payloads.

**Finding**:
An Audit's verdict on one Entry:

- _Matched_: its payload matches the ROM its name gives.
- _Misnamed_: its payload matches a ROM, and its name is not a ROM in the DAT Revision.
- _Conflicting_: its name is one ROM, but its payload matches another.
- _Mismatched_: its name is a ROM, but its payload matches no ROM. This is not proof of damage.
- _Unmatched_: neither its name nor its payload matches a ROM.
- _Unreadable_: it could not be read; the reason is recorded. Never reported as any other Finding.

_Avoid_: Bad, Extra

**Verified**:
An Entry whose Finding is Matched with Payload Evidence. Claim Evidence never makes an Entry Verified.

**Needs Review**:
An Entry whose Finding is Misnamed, Conflicting, Mismatched, Unmatched or Unreadable, or a Selection decision that
remains unresolved.

**Game Status**:
An Audit's verdict on one Game: _Complete_ (every ROM present), _Incomplete_ (some), or _Missing_ (none). A
Misnamed or Conflicting Entry counts towards the Game its payload matches. A ROM present only on Claim Evidence still
counts, but the status names it as resting on claims, and a Build never sources it.

## Selection and change

**Selection Profile**:
The Operator's rules for including or excluding releases and choosing preferred releases of a Game: global settings
with per-system settings. It holds no DAT content and is reusable across DATs. Each saved version is immutable.
Profile is its short form.
_Avoid_: Filtered DAT (when meaning the rules)

**Selection**:
The exact releases chosen from a particular DAT Revision using a Selection Profile and any explicit Operator
overrides, each with the rule responsible.
_Avoid_: Profile (when meaning the chosen releases), Filtered DAT

**Change Plan**:
A proposed set of changes that the Operator approves before a Source Set is repaired or a Selection is built into a
Play Library. Approval to repair a Source Set is separate from approval to build a Play Library.

**Repair**:
Applying an approved Change Plan to a Source Set, proposed from its latest Audit's payload Evidence and from items
assessed in Ingest Folders: renaming Entries to the DAT's names, repacking Games, filling Missing Games, replacing
Mismatched or Unreadable Entries, and setting aside what no Game needs. Each item has a default the Operator can change
before approving. Approving queues it as a Job, which the Operator can cancel between its steps. While a Repair of a
Source Set is queued, running or owes its follow-up Audit, no Audit or Build reads that Source Set, and the reverse. A
Repair deletes nothing but an original it converted, and only when Delete Originals After Conversion says so: what it
replaces or sets aside becomes a Kept File.
_Avoid_: Fix, rebuild, quarantine

**Build**:
Applying an approved Change Plan that creates or updates a Play Library from a Selection's files in a Source Set.
_Avoid_: Publish, export

**Managed Folder**:
A Play Library folder that the Operator registers for Title Select to build into, identified by its Root and a path
relative to it, and bound to one system, one Source Set and one Selection Profile. Of a Play Library, Title Select
writes only to Managed Folders; every other Play Library file is unmanaged and never touched. A system may have several.
_Avoid_: Output folder, target

**Path Template**:
A pattern of tokens, such as `{provider}/{dat}` or `{platform}`, that proposes the path of a Source Set or Managed
Folder at registration. It only proposes: a registered path never moves when a template changes.
_Avoid_: Naming scheme

**Incomplete**:
A Managed Folder whose current Build lacks a selected Game, because its group was unresolved and skipped, had no
source, failed, or is in conflict. Title Select shows why and how many.

**Drift**:
Any difference between a Managed Folder and its current Build: a built file changed or missing, or an entry Title
Select did not build. Drift is reported, never silently corrected; only an approved Change Plan acts on it. An entry
the Operator acknowledges is not reported again until it changes.
_Avoid_: Corruption (when meaning any difference)

**Storage Policy**:
How each folder stores a Game: as TorrentZip, one `<game>.zip` per Game, or loose, as its one ROM under the DAT's
name. It has two scopes, Source Sets and Play Libraries, each with a global default and per-system overrides, as a
Selection Profile has. The default is TorrentZip everywhere. Only a Game with one ROM can be stored loose; a Game with
more stays TorrentZip, and the plan says why. Every write of a Game into a folder, by a Repair, a Build or a fill,
follows it.
_Avoid_: Format setting, compression

**Conversion**:
Rewriting a Game stored in one format into the one the Storage Policy now gives its folder, with exactly the same
ROMs, as an item of an approved Change Plan: a Repair of a Source Set, or a Build of a Play Library. The original becomes a Kept File, unless Delete
Originals After Conversion is on.
_Avoid_: Recompress, migration

**Delete Originals After Conversion**:
A Storage Policy setting, global with per-system overrides and off by default, under which a Conversion deletes each
original once its converted file is verified and the original is proven unchanged since the Conversion read it. An
original that can't be proven so stays a Kept File.

**Kept File**:
A file or folder removed from a Managed Folder by a Build or a cleanup, from a Source Set by a Repair, or from an
Ingest Folder by a plan, set aside in that folder's retention area so that no tool or Audit scans it, until the
Operator approves deleting or restoring it.
_Avoid_: Trash, backup
