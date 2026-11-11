---
status: open
kind: tooling
opened: 2026-09-29
---

# ToyOS against Linux on both benches: the scoreboard

`tests/guest/osbench` is the one program, built for `x86_64-unknown-toyos`
and `x86_64-unknown-linux-musl`, that measures each bench under its Ubuntu
24.04 and under the shipped ToyOS kernel. This record is the scoreboard's
home: what it measures, how a session is judged, and the latest session's
rows behind Linux. Its rows are osbench's groups:

- memcpy from 64 B to 64 MiB: `mem.memcpy.*`;
- pipe round trips, and throughput at 64 B to 64 KiB: `pingpong.pipe.*`,
  `pipe.throughput.*`;
- munmaps per second of a page a sibling on another core touched:
  `mem.munmap_touched_remote.4k`. Each side reads its APIC ID from
  CPUID.0BH:EDX (01H:EBX where 0BH enumerates nothing) before and after each
  window, and `osbench_rules::pair::admit` refuses the window when the two
  sides' IDs match or either side's changed across it;
- lateness past a 1 ms timer: `time.timer_1ms.late`;
- NVMe reads, read-only: `nvme.read.*`;
- the package's power, idle and under defined loads, with its C-state
  residency where Intel documents it: `energy.*`, run only by name, a ToyOS
  boot of its own (`osbench_energy`) and as root under Linux;
- the machine's PCI, USB, NVMe and cpuidle identity: `identity`, with no
  serial number or network identifier;
- TCP connect, round trip and throughput between the two benches:
  `lan.wired.*`, `lan.wifi.*`, the bench under test against the
  `osbench-peer` the other bench's Linux runs, each way. ToyOS and Linux on
  one path are the comparison, and Linux's runs on it its ceiling and its
  noise; `osbench-compare` reads the runs, interleaved, four of each at least
  for its exact test to resolve a difference. `lan` and `internet` run only
  by name, each its own metal row, since they need another party;
- a short CPU crunch, one thread then all, that stops at TjMax less 15 C
  where the package's temperature can be read: `crunch.*`.

`tests/guest/osbench/README.md` is how a comparison is run: both halves from
one revision, interleaved runs, Linux as installed for the baseline, and
Linux under ToyOS's frequency request as a labelled experiment. `persist`
runs on the bench's NVMe under both: the `osbench` row refuses a boot whose
DATA is not the NVMe's TOYOS-DATA partition. The README says why the AMD
bench's Linux keeps time on the HPET.

`internet.*`, HTTPS to Cloudflare's speed test, is exploratory evidence of a
path neither system owns, run only when a pass names its row: no automated
test depends on the public internet.

The first paired session (`ToyOSOrg/evidence` branch `pr-1065`, `r3/`) put
ToyOS's T14 LAN download and 64 B round trip at Linux's and its upload at
a third of it. That gap was closed by #1069, #1102, #1112 and #1114, and
the T14 parity session of #1147 read the upload and its first 300 ms at
Linux's ([evidence](https://github.com/ToyOSOrg/evidence/tree/1c9f4c83c362a1b30746f1e096ad412a2d5f25f9/r2/session)).

The cryptographic rows are not osbench's. AES-GCM, ChaCha20-Poly1305,
SHA-256 and the rest were measured under Linux on both benches against ring,
OpenSSL and AWS-LC by the crypto oracle probe, and that table is the
evidence: `ToyOSOrg/evidence` at `bed0b00efac82546b995f0841da54aea11c45694`,
`r2/table.md`.

The AMD bench's `crunch.single.deflate` read 4 to 6 % slower in four boots
of twelve, at the same clock, with and without the deep idle states
(`issues/the-amd-benchs-deflate-runs-6-percent-slow-in-some-boots-at-the-same-clock.md`).
An AMD single-thread row compared across boots needs repeats enough to
see which mode each boot read.

What ToyOS cannot measure is reported `unsupported` by name, and is not a
comparison until it can:

- **NVMe reads**: a program reaches a disk's blocks only through a grant the
  supervisor mints for a file server's role or the idle slot, and the bench's
  NVMe holds another system.
