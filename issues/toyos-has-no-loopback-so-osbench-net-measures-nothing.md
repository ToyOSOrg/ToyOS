---
status: open
kind: defect
opened: 2026-10-28
---

# ToyOS has no loopback, so `osbench net` measures nothing

netstack carries no loopback. Its UDP refuses a loopback destination and a
send to its own address by name (`system/netstack/udp/src/lib.rs`'s
`unusable`: `Counter::SendLoopback`, `SendToSelf`). `osbench net`, which
measures TCP and UDP between two local processes over 127.0.0.1, reports all
five rows unsupported on ToyOS without trying them, so how netstack's TCP
answers a connect to 127/8 is unread. The 2026-10-28 scoreboard (ToyOS at 069cbbeb8, 4 boots off the internal NVMe against 5 runs of each bench's Linux as installed, interleaved; p50s, with the run-to-run range; evidence branch `scoreboard-2026-10-28`):

| row | T14 Linux | AMD Linux |
|---|---|---|
| `net.tcp.connect_accept_close` | 20,913 ns (1.0%) | 49,002 ns (59.0%) |
| `net.tcp.reqrep.64` | 8,935 ns (8.5%) | 27,241 ns (30.8%) |
| `net.tcp.stream.65536` | 6,719 MB/s (2.3%) | 2,894 MB/s (25.1%) |
| `net.udp.reqrep.64` | 7,287 ns (2.3%) | 22,642 ns (78.6%) |

ToyOS: `unsupported` in all 4 boots on both benches. A program that talks to
itself or a sibling over 127.0.0.1, as many servers and tests do, cannot
run unchanged on ToyOS.

**Exit**: netstack carries a loopback path, `osbench net` attempts its rows
on ToyOS, and they are within Linux's on both benches in a labelled
current-state comparison.

**Owner**: netstack (`system/netstack/`).
