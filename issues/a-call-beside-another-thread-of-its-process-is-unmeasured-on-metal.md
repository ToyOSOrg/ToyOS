---
status: open
kind: tooling
opened: 2026-11-11
---

# A call beside another thread of its process is unmeasured on metal

A process's threads have their requests on one directory capability in flight at once: up to the wire's 16, each in
its own share of the window, every thread waiting for its own tag's reply (`sdk/toyos/src/fs.rs`, its in-flight core
`fs/flight.rs`, modelled under loom in `kernel/loom/tests/fs_flight.rs`). One connection per process and capability
stays, so the server's per-connection fairness is unchanged; a connection per thread was the alternative, rejected
because it multiplies windows and server slots by threads and gives a many-threaded process a larger share of the
server.

Before, the process took one turn on the capability for each whole round trip. On the T14 (osbench `fileops`,
scratch stage timers, 2026-11-11, evidence of PR 1225), `fstat` on /tmp beside a thread of the same process asking as
fast as it is answered took 21.4 to 21.6 µs p50 against 7.6 alone (2.8x), and beside another process 8.4 to 8.5 µs
(1.1x). What a call beside another thread costs now is unmeasured: the benches are down.

- **Owed on metal:** `osbench_disk` on the T14, main from before the tagged replies and after, two boots a side in
  A, B, B, A order. On both volumes, main after must show `stat.beside_a_thread` and
  `open_close.beside_a_thread` within 1.2x of `stat` and `open_close` in the same boot; main before is the
  comparison.

**Exit**: that measurement, the two rows within 1.2x on both volumes, or each remaining difference measured to its
parts.

**Owner**: agents/fileops (the file server's client).
