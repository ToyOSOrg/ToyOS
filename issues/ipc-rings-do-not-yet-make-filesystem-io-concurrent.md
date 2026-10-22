---
status: open
kind: track
opened: 2026-10-16
---

# Concurrent filesystem I/O and userland service lookup

## Storage contract

Use an asynchronous storage core with convenient blocking APIs over the same
path. Ordinary Rust programs retain their blocking APIs. Bulk transport and
protocol interpretation belong in userland; no universal kernel I/O executor.
Shared rings alone do not establish concurrent requests or eliminate copies.

The design must carry tagged outstanding operations and explicit buffer
ownership through cache misses, the filesystem and block service. Preserve
write ordering, durability, bounded queues, cancellation and disconnect
semantics. Prefer client-provided reusable buffers; retain copies needed to
snapshot untrusted control data. Do not move serialization one layer down.

Under contention, favor interactive responsiveness while background work keeps
progressing. Start with bounded fair dispatch; justify the smallest useful
classification mechanism. CPU priority alone cannot establish I/O priority.
Shared writeback and already-issued device operations constrain scheduling;
no hard storage deadline or generic kernel dependency graph is promised.

After service failure, report uncertain outcomes. Retry only when the protocol
proves replay safe; a lost reply does not prove that a mutation did nothing.
Do not introduce persistent request/result records solely to make arbitrary
mutations recover transparently. Keep acknowledged writes awaiting durable
flush distinct from operations whose completion was never confirmed.

## Service lookup

Move service names and lookup to the supervisor and SDK. The supervisor supplies
authorized connectors; the SDK resolves their names locally. The kernel enforces
capability authority. Names themselves confer no access. Preserve narrowing,
controlled delegation and later authorized grants without giving children the
parent's entire lookup table. No separate resolver daemon or generic kernel
capability bundle is required. Filesystem path resolution is a separate concern.

## Completion

Review the request/buffer lifetime and safe retry contracts, then migrate callers
and delete superseded paths. Demonstrate concurrent independent requests, bounded
work and background progress, correct durability and honest failure reporting.
Measure before claiming reduced notification cost or improved throughput.
