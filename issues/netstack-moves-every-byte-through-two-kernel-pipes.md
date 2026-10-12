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
copies are 73% of it, and of the lock the session design removes at most
0.192 ms per MB on the receive path, beside the pipe syscalls' work outside
the hold, which no row reads apart
(https://github.com/ToyOSOrg/ToyOS/pull/863#issuecomment-6114364774).

What a pipe also cannot say is carried by workarounds this track deletes: how
a stream ended is the order its two pipes close in, one bit
(`issues/a-streams-failure-reaches-its-client-as-a-reset-whatever-it-was.md`);
a stream's client leaving is learned only through those pipes
(`OWNERLESS_LIFE`, `system/netstack/node/src/streams.rs`); a datagram costs a
pipe write and a request on a fresh connection.

The owner ruled the replacement a go, in his words "pipes aren't meant for a
lot of data transfer, especially not network", and ruled zero copy from the
NIC into an application's memory out unless the design showed a useful
benefit. The design dropped it under that condition, on its own reading: the
one copy it would remove is 4% of C0, against a filter and a queue per flow,
header split, an IOMMU window per client and a reassembly that copies. He
accepted the design on the scout's reading with one constraint on every
stage: the data path is userland's, and the kernel gains only what it alone
can answer (https://github.com/ToyOSOrg/ToyOS/pull/863#issuecomment-6114483274).
The design and its roast are PR #863's body.

- Stage 0, done: the scout boot on the T14 that splits the lock's hold into
  its two copies (PR #863).
- Stage 1, done (PR #867): the kernel's one new answer is `SYS_FSTAT`'s
  `DeviceMemory`, and a receiver judges a lent region through `fstat` before
  it maps it.
- Stage 2, done (PR #878): a byte ring with untrusted cursors and want words
  in `toyos-transport` (`lib/blockring/transport`), host-tested and modelled
  in loom.
- Stage 3: a TCP stream is one region and one connection: netstack's receive
  and send buffers are the region's two rings, beside [tcp]'s heap storage,
  which stays for stage 4's passive children, and the connection carries the
  connect and its answer, then only doorbells and the hang-up. A stream
  belongs to the process that connected or accepted it: `toyos::net` refuses
  by name a handle it holds no end for
  (`issues/a-stream-cannot-move-between-processes.md`). netstack rings a
  reader on the connection and a writer waiting for send room on a pipe of the
  stream's own, so neither drains a wake rung for the other. Within a
  direction no thread holds the end across its wait, and a thread that took
  the doorbell while another waits on it passes the take on through netstack.
  A registration arms the want of each direction it names and rings netstack
  if that direction is already ready, and netstack rings back. A fatal `end`,
  any but a FIN, makes the send direction ready to every look, as Linux's
  `tcp_poll` answers a reset socket writable. std, libc and the mio and
  socket2 forks read and write the rings through `toyos::net`, and std gains
  `FromRawFd`, `IntoRawFd` and the `OwnedFd` conversions for `TcpStream`,
  which take only the process's own ends, with `OwnedFd`'s drop letting a
  stream's handle go through `toyos::net`. Exit: three loom models that drive
  the client end and netstack's ringing code as stage 3 lands them, each red
  under a control that reverts its decision. In one, a stream's reader and
  writer, each asleep on its own want while netstack's thread releases room
  and publishes data in either order, end with neither waiting, and the
  control rings both wants on one doorbell. In another, a blocking call and a
  `poll` of one direction, the poll's watch answered by the doorbell's level
  at its look as the kernel's `deliver` answers it and the blocking call
  leaving bytes, the receive ring's `end` or room behind, end with the poll
  answered, and the control does not pass the take on. In the third, a source
  registered while its direction is ready, in both directions, with the
  receive ring's `end`, and with a fatal `end` while the send ring is full,
  made ready before the registration or racing it, gets its first event; it is
  red without the registration's arm and ring, red with either of the two
  removed, red under stage 2's `want-relaxed`, and red with the fatal `end`
  where a look reads only the send ring's head. A test: `toyos::net`, handed a
  stream handle the process holds no end for, refuses it by name and writes
  nothing on its connection. And a T14 boot of round 3's `internet_download`
  on stage 3's head reads, in every run, `pipe_copy_bytes` under 0.01 per byte
  downloaded, its C0, hold and spin per MB posted beside the scout's rows. No
  tree that lands has that row or that counter: the boot carries the scout's
  instrument, `git diff 777c96b2b 2f9e090b8` (`wt/toyos-netring-scout` over
  its base), onto stage 3's head, less the netstack stamps on the pipe writes
  stage 3 deletes.
- Stage 4: a listener and a datagram socket the same way, and the pipe ABI's
  messages, `SYS_CONNECTION_JOIN` and `toyos::net`'s pipe handles go. A
  passive child's bytes wait in [tcp]'s heap storage, at most
  `limits::RECEIVE_BUFFER_INITIAL` a child and `limits::LISTEN_READY` children
  a listener, and its accept copies them into the region before it answers.
  Exit: over the region, `system/netstack/node/tests/listeners.rs`'s
  `a_listener_answers_a_syn_and_wakes_its_owner_when_the_handshake_ends`
  passes, and so does a case in which a child holds, before its accept, text
  in order, text past a hole and its FIN behind that, the hole is filled after
  the accept, and the region reads all of it in order, then its `end`. That
  case is red under each of three controls that drop at the accept one thing
  the child held: its in-order bytes, which also reds the first, its
  out-of-order range, its FIN.

Owner: the orchestrator, who stages it.

Exit: stage 3's and stage 4's exits, and the pipe ABI gone with stage 4:
`rg -n 'Piped|DATA_HANDLES|NOTIFY_HANDLES|connection_join|CONNECTION_JOIN' sdk
system kernel` and `rg -n 'pipe_pair' sdk/toyos/src/net.rs` are empty.
