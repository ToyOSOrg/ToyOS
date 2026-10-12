---
status: open
kind: defect
opened: 2026-10-12
---

# Servers let a client go on the handshake limit with its request unread

Eight servers hold each connection they accept in a table until its first request is whole, and
let it go `HANDSHAKE_TIMEOUT`, 2 s, after its accept. Each declares its own copy:
`system/compositor/src/client.rs`, `system/diskserver/src/main.rs`,
`system/filepicker/src/main.rs`, `system/fileserver/src/main.rs`,
`system/logkeeper/src/inspect.rs`, `system/netstack/src/client.rs`,
`system/supervisor/src/main.rs` and `system/usbd/src/main.rs`. The limit is what drains the table,
so that a client that connects and says nothing cannot hold it; the defect is when a pass applies
it. Two more servers hold such a connection with no limit at all: soundserver's control plane, 63
slots (`MAX_CONTROL_CLIENTS`, `system/soundserver/src/control.rs`), and every surface host,
console's and terminal's, 4 (`toyos::surface::MAX_CLIENTS`, `sdk/toyos/src/surface.rs`). Lines are
at `1a7464692`.

**The order.** Six let a connection go on the limit before they read what the same wait reported:
netstack (`main.rs:449-456`, ahead of its read at `:498`), the compositor (`session.rs:353-360`,
ahead of `take_frames` at `:379`), diskserver (`main.rs:687-688`), filepicker (`main.rs:537-544`),
logkeeper (`inspect.rs:153-154`) and usbd (`main.rs:136-139`, each connection's limit checked
before it is read). A request already whole in its connection is dropped when the pass that would
read it comes late, so the limit times the server's passes as well as its client, and a machine
whose CPUs are busy elsewhere is late with nothing done wrong. Filepicker's picker session holds
its loop (`main.rs:610-613`): a connection it accepted before a session and had not read is let go
on the pass after it, once the session outlasts the limit. The supervisor (`main.rs:1299-1311`)
and fileserver (`main.rs:411-429`) serve first, as `5a68db78b` made init do after a stop asked
through its power port at 2.196 s was dropped unread at 4.753 s. They still read only the
connections the wait reported, so a request made whole after the wait returned and before the
limit's clock is read is let go whole as well. The compositor's table also holds a connection
whose copy it began, until its commit, timed by the same limit from the copy's beginning
(`client.rs:124-126`, `session.rs:1091`).

**What a client reads.** A connection let go on the limit is closed with nothing said to it.
netstack's client reads that as `NetError::NetstackNotFound` (`hangup`, `sdk/toyos/src/net.rs`),
the word for a machine with no netstack.

**The log.** netstack, the compositor, filepicker, fileserver, the supervisor and usbd name the
client they let go on the limit. diskserver's drop says nothing (`main.rs:688`), and logkeeper's
says nothing by choice (`inspect.rs:149-152`): a line per drop, it says, would let every holder of
`log` write into the log at its own rate. A drop on the limit comes at the server's rate, not the
client's: every entry a server drops in a span as long as the limit was in its table when the span
began, so it drops at most its table's size per limit. Today a drop's line cannot say whether the
client or the server's pass was late; once a connection is read before it is let go, a drop says
the client had not sent its request.

**The wait.** netstack (`main.rs:435-437`) and logkeeper (`inspect.rs:144-145`) wait a fresh whole
limit on each pass rather than what is left of the oldest entry's, so an entry can wait nearly
twice the limit: what the supervisor calls "a bound that is not the bound" (`main.rs:1198-1203`).
diskserver, filepicker, fileserver, the supervisor and usbd wait what is left; the compositor
wakes once a frame.

**A full table.** netstack (`main.rs:481-488`), the compositor (`session.rs:694-709`), filepicker
(`main.rs:554-558`), the supervisor (`main.rs:1232-1238`), usbd (`main.rs:127-129`) and logkeeper
(`inspect.rs:156-161`) accept the next connection and close it, naming it only in their own log
and logkeeper not even there; netstack's client reads that as `NetstackNotFound` too. soundserver
accepts it and signals `MSG_STREAM_ERROR` (`control.rs:185-188`), and a surface host accepts it and
closes it, which its client reads as `GrabError::HostGone` (`surface.rs:181-196`). diskserver
(`main.rs:650-651`) and fileserver (`main.rs:390-391`) stop accepting until an entry leaves, and
the connection waits in its port's queue, past whose 32 the kernel answers the connect
`ResourceExhausted` (`MAX_PENDING_CONNECTIONS`, `kernel/src/object/port.rs`). fileserver's
per-share part (`HANDSHAKE_SHARE`, `main.rs:455-462`) is a bound of another kind: one share's,
which stopping the acceptor would put on every share, and it answers `ResourceExhausted` in a frame
before it lets go.

**The supervisor's one table.** Its launcher, grants, swap and power connections share one table
of `MAX_PENDING_LAUNCHES` (`main.rs:1213-1248`), so once a full table blocks, a stop asked through
the power port waits behind silent launcher connections: the lateness `5a68db78b` recorded.

