---
status: open
kind: defect
opened: 2026-10-14
---

# Values no document states stand where another OS was their source

Each value below was set as Linux's and cited so. Its citation is gone, and no vendor
document or published specification states the number; the behaviour is unchanged.

- `kernel/src/arch/x86_64/control_regs.rs`'s `ENERGY_PERF_BIAS`, 6. SDM Vol. 3B §17.3.4 gives
  the scale, 0 favouring performance and 15 energy, with 7 "roughly" balanced, and names no 6.
- `lib/xhci/src/ladder.rs`'s `RESET_RECOVERY_NS`, 50 ms. USB 2.0 §7.1.7.5 asks 10 ms (TRSTRCY).
- `system/fileserver/src/resolve.rs`'s `MAX_LINKS`, 40. POSIX asks `{SYMLOOP_MAX}` of at least
  `{_POSIX_SYMLOOP_MAX}`, 8.
- `system/netstack/shard/tcp/src/conn.rs`'s `PROBE_SLACK`, 2 ms, added to RFC 8985 §7.2's
  `2 * SRTT`, which the RFC's pseudocode does not add.

Owner: the orchestrator, which dispatches each to the subsystem's owner.

**Exit**: each value is the one a vendor document or specification states, cited at the
site, or carries a ToyOS reason a measurement on a declared machine backs, recorded with
the change that sets it.
