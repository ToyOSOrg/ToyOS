---
status: open
kind: defect
opened: 2026-10-28
---

# A small message's round trip over a native connection costs twice Linux's on the T14

`osbench ipc`'s `ipc.reqrep.8` sends 8 bytes to a server process and waits
for its 8-byte answer, over ToyOS's native connection and over a Unix stream
socket on Linux. `ipc.reqrep_handle.8` also moves a handle with each request.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range. The AMD's Linux runs 1 to 3 timed through the TSC and 4
and 5 through the HPET, at 1.2 µs a timestamp.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `ipc.reqrep.8` | 7,184 ns (1.9%) | 3,515 ns (22.1%) | 7,324 ns (25.3%) | 13,829 ns (58.1%) |
| `ipc.reqrep_handle.8` | 8,789 ns (2.5%) | 7,260 ns (3.7%) | 8,887 ns (23.4%) | 21,077 ns (42.7%) |

On the T14 ToyOS loses both rows, 2.04x and 1.21x, and both resolve. On the
AMD it does not lose:
- `reqrep_handle.8` resolves ToyOS better (0.42x). ToyOS's slowest boot,
  9,959 ns, beats every TSC Linux run, 20,238 to 21,099 ns.
- `reqrep.8` is unresolved. Linux's runs spread within the TSC runs
  themselves, 7,684, 14,983 and 15,719 ns, and the HPET runs read 13,829 and
  10,551, so the median is an HPET run's.

The bulk-copy rows (`ipc.reqrep.4096`, `65536`) are
`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`'s. What the
T14's 8-byte round trip spends its time on is unmeasured.

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `ipc.connect_accept` | ns | 2,326 (0.9%) | 3,828 (1.1%) | 0.61x | 2,018 (2.0%) | 12,362 (12.2%) | 0.16x |
| `ipc.reqrep.4096` | ns | 6,574 (20.8%) | 7,184 (1.5%) | 0.92x | 6,662 (1.4%) | 18,369 (28.0%) | 0.36x |
| `ipc.reqrep.65536` | ns | 26,074 (2.3%) | 18,920 (3.9%) | 1.38x | 21,721 (104.3%) | 47,109 (40.9%) | 0.46x |
| `ipc.reqrep.8` | ns | 5,973 (1.6%) | 3,492 (14.2%) | 1.71x | 5,966 (1.0%) | 13,689 (33.9%) | 0.44x |
| `ipc.reqrep_handle.8` | ns | 6,403 (1.7%) | 7,248 (3.0%) | 0.88x | 6,412 (32.0%) | 22,000 (8.4%) | 0.29x |

On the T14 `ipc.reqrep.8` is behind at 1.71x; `ipc.reqrep_handle.8` is now
ahead and `ipc.reqrep.4096` at parity. No `ipc` row is behind on the AMD
bench: `ipc.reqrep.65536` is unresolved, ToyOS's boots ranging 104%, and the
rest are ahead.

**Where it goes, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/3ddc686338d20b13528b597a7437e3b027479728/diagnosis)): segment probes on
main 422915e2c, one boot a bench, each probe's own cost subtracted; a probe
stalls the pipeline around it, so each part is an upper bound and their
ranking is the finding. `ipc.reqrep.8`'s server CPU probed 5,844 ns a round
trip on the T14 and 5,587 on the AMD bench: the run queue 1,400 and 1,080,
the wake out of halt to the kick's handler 1,279 and 1,886, the message path
924 and 814 (the receive's table lookup and take 558 and 482), the idle path
485 and 443, the switch 398 and 362, the entry and exit 310 and 340, the
pass's fixed work 282 and 240, the timer 267 and 155, the post and kick 255
and 152, and the wait 245 and 115. #1260 makes the run queue's operations
cheaper.

**Owed on metal:** `ipc.reqrep.*` in one A/B of main from before and after #1260 (the run queue's operations), on the T14 and the AMD bench, with a Linux reference timed through the TSC and its `.slack1ns` rows. The speed-up is not yet measured on metal. Main must read none of them slower.

**Exit**: `ipc.reqrep.8` and `ipc.reqrep_handle.8` within Linux's on both
benches in a labelled current-state comparison, or each remaining difference
measured to its parts and justified.

**Owner**: native IPC (`sdk/toyos/src/ipc.rs` and the kernel's message path).
