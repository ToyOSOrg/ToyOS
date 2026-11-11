---
status: open
kind: defect
opened: 2026-10-28
---

# A memory-filesystem call costs ToyOS up to 35 times Linux's

`osbench files` runs the same operations under each system's memory
filesystem: ToyOS's `/tmp`, the MEMORY file server's
(`system/fileserver/src/memory.rs`), and Linux's `/dev/shm`, tmpfs through
the VFS. The current figures are the second table.

**When the kernel still served `/tmp` through syscalls**, from the
2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range. The AMD's Linux runs 1 to 3 timed through the TSC and 4
and 5 through the HPET, at 1.2 µs a timestamp.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `fs.mem.open_close` | 7,291 ns (7.7%) | 1,114 ns (8.1%) | 6,157 ns (0.8%) | 2,946 ns (48.4%) |
| `fs.mem.stat` | 3,115 ns (5.3%) | 584 ns (2.6%) | 2,616 ns (1.1%) | 1,513 ns (90.7%) |
| `fs.mem.rename` | 7,374 ns (6.3%) | 1,624 ns (2.3%) | 6,282 ns (1.1%) | 4,538 ns (35.7%) |
| `fs.mem.mkdir_rmdir` | 9,011 ns (9.1%) | 2,213 ns (1.5%) | 7,705 ns (3.1%) | 6,702 ns (23.3%) |
| `fs.mem.tree.100` | 1,505,195 ns (4.1%) | 548,925 ns (1.1%) | 1,384,280 ns (1.3%) | 1,457,995 ns (2.6%) |
| `fs.mem.create_write_unlink` | 12,832 ns (4.7%) | 4,715 ns (2.1%) | 10,801 ns (0.6%) | 12,743 ns (13.7%) |
| `fs.mem.write_4k_at` | 1,237 ns (4.0%) | 625 ns (5.0%) | 1,563 ns (7.7%) | 1,823 ns (61.0%) |
| `fs.mem.read_4k_at` | 1,222 ns (2.3%) | 710 ns (4.5%) | 1,422 ns (3.5%) | 1,403 ns (70.8%) |

On the T14 ToyOS lost all eight, 1.7x to 6.5x, each resolved. On the AMD it
loses `open_close` (2.1x) and `rename` (1.4x), both resolved, wins
`tree.100`, `create_write_unlink` and `write_4k_at`, and is unresolved on the
other three. There the HPET explains Linux's spread: its TSC runs read
`open_close` at 2.9 µs and its HPET runs at 4.3 µs. The sequential copies are
`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`'s.

Each `fs.mem` call is a file server request now, which cost 3 to 12 times a
kernel file call when both existed
(`issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md`).

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `fs.mem.create_write_unlink` | ns | 30,518 (0.5%) | 4,651 (1.7%) | 6.56x | 36,674 (19.9%) | 14,178 (11.6%) | 2.59x |
| `fs.mem.fstat` | ns | 7,560 (1.6%) | 213 (7.5%) | 35.49x | 9,117 (23.0%) | 1,886 (74.5%) | 4.83x |
| `fs.mem.mkdir_rmdir` | ns | 20,124 (1.4%) | 2,190 (1.6%) | 9.19x | 23,985 (17.5%) | 8,032 (18.0%) | 2.99x |
| `fs.mem.open_close` | ns | 12,290 (17.1%) | 1,119 (1.3%) | 10.98x | 14,612 (21.1%) | 4,330 (32.9%) | 3.37x |
| `fs.mem.pread_4k` | ns | 729 (5.1%) | 574 (8.4%) | 1.27x | 1,157 (3.4%) | 1,956 (58.9%) | 0.59x |
| `fs.mem.pread_8` | ns | 522 (1.9%) | 343 (3.5%) | 1.52x | 716 (7.0%) | 1,886 (62.2%) | 0.38x |
| `fs.mem.read_4k` | ns | 762 (7.2%) | 551 (5.3%) | 1.38x | 1,047 (4.8%) | 1,955 (47.8%) | 0.54x |
| `fs.mem.read_4k_at` | ns | 1,054 (3.0%) | 685 (10.7%) | 1.54x | 1,458 (2.8%) | 2,375 (40.9%) | 0.61x |
| `fs.mem.readdir.1000` | ns | 528,170 (0.1%) | 181,512 (2.6%) | 2.91x | 446,006 (2.1%) | 271,128 (3.2%) | 1.65x |
| `fs.mem.rename` | ns | 11,800 (2.1%) | 1,596 (1.8%) | 7.39x | 13,570 (15.5%) | 5,867 (27.2%) | 2.31x |
| `fs.mem.reopen_read_4k` | ns | 12,702 (1.2%) | 1,384 (1.2%) | 9.18x | 15,308 (23.0%) | 4,889 (27.4%) | 3.13x |
| `fs.mem.seek` | ns | 248 (0.0%) | 116 (1.7%) | 2.14x | 200 (0.0%) | 1,397 (84.9%) | 0.14x |
| `fs.mem.seq_read.1048576` | MB/s | 7,298 (1.4%) | 9,046 (6.2%) | 1.24x | 10,394 (4.6%) | 8,567 (3.7%) | 0.82x |
| `fs.mem.seq_read.4096` | MB/s | 5,734 (0.9%) | 8,226 (5.3%) | 1.43x | 7,096 (1.1%) | 5,401 (2.2%) | 0.76x |
| `fs.mem.seq_write.1048576` | MB/s | 4,008 (2.6%) | 4,576 (0.8%) | 1.14x | 1,930 (7.3%) | 1,562 (0.5%) | 0.81x |
| `fs.mem.seq_write.4096` | MB/s | 438 (1.4%) | 3,605 (0.6%) | 8.23x | 354 (24.6%) | 1,309 (1.2%) | 3.70x |
| `fs.mem.stat` | ns | 9,490 (0.8%) | 578 (2.1%) | 16.42x | 11,246 (19.9%) | 2,863 (47.9%) | 3.93x |
| `fs.mem.tree.100` | ns | 3,357,680 (1.6%) | 559,607 (0.8%) | 6.00x | 4,017,158 (18.7%) | 1,405,962 (1.1%) | 2.86x |
| `fs.mem.write_4k_at` | ns | 9,033 (1.1%) | 617 (4.7%) | 14.64x | 11,361 (20.1%) | 2,863 (41.2%) | 3.97x |

