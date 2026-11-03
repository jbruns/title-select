# Games Organizer: taking over Downloader output

Findings for #4, part of #3. Researched 2026-10-28 against primary sources pinned to a tag or commit: Sonarr
`v4.0.20.3014`, Radarr `v6.4.4.10685`, qBittorrent `release-5.3.0`, SABnzbd `5.1.3` and its 5.1 docs, OpenZFS
`zfs-2.4.4` and Linux `v6.12`. Wiki pages that are not tagged are cited at the commit read. No Downloader or Host
was queried. **Inference** marks conclusions that the sources do not state directly.

The starting point is the home-ops spike
([0734-games-organizer-spike.md](https://github.com/jbruns/home-ops/blob/main/docs/research/0734-games-organizer-spike.md)).
Vocabulary comes from this repo's `GLOSSARY.md` and the home-ops
[GLOSSARY.md](https://github.com/jbruns/home-ops/blob/main/GLOSSARY.md) (Organizer, Downloader, Ingest Folder).

## Recommendation

**Partly in scope.** The Organizer takes over Downloader output by treating a Downloader's completed folder as an
**explicitly registered local source**, the same thing #3 already needs for repair. It does not use a Downloader API
in the pilot.

- **From slice 3 (repair Change Plan):** the Operator can register a Downloader's completed category folder, or a
  games Ingest Folder, as a read-only local source. The Organizer assesses each new item against an explicit DAT
  Revision, reading payloads first with Payload Evidence, exactly as an Audit reads a Verified Set. Matches that fill
  a Missing Game or replace a Bad or Unreadable Entry are offered in a repair Change Plan. Nothing joins a Verified
  Set without approval.
- **From slice 6 (Additions):** items that match no ROM can be offered as Additions. They join the
  Library only, never a Verified Set.
- **Deferred until after slice 8:** qBittorrent and SABnzbd API adapters, used only to label items with their
  Downloader state and to show download progress next to ingestion progress.
- **Out of scope:** adding downloads or managing categories (Requester behaviour; #3: "The Service does not acquire
  games"), auto-approval, hardlinks, and writing to or deleting from Downloader folders. Cleanup after seeding stays
  with the Downloader.

Why:

- Latency does not decide the design. Hashing every payload dominates ingestion time (§2), so a 1-minute API poll,
  as Sonarr uses (§1), would gain nothing that matters.
- A folder needs no Downloader credential. Neither qBittorrent nor SABnzbd offers a read-only API credential (§1),
  and a Protected Service should not hold full control of a Downloader just to learn that a file is finished.
- One mechanism covers qBittorrent, SABnzbd and manual drops into an Ingest Folder, and it reuses the Audit engine,
  whose input is already a folder plus a DAT Revision.
- Incompleteness is handled by convention and by revalidation (§1), and every write happens later, inside an
  approved Change Plan that revalidates its inputs anyway (#3).

No proposal here contradicts ADR 0001 or ADR 0002. Auto-approval would contradict the glossary ("It changes only
through an approved repair Change Plan") and #3. It is rejected, not deferred (§4).

## 1. How does the Organizer learn that a file is finished?

### What existing Organizers do

Sonarr and Radarr call this Completed Download Handling. They poll the download client's API for items in their
category, and the client reports where each item ended up
([Servarr wiki, sonarr/settings.md L845-L866](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/settings.md#L845-L866)).

- **Polling interval.** Sonarr refreshes monitored downloads every minute
  ([TaskManager.cs L68-L73](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Jobs/TaskManager.cs#L68-L73)),
  with a 5-second debounce
  ([DownloadMonitoringService.cs L53](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/TrackedDownloads/DownloadMonitoringService.cs#L53)).
  Radarr makes the interval configurable, with a minimum of 1 minute
  ([TaskManager.cs L201-L211](https://github.com/Radarr/Radarr/blob/v6.4.4.10685/src/NzbDrone.Core/Jobs/TaskManager.cs#L201-L211)).
- **Category is the contract.** Sonarr queries qBittorrent with `torrents/info?category=`
  ([QBittorrentProxyV2.cs L99-L102](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrentProxyV2.cs#L99-L102)).
  It warns about, and does not import, a completed item that "wasn't grabbed by Sonarr and not in a category"
  ([CompletedDownloadService.cs L84-L88](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/CompletedDownloadService.cs#L84-L88)).
- **Completed path.** For qBittorrent, Sonarr uses `content_path` (API 2.6.1 and later)
  ([QBittorrent.cs L314-L324](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrent.cs#L314-L324)).
  For SABnzbd it uses the history item's `storage`, and treats any history status other than Completed or Failed
  ("Verifying/Moving etc") as still downloading
  ([Sabnzbd.cs L138-L165](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/Sabnzbd/Sabnzbd.cs#L138-L165)).
- **Remote Path Mappings.** When the client and the Organizer see different paths, a mapping translates them. The
  wiki calls this a "dumb" search and replace
  ([settings.md L888-L897](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/settings.md#L888-L897)).
  Sonarr suggests a mapping when a reported path does not exist locally
  ([CompletedDownloadService.cs L295-L312](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/CompletedDownloadService.cs#L295-L312)).
- **Import modes.** The modes are Auto, Move and Copy
  ([ImportMode.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/EpisodeImport/ImportMode.cs)).
  Auto moves only when the client says the item can be moved, and otherwise copies
  ([DownloadedEpisodesImportService.cs L204-L206](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/DownloadedEpisodesImportService.cs#L204-L206)).
  For qBittorrent, an item can be moved or removed only when Remove Completed is on, the torrent is stopped, and its
  seeding limit has been reached
  ([QBittorrent.cs L242-L245](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrent.cs#L242-L245)).
  So a seeding torrent is copied, or hardlinked when "Use Hardlinks instead of Copy" is on
  ([EpisodeFileMovingService.cs L100-L107](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/EpisodeFileMovingService.cs#L100-L107)).
  The wiki says this leaves torrents in place to seed, at the cost of "double space" when hardlinks are unavailable
  ([settings.md L845-L866](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/settings.md#L845-L866)).

### What the Downloaders expose

**qBittorrent** (WebUI API v2,
[WebUI-API-(qBittorrent-5.0)](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-5.0)), wiki commit
`31ea427`):

- `torrents/info` filters by `filter` and `category`. It returns `state`, `progress`, `amount_left`,
  `completion_on`, `save_path`, `content_path` and `category`. `sync/maindata` with a `rid` returns incremental
  changes. Categories can carry their own `savePath`.
- Preferences `temp_path_enabled` and `temp_path` keep incomplete torrents in a separate folder, so a torrent reaches
  its save path only when it completes. `recheck_completed_torrents` rehashes on completion. `max_ratio` and
  `max_seeding_time` end seeding.
- The wiki's state list still says `pausedUP`. The 5.x source emits `stoppedUP`
  ([serialize_torrent.cpp L44-L85](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/webui/api/serialize/serialize_torrent.cpp#L44-L85)).
  Sonarr maps both
  ([QBittorrent.cs L247-L311](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrent.cs#L247-L311)).
- Authentication is a session cookie or, from 5.2.0, a single API key sent as a Bearer token
  ([API-Key-Authentication-(≥v5.2.0)](https://github.com/qbittorrent/qBittorrent/wiki/API-Key-Authentication-(%E2%89%A5v5.2.0))).
  The page describes no scopes. **Inference:** any credential that can read the queue can also add, delete and
  move torrents.
- Push hook: "Run external program on torrent finished" starts a detached process with placeholders, including `%F`
  content path, `%D` save path, `%L` category and `%I` info hash
  ([application.cpp L588-L600](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/app/application.cpp#L588-L600),
  [L720-L742](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/app/application.cpp#L720-L742),
  [L786-L792](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/app/application.cpp#L786-L792)).
  The hook is fire-and-forget: qBittorrent only logs whether the process started.

**SABnzbd** ([API](https://sabnzbd.org/wiki/configuration/5.1/api), docs commit `91f6b6d`):

- `mode=queue` covers active downloads. `mode=history` takes `cat`, `nzo_ids`, `status` and
  `last_history_update` filters. Each history item has `status` (Completed or Failed), `storage` (final location),
  `path` (temporary location) and a `stage_log`. Post-processing reports Queued, QuickCheck, Verifying, Repairing,
  Fetching, Extracting, Moving and Running. `mode=get_cats` lists categories.
- Authentication is the full `apikey`. The separate NZB key can only add jobs. **Inference:** there is no
  read-only key.
- Files download into the Temporary Download Folder, then are verified and extracted into the Complete folder
  ([folders](https://sabnzbd.org/wiki/configuration/5.1/folders)). A category can set its own folder or full path
  ([categories](https://sabnzbd.org/wiki/configuration/5.1/categories)). Unpacking writes to an `_UNPACK_`-prefixed
  folder
  ([postproc.py L793-L798](https://github.com/sabnzbd/sabnzbd/blob/5.1.3/sabnzbd/postproc.py#L793-L798)), and failed
  jobs are renamed with `_FAILED_`
  ([postproc.py L556-L562](https://github.com/sabnzbd/sabnzbd/blob/5.1.3/sabnzbd/postproc.py#L556-L562)).
- Push hooks: a post-processing script receives `SAB_COMPLETE_DIR`, `SAB_PP_STATUS`, `SAB_CAT` and `SAB_NZO_ID`, and
  "will always run, even when the job has completed with errors"
  ([post-processing scripts](https://sabnzbd.org/wiki/configuration/5.1/scripts/post-processing-scripts)).
  Notification scripts receive `complete` and `failed` events
  ([notification scripts](https://sabnzbd.org/wiki/configuration/5.1/scripts/notification-scripts)).

### Folder or API

| | Watched folder (registered local source) | Downloader API |
|---|---|---|
| Credential | None; read-only mount | Full-control key for each Downloader |
| Covers manual drops | Yes (Ingest Folder) | No |
| Completeness signal | Folder conventions plus revalidation | Explicit (`stoppedUP`/`uploading`, history `Completed`) |
| Seeding status | Not visible | Visible |
| Extra code | One scanner | One adapter per Downloader, with Contract Tests ([home-ops ADR 0018](https://github.com/jbruns/home-ops/blob/main/docs/adr/0018-owned-applications-live-in-their-own-repos.md)) |
| Path mapping | The Organizer's own mount | Remote Path Mappings |

**Inference:** a folder is enough for the pilot if three conventions hold.

- qBittorrent keeps incomplete torrents in `temp_path`, so the completed category folder holds only completed
  torrents.
- The Organizer ignores SABnzbd's `_UNPACK_` and `_FAILED_` folders.
- An item counts as settled only after its size and modification time are unchanged across two scans. The 1a Audit
  already reports a file that changes while it is read. Apply revalidates every input before writing (#3), so a
  premature assessment can never become a premature write.

The push hooks are not used. Both run inside the Downloader's container and would need either a network path to the
Organizer or shared state, and qBittorrent's hook is fire-and-forget. **Inference:** for a Protected Service, a scan
is simpler than an inbound hook endpoint.

Where the games download and ingest folders live is a home-ops deployment question. ADR 0009 gives each Library its
own `usenet/`, `torrents/` and `ingest/` folders and says Organizers copy rather than hardlink from `torrents/`
([ADR 0009](https://github.com/jbruns/home-ops/blob/main/docs/adr/0009-library-isolation-by-role.md)). No games
download folders exist yet.

## 2. How can the Organizer clearly expose ingestion progress?

### What Sonarr and Radarr show

- Each tracked download has a state: Downloading, ImportBlocked, ImportPending, Importing, Imported, FailedPending,
  Failed or Ignored. It also has a status (Ok, Warning or Error) and a list of status messages
  ([TrackedDownload.cs L49-L66](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/TrackedDownloads/TrackedDownload.cs#L49-L66)).
  Radarr has the same enum
  ([TrackedDownload.cs L49](https://github.com/Radarr/Radarr/blob/v6.4.4.10685/src/NzbDrone.Core/Download/TrackedDownloads/TrackedDownload.cs#L49)).
- An item that cannot be imported automatically becomes ImportBlocked, with messages saying why, and raises a "manual
  interaction required" event
  ([CompletedDownloadService.cs L273-L288](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/CompletedDownloadService.cs#L273-L288)).
  The queue offers manual import for it
  ([activity.md L15-L44](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/activity.md#L15-L44)).
- The queue "is not stored anywhere within the application, but is updated via your Download Client's API
  responses", and it looks only 60 items deep for Usenet (same source).
- File verification after a copy checks only the size
  ([DiskTransferService.cs L473-L520](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Common/Disk/DiskTransferService.cs#L473-L520)).
  There is no hashing stage to show.

### What Title Select needs instead (inference)

In Sonarr, waiting for a person is a failure state (ImportBlocked). In Title Select it is the normal resting state,
because every change needs an approved Change Plan. Hashing also adds a long, measurable stage that Sonarr does not
have. So the unit of progress is a durable **incoming item**: one file or folder seen in a registered source. It
outlives the jobs that act on it.

| State | Meaning | Shows | Moves on when |
|---|---|---|---|
| Seen | Found by a scan | Source, path, size | Unchanged across two scans |
| Assessing | Payloads being read and hashed | Bytes read / total, throughput, current Entry | Every Entry has Evidence |
| Assessed | Findings recorded against a named DAT Revision | Findings, Evidence Level, Game each Entry matches, proposed classification | The Operator includes it in a plan, or dismisses it |
| Proposed | In a pending repair Change Plan or Addition | Plan link, target paths, conflicts | Approved or rejected |
| Applying | Staging and verifying the copy | Bytes written / verified | Destination Evidence matches |
| Applied | Now an Entry of a Verified Set, or an Addition | Destination, provenance | — |

Side states are never hidden:

- **Unreadable**, with the recorded reason (the existing Finding).
- **Failed**, with the job error and whether it can be retried.
- **Stale**, when the source changed after assessment, so Evidence must be gathered again (#3: "Resume scan work only
  when its evidence remains valid").
- **Gone**, when the Downloader removed the source before apply.
- **Already held**, when every payload matches an Entry the Verified Set already has. It can be dismissed with no
  change.
- **Dismissed** by the Operator.

Roughly, Sonarr's ImportPending maps to Assessed, Importing to Applying and Imported to Applied. Every row carries
Sonarr-style status messages saying why it is in that state and what would move it on. Unlike Sonarr's queue, the
state persists across restarts (slice 1c).

**Cost of assessment (inference).** Assessment reads each payload once, so its time is roughly total bytes divided
by the read and hash rate allowed under slice 7's resource limits. Every ROM in the Atari DAT together totals 37.4 MiB
(spike §2), so a cartridge-era item is seconds of work. Disc-based systems are where the Assessing bar matters.

## 3. Seeding: copy, never hardlink

#3 says: "Do not use shared writable hardlinks between preservation sources and managed output."

### Cost of a byte copy (inference unless cited)

For an accepted item of size S:

- **Assessment:** one read of S.
- **Apply:** reading the source while copying lets the copy pass re-hash the source, which revalidates it. Writing
  S, then reading the destination back once, gives the new Entry its own Payload Evidence. That is two reads and one
  write.
- **Hashing during the copy is not enough on its own.** It proves what was read, not what was stored. The
  destination read-back is what gives the Verified Set Entry its Payload Evidence. Sonarr, by contrast, verifies
  only the size (§2).
- **Space:** each accepted byte is stored twice until the Downloader removes its copy at the end of seeding. Daily
  Periodic Snapshots of the games dataset (ADR 0009) keep old blocks longer. Only accepted items are copied, because
  copying happens at apply time, not at detection.
- **Repacking** into one ZIP per game (spike §7) writes new bytes, so it is always a full copy. Its Evidence comes
  from reading the new ZIP.

### ZFS block cloning

- `block_cloning` lets `copy_file_range` share existing blocks instead of writing new ones, using "no additional
  space". It works across datasets "under some conditions (like equal recordsize, the same master encryption key,
  etc.)"
  ([zpool-features.7 L387-L403](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zpool-features.7#L387-L403);
  [brt.c L150-L162](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/zfs/brt.c#L150-L162)).
  Pool properties `bcloneused` and `bclonesaved` report the savings
  ([zpoolprops.7 L68-L72](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zpoolprops.7#L68-L72)).
- On Linux, ZFS's `copy_file_range` tries a clone. If cloning is unsupported, or `zfs_bclone_enabled` is off, it
  falls back to a byte copy without reporting an error
  ([zpl_file_range.c L57-L123](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/os/linux/zfs/zpl_file_range.c#L57-L123)).
  `zfs_bclone_enabled` defaults to off in 2.2.x
  ([zfs-2.2.7 zfs_vnops.c L66](https://github.com/openzfs/zfs/blob/zfs-2.2.7/module/zfs/zfs_vnops.c#L66)) and to on
  from 2.3.0
  ([zfs-2.4.4 zfs_vnops.c L69](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/zfs/zfs_vnops.c#L69);
  [zfs.4 L1435-L1449](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man4/zfs.4#L1435-L1449)).
- Across datasets, the method matters. `FICLONE` returns `EXDEV` unless both files are on the same superblock
  ([remap_range.c L376-L385](https://github.com/torvalds/linux/blob/v6.12/fs/remap_range.c#L376-L385);
  [ioctl_ficlone(2)](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html)). Each ZFS dataset is a separate
  filesystem, so `FICLONE` cannot clone across datasets. `copy_file_range` allows a cross-filesystem copy when both
  sides use the same implementation
  ([read_write.c L1519-L1537](https://github.com/torvalds/linux/blob/v6.12/fs/read_write.c#L1519-L1537);
  [copy_file_range(2)](https://man7.org/linux/man-pages/man2/copy_file_range.2.html)).
  Sonarr's reflink path on ZFS and btrfs falls back to a byte copy when the reflink fails
  ([DiskTransferService.cs L336-L358](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Common/Disk/DiskTransferService.cs#L336-L358)).
- Rust's `std::fs::copy` uses `copy_file_range` on Linux where possible
  ([std::fs::copy](https://doc.rust-lang.org/std/fs/fn.copy.html)). **Inference:** the Organizer would get a block
  clone from ZFS without special code. It must still read the destination for Evidence; that read covers the same
  blocks the clone shares with the source.

**Is a block clone a "shared writable hardlink"? No (inference from the sources).** A hardlink is one file under two
names, so a write through either name changes both. A clone is a second file whose blocks are extra references to
the same data ([brt.c L45-L50](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/zfs/brt.c#L45-L50)); for clones,
writes "remain private to the file being written"
([ioctl_ficlone(2)](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html)). A Downloader rewriting or deleting
its file cannot change the Verified Set's file. So a clone meets #3's intent, and ADR 0009 already prefers it ("on
ZFS the copy should be a block clone").

Caveat: a clone shares physical blocks, so it is not an independent second copy on disk. That is fine here because
the Downloader's file is not a preservation copy. Snapshots and backups of the games dataset remain the preservation
copies.

Clone conditions to confirm on the Host before relying on them: same pool, equal recordsize, same encryption key,
`block_cloning` active and `zfs_bclone_enabled` on. A clone is an optimisation: if any condition fails, the result
is a correct byte copy that costs double space.

ADR 0009 tension (inference): to get a clone, the download folder must be on the same pool as the games dataset. To
keep the games dataset's snapshots free of download churn, it should be on a different dataset. Same pool, different
dataset, same recordsize satisfies both.

## 4. How does an incoming file become a Verified Set member, a repair source or an Addition?

| Step | What happens | Approval |
|---|---|---|
| Register a source | Operator configuration: path, kind (Downloader folder or Ingest Folder), read-only | None (configuration; changes no Verified Set or Library) |
| Assess | Read-only. Findings and Evidence against an explicit DAT Revision, by default the one from the target Verified Set's latest Audit | None (writes nothing, like an Audit under ADR 0002) |
| Fill a Missing Game, or replace a Bad or Unreadable Entry | Proposed as a repair Change Plan. The displaced Entry is quarantined (#3) | **Repair Change Plan** |
| Rename or repack Misnamed or Conflicting incoming payloads into DAT names | Part of the same repair Change Plan | **Repair Change Plan** |
| Publish the result into the Library | A separate publish Change Plan (slice 5) | **Publish Change Plan**, separate from repair (glossary) |
| Extra (no ROM matched) | Offered as an Addition. Library only, never the Verified Set | **Addition approval** (slice 6) |
| Already held | No change | None |

Rules this respects:

- **ADR 0002 and the glossary:** an Audit never writes, and a Verified Set "changes only through an approved repair
  Change Plan". Assessing a source writes nothing. Membership happens only at apply time.
- **#3:** repairs use "adequately verified payloads from explicitly registered local sources", and apply revalidates
  inputs, so a source that changed after approval is caught (Stale).
- **Addition:** approval "never converts a DAT mismatch into an official match" (#3). An unmatched download
  is never added to a Verified Set.
- **The DAT Revision is explicit.** If an item matches only a newer revision, the Operator must adopt that revision
  first (#3: "Adoption is explicit").

**Flag: auto-approval contradicts the glossary and #3** (not ADR 0002, which covers Audits). An "import
automatically when Matched" option, like Sonarr's default, would let a Verified Set change without an approved repair
Change Plan. It is out of scope; changing that would need an ADR.

**Glossary gaps** (for `/domain-modeling`, not decided here):

- "Audit" is defined only as an assessment "of a Verified Set". Assessing a registered source uses the same engine,
  Findings and Evidence, but is not an Audit by that definition. Either widen Audit or add a term.
- There is no term for an **incoming item** (§2).
- The home-ops Organizer definition says it takes a file over "into its own Library". Here the first target is a
  Verified Set, through repair, and the Library follows only through a separate publish plan. The Organizer
  definition (#4) may need to say so.
- "Repair source" is used informally ("explicitly registered local sources" in #3) but is not defined.

## 5. What changes in the job engine or the pilot slicing if decided now?

Little now, and it is cheap to keep open. Recommended changes:

- **1a/1b:** none. The Audit already takes a folder and a DAT path
  ([main.rs](../../src/main.rs), [`lib.rs` `audit`](../../src/lib.rs)). Keep that input shape, a folder plus a DAT
  Revision, rather than a Verified Set ID, so a registered source can use the same engine.
- **1c (durable jobs):**
  - Model per-item state (incoming items, §2) separately from jobs, since an item waits days in Proposed while jobs
    come and go.
  - Allow more job kinds than Audit: scan source, assess item, apply plan.
  - Report progress in bytes as well as Entries.
  - Make every scanning and hashing job count against slice 7's shared resource limits.
- **3 (repair Change Plan):** add registered local sources (already implied by #3), the "fill Missing, replace
  Bad/Unreadable" proposal, quarantine of displaced Entries, Stale/Gone revalidation, and destination read-back
  Evidence. Copy with `copy_file_range` so ZFS can block-clone.
- **6 (Additions):** accept an Extra incoming item as an Addition candidate.
- **8 (UI):** the incoming-items view in §2.
- **After the pilot:** qBittorrent and SABnzbd adapters with Contract Tests, used only to label items with their
  Downloader state. A home-ops deployment ticket for games download folders and read-only mounts, alongside 1c's
  deployment ticket.

Deciding later instead risks a 1c job model with only Audit jobs keyed by Verified Set and no durable per-item
state. Retrofitting that is the main cost this research avoids.

## Open questions

- Which OpenZFS version runs on the Host, is `block_cloning` active, and is `zfs_bclone_enabled` on?
- Where do games download folders live (home-ops deployment), and with what recordsize?
- Is Addition approval a Change Plan, or a separate approval flow? #3 says "a simple approval flow".
- Should assessment be a widened Audit or a new term (glossary gap above)?

## Sources

- Sonarr `v4.0.20.3014`: [TaskManager.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Jobs/TaskManager.cs),
  [DownloadMonitoringService.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/TrackedDownloads/DownloadMonitoringService.cs),
  [TrackedDownload.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/TrackedDownloads/TrackedDownload.cs),
  [CompletedDownloadService.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/CompletedDownloadService.cs),
  [QBittorrent.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrent.cs),
  [QBittorrentProxyV2.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrentProxyV2.cs),
  [Sabnzbd.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/Download/Clients/Sabnzbd/Sabnzbd.cs),
  [ImportMode.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/EpisodeImport/ImportMode.cs),
  [DownloadedEpisodesImportService.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/DownloadedEpisodesImportService.cs),
  [EpisodeFileMovingService.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Core/MediaFiles/EpisodeFileMovingService.cs),
  [DiskTransferService.cs](https://github.com/Sonarr/Sonarr/blob/v4.0.20.3014/src/NzbDrone.Common/Disk/DiskTransferService.cs).
- Radarr `v6.4.4.10685`: [TrackedDownload.cs](https://github.com/Radarr/Radarr/blob/v6.4.4.10685/src/NzbDrone.Core/Download/TrackedDownloads/TrackedDownload.cs),
  [TaskManager.cs](https://github.com/Radarr/Radarr/blob/v6.4.4.10685/src/NzbDrone.Core/Jobs/TaskManager.cs).
- Servarr Wiki at `f0dd056`: [sonarr/settings.md](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/settings.md),
  [sonarr/activity.md](https://github.com/Servarr/Wiki/blob/f0dd05691d1a3ee92255d9c0342e2d0b0d0d006a/sonarr/activity.md).
- qBittorrent `release-5.3.0`: [application.cpp](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/app/application.cpp),
  [serialize_torrent.cpp](https://github.com/qbittorrent/qBittorrent/blob/release-5.3.0/src/webui/api/serialize/serialize_torrent.cpp).
  Wiki at `31ea427`: [WebUI API (5.0)](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-5.0)),
  [API key authentication](https://github.com/qbittorrent/qBittorrent/wiki/API-Key-Authentication-(%E2%89%A5v5.2.0)).
- SABnzbd `5.1.3`: [postproc.py](https://github.com/sabnzbd/sabnzbd/blob/5.1.3/sabnzbd/postproc.py). Docs 5.1 at
  `91f6b6d`: [API](https://sabnzbd.org/wiki/configuration/5.1/api),
  [folders](https://sabnzbd.org/wiki/configuration/5.1/folders),
  [categories](https://sabnzbd.org/wiki/configuration/5.1/categories),
  [post-processing scripts](https://sabnzbd.org/wiki/configuration/5.1/scripts/post-processing-scripts),
  [notification scripts](https://sabnzbd.org/wiki/configuration/5.1/scripts/notification-scripts).
- OpenZFS `zfs-2.4.4`: [zpool-features.7](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zpool-features.7),
  [zpoolprops.7](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zpoolprops.7),
  [zfs.4](https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man4/zfs.4),
  [brt.c](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/zfs/brt.c),
  [zpl_file_range.c](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/os/linux/zfs/zpl_file_range.c),
  [zfs_vnops.c](https://github.com/openzfs/zfs/blob/zfs-2.4.4/module/zfs/zfs_vnops.c);
  `zfs-2.2.7` [zfs_vnops.c](https://github.com/openzfs/zfs/blob/zfs-2.2.7/module/zfs/zfs_vnops.c).
- Linux `v6.12`: [fs/remap_range.c](https://github.com/torvalds/linux/blob/v6.12/fs/remap_range.c),
  [fs/read_write.c](https://github.com/torvalds/linux/blob/v6.12/fs/read_write.c); man-pages
  [ioctl_ficlone(2)](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html),
  [copy_file_range(2)](https://man7.org/linux/man-pages/man2/copy_file_range.2.html).
- Rust: [std::fs::copy](https://doc.rust-lang.org/std/fs/fn.copy.html).
- home-ops: [GLOSSARY.md](https://github.com/jbruns/home-ops/blob/main/GLOSSARY.md),
  [ADR 0009](https://github.com/jbruns/home-ops/blob/main/docs/adr/0009-library-isolation-by-role.md),
  [ADR 0018](https://github.com/jbruns/home-ops/blob/main/docs/adr/0018-owned-applications-live-in-their-own-repos.md),
  [0734 spike](https://github.com/jbruns/home-ops/blob/main/docs/research/0734-games-organizer-spike.md).
- This repo: `GLOSSARY.md`, ADR 0001, ADR 0002, #3.
