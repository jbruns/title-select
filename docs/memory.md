# Memory

How much memory Title Select uses, and how to measure it again. The Host's memory limit is set from these peaks: twice
the largest, rounded up.

## Method

The container's cgroup v2 `memory.peak` records the highest memory use since the cgroup was created. On Linux 6.12, a
write to `memory.peak` resets the peak only for the open file that wrote it. A reading therefore has to come from a
process that holds the file open for the whole measurement.

1. Find the container's cgroup folder: `/sys/fs/cgroup/docker/<container id>`.
2. Run a small helper container with that folder mounted read-write at `/cg`. The helper:
   - opens `/cg/memory.peak` and keeps it open;
   - writes `0` to it whenever it receives `SIGUSR1`, which resets its peak;
   - prints the peak every 5 seconds when it changes.

   ```python
   import os, time, signal
   fd = os.open("/cg/memory.peak", os.O_RDWR)
   def rd(): return int(os.pread(fd, 64, 0).decode())
   def reset(*a):
       os.write(fd, b"0"); print(time.strftime("%H:%M:%S"), "RESET", rd(), flush=True)
   signal.signal(signal.SIGUSR1, reset)
   reset()
   last = 0
   while True:
       time.sleep(5)
       p = rd()
       if p != last:
           print(time.strftime("%H:%M:%S"), "peak", p, "MiB", round(p / 1048576, 1), flush=True)
           last = p
   ```

3. Before each job you measure, send the helper `SIGUSR1` (`docker kill -s USR1 <helper>`). After the job, read the
   helper's log and match the timestamps to the job's start and end in the jobs list.
4. Remove the helper when you're done.

`memory.peak` counts page cache charged to the container as well as the service's own memory. The page cache holds
mostly the store, and the kernel reclaims it under the limit, so a peak is an upper bound. `memory.stat` gives the
split: at rest after these measurements, 14 MiB was anonymous memory and 20 MiB page cache.

## Results

### 0.4.0

Measured on version 0.4.0, with a 512M limit and one CPU, when selection ran Retool in Python, in a child process
inside the container. Selection now runs inside Title Select, so the selection rows below no longer apply: see the
PlayStation measurements that follow. The Atari 2600 source set has 802 files and its DAT describes 872 games. The Game Boy Advance source set has 3,559 ZIPs, and the build wrote 961 games (3.3 GB).

| Job or request                      | Atari 2600 | Game Boy Advance |
|-------------------------------------|-----------:|-----------------:|
| Full audit                          | 17.6 MiB   | 26.3 MiB (7 min 19 s) |
| Incremental audit, nothing changed  | 18.8 MiB   |                  |
| Forced full audit                   | 18.1 MiB   |                  |
| Repair, planning                    | 17.2 MiB   |                  |
| Repair, with its follow-up audit    | 19.0 MiB   |                  |
| Selection                           | 28–32 MiB  | 77.5 MiB         |
| Selection profile preview           |            | 84.7 MiB         |
| Folder check, then build            |            | under 77.5 MiB (85 s) |
| Ingest scan, then addition          |            | under 33.9 MiB   |

The service idles at 16–19 MiB.

Audits, repairs and builds grow with the number of files, not with their size in bytes: a 3.3 GB build peaked lower
than the selection before it.

The selection engine, and the profile preview that runs it, use the most memory, and grow with the DAT. The Game Boy
Advance DAT is about four times the size of the Atari 2600 DAT, and used about 2.5 times the memory.

### PlayStation, 0.6.0 and 0.7.0

The PlayStation (America) source set has 2,027 games in ZIPs. Its Redump DAT is far larger than the Game Boy Advance
DAT. These are whole-container peaks since the container started, read from `memory.peak` without a reset, with a 512M
limit and one CPU. Each covers every job since that start, so it is an upper bound for any one of them.

| Version | Jobs since the container started                                                   | Peak       |
|---------|------------------------------------------------------------------------------------|-----------:|
| 0.6.0   | Full audit, repair, selections with shadow mode, a 3 h play-library build, and its check | 195.75 MiB |
| 0.7.0   | Selections for PlayStation, Atari 2600 and Game Boy Advance                        | 172 MiB    |

At rest after the 0.7.0 selections, the container held 12 MiB of anonymous memory and 10 MiB of page cache.

The limit is therefore 400M: twice the largest peak, 195.75 MiB, rounded up. Measure again before adding a system whose
DAT is much larger than PlayStation's, and raise the limit if its selection needs it.

The image has no shell, so read `memory.peak` from the Host. If you can't reach Docker there, find the cgroup folder
from the service's process: `/sys/fs/cgroup` followed by the path in `/proc/<pid>/cgroup`.

## 7z in Ingest Folders

Decoding a 7z (ADR 0018) needs its dictionary in memory: LZMA and LZMA2 allocate it whole, and zstd allocates its
window. The other memory 7z needs is small and fixed: members stream through the hasher, and nothing is cached.

- The dictionary each block declares is read from the header before anything is decoded. An archive that needs more
  than `[ingest] max_dictionary_mib` (128 MiB by default) is refused without allocating it. The test
  `crates/engine/tests/sevenz_memory.rs` shows that the peak allocation for a refused 1.5 GiB dictionary stays under
  1 MiB and does not grow with the dictionary.
- One global gate in the Store lets at most `[ingest] archive_workers` (2 by default) archives decode at once, across
  every ingest scan and Change Plan.

7z therefore adds at most `archive_workers` × `max_dictionary_mib` to the peaks above, 256 MiB with the defaults.
The Operator sizes the limit at about:

> 2 × the largest peak without 7z + `archive_workers` × `max_dictionary_mib`

With the 400M limit above and the defaults, that is about 656M. Measure an ingest scan of a large solid 7z after
release to confirm the term, and record it here.
