---
status: open
kind: track
opened: 2026-10-11
---

# netstack moves every byte through two kernel pipes

A received byte is copied three times between the card's buffer and the
application's: netstack writes it into its connection's heap ring
(`system/netstack/shard/tcp/src/ring.rs`), then into the client's pipe with a
`SYS_WRITE` whose copy runs under that pipe's lock with preemption off
(`kernel/src/pipe.rs`, `issues/a-pipes-lock-spans-a-user-copy-and-its-first-allocation.md`),
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
  in the SDK's `toyos::transport` (`sdk/toyos/src/transport`), host-tested and modelled
  in loom.
- Stage 3, done (PR #905): a TCP stream, connected or accepted, is one
  region whose two rings are [tcp]'s buffers and the connection it was opened
  on, which then carries only exports; its doorbells and its holders' leaving
  are events; std, libc, mio and
  socket2 reach it through `toyos::net::stream`, and `SYS_CONNECTION_JOIN` is
  gone. A stream is shared with a child as a holder of its own; other
  sockets are not yet (`issues/a-socket-libc-or-socket2-has-not-bound-is-not-shared-with-a-child.md`). A sent byte is copied
  twice by netstack, out of the lent send ring into a scratch buffer and from
  it into the frame, since the frame builder takes one slice; a builder that
  copies each span of the ring into the frame itself would remove the first.
  The exit read on the T14, `internet_download`, six whole runs per image,
  against main `f0a987bc6` under the same instrument, medians of runs 2 to 6
  per MB: `pipe_copy_bytes` 0.000135 per byte downloaded (every run under
  0.00015) against 2.0029, the pipe lock held 0.179 ms against 0.587 and spun
  0.092 against 0.108, C0 7.735 ms against 7.770, 812.7 Mb/s against 824.1
  (https://github.com/ToyOSOrg/evidence/tree/7cc1bda0dcb038084de60d65c64f30a3956a13a3/round3).
- Stage 4: a datagram socket is a region as a stream is. A listener and a
  datagram socket are already their holders' connections and bells
  (`toyos::net::socket`), each datagram one frame on its holder's
  connection, and the pipe ABI's messages and `toyos::net`'s pipe handles are
  gone; a datagram is still copied through the connection. netstack copies each datagram record's
  `len`, `port` and `addr` out of the region once and judges the copy: a
  `len` past the bytes the record holds ends that socket alone. `toyos::net`'s `send_to` refuses
  what a sender can judge and writes no record: a payload past
  `toyos::net::socket::MAX_DATAGRAM`, as POSIX's `sendto` fails
  `[EMSGSIZE]`; port 0; an unspecified address. That bound is the one
  declaration: [udp]'s `limits::MAX_PAYLOAD` goes, and [udp], the node's
  `name.rs` and `resolve.rs`, netstack's `serve.rs` and [udp]'s
  `tests/send.rs` read it. [udp] still refuses each by its counter at the
  trust boundary. What only netstack can judge, a connected socket's other
  destination, a route, broadcast permission, a source address and a pending
  ICMP error, netstack stores in the region's `pending` word if it is 0,
  then exchanges both client wants for 0 and rings each that was set, as it
  does for a stream's fatal `end`; a call's look after it arms reads
  `pending` beside its ring, and the socket's next call answers it and
  clears it, as POSIX's pending error is "returned in response to a
  subsequent send(), recv(), or getsockopt() operation" (XSH 2.10.10).
  netstack takes a record off the send ring only while [udp]'s queue for
  the socket has room, and looks again when a datagram leaves it or is
  dropped, so a full queue holds the ring and refuses nothing. A
  `send_to` over a full send ring waits as a stream's writer does, on a
  bell of the socket's own that netstack makes at the bind and alone
  writes, its want `tx_room`, as POSIX's `sendto` "shall block until space
  is available or a timeout occurs"; nonblocking, which std's
  `UdpSocket::set_nonblocking` then sets, it answers `WouldBlock`. Exit: a datagram record whose `len` is past the
  bytes it holds is refused while netstack serves on, red when netstack takes
  that `len` as written. A case per refusal: an oversized payload, port 0
  and an unspecified address each answer their error at `send_to` with no
  record written, and each refusal only netstack judges answers on the
  socket's next call; each is red when that call answers `Ok`. A case: a
  nonblocking `send_to` over a full send ring answers `WouldBlock` and
  writes no record, red when `set_nonblocking` sets nothing. A case: the
  socket's queue is full to a next hop that never answers and a `send_to`
  sleeps over a full send ring; when [ip] gives the hop up and [udp]'s
  `Udp::fail` drops the queue, netstack takes the held record and the sender
  wakes, red when netstack looks again only after a datagram leaves. Two loom
  models drive the client end and netstack's code as stage 4 lands them. In
  one, a `recv_from` asleep with nothing to receive, and a `send_to` asleep
  over a full send ring, each answers a refusal only netstack judges that
  lands meanwhile; each is red when the look reads `pending` before it arms,
  red when netstack exchanges the wants before it stores `pending`, and red
  under `transport-want-relaxed`, and the sender's is red when netstack rings only
  `rx_data`. In the other, [udp]'s queue starts full and a datagram leaves
  it as its own step, interleaved with netstack's looks, so netstack can find
  a published record while the queue is full and hold it; a sender that
  fills the send ring, beside a `recv_from` asleep on the connection, has
  every record sent, in order, none refused. It is red when netstack refuses
  a record at the full queue, red when it rings `tx_room` on the connection,
  red when after the leave it waits for the client's next ring without
  looking at the send ring again, and red under `transport-want-relaxed`.

Owner: the orchestrator, who stages it.

Exit: stage 3's and stage 4's exits, and the pipe ABI gone with stage 4:
`rg -n 'Piped|DATA_HANDLES|NOTIFY_HANDLES|connection_join|CONNECTION_JOIN' sdk
system kernel` and `rg -n 'pipe_pair' sdk/toyos/src/net.rs` are empty.
