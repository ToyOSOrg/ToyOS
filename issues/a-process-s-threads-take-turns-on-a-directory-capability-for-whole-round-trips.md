---
status: assigned
kind: defect
opened: 2026-11-11
---

# A process's threads take turns on a directory capability for whole round trips

`sdk/toyos/src/files/owner.rs` holds one connection per directory capability behind a `Turnstile`, and every
request through it (`fs.rs` `Dir::call_with`) takes the turn for its whole round trip: one request in flight per
capability per process, though the wire carries up to 16 tagged requests. Two threads of one program calling into
one capability serialise, and unfairly.

Measured on the T14 (osbench `fileops`, scratch stage timers, 2026-11-11, evidence of PR 1225): `fstat` on /tmp
beside a thread of the same process asking as fast as it is answered takes 21.4 to 21.6 µs p50 against 7.6 alone
(2.8x), and the other thread is answered 7.4 to 7.9 times for each answer to this one. Beside another process the
same call takes 8.4 to 8.5 µs (1.1x): the server interleaves two clients fine, so the cost is the client's turn.
Every multi-threaded program pays it on every path call: a parallel compiler, an async runtime.

Owner: agents/fileops, after storage step 3a (holder-side `fstat` and writes, which leave the turnstile already).
Exit: requests from different threads are in flight on one capability at once, each thread waiting for its own
tag's reply, the window divided into per-request slots; one connection per process and capability stays, so the
server's per-client fairness is unchanged (a connection per thread was the alternative, rejected because it
multiplies windows and server slots by threads and gives a many-threaded process a larger share of the server).
Done when the `fstat`-beside-a-thread row reads within 1.2x of alone for `stat`, open and a path call.