These are the first figures since the move. On
the T14 every `fs.mem` row is behind: the metadata rows 6x to 35x, a 4 KiB
write at an offset 14.6x, 4 KiB sequential writes 8.2x, 1 MiB sequential
writes 1.14x, and the reads 1.24x to 1.54x. On the AMD bench the metadata
rows, `write_4k_at`, `seq_write.4096` and `readdir.1000` are behind, the
sequential reads and 1 MiB writes ahead, and the 4 KiB and 8-byte reads
unresolved against Linux's HPET-widened spread. `seek`, which the client
answers itself, is 248 ns against Linux's 116 ns on the T14.

#1246 takes the process table and the process's data lock off every
handle-taking call: a served file's read (`SYS_PAGED_READ`) looks its holder
up under the handle table's own lock, read off the calling thread, where
pipeperf had measured the lookup at 60 ns on the T14 and 33 ns on the AMD
bench. Its speed-up is not yet measured on metal.

- **Owed on metal:** the T14's whole `testcases`, `testcases-watchdog` and
  `shared-debug` boots of main after #1246. Main must pass every member,
  among them `handle_lifetime`, `abuse_handle_table`, `abuse_thread_table`,
  `messages_are_whole`, `stream_handles`, `spawn_lands_claimed`,
  `spawn_child_ends_first`, `spawn_returns_the_census`, `endowment_denied`,
  `a_served_file_reads_through_its_paged_object`,
  `a_pager_s_end_answers_its_waiting_holders_gone`, `std_threads`,
  `inbox_cancel_wakes` and `port_badge`.
- **Owed on metal:** an A/B of main before and after #1246, on the T14 and
  the AMD bench: `osbench files`, `ipc`, `time` and `sched`, four boots a
  side in rotated order, each boot's p50, compared by an exact two-sided
  Mann-Whitney test; and Linux's `osbench files` and `ipc` on each bench.
  The claimed rows are `fs.mem.pread_8`, `fs.mem.pread_4k`,
  `fs.mem.direct.pread_4k`, `fs.mem.read_4k`, `fs.mem.read_4k_at`,
  `fs.mem.direct.read_4k_at`, `ipc.reqrep.8` and `ipc.reqrep_handle.8`:
  main after reads each lower or not separated, with how far each moved set
  against the 60 ns (T14) and 33 ns (AMD) lookup, and no other row 5% or
  more worse unless the code #1246 changed explains it. A missing gain does
  not reject the change; it decides whether a lock-free handle read is worth
  building.

**Exit**: every `fs.mem` row of the second table within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: the file server.