- **Package temperature**: no counter carries it
  (`toyos_abi::counters::Counter`), so the ToyOS crunch runs its one-thread
  phase alone and reports every all-core row `unmeasured`, with nothing to
  stop it at. The sampler's `IA32_PACKAGE_THERM_STATUS` and
  `MSR_TEMPERATURE_TARGET` in
  `issues/the-kernel-owns-cpu-performance-state.md` is what closes it; the
  package's energy is a counter now (`Counter::PackageEnergy`). The
  first ToyOS crunch on the T14 held 3,788 MHz all-core where Linux fell to
  1.7-1.9 GHz, which
  `issues/the-t14s-thermal-modes-do-not-move-its-package-power-limits.md`
  would explain: ToyOS refuses the firmware's PL1 and PL2 writes.
- **Wi-Fi**: `issues/there-is-no-wifi.md`. The T14's `lan.wifi.*` and every
  AMD bench `lan.*` and `internet.*` row, whose one network card is its
  AX200, are measured under Linux alone.
- **USB, NVMe and cpuidle identity**: their owners serve no program that
  reads them.
- **Loopback** (`net.*`): netstack refuses 127/8 as a destination
  (`issues/toyos-has-no-loopback-so-osbench-net-measures-nothing.md`).

## The scoreboard of 2026-11-11

