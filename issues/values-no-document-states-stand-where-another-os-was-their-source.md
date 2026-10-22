---
status: open
kind: defect
opened: 2026-10-14
---

# Values no document states stand where another OS was their source

Each value below was set as Linux's and cited so. Its citation is gone, and no vendor
document or published specification states the number; the behaviour is unchanged.

- `system/fileserver/src/resolve.rs`'s `MAX_LINKS`, 40. POSIX asks `{SYMLOOP_MAX}` of at least
  `{_POSIX_SYMLOOP_MAX}`, 8.
- `system/netstack/shard/tcp/src/conn.rs`'s `PROBE_SLACK`, 2 ms, added to RFC 8985 §7.2's
  `2 * SRTT`, which the RFC's pseudocode does not add.

Owner: the orchestrator, which dispatches each to the subsystem's owner.

**Exit**: each value is the one a vendor document or specification states, cited at the
site, or carries a ToyOS reason a measurement on a declared machine backs, recorded with
the change that sets it.
