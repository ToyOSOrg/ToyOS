---
status: expected-red
kind: tooling
opened: 2026-10-01
---

# `lan_dhcp_lease` asserts a line its wait does not wait for

`638L-638r5-whole.log` (`wt/toyos-tight` `59940c452`, "ceilings paid at
1.00x"): `"netd: ready, at most " never reached the the lan boot after "netd:
DHCP: lease "`. The capture ends at the harness's ready marker:

```
{1.281 netd} netd: DHCP: lease 10.0.2.15/24 from 10.0.2.2, gateway 10.0.2.2, dns [10.0.2.3], 43 ms after netd came up
{1.290 test-runner} ===READY===
```

`tests/common/lan.rs`'s `lan_dhcp_lease` awaits the lease line, drops the
guest, and then holds the capture to `READY` coming after `LEASE`. The boot
log already carries the lease, so the await returns at once and the capture is
whatever arrived before `===READY===`; netd says it is ready after its lease,
and test-runner's marker can come first. The branch does not touch the test,
netd's lease path or the boot's order, and the race is the same on `main`.

**Exit**: the test waits for the line it asserts; then the row goes.
