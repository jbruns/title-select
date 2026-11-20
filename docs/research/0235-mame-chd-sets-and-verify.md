# MAME CHD sets: layout, delta CHDs and `chdman verify`

Findings for #235, part of #233. Researched 2026-11-19 against sources pinned to a tag or commit: MAME `mame0289`
(tag commit `f34f0250`, source files fetched at the tag, ListXML asset `mame0289lx.zip` SHA-256 `d5d8f402…2817`
checked against the release's `SHA256SUMS`, software lists from the tag's source tarball) and igir at `c3b03daa`.
Behaviour was probed with chdman 0.289 (a packaged build reporting `0.289 (unknown)`) on synthetic images only: no
Source Set, Host or Operator data was read. Counts were computed locally from the pinned ListXML and software lists.
**Inference** marks conclusions that the sources do not state directly. It extends `docs/research/0105-mame-selection.md`
and the triage of #228; it doesn't repeat them.

Source paths below are relative to the MAME tree at `mame0289`.

## Answer in brief

- **Distribution:** CHD sets are distributed **merged**, one folder per parent: `<parent>/<disk>.chd`, with every
  clone's disks in its parent's folder. The main public catalogue offers only merged CHD sets, for machines and for
  software lists. Split and non-merged layouts also work in MAME, because MAME searches the machine's own folder and
  then each ancestor's (§1).
- **`merge=` disks:** a clone's `merge=` disk is the parent's disk, stored once in the parent's folder. In 0.289, 140 of
  1,402 machine disk entries carry `merge=`. No family has two different disks with the same name, so a merged folder
  never collides (§1).
- **Delta CHDs:** nothing in a DAT tells a delta CHD from a self-contained one. A delta CHD has the same combined
  SHA-1 as its self-contained copy. Only the header's parent SHA-1 tells them apart. The sources give no count of
  delta CHDs in distributed sets (§2).
- **Software lists:** MAME looks for `<rompath>/<list>/<software>/<disk>.chd`, then the parent item's folder, then
  `<rompath>/<software>/`, then a loose `<rompath>/<list>/<disk>.chd`. Software-list disks carry no `merge=` (§3).
- **`chdman verify`:** it decompresses the whole CHD. It checks the raw-data SHA-1 for every media type, and for
  v4 and v5 it then checks the combined SHA-1 over the raw SHA-1 and the checksummed metadata. **A mismatch exits 0.**
  So does an uncompressed CHD, which isn't verified at all. A delta CHD verifies only with `-ip <parent.chd>`;
  without it, it exits 1 with `Requires parent` (§4).
- **Header:** v5 stores the raw SHA-1 at bytes 64..84, the combined SHA-1 at 84..104 and the parent SHA-1 at 104..124.
  A parent SHA-1 that isn't all zeros marks a delta CHD. A MAME DAT's disk `sha1` is the combined SHA-1 at 84..104 (§5).

## 1. Machine CHD sets: merged, split, non-merged

**MAME's own definitions** (`docs/source/usingmame/aboutromsets.rst`, "Parents, Clones, Splitting, and Merging" and
"ROMs and CHDs") describe the three layouts for ROM sets and add that CHDs "should not be stored PKZIP or 7-Zip
archives as ROM images would be". So a disk sits as a loose `.chd` in a folder named for the set.

**How MAME finds a machine's disk** (`src/emu/romload.cpp`, `do_open_disk`, lines 160-238; `src/emu/driver.cpp`,
lines 31-39):

1. The search path is the machine's own name, then each ancestor up the `parent` chain (`cloneof`, and `romof`
   to a BIOS). Each entry is a folder under every `rompath` (`src/emu/emuopts.cpp:44`).
2. MAME opens `<name>.chd` in the first folder of that path that has it.
3. If no folder has it, MAME walks the ancestors' disk entries for one **with the same hashes but another name**,
   and tries that name. This is how a renamed `merge=` disk is found in the parent's folder.

So MAME runs a clone's disk from `<clone>/`, `<parent>/` or `<bios>/`. Merged, split and non-merged CHD sets all
work.

