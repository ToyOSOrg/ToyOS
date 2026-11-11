---
status: open
kind: defect
opened: 2026-10-28
---

# A file server request costs three to twelve times a kernel file call

Every file operation on DATA is a request to the DATA file server process,
but for a seek from the start or the current offset of a file no other
process shares, which the client keeps (#1110). On the AMD bench (2026-10-28, `osbench` at PR #1097's c8f3fbfb0, DATA on its
NVMe), that boot's cached rows on DATA against its kernel `/tmp`, and the
bench's Linux page cache on the same NVMe:

| row, p50 | ToyOS DATA | ToyOS `/tmp` | DATA / `/tmp` | Linux home |
|---|---|---|---|---|
| `read_4k_at` (seek and 4 KiB read) | 16,903 ns | 1,413 ns | 12.0x | 1,463–2,375 ns |
| `stat` | 10,530 ns | 2,596 ns | 4.1x | 1,453–2,794 ns |
| `open_close` | 18,686 ns | 6,091 ns | 3.1x | 3,056–4,400 ns |
| `rename` | 59,154 ns | 6,242 ns | 9.5x | 9,308–10,895 ns |
| `seq_read.1048576` | 6,401 MB/s | 8,041 MB/s | | 11,295–11,403 MB/s |

Four of these rows the disk does not set. On 2026-10-24's in-memory DATA the
same rows read within about 5%: `read_4k_at` 16,822, `stat` 10,450 and
`rename` 58,002 ns, and `seq_read.1048576` 6,080 MB/s. `open_close` read
14,178 ns there, so it is 32% dearer on the NVMe, and what that costs is
unattributed. Both tables are in PR #1097's evidence. What the round trip
spends its time on, on metal, is unmeasured.
`issues/a-cached-read-from-a-file-server-is-measured-only-under-tcg.md`
splits it under TCG.

