# Services and failure

## The supervisor

- The one program the kernel starts, and the holder of the system capability.
- It starts every program with what that program's manifest row grants
  ([The system manifest](../using/manifest.md)), and creates every service port
  before any server runs, so a client's first connection works whether or not
  its server has started.
- It never dies and stays small: its decisions are pure, host-tested logic,
  and the process only holds handles, spawns and runs its loop.
- It owns the machine's orderly stop, derived from the manifest's
  dependencies. Once it asks the kernel to end the machine, no program but
  the asking thread runs its own code again; the kernel waits for no
  program to finish, so what must finish first is done before the ask.
- It replaces a running service's binary when an authorised
  caller in a login session asks.

The other services are the components in the [Overview](overview.md). Each is
an ordinary process holding only what its row grants.

## Admission

Every server admits its clients the same way, through one shared mechanism:

- **A connection waits for its first whole request in a bounded table**, for a
  bounded time. It is read once more before it is let go for its silence, so a
  request that arrived whole is served however late the server came to it; a
  connection let go is closed unanswered.
- **A request is read only from an admitted client**, whole and without
  waiting, and answered by one send that never waits.
- **One client holds at most a share of the table.** A client is what the
  badge on its connection says, written by whoever minted its connector. Past
  its share its next connection is refused by name while another client's is
  taken; a port whose connector carries no badge that names its holder has one
  share for all its holders.
- **A full table accepts no more**, whatever wakes its server. The port's
  queue holds the next connection, and the kernel refuses the one past that
  queue by name.
- **No client writes its server's log at its own rate.** Of a client's
  refusals and of connections let go for what they sent or did not, its
  server says one line a client a limit for as many clients as its table has
  slots, and one more line a limit shared by all clients past those; no
  client is said of twice in a limit. Each refusal or connection let go that
  is not said is counted into the next line said, and a count no line has
  carried within a limit of the last is said on its own, on the shared line.
  So however many clients arrive, a table says at most one line a slot and
  one more a limit, and says or counts every refusal and connection let go
  within a limit of the line before. A hang-up is not said: a client may
  connect only to learn that its server is there.
- **The size of a table and of a share are the server's**; the mechanism and
  its limit are the same for all.

## Failure

```text
   service crashes ──> parent keeps its endpoint ──> restart (bounded attempts)
                                                      │
                     clients: what was durable survives; open state answers "gone"
```

- **Parents supervise.** A crashed service is restarted by its parent within a
  bounded number of attempts; a feature may be interrupted meanwhile.
- **A service's identity outlives its process.** The parent keeps the endpoint
  while recovery is owed and gives the replacement only its authorised
  capabilities.
- **Recovery restores durable state, not sessions.** File servers recover what
  was on disk, not old opens; network sessions may fail; the memory service
  keeps recoverable swapped contents.
- **A file server's end is the loss of its file system.** Every mapping of its
  files is revoked, so a program executing from or reading a mapping of them,
  a service included, ends at its next access to that mapping rather than run
  on with stale memory.
- **Nothing is silently replayed.** An operation with an uncertain outcome is
  reported, and retried only where safe; a write whose acknowledgement was lost
  is never replayed behind the client's back.
- **A restart refused for lack of resources stays visibly pending** and retries
  when resources return, without busy-looping. Recovery state is consistent
  across a crash; memory merely kept alive does not establish that.
- **No restart because time passed.** Stalled progress is reported; manual
  action remains available.
- **A compositor crash may end its session**, never unrelated work.