**What distributed sets look like.** The main public catalogue of MAME sets lists only merged CHD sets: "MAME 0.288
CHDs (merged)" with a 0.289 datfile, "MAME 0.289 Software List CHDs (merged)" and a rollback set
(`https://pleasuredome.github.io/pleasuredome/mame`, read 2026-11-19, not versioned). igir's arcade docs say of merged
sets: "most downloads found online will be in this format because it is the most space-efficient"
(`docs/usage/arcade.md:47` at `c3b03daa`). igir and RomVault both build and check all three layouts, CHDs included
(`docs/usage/arcade.md:42-47`; `https://romvault.com/mamesets`, which says scanning and fixing CHDs "will take a very
long time"). **Inference:** a Source Set should expect a merged CHD set, one folder per parent, and must still read
split and non-merged ones, since a ROM manager makes any of them.

**`merge=` disks in 0.289** (computed from `mame0289.xml`):

| Measure | Count |
| --- | --- |
| Machines with at least one `<disk>` | 1,260 (773 parents, 487 clones) |
| `<disk>` entries | 1,402 (971 good, 247 `baddump`, 184 `nodump`, 3 `optional`) |
| Entries with `merge=` | 140, 8 of them under a different name in the parent |
| Clones whose disks all carry `merge=` | 94 |
| Clones with no `merge=` disk | 375 |
| Clones with some of each | 18 |
| Non-clones with `merge=` (a disk shared with their BIOS through `romof`) | 16 |
| Clone disks without `merge=` (the clone's own disk) | 331 |
| Distinct disk SHA-1s; SHA-1s used by more than one machine | 1,033; 103 |
| Families in which two different disks share a name, in a merged folder | 0 |

The 184 `nodump` disks have no SHA-1. A merged set holds `merge=` disks once, under the merge name, in the parent's
folder. A split set leaves them out of the clone's folder. A non-merged set copies them into the clone's folder too.

## 2. Delta CHDs

MAME defines a delta CHD as one that "only stores the parts of the data that differ from its parent CHD". Only
clones, devices with a parent ROM device and clone software items may use one, and "the parent CHD file must be
present to use a delta CHD file" (`aboutromsets.rst`, "ROMs and CHDs"). MAME opens a delta CHD's parent by searching
the ancestors' disks for the parent SHA-1 (`romload.cpp`, `open_parent_disk`, lines 241-279).

**A delta CHD and its self-contained copy have the same combined SHA-1.** A synthetic probe made a 4 MiB parent image,
and a clone image that differs in 9 bytes. It compressed the clone twice, once self-contained and once as a delta
against the parent (`chdman createhd -op`). It then flattened the delta with `chdman copy -i delta.chd -ip parent.chd`:

| CHD | Size | Raw SHA-1 (64..84) | Combined SHA-1 (84..104) | Parent SHA-1 (104..124) |
| --- | --- | --- | --- | --- |
| self-contained clone | 4,196,553 | `48a50e69…6877` | `65ac6622…c67c` | zeros |
| delta clone | 4,301 | `48a50e69…6877` | `65ac6622…c67c` | `cb508586…debeb` (the parent's combined SHA-1) |
| delta flattened by `chdman copy` | — | `48a50e69…6877` | `65ac6622…c67c` | zeros |

So a DAT's `sha1` matches both. A tool that reads only the header's combined SHA-1, such as MAME's own audit, igir,
or a Quick Inventory, can't tell a delta CHD from a self-contained one without also reading bytes 104..124.

**How often distributed sets hold delta CHDs is not documented.** Neither MAME, igir, RomVault nor the set catalogue
says. **Inference:** the ROM managers that build and rebuild distributed sets copy CHDs as files and have no
documented step that makes deltas, so most CHDs in a distributed set are likely self-contained. That stays a guess
until a real set is read: the parent SHA-1 is a 20-byte read per file, so the first acceptance against real data can
count them cheaply.

**MAME's own audit hides a missing parent.** `media_auditor::audit_one_disk` reads only the header's combined SHA-1.
A delta CHD whose parent can't be found reports as "not found": "FIXME: A CHD with an invalid header or missing parent
is treated as not found" (`src/frontend/mame/audit.cpp:659-660`). When a machine starts, the same case is
`REQUIRES_PARENT` (`romload.cpp:1194-1195`).

## 3. Software-list CHDs

`make_software_searchpath` (`romload.cpp:137-157`) and `open_disk_image` for a software item (lines 1293-1314) search,
under each `rompath`:

1. `<list>/<software>/`, then `<list>/<parent>/` for each `cloneof` ancestor;
2. `<software>/`, then `<parent>/`, at the top of the rompath;
3. `<list>/` itself, "for loose disk images in software list directory" (line 1302). Loading at run time adds the same
   folder, "convenient for CD-only lists so you don't need an extra level of directories containing one file each"
   (lines 1395-1397).

The file name is `<disk name>.chd`. As for machines, a clone's disk is also found in a parent's folder under a
different name when the hashes match (`next_parent_software`, lines 118-134).

**Inference:** the canonical layout is `<list>/<software>/<disk>.chd`, as #228 assumed. A merged software-list CHD set
puts clone items' disks in `<list>/<parent>/`. MAME also accepts a loose `<list>/<disk>.chd`, which an Ingest Folder
may meet.

In 0.289's software lists (computed from the tag's software-list XML files):

| Measure | Count |
| --- | --- |
| Lists with disks | 64 |
| Items with disks | 10,456 (2,633 clones) |
| Disks | 11,563 (10,300 good, 1,250 `baddump`, 13 `nodump`) |
| Items with more than one disk, or disks in more than one part | 702 |
| Clone disks with the same SHA-1 as a disk of their parent | 26 |
| Disks carrying `merge=` | 0 |

Because software-list disks carry no `merge=`, a clone item's disk that matches its parent's is shared only by its
SHA-1. That affects how #234 counts a shared disk for software lists.

## 4. What `chdman verify` checks

`do_verify` (`src/tools/chdman.cpp:1784-1866`) is the same for every media type: hard disk, CD, DVD and laserdisc.

1. It opens the CHD, with the parent from `-ip`/`--inputparent` if given (lines 1788-1803). chdman doesn't search for
   a parent itself: its only parent option is `-ip` (`chdman.cpp:675`; `docs/source/tools/chdman.rst:104-119`). The
   option `-f`/`--fix` rewrites the header's SHA-1s, so it must never be passed on a Source Set.
2. **An uncompressed CHD is not verified:** `report_error(0, "No verification to be done; CHD is uncompressed")`
   (line 1807). With error 0, the process exits 0. The docs add that "the integrity of writable CHD files cannot be
   verified" (`chdman.rst:107-110`).
3. **It decompresses every hunk** of `logical_bytes` through `read_bytes`, 32 MiB at a time (`TEMP_BUFFER_SIZE`, line
   68), and hashes the result (lines 1815-1830). There is no thread option: the work runs on one core.
4. It compares that hash with the header's raw SHA-1. For a v3 CHD, which has no separate raw SHA-1, it uses the
   header SHA-1 (line 1811).
5. If the raw SHA-1 matches, it prints `Raw SHA1 verification successful!`. For v4 and later, it then recomputes the
   combined SHA-1 (`chd_file::compute_overall_sha1`, `src/lib/util/chd.cpp:1709-1751`) and compares it with the header.
   The combined SHA-1 hashes the raw SHA-1 followed by the sorted (tag, SHA-1) pairs of every metadata entry flagged
   `CHD_MDFLAGS_CHECKSUM`. That is the hard disk's `GDDD` geometry, a CD's track table, the `DVD ` tag, or a
   laserdisc's A/V metadata.

So `verify` proves that the decompressed data and the checksummed metadata match the header's SHA-1s. Since a MAME DAT
lists the combined SHA-1 (§5), a verified CHD whose header combined SHA-1 equals the DAT's has proven its data.

**Exit codes and output** (chdman 0.289, synthetic images; the banner line `chdman - MAME Compressed Hunks of Data
(CHD) manager 0.289 …` always comes first on stdout):

| Case | stdout after the banner | stderr | Exit |
| --- | --- | --- | --- |
| Good HD, CD or DVD CHD | `Raw SHA1 verification successful!`, `Overall SHA1 verification successful!` | progress only | 0 |
| Raw SHA-1 in header wrong | — | `Error: Raw SHA1 in header = …`, `actual SHA1 = …` | **0** |
| Combined SHA-1 in header wrong | `Raw SHA1 verification successful!` | `Error: Overall SHA1 in header = …`, `actual SHA1 = …` | **0** |
| Data corrupted | — | `Error reading CHD file (…): Decompression error`, `Fatal error occurred: 1` | 1 |
| Uncompressed CHD | — | `No verification to be done; CHD is uncompressed`, `Fatal error occurred: 0` | **0** |
| Delta CHD, no `-ip` (parent beside it or not) | — | `Error reading CHD file (…): Requires parent`, `Fatal error occurred: 1` | 1 |
| Delta CHD, `-ip` the right parent | both success lines | progress only | 0 |
| Delta CHD, `-ip` a wrong parent | — | `Error opening CHD file (…): Invalid parent`, `Fatal error occurred: 1` | 1 |
| Not a CHD | — | `Error opening CHD file (…): Invalid file` | 1 |
| Missing file | — | `Error opening CHD file (…): No such file or directory` | 1 |

The exit codes come from `main` (`chdman.cpp:3524-3541`): a handler that returns gives 0, and `report_error(n, …)`
throws `fatal_error(n)`, whose `n` becomes the exit code (lines 950-959). The mismatch branches (lines 1834-1866) print
to stderr and return normally, so they exit 0. **A wrapper must not trust the exit code alone.** It should require
exit 0 *and* `Overall SHA1 verification successful!` on stdout for v4 and v5. To get an independent answer, it can
also compare the DAT's SHA-1 against the header before running chdman.

The laserdisc case was not probed, since it needs an A/V source. The source shows the same path: `read_bytes` through
the A/V codec, with the A/V metadata in the combined SHA-1.

**Cost.** Verify reads and decompresses the whole logical image on one core. On the probe machine (Apple M4 Max), a
1 GiB hard-disk image (half random, half zeros, 512 MiB as a CHD) verified in 6.1 s wall time and 6.0 s user time,
about 170 MiB/s of logical data. `shasum` read the same CHD file in 0.5 s. LZMA- or FLAC-heavy real disks will
decompress more slowly than this mostly stored probe. **Inference:** a Full Audit of a merged CHD set, which runs
from hundreds of GiB to a few TiB, costs hours of CPU. The existing chdman time limit (`crates/api/src/settings.rs`)
should scale with logical size here too.

**Old versions.** `chd_file::open_common` reads v3, v4 and v5 (`chd.cpp:2576-2579`) and refuses others with the error
`UNSUPPORTED_VERSION`, whose message is `Mismatched DIFF and CHD or unsupported CHD version`. So `verify` works on v3 and v4 read-only: v3 checks only the raw SHA-1, and v4 checks both.
**Inference:** a v4 CHD's header combined SHA-1 (bytes 48..68) is the same kind of hash as v5's, so MAME's audit
accepts a v4 CHD whose SHA-1 matches. A v3 header holds only the raw SHA-1 (bytes 80..100), so it won't match a DAT's
combined SHA-1 when the media has checksummed metadata. #228's v5-only rule is stricter than MAME, which is a choice,
not a constraint.

## 5. Header fields

From the header comments in `src/lib/util/chd.h:86-165` and the parsers (`chd.cpp:1929-1931`, `1992-1994`,
`2047-2049`), all big-endian, signature `MComprHD` at 0, version at 12..16:

| Version | Header length | Raw SHA-1 | Combined SHA-1 (`sha1()`) | Parent SHA-1 | Delta when |
| --- | --- | --- | --- | --- | --- |
| v3 | 120 | — (the SHA-1 at 80..100 is raw only) | 80..100 | 100..120 | flags (16..20) bit 0 set |
| v4 | 108 | 88..108 | 48..68 | 68..88 | flags (16..20) bit 0 set |
| v5 | 124 | 64..84 | 84..104 | 104..124 | parent SHA-1 not all zeros ("If parentsha1 != 0, we have a parent (no need for flags)", `chd.h:164`) |

A v5 CHD is uncompressed when `compressors[0]` (bytes 16..20) is 0 (`chd.h:165`). That is the case `verify` skips.

**The DAT `sha1` is the combined SHA-1.** MAME's audit compares the DAT's disk hashes with `chd_file::sha1()`
(`audit.cpp:662-671`), and so does machine start (`romload.cpp:1208-1218`). `sha1()` reads `m_sha1_offset`, which is
84 for v5 (`chd.cpp:2047`). `chdman info` prints it as `SHA1:`, the raw one as `Data SHA1:` and a delta's parent as
`Parent SHA1:`. The parent SHA-1 is the parent's *combined* SHA-1 (`chd.h:161`). In the probe it equals the parent
CHD's bytes 84..104, so it is also the parent disk's DAT `sha1`, which lets an Audit name the missing parent.

## Consequences for the open tickets

- **#236 (set layouts):** merged, one folder per parent, is what is distributed. MAME's search rule (own folder, then
  ancestors, then a hash match under another name) is what Title Select's Audit must mirror to place a clone's disk.
  A loose `<list>/<disk>.chd` is a valid software-list place.
- **#234 (how dependencies count):** machine `merge=` disks are named in the DAT, but software-list disks shared with a
  parent are shared only by SHA-1 (26 cases in 0.289). 16 non-clone machines share a disk with their BIOS through
  `romof`.
- **The `chdman verify` slice (#228):** check stdout for `Overall SHA1 verification successful!`, not only the exit
  code. Treat "CHD is uncompressed" as not verified. Never pass `-f`. Pass `-ip` only if delta CHDs are ever accepted.
