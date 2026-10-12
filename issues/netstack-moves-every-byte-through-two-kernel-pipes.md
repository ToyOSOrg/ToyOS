---
status: open
kind: track
opened: 2026-10-11
---

# netstack moves every byte through two kernel pipes

A received byte is copied three times between the card's buffer and the
application's: netstack writes it into its connection's heap ring
(`system/netstack/shard/tcp/src/ring.rs`), then into the client's pipe with a
`SYS_WRITE` whose copy runs under the one global `PIPES` lock with preemption
off (`kernel/src/pipe.rs`, `issues/the-global-pipe-lock-spans-a-user-copy.md`),
and the client's `SYS_READ` copies it out under the same lock. A sent byte is
copied into the pipe, out of it into netstack's stack chunk, into the
connection's send ring, and into the frame. Both buffers hold the same bytes: a
stream holds its pipes' 2 MiB pages beside its own 4 MiB rings
(`PLACE_BYTES`, `system/netstack/src/main.rs`).

Evidence, PR #861's round 3 on the T14, stage B at K = 32, medians of six runs
per MB of a 170 MB HTTPS download: the pipe table's lock held 0.281 ms and
spun for 0.047 ms (`pipe_lock_hold_cycles`, `pipe_lock_spin_cycles`, TSC
time), netstack's ring write took 0.225 ms (`cy_ring_write`), against 5.447 ms
of C0. The kernel copied 2.00 bytes of pipe per byte downloaded
(`pipe_copy_bytes`). Stage 0's scout split the hold on one T14 boot: the two
copies are 73% of it, and what the session design removes on the receive path
reads 0.192 ms per MB at most
(https://github.com/ToyOSOrg/ToyOS/pull/863#issuecomment-6114364774).

What a pipe also cannot say is carried by workarounds this track deletes: how
a stream ended is the order its two pipes close in, one bit
(`issues/a-streams-failure-reaches-its-client-as-a-reset-whatever-it-was.md`);
a stream's client leaving is learned only through those pipes
(`OWNERLESS_LIFE`, `system/netstack/node/src/streams.rs`); a datagram costs a
pipe write and a request on a fresh connection.

The owner ruled the replacement a go, in his words "pipes aren't meant for a
lot of data transfer, especially not network", and dropped zero copy from the
NIC into an application's memory. He accepted the design on the scout's
reading with one constraint on every stage: the data path is userland's, and
the kernel gains only what it alone can answer
(https://github.com/ToyOSOrg/ToyOS/pull/863#issuecomment-6114483274). The
design and its roast are PR #863's body.

- Stage 0, done: the scout boot on the T14 that splits the lock's hold into
  its two copies (PR #863).
- Stage 1, PR #867: the kernel's one new answer is `SYS_FSTAT`'s
  `DeviceMemory`, and a receiver judges a lent region through `fstat` before
  it maps it (closes
  `issues/a-file-server-maps-a-lent-window-at-the-size-it-expects.md`).
- Stage 2, PR #878: a byte ring with untrusted cursors and want words in
  `toyos-transport` (`lib/blockring/transport`), host-tested and modelled in
  loom.
- Stage 3: a TCP stream is one region and one connection: netstack's receive
  and send buffers are the region's two rings, the connection carries the
  connect and then only doorbells and the hang-up; std, libc and the mio and
  socket2 forks read and write the rings through `toyos::net`.
- Stage 4: a listener and a datagram socket the same way, and the pipe ABI's
  messages, `SYS_CONNECTION_JOIN` and `toyos::net`'s pipe handles go.

Owner: the orchestrator, who stages it.

Exit: no byte of a client's stream or datagram crosses a kernel pipe
(`rg -n 'Piped|DATA_HANDLES|connection_join' sdk system` is empty), and a T14
boot of the round 3 download on the session build reads `pipe_copy_bytes`
under 0.01 per byte downloaded beside the scout's rows.
