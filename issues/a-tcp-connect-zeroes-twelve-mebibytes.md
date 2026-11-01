---
status: assigned
kind: defect
opened: 2026-10-27
---

# A TCP connect zeroes twelve mebibytes

Before a ToyOS TCP connect returns, it makes and zeroes memory the connection
may never use:

- **The stream region**, 8 MiB (`toyos::net::stream::REGION_BYTES`), whose
  2048 frames `SYS_SHM_CREATE` took and zeroed before it answered. Since
  #1077 a connect commits its header and 64 KiB of each ring, 132 KiB, and
  each ring's producer commits more as it doubles the ring
  (`toyos::transport::stream`); the T14's reading of that is owed.
- **The connection's two directions**, 2 MiB each: the request's page on the
  client's write and the answer's on netstack's, each zeroed whole on that
  first write (`issues/a-pipe-is-a-two-mebibyte-page-for-a-mapping-nothing-makes.md`).
  Since #1074 a direction takes 4 KiB frames as it fills and zeroes none, so
  this part is measured no longer.

**Measured** by the paired `osbench lan` sessions from the T14's I219 to the
AMD bench's Linux peer over its Wi-Fi:

| `lan.wired.tcp.connect`, ms | p50 | min |
|---|---|---|
| ToyOS, 8 MiB region, `aeba59fb0` (2026-10-24) | 3.02, 2.05, 3.00, 3.06 | 2.16, 1.70, 2.08, 1.94 |
| Linux, same session | 1.24, 2.01, 1.97, 1.28 | 0.91, 1.39, 0.95, 0.91 |
| ToyOS, 8 MiB region, `af239c8bb` (2026-10-27) | 3.00, 3.00, 2.95, 2.97 | 1.99, 1.93, 1.79, 2.05 |
| ToyOS, 2 MiB region, otherwise `af239c8bb` (2026-10-27) | 2.03, 2.06 | 1.61, 1.67 |
| Linux, 2026-10-27 session | 8.03, 1.93, 2.02, 2.01 | 1.08, 1.01, 0.98, 1.28 |

Both systems' p50s are bimodal on this path, so the p50s alone did not
separate work from a wait on the peer. The image whose region was the only
difference does: three quarters of the region taken away moved the p50 from
about 3.0 to about 2.05 ms, Linux's own. Its minimums, 1.61 and 1.67 ms, are
still about 0.6 ms above Linux's; the pipes' 4 MiB are the derived
candidate for that, not a reading. Evidence:
[`aeba59fb0/`](https://github.com/ToyOSOrg/evidence/tree/37288c84c175a647145fe83fd51d945806cdb4ff/r1/aeba59fb0) and [`af239c8bb/`](https://github.com/ToyOSOrg/evidence/tree/37288c84c175a647145fe83fd51d945806cdb4ff/r1/af239c8bb).

**Direction, the owner's (2026-10-27)**: remove the waste, not shrink a
constant: size each stream region to what its connection uses, and take
pipes off 2 MiB pages. The kernel's rule that a fault on a memory object
never allocates stays: no zeroing on first touch. Growing a region as its
connection needs more is the implementer's design for that sizing.

**Exit**: a connect zeroes no memory its connection has not asked for, and
`lan.wired.tcp.connect` reads within Linux's run-to-run range on the same
path.

**Owner**: the `tcp-send` assignment, under the orchestrator.
