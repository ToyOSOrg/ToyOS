---
status: open
kind: defect
opened: 2026-09-29
---

# A nonblocking perf-state read can only answer `WouldBlock`

`kernel/src/object/ops.rs:396` (`DeviceType::PerfState => claim.read_perf_state(&mut None, buf)`)
is the nonblocking arm: on SMP it always issues a fresh ask, sends an IPI to
kick every other CPU, and still returns `WouldBlock`, because the ask cannot
be answered within the call that issued it. No retry ever succeeds, and a
poll on the claim is refused rather than reporting the readiness a
nonblocking read would need. A CPU-wide shootdown that can only ever answer
"try a blocking read instead" is pure cost.

Exit: `read_block_device`/poll on `PerfState` refuse `NotSupported` by name,
so a caller learns not to retry nonblocking rather than paying the kick to be
told again; `object::ops.rs`'s `&mut None` arm for `PerfState` goes with it.
