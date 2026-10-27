---
status: open
kind: defect
opened: 2026-10-27
---

# A TCP connect zeroes twelve mebibytes

A ToyOS TCP connect on the T14 takes about 0.8 ms longer than the same
program's connect under the bench's Linux, on the same path to the same
peer. The local cost is memory the connect makes and zeroes, whatever the
connection then carries. That is an inference from the code and from the
T14's write bandwidth, not a split anyone has read.

**Measured** on 2026-10-24 by the paired `osbench lan` session at
`aeba59fb0`: four ToyOS runs and four Linux runs, interleaved, from the T14's
I219 to the AMD bench's Linux peer over its Wi-Fi.

| | ToyOS | Linux |
|---|---|---|
| `lan.wired.tcp.connect` p50, ms | 3.02, 2.05, 3.00, 3.06 | 1.24, 2.01, 1.97, 1.28 |
| `lan.wired.tcp.connect` min, ms | 2.16, 1.70, 2.08, 1.94 | 0.91, 1.39, 0.95, 0.91 |
| `internet.tcp.connect` p50, ms | 6.98, 6.85, 7.01, 7.01 | 5.99, 6.07, 5.97, 5.99 |

The 64-byte round trip on the same path read 2.00 ms under ToyOS against
2.09 ms under Linux, so the path does not separate the two. The ToyOS run
whose connect row ended at 3.1 GHz read 2.05 ms; the three that ended at
1.5 GHz read 3.0 ms. Processor work, not a wait, is what grows.

**Derived**: what one connect makes before it returns.

- The client's region, `toyos::net::stream::REGION_BYTES`, 8 MiB.
  `SYS_SHM_CREATE` takes and zeroes each of its 2048 frames before it
  answers (`kernel/src/object/shm.rs`, `SharedMemObject::create`), and
  netstack maps all of them before the SYN leaves.
- The connection's two pipes, 2 MiB each: the request's on the client's
  write and the answer's on netstack's, each zeroed whole on that first
  write (`issues/a-pipe-is-a-two-mebibyte-page-for-a-mapping-nothing-makes.md`).

Twelve mebibytes at the 9.35 GB/s the T14 wrote under ToyOS
(`mem.bw.write`, 2026-10-22) is 1.3 ms; the lowest gap between the two
systems' minimums is 0.8 ms. A connection that moves a few bytes needs
none of this memory zeroed.

**What would remove it**, each beyond `toyos::net` and netstack:

- A memory object's frames promised at its creation and taken and zeroed
  on the first touch through any mapping, as private memory's are
  (`kernel/src/mm/space.rs`). A stream then zeroes what its rings reach.
  It changes the memory object's contract that a fault on one never
  allocates, so it is the owner's to rule.
- A pipe's ring that is no 2 MiB page, the pipe issue's own exit: the
  connection's 4 MiB.

**Exit**: the cause read on the T14, by an image whose region is the only
difference, and `lan.wired.tcp.connect` within Linux's run-to-run range on
the same path.

**Owner**: the orchestrator, who holds the pipe issue and puts the memory
object's contract to the owner.
