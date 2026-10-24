---
status: open
kind: tooling
opened: 2026-10-24
---

# A metal judge says "on the T14" for every bench's readback

`Readback::job_passed` (`tests/common/metal.rs`) words a job's non-zero exit as
"`<binary>` exited `<code>` on the T14", whatever bench the readback came from. A
`--metal-host` run's FAIL line then names a machine it never judged, and a reader
can take the AMD bench's failure for the T14's.

**Evidence.** #1056's round 2 at 2c00ebbdc: the AMD bench's `cryptocase` judge
log (`--metal-host` for the AMD bench) reads `FAIL crypto_instrument:
test_rs_crypto_parity exited 101 on the T14`.

Owner: the orchestrator.

**Exit**: a judge's line names the bench its readback came from, or none.
