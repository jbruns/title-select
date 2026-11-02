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

Measured on version 0.4.0, with a 512M limit and one CPU. The Atari 2600 source set has 802 files and its DAT describes
872 games. The Game Boy Advance source set has 3,559 ZIPs, and the build wrote 961 games (3.3 GB).

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
| Ingest scan, then operator addition |            | under 33.9 MiB   |

The service idles at 16–19 MiB.

Audits, repairs and builds grow with the number of files, not with their size in bytes: a 3.3 GB build peaked lower
than the selection before it.

The selection engine, and the profile preview that runs it, use the most memory, and grow with the DAT. The Game Boy
Advance DAT is about four times the size of the Atari 2600 DAT, and used about 2.5 times the memory.

The limit is therefore 192M: twice the largest peak, 84.7 MiB, rounded up. Measure again before adding a system whose
DAT is much larger than these, and raise the limit if its selection needs it.
