---
status: open
kind: defect
opened: 2026-09-24
---

# Every random byte is one RDRAND, with no generator behind it

`SYS_RANDOM` (`kernel/src/arch/syscall/io.rs`) answers every request with
`RDRAND` values copied straight to the caller. The kernel has no random
generator of its own. Every key sshd mints, every future TLS session and every
nonce therefore rests on one instruction from one vendor. There is no second
source to mix with it, no reseed, and no health check. That single source is
where real failures have been: some AMD parts returned all-ones from `RDRAND`
after a resume. Linux, Fuchsia and the BSDs all feed the hardware source into a
kernel generator instead of handing it out raw. What the tree gets right: a
DRNG with nothing to give is refused loudly, never waited on or papered over.

**Exit**: a ChaCha20-based kernel generator answers `SYS_RANDOM`. It is seeded
from `RDSEED`, `RDRAND` and a jitter source, and reseeded on a schedule. At
boot a health test refuses a source that repeats or is stuck, by name. A test
feeds the generator a stuck source and shows the boot refused.