**#1110, the client's offset** (base b3dbfbfb0, head 1293be570, two pairs
per bench, p50 µs, [evidence](https://github.com/ToyOSOrg/evidence/tree/d08c7dceb4e9f4301f0faff8905b867c467d1226/s1-ab)):
`seek` is local, and `read_4k_at` and `write_4k_at` are one round trip.

| row | T14 | AMD |
|---|---|---|
| `seek` | 8.43 → 0.24, 8.31 → 0.24 | 7.84 → 0.19, 9.79 → 0.20 |
| `read_4k_at` | 18.17 → 9.96, 17.82 → 10.13 | 17.10 → 9.48, 20.99 → 11.36 |
| `write_4k_at` | 18.22 → 10.05, 17.98 → 10.21 | 17.69 → 10.06, 21.64 → 11.98 |

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `fs.disk.create_write_unlink` | ns | 120,629 (0.5%) | 11,249 (1.6%) | 10.72x | 120,460 (4.8%) | 29,054 (7.2%) | 4.15x |
| `fs.disk.fstat` | ns | 9,532 (1.7%) | 215 (7.9%) | 44.33x | 8,936 (24.5%) | 1,885 (75.0%) | 4.74x |
| `fs.disk.mkdir_rmdir` | ns | 77,662 (0.3%) | 14,672 (0.6%) | 5.29x | 77,305 (5.1%) | 38,484 (4.3%) | 2.01x |
| `fs.disk.open_close` | ns | 21,544 (1.2%) | 1,218 (2.3%) | 17.69x | 18,670 (17.5%) | 4,331 (31.3%) | 4.31x |
| `fs.disk.pread_4k` | ns | 784 (13.7%) | 587 (11.1%) | 1.34x | 1,142 (1.8%) | 2,374 (51.1%) | 0.48x |
| `fs.disk.pread_8` | ns | 544 (21.3%) | 376 (1.3%) | 1.45x | 687 (17.5%) | 1,886 (59.7%) | 0.36x |
| `fs.disk.read_4k` | ns | 753 (2.9%) | 601 (15.5%) | 1.25x | 1,012 (4.0%) | 2,305 (55.6%) | 0.44x |
| `fs.disk.read_4k_at` | ns | 1,056 (4.9%) | 715 (8.0%) | 1.48x | 1,442 (1.5%) | 2,375 (44.7%) | 0.61x |
| `fs.disk.readdir.1000` | ns | 2,854,698 (2.0%) | 274,766 (0.9%) | 10.39x | 2,029,621 (4.9%) | 441,684 (3.7%) | 4.60x |
| `fs.disk.rename` | ns | 43,831 (1.1%) | 3,595 (0.9%) | 12.19x | 40,481 (5.4%) | 10,825 (14.2%) | 3.74x |
| `fs.disk.reopen_read_4k` | ns | 22,020 (0.8%) | 1,539 (2.0%) | 14.31x | 19,226 (17.4%) | 5,238 (27.9%) | 3.67x |
| `fs.disk.seek` | ns | 246 (0.8%) | 117 (0.9%) | 2.10x | 200 (4.5%) | 1,397 (85.0%) | 0.14x |
| `fs.disk.seq_read.1048576` | MB/s | 7,248 (2.5%) | 9,516 (6.7%) | 1.31x | 9,980 (3.4%) | 11,500 (3.8%) | 1.15x |
| `fs.disk.seq_read.4096` | MB/s | 5,402 (1.6%) | 7,694 (3.2%) | 1.42x | 6,912 (1.5%) | 5,022 (2.7%) | 0.73x |
| `fs.disk.seq_write.1048576` | MB/s | 504 (3.0%) | 347 (1.7%) | 0.69x | 464 (4.5%) | 791 (5.3%) | 1.70x |
| `fs.disk.seq_write.4096` | MB/s | 179 (1.7%) | 634 (2.8%) | 3.54x | 202 (10.4%) | 721 (4.2%) | 3.57x |
| `fs.disk.stat` | ns | 12,312 (21.5%) | 578 (1.4%) | 21.30x | 11,707 (18.5%) | 2,794 (50.8%) | 4.19x |
| `fs.disk.tree.100` | ns | 19,970,802 (3.0%) | 1,349,906 (1.4%) | 14.79x | 17,501,764 (2.4%) | 3,448,059 (1.9%) | 5.08x |
| `fs.disk.write_4k_at` | ns | 22,421 (1.4%) | 721 (4.7%) | 31.10x | 26,440 (9.5%) | 2,933 (35.5%) | 9.01x |
| `persist.seq_read_cached` | MB/s | 7,862 (4.8%) | 11,256 (14.3%) | 1.43x | 6,004 (12.5%) | 11,141 (42.0%) | 1.86x |

On the T14 every cached `fs.disk` row but `seq_write.1048576` is behind, the
metadata rows 5x to 44x. The cached reads are now 1.25x to 1.48x on the T14.
On the AMD bench the metadata rows, `write_4k_at`, both sequential writes
and `seq_read.1048576` are behind, and no 4 KiB or 8-byte read is. `persist.seq_read_cached`, a cached read of a
64 MiB DATA file, is behind on both benches and had no record. `seek`, which
the client answers itself, is 246 ns against Linux's 117 ns on the T14.

**#1258, an open file written and stated through its paged object**, is
not yet measured on metal, its speed-up included; its rows are owed:

- **Owed on metal:** the T14's whole `testcases` shared boot on main after
  #1258. Main must show every member passing, among them
  `a_served_files_read_never_tears_against_its_write`,
  `a_served_files_writes_land_whole` and
  `a_pager_s_end_answers_its_waiting_holders_gone`; no paged overdue report;
  and `--fat32-check` clean.
- **Owed on metal:** `osbench_disk` (`diskbenchcase`) on the T14, main from
  before #1258 and after, two boots a side in A, B, B, A order. Main after
  must show `fs.disk.fstat` at about 1 µs or below (from 9,532 ns),
  `fs.disk.write_4k_at` at a few µs (from 22,421 ns) and
  `fs.disk.seq_write.4096` above main before's. `fs.disk.seq_write.1048576`,
  `readdir.1000`, `stat`, `open_close`, `rename`, `mkdir_rmdir` and
  `create_write_unlink` must lie within main before's spread, and no row may
  be beyond twice main before's.
- **Owed on the `testcases` shared boot's x86-64 guest row**, once it exists:
  - **The run.** `a_pager_s_end_answers_its_waiting_holders_gone` at #1258's code, or at any revision whose
    paged object, pure paged module and pagerprobe are #1258's, run on several vCPUs. It shows order and contents
    only. It must:
    - pass, its log showing pagerprobe's helper's writes admitted, then the owner's read-back, then the pager's
      end answering the wait `Gone`, then `pagerprobe: ok`; the helper's page written against credit somewhere
      between its writes admitted and `Gone`, since its process and the owner's print in either order;
    - leave no paged overdue report;
    - pass with the two served-file tests beside it.
  - **The control.** With the kernel refusing every holder's write (#1258's control branch
    `fileops-step1-ctl-refuse`), that test must go red: the helper on its first write, answered `WouldBlock`, and
    the pager on never seeing the credited object dirtied. Every other member must stay green.
- **Shown by no test yet:** that opening a file on `/tmp` or `/home` mints a write holder, and that the SDK writes
  through the holder before it asks the server. Today that rests on reading the file server's open and the SDK's
  write path, and on the owed `osbench_disk` rows, whose holder-path figures would show it.

**Exit**: the cached `fs.disk.*` rows within Linux's on both benches, or
each row's remaining difference measured to its parts and justified, in a
labelled current-state comparison.

**Owner**: the file server (`system/fileserver/`) and its transport
(`sdk/toyos/src/fs.rs`).