**What is published.** RFC 9110 §15.5.9 gives an HTTP server a limit on a request it has not
received whole, and a word for it, 408 (Request Timeout)
([RFC 9110](https://www.rfc-editor.org/rfc/rfc9110#section-15.5.9)). RFC 9112 §9.5 sets no length
for a server's timeout, and asks a server to let the transport's flow control resolve a temporary
overload rather than close connections for their clients to retry
([RFC 9112](https://www.rfc-editor.org/rfc/rfc9112#section-9.5)). The published defaults for a
request's header are 60 s in nginx, `client_header_timeout`
([source](https://github.com/nginx/nginx/blob/0a5db852e18d1c22c62a5ede1412ac3f416111a0/src/http/ngx_http_core_module.c#L3620-L3621),
[documentation](https://nginx.org/en/docs/http/ngx_http_core_module.html#client_header_timeout)),
and 20 s, stretched to 40 s while it arrives at 500 bytes/s, in Apache httpd,
`RequestReadTimeout header=20-40,MinRate=500`
([source](https://github.com/apache/httpd/blob/1e06e9860794ce39605b79a0a787abfe084e5677/modules/filters/mod_reqtimeout.c#L35-L37),
[documentation](https://httpd.apache.org/docs/2.4/mod/mod_reqtimeout.html#requestreadtimeout)).
2 s is a tenth of the shorter. On the limit, nginx closes the connection with no answer
([source](https://github.com/nginx/nginx/blob/0a5db852e18d1c22c62a5ede1412ac3f416111a0/src/http/ngx_http_request.c#L1130-L1134)).

**Seen** once, in a whole `cargo test` at `be929d941`, then the head of #886, which changes
nothing in `system/netstack`, `sdk/toyos/src/net.rs` or the test against its base `ecd6d1cc5`:
`netstack_socket_churn` red after 14 s, netstack's `dropping client 86031 — it never finished its
request` and then the test's loop of connects answered `NetstackNotFound` where it asserts
`ResourceExhausted`
([log](https://github.com/ToyOSOrg/evidence/blob/4dab62b2bd351b7467f238b7e8f7452237db7315/build/gates-be929d941/guest.log#L647-L682),
[summary](https://github.com/ToyOSOrg/evidence/blob/4dab62b2bd351b7467f238b7e8f7452237db7315/build/gates-be929d941/summary.txt)).
The harness priced its liveness ceilings at 1.00x on 14 cores
([log](https://github.com/ToyOSOrg/evidence/blob/4dab62b2bd351b7467f238b7e8f7452237db7315/build/gates-be929d941/guest.log#L1087-L1088)).
That run's host load was not recorded. The same gates at `f126fb514` were green, the test in 15 s
([log](https://github.com/ToyOSOrg/evidence/blob/4dab62b2bd351b7467f238b7e8f7452237db7315/build/gates/guest.log#L680)),
at one-minute load averages of 36.22 as the suite began and 36.73 as it ended
([guest.load](https://github.com/ToyOSOrg/evidence/blob/4dab62b2bd351b7467f238b7e8f7452237db7315/build/gates/guest.load)).
The log does not say whether the client sent its request late or netstack read it late.

**The cut test.** `netstack_socket_churn` is cut, an integration test of a userland program that
`.claude/agents/reviewer.md`'s **Tests** refuses; it does not come back. What it reached of the
node is held by `system/netstack/node/tests/listeners.rs`'s
`a_connect_past_the_places_is_refused_and_sends_nothing`,
`a_departed_clients_connection_holds_its_place_until_it_is_cut` and
`a_wake_the_owners_pipe_refuses_ends_the_listener`, and by
`system/netstack/node/tests/streams.rs`'s `a_client_that_left_is_finished_by_the_stack`,
`a_writer_that_leaves_after_the_shutdown_leaves_the_stream_to_its_reader`,
`a_pipe_that_is_no_pipe_resets_its_connection` and
`a_refused_watch_resets_only_a_stream_that_holds_the_end`. What only `system/netstack/src/serve.rs`
does, which carries no test, no test reaches since the cut.

**Owner**: the orchestrator, which dispatches the fix.

**Exit condition**:

- Each of the eight reads a connection before it lets it go on the limit, and serves the request
  it finds whole there however late its pass came; soundserver and every surface host read a
  connection before they let it go.
- The limit stays, on a connection that has sent no whole request, and every server, diskserver
  and logkeeper among them, logs that drop by name.
- The eight copies are one declaration every server reads, beside `FrameRx` in `toyos::ipc`, that
  takes its clock as an argument; a host test there goes red when a connection whose request is
  whole is let go on a pass past its limit.
- Its length is taken from nginx's and Apache httpd's published defaults and cites them, or
  another is defended against them at the declaration.
- No server waits a fresh whole limit: each wakes by what is left of its oldest entry's.
- A server whose table is full, soundserver and every surface host among them, stops accepting
  until an entry leaves it, as diskserver and fileserver do and RFC 9112 §9.5 asks, and none
  accepts a connection to close it because the table is full: the port's queue behind it holds the
  connection, and the kernel refuses the one past that queue by name.
- The supervisor's power connections wait in a table of their own, so a stop never waits behind
  launcher, grants or swap connections.
- A connection let go on the limit is closed with no answer, as nginx does. Its client names that
  as the server letting it go, never as no server, `NetError::NetstackNotFound` among them, and
  every client library of these servers does the same.
- `system/netstack/src/serve.rs` has its own tests of a second receive refused
  `ERR_RESOURCE_EXHAUSTED` (`serve.rs:563-567`), a waiting receive answered by its datagram
  (`settle`, `:676-680`), and the kernel's watch answers that end a stream, a listener or a datagram
  socket whose owner left or whose end is no pipe (`Sockets::answered`, `:798-843`).
