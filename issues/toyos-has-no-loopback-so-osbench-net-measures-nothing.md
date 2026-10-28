---
status: open
kind: defect
opened: 2026-10-28
---

# ToyOS has no loopback, so `osbench net` measures nothing

netstack treats 127/8 as a destination that never exists (`is_martian`,
`system/netstack/wire/src/addr.rs`), and so does each of its paths:
- **TCP connect.** `shard::connect` asks the route first, and the route
  refuses a martian (`system/netstack/ip/src/route.rs`,
  `Counter::RouteInvalidDestination`) before it would match the host's own
  address.
- **TCP listen.** `shard::listen` refuses an address the host does not hold,
  127.0.0.1 among them, as `NotLocal`, answered `ERR_INVALID_INPUT`
  (`system/netstack/src/serve.rs`). So `TcpListener::bind("127.0.0.1:0")`,
  a loopback program's first call, fails.
- **UDP.** `unusable` refuses a loopback destination and a send to the
  host's own address by name (`system/netstack/udp/src/lib.rs`,
  `Counter::SendLoopback`, `SendToSelf`).
- **Tests.** The unit tests hold these refusals: the route's
  `s_ip_rte_006_invalid_and_local_destinations` (127.0.0.1 is
  `RouteInvalidDestination`, `system/netstack/ip/tests/addr.rs`) and UDP's
  `s_udp_us_003_only_an_assigned_address_binds`.

**The connect's refusal is misreported.** `serve.rs`'s `connect_refused`
answers any route refusal `ERR_NOT_CONNECTED`, whose contract is "a machine
with no address or no route yet … which clears when a lease lands". For
127/8 it never clears. A program connecting to localhost is told it is
waiting for a lease, not that the destination is refused.

`osbench net` measures TCP and UDP between two local processes over
127.0.0.1, and reports all five rows unsupported on ToyOS without trying. From
the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`), five
runs of each bench's Linux:

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `net.tcp.connect_accept_close` | unsupported | 20,913 ns (1.0%) | unsupported | 49,002 ns (59.0%) |
| `net.tcp.reqrep.64` | unsupported | 8,935 ns (8.5%) | unsupported | 27,241 ns (30.8%) |
| `net.tcp.stream.65536` | unsupported | 6,719 MB/s (2.3%) | unsupported | 2,894 MB/s (25.1%) |
| `net.udp.reqrep.64` | unsupported | 7,287 ns (2.3%) | unsupported | 22,642 ns (78.6%) |

A program that talks to itself or a sibling over 127.0.0.1, as many servers
and tests do, cannot run unchanged on ToyOS.

**Exit**: netstack carries a loopback path, and a connect it still refuses is
answered by what it is, not as a missing lease. `osbench net` attempts its
rows on ToyOS, and they are within Linux's on both benches in a labelled
current-state comparison.

**Owner**: netstack (`system/netstack/`).
