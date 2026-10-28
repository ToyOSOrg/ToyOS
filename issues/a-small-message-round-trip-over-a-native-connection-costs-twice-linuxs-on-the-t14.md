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

**Exit**: `ipc.reqrep.8` and `ipc.reqrep_handle.8` within Linux's on both
benches in a labelled current-state comparison, or each remaining difference
measured to its parts and justified.

**Owner**: native IPC (`sdk/toyos/src/ipc.rs` and the kernel's message path).
