---
status: open
kind: defect
opened: 2026-10-09
---

# A DHCP message the client refuses is a log line each

`toyos-dhcp` keeps every refusal whose rule is logged (`Client::refuse`,
`toyos-dhcp/src/lib.rs`) until `Client::drain_refusals`, up to
`limits::EVENTS` a call, and the node hands each on as `Event::Dhcp`
(`userland/netstack/node/src/lib.rs`, `Node::drain_events`), which netstack
writes to the log (`userland/netstack/src/serve.rs`, `Sockets::settle`). The
stack's own refusals pass through a `RefusalLog` in the shard, at most one line
a rule in any 10 s with a count of the rest
(`toyos-net-shard/src/lib.rs`); the client's pass through none.

So any host on the link that sends malformed replies to port 68 writes one
line of this machine's log a frame, for as long as it likes: the log is a file
logkeeper keeps on the stick, and a served log sends each line on.

Read from the code, not measured.

**Exit condition**: the client's logged refusals are bounded as the shard's
are, and a test of the node that feeds it a hundred replies one rule refuses
finds one line and a count.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
