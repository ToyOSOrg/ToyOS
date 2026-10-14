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
- Stage 3 (PR #905): a TCP stream is one region and one connection, a
  connected one and an accepted one alike, as the orchestrator ruled on #905
  (R1): netstack's receive and send buffers are the region's two rings, and
  [tcp]'s heap storage stays for a passive child before its accept, at most
  `limits::RECEIVE_BUFFER_INITIAL` a child and `limits::LISTEN_READY`
  children a listener. An accept takes one of the wakes netstack writes on the
  listener's pipe for each child that waits before it makes a region, so a
  nonblocking accept with none answers `WouldBlock` having made nothing; it
  lends the region, netstack copies what the child held into the receive ring
  at its offsets, text past a hole included and the FIN [tcp]'s state, before
  it answers, and the answer carries the peer's address and the `nodelay` the
  child inherited from its listener. `SYS_CONNECTION_JOIN` goes. The
  connection carries the connect or the accept and its answer, then only
  doorbells and the hang-up. A stream belongs to the process that connected
  or accepted it: `toyos::net` refuses by name a handle it holds no end for.
  netstack rings a reader on the connection and a writer waiting for send room
  on a pipe of the stream's own, so neither drains a wake rung for the other.
  Within a direction no thread holds the end across its wait, and a thread
  that took the doorbell while another waits on it passes the take on through
  netstack. A registration arms the want of each direction it names and rings
  netstack if that direction is already ready, and netstack rings back. A
  fatal `end`, any but a FIN, makes the send direction ready to every look, as
  POSIX's `select` counts a descriptor ready for writing when a write "would
  not block, whether or not the function would transfer data successfully",
  and a `write` reads it before it copies, answering its error and copying
  nothing, as RFC 9293 flushes every segment queue at a reset or a user
  timeout (§3.10.7.4, §3.10.8) and answers a SEND that finds no connection an
  error (§3.10.2). std and libc read and write the rings through
  `toyos::net`, whose table of the process's ends is the instance std links
  (R2): the mio and socket2 forks reach a stream through std, and
  `std::os::toyos::net` gives them what std has no word for, the bell to watch
  and the registration. `toyos-transport` and `toyos-untrusted`, whose code
  the client end is, are published SDK crates in std's graph (R3). std gains
  `FromRawFd`, `IntoRawFd` and the `OwnedFd` conversions for `TcpStream`,
  which take only the process's own ends, and the `rust/` fork's ToyOS arms of
  `OwnedFd`'s drop and `BorrowedFd::try_clone_to_owned` let go of and enter a
  stream's handle through `toyos::net`. A stream's handle reads doorbells, so
  `File::from_raw_fd` of one panics with `toyos::net`'s refusal, and every
  path std offers to move a raw handle into another process asks
  `toyos::net` of each handle before anything moves and answers its refusal:
  a spawn's stdio slots, `CommandExt::inherit_handle` into any slot,
  `CommandExt::endow` and `Command::provide`, each with no child started and
  the handle still the caller's, so no refused move leaves an entry that
  outlives its handle's number. `toyos::ipc`'s `send_handles` is refused by
  its type: it takes `OwnedHandle`s, and a stream's handle becomes one only
  through `OwnedHandle::from_raw`, whose `# Safety` excludes a handle
  `toyos::net` answers for. The bare `dup`, `SYS_HANDLE_SEND` and `SYS_SPAWN`
  syscalls stay unchecked. A regression,
  `issues/a-stream-cannot-move-between-processes.md`. Exit: three loom models
  that drive the client end and netstack's ringing code as stage 3 lands
  them, each red under a control that reverts its decision. In one, a
  stream's reader and writer, each asleep on its own want while netstack's
  thread releases room and publishes data in either order, end with neither
  waiting, and the control rings both wants on one doorbell. In another, a
  blocking call and a `poll` of one direction, the poll's watch answered by
  the doorbell's level at its look as the kernel's `deliver` answers it and
  the blocking call leaving bytes, the receive ring's `end` or room behind,
  end with the poll answered, and the control does not pass the take on. In
  the third, a source registered while its direction is ready, in both
  directions, with the receive ring's `end`, and with a fatal `end` while the
  send ring is full, made ready before the registration or racing it, gets
  its first event; it is red without the registration's arm and ring, red
  with either of the two removed, red under stage 2's `want-relaxed`, and red
  with the fatal `end` where a look reads only the send ring's head. Over the
  region, `system/netstack/node/tests/listeners.rs`'s
  `a_listener_answers_a_syn_and_wakes_its_owner_when_the_handshake_ends`
  passes, and so does a case in which a child holds, before its accept, text
  in order, text past a hole and its FIN behind that, the hole is filled after
  the accept, and the region reads all of it in order, then its `end`. That
  case is red under each of three controls that drop at the accept one thing
  the child held: its in-order bytes, which also reds the first, its
  out-of-order range, its FIN. A test: `toyos::net`, handed a stream handle
  the process holds no end for, refuses it by name and writes nothing on its
  connection, and a stream's own handle, given out by `into_raw_fd` or
  `From<TcpStream> for OwnedFd` or made by `try_clone_to_owned`, and taken
  back, shares the one end and its cursors, red when giving the handle out
  lets its entry go. A test: `File::from_raw_fd` of a stream's handle panics
  with that refusal, red when it takes the handle silently. A test per moving
  path, each handed a stream's handle: a spawn's stdio slot, `inherit_handle`
  into a slot past the stdio slots, `endow` and `provide` each answer the
  refusal and start no child, and the stream then reads and writes through
  that handle; each is red when its path takes the handle silently, and
  `endow`'s and `provide`'s are red when the handle moves and its entry stays.
  A test: a `write` with room after a reset answers `ConnectionReset` and
  copies nothing, red when `write` copies before it reads the `end`. And a T14
  boot of round 3's `internet_download` on stage 3's head reads, in every run,
  `pipe_copy_bytes` under 0.01 per byte downloaded, its C0, hold and spin per
  MB posted beside the scout's rows. No tree that lands has that row or that
  counter: the boot carries the scout's instrument, `git diff 777c96b2b
  2f9e090b8` (`wt/toyos-netring-scout` over its base), onto stage 3's head,
  less the netstack stamps on the pipe writes stage 3 deletes. As built, a
  sent byte is copied twice by netstack, out of the lent send ring into a
  scratch buffer and from it into the frame, since the frame builder takes
  one slice; a builder that copies each span of the ring into the frame
  itself would remove the first.
- Stage 4: a listener's connection carries its wakes in place of its pipe, a
  datagram socket is a region as a stream is, and the pipe ABI's messages and
  `toyos::net`'s pipe handles go. netstack copies each datagram record's
  `len`, `port` and `addr` out of the region once and judges the copy: a
  `len` past the bytes the record holds ends that socket alone. `toyos::net`'s `send_to` refuses
  what a sender can judge and writes no record: a payload past
  `toyos_abi::net::UDP_MAX_PAYLOAD`, as POSIX's `sendto` fails
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
  under `want-relaxed`, and the sender's is red when netstack rings only
  `rx_data`. In the other, [udp]'s queue starts full and a datagram leaves
  it as its own step, interleaved with netstack's looks, so netstack can find
  a published record while the queue is full and hold it; a sender that
  fills the send ring, beside a `recv_from` asleep on the connection, has
  every record sent, in order, none refused. It is red when netstack refuses
  a record at the full queue, red when it rings `tx_room` on the connection,
  red when after the leave it waits for the client's next ring without
  looking at the send ring again, and red under `want-relaxed`.

Owner: the orchestrator, who stages it.

Exit: stage 3's and stage 4's exits, and the pipe ABI gone with stage 4:
`rg -n 'Piped|DATA_HANDLES|NOTIFY_HANDLES|connection_join|CONNECTION_JOIN' sdk
system kernel` and `rg -n 'pipe_pair' sdk/toyos/src/net.rs` are empty.