ToyOS at main ad5db5a65 against each bench's Linux as installed, in one
session a bench: four `osbench` boots interleaved with five Linux
`osbench all` runs ([evidence](https://github.com/ToyOSOrg/evidence/tree/784d15e532d89aaa178162a1546024af1fc851df), whose `README.md` states the session, the masks,
what the host's network outage moved, and its own corrections). A cell is the median of the
runs' p50s with their run-to-run range. The spawn rows are ad5db5a65's:
#1200 has since changed how a child's arguments are laid out, unmeasured
here.

**The rule**, set before the runs. A row is behind or ahead when the exact
two-sided Mann-Whitney test over the runs' p50s gives p under 0.05, which at
four against five allows no inversion (2/126) or one (4/126); every row
behind here has none. It is at parity when unresolved with medians within
1.10, and unresolved otherwise. Each p is its row's alone: with no
difference anywhere about 3% of rows would resolve. A row fewer than four
runs of either system measured has no verdict. One boot set is a measured
comparison of one revision; no test asserts any figure here.

**From the next scoreboard on, a timer row is judged against Linux's
`.slack1ns` variant where one exists**, since Linux pads an ordinary sleep by
its 50 µs timer slack and ToyOS has none. This session's rule was set
against the default rows, so its counts are against those and the slack-free
reading is given beside them, exploratory.

`hot` orders the rows and is this scoreboard's own, not a declaration:

- 0, the product's stated performance requirements, by proxy. Build
  throughput is read off `work.build_shaped`, which compresses files, and
  application launch off the spawn rows, which end at the child's exit.
  Battery life is the third such requirement and has no row here: the
  energy rows are staged, not run, and their units have no better direction.
- 1, a path every program pays per operation.
- 2, a per-object path.
- 3, the rest.

By the rule the T14 reads 70 rows behind, 52 ahead, 7 at parity and 5
unresolved of 134, and the AMD bench 35, 76, 8 and 14 of 133 (its
`crunch.single.parse` has two Linux runs: three stopped at the package
limit).

### The T14, every row behind, by hotness and then ratio

| hot | row | ToyOS | Linux | ToyOS slower by | owning record |
|---|---|---|---|---|---|
| 0 | `proc.spawn_wait.large` | 8,072,387 (1.0%) ns | 54,731 (1.1%) | 147.49x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `work.build_shaped.j8` | 220,886,682 (2.5%) ns | 99,510,091 (1.7%) | 2.22x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `proc.posix_spawn_wait.std` | 387,182 (0.7%) ns | 178,366 (3.3%) | 2.17x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `process.spawn_wait` | 376,368 (1.3%) ns | 187,675 (2.2%) | 2.01x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `proc.posix_spawn_wait.tiny` | 78,475 (5.0%) ns | 43,904 (3.0%) | 1.79x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `proc.spawn_wait_loop.100` | 7,687,359 (10.5%) ns | 5,439,550 (2.7%) | 1.41x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `proc.spawn_wait.tiny` | 72,164 (7.3%) ns | 52,413 (2.1%) | 1.38x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 1 | `fs.disk.fstat` | 9,532 (1.7%) ns | 215 (7.9%) | 44.33x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.fstat` | 7,560 (1.6%) ns | 213 (7.5%) | 35.49x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.write_4k_at` | 22,421 (1.4%) ns | 721 (4.7%) | 31.10x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.disk.stat` | 12,312 (21.5%) ns | 578 (1.4%) | 21.30x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.disk.open_close` | 21,544 (1.2%) ns | 1,218 (2.3%) | 17.69x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.stat` | 9,490 (0.8%) ns | 578 (2.1%) | 16.42x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.write_4k_at` | 9,033 (1.1%) ns | 617 (4.7%) | 14.64x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.open_close` | 12,290 (17.1%) ns | 1,119 (1.3%) | 10.98x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.seq_write.4096` | 438 (1.4%) MB/s | 3,605 (0.6%) | 8.23x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.seq_write.4096` | 179 (1.7%) MB/s | 634 (2.8%) | 3.54x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `time.yield` | 651,614 (0.5%) ps | 190,906 (2.1%) | 3.41x | `issues/a-yield-costs-toyos-up-to-3-4-times-linuxs.md` |
| 1 | `fs.mem.seek` | 248 (0.0%) ns | 116 (1.7%) | 2.14x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.seek` | 246 (0.8%) ns | 117 (0.9%) | 2.10x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `pingpong.futex.same_cpu` | 4,091 (1.0%) ns | 2,298 (3.4%) | 1.78x | `issues/a-two-thread-ping-pong-costs-toyos-up-to-twice-linuxs-on-the-t14.md` |
| 1 | `wake.onto_busy` | 4,444 (2.2%) ns | 2,553 (3.0%) | 1.74x | `issues/a-wake-onto-a-busy-cpu-costs-toyos-1-8-times-linuxs.md` |
| 1 | `ipc.reqrep.8` | 5,973 (1.6%) ns | 3,492 (14.2%) | 1.71x | `issues/a-small-message-round-trip-over-a-native-connection-costs-twice-linuxs-on-the-t14.md` |
| 1 | `pingpong.pipe.cross_core` | 5,758 (2.0%) ns | 3,697 (10.8%) | 1.56x | `issues/a-two-thread-ping-pong-costs-toyos-up-to-twice-linuxs-on-the-t14.md` |
| 1 | `fs.mem.read_4k_at` | 1,054 (3.0%) ns | 685 (10.7%) | 1.54x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.pread_8` | 522 (1.9%) ns | 343 (3.5%) | 1.52x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `pipe.throughput.256` | 440 (7.0%) MB/s | 651 (4.5%) | 1.48x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `fs.disk.read_4k_at` | 1,056 (4.9%) ns | 715 (8.0%) | 1.48x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `time.now` | 21,005 (0.0%) ps | 14,283 (0.1%) | 1.47x | `issues/a-clock-read-costs-toyos-half-again-linuxs-on-the-t14.md` |
| 1 | `pipe.throughput.64` | 116 (3.4%) MB/s | 168 (0.6%) | 1.45x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `fs.disk.pread_8` | 544 (21.3%) ns | 376 (1.3%) | 1.45x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.seq_read.4096` | 5,734 (0.9%) MB/s | 8,226 (5.3%) | 1.43x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.seq_read.4096` | 5,402 (1.6%) MB/s | 7,694 (3.2%) | 1.42x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `ipc.reqrep.65536` | 26,074 (2.3%) ns | 18,920 (3.9%) | 1.38x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `fs.disk.pread_4k` | 784 (13.7%) ns | 587 (11.1%) | 1.34x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.disk.seq_read.1048576` | 7,248 (2.5%) MB/s | 9,516 (6.7%) | 1.31x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.pread_4k` | 729 (5.1%) ns | 574 (8.4%) | 1.27x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `wake.storm.drain.w64` | 312,458 (4.0%) ns | 250,576 (11.2%) | 1.25x | `issues/a-wake-storm-drains-up-to-a-quarter-slower-than-linuxs-on-the-t14.md` |
| 1 | `fs.mem.seq_read.1048576` | 7,298 (1.4%) MB/s | 9,046 (6.2%) | 1.24x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.seq_write.1048576` | 4,008 (2.6%) MB/s | 4,576 (0.8%) | 1.14x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `wake.storm.drain.w4` | 138,134 (15.8%) ns | 126,715 (5.8%) | 1.09x | `issues/a-wake-storm-drains-up-to-a-quarter-slower-than-linuxs-on-the-t14.md` |
| 1 | `pipe.throughput.1024` | 1,637 (9.8%) MB/s | 1,767 (2.1%) | 1.08x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `mem.memcpy.4194304` | 20,371 (1.3%) MB/s | 21,445 (7.8%) | 1.05x | `issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md` |
| 1 | `mem.bw.copy` | 6,652 (1.6%) MB/s | 6,951 (3.2%) | 1.04x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `mem.memcpy.67108864` | 9,308 (1.4%) MB/s | 9,647 (0.7%) | 1.04x | `issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md` |
| 1 | `mem.memcpy.16777216` | 10,423 (3.2%) MB/s | 10,760 (3.2%) | 1.03x | `issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md` |
| 2 | `fs.disk.tree.100` | 19,970,802 (3.0%) ns | 1,349,906 (1.4%) | 14.79x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.reopen_read_4k` | 22,020 (0.8%) ns | 1,539 (2.0%) | 14.31x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.rename` | 43,831 (1.1%) ns | 3,595 (0.9%) | 12.19x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.create_write_unlink` | 120,629 (0.5%) ns | 11,249 (1.6%) | 10.72x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.readdir.1000` | 2,854,698 (2.0%) ns | 274,766 (0.9%) | 10.39x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.mem.mkdir_rmdir` | 20,124 (1.4%) ns | 2,190 (1.6%) | 9.19x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.reopen_read_4k` | 12,702 (1.2%) ns | 1,384 (1.2%) | 9.18x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `persist.create_fsync_unlink` | 7,281,693 (7.3%) ns | 958,357 (11.1%) | 7.60x | `issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md` |
| 2 | `fs.mem.rename` | 11,800 (2.1%) ns | 1,596 (1.8%) | 7.39x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.create_write_unlink` | 30,518 (0.5%) ns | 4,651 (1.7%) | 6.56x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `persist.append_4k_fsync` | 6,471,851 (12.3%) ns | 1,011,872 (15.7%) | 6.40x | `issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md` |
| 2 | `persist.write_4k_fsync` | 6,352,402 (13.5%) ns | 1,033,439 (20.1%) | 6.15x | `issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md` |
| 2 | `persist.write_4k_fsync_beside_1m` | 7,598,251 (11.8%) ns | 1,258,302 (23.6%) | 6.04x | `issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md` |
| 2 | `fs.mem.tree.100` | 3,357,680 (1.6%) ns | 559,607 (0.8%) | 6.00x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.disk.mkdir_rmdir` | 77,662 (0.3%) ns | 14,672 (0.6%) | 5.29x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `persist.seq_write_fsync` | 182 (18.7%) MB/s | 590 (3.9%) | 3.24x | `issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md` |
| 2 | `fs.mem.readdir.1000` | 528,170 (0.1%) ns | 181,512 (2.6%) | 2.91x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `mem.munmap_touched_remote.4k` | 3,341 (0.1%) ns | 2,279 (0.8%) | 1.47x | `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md` |
| 2 | `persist.seq_read_cached` | 7,862 (4.8%) MB/s | 11,256 (14.3%) | 1.43x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.mem.read_4k` | 762 (7.2%) ns | 551 (5.3%) | 1.38x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `mem.map_unmap_4k` | 2,072 (0.4%) ns | 1,609 (2.1%) | 1.29x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 2 | `fs.disk.read_4k` | 753 (2.9%) ns | 601 (15.5%) | 1.25x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `mem.munmap_part.4k` | 1,406 (2.4%) ns | 1,254 (0.7%) | 1.12x | `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md` |
| 3 | `fair.shared.total` | 2,369,620 (0.1%) chunks/s | 2,391,673 (0.0%) | 1.01x | `issues/all-cpu-throughput-in-one-group-reads-under-linuxs.md` |

Unresolved and reading behind: `pingpong.pipe.same_cpu` 1.31x (Linux's runs
range 2.9 to 7.0 µs) and `mem.anon.map` 1.11x.

**`pingpong.futex.cross_core` did not measure a parked round trip in three
of the T14's four boots.** Their p50s read 257, 250 and 257 ns, and the
third mixed both shapes in one run (p90 4,578 ns). A round trip that short
cannot hold a park: the same boots' `time.yield` alone costs 652 ns. The
fourth boot read 4,573 ns, as all four boots did on 2026-10-28 (4,553 to
4,668 ns), which is 1.37x Linux's 3,346 ns. So where the row measured what
it means it is behind as before, and the rule, which reads p50s, calls it
unresolved. That neither thread slept in the short rounds is an inference
from the timings: the row counts no parks
(`issues/a-two-thread-ping-pong-costs-toyos-up-to-twice-linuxs-on-the-t14.md`).

**The T14's timer rows, against Linux with and without its timer slack:**

| row | ToyOS ns | Linux, default slack | ToyOS against it | Linux, slack 1 ns | ToyOS against it | p | reading |
|---|---|---|---|---|---|---|---|
| `time.sleep_late.100us` | 5,210 | 51,850 | 0.10x | 1,827 | 2.85x | 0.016 | behind |
| `time.sleep_late.10ms` | 31,318 | 218,198 | 0.14x | 163,553 | 0.19x | 0.016 | ahead |
| `time.sleep_late.10us` | 1,434 | 51,819 | 0.03x | 1,820 | 0.79x | 0.016 | ahead |
| `time.sleep_late.1ms` | 25,639 | 72,340 | 0.35x | 20,400 | 1.26x | 0.286 | unresolved |
| `time.sleep_late.1us` | 1,501 | 51,814 | 0.03x | 3,982 | 0.38x | 0.016 | ahead |
| `time.timer_1ms.late` | 14,155 | 70,302 | 0.20x | 20,087 | 0.70x | 0.190 | unresolved |
| `wake.periodic.idle` | 20,806 | 67,862 | 0.31x | 17,667 | 1.18x | 0.016 | behind |
| `wake.periodic.loaded` | 3,624 | 52,374 | 0.07x | 2,373 | 1.53x | 0.016 | behind |

Against the default rows all eight count ahead. Against the slack-free
Linux three are behind, each resolved, and would rank at hot 1 for the two
`wake.periodic` rows (1.53x and 1.18x) and hot 3 for `time.sleep_late.100us`
(2.85x); `issues/a-timed-wake-is-later-than-linuxs-without-its-timer-slack.md`
owns them.

### The AMD bench

Its Linux timed runs 1 to 3 through the HPET and runs 4 and 5 through the
TSC, as each boot chose. An HPET timestamp costs about 1.2 µs, which is
osbench's instrument and not Linux's path, and every per-operation Linux
figure of those runs carries it. So the verdicts are against all five runs
and the table is ordered by the ratio against the two TSC runs, which is the
figure to read and, at two runs, exploratory. The verdicts barely depend on
it: every row behind is worse than both TSC runs in every boot, and of the
76 ahead every boot beats both TSC runs in 74 (not `fs.disk.pread_4k` and
`persist.create_fsync_unlink`).

**A clean reading needs** five Linux runs that all state `clocksource=tsc`,
interleaved as now: the clocksource checked before each run and Linux
rebooted until it is the TSC, with the boots passed over logged. Not
`tsc=reliable`, which `tests/guest/osbench/README.md` rules out.

| hot | row | ToyOS | Linux, all five | slower by | against the TSC runs | owning record |
|---|---|---|---|---|---|---|
| 0 | `proc.spawn_wait.large` | 12,586,708 (1.1%) ns | 278,643 (17.1%) | 45.17x | 45.64x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 0 | `work.build_shaped.j16` | 277,092,641 (0.7%) ns | 107,734,386 (24.4%) | 2.57x | 2.45x | `issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md` |
| 1 | `fs.mem.fstat` | 9,117 (23.0%) ns | 1,886 (74.5%) | 4.83x | 18.76x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.fstat` | 8,936 (24.5%) ns | 1,885 (75.0%) | 4.74x | 18.58x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.disk.write_4k_at` | 26,440 (9.5%) ns | 2,933 (35.5%) | 9.01x | 13.33x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.disk.stat` | 11,707 (18.5%) ns | 2,794 (50.8%) | 4.19x | 8.03x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.stat` | 11,246 (19.9%) ns | 2,863 (47.9%) | 3.93x | 7.51x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.write_4k_at` | 11,361 (20.1%) ns | 2,863 (41.2%) | 3.97x | 6.48x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.open_close` | 18,670 (17.5%) ns | 4,331 (31.3%) | 4.31x | 5.85x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `fs.mem.open_close` | 14,612 (21.1%) ns | 4,330 (32.9%) | 3.37x | 5.02x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.mem.seq_write.4096` | 354 (24.6%) MB/s | 1,309 (1.2%) | 3.70x | 3.71x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 1 | `fs.disk.seq_write.4096` | 202 (10.4%) MB/s | 721 (4.2%) | 3.57x | 3.60x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `wake.onto_busy` | 18,335 (1.2%) ns | 10,197 (29.1%) | 1.80x | 2.16x | `issues/a-wake-onto-a-busy-cpu-costs-toyos-1-8-times-linuxs.md` |
| 1 | `pipe.throughput.4096` | 3,022 (0.5%) MB/s | 4,951 (28.5%) | 1.64x | 2.00x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `fs.disk.seq_write.1048576` | 464 (4.5%) MB/s | 791 (5.3%) | 1.70x | 1.74x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `mem.memcpy.64` | 3,967 (0.8%) MB/s | 6,492 (1.6%) | 1.64x | 1.65x | `issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md` |
| 1 | `pipe.throughput.16384` | 3,020 (1.1%) MB/s | 3,714 (6.5%) | 1.23x | 1.25x | `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md` |
| 1 | `fs.disk.seq_read.1048576` | 9,980 (3.4%) MB/s | 11,500 (3.8%) | 1.15x | 1.16x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 1 | `time.yield` | 622,208 (4.2%) ps | 551,970 (1.3%) | 1.13x | 1.13x | `issues/a-yield-costs-toyos-up-to-3-4-times-linuxs.md` |
| 1 | `mem.memcpy.256` | 14,598 (0.9%) MB/s | 16,150 (1.7%) | 1.11x | 1.12x | `issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md` |
| 2 | `fs.disk.tree.100` | 17,501,764 (2.4%) ns | 3,448,059 (1.9%) | 5.08x | 5.13x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.reopen_read_4k` | 19,226 (17.4%) ns | 5,238 (27.9%) | 3.67x | 4.90x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.readdir.1000` | 2,029,621 (4.9%) ns | 441,684 (3.7%) | 4.60x | 4.67x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.create_write_unlink` | 120,460 (4.8%) ns | 29,054 (7.2%) | 4.15x | 4.42x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.disk.rename` | 40,481 (5.4%) ns | 10,825 (14.2%) | 3.74x | 4.34x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `fs.mem.reopen_read_4k` | 15,308 (23.0%) ns | 4,889 (27.4%) | 3.13x | 4.30x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.mkdir_rmdir` | 23,985 (17.5%) ns | 8,032 (18.0%) | 2.99x | 3.59x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.rename` | 13,570 (15.5%) ns | 5,867 (27.2%) | 2.31x | 3.03x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.create_write_unlink` | 36,674 (19.9%) ns | 14,178 (11.6%) | 2.59x | 2.88x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.mem.tree.100` | 4,017,158 (18.7%) ns | 1,405,962 (1.1%) | 2.86x | 2.87x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `fs.disk.mkdir_rmdir` | 77,305 (5.1%) ns | 38,484 (4.3%) | 2.01x | 2.09x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 2 | `persist.seq_write_fsync` | 260 (7.3%) MB/s | 523 (1.7%) | 2.01x | 2.01x | `a-sequential-durable-write-on-data-runs-at-about-half-linuxs`, the record open PR #1216 adds |
| 2 | `fs.mem.readdir.1000` | 446,006 (2.1%) ns | 271,128 (3.2%) | 1.65x | 1.66x | `issues/a-memory-filesystem-call-costs-toyos-up-to-35-times-linuxs.md` |
| 2 | `persist.seq_read_cached` | 6,004 (12.5%) MB/s | 11,141 (42.0%) | 1.86x | 1.12x | `issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md` |
| 3 | `crunch.single.matmul` | 943,710 (1.2%) ns | 920,525 (2.2%) | 1.03x | 1.04x | `issues/toyos-uses-what-modern-hardware-offers-for-speed.md` |

Not behind by the rule, yet behind both TSC runs: `fair.shared.total`,
4.06 M chunks/s against 5.09 M and 5.10 M
(`issues/all-cpu-throughput-in-one-group-reads-under-linuxs.md`).

**"76 ahead" is not 76 faster paths.** Two things Linux as installed carries
there:

- Its system-call path pays mitigations ToyOS does not have: `syscall.null`
  reads 159 against 49 ns, and its run lines state retpolines, conditional
  IBPB, STIBP and Safe RET
  (`issues/indirect-branches-and-returns-run-without-thunks.md`,
  `issues/no-entry-or-switch-clears-the-bhb-fills-the-rsb-or-issues-an-ibpb.md`).
- After 23 of the 76, all shapes that block and wake, the clock Linux's
  reporting thread read was 1.37 to 1.9 GHz in the median run, where ToyOS's
  read 4.2 to 4.3 GHz. The probe reads the reporting thread's CPU, so that
  the measured threads ran slow is an inference; the README's labelled
  `performance`-governor experiment would settle it.

**The AMD bench's timer rows:**

| row | ToyOS ns | Linux, default slack | ToyOS against it | Linux, slack 1 ns | ToyOS against it | p | reading | slack-free TSC runs |
|---|---|---|---|---|---|---|---|---|
| `time.sleep_late.100us` | 5,688 | 65,595 | 0.09x | 11,189 | 0.51x | 0.016 | ahead | 10,351, 10,433 |
| `time.sleep_late.10ms` | 56,524 | 118,624 | 0.48x | 289,050 | 0.20x | 0.016 | ahead | 289,050, 297,859 |
| `time.sleep_late.10us` | 1,367 | 61,099 | 0.02x | 11,023 | 0.12x | 0.016 | ahead | 10,454, 10,478 |
| `time.sleep_late.1ms` | 6,192 | 88,402 | 0.07x | 24,312 | 0.25x | 0.016 | ahead | 11,191, 11,652 |
| `time.sleep_late.1us` | 1,400 | 61,020 | 0.02x | 13,497 | 0.10x | 0.016 | ahead | 13,497, 12,615 |
| `time.timer_1ms.late` | 49,110 | 74,260 | 0.66x | 24,250 | 2.03x | 0.016 | behind | 11,126, 11,601 |
| `wake.periodic.idle` | 20,426 | 73,658 | 0.28x | 20,134 | 1.01x | 1.000 | at parity | 10,810, 11,027 |
| `wake.periodic.loaded` | 4,403 | 59,212 | 0.07x | 9,156 | 0.48x | 0.413 | unresolved | 4,339, 4,414 |

`time.timer_1ms.late` counts ahead against the default row and is 2.03x
behind the slack-free one, resolved, and 4.3x behind its two TSC runs; the
same record owns it. `wake.periodic.idle` reads 6.2, 34.6, 6.2 and 35.0 µs
in ToyOS's four boots, the two modes
`issues/many-osbench-rows-on-the-amd-bench-are-bimodal-per-boot.md` records.

### Durable writes

The T14 was in its slow mode in four boots of four, for the whole of
`persist`: `write_4k_fsync` 6.49, 6.21, 6.57 and 5.72 ms against Linux's 0.87
to 1.08 ms, `seq_write_fsync` 189, 201, 167 and 175 MB/s against 574 to 597.
On the AMD bench the 4 KiB durable writes are ahead (2.3 against 3.6 ms) and
`persist.seq_write_fsync` is behind in every boot, 249 to 268 MB/s against 518
to 527.

### Not measured

`osbench_energy` on both benches and `osbench_lan` on the T14 are staged and
not yet run. Beyond what the list above reports unsupported:

- Linux's `nvme.read.*` was unmeasured too, since the run was not root's, and
  the all-core crunch is `unmeasured` on ToyOS, not unsupported.
- The six `fair.*` share rows have no better direction and so no verdict.
- Boot to a usable state: both systems report it as conditions, not rows.
- No row measures graphics or audio against Linux (a frame, a present, a
  mixed period).
- No row measures an application's launch to a usable state, cold or warm,
  or a build of compiler work: hot 0 stands on proxies.
- No row reads a file the cache does not hold, or the first durable write
  after the drive has idled.

**Exit**: a session on both benches at a named main revision, judged by the
rule above with the slack-free timer rows and five TSC-timed Linux runs on
the AMD bench, with energy and the LAN in it, in which no row is behind
Linux or each row behind is justified in its owning record; and a row for
each gap listed under "Not measured". Linux's outputs are a goal ToyOS's are
compared against in evidence, never a fixture a test reads.
